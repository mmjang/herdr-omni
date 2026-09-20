import { BoxRenderable, InputRenderable, InputRenderableEvents, TextRenderable, StyledText, fg, bold, type CliRenderer } from "@opentui/core";
import type { CommandResult, PaletteItem, SavedSession, ResumeWorkspaceChoice } from "./types";
import { mergeSessions, savedSessionItem, sessionKey, cleanText, agentActivity, TRANSCRIPT_DEBOUNCE_MS, type SessionJob } from "./sessions";
import { activityAge } from "./activity";
import { transcriptPreview, TRANSCRIPT_PREVIEW_ROWS } from "./transcript-preview";
import { fallbackTheme, type PaletteTheme } from "./theme";
import { viewport } from "./viewport";
import { searchResults, matchingPositions } from "./search";
import { resultTabs, tabResults, tabMatchCounts, sectionLabel, type ResultRow } from "./result-tabs";
import { version } from "../package.json";
import type { UpdateOffer } from "./update";
import type { PanePreviewReader } from "./pane-preview";
import { paneScreen } from "./pane-screen";
export { filterPaletteItems } from "./search";

export interface PaletteDeps { /** Herdr's live palette; omit for the built-in catppuccin fallback. */ theme?: PaletteTheme; history?: Record<string, number>; currentWorkspaceId?: string; run: (item: PaletteItem, input?: string) => Promise<CommandResult>; close: () => void; update?: (offer: UpdateOffer) => Promise<CommandResult>; dismissUpdate?: (offer: UpdateOffer) => void; sessionJob?: SessionJob; panePreview?: PanePreviewReader }

/** Rows the chrome always owns: heading, input, the blank line below it, the footer bar. */
const CHROME_ROWS = 5;
export const PANE_PREVIEW_REFRESH_MS = 1000;

