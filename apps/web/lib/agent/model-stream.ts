import { streamSimple } from '@earendil-works/pi-ai/api/openai-completions';
import { AssistantMessageEventStream } from '@earendil-works/pi-ai/utils/event-stream';
import { apiFetch } from '../api';
import { MAX_CONNECTION_RETRIES, retryDelay, temporaryStatus } from './requests';

// Pi reports interrupted response bodies through the terminal stream event.
const INTERRUPTED_STREAM = [
  /network[ _-]?error|failed to fetch|load failed|connection[ _-](?:error|(?:was )?closed|(?:was )?reset|lost)|socket hang up|timed? ?out|econnreset|etimedout/i,
  /stream (?:ended without finish_reason|closed|interrupted|disconnected)|premature close|terminated|unexpected (?:end|eof)/i,
];
const MAX_CONTEXT_REDUCTIONS = 3;
const MAX_RETRY_WAIT = 30_000;
type Context = Parameters<typeof streamSimple>[1];

function retryAfter(value: string | null) {
  if (!value) return undefined;
  const seconds = Number(value);
  const delay = Number.isFinite(seconds) ? seconds * 1000 : Date.parse(value) - Date.now();
  return Number.isFinite(delay) ? Math.max(0, delay) : undefined;
}

export function streamModel(
  model: Parameters<typeof streamSimple>[0],
  context: Context,
  options: Parameters<typeof streamSimple>[2],
  recovery: {
    onRetry: (attempt: number, limit: number) => void;
    compact: (context: Context) => Context | undefined;
  },
) {
  const output = new AssistantMessageEventStream();
  const signal = options?.signal;
  async function pipe() {
    let started = false;
    let attempts = 0;
    let contextReductions = 0;
    let current = context;
    attempts: for (;;) {
      signal?.throwIfAborted();
      let responseStatus: number | undefined;
      let networkFailure = false;
      let retryWait: number | undefined;
      const source = streamSimple(model, current, {
        ...options,
        apiKey: 'local-session',
        // One retry budget covers both opening and reading the response.
        maxRetries: 0,
        fetch: async (input, init) => {
          try {
            const response = await apiFetch(input, { ...init, propagateCsrfFailure: true });
            responseStatus = response.status;
            retryWait = retryAfter(response.headers.get('retry-after'));
            return response;
          } catch (error) {
            networkFailure = error instanceof TypeError;
            throw error;
          }
        },
      });
      for await (const event of source) {
        if (event.type === 'error' && event.reason === 'error') {
          if (responseStatus === 413 && contextReductions < MAX_CONTEXT_REDUCTIONS && !signal?.aborted) {
            const next = recovery.compact(current);
            if (next) {
              current = next;
              contextReductions++;
              continue attempts;
            }
          }
          const streamFailed = (responseStatus === undefined || responseStatus === 200) &&
            (event.error.rawStopReason === 'network_error' ||
              INTERRUPTED_STREAM.some(pattern => pattern.test(event.error.errorMessage ?? '')));
          const interrupted = networkFailure || streamFailed || (responseStatus !== undefined && temporaryStatus(responseStatus));
          const longWait = (retryWait ?? 0) > MAX_RETRY_WAIT;
          if (interrupted && !signal?.aborted && !longWait && attempts < MAX_CONNECTION_RETRIES) {
            recovery.onRetry(attempts + 1, MAX_CONNECTION_RETRIES);
            await retryDelay(attempts, signal, retryWait === undefined ? undefined : Math.max(250, retryWait));
            attempts++;
            continue attempts;
          }
          const errorMessage = responseStatus === 413 ? '模型上下文超出上限。本轮进度已保存。'
            : interrupted ? (longWait ? '模型服务繁忙，需要稍后再试。本轮进度已保存。'
              : `模型连接持续失败，已自动重试 ${attempts} 次。本轮进度已保存。`)
              : event.error.errorMessage;
          output.push({ ...event, error: { ...event.error, content: [],
            errorMessage } });
          return;
        }
        // The agent appends an in-flight message on start. All attempts share that slot.
        if (event.type !== 'start' || !started) output.push(event);
        if (event.type === 'start') started = true;
        if (event.type === 'done' || event.type === 'error') return;
      }
      throw new Error('模型响应意外结束。');
    }
  }
  void pipe().catch(error => {
    const reason = signal?.aborted ? 'aborted' : 'error';
    output.push({ type: 'error', reason, error: {
      role: 'assistant', content: [], api: model.api, provider: model.provider, model: model.id,
      usage: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, totalTokens: 0,
        cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 } },
      stopReason: reason, errorMessage: error instanceof Error ? error.message : String(error), timestamp: Date.now(),
    } });
  });
  return output;
}
