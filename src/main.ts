import { createCliRenderer } from "@opentui/core";
import { loadPaletteItems } from "./config";
import { execute } from "./execute";
import { mountPalette } from "./palette";
import { loadTheme } from "./theme";
import { loadLiveItems } from "./live";
import { loadHistory, recordSelection } from "./history";

// Read Herdr's config before drawing so the popup uses the theme Herdr itself is rendering with.
const theme = loadTheme();
const categoryOrder = ["Actions", "Workspace", "Tabs", "Worktrees", "Agents", "Custom"];
const items = [...loadPaletteItems(), ...await loadLiveItems()]
  .sort((left, right) => categoryOrder.indexOf(left.category) - categoryOrder.indexOf(right.category));
const renderer = await createCliRenderer({ exitOnCtrlC: true, backgroundColor: theme.background });
mountPalette(renderer, items, { theme, history: loadHistory(), run: async (item, input) => {
  const result = await execute(item, input);
  if (result.ok && item.category !== "Actions") recordSelection(item.id);
  return result;
}, close: () => renderer.destroy() });
