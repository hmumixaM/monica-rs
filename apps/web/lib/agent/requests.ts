import { apiFetch } from '../api';

const MAX_HTTP_RETRIES = 2;
export const MAX_CONNECTION_RETRIES = 10;

export function temporaryStatus(status: number) {
  return status === 408 || status === 429 || (status >= 500 && status < 600);
}

export function retryDelay(attempt: number, signal?: AbortSignal, delay = Math.min(12_000, 500 * 2 ** attempt)) {
  signal?.throwIfAborted();
  return new Promise<void>((resolve, reject) => {
    const timer = setTimeout(() => {
      signal?.removeEventListener('abort', abort);
      resolve();
    }, delay);
    function abort() {
      clearTimeout(timer);
      reject(signal?.reason);
    }
    signal?.addEventListener('abort', abort, { once: true });
  });
}

export async function readJson<T = any>(path: string, signal?: AbortSignal, failure = '查询失败'): Promise<T> {
  for (let attempt = 0; ; attempt++) {
    signal?.throwIfAborted();
    try {
      const response = await apiFetch(path, { signal });
      if (response.ok) return await response.json();
      void response.body?.cancel().catch(() => undefined);
      if (response.status === 401) throw new Error('Monica 登录状态已失效，请重新登录。');
      if (attempt >= MAX_HTTP_RETRIES || !temporaryStatus(response.status)) {
        throw new Error(`${failure} (HTTP ${response.status})`);
      }
    } catch (error) {
      signal?.throwIfAborted();
      if (!(error instanceof TypeError)) throw error;
      if (attempt >= MAX_CONNECTION_RETRIES) throw new Error(`${failure}，连接中断。结果未知。`);
    }
    await retryDelay(attempt, signal, Math.min(1_000, 250 * 2 ** attempt));
  }
}
