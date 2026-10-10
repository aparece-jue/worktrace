import { useEffect } from "react";

/** The backend owns calendar boundaries; this timer only invalidates an expired sample. */
export function useDayBoundaryRefresh(end: number | undefined, refresh: () => Promise<void>): void {
  useEffect(() => {
    if (end === undefined) return;
    let disposed = false;
    let timer: ReturnType<typeof setTimeout>;
    const schedule = (): void => {
      timer = setTimeout(() => {
        if (disposed) return;
        if (Date.now() >= end) {
          void refresh();
          // A failed query must not produce a tight retry loop.
          timer = setTimeout(schedule, 60_000);
        } else {
          schedule();
        }
      }, Math.max(1, Math.min(60_000, end - Date.now())));
    };
    schedule();
    return () => { disposed = true; clearTimeout(timer); };
  }, [end, refresh]);
}
