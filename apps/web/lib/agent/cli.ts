import type { AgentTool } from '@earendil-works/pi-agent-core';
import { validateToolArguments } from '@earendil-works/pi-ai/utils/validation';
import { directoryCommands, type CommandArgs } from './cli-directory';
import { lookupCommands, TOOL_LABELS } from './tools';

const MAX_LOOKUPS = 240;
const BATCH_SIZE = 6;
const MAX_OUTPUT_BYTES = 48_000;
const UUID = /^[\da-f]{8}(?:-[\da-f]{4}){3}-[\da-f]{12}$/i;

function object(value: unknown): CommandArgs {
  if (value === null || typeof value !== 'object' || Array.isArray(value)) throw new Error('查询参数必须是对象。');
  return value as CommandArgs;
}

function errorText(error: unknown) {
  return (error instanceof Error ? error.message : String(error)).slice(0, 400);
}

const commands = new Map(lookupCommands().map(command => [command.name, command]));

function lookupDispatcher(signal: AbortSignal) {
  let lookups = 0;
  const { directory, directory_facets: facets } = directoryCommands(signal);
  async function lookup(name: unknown, input: unknown): Promise<unknown> {
    signal.throwIfAborted();
    if (++lookups > MAX_LOOKUPS) throw new Error('本脚本的查询次数已用完。请分批查询。');
    const args = object(input);
    if (name === 'directory') return directory(args);
    if (name === 'directory_facets') return facets(args);
    if (name === 'help') {
      if (!args.command) return ['directory', 'directory_facets', 'batch', ...commands.keys()];
      const command = typeof args.command === 'string' ? commands.get(args.command) : undefined;
      if (!command) throw new Error('请使用已列出的查询命令。directory、directory_facets 和 batch 的参数见工具说明。');
      return { description: command.description, parameters: command.parameters };
    }
    const tool = typeof name === 'string' ? commands.get(name) : undefined;
    if (!tool) throw new Error('未知的 Monica 查询命令。');
    for (const key of ['profile_id', 'record_id']) {
      const value = args[key];
      if (value !== undefined && (typeof value !== 'string' || !UUID.test(value))) {
        throw new Error(`${key} 必须是 Monica 记录的 UUID。`);
      }
    }
    const id = `cli-${lookups}`;
    const validated = validateToolArguments(tool, { type: 'toolCall', id, name: tool.name, arguments: args });
    const result = await tool.execute(id, validated, signal);
    const output = result.content.find(part => part.type === 'text');
    if (!output || output.type !== 'text') throw new Error('查询没有返回文字结果。');
    return JSON.parse(output.text);
  }

  async function checked(name: unknown, args: unknown) {
    try { return await lookup(name, args); }
    catch (error) { signal.throwIfAborted(); return { error: errorText(error) }; }
  }

  return async (name: string, json: string) => {
    if (json.length > 65_536) throw new Error('查询参数过长。');
    const args: unknown = JSON.parse(json);
    if (name !== 'batch') return checked(name, args);
    if (!Array.isArray(args) || args.length > MAX_LOOKUPS - lookups) {
      throw new Error('batch 需要命令数组，且不能超过脚本剩余的查询次数。');
    }
    const results: unknown[] = [];
    for (let offset = 0; offset < args.length; offset += BATCH_SIZE) {
      signal.throwIfAborted();
      const batch = args.slice(offset, offset + BATCH_SIZE).map(entry => object(entry));
      results.push(...await Promise.all(batch.map(entry => checked(entry.command, entry.args ?? {}))));
    }
    return results;
  };
}

function runScript(source: string, signal?: AbortSignal): Promise<string> {
  signal?.throwIfAborted();
  if (source.length > 32_000) throw new Error('JavaScript 查询脚本过长。');
  return new Promise((resolve, reject) => {
    const worker = new Worker(new URL('./cli.worker.ts', import.meta.url), { type: 'module' });
    const controller = new AbortController();
    const lookup = lookupDispatcher(controller.signal);
    const timeout = setTimeout(() => fail(new Error('JavaScript 查询超时。请缩小范围或分批查询。')), 120_000);
    function cleanup() {
      clearTimeout(timeout);
      signal?.removeEventListener('abort', abort);
      controller.abort();
      worker.terminate();
    }
    function fail(error: unknown) { cleanup(); reject(error); }
    function abort() { fail(signal?.reason); }
    signal?.addEventListener('abort', abort, { once: true });
    worker.onerror = () => fail(new Error('无法启动 JavaScript 查询环境。'));
    worker.onmessage = async ({ data }) => {
      if (data.type === 'error') { fail(new Error(errorText(data.error))); return; }
      if (data.type === 'result') {
        if (new TextEncoder().encode(data.json).length > MAX_OUTPUT_BYTES) {
          fail(new Error('脚本输出过长。请先在 JavaScript 中筛选或汇总，并保留必要的原文链接。'));
        } else {
          cleanup();
          resolve(data.json);
        }
        return;
      }
      if (data.type === 'lookup') {
        let json: string;
        try { json = JSON.stringify(await lookup(data.name, data.json)); }
        catch (error) { json = JSON.stringify({ error: errorText(error) }); }
        if (!controller.signal.aborted) worker.postMessage({ json });
      }
    };
    worker.postMessage({ source });
  });
}

export const cliTool: AgentTool = {
  name: 'monica_cli',
  label: TOOL_LABELS.monica_cli,
  description: [
    '运行一段 JavaScript 查询 Monica。用 return 返回可序列化数据。在脚本里直接调用同步函数 cli(command, args)，可以用循环、filter、map、sort 做批量查询和归纳。',
    'directory 参数 query、type（person/group/all）、tags（全部匹配的名称或 ID 数组）、company、school、provider、sort（relevance/name/last_contact/messages/birthday）、limit（至多 500）、offset。返回 rows、total、next_offset。',
    'directory_facets 参数 kind（tags/companies/schools）、query、limit、offset，可发现筛选值。',
    'batch 接受 [{command,args}, ...]，每项独立返回数据或 {error}，结果顺序同输入。脚本至多执行 240 项查询。先筛选再批读，并在脚本里缩小最终输出。',
    '不确定某项记录查询的参数时，用 cli("help", {command:"命令名"}) 查看参数定义。',
    ...[...commands.values()].map(command => {
      const schema = command.parameters as { properties?: Record<string, unknown>; required?: string[] };
      return `${command.name}(${Object.keys(schema.properties ?? {}).map(key => schema.required?.includes(key) ? key : `${key}?`).join(', ')}) ${command.description}`;
    }),
  ].join('\n'),
  parameters: {
    type: 'object', properties: { script: { type: 'string', description: 'JavaScript 函数体，最后 return 查询结果。' } },
    required: ['script'], additionalProperties: false,
  } as unknown as AgentTool['parameters'],
  async execute(_id, args, signal) {
    const { script } = args as { script: string };
    return { content: [{ type: 'text', text: await runScript(script, signal) }], details: undefined };
  },
};
