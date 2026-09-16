import { createCliRenderer } from "@opentui/core";
import { loadPaletteItems } from "./config";
import { execute } from "./execute";
import { mountPalette } from "./palette";
import { loadTheme } from "./theme";
import { loadLiveItems } from "./live";
import { loadHistory, recordSelection } from "./history";
import { startLiveRefresh } from "./refresh";
import { CATEGORY_ORDER } from "./constants";
import { checkForUpdate, dismissUpdate, installUpdate } from "./update";
import { version } from "../package.json";
import { runSessionJob } from "./sessions";
import { parseLaunchContext } from "./herdr";

// Read Herdr's config before drawing so the popup uses the theme Herdr itself is rendering with.
const theme = loadTheme();
const items = loadPaletteItems();
const renderer = await createCliRenderer({ exitOnCtrlC: true, useMouse: true, backgroundColor: theme.background });
const palette = mountPalette(renderer, items, { theme, history: loadHistory(), currentWorkspaceId: parseLaunchContext()?.workspaceId, run: async (item, input) => {
  const result = await execute(item, input);
  if (result.ok && item.category !== "Actions") recordSelection(item.id);
  return result;
}, close: () => renderer.destroy(), update: installUpdate, dismissUpdate, sessionJob: runSessionJob });
palette.setLoading(true);
let firstLoad = true;
const stopRefresh = startLiveRefresh(async publish => {
  const liveItems = await loadLiveItems(firstLoad ? publish : undefined);
  firstLoad = false;
  return liveItems;
}, liveItems => {
  palette.updateItems([...items, ...liveItems].sort((left, right) => CATEGORY_ORDER.indexOf(left.category) - CATEGORY_ORDER.indexOf(right.category)));
}, () => palette.refreshFailed());
renderer.on("destroy", stopRefresh);
const updateCheck = new AbortController();
renderer.on("destroy", () => updateCheck.abort());
void checkForUpdate(version, updateCheck.signal).then(offer => {
  if (offer && !updateCheck.signal.aborted) palette.offerUpdate(offer);
}).catch(() => { /* Updates must never prevent normal navigation. */ });
