import { createCliRenderer } from "@opentui/core";
import { loadPaletteItems } from "./config";
import { execute } from "./execute";
import { mountPalette } from "./palette";
import { loadTheme } from "./theme";
import { loadLiveItems } from "./live";
import { loadHistory, recordSelection } from "./history";
import { startLiveRefresh } from "./refresh";

// Read Herdr's config before drawing so the popup uses the theme Herdr itself is rendering with.
const theme = loadTheme();
const categoryOrder = ["Actions", "Workspace", "Tabs", "Worktrees", "Agents", "Custom"];
const items = loadPaletteItems();
const renderer = await createCliRenderer({ exitOnCtrlC: true, backgroundColor: theme.background });
const palette = mountPalette(renderer, items, { theme, history: loadHistory(), run: async (item, input) => {
  const result = await execute(item, input);
  if (result.ok && item.category !== "Actions") recordSelection(item.id);
  return result;
}, close: () => renderer.destroy() });
palette.setLoading(true);
let firstLoad = true;
const stopRefresh = startLiveRefresh(async publish => {
  const liveItems = await loadLiveItems(firstLoad ? publish : undefined);
  firstLoad = false;
  return liveItems;
}, liveItems => {
  palette.updateItems([...items, ...liveItems].sort((left, right) => categoryOrder.indexOf(left.category) - categoryOrder.indexOf(right.category)));
}, () => palette.refreshFailed());
renderer.on("destroy", stopRefresh);
