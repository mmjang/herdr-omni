import type { PaletteItem } from "./types";

/** Poll sequentially; closing prevents both rescheduling and late UI updates. */
export function startLiveRefresh(load: (publish: (items: PaletteItem[]) => void) => Promise<PaletteItem[]>, publish: (items: PaletteItem[]) => void, failed: () => void, interval = 2000) {
  let stopped = false;
  let timer: ReturnType<typeof setTimeout> | undefined;
  const update = (items: PaletteItem[]) => { if (!stopped) publish(items); };
  async function refresh() {
    try { update(await load(update)); }
    catch { if (!stopped) failed(); }
    finally { if (!stopped) timer = setTimeout(refresh, interval); }
  }
  void refresh();
  return () => { stopped = true; clearTimeout(timer); };
}
