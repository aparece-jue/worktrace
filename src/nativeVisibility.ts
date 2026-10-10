import { domainState } from "./state/domainState";

/** Combine native minimize/restore with document visibility; focus loss is not hiding. */
export async function installNativeVisibility(): Promise<() => void> {
  if (!("__TAURI_INTERNALS__" in window)) return () => {};
  const { getCurrentWindow } = await import("@tauri-apps/api/window");
  const current = getCurrentWindow();
  let disposed = false;
  let request = 0;
  const refresh = async () => {
    const token = ++request;
    const minimized = await current.isMinimized();
    if (!disposed && token === request) domainState.setWindowVisible(!minimized);
  };
  const off = await current.onResized(() => { void refresh().catch(console.warn); });
  try { await refresh(); }
  catch (error) { disposed = true; off(); throw error; }
  return () => { disposed = true; ++request; off(); domainState.setWindowVisible(true); };
}
