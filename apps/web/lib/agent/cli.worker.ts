import variant from '@jitl/quickjs-wasmfile-release-asyncify';
import { newQuickJSAsyncWASMModuleFromVariant } from 'quickjs-emscripten-core';

// A guest has a JavaScript runtime and one JSON bridge. Browser APIs are never
// installed in its global scope. Each program runs in its own worker.
async function run(source: string) {
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
        const cli = (command, args = {}) => JSON.parse(__monicaLookup(command, JSON.stringify(args)));
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

self.onmessage = ({ data }: MessageEvent<{ source: string }>) => {
  self.onmessage = null;
  void run(data.source).then(
    json => self.postMessage({ type: 'result', json }),
    error => self.postMessage({ type: 'error', error: error instanceof Error ? error.message : 'JavaScript 查询执行失败。' }),
  );
};
