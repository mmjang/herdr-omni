import { BoxRenderable, InputRenderable, InputRenderableEvents, TextRenderable, StyledText, fg, bold, type CliRenderer } from "@opentui/core";
import type { CommandResult, PaletteItem } from "./types";
import { fallbackTheme, type PaletteTheme } from "./theme";
import { viewport } from "./viewport";
import { filterPaletteItems, searchResults, matchingPositions } from "./search";
import { version } from "../package.json";
import type { UpdateOffer } from "./update";
export { filterPaletteItems } from "./search";

export interface PaletteDeps { /** Herdr's live palette; omit for the built-in catppuccin fallback. */ theme?: PaletteTheme; history?: Record<string, number>; run: (item: PaletteItem, input?: string) => Promise<CommandResult>; close: () => void; update?: (offer: UpdateOffer) => Promise<CommandResult>; dismissUpdate?: (offer: UpdateOffer) => void }

/** Rows the chrome always owns: heading, input, the blank line below it, the footer bar. */
const CHROME_ROWS = 4;

export function mountPalette(renderer: CliRenderer, allItems: PaletteItem[], deps: PaletteDeps) {
  const theme = deps.theme ?? fallbackTheme;
  let query = "", selected = 0, status = "", running = false;
  let promptItem: PaletteItem | undefined;
  let promptValue = "";
  let panel: BoxRenderable | undefined;
  let activeInput: InputRenderable | undefined;
  let loading = false;
  let refreshError = false;
  let interacted = false;
  let updateOffer: UpdateOffer | undefined;
  let updateDialog = false;
  let updateSelected = false; // Default to Later; Enter must never silently approve an upgrade.
  let updating = false;
  let updated = false;
  let updateError = "";
  let destroyed = false;
  renderer.on("destroy", () => { destroyed = true; });

  const visibleItems = () => filterPaletteItems(allItems, query, deps.history);
  const prompting = () => promptItem !== undefined;

  function redraw(preserveCursor = false) {
    if (destroyed) return;
    const cursor = preserveCursor ? activeInput?.cursorOffset : undefined;
    panel?.destroy();
    const results = searchResults(allItems, query, deps.history);
    const items = results.map(result => result.item);
    selected = Math.max(0, Math.min(selected, items.length - 1));
    panel = new BoxRenderable(renderer, { id: "palette", flexDirection: "column", width: "100%", height: "100%", backgroundColor: theme.background });
    const body = new BoxRenderable(renderer, { id: "body", flexDirection: "column", flexGrow: 1, paddingLeft: 2, paddingRight: 2 });
    panel.add(body);
    const heading = new BoxRenderable(renderer, { id: "heading", flexDirection: "row" });
    heading.add(new TextRenderable(renderer, { id: "title", content: prompting() ? promptItem!.title : "Herdr Omni", fg: theme.text, attributes: 1 }));
    heading.add(new TextRenderable(renderer, { id: "version", content: `  v${version}`, fg: theme.muted, flexGrow: 1 }));
    heading.add(new TextRenderable(renderer, { id: "escape", content: updating ? "" : "esc", fg: theme.muted }));
    body.add(heading);
    if (updateDialog && updateOffer) {
      activeInput = undefined;
      const dialog = new BoxRenderable(renderer, { id: "update-dialog", flexDirection: "column", flexGrow: 1, paddingTop: 1 });
      dialog.add(new TextRenderable(renderer, { id: "update-title", content: updated ? `Updated to v${updateOffer.version}` : `Herdr Omni v${updateOffer.version} is available`, fg: theme.accent, attributes: 1 }));
      dialog.add(new TextRenderable(renderer, { id: "update-description", content: updated ? "Reopen Omni to use the new version." : updating ? "Installing update…" : `Upgrade from v${version}? Your settings and history will be kept.`, fg: theme.text }));
      if (updateError) dialog.add(new TextRenderable(renderer, { id: "update-error", content: updateError.replace(/\s+/g, " ").slice(0, 300), fg: theme.accent }));
      const buttons = new BoxRenderable(renderer, { id: "update-buttons", flexDirection: "row", marginTop: 1 });
      const button = (id: string, label: string, selected: boolean, action: () => void) => {
        const control = new TextRenderable(renderer, { id, content: ` ${selected ? "[" : " "}${label}${selected ? "]" : " "} `, fg: selected ? theme.accent : theme.muted, attributes: selected ? 1 : 0 });
        let pressed = false;
        control.onMouseDown = event => { if (event.button === 0) { pressed = true; event.preventDefault(); } };
        control.onMouseDrag = () => { pressed = false; };
        control.onMouseOut = () => { pressed = false; };
        control.onMouseUp = event => { const activate = pressed && event.button === 0 && !updating; pressed = false; if (activate) action(); };
        buttons.add(control);
      };
      if (updated) button("update-close", "Close", true, deps.close);
      else if (!updating) {
        button("update-install", updateError ? "Retry" : "Update", updateSelected, () => { void performUpdate(); });
        button("update-later", "Later", !updateSelected, dismissUpgrade);
      }
      dialog.add(buttons);
      body.add(dialog);
      const footer = new BoxRenderable(renderer, { id: "update-footer", height: 1, flexShrink: 0, backgroundColor: theme.footer });
      footer.add(new TextRenderable(renderer, { id: "update-footer-text", content: updating ? "  Please wait for installation to finish." : updated ? "  enter/esc close" : "  ←/→ or tab choose · enter confirm · esc later", fg: theme.footerText }));
      panel.add(footer);
      renderer.root.add(panel);
      return;
    }
    const input = new InputRenderable(renderer, {
      id: "search",
      value: prompting() ? promptValue : query,
      placeholder: prompting() ? promptItem!.prompt!.placeholder : "Search · @ workspaces · > agents · : actions",
      backgroundColor: theme.background,
      focusedBackgroundColor: theme.background,
      textColor: theme.text,
      cursorColor: theme.accent,
    });
    activeInput = input;
    input.on(InputRenderableEvents.INPUT, (value: string) => {
      interacted = true;
      if (prompting()) promptValue = value;
      else { query = value; selected = 0; }
      status = "";
      redraw();
    });
    body.add(input); input.focus();
    if (cursor !== undefined) input.cursorOffset = cursor;
    const list = new BoxRenderable(renderer, { id: "list", flexDirection: "column", flexGrow: 1, marginTop: 1 });
    body.add(list);
    if (prompting()) {
      list.add(new TextRenderable(renderer, { id: "prompt-hint", content: promptItem!.description, fg: theme.muted }));
    } else if (items.length === 0) {
      list.add(new TextRenderable(renderer, { id: "empty", content: loading ? "Loading live results…" : "No results match your search.", fg: theme.muted }));
    } else {
      const window = viewport(results, selected, Math.max(1, renderer.height - CHROME_ROWS - (status ? 1 : 0) - (updateOffer ? 1 : 0)), result => result.section);
      let category = "";
      items.slice(window.start, window.end).forEach((item, offset) => {
        const index = window.start + offset;
        if (results[index]!.section !== category) {
          category = results[index]!.section;
          list.add(new TextRenderable(renderer, { id: `category-${index}`, content: category, fg: theme.accent, attributes: 1 }));
        }
        const row = new BoxRenderable(renderer, { id: `item-${index}`, flexDirection: "row", width: "100%", paddingLeft: 1, paddingRight: 2, backgroundColor: index === selected ? theme.panel : theme.background });
        let pressed = false;
        row.onMouseDown = event => {
          if (event.button !== 0 || running) return;
          interacted = true;
          pressed = true;
          event.preventDefault();
        };
        row.onMouseDrag = () => { pressed = false; };
        row.onMouseOut = () => { pressed = false; };
        row.onMouseUp = event => {
          if (event.button !== 0 || !pressed) return;
          pressed = false;
          event.preventDefault();
          event.stopPropagation();
          if (running || prompting()) return;
          // Resolve the rendered identity, not an index that live refresh may have changed.
          const currentIndex = visibleItems().findIndex(candidate => candidate.id === item.id);
          if (currentIndex < 0) return;
          selected = currentIndex;
          void select();
        };
        row.add(new TextRenderable(renderer, { id: `mark-${index}`, content: index === selected ? "┃" : " ", fg: theme.accent }));
        const positions = matchingPositions(query, item.title);
        const normalColor = index === selected ? theme.text : theme.muted;
        const content = new StyledText([
          fg(normalColor)(`${item.icon}  `),
          ...[...item.title].map((char, i) => positions.has(i) ? bold(fg(theme.accent)(char)) : fg(normalColor)(char)),
        ]);
        row.add(new TextRenderable(renderer, { id: `label-${index}`, content, fg: normalColor, flexGrow: 1 }));
        row.add(new TextRenderable(renderer, {
          id: `key-${index}`,
          content: item.agentStatus ? ` [${item.agentStatus}]` : item.shortcuts.join(" / "),
          fg: item.agentStatus === "unknown" ? theme.muted : index === selected || item.agentStatus ? theme.accent : theme.shortcut,
          flexShrink: 0,
        }));
        list.add(row);
      });
    }
    if (status) body.add(new TextRenderable(renderer, { id: "status", content: status.replace(/\s+/g, " ").slice(0, Math.max(20, renderer.width - 4)), fg: theme.accent }));
    if (updateOffer) {
      const notice = new TextRenderable(renderer, { id: "update-notice", content: `v${updateOffer.version} available · ctrl+u to review update`, fg: theme.accent });
      notice.onMouseUp = event => { if (event.button === 0 && !running) openUpgrade(); };
      body.add(notice);
    }
    panel.add(footerBar(prompting() ? 0 : items.length));
    renderer.root.add(panel);
  }

  function footerBar(count: number) {
    const bar = new BoxRenderable(renderer, { id: "footer", flexDirection: "row", width: "100%", flexShrink: 0, backgroundColor: theme.footer, paddingLeft: 2, paddingRight: 2 });
    const key = (id: string, content: string) => new TextRenderable(renderer, { id, content, fg: theme.accent, attributes: 1 });
    const label = (id: string, content: string, grow = false) => new TextRenderable(renderer, { id, content, fg: theme.footerText, flexGrow: grow ? 1 : 0 });
    if (prompting()) {
      bar.add(key("footer-enter", "enter")); bar.add(label("footer-select", " confirm   "));
      bar.add(key("footer-esc", "esc")); bar.add(label("footer-back", " back", true));
    } else {
      bar.add(key("footer-enter", "enter/click")); bar.add(label("footer-select", " select   "));
      bar.add(key("footer-arrows", "↑/↓")); bar.add(label("footer-move", " move", true));
      bar.add(label("footer-count", loading ? "Loading…" : refreshError ? "Refresh unavailable" : `${count} results`));
    }
    return bar;
  }

  function cancelPrompt() {
    promptItem = undefined;
    promptValue = "";
    status = "";
    redraw();
  }

  function openUpgrade() {
    if (!updateOffer || !deps.update || running) return;
    updateDialog = true;
    updateSelected = false;
    activeInput?.blur();
    redraw();
  }

  function dismissUpgrade() {
    if (updating) return;
    if (updateOffer) deps.dismissUpdate?.(updateOffer);
    updateOffer = undefined;
    updateDialog = false;
    updateError = "";
    redraw();
  }

  async function performUpdate() {
    if (updating || !updateOffer || !deps.update) return;
    updating = true;
    updateError = "";
    redraw();
    try {
      const result = await deps.update(updateOffer);
      if (result.ok) updated = true;
      else updateError = result.message;
    } catch (error) {
      updateError = error instanceof Error ? error.message : String(error);
    } finally {
      updating = false;
    }
    redraw();
  }

  async function run(item: PaletteItem, input?: string) {
    running = true;
    try {
      const result = await deps.run(item, input);
      if (result.ok) return deps.close();
      status = result.message;
    } catch (error) {
      status = error instanceof Error ? error.message : String(error);
    } finally {
      running = false;
    }
    redraw();
  }

  async function select() {
    interacted = true;
    if (prompting()) return run(promptItem!, promptValue);
    const item = visibleItems()[selected];
    if (!item) { status = "No results match your search."; return redraw(); }
    if (item.prompt) {
      promptItem = item;
      promptValue = "";
      status = "";
      return redraw();
    }
    return run(item);
  }

  renderer.keyInput.on("keypress", async key => {
    if (updateDialog) {
      if (updating) return;
      if (updated) { if (key.name === "return" || key.name === "escape") deps.close(); return; }
      if (key.name === "escape") return dismissUpgrade();
      if (["tab", "left", "right"].includes(key.name)) {
        updateSelected = key.name === "left" ? true : key.name === "right" ? false : !updateSelected;
        return redraw();
      }
      if (key.name === "return") return updateSelected ? performUpdate() : dismissUpgrade();
      return;
    }
    if (key.ctrl && key.name === "u" && updateOffer) return openUpgrade();
    if (["up", "down", "left", "right", "home", "end"].includes(key.name) || (key.ctrl && ["p", "n"].includes(key.name))) interacted = true;
    if (key.name === "escape") return prompting() ? cancelPrompt() : deps.close();
    if (prompting()) {
      if (key.name === "return" && !running) return select();
      return;
    }
    const total = visibleItems().length;
    if (key.name === "up" || (key.ctrl && key.name === "p")) { selected = (selected - 1 + total) % Math.max(1, total); return redraw(); }
    if (key.name === "down" || (key.ctrl && key.name === "n")) { selected = (selected + 1) % Math.max(1, total); return redraw(); }
    if (key.name === "return" && !running) return select();
  });

  redraw();
  return {
    offerUpdate(offer: UpdateOffer) {
      if (destroyed || !deps.update || updated || updateOffer) return;
      updateOffer = offer;
      if (!interacted && !query && !prompting() && !running) openUpgrade();
      else redraw(true);
    },
    setLoading(value: boolean) { loading = value; redraw(true); },
    refreshFailed() { loading = false; refreshError = true; if (!prompting() && !running) redraw(true); },
    updateItems(items: PaletteItem[]) {
      const selectedId = visibleItems()[selected]?.id;
      const changed = JSON.stringify(items) !== JSON.stringify(allItems) || loading || refreshError;
      allItems = items;
      loading = false;
      refreshError = false;
      const nextIndex = visibleItems().findIndex(item => item.id === selectedId);
      if (!interacted) selected = 0;
      else if (nextIndex >= 0) selected = nextIndex;
      if (changed && !prompting() && !running) redraw(true);
    },
  };
}
