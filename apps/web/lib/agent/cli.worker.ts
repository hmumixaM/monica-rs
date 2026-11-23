import variant from '@jitl/quickjs-wasmfile-release-asyncify';
import { newQuickJSAsyncWASMModuleFromVariant } from 'quickjs-emscripten-core';

// A guest has a JavaScript runtime and one JSON bridge. Browser APIs are never
// installed in its global scope. Each program runs in its own worker.
async function run({ source, batchSize, maxLookups }: Program) {
  const module = await newQuickJSAsyncWASMModuleFromVariant(variant);
  const vm = module.newContext();
  let deadline = performance.now() + 2_000;
  vm.runtime.setMemoryLimit(32 * 1024 * 1024);
  vm.runtime.setMaxStackSize(512 * 1024);
  vm.runtime.setInterruptHandler(() => performance.now() > deadline);
  try {
    const invoke = vm.newAsyncifiedFunction('invoke', async (command, args) => {
      const name = vm.getString(command);
      const json = vm.getString(args);
      const started = performance.now();
      const reply = await new Promise<string>(resolve => {
        self.onmessage = ({ data }) => resolve(data.json);
        self.postMessage({ type: 'lookup', name, json });
      });
      deadline += performance.now() - started;
      return vm.newString(reply);
    });
    try {
      vm.setProp(vm.global, '__monicaLookup', invoke);
    } finally {
      invoke.dispose();
    }
    const result = await vm.evalCodeAsync(`
      (() => {
        const request = (command, args) => JSON.parse(__monicaLookup(command, JSON.stringify(args)));
        const cli = (command, args = {}, project) => {
          if (command !== 'batch') return request(command, args);
          if (!Array.isArray(args) || args.length > ${maxLookups}) {
            throw new Error('batch 需要命令数组，每批最多 ' + ${maxLookups} + ' 项。');
          }
          if (project !== undefined && typeof project !== 'function') {
            throw new Error('batch 的第三个参数必须是提取结果的函数。');
          }
          const rows = [];
          for (let offset = 0; offset < args.length; offset += ${batchSize}) {
            const slice = args.slice(offset, offset + ${batchSize});
            const chunk = request('batch', slice);
            for (let index = 0; index < slice.length; index++) {
              const row = Array.isArray(chunk) ? chunk[index] : chunk;
              rows.push(project ? project(row, offset + index) : row);
            }
          }
          return rows;
        };
        const result = ((cli) => { "use strict";\n${source}\n})(cli);
        if (result && typeof result.then === 'function') throw new Error('cli 是同步函数，请直接返回结果，无需 async 或 await。');
        return JSON.stringify(result ?? null);
      })()`, 'monica-query.js');
    if (result.error) {
      try {
        const message = vm.getProp(result.error, 'message');
        try { throw new Error(vm.getString(message) || 'JavaScript 查询执行失败。'); }
        finally { message.dispose(); }
      } finally { result.error.dispose(); }
    }
    try {
      return vm.getString(result.value);
    } finally { result.value.dispose(); }
  } finally { vm.dispose(); }
}

type Program = { source: string; batchSize: number; maxLookups: number };

self.onmessage = ({ data }: MessageEvent<Program>) => {
  self.onmessage = null;
  void run(data).then(
    json => self.postMessage({ type: 'result', json }),
    error => self.postMessage({ type: 'error', error: error instanceof Error ? error.message : 'JavaScript 查询执行失败。' }),
  );
};
