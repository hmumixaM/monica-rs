import { useEffect, useRef, useState } from 'react';
import { AskError, readConversation, type Message, type Run } from './sessions';

const empty = { id: '', messages: [] as Message[], run: null as Run | null, loaded: false, failure: '' };

export function useConversation(id: string) {
  const [view, setView] = useState(empty);
  const refresh = useRef<() => void>(() => undefined);

  useEffect(() => {
    if (!id) return;
    const controller = new AbortController();
    let cursor = 0;
    let messages: Message[] = [];
    let timer: ReturnType<typeof setTimeout>;
    let polling = false;
    let requested = false;
    let errors = 0;
    let active = false;
    async function poll() {
      if (controller.signal.aborted) return;
      clearTimeout(timer);
      if (polling) { requested = true; return; }
      polling = true;
      try {
        let page;
        do {
          page = await readConversation(id, cursor, controller.signal);
          if (controller.signal.aborted) return;
          if (page.rows.length) messages = [...messages, ...page.rows.map(({ message }) => message)];
          cursor = page.rows.at(-1)?.seq ?? cursor;
        } while (page.more);
        errors = 0;
        active = page.chat.running;
        setView(previous => {
          const run = page.run;
          const same = previous.id === id && previous.messages === messages && !previous.failure
            && previous.run?.id === run?.id && previous.run?.status === run?.status
            && previous.run?.progress === run?.progress && previous.run?.error === run?.error
            && previous.run?.model === run?.model && JSON.stringify(previous.run?.draft) === JSON.stringify(run?.draft);
          return same ? previous : { id, messages, run, loaded: true, failure: '' };
        });
      } catch (error) {
        if (controller.signal.aborted) return;
        if (error instanceof AskError && (error.status === 401 || error.status === 404)) {
          setView({ ...empty, id, failure: error.message });
          controller.abort();
          return;
        }
        errors++;
        setView(previous => ({
          ...(previous.id === id ? previous : empty), id,
          failure: active ? '暂时无法同步进度，正在重连。' : '暂时无法加载会话，正在重连。',
        }));
      } finally {
        polling = false;
        if (!controller.signal.aborted) {
          timer = setTimeout(poll, requested ? 0 : errors ? Math.min(5000, 500 * 2 ** errors) : active ? 800 : 3500);
          requested = false;
        }
      }
    }
    refresh.current = () => { void poll(); };
    const resume = () => { if (document.visibilityState === 'visible') refresh.current(); };
    document.addEventListener('visibilitychange', resume);
    window.addEventListener('online', resume);
    refresh.current();
    return () => {
      controller.abort();
      clearTimeout(timer);
      refresh.current = () => undefined;
      window.removeEventListener('online', resume);
      document.removeEventListener('visibilitychange', resume);
    };
  }, [id]);

  const current = view.id === id ? view : empty;
  return { ...current, refresh: () => refresh.current() };
}
