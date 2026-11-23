import type { AgentTool } from '@earendil-works/pi-agent-core';
import { validateToolArguments } from '@earendil-works/pi-ai/utils/validation';
import { directoryCommands, type CommandArgs } from './cli-directory';
import { lookupCommands, TOOL_LABELS } from './tools';

const MAX_LOOKUPS = 240;
const BATCH_SIZE = 6;
const MAX_LOOKUP_BYTES = 256 * 1024;
const MAX_OUTPUT_BYTES = 48_000;
const MAX_TRACE_BYTES = 64 * 1024;
const encoder = new TextEncoder();
const UUID = /^[\da-f]{8}(?:-[\da-f]{4}){3}-[\da-f]{12}$/i;

function object(value: unknown): CommandArgs {
  if (value === null || typeof value !== 'object' || Array.isArray(value)) throw new Error('查询参数必须是对象。');
  return value as CommandArgs;
}

function errorText(error: unknown) {
  return (error instanceof Error ? error.message : String(error)).slice(0, 400);
}

const commands = new Map(lookupCommands().map(command => [command.name, command]));

export function cliTitle(value: unknown) {
  return (typeof value === 'string' && value.trim().replace(/\s+/g, ' ').slice(0, 60)) || TOOL_LABELS.monica_cli;
}

export type CliQueryDetails = { queries: string[]; omittedQueries: number };

function lookupDispatcher(signal: AbortSignal) {
  let lookups = 0;
  let succeeded = 0;
  let failed = 0;
  let traceBytes = 0;
  const details: CliQueryDetails = { queries: [], omittedQueries: 0 };
  const { directory, directory_facets: facets } = directoryCommands(signal);

  function recordQuery(name: string, args: CommandArgs) {
    const query = `cli(${JSON.stringify(name)}, ${JSON.stringify(args)})`;
    const bytes = encoder.encode(query).length;
    if (traceBytes + bytes > MAX_TRACE_BYTES) {
      details.omittedQueries++;
      return;
    }
    traceBytes += bytes;
    details.queries.push(query);
  }

  async function lookup(name: unknown, input: unknown): Promise<unknown> {
    signal.throwIfAborted();
    if (++lookups > MAX_LOOKUPS) throw new Error('本脚本的查询次数已用完。请分批查询。');
    const args = object(input);
    if (name === 'directory' || name === 'directory_facets') {
      recordQuery(name, args);
      return name === 'directory' ? directory(args) : facets(args);
    }
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
    recordQuery(tool.name, validated);
    const result = await tool.execute(id, validated, signal);
    const output = result.content.find(part => part.type === 'text');
    if (!output || output.type !== 'text') throw new Error('查询没有返回文字结果。');
    return JSON.parse(output.text);
  }

  async function checked(name: unknown, args: unknown) {
    try {
      const result = await lookup(name, args);
      if (encoder.encode(JSON.stringify(result)).length > MAX_LOOKUP_BYTES) {
        throw new Error('单次查询结果过长。请缩小时间范围、减少分页大小或限制搜索词。');
      }
      if (name !== 'help') {
        if (name === 'search_messages' && object(result).error) failed++;
        else succeeded++;
      }
      return result;
    } catch (error) { signal.throwIfAborted(); return failure(error); }
  }

  function failure(error: unknown) {
    failed++;
    return { error: errorText(error) };
  }

  async function dispatch(name: string, json: string) {
    if (json.length > 65_536) throw new Error('查询参数过长。');
    const args: unknown = JSON.parse(json);
    if (name !== 'batch') return checked(name, args);
    if (!Array.isArray(args) || args.length > BATCH_SIZE) {
      throw new Error('batch 分片需要命令数组。');
    }
    return Promise.all(args.map(async item => {
      try {
        const entry = object(item);
        return await checked(entry.command, entry.args ?? {});
      } catch (error) { signal.throwIfAborted(); return failure(error); }
    }));
  }

  return { dispatch, failure, details, allFailed: () => succeeded === 0 && failed > 0 };
}

function runScript(source: string, signal?: AbortSignal): Promise<{ text: string; isError: boolean; details: CliQueryDetails }> {
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
    function fail(error: unknown) {
      cleanup();
      if (signal?.aborted) reject(error);
      else resolve({ text: errorText(error), isError: true, details: lookup.details });
    }
    function abort() { fail(signal?.reason); }
    signal?.addEventListener('abort', abort, { once: true });
    worker.onerror = () => fail(new Error('无法启动 JavaScript 查询环境。'));
    worker.onmessage = async ({ data }) => {
      if (data.type === 'error') { fail(new Error(errorText(data.error))); return; }
      if (data.type === 'result') {
        if (encoder.encode(data.json).length > MAX_OUTPUT_BYTES) {
          fail(new Error('脚本输出过长。请先在 JavaScript 中筛选或汇总，并保留必要的原文链接。'));
        } else {
          cleanup();
          resolve({ text: data.json, isError: lookup.allFailed(), details: lookup.details });
        }
        return;
      }
      if (data.type === 'lookup') {
        let json: string;
        try { json = JSON.stringify(await lookup.dispatch(data.name, data.json)); }
        catch (error) { json = JSON.stringify(lookup.failure(error)); }
        if (!controller.signal.aborted) worker.postMessage({ json });
      }
    };
    worker.postMessage({ source, batchSize: BATCH_SIZE, maxLookups: MAX_LOOKUPS });
  });
}

export const cliTool: AgentTool = {
  name: 'monica_cli',
  label: TOOL_LABELS.monica_cli,
  description: [
    '运行一段 JavaScript 查询 Monica。用 return 返回可序列化数据。在脚本里直接调用同步函数 cli(command, args)，可以用循环、filter、map、sort 做批量查询和归纳。',
    'directory 参数 query、type（person/group/all）、tags（全部匹配的名称或 ID 数组）、company、school、provider、sort（relevance/name/last_contact/messages/birthday）、limit（至多 500）、offset。返回 rows、total、next_offset。',
    'directory_facets 参数 kind（tags/companies/schools）、query、limit、offset，可发现筛选值。',
    'batch 接受 [{command,args}, ...]，每项独立返回数据或 {error}，结果顺序同输入。脚本至多执行 240 项查询。大批量时用 cli("batch", commands, (result, index) => ...) 逐项提取必要字段，避免把所有原始记录留在内存。',
    '不确定某项记录查询的参数时，用 cli("help", {command:"命令名"}) 查看参数定义。',
    ...[...commands.values()].map(command => {
      const schema = command.parameters as { properties?: Record<string, unknown>; required?: string[] };
      return `${command.name}(${Object.keys(schema.properties ?? {}).map(key => schema.required?.includes(key) ? key : `${key}?`).join(', ')}) ${command.description}`;
    }),
  ].join('\n'),
  parameters: {
    type: 'object', properties: {
      title: { type: 'string', description: '用简短中文概括这段脚本查什么，例如「筛选同事并读取近况」。' },
      script: { type: 'string', description: 'JavaScript 函数体，最后 return 查询结果。' },
    },
    required: ['title', 'script'], additionalProperties: false,
  } as unknown as AgentTool['parameters'],
  prepareArguments(input) {
    const args = object(input);
    return { ...args, title: cliTitle(args.title) };
  },
  async execute(_id, args, signal) {
    const { script } = args as { script: string };
    const { text, isError, details } = await runScript(script, signal);
    return { content: [{ type: 'text', text }], details, isError };
  },
};
