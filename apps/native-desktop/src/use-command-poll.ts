import { useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";

/** Schedule after completion so a slow native operation never queues more IPC. */
export function useCommandPoll<T>(
  command: string,
  onValue: (value: T) => void,
  onError?: (message: string) => void,
) {
  useEffect(() => {
    let active = true;
    let pending = false;
    let timer: ReturnType<typeof setTimeout> | undefined;

    async function read() {
      if (!active || pending) return;
      clearTimeout(timer);
      if (document.visibilityState === "visible") {
        pending = true;
        try {
          const value = await invoke<T>(command);
          if (active) onValue(value);
        } catch (error) {
          if (active) onError?.(String(error));
        } finally {
          pending = false;
        }
      }
      if (active) timer = setTimeout(() => void read(), 5000);
    }

    function becameVisible() {
      if (document.visibilityState === "visible") void read();
    }
    document.addEventListener("visibilitychange", becameVisible);
    void read();
    return () => {
      active = false;
      clearTimeout(timer);
      document.removeEventListener("visibilitychange", becameVisible);
    };
  }, [command, onValue, onError]);
}