export function mountPalette(renderer: CliRenderer, allItems: PaletteItem[], deps: PaletteDeps) {
  const theme = deps.theme ?? fallbackTheme;
  let query = "", selected = 0, status = "", running = false;
  let activeTab = "All";
  let runningTitle = "", runningStarted = 0;
  let runningTimer: ReturnType<typeof setInterval> | undefined;
  let promptItem: PaletteItem | undefined;
  let workspacePicker: { item: PaletteItem; choices: ResumeWorkspaceChoice[]; selected: number; message: string } | undefined;
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
  let sessions: SavedSession[] = [];
  let sessionsStarted = false, sessionsLoading = false, transcripts = false;
  let sessionError = "", scanStatus = "", scanError = "";
  const sessionController = new AbortController();
  let scanController: AbortController | undefined;
  let scanTimer: ReturnType<typeof setTimeout> | undefined;
  let scanLoading = false;
  let loadingIndicator: TextRenderable | undefined;
  let loadingTick = 0;
  let loadingTimer: ReturnType<typeof setInterval> | undefined;
  const loadingText = () => `${["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"][loadingTick % 10]} Searching transcripts…  `;
  const stopLoading = () => {
    scanLoading = false;
    clearInterval(loadingTimer);
    loadingTimer = undefined;
    if (loadingIndicator) loadingIndicator.content = "";
  };
  let transcriptHits: Array<{ session: SavedSession; excerpt: string }> = [];
  let previewEnabled = true, previewExpanded = false;
  let previewOffset = 0, previewSelection = "";
  let previewKey = "", previewText = "", previewState = "";
  let previewController: AbortController | undefined;
  let previewTimer: ReturnType<typeof setTimeout> | undefined;
  const previewCache = new Map<string, { text: string; time: number }>();
  function stopPreview() { clearTimeout(previewTimer); previewController?.abort(); previewController = undefined; }
  function loadPreview(item?: PaletteItem) {
    const session = item?.session;
    const paneId = item?.savedSession ? undefined : item?.livePaneId;
    const identity = paneId ? `pane:${paneId}:${session ? sessionKey(session) : ""}` : session && deps.sessionJob ? `${sessionKey(session)}:${session.updatedAt}` : "";
    const key = previewEnabled && !transcripts && !prompting() && !running && !workspacePicker && !updateDialog ? identity : "";
    if (key === previewKey) return;
    stopPreview(); previewKey = key; previewText = ""; previewState = "";
    if (!key) return;
    if (paneId) {
      if (!deps.panePreview) { previewState = "Live pane preview unavailable"; return; }
      previewState = "Reading live pane…";
      const controller = new AbortController();
      previewController = controller;
      const poll = async () => {
        if (destroyed || controller.signal.aborted) return;
        let text = "", state = "";
        try { text = await deps.panePreview!(paneId, controller.signal); }
        catch { state = "Live pane unavailable · retrying…"; }
        if (destroyed || controller.signal.aborted || key !== previewKey) return;
        const changed = text !== previewText || state !== previewState;
        previewText = text; previewState = state;
        // Sequential reads never overlap. Hide/switch/close aborts the reader and timer.
        if (changed) redraw(true);
        if (!destroyed && !controller.signal.aborted) previewTimer = setTimeout(poll, PANE_PREVIEW_REFRESH_MS);
      };
      previewTimer = setTimeout(poll, TRANSCRIPT_DEBOUNCE_MS);
      return;
    }
    if (!session) return;
    const cached = previewCache.get(key);
    if (cached && Date.now() - cached.time < 30_000) { previewText = cached.text; return; }
    previewState = "Loading recent conversation…";
    const controller = new AbortController();
    previewController = controller;
    previewTimer = setTimeout(() => {
      void deps.sessionJob!({ type: "preview", session }, controller.signal, event => {
        if (destroyed || controller.signal.aborted || key !== previewKey) return;
        if (event.type === "preview" && sessionKey(event.session) === sessionKey(session)) {
          previewText = event.excerpt; previewState = "";
          if (previewCache.size >= 30) previewCache.delete(previewCache.keys().next().value!);
          previewCache.set(key, { text: event.excerpt, time: Date.now() });
        }
        if (event.type === "error") previewState = "Conversation unavailable";
        redraw(true);
      }).catch(() => { if (!controller.signal.aborted) previewState = "Conversation unavailable"; })
        .finally(() => {
          if (destroyed || controller.signal.aborted) return;
          if (!previewText && previewState === "Loading recent conversation…") previewState = "No conversation text available";
          redraw(true);
        });
    }, TRANSCRIPT_DEBOUNCE_MS);
  }
  const stopScan = () => { stopLoading(); clearTimeout(scanTimer); scanController?.abort(); scanController = undefined; };
  const transcriptQuery = () => (">@:".includes(query[0] ?? " ") ? query.slice(1) : query).trim();
  const sessionMetadataVisible = () => query.startsWith(">") || (!query.startsWith("@") && !query.startsWith(":"));
  const sessionSearchVisible = () => sessionMetadataVisible() || transcripts;
  renderer.on("destroy", () => { destroyed = true; clearInterval(runningTimer); sessionController.abort(); stopScan(); stopPreview(); });

  function allResults(): ResultRow[] {
    const combined = sessionMetadataVisible() ? mergeSessions(allItems, sessions) : allItems;
    const normal = searchResults(combined, ">@:".includes(query[0] ?? " ") ? query.slice(1) : query, deps.history);
    if (!transcripts) return normal;
    const ids = new Set(normal.map(result => result.item.id));
    const bySession = new Map(combined.flatMap(item => item.session ? [[sessionKey(item.session), item] as const] : []));
    return [...normal, ...transcriptHits.flatMap(hit => {
      const item = bySession.get(sessionKey(hit.session)) ?? savedSessionItem(hit.session);
      if (ids.has(item.id)) return [];
      ids.add(item.id);
      return [{ item, section: "Transcript matches" }];
    })];
  }
  const tabs = () => [...new Set([...resultTabs(allResults(), [...allItems, ...(sessions.length ? [savedSessionItem(sessions[0]!)] : [])]), activeTab])];
  const visibleResults = () => tabResults(allResults(), activeTab);
  const visibleItems = () => visibleResults().map(result => result.item);
  const prompting = () => promptItem !== undefined;

  function startTranscriptSearch() {
    if (transcripts || prompting() || running || updateDialog || workspacePicker || !transcriptQuery() || !deps.sessionJob) return;
    const id = visibleItems()[selected]?.id;
    transcripts = true;
    activeTab = "All";
    interacted = true;
    discoverSessions();
    scheduleScan();
    const next = visibleItems().findIndex(item => item.id === id);
    selected = next >= 0 ? next : 0;
    redraw(true);
  }

  function transcriptSearchLink(id: string, content: string) {
    const link = new TextRenderable(renderer, { id, content, fg: theme.accent, height: 1 });
    let pressed = false;
    link.onMouseDown = event => { if (event.button === 0) { pressed = true; event.preventDefault(); } };
    link.onMouseDrag = () => { pressed = false; };
    link.onMouseOut = () => { pressed = false; };
    link.onMouseUp = event => {
      const activate = pressed && event.button === 0;
      pressed = false;
      if (activate) { event.preventDefault(); event.stopPropagation(); startTranscriptSearch(); }
    };
    return link;
  }

  function switchTab(tab: string) {
    activeTab = tab;
    if (">@:".includes(query[0] ?? " ")) query = query.slice(1);
    selected = 0;
    status = "";
    interacted = true;
    discoverSessions();
    redraw();
  }

  function preserveSelection(change: () => void) {
    const previous = visibleItems()[selected];
    change();
    const next = visibleItems().findIndex(item => item.id === previous?.id || Boolean(previous?.session && item.session && sessionKey(item.session) === sessionKey(previous.session)));
    if (next >= 0) selected = next;
    if (!prompting() && !running && !updateDialog) redraw(true);
  }

  function scheduleScan() {
    stopScan();
    transcriptHits = [];
    scanStatus = "";
    scanError = "";
    if (!transcripts || !transcriptQuery() || !deps.sessionJob) return;
    scanLoading = true;
    loadingTick = 0;
    loadingTimer = setInterval(() => {
      loadingTick++;
      if (loadingIndicator && !destroyed) loadingIndicator.content = loadingText();
    }, 100);
    if (sessionsLoading) return;
    const controller = new AbortController();
    scanController = controller;
    scanStatus = "Searching transcripts…";
    scanTimer = setTimeout(() => {
      const scanSessions = [...new Map([...allItems.flatMap(item => item.session ? [item.session] : []), ...sessions]
        .map(session => [sessionKey(session), session])).values()].sort((a, b) => b.updatedAt - a.updatedAt);
      void deps.sessionJob!({ type: "search", sessions: scanSessions, query: transcriptQuery() }, controller.signal, event => {
        if (destroyed || controller.signal.aborted) return;
        preserveSelection(() => {
          if (event.type === "hit") transcriptHits.push(event);
          if (event.type === "progress") scanStatus = `Searching transcripts… ${event.scanned}/${event.total}`;
          if (event.type === "error") scanError = event.message;
          if (event.type === "done") { stopLoading(); scanStatus = event.limited ? "Transcript result limit reached" : "Transcript search complete"; }
        });
      }).catch(() => {
        if (!destroyed && !controller.signal.aborted) preserveSelection(() => { scanStatus = "Transcript search unavailable"; });
      }).finally(() => {
        if (!destroyed && !controller.signal.aborted) {
          stopLoading();
          if (!prompting() && !running && !updateDialog) redraw(true);
        }
      });
    }, TRANSCRIPT_DEBOUNCE_MS);
  }

  function discoverSessions() {
    if (!deps.sessionJob || sessionsStarted || !sessionSearchVisible()) return;
    sessionsStarted = sessionsLoading = true;
    void deps.sessionJob({ type: "list" }, sessionController.signal, event => {
      if (destroyed) return;
      preserveSelection(() => {
        if (event.type === "sessions") sessions = [...new Map([...sessions, ...event.sessions].map(session => [sessionKey(session), session])).values()].sort((a, b) => b.updatedAt - a.updatedAt);
        if (event.type === "error") sessionError = event.message;
      });
    }).catch(() => { sessionError = "Saved sessions unavailable"; }).finally(() => {
      if (destroyed) return;
      sessionsLoading = false;
      scheduleScan();
      if (!prompting() && !updateDialog) redraw(true);
    });
  }

  function redraw(preserveCursor = false) {
    if (destroyed) return;
    const cursor = preserveCursor ? activeInput?.cursorOffset : undefined;
    loadingIndicator = undefined;
    panel?.destroyRecursively();
    const results = visibleResults();
    const items = results.map(result => result.item);
    selected = Math.max(0, Math.min(selected, items.length - 1));
    const selectedItem = items[selected];
    const selectedSession = selectedItem?.session;
    const livePreview = !transcripts && !selectedItem?.savedSession && Boolean(selectedItem?.livePaneId);
    const selection = `${selectedItem?.id ?? ""}:${transcripts}`;
    if (previewSelection !== selection) { previewSelection = selection; previewOffset = 0; }
    loadPreview(selectedItem);
    const showPreview = previewEnabled && !prompting() && Boolean(selectedSession || livePreview);
    const sidePreview = showPreview && !previewExpanded && renderer.width >= 120;
    const previewWidth = sidePreview ? Math.floor((renderer.width - 4) * 0.42) : renderer.width - 4;
    const previewHeight = Math.max(3, Math.min(previewExpanded ? renderer.height - 9 : TRANSCRIPT_PREVIEW_ROWS + 3, Math.floor(renderer.height * (previewExpanded ? 0.65 : 0.45))));
    const excerpt = transcripts && selectedSession ? transcriptHits.find(hit => sessionKey(hit.session) === sessionKey(selectedSession))?.excerpt : previewText;
    const contentWidth = Math.max(1, previewWidth - 2);
    const contentRows = Math.max(1, sidePreview ? renderer.height - 10 : previewHeight - 3);
    const screen = livePreview ? paneScreen(previewText, contentWidth, contentRows, previewOffset) : undefined;
    if (screen) previewOffset = screen.offset;
    const preview = screen?.lines ?? (excerpt ? transcriptPreview(excerpt, transcripts ? transcriptQuery() : "", contentWidth, contentRows, previewOffset) : []);
    const previewRows = showPreview && !sidePreview ? previewHeight : 0;
    panel = new BoxRenderable(renderer, { id: "palette", flexDirection: "column", width: "100%", height: "100%", backgroundColor: theme.background });
    const body = new BoxRenderable(renderer, { id: "body", flexDirection: "column", flexGrow: 1, paddingLeft: 2, paddingRight: 2 });
    panel.add(body);
    const heading = new BoxRenderable(renderer, { id: "heading", flexDirection: "row" });
    heading.add(new TextRenderable(renderer, { id: "title", content: prompting() ? promptItem!.title : "Herdr Omni", fg: theme.text, attributes: 1 }));
    heading.add(new TextRenderable(renderer, { id: "version", content: `  v${version}`, fg: theme.muted, flexGrow: 1 }));
    if (scanLoading && !running && !prompting() && !updateDialog && !workspacePicker) {
      loadingIndicator = new TextRenderable(renderer, { id: "transcript-loading", content: loadingText(), fg: theme.accent, height: 1 });
      heading.add(loadingIndicator);
    }
    heading.add(new TextRenderable(renderer, { id: "escape", content: updating || running ? "" : prompting() || updateDialog || workspacePicker ? "esc" : "Ctrl+Y preview · tab switch · esc", fg: theme.muted }));
    body.add(heading);
    if (running) {
      activeInput = undefined;
      body.add(new TextRenderable(renderer, { id: "running-title", content: `${runningTitle} (${Math.floor((Date.now() - runningStarted) / 1000)}s)`, fg: theme.accent, marginTop: 1 }));
      body.add(new TextRenderable(renderer, { id: "running-hint", content: "Please wait. This may take a few moments.", fg: theme.muted }));
      renderer.root.add(panel);
      return;
    }
    if (workspacePicker) {
      activeInput = undefined;
      body.add(new TextRenderable(renderer, { id: "workspace-picker-title", content: "Resume in workspace…", fg: theme.accent, marginTop: 1 }));
      body.add(new TextRenderable(renderer, { id: "workspace-picker-hint", content: workspacePicker.message, fg: theme.muted, height: 2 }));
      const picker = workspacePicker;
      const count = Math.max(1, Math.floor((renderer.height - 7) / 2));
      const start = Math.max(0, picker.selected - count + 1);
      picker.choices.slice(start, start + count).forEach((choice, offset) => {
        const index = start + offset;
        const row = new TextRenderable(renderer, { id: `workspace-choice-${index}`, content: `${index === picker.selected ? "›" : " "} ${choice.label} (${choice.id}) · ${choice.reason}`, fg: index === picker.selected ? theme.accent : theme.text, height: 1 });
        row.onMouseUp = event => { if (event.button === 0) { picker.selected = index; redraw(); } };
        body.add(row);
        body.add(new TextRenderable(renderer, { id: `workspace-path-${index}`, content: `  ${cleanText(choice.cwd)}`, fg: theme.muted, height: 1 }));
      });
      if (status) body.add(new TextRenderable(renderer, { id: "workspace-error", content: status, fg: theme.accent }));
      const footer = new TextRenderable(renderer, { id: "workspace-footer", content: "  ↑/↓ choose · enter resume · esc back", fg: theme.accent, height: 1, flexShrink: 0 });
      panel.add(footer);
      renderer.root.add(panel);
      return;
    }
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
      else {
        const previousPrefix = ">@:".includes(query[0] ?? " ") ? query[0] : undefined;
        query = value;
        const nextPrefix = ">@:".includes(query[0] ?? " ") ? query[0] : undefined;
        if (nextPrefix !== previousPrefix) {
          if (nextPrefix === "@") activeTab = "Workspace";
          else if (nextPrefix === ">") activeTab = "Agents";
          else if (nextPrefix === ":") activeTab = "Actions";
          else if (previousPrefix) activeTab = "All";
        }
        selected = 0; discoverSessions(); scheduleScan();
      }
      status = "";
      redraw(true);
    });
    body.add(input); input.focus();
    input.cursorOffset = cursor ?? input.editBuffer.getEOL().offset;
    if (!prompting()) {
      const labels = new BoxRenderable(renderer, { id: "result-tabs", flexDirection: "row", height: 1, flexShrink: 0 });
      const counts = tabMatchCounts(allResults());
      for (const tab of [...new Set([...tabs(), activeTab])]) {
        const count = counts.get(tab) ?? "0";
        const label = new TextRenderable(renderer, { id: `result-tab-${tab}`, content: tab === activeTab ? `[${sectionLabel(tab)}] (${count}) ` : ` ${sectionLabel(tab)} (${count})  `,
          fg: tab === activeTab ? theme.accent : theme.muted, attributes: tab === activeTab ? 1 : 0, height: 1, flexShrink: 0 });
        label.onMouseUp = event => { if (event.button === 0) switchTab(tab); };
        labels.add(label);
      }
      body.add(labels);
    }
    const resultsBody = new BoxRenderable(renderer, { id: "results-body", flexDirection: sidePreview ? "row" : "column", flexGrow: 1, marginTop: 1 });
    body.add(resultsBody);
    const list = new BoxRenderable(renderer, { id: "list", flexDirection: "column", flexGrow: 1, minWidth: 0, overflow: "hidden" });
    resultsBody.add(list);
    if (prompting()) {
      list.add(new TextRenderable(renderer, { id: "prompt-hint", content: promptItem!.description, fg: theme.muted }));
    } else if (items.length === 0) {
      list.add(new TextRenderable(renderer, { id: "empty", content: loading ? "Loading live results…" : "No results match your search.", fg: theme.muted }));
      if (!loading && !transcripts && transcriptQuery() && deps.sessionJob) {
        list.add(transcriptSearchLink("empty-transcripts", "Search session content · Ctrl+F or click"));
      }
    } else {
      const window = viewport(results, selected, Math.max(1, renderer.height - CHROME_ROWS - (status ? 1 : 0) - (updateOffer ? 1 : 0) - previewRows - (sessionSearchVisible() && (sessionsLoading || scanStatus || sessionError) ? 1 : 0)), result => result.section);
      let category = "";
      items.slice(window.start, window.end).forEach((item, offset) => {
        const index = window.start + offset;
        if (results[index]!.section !== category) {
          category = results[index]!.section;
          list.add(new TextRenderable(renderer, { id: `category-${index}`, content: sectionLabel(category), fg: theme.accent, attributes: 1 }));
        }
        const row = new BoxRenderable(renderer, { id: `item-${index}`, flexDirection: "row", width: "100%", height: 1, flexShrink: 0, paddingLeft: 1, paddingRight: 2, backgroundColor: index === selected ? theme.panel : theme.background });
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
        const positions = results[index]!.section === "Transcript matches" ? new Set<number>() : matchingPositions(query, item.title);
        const normalColor = index === selected ? theme.text : theme.muted;
        const content = new StyledText([
          fg(item.currentWorkspace && item.category === "Agents" ? theme.accent : normalColor)(`${item.currentWorkspace && item.category === "Agents" ? "●" : item.icon}  `),
          ...[...item.title].map((char, i) => positions.has(i) ? bold(fg(theme.accent)(char)) : fg(normalColor)(char)),
        ]);
        row.add(new TextRenderable(renderer, { id: `label-${index}`, content, fg: normalColor, flexGrow: 1, height: 1 }));
        row.add(new TextRenderable(renderer, {
          id: `key-${index}`,
          content: item.category === "Agents"
            ? ` ${[item.agentStatus ? `[${item.agentStatus}]` : item.savedSession ? `Saved · ${item.session!.provider}` : "",
              activityAge(agentActivity(item)), item.currentWorkspace ? "current" : ""].filter(Boolean).join(" · ")}`
            : item.shortcuts.join(" / "),
          fg: item.agentStatus === "blocked" ? theme.error : item.agentStatus === "unknown" ? theme.muted : index === selected || item.agentStatus ? theme.accent : theme.shortcut,
          attributes: item.agentStatus === "blocked" ? 1 : 0,
          flexShrink: 0,
        }));
        if (item.session || item.livePaneId) {
          const peek = new TextRenderable(renderer, { id: `peek-${index}`, content: " ◧", fg: theme.accent, flexShrink: 0 });
          peek.onMouseDown = event => { event.preventDefault(); event.stopPropagation(); };
          peek.onMouseUp = event => {
            event.preventDefault(); event.stopPropagation();
            if (event.button !== 0) return;
            const next = visibleItems().findIndex(candidate => candidate.id === item.id);
            if (next < 0) return;
            selected = next; interacted = true; previewEnabled = true; redraw(true);
          };
          row.add(peek);
        }
        list.add(row);
      });
    }
    const detail = (id: string, text: string) => body.add(new TextRenderable(renderer, { id, content: cleanText(text).slice(0, Math.max(10, renderer.width - 4)), fg: theme.muted, height: 1, flexShrink: 0 }));
    if (showPreview && selectedItem) {
      const previewBox = new BoxRenderable(renderer, { id: "session-preview", flexDirection: "column", flexShrink: 0,
        ...(sidePreview ? { width: previewWidth, paddingLeft: 2 } : { height: previewHeight }), backgroundColor: theme.panel, overflow: "hidden" });
      resultsBody.add(previewBox);
      const previewTitle = livePreview ? `Live pane · ${screen?.offset ? "scrolled" : "following"}` : transcripts ? "Transcript context" : "Recent conversation";
      const previewHeader = new TextRenderable(renderer, { id: "preview-heading", content: previewTitle, fg: theme.accent, height: 1, flexShrink: 0 });
      previewBox.add(previewHeader);
      const previewDetail = livePreview
        ? `${selectedItem.livePaneId} · ${selectedItem.agentStatus ?? "unknown"} · ${screen?.totalRows ? `rows ${screen.start + 1}–${screen.end}/${screen.totalRows}` : "visible screen"}`
        : `${selectedSession!.provider} · ${selectedItem.savedSession ? "Saved" : selectedItem.agentStatus ?? "Live"} · ${cleanText(selectedSession!.cwd)}`;
      previewBox.add(new TextRenderable(renderer, { id: "session-detail", content: previewDetail, fg: selectedItem.agentStatus === "blocked" ? theme.error : theme.muted, height: 1, flexShrink: 0 }));
      const expand = new TextRenderable(renderer, { id: "preview-expand", content: `${previewExpanded ? "Collapse" : "Expand"} · Ctrl+O  PgUp/Dn scroll`, fg: theme.muted, height: 1, flexShrink: 0 });
      expand.onMouseUp = event => { if (event.button === 0) { interacted = true; previewExpanded = !previewExpanded; redraw(true); } };
      previewBox.add(expand);
      if (!preview.length) previewBox.add(new TextRenderable(renderer, { id: "preview-state", content: transcripts ? "No matching context for this session" : previewState || (livePreview ? "Pane screen is empty" : "No conversation text available"), fg: theme.muted, height: 1 }));
      preview.forEach((line, index) => previewBox.add(new TextRenderable(renderer, {
        id: `transcript-excerpt-${index}`,
        content: new StyledText(line.map(part => part.match ? bold(fg(theme.accent)(part.text)) : fg(theme.text)(part.text))),
        height: 1, flexShrink: 0,
      })));
    }
    if (!prompting() && sessionSearchVisible() && (sessionsLoading || scanStatus || sessionError)) detail("session-progress", [sessionsLoading ? "Loading saved sessions…" : scanStatus, scanError, sessionError].filter(Boolean).join(" · "));
    if (status) body.add(new TextRenderable(renderer, { id: "status", content: status.replace(/\s+/g, " ").slice(0, Math.max(20, renderer.width - 4)), fg: theme.accent }));
    if (updateOffer) {
      const notice = new TextRenderable(renderer, { id: "update-notice", content: `v${updateOffer.version} available · ctrl+u to review update`, fg: theme.accent });
      notice.onMouseUp = event => { if (event.button === 0 && !running) openUpgrade(); };
      body.add(notice);
    }
    panel.add(footerBar(prompting() ? 0 : allResults().filter(row => activeTab === "All" || row.section === activeTab).length));
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
      if (deps.sessionJob) bar.add(transcripts
        ? label("footer-transcripts", " esc back · content search on  ")
        : transcriptSearchLink("footer-transcripts", " Ctrl+F · Search session content  "));
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
    if (!updateOffer || !deps.update || running || workspacePicker) return;
    updateDialog = true;
    stopScan();
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
    if (running || destroyed) return;
    stopScan();
    running = true;
    runningTitle = item.invocation.kind === "resume-session" ? "Resuming session…" : "Running action…";
    runningStarted = Date.now();
    redraw();
    runningTimer = setInterval(() => { if (!destroyed) redraw(); }, 1000);
    try {
      const result = await deps.run(item, input);
      if (destroyed) return;
      if (result.ok) return deps.close();
      if (result.workspaceChoices?.length && item.invocation.kind === "resume-session") {
        workspacePicker = { item, choices: result.workspaceChoices, selected: 0, message: result.message };
        status = "";
      } else if (result.confirmWorkspace && item.invocation.kind === "resume-session") {
        promptItem = { ...item, title: "Resume in current workspace?", description: result.message,
          invocation: { ...item.invocation, fallbackWorkspaceId: result.confirmWorkspace.id },
          prompt: { placeholder: 'Type "yes" to resume here, or Esc to cancel' } };
        promptValue = "";
        status = "";
      } else status = result.message;
    } catch (error) {
      status = error instanceof Error ? error.message : String(error);
    } finally {
      clearInterval(runningTimer);
      runningTimer = undefined;
      running = false;
    }
    if (!destroyed) redraw();
  }

  async function select() {
    interacted = true;
    if (prompting()) return run(promptItem!, promptValue);
    const item = visibleItems()[selected];
    if (!item) { status = "No results match your search."; return redraw(); }
    const viewAll = visibleResults()[selected]?.viewAll;
    if (viewAll) return switchTab(viewAll);
    if (item.prompt) {
      promptItem = item;
      promptValue = "";
      status = "";
      return redraw();
    }
    return run(item);
  }

  renderer.keyInput.on("keypress", async key => {
    if (running) { key.preventDefault(); return; }
    if (workspacePicker) {
      key.preventDefault();
      const picker = workspacePicker;
      if (key.name === "escape") { workspacePicker = undefined; status = ""; return redraw(); }
      if (key.name === "up" || key.name === "down") {
        picker.selected = (picker.selected + (key.name === "up" ? -1 : 1) + picker.choices.length) % picker.choices.length;
        return redraw();
      }
      if (key.name === "return" && picker.item.invocation.kind === "resume-session") {
        const choice = picker.choices[picker.selected]!;
        return run({ ...picker.item, invocation: { ...picker.item.invocation, fallbackWorkspaceId: undefined, destination: { id: choice.id, cwd: choice.cwd } } });
      }
      return;
    }
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
    if (key.ctrl && key.name === "f" && !prompting()) {
      key.preventDefault();
      return startTranscriptSearch();
    }
    if (!prompting() && key.ctrl && ["o", "y"].includes(key.name)) {
      key.preventDefault();
      interacted = true;
      if (key.name === "y") previewEnabled = !previewEnabled;
      else { previewEnabled = true; previewExpanded = !previewExpanded; }
      return redraw(true);
    }
    const previewItem = visibleItems()[selected];
    if (!prompting() && previewEnabled && ["pageup", "pagedown"].includes(key.name) && (previewItem?.session || previewItem?.livePaneId)) {
      key.preventDefault();
      interacted = true;
      const fromBottom = !transcripts && !previewItem.savedSession && Boolean(previewItem.livePaneId);
      previewOffset = Math.max(0, previewOffset + (key.name === "pageup" ? -5 : 5) * (fromBottom ? -1 : 1));
      return redraw(true);
    }
    if (["up", "down", "left", "right", "home", "end"].includes(key.name) || (key.ctrl && ["p", "n"].includes(key.name))) interacted = true;
    if (key.name === "escape") {
      if (prompting()) return cancelPrompt();
      if (transcripts) {
        preserveSelection(() => { transcripts = false; scheduleScan(); });
        return;
      }
      return deps.close();
    }
    if (prompting()) {
      if (key.name === "return" && !running) return select();
      return;
    }
    if (key.name === "tab") {
      key.preventDefault();
      const available = tabs();
      const index = available.indexOf(activeTab);
      return switchTab(available[(index + (key.shift ? -1 : 1) + available.length) % available.length]!);
    }
    const total = visibleItems().length;
    if (key.name === "up" || (key.ctrl && key.name === "p")) { selected = (selected - 1 + total) % Math.max(1, total); return redraw(); }
    if (key.name === "down" || (key.ctrl && key.name === "n")) { selected = (selected + 1) % Math.max(1, total); return redraw(); }
    if (key.name === "return" && !running) return select();
  });

  renderer.on("resize", () => redraw(true));
  redraw();
  discoverSessions();
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
      const previous = visibleItems()[selected];
      const changed = JSON.stringify(items) !== JSON.stringify(allItems) || loading || refreshError;
      allItems = items;
      loading = false;
      refreshError = false;
      const nextIndex = visibleItems().findIndex(item => item.id === previous?.id || Boolean(previous?.session && item.session && sessionKey(item.session) === sessionKey(previous.session)));
      if (!interacted) selected = 0;
      else if (nextIndex >= 0) selected = nextIndex;
      if (changed && !prompting() && !running) redraw(true);
    },
  };
}
