use crate::{backend, config, search, sessions, update};
use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers,
        MouseButton, MouseEventKind,
    },
    execute,
};
use ratatui::{
    prelude::*,
    widgets::{Block, Clear, Paragraph},
};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    io,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Sender},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

const TABS: [&str; 9] = [
    "All",
    "Workspace",
    "Agents",
    "Tabs",
    "Actions",
    "Transcript matches",
    "Panes",
    "Herdr",
    "Custom",
];
const TAB_ORDER: [usize; 9] = [0, 1, 2, 3, 4, 6, 7, 8, 5];
fn section_label(section: &str) -> &str {
    if section == "Workspace" {
        "Workspaces"
    } else {
        section
    }
}
fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v[k].as_str().unwrap_or("")
}
fn unscoped(query: &str) -> &str {
    if query.starts_with(['@', '>', ':']) {
        &query[1..]
    } else {
        query
    }
}

fn section(v: &Value) -> &str {
    if let Some(section) = v["_section"].as_str() {
        return section;
    }
    if s(v, "category") == "Worktrees" {
        "Workspace"
    } else {
        s(v, "category")
    }
}

fn tab_rows(results: Vec<Value>, tab: &str, query: &str) -> Vec<Value> {
    if tab != "All" {
        return results.into_iter().filter(|v| section(v) == tab).collect();
    }
    let mut groups: Vec<(String, Vec<Value>)> = vec![];
    for item in results {
        let cat = section(&item).to_owned();
        if let Some((_, rows)) = groups.iter_mut().find(|(c, _)| *c == cat) {
            rows.push(item);
        } else {
            groups.push((cat, vec![item]));
        }
    }
    if query.trim().is_empty() {
        groups.sort_by_key(|(cat, _)| {
            TAB_ORDER
                .iter()
                .position(|i| TABS[*i] == cat)
                .unwrap_or(TABS.len())
        });
    }
    groups
        .into_iter()
        .flat_map(|(cat, items)| {
            let count = items.len();
            let mut rows: Vec<_> = items.into_iter().take(5).collect();
            rows.push(json!({
                "id": format!("ui:view-all:{cat}"),
                "title": format!("View all {} ({count})", section_label(&cat)),
                "category": "Custom", "_section": cat, "viewAll": cat, "icon": "→"
            }));
            rows
        })
        .collect()
}
fn result_viewport(rows: &[Value], selected: usize, capacity: usize) -> (usize, usize) {
    if rows.is_empty() {
        return (0, 0);
    }
    let selected = selected.min(rows.len() - 1);
    let cost = |start: usize, end: usize| {
        let mut n = 0;
        let mut previous = "";
        for (i, row) in rows.iter().enumerate().take(end).skip(start) {
            let cat = section(row);
            if i == start || cat != previous {
                n += 1;
            }
            n += 1;
            previous = cat;
        }
        n
    };
    let mut start = selected;
    while start > 0 && cost(start - 1, selected + 1) <= capacity {
        start -= 1;
    }
    let mut end = selected + 1;
    while end < rows.len() && cost(start, end + 1) <= capacity {
        end += 1;
    }
    (start, end)
}

fn demo_data() -> &'static Value {
    static DATA: std::sync::OnceLock<Value> = std::sync::OnceLock::new();
    DATA.get_or_init(|| {
        serde_json::from_str(include_str!("demo.json")).expect("valid embedded demo data")
    })
}
fn same_item(a: &Value, b: &Value) -> bool {
    a["id"] == b["id"]
        || (a["session"].is_object()
            && b["session"].is_object()
            && a["session"]["id"] == b["session"]["id"]
            && a["session"]["provider"] == b["session"]["provider"])
}
fn session_cache_key(item: &Value) -> String {
    format!(
        "{}:{}:{}",
        s(&item["session"], "provider"),
        s(&item["session"], "id"),
        item["session"]["updatedAt"]
    )
}
fn clip_line(line: &str, width: usize) -> String {
    if UnicodeWidthStr::width(line) <= width {
        return line.to_owned();
    }
    if width == 0 {
        return String::new();
    }
    let mut out = String::new();
    for grapheme in line.graphemes(true) {
        if UnicodeWidthStr::width(out.as_str()) + UnicodeWidthStr::width(grapheme) >= width {
            break;
        }
        out.push_str(grapheme);
    }
    out.push('…');
    out
}

fn activity_age(timestamp: i64, now: i64) -> String {
    if timestamp <= 0 {
        return "unknown activity".into();
    }
    let seconds = now.saturating_sub(timestamp).max(0) / 1000;
    if seconds < 60 {
        format!("{seconds}s ago")
    } else if seconds < 3600 {
        format!("{}min ago", seconds / 60)
    } else if seconds < 86400 {
        format!("{}h ago", seconds / 3600)
    } else {
        let days = seconds / 86400;
        format!("{days} {} ago", if days == 1 { "day" } else { "days" })
    }
}

fn preview_lines(
    text: &str,
    query: &str,
    width: usize,
    capacity: usize,
    offset: usize,
    live: bool,
    accent: Color,
) -> (Vec<Line<'static>>, usize) {
    if capacity == 0 || width == 0 {
        return (vec![], 0);
    }
    if live {
        let mut lines: Vec<_> = text.split('\n').collect();
        while lines.last().is_some_and(|s| s.trim().is_empty()) {
            lines.pop();
        }
        let offset = offset.min(lines.len().saturating_sub(capacity));
        let start = lines.len().saturating_sub(capacity).saturating_sub(offset);
        return (
            lines
                .into_iter()
                .skip(start)
                .take(capacity)
                .map(|line| Line::from(vec![Span::raw(clip_line(line.trim_end(), width))]))
                .collect(),
            offset,
        );
    }
    let text: String = text
        .chars()
        .map(|c| {
            if c != '\n' && (c <= '\u{1f}' || ('\u{7f}'..='\u{9f}').contains(&c)) {
                ' '
            } else {
                c
            }
        })
        .collect();
    let lower = text.to_lowercase();
    let query = query
        .chars()
        .map(|c| {
            if c <= '\u{1f}' || ('\u{7f}'..='\u{9f}').contains(&c) {
                ' '
            } else {
                c
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    let ranges: Vec<_> = if query.is_empty() {
        vec![]
    } else {
        lower
            .match_indices(&query)
            .map(|(i, _)| {
                let start = lower[..i].encode_utf16().count();
                start..start + query.encode_utf16().count()
            })
            .collect()
    };
    let focus = ranges.first().map(|r| r.start).unwrap_or(0);
    let mut position = 0;
    let mut focus_line = 0;
    let mut cells = 0;
    let mut lines: Vec<Line<'static>> = vec![Line::default()];
    for grapheme in text.graphemes(true) {
        let size = UnicodeWidthStr::width(grapheme);
        if grapheme == "\n" || cells + size > width {
            lines.push(Line::default());
            cells = 0;
        }
        if grapheme != "\n" {
            if position <= focus {
                focus_line = lines.len() - 1;
            }
            let highlighted = ranges.iter().any(|r| r.contains(&position));
            lines.last_mut().unwrap().spans.push(Span::styled(
                grapheme.to_owned(),
                if highlighted {
                    Style::default().fg(accent).bold()
                } else {
                    Style::default()
                },
            ));
            cells += size;
        }
        position += grapheme.encode_utf16().count();
    }
    let base = focus_line.saturating_sub(capacity / 3);
    let start = (base + offset).min(lines.len().saturating_sub(capacity));
    (
        lines.into_iter().skip(start).take(capacity).collect(),
        start.saturating_sub(base),
    )
}

enum Message {
    Live(Result<Vec<Value>, String>),
    Sessions(Result<Vec<Value>, String>),
    SessionsDone,
    Preview(String, Result<String, String>),
    Detail(String, Result<Option<String>, String>),
    Hit(u64, Value),
    Progress(u64, usize, usize),
    ScanDone(u64, Result<(), String>),
    Executed(Value, Value),
    Offer(Option<Value>),
    Updated(Result<(), String>),
}
#[derive(Default)]
struct Editor {
    text: String,
    cursor: usize,
}
impl Editor {
    fn key(&mut self, code: KeyCode) -> bool {
        match code {
            KeyCode::Char(c) => {
                self.text.insert(self.cursor, c);
                self.cursor += c.len_utf8();
            }
            KeyCode::Backspace if self.cursor > 0 => {
                let p = self.text[..self.cursor].char_indices().last().unwrap().0;
                self.text.drain(p..self.cursor);
                self.cursor = p;
            }
            KeyCode::Delete if self.cursor < self.text.len() => {
                let end = self.cursor + self.text[self.cursor..].chars().next().unwrap().len_utf8();
                self.text.drain(self.cursor..end);
            }
            KeyCode::Left => {
                self.cursor = self.text[..self.cursor]
                    .char_indices()
                    .last()
                    .map(|(i, _)| i)
                    .unwrap_or(0)
            }
            KeyCode::Right => {
                self.cursor += self.text[self.cursor..]
                    .chars()
                    .next()
                    .map(char::len_utf8)
                    .unwrap_or(0)
            }
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = self.text.len(),
            _ => return false,
        };
        true
    }
}
struct App {
    version: &'static str,
    jobs: Vec<thread::JoinHandle<()>>,
    shutdown: Arc<AtomicBool>,
    base: Vec<Value>,
    live: Vec<Value>,
    saved: Vec<Value>,
    rows: Vec<Value>,
    history: Value,
    theme: Value,
    editor: Editor,
    input: Editor,
    tab: usize,
    selected: usize,
    status: String,
    live_error: Option<String>,
    sessions_loading: bool,
    prompt: Option<Value>,
    choices: Option<(Value, Vec<Value>, usize)>,
    transcripts: bool,
    hits: Vec<Value>,
    generation: u64,
    scan_due: Option<Instant>,
    scanning: bool,
    cancel_scan: Arc<AtomicBool>,
    scan_progress: (usize, usize),
    preview_enabled: bool,
    expanded: bool,
    preview: String,
    preview_state: String,
    preview_completed: bool,
    preview_cache: Vec<(String, Instant, String)>,
    preview_key: String,
    preview_identity: String,
    preview_serial: u64,
    pane_choices: HashMap<String, String>,
    detail: String,
    detail_busy: bool,
    detail_at: Instant,
    cancel_detail: Arc<AtomicBool>,
    preview_busy: bool,
    cancel_preview: Arc<AtomicBool>,
    preview_at: Instant,
    offset: usize,
    pane: usize,
    busy: bool,
    busy_title: String,
    busy_started: Instant,
    updated: bool,
    interacted: bool,
    update_error: String,
    animation_started: Instant,
    transcript_links: Vec<Rect>,
    dialog_rect: Rect,
    dialog_buttons: Vec<(Rect, bool)>,
    choiceboxes: Vec<(Rect, usize)>,
    input_area: Rect,
    input_start: usize,
    update_notice: Rect,
    pressed: Option<(Position, Option<String>)>,
    offer: Option<Value>,
    update_dialog: bool,
    approve_update: bool,
    quit: bool,
    demo: bool,
    hitboxes: Vec<(Rect, usize)>,
    tabboxes: Vec<(Rect, usize)>,
    paneboxes: Vec<(Rect, usize)>,
    counts: [usize; 9],
    present_tabs: Vec<usize>,
    preview_area: Rect,
}
impl App {
    fn new(demo: bool) -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION"),
            jobs: vec![],
            shutdown: Arc::new(AtomicBool::new(false)),
            base: config::load_items(),
            live: vec![],
            saved: vec![],
            rows: vec![],
            history: config::load_history(),
            theme: config::load_theme(),
            editor: Editor::default(),
            input: Editor::default(),
            tab: 0,
            selected: 0,
            status: "Loading…".into(),
            live_error: None,
            sessions_loading: !demo,
            prompt: None,
            choices: None,
            transcripts: false,
            hits: vec![],
            generation: 0,
            scan_due: None,
            scanning: false,
            cancel_scan: Arc::new(AtomicBool::new(false)),
            scan_progress: (0, 0),
            preview_enabled: true,
            expanded: false,
            preview: String::new(),
            preview_state: String::new(),
            preview_completed: false,
            preview_cache: vec![],
            preview_key: String::new(),
            preview_identity: String::new(),
            preview_serial: 0,
            pane_choices: HashMap::new(),
            detail: String::new(),
            detail_busy: false,
            detail_at: Instant::now() - Duration::from_secs(10),
            cancel_detail: Arc::new(AtomicBool::new(false)),
            preview_busy: false,
            cancel_preview: Arc::new(AtomicBool::new(false)),
            preview_at: Instant::now() - Duration::from_secs(2),
            offset: 0,
            pane: 0,
            busy: false,
            busy_title: String::new(),
            busy_started: Instant::now(),
            updated: false,
            interacted: false,
            update_error: String::new(),
            animation_started: Instant::now(),
            transcript_links: vec![],
            dialog_rect: Rect::default(),
            dialog_buttons: vec![],
            choiceboxes: vec![],
            input_area: Rect::default(),
            input_start: 0,
            update_notice: Rect::default(),
            pressed: None,
            offer: None,
            update_dialog: false,
            approve_update: false,
            quit: false,
            demo,
            hitboxes: vec![],
            tabboxes: vec![],
            paneboxes: vec![],
            counts: [0; 9],
            present_tabs: vec![0, 1, 2, 3, 4],
            preview_area: Rect::default(),
        }
    }
    fn spawn(&mut self, job: impl FnOnce() + Send + 'static) {
        self.jobs.retain(|job| !job.is_finished());
        self.jobs.push(thread::spawn(job));
    }
    fn stop_jobs(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
        self.cancel_scan.store(true, Ordering::Relaxed);
        self.cancel_preview.store(true, Ordering::Relaxed);
        self.cancel_detail.store(true, Ordering::Relaxed);
        for job in self.jobs.drain(..) {
            let _ = job.join();
        }
    }
    fn rebuild(&mut self) {
        let old = self.rows.get(self.selected).cloned();
        let mut all_items = self.base.clone();
        all_items.extend(self.live.clone());
        let combined = if self.editor.text.starts_with(['@', ':']) {
            all_items.clone()
        } else {
            sessions::merge_sessions(all_items.clone(), &self.saved)
        };
        let mut results =
            search::search(&combined, unscoped(&self.editor.text), "All", &self.history);
        if self.transcripts {
            for hit in &self.hits {
                let existing = combined.iter().rev().find(|v| {
                    v["session"].is_object()
                        && v["session"]["id"] == hit["session"]["id"]
                        && v["session"]["provider"] == hit["session"]["provider"]
                });
                let fallback = sessions::merge_sessions(vec![], &[hit["session"].clone()]);
                if let Some(mut item) = existing.cloned().or_else(|| fallback.into_iter().next()) {
                    if !results.iter().any(|v| v["id"] == item["id"]) {
                        item["excerpt"] = hit["excerpt"].clone();
                        item["_section"] = json!("Transcript matches");
                        results.push(item);
                    }
                }
            }
        }
        self.counts = [0; 9];
        for item in &results {
            self.counts[0] += 1;
            if let Some(i) = TABS.iter().position(|c| *c == section(item)) {
                self.counts[i] += 1;
            }
        }
        self.present_tabs = TAB_ORDER
            .into_iter()
            .filter(|i| {
                *i < 5
                    || self.tab == *i
                    || all_items
                        .iter()
                        .chain(results.iter())
                        .any(|v| section(v) == TABS[*i])
            })
            .collect();
        self.rows = tab_rows(results, TABS[self.tab], unscoped(&self.editor.text));
        self.selected = old
            .and_then(|previous| {
                self.rows.iter().position(|v| {
                    v["id"] == previous["id"]
                        || (previous["session"].is_object()
                            && v["session"].is_object()
                            && previous["session"]["id"] == v["session"]["id"]
                            && previous["session"]["provider"] == v["session"]["provider"])
                })
            })
            .unwrap_or(self.selected)
            .min(self.rows.len().saturating_sub(1));
    }
    fn changed(&mut self) {
        self.cancel_scan.store(true, Ordering::Relaxed);
        self.cancel_scan = Arc::new(AtomicBool::new(false));
        self.scanning = false;
        self.scan_progress = (0, 0);
        self.selected = 0;
        self.generation += 1;
        self.hits.clear();
        self.scan_due = if self.transcripts {
            Some(Instant::now() + Duration::from_millis(200))
        } else {
            None
        };
        self.rebuild();
        self.selected = 0;
    }
    fn switch_tab(&mut self, index: usize) {
        self.tab = index;
        if self.editor.text.starts_with(['@', '>', ':']) {
            self.editor.text.remove(0);
            self.editor.cursor = self.editor.cursor.saturating_sub(1);
        }
        self.selected = 0;
        self.status.clear();
        self.interacted = true;
        self.rebuild();
        self.selected = 0;
    }
    fn run_item(&mut self, item: Value, input: String, tx: &Sender<Message>) {
        if self.demo {
            self.status = "Demo: actions are disabled".into();
            return;
        }
        self.cancel_scan.store(true, Ordering::Relaxed);
        self.scan_due = None;
        self.scanning = false;
        self.generation += 1;
        self.busy = true;
        self.busy_started = Instant::now();
        self.busy_title = if item["invocation"]["kind"] == "resume-session" {
            "Resuming session…"
        } else {
            "Running action…"
        }
        .into();
        self.status = "Running…".into();
        let tx = tx.clone();
        self.spawn(move || {
            let result = backend::execute(&item, &input);
            let _ = tx.send(Message::Executed(item, result));
        });
    }
    fn select(&mut self, tx: &Sender<Message>) {
        if let Some((item, choices, index)) = &self.choices {
            let mut item = item.clone();
            item["invocation"]["destination"] =
                json!({"id":choices[*index]["id"],"cwd":choices[*index]["cwd"]});
            self.choices = None;
            self.run_item(item, String::new(), tx);
            return;
        }
        if let Some(item) = self.prompt.clone() {
            self.run_item(item, self.input.text.clone(), tx);
            return;
        }
        if let Some(item) = self.rows.get(self.selected).cloned() {
            if let Some(cat) = item["viewAll"].as_str() {
                if let Some(i) = TABS.iter().position(|v| *v == cat) {
                    self.switch_tab(i);
                }
                return;
            }
            if item["prompt"].is_object() {
                self.prompt = Some(item);
                self.input = Editor::default();
            } else {
                self.run_item(item, String::new(), tx);
            }
        }
    }
    fn message(&mut self, msg: Message) {
        match msg {
            Message::Live(r) => match r {
                Ok(items) => {
                    self.live = items;
                    self.live_error = None;
                    if self.status == "Loading…" {
                        self.status.clear();
                    }
                    self.rebuild();
                    if !self.interacted {
                        self.selected = 0;
                    }
                }
                Err(e) => {
                    self.live_error = Some(e);
                    if self.status == "Loading…" {
                        self.status.clear();
                    }
                }
            },
            Message::Sessions(r) => match r {
                Ok(items) => {
                    for item in items {
                        if let Some(index) = self.saved.iter().position(|v| {
                            v["provider"] == item["provider"] && v["id"] == item["id"]
                        }) {
                            self.saved[index] = item;
                        } else {
                            self.saved.push(item);
                        }
                    }
                    self.saved.sort_by(|a, b| {
                        b["updatedAt"]
                            .as_f64()
                            .unwrap_or(0.0)
                            .total_cmp(&a["updatedAt"].as_f64().unwrap_or(0.0))
                    });
                    self.rebuild();
                }
                Err(e) => self.status = e,
            },
            Message::SessionsDone => {
                self.sessions_loading = false;
                if self.transcripts {
                    let old = self.rows.get(self.selected).cloned();
                    self.changed();
                    self.selected = old
                        .and_then(|old| self.rows.iter().position(|v| same_item(v, &old)))
                        .unwrap_or(0);
                }
            }
            Message::Preview(key, r) => {
                if key == self.preview_key {
                    self.preview_busy = false;
                    if let Ok(text) = &r {
                        if let Some(item) = self
                            .rows
                            .get(self.selected)
                            .filter(|v| v["session"].is_object() && !v["livePaneId"].is_string())
                        {
                            let cache_key = session_cache_key(item);
                            self.preview_cache.retain(|(key, _, _)| *key != cache_key);
                            if self.preview_cache.len() >= 30 {
                                self.preview_cache.remove(0);
                            }
                            self.preview_cache
                                .push((cache_key, Instant::now(), text.clone()));
                        }
                    }
                    self.preview_completed = true;
                    match r {
                        Ok(text) => {
                            self.preview = text;
                            self.preview_state.clear();
                        }
                        Err(_) => {
                            self.preview.clear();
                            self.preview_state = if self.is_live_preview() {
                                "Live pane unavailable · retrying…".into()
                            } else {
                                "Conversation unavailable".into()
                            };
                        }
                    }
                    self.preview_at = Instant::now();
                }
            }
            Message::Detail(key, result) => {
                if key == self.preview_key {
                    self.detail_busy = false;
                    self.detail = result
                        .ok()
                        .flatten()
                        .unwrap_or_else(|| "unavailable".into());
                    self.detail_at = Instant::now();
                }
            }
            Message::Hit(g, hit) => {
                if g == self.generation {
                    self.hits.push(hit);
                    self.rebuild();
                }
            }
            Message::Progress(g, done, total) => {
                if g == self.generation {
                    self.scan_progress = (done, total);
                }
            }
            Message::ScanDone(g, result) => {
                if g == self.generation {
                    self.scanning = false;
                    self.status = match result {
                        Ok(()) => {
                            if self.hits.len() >= sessions::TRANSCRIPT_RESULT_LIMIT
                                && self.scan_progress.0 < self.scan_progress.1
                            {
                                "Transcript result limit reached".into()
                            } else {
                                "Transcript search complete".into()
                            }
                        }
                        Err(e) => e,
                    };
                }
            }
            Message::Executed(item, result) => {
                self.busy = false;
                if result["ok"] == true {
                    if !["Actions", "Workspace", "Worktrees"].contains(&s(&item, "category")) {
                        config::record_selection(s(&item, "id"));
                    }
                    self.quit = true;
                } else {
                    self.status = s(&result, "message").into();
                    if let Some(choices) = result["workspaceChoices"]
                        .as_array()
                        .filter(|a| !a.is_empty())
                    {
                        self.choices = Some((item, choices.clone(), 0));
                    } else if result["confirmWorkspace"].is_object() {
                        let mut item = item;
                        item["invocation"]["fallbackWorkspaceId"] =
                            result["confirmWorkspace"]["id"].clone();
                        item["title"] = json!("Resume in current workspace?");
                        item["description"] = result["message"].clone();
                        item["prompt"] =
                            json!({"placeholder":"Type \"yes\" to resume here, or Esc to cancel"});
                        self.status.clear();
                        self.prompt = Some(item);
                        self.input = Editor::default();
                    }
                }
            }
            Message::Offer(offer) => {
                self.offer = offer;
                if self.offer.is_some()
                    && !self.interacted
                    && self.editor.text.is_empty()
                    && self.prompt.is_none()
                    && !self.busy
                {
                    self.update_dialog = true;
                    self.approve_update = false;
                }
            }
            Message::Updated(r) => {
                self.busy = false;
                match r {
                    Ok(()) => {
                        self.updated = true;
                        self.update_error.clear();
                    }
                    Err(e) => self.update_error = e,
                }
            }
        }
    }
    fn tick(&mut self, tx: &Sender<Message>) {
        if self.scan_due.is_some_and(|due| Instant::now() >= due) {
            self.scan_due = None;
            let query = unscoped(&self.editor.text).trim().to_owned();
            if !query.is_empty() {
                if self.demo {
                    self.hits.clear();
                    for (i, session) in demo_data()["sessions"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .enumerate()
                    {
                        let messages = vec![s(&demo_data()["examples"][i], "text").to_owned()];
                        if let Some(excerpt) = sessions::transcript_excerpt(&messages, &query) {
                            self.hits.push(json!({"session":session,"excerpt":excerpt}));
                        }
                    }
                    self.scanning = false;
                    self.status = "Transcript search complete".into();
                    self.rebuild();
                } else {
                    self.scanning = true;
                    let mut sessions: Vec<Value> = vec![];
                    for session in self
                        .base
                        .iter()
                        .chain(self.live.iter())
                        .filter_map(|v| v.get("session").filter(|s| s.is_object()))
                        .chain(self.saved.iter())
                    {
                        if let Some(index) = sessions.iter().position(|v| {
                            v["id"] == session["id"] && v["provider"] == session["provider"]
                        }) {
                            sessions[index] = session.clone();
                        } else {
                            sessions.push(session.clone());
                        }
                    }
                    sessions
                        .sort_by_key(|v| std::cmp::Reverse(v["updatedAt"].as_i64().unwrap_or(0)));
                    let g = self.generation;
                    let tx = tx.clone();
                    let cancel = self.cancel_scan.clone();
                    self.spawn(move || {
                        let result = sessions::search_cancellable(
                            &sessions,
                            &query,
                            &cancel,
                            |hit| {
                                let _ = tx.send(Message::Hit(g, hit));
                            },
                            |done, total| {
                                let _ = tx.send(Message::Progress(g, done, total));
                            },
                        );
                        let _ = tx.send(Message::ScanDone(g, result));
                    });
                }
            }
        }
        if !self.preview_enabled
            || self.prompt.is_some()
            || self.choices.is_some()
            || self.busy
            || self.update_dialog
        {
            self.cancel_preview.store(true, Ordering::Relaxed);
            self.cancel_detail.store(true, Ordering::Relaxed);
            self.preview_identity.clear();
            self.preview_key.clear();
            self.preview_busy = false;
            return;
        }
        let Some(item) = self.rows.get(self.selected).cloned() else {
            self.cancel_preview.store(true, Ordering::Relaxed);
            self.cancel_detail.store(true, Ordering::Relaxed);
            self.preview_identity.clear();
            self.preview_key.clear();
            self.preview_busy = false;
            return;
        };
        if let Some(panes) = item["resourcePreview"]["panes"].as_array() {
            self.pane = self
                .pane_choices
                .get(s(&item, "id"))
                .and_then(|id| panes.iter().position(|p| s(p, "id") == id))
                .or_else(|| panes.iter().position(|p| p["focused"] == true))
                .unwrap_or(0);
            if let Some(pane) = panes.get(self.pane) {
                self.pane_choices
                    .insert(s(&item, "id").to_owned(), s(pane, "id").to_owned());
            }
        }
        let identity = format!(
            "{}:{}:{}:{}:{}",
            s(&item, "id"),
            self.pane,
            self.transcripts,
            item["resourcePreview"],
            session_cache_key(&item)
        );
        if identity != self.preview_identity {
            self.cancel_preview.store(true, Ordering::Relaxed);
            self.cancel_detail.store(true, Ordering::Relaxed);
            self.cancel_preview = Arc::new(AtomicBool::new(false));
            self.cancel_detail = Arc::new(AtomicBool::new(false));
            self.detail.clear();
            self.detail_busy = false;
            self.detail_at = Instant::now() - Duration::from_secs(10);
            self.preview_identity = identity.clone();
            self.preview_serial += 1;
            self.preview_key = format!("{}:{identity}", self.preview_serial);
            self.preview_busy = false;
            self.preview.clear();
            self.preview_state.clear();
            self.preview_completed = false;
            self.offset = 0;
            self.preview_at = Instant::now() - Duration::from_millis(800);
            if !item["livePaneId"].is_string() && item["session"].is_object() {
                if let Some((_, _, text)) = self.preview_cache.iter().find(|(key, time, _)| {
                    *key == session_cache_key(&item) && time.elapsed() < Duration::from_secs(30)
                }) {
                    self.preview = text.clone();
                    self.preview_completed = true;
                }
            }
        }
        let key = self.preview_key.clone();
        if self.transcripts {
            self.preview = self
                .hits
                .iter()
                .find(|h| {
                    h["session"]["id"] == item["session"]["id"]
                        && h["session"]["provider"] == item["session"]["provider"]
                })
                .map(|h| s(h, "excerpt").to_owned())
                .unwrap_or_default();
            return;
        }
        if !self.demo && !self.detail_busy {
            let resource = &item["resourcePreview"];
            let target = match s(resource, "kind") {
                "workspace" if !resource["branch"].is_string() => Some(
                    json!({"kind":"workspace","workspaceId":resource["workspaceId"],"path":resource["paths"][0]}),
                ),
                "tab" if resource["panes"].get(self.pane).is_some() => {
                    Some(json!({"kind":"pane","paneId":resource["panes"][self.pane]["id"]}))
                }
                _ => None,
            };
            if let Some(target) = target {
                let interval = if target["kind"] == "pane" { 2 } else { 5 };
                if self.detail_at.elapsed() >= Duration::from_secs(interval) {
                    self.detail_busy = true;
                    let tx = tx.clone();
                    let key = key.clone();
                    let cancel = self.cancel_detail.clone();
                    self.spawn(move || {
                        let result = backend::resource_details_cancellable(&target, &cancel);
                        let _ = tx.send(Message::Detail(key, result));
                    });
                }
            }
        }
        let live = item["livePaneId"].is_string() || item["resourcePreview"].is_object();
        if self.preview_busy
            || (!live && self.preview_completed)
            || self.preview_at.elapsed() < Duration::from_secs(1)
        {
            return;
        }
        if !live && !item["session"].is_object() {
            return;
        }
        if self.demo {
            let pane_id = if item["resourcePreview"]["kind"] == "tab" {
                s(&item["resourcePreview"]["panes"][self.pane], "id")
            } else {
                s(&item, "livePaneId")
            };
            self.detail = if pane_id == "demo:p2" {
                "bun".into()
            } else {
                "claude".into()
            };
            self.preview = match s(&item["resourcePreview"], "kind") {
                "workspace" | "worktree" => backend::preview(&item, self.pane)
                    .unwrap_or_default()
                    .replace("Branch: unavailable", "Branch: feature/payments"),
                _ if !pane_id.is_empty() => {
                    if pane_id == "demo:p2" {
                        "Build terminal\n\n$ bun run dev\nServer running at localhost:3000\n\nGET /checkout 200\nRefresh 1".into()
                    } else {
                        "Claude Code · shop\n\n● Checking payment callback idempotency\n\n  bun test payment.test.ts\n  12 pass · 0 fail\n\n  Preview refresh 1 (sample pane)\n\n╭─ Permission required ─────────────────╮\n│ Run the integration tests?           │\n│ ❯ 1. Allow once                      │\n│   2. Reject                          │\n╰──────────────────────────────────────╯\n\nEnter to confirm · Esc to cancel".into()
                    }
                }
                _ => demo_data()["sessions"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .position(|v| v["id"] == item["session"]["id"])
                    .map(|i| s(&demo_data()["examples"][i], "text").to_owned())
                    .unwrap_or_default(),
            };
            self.preview_completed = true;
            self.preview_at = Instant::now();
            return;
        }
        self.preview_busy = true;
        let tx = tx.clone();
        let pane = self.pane;
        let cancel = self.cancel_preview.clone();
        self.spawn(move || {
            let result = if live {
                backend::preview_cancellable(&item, pane, &cancel)
            } else {
                sessions::preview_cancellable(&item["session"], &cancel)
            };
            let _ = tx.send(Message::Preview(key, result));
        });
    }
    fn key(&mut self, key: event::KeyEvent, tx: &Sender<Message>) {
        if key.kind == KeyEventKind::Release {
            return;
        }
        self.interacted = true;
        if self.busy {
            return;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl && key.code == KeyCode::Char('c') {
            self.quit = true;
            return;
        }
        if self.update_dialog {
            if self.updated {
                if matches!(key.code, KeyCode::Enter | KeyCode::Esc) {
                    self.quit = true;
                }
                return;
            }
            match key.code {
                KeyCode::Esc => {
                    if let Some(o) = &self.offer {
                        update::dismiss(o);
                    }
                    self.offer = None;
                    self.update_dialog = false;
                }
                KeyCode::Tab | KeyCode::BackTab => self.approve_update = !self.approve_update,
                KeyCode::Left => self.approve_update = true,
                KeyCode::Right => self.approve_update = false,
                KeyCode::Enter => {
                    if self.approve_update {
                        if let Some(offer) = self.offer.clone() {
                            self.busy = true;
                            self.busy_title = "Installing update…".into();
                            self.busy_started = Instant::now();
                            self.update_error.clear();
                            let tx = tx.clone();
                            self.spawn(move || {
                                let _ = tx.send(Message::Updated(update::install(&offer)));
                            });
                        }
                    } else {
                        if let Some(o) = &self.offer {
                            update::dismiss(o);
                        }
                        self.offer = None;
                        self.update_dialog = false;
                    }
                }
                _ => {}
            }
            return;
        }
        if self.choices.is_some() {
            match key.code {
                KeyCode::Esc => {
                    self.choices = None;
                    self.status.clear();
                }
                KeyCode::Up | KeyCode::Down => {
                    let (_, choices, index) = self.choices.as_mut().unwrap();
                    *index = (*index
                        + choices.len()
                        + if key.code == KeyCode::Up {
                            choices.len() - 1
                        } else {
                            1
                        })
                        % choices.len();
                }
                KeyCode::Enter => self.select(tx),
                _ => {}
            }
            return;
        }
        if key.code == KeyCode::Esc {
            if self.prompt.take().is_some() {
                self.input = Editor::default();
                self.status.clear();
            } else if self.transcripts {
                self.transcripts = false;
                let old = self.rows.get(self.selected).cloned();
                self.changed();
                self.selected = old
                    .and_then(|old| self.rows.iter().position(|v| same_item(v, &old)))
                    .unwrap_or(0);
                self.status.clear();
            } else {
                self.quit = true;
            }
            return;
        }
        if self.prompt.is_some() {
            if key.code == KeyCode::Enter {
                self.select(tx);
            } else if !ctrl {
                self.input.key(key.code);
            }
            return;
        }
        if ctrl {
            match key.code {
                KeyCode::Char('f') => self.start_transcripts(),
                KeyCode::Char('o') => {
                    self.preview_enabled = true;
                    self.expanded = !self.expanded;
                }
                KeyCode::Char('y') => self.preview_enabled = !self.preview_enabled,
                KeyCode::Char('u') => {
                    if self.offer.is_some() {
                        self.cancel_scan.store(true, Ordering::Relaxed);
                        self.scan_due = None;
                        self.scanning = false;
                        self.generation += 1;
                        self.update_dialog = true;
                        self.approve_update = false;
                    }
                }
                KeyCode::Char('p') => self.move_selection(-1),
                KeyCode::Char('n') => self.move_selection(1),
                _ => {}
            }
            return;
        }
        match key.code {
            KeyCode::Enter => self.select(tx),
            KeyCode::Up => self.move_selection(-1),
            KeyCode::Down => self.move_selection(1),
            KeyCode::Tab => {
                let tabs = self.available_tabs();
                let index = tabs.iter().position(|i| *i == self.tab).unwrap_or(0);
                self.switch_tab(tabs[(index + 1) % tabs.len()]);
            }
            KeyCode::BackTab => {
                let tabs = self.available_tabs();
                let index = tabs.iter().position(|i| *i == self.tab).unwrap_or(0);
                self.switch_tab(tabs[(index + tabs.len() - 1) % tabs.len()]);
            }
            KeyCode::F(6) if self.preview_enabled && !self.transcripts => {
                if let Some(panes) = self
                    .rows
                    .get(self.selected)
                    .and_then(|v| v["resourcePreview"]["panes"].as_array())
                {
                    if !panes.is_empty() {
                        self.pane = (self.pane
                            + if key.modifiers.contains(KeyModifiers::SHIFT) {
                                panes.len() - 1
                            } else {
                                1
                            })
                            % panes.len();
                        if let Some(item) = self.rows.get(self.selected) {
                            self.pane_choices.insert(
                                s(item, "id").to_owned(),
                                s(&panes[self.pane], "id").to_owned(),
                            );
                        }
                    }
                }
            }
            KeyCode::PageUp if self.preview_enabled => {
                if self.is_live_preview() {
                    self.offset = self.offset.saturating_add(5)
                } else {
                    self.offset = self.offset.saturating_sub(5)
                }
            }
            KeyCode::PageDown if self.preview_enabled => {
                if self.is_live_preview() {
                    self.offset = self.offset.saturating_sub(5)
                } else {
                    self.offset = self.offset.saturating_add(5)
                }
            }
            code => {
                let old = self.editor.text.clone();
                self.editor.key(code);
                if old != self.editor.text {
                    let prefix = |q: &str| q.chars().next().filter(|c| ['@', '>', ':'].contains(c));
                    if prefix(&old) != prefix(&self.editor.text) {
                        self.tab = match prefix(&self.editor.text) {
                            Some('@') => 1,
                            Some('>') => 2,
                            Some(':') => 4,
                            _ => 0,
                        };
                    }
                    self.status.clear();
                    self.changed();
                }
            }
        }
    }
    fn mouse(&mut self, mouse: event::MouseEvent, tx: &Sender<Message>) {
        if self.busy {
            return;
        }
        let p = Position::new(mouse.column, mouse.row);
        if matches!(
            mouse.kind,
            MouseEventKind::Down(_) | MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
        ) {
            self.interacted = true;
        }
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                let id = self
                    .hitboxes
                    .iter()
                    .find(|(r, _)| r.contains(p))
                    .and_then(|(_, i)| self.rows.get(*i))
                    .map(|v| s(v, "id").to_owned());
                self.pressed = Some((p, id));
                return;
            }
            MouseEventKind::Drag(_) => {
                self.pressed = None;
                return;
            }
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                if self.update_dialog || self.prompt.is_some() || self.choices.is_some() {
                    return;
                }
                if self.preview_area.contains(p) {
                    let up = mouse.kind == MouseEventKind::ScrollUp;
                    if up == self.is_live_preview() {
                        self.offset = self.offset.saturating_add(3);
                    } else {
                        self.offset = self.offset.saturating_sub(3);
                    }
                } else {
                    self.move_selection(if mouse.kind == MouseEventKind::ScrollUp {
                        -1
                    } else {
                        1
                    });
                }
                return;
            }
            MouseEventKind::Up(MouseButton::Left) => {}
            _ => return,
        }
        let Some((pressed, id)) = self.pressed.take() else {
            return;
        };
        if pressed != p {
            return;
        }
        if self.update_dialog {
            if let Some((_, approve)) = self.dialog_buttons.iter().find(|(r, _)| r.contains(p)) {
                self.approve_update = *approve;
                self.key(event::KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), tx);
            }
            return;
        }
        if let Some((_, _, index)) = &mut self.choices {
            if let Some((_, clicked)) = self.choiceboxes.iter().find(|(r, _)| r.contains(p)) {
                *index = *clicked;
            }
            return;
        }
        if self.input_area.contains(p) {
            let editor = if self.prompt.is_some() {
                &mut self.input
            } else {
                &mut self.editor
            };
            let mut cells = 0;
            let mut cursor = self.input_start;
            for g in editor.text[self.input_start..].graphemes(true) {
                let w = UnicodeWidthStr::width(g) as u16;
                if cells + w > p.x - self.input_area.x {
                    break;
                }
                cells += w;
                cursor += g.len();
            }
            editor.cursor = cursor;
            return;
        }
        if self.prompt.is_some() {
            return;
        }
        if self.update_notice.contains(p) && self.offer.is_some() {
            self.key(
                event::KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL),
                tx,
            );
            return;
        }
        if self.transcript_links.iter().any(|r| r.contains(p)) {
            self.start_transcripts();
        } else if let Some((_, i)) = self.tabboxes.iter().find(|(r, _)| r.contains(p)) {
            self.switch_tab(*i);
        } else if let Some((_, i)) = self.paneboxes.iter().find(|(r, _)| r.contains(p)) {
            self.pane = *i;
            if let Some(item) = self.rows.get(self.selected) {
                self.pane_choices.insert(
                    s(item, "id").to_owned(),
                    s(&item["resourcePreview"]["panes"][*i], "id").to_owned(),
                );
            }
        } else if self.preview_area.contains(p) && mouse.row == self.preview_area.y + 2 {
            self.expanded = !self.expanded;
        } else if let Some(id) = id {
            if let Some(index) = self.rows.iter().position(|v| s(v, "id") == id) {
                self.selected = index;
                let item = &self.rows[index];
                let has_preview = item["session"].is_object()
                    || item["livePaneId"].is_string()
                    || item["resourcePreview"].is_object();
                let peek = has_preview
                    && self
                        .hitboxes
                        .iter()
                        .find(|(r, _)| r.contains(p))
                        .is_some_and(|(r, _)| {
                            mouse.column >= r.right().saturating_sub(4)
                                && mouse.column < r.right().saturating_sub(2)
                        });
                if peek {
                    self.preview_enabled = true;
                } else {
                    self.select(tx);
                }
            }
        }
    }
    fn available_tabs(&self) -> Vec<usize> {
        self.present_tabs.clone()
    }
    fn start_transcripts(&mut self) {
        if self.transcripts
            || self.prompt.is_some()
            || self.busy
            || self.update_dialog
            || self.choices.is_some()
            || unscoped(&self.editor.text).trim().is_empty()
        {
            return;
        }
        let old = self.rows.get(self.selected).cloned();
        self.transcripts = true;
        self.tab = 0;
        self.changed();
        self.selected = old
            .and_then(|old| self.rows.iter().position(|v| same_item(v, &old)))
            .unwrap_or(0);
    }
    fn move_selection(&mut self, delta: isize) {
        if !self.rows.is_empty() {
            self.selected =
                (self.selected as isize + delta).rem_euclid(self.rows.len() as isize) as usize;
            self.pane = 0;
        }
    }
    fn is_live_preview(&self) -> bool {
        !self.transcripts
            && self.rows.get(self.selected).is_some_and(|v| {
                v["livePaneId"].is_string() || v["resourcePreview"]["kind"] == "tab"
            })
    }
    fn color(&self, key: &str, fallback: Color) -> Color {
        s(&self.theme, key).parse().unwrap_or(fallback)
    }
    fn draw(&mut self, f: &mut Frame) {
        // A resize can transiently report zero rows, and dialogs need room for
        // their fixed chrome. Never leave actionable hitboxes outside the frame.
        if f.area().width < 12 || f.area().height < 8 {
            self.hitboxes.clear();
            self.tabboxes.clear();
            self.paneboxes.clear();
            self.choiceboxes.clear();
            self.dialog_buttons.clear();
            self.transcript_links.clear();
            self.input_area = Rect::default();
            self.preview_area = Rect::default();
            self.update_notice = Rect::default();
            self.pressed = None;
            if !f.area().is_empty() {
                f.render_widget(Paragraph::new("Resize terminal"), f.area());
            }
            return;
        }
        let bg = self.color("background", Color::Rgb(30, 30, 46));
        let fg = self.color("text", Color::White);
        let accent = self.color("accent", Color::Cyan);
        let muted = self.color("muted", Color::Gray);
        let panel = self.color("panel", Color::DarkGray);
        let error = self.color("error", Color::Red);
        f.render_widget(
            Block::default().style(Style::default().bg(bg).fg(fg)),
            f.area(),
        );
        let area = f.area().inner(Margin::new(2, 0));
        let has_status = self.busy
            || self.scanning
            || self.offer.is_some()
            || !self.status.is_empty()
            || self.live_error.is_some()
            || self.sessions_loading;
        let chunks = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(u16::from(self.prompt.is_none())),
            Constraint::Min(1),
            Constraint::Length(u16::from(has_status)),
            Constraint::Length(1),
        ])
        .split(area);
        self.transcript_links.clear();
        let heading = self
            .prompt
            .as_ref()
            .map(|p| s(p, "title"))
            .unwrap_or("Herdr Omni");
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(heading.to_owned(), Style::default().fg(fg).bold()),
                Span::styled(format!("  v{}", self.version), Style::default().fg(muted)),
            ])),
            chunks[0],
        );
        let hint = if self.busy {
            ""
        } else if self.prompt.is_some() || self.update_dialog || self.choices.is_some() {
            "esc"
        } else {
            "Ctrl+Y preview · tab switch · esc"
        };
        let hint_width = UnicodeWidthStr::width(hint) as u16;
        if chunks[0].width > hint_width + 24 {
            f.render_widget(
                Paragraph::new(hint).style(Style::default().fg(muted)),
                Rect::new(chunks[0].right() - hint_width, chunks[0].y, hint_width, 1),
            );
        }
        if self.scanning
            && !self.busy
            && self.prompt.is_none()
            && !self.update_dialog
            && self.choices.is_none()
        {
            let loading = format!(
                "{} Searching transcripts…  ",
                ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"]
                    [(self.animation_started.elapsed().as_millis() / 100) as usize % 10]
            );
            let width = UnicodeWidthStr::width(loading.as_str()) as u16;
            if chunks[0].width > hint_width + width + 24 {
                f.render_widget(
                    Paragraph::new(loading).style(Style::default().fg(accent)),
                    Rect::new(
                        chunks[0].right() - hint_width - width,
                        chunks[0].y,
                        width,
                        1,
                    ),
                );
            }
        }
        self.dialog_buttons.clear();
        self.choiceboxes.clear();
        self.update_notice = Rect::default();
        if self.update_dialog || self.choices.is_some() || self.busy {
            self.hitboxes.clear();
            self.tabboxes.clear();
            self.paneboxes.clear();
            self.input_area = Rect::default();
            self.preview_area = Rect::default();
            let body = Rect::new(
                area.x,
                area.y + 2,
                area.width,
                area.height.saturating_sub(3),
            );
            self.dialog_rect = body;
            let footer = Rect::new(
                f.area().x,
                area.bottom().saturating_sub(1),
                f.area().width,
                1,
            );
            let footer_bg = self.color("footer", panel);
            let footer_fg = self.color("footerText", muted);
            f.render_widget(
                Block::default().style(Style::default().bg(footer_bg)),
                footer,
            );
            if let Some((_, choices, index)) = &self.choices {
                f.render_widget(
                    Paragraph::new("Resume in workspace…").style(Style::default().fg(accent)),
                    Rect::new(body.x, body.y, body.width, 1),
                );
                f.render_widget(
                    Paragraph::new(self.status.clone()).style(Style::default().fg(muted)),
                    Rect::new(body.x, body.y + 1, body.width, 2),
                );
                let count = (area.height.saturating_sub(7) / 2).max(1) as usize;
                let start = index.saturating_sub(count - 1);
                for (i, c) in choices.iter().enumerate().skip(start).take(count) {
                    let rect =
                        Rect::new(body.x, body.y + 3 + (i - start) as u16 * 2, body.width, 1);
                    f.render_widget(
                        Paragraph::new(format!(
                            "{} {} ({}) · {}",
                            if i == *index { "›" } else { " " },
                            s(c, "label"),
                            s(c, "id"),
                            s(c, "reason")
                        ))
                        .style(Style::default().fg(if i == *index {
                            accent
                        } else {
                            fg
                        })),
                        rect,
                    );
                    if rect.y + 1 < body.bottom() {
                        f.render_widget(
                            Paragraph::new(format!("  {}", s(c, "cwd")))
                                .style(Style::default().fg(muted)),
                            Rect::new(rect.x, rect.y + 1, rect.width, 1),
                        );
                    }
                    self.choiceboxes.push((rect, i));
                }
                f.render_widget(
                    Paragraph::new("  ↑/↓ choose · enter resume · esc back")
                        .style(Style::default().fg(accent)),
                    footer,
                );
            } else if self.update_dialog {
                let version = self.offer.as_ref().map(|v| s(v, "version")).unwrap_or("");
                let title = if self.updated {
                    format!("Updated to v{version}")
                } else {
                    format!("Herdr Omni v{version} is available")
                };
                let description = if self.updated {
                    "Reopen Omni to use the new version.".into()
                } else if self.busy {
                    "Installing update…".into()
                } else {
                    format!(
                        "Upgrade from v{}? Your settings and history will be kept.",
                        self.version
                    )
                };
                f.render_widget(
                    Paragraph::new(title).style(Style::default().fg(accent).bold()),
                    Rect::new(body.x, body.y, body.width, 1),
                );
                f.render_widget(
                    Paragraph::new(description).style(Style::default().fg(fg)),
                    Rect::new(body.x, body.y + 1, body.width, 1),
                );
                if !self.update_error.is_empty() {
                    f.render_widget(
                        Paragraph::new(
                            self.update_error
                                .split_whitespace()
                                .collect::<Vec<_>>()
                                .join(" "),
                        )
                        .style(Style::default().fg(accent)),
                        Rect::new(body.x, body.y + 2, body.width, 1),
                    );
                }
                let y = body.y + 3 + u16::from(!self.update_error.is_empty());
                let mut x = body.x;
                if !self.busy {
                    for (label, approve) in if self.updated {
                        vec![("Close", true)]
                    } else {
                        vec![
                            (
                                if self.update_error.is_empty() {
                                    "Update"
                                } else {
                                    "Retry"
                                },
                                true,
                            ),
                            ("Later", false),
                        ]
                    } {
                        let selected = self.updated || approve == self.approve_update;
                        let label = format!(
                            " {}{label}{} ",
                            if selected { "[" } else { " " },
                            if selected { "]" } else { " " }
                        );
                        let width = label.len() as u16;
                        let rect = Rect::new(x, y, width.min(body.right().saturating_sub(x)), 1);
                        f.render_widget(
                            Paragraph::new(label).style(Style::default().fg(if selected {
                                accent
                            } else {
                                muted
                            })),
                            rect,
                        );
                        self.dialog_buttons.push((rect, approve));
                        x += width;
                    }
                }
                f.render_widget(
                    Paragraph::new(if self.busy {
                        "  Please wait for installation to finish."
                    } else if self.updated {
                        "  enter/esc close"
                    } else {
                        "  ←/→ or tab choose · enter confirm · esc later"
                    })
                    .style(Style::default().fg(footer_fg)),
                    footer,
                );
            } else {
                f.render_widget(
                    Paragraph::new(format!(
                        "{} ({}s)\nPlease wait. This may take a few moments.",
                        self.busy_title,
                        self.busy_started.elapsed().as_secs()
                    ))
                    .style(Style::default().fg(accent)),
                    body,
                );
            }
            return;
        }
        let editor = if self.prompt.is_some() {
            &self.input
        } else {
            &self.editor
        };
        let title = self
            .prompt
            .as_ref()
            .map(|p| s(&p["prompt"], "placeholder"))
            .unwrap_or("Search · @ workspaces · > agents · : actions");
        let max_width = chunks[1].width.saturating_sub(1) as usize;
        let mut start = 0;
        while UnicodeWidthStr::width(&editor.text[start..editor.cursor]) > max_width {
            start += editor.text[start..].chars().next().unwrap().len_utf8();
        }
        self.input_area = chunks[1];
        self.input_start = start;
        f.render_widget(
            Paragraph::new(if editor.text.is_empty() {
                title.to_owned()
            } else {
                editor.text[start..].to_owned()
            })
            .style(Style::default().fg(if editor.text.is_empty() {
                muted
            } else {
                fg
            })),
            chunks[1],
        );
        if !self.busy && !self.update_dialog && self.choices.is_none() {
            f.set_cursor_position((
                chunks[1].x + UnicodeWidthStr::width(&editor.text[start..editor.cursor]) as u16,
                chunks[1].y,
            ));
        }
        self.tabboxes.clear();
        let mut x = chunks[2].x;
        for i in self
            .available_tabs()
            .into_iter()
            .filter(|_| self.prompt.is_none())
        {
            let tab = section_label(TABS[i]);
            let count = if self.counts[i] > 99 {
                "99+".to_owned()
            } else {
                self.counts[i].to_string()
            };
            let label = if self.tab == i {
                format!("[{tab}] ({count}) ")
            } else {
                format!(" {tab} ({count})  ")
            };
            let width = UnicodeWidthStr::width(label.as_str()) as u16;
            let rect = Rect::new(
                x,
                chunks[2].y,
                width.min(chunks[2].right().saturating_sub(x)),
                1,
            );
            f.render_widget(
                Paragraph::new(label).style(if self.tab == i {
                    Style::default().fg(accent).bold()
                } else {
                    Style::default().fg(muted)
                }),
                rect,
            );
            self.tabboxes.push((rect, i));
            x = x.saturating_add(width);
        }
        let has_preview = self.rows.get(self.selected).is_some_and(|v| {
            v["session"].is_object()
                || v["resourcePreview"].is_object()
                || v["livePaneId"].is_string()
        });
        let show_preview = self.preview_enabled && has_preview && self.prompt.is_none();
        let body = Rect::new(
            chunks[3].x,
            chunks[3].y.saturating_add(1),
            chunks[3].width,
            chunks[3].height.saturating_sub(1),
        );
        let (list, preview) = if show_preview {
            let parts = if area.width >= 116 && !self.expanded {
                Layout::horizontal([
                    Constraint::Min(0),
                    Constraint::Length((area.width as f64 * 0.42).floor() as u16),
                ])
                .split(body)
            } else {
                let is_tab = self
                    .rows
                    .get(self.selected)
                    .is_some_and(|v| v["resourcePreview"]["kind"] == "tab");
                let wanted = if self.expanded {
                    f.area().height.saturating_sub(9)
                } else {
                    6 + if is_tab { 7 } else { 3 }
                };
                let fraction = (f.area().height as f64 * if self.expanded { 0.65 } else { 0.45 })
                    .floor() as u16;
                let height = wanted.min(fraction).max(3).min(body.height);
                Layout::vertical([Constraint::Min(0), Constraint::Length(height)]).split(body)
            };
            (parts[0], parts[1])
        } else {
            (body, Rect::default())
        };
        let side_preview = show_preview && area.width >= 116 && !self.expanded;
        let preview_padding = if side_preview { 2 } else { 0 };
        self.preview_area = preview;
        self.hitboxes.clear();
        self.paneboxes.clear();
        let height = list.height as usize;
        let (start, end) = result_viewport(&self.rows, self.selected, height);
        if self.rows.is_empty() {
            f.render_widget(
                Paragraph::new(if self.status == "Loading…" {
                    "Loading live results…"
                } else {
                    "No results match your search."
                })
                .style(Style::default().fg(muted)),
                list,
            );
            if self.status != "Loading…"
                && !self.transcripts
                && !unscoped(&self.editor.text).trim().is_empty()
                && list.height > 1
            {
                let link = Rect::new(list.x, list.y + 1, list.width, 1);
                self.transcript_links.push(link);
                f.render_widget(
                    Paragraph::new("Search session content · Ctrl+F or click")
                        .style(Style::default().fg(accent)),
                    link,
                );
            }
        }
        let mut y = list.y;
        let mut previous_section = "";
        for (n, item) in self.rows.iter().enumerate().take(end).skip(start) {
            if n == start || section(item) != previous_section {
                if y >= list.bottom() {
                    break;
                }
                f.render_widget(
                    Paragraph::new(section_label(section(item)))
                        .style(Style::default().fg(accent).bold()),
                    Rect::new(list.x, y, list.width, 1),
                );
                y += 1;
                previous_section = section(item);
            }
            if y >= list.bottom() {
                break;
            }
            let rect = Rect::new(list.x, y, list.width, 1);
            y += 1;
            let status = s(item, "agentStatus");
            let suffix = if s(item, "category") == "Agents" {
                let label = if !status.is_empty() {
                    format!("[{status}]")
                } else if item["savedSession"] == true {
                    format!("Saved · {}", s(&item["session"], "provider"))
                } else {
                    String::new()
                };
                let stamp = item["lastActiveAt"]
                    .as_i64()
                    .unwrap_or(0)
                    .max(item["session"]["updatedAt"].as_i64().unwrap_or(0));
                let mut fields = vec![];
                if !label.is_empty() {
                    fields.push(label);
                }
                fields.push(activity_age(stamp, chrono::Utc::now().timestamp_millis()));
                if item["currentWorkspace"] == true {
                    fields.push("current".into());
                }
                fields.join(" · ")
            } else if status.is_empty() {
                item["shortcuts"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(Value::as_str)
                            .collect::<Vec<_>>()
                            .join(" / ")
                    })
                    .unwrap_or_default()
            } else {
                format!(
                    "[{status}]{}",
                    if item["currentWorkspace"] == true {
                        " · current"
                    } else {
                        ""
                    }
                )
            };
            let positions = if section(item) == "Transcript matches" {
                vec![]
            } else {
                search::matching_positions(&self.editor.text, s(item, "title"))
            };
            let current_agent = item["currentWorkspace"] == true && s(item, "category") == "Agents";
            let normal = if n == self.selected { fg } else { muted };
            let mut spans = vec![
                Span::raw(" "),
                Span::styled(
                    if n == self.selected { "┃" } else { " " },
                    Style::default().fg(accent),
                ),
                Span::styled(
                    format!(
                        "{}  ",
                        if current_agent {
                            "●"
                        } else {
                            s(item, "icon")
                        }
                    ),
                    Style::default().fg(if current_agent { accent } else { normal }),
                ),
            ];
            spans.extend(s(item, "title").chars().enumerate().map(|(i, c)| {
                Span::styled(
                    c.to_string(),
                    if positions.contains(&i) {
                        Style::default().fg(accent).bold()
                    } else {
                        Style::default()
                    },
                )
            }));
            let has_peek = item["session"].is_object()
                || item["livePaneId"].is_string()
                || item["resourcePreview"].is_object();
            let suffix_width = UnicodeWidthStr::width(suffix.as_str()) as u16 + 2;
            let suffix_space =
                suffix_width.min(rect.width.saturating_sub(if has_peek { 4 } else { 2 }));
            let label_rect = Rect::new(
                rect.x,
                rect.y,
                rect.width
                    .saturating_sub(suffix_space + if has_peek { 4 } else { 2 }),
                1,
            );
            let suffix_rect = Rect::new(label_rect.right(), rect.y, suffix_space, 1);
            let suffix_span = Span::styled(
                format!("  {suffix}"),
                Style::default()
                    .fg(if status == "blocked" {
                        error
                    } else if status == "unknown" {
                        muted
                    } else if n == self.selected || !status.is_empty() {
                        accent
                    } else {
                        self.color("shortcut", muted)
                    })
                    .add_modifier(if status == "blocked" {
                        Modifier::BOLD
                    } else {
                        Modifier::empty()
                    }),
            );
            let line = Line::from(spans);
            f.render_widget(
                Paragraph::new(line).style(
                    Style::default()
                        .fg(if n == self.selected { fg } else { muted })
                        .bg(if n == self.selected { panel } else { bg }),
                ),
                label_rect,
            );
            f.render_widget(
                Paragraph::new(Line::from(suffix_span))
                    .alignment(Alignment::Right)
                    .style(Style::default().bg(if n == self.selected { panel } else { bg })),
                suffix_rect,
            );
            if item["session"].is_object()
                || item["livePaneId"].is_string()
                || item["resourcePreview"].is_object()
            {
                let peek = Rect::new(rect.right().saturating_sub(4), rect.y, rect.width.min(2), 1);
                f.render_widget(
                    Paragraph::new(" ◧").style(
                        Style::default()
                            .fg(accent)
                            .bg(if n == self.selected { panel } else { bg }),
                    ),
                    peek,
                );
            }
            self.hitboxes.push((rect, n));
        }
        if let Some(prompt) = &self.prompt {
            f.render_widget(Clear, list);
            f.render_widget(
                Paragraph::new(s(prompt, "description")).style(Style::default().bg(bg).fg(muted)),
                list,
            );
        }
        if show_preview {
            let workspace_text = if !self.detail.is_empty()
                && self
                    .rows
                    .get(self.selected)
                    .is_some_and(|v| v["resourcePreview"]["kind"] == "workspace")
            {
                self.preview.replacen(
                    "Branch: unavailable",
                    &format!("Branch: {}", self.detail),
                    1,
                )
            } else {
                self.preview.clone()
            };
            let text = if !self.preview_state.is_empty() {
                &self.preview_state
            } else if self.preview.is_empty() {
                if self.transcripts {
                    "No matching context for this session"
                } else if !self.preview_completed {
                    if self.is_live_preview() {
                        "Reading live pane…"
                    } else {
                        "Loading recent conversation…"
                    }
                } else if self.is_live_preview() {
                    "Pane screen is empty"
                } else {
                    "No conversation text available"
                }
            } else {
                &workspace_text
            };
            let panes = self
                .rows
                .get(self.selected)
                .and_then(|v| v["resourcePreview"]["panes"].as_array())
                .filter(|_| !self.transcripts);
            let available_rows = if side_preview {
                f.area().height.saturating_sub(10)
            } else {
                preview.height.saturating_sub(3)
            };
            let pane_rows = panes
                .map(|p| {
                    p.len()
                        .min(3)
                        .min(available_rows.saturating_sub(5) as usize)
                })
                .unwrap_or(0);
            let show_program = panes.is_some_and(|p| !p.is_empty()) && available_rows > 1;
            let show_path = panes.is_some_and(|p| !p.is_empty()) && available_rows > 3;
            let meta_rows = u16::from(show_program) + u16::from(show_path);
            let h = available_rows
                .saturating_sub(pane_rows as u16 + meta_rows)
                .max(1) as usize;
            let query = if self.transcripts {
                unscoped(&self.editor.text)
            } else {
                ""
            };
            let (text, offset) = preview_lines(
                text,
                query,
                preview.width.saturating_sub(2) as usize,
                h,
                self.offset,
                self.is_live_preview(),
                accent,
            );
            self.offset = offset;
            f.render_widget(
                Block::default().style(Style::default().bg(panel).fg(fg)),
                preview,
            );
            let selected = self.rows.get(self.selected).cloned().unwrap_or(Value::Null);
            let resource = if self.transcripts {
                &Value::Null
            } else {
                &selected["resourcePreview"]
            };
            let heading = match s(resource, "kind") {
                "workspace" => "Workspace overview",
                "worktree" => "Worktree overview",
                "tab" => "Tab preview · live panes",
                _ if self.is_live_preview() => {
                    if self.offset > 0 {
                        "Live pane · scrolled"
                    } else {
                        "Live pane · following"
                    }
                }
                _ if self.transcripts => "Transcript context",
                _ => "Recent conversation",
            };
            let detail = match s(resource, "kind") {
                "workspace" => format!(
                    "{} tabs · {} panes",
                    resource["tabs"].as_array().map_or(0, Vec::len),
                    resource["paneCount"].as_u64().unwrap_or(0)
                ),
                "worktree" => "Unopened worktree".into(),
                "tab" => format!("{} panes · F6 / Shift+F6 switch", panes.map_or(0, Vec::len)),
                _ if self.is_live_preview() => {
                    let total = self
                        .preview
                        .trim_end_matches(|c: char| c.is_whitespace())
                        .split('\n')
                        .count();
                    let has_rows = !self.preview.trim().is_empty();
                    let end = total.saturating_sub(self.offset);
                    let start = end.saturating_sub(h);
                    format!(
                        "{} · {} · {}",
                        s(&selected, "livePaneId"),
                        selected["agentStatus"].as_str().unwrap_or("unknown"),
                        if has_rows {
                            format!("rows {}–{end}/{total}", start + 1)
                        } else {
                            "visible screen".into()
                        }
                    )
                }
                _ => format!(
                    "{} · {} · {}",
                    s(&selected["session"], "provider"),
                    if selected["savedSession"] == true {
                        "Saved"
                    } else {
                        s(&selected, "agentStatus")
                    },
                    s(&selected["session"], "cwd")
                ),
            };
            for (row, text) in [
                heading.to_owned(),
                detail,
                format!(
                    "{} · Ctrl+O  PgUp/Dn scroll",
                    if self.expanded { "Collapse" } else { "Expand" }
                ),
            ]
            .into_iter()
            .enumerate()
            {
                if row < preview.height as usize {
                    f.render_widget(
                        Paragraph::new(text).style(Style::default().fg(if row == 0 {
                            accent
                        } else {
                            muted
                        })),
                        Rect::new(
                            preview.x + preview_padding,
                            preview.y + row as u16,
                            preview.width.saturating_sub(2),
                            1,
                        ),
                    );
                }
            }
            if let Some(panes) = panes {
                let start = self.pane.saturating_sub(pane_rows.saturating_sub(1));
                for (index, pane) in panes.iter().enumerate().skip(start).take(pane_rows) {
                    let rect = Rect::new(
                        preview.x + preview_padding,
                        preview.y + 3 + (index - start) as u16,
                        preview.width.saturating_sub(2),
                        1,
                    );
                    f.render_widget(
                        Paragraph::new(format!(
                            "{} {}. {}{}",
                            if index == self.pane { "›" } else { " " },
                            index + 1,
                            s(pane, "label")
                                .split_whitespace()
                                .collect::<Vec<_>>()
                                .join(" "),
                            if s(pane, "agent").is_empty() {
                                String::new()
                            } else {
                                format!(
                                    " · {} [{}]",
                                    s(pane, "agent"),
                                    pane["status"].as_str().unwrap_or("unknown")
                                )
                            }
                        ))
                        .style(Style::default().fg(
                            if s(pane, "status") == "blocked" {
                                error
                            } else if index == self.pane {
                                accent
                            } else {
                                muted
                            },
                        )),
                        rect,
                    );
                    self.paneboxes.push((rect, index));
                }
            }
            if meta_rows > 0 {
                let pane = &panes.unwrap()[self.pane.min(panes.unwrap().len() - 1)];
                let meta = Rect::new(
                    preview.x + preview_padding,
                    preview.y + 3 + pane_rows as u16,
                    preview.width.saturating_sub(2),
                    meta_rows,
                );
                let program = if self.demo {
                    if self.pane == 0 {
                        "codex"
                    } else {
                        "zsh"
                    }
                } else if self.detail.is_empty() {
                    "loading…"
                } else {
                    &self.detail
                };
                f.render_widget(
                    Paragraph::new(format!(
                        "Program: {program}\n{} · {}",
                        s(pane, "id"),
                        s(pane, "cwd")
                    ))
                    .style(Style::default().fg(muted)),
                    meta,
                );
            }
            f.render_widget(
                Paragraph::new(text).style(Style::default().bg(panel).fg(fg)),
                Rect::new(
                    preview.x + preview_padding,
                    preview.y + 3 + pane_rows as u16 + meta_rows,
                    preview.width.saturating_sub(2),
                    h as u16,
                ),
            );
        }
        if self.offer.is_some() {
            self.update_notice = chunks[4];
        }
        let status = if self.busy {
            "Running…".to_owned()
        } else if self.scanning {
            format!(
                "Searching transcripts… {}/{}",
                self.scan_progress.0, self.scan_progress.1
            )
        } else if let Some(o) = &self.offer {
            format!("{}  v{} available · Ctrl+U", self.status, s(o, "version"))
        } else if !self.status.is_empty() {
            self.status.clone()
        } else if let Some(error) = &self.live_error {
            format!("Refresh unavailable · {error}")
        } else if self.sessions_loading {
            "Loading saved sessions…".into()
        } else {
            String::new()
        };
        f.render_widget(
            Paragraph::new(status).style(Style::default().fg(accent)),
            chunks[4],
        );
        let footer_bg = self.color("footer", panel);
        let footer_fg = self.color("footerText", muted);
        let footer = Rect::new(f.area().x, chunks[5].y, f.area().width, chunks[5].height);
        f.render_widget(
            Block::default().style(Style::default().bg(footer_bg)),
            footer,
        );
        let left = if self.prompt.is_some() {
            "enter confirm   esc back"
        } else {
            "enter/click select   ↑/↓ move"
        };
        let right = if self.prompt.is_some() {
            String::new()
        } else {
            format!(
                "{}{}",
                if self.transcripts {
                    "esc back · content search on  "
                } else {
                    "Ctrl+F · Search session content  "
                },
                if self.status == "Loading…" {
                    "Loading…".to_owned()
                } else if self.live_error.is_some() {
                    "Refresh unavailable".to_owned()
                } else {
                    format!("{} results", self.counts[self.tab])
                }
            )
        };
        f.render_widget(
            Paragraph::new(left).style(Style::default().fg(footer_fg).bg(footer_bg)),
            chunks[5],
        );
        let right_width = (UnicodeWidthStr::width(right.as_str()) as u16).min(chunks[5].width);
        let right_rect = Rect::new(
            chunks[5].right().saturating_sub(right_width),
            chunks[5].y,
            right_width,
            1,
        );
        if chunks[5].width > UnicodeWidthStr::width(left) as u16 + right_width {
            f.render_widget(
                Paragraph::new(right).style(Style::default().fg(footer_fg).bg(footer_bg)),
                right_rect,
            );
        }
        if !self.transcripts && self.prompt.is_none() {
            self.transcript_links.push(Rect::new(
                right_rect.x,
                right_rect.y,
                right_rect
                    .width
                    .min(UnicodeWidthStr::width("Ctrl+F · Search session content  ") as u16),
                1,
            ));
        }
    }
}

impl Drop for App {
    fn drop(&mut self) {
        self.stop_jobs();
    }
}

pub fn run(demo: bool) -> io::Result<()> {
    let mut app = App::new(demo);
    let (tx, rx) = mpsc::channel();
    if demo {
        app.status.clear();
        app.base.clear();
        app.live = demo_data()["items"].as_array().expect("demo items").clone();
        for item in &mut app.live {
            if item["session"].is_object() {
                let index = demo_data()["sessions"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .position(|v| v["id"] == item["session"]["id"])
                    .unwrap_or(0);
                let timestamp = chrono::Utc::now().timestamp_millis() - (index as i64 + 1) * 60_000;
                item["session"]["updatedAt"] = json!(timestamp);
                item["lastActiveAt"] = json!(timestamp);
            }
        }
    } else {
        let live_tx = tx.clone();
        let cancel = app.shutdown.clone();
        app.spawn(move || loop {
            if cancel.load(Ordering::Relaxed) {
                break;
            }
            if live_tx
                .send(Message::Live(backend::load_live_items_cancellable(&cancel)))
                .is_err()
            {
                break;
            }
            for _ in 0..100 {
                if cancel.load(Ordering::Relaxed) {
                    return;
                }
                thread::sleep(Duration::from_millis(20));
            }
        });
        let tx = tx.clone();
        let cancel = app.shutdown.clone();
        app.spawn(move || {
            if let Err(error) = sessions::list_sessions_incremental_cancellable(&cancel, |batch| {
                let _ = tx.send(Message::Sessions(Ok(batch)));
            }) {
                let _ = tx.send(Message::Sessions(Err(error)));
            }
            let _ = tx.send(Message::SessionsDone);
        });
    }
    if !demo {
        let tx = tx.clone();
        let cancel = app.shutdown.clone();
        app.spawn(move || {
            let _ = tx.send(Message::Offer(update::check_cancellable(
                env!("CARGO_PKG_VERSION"),
                &cancel,
            )));
        });
    }
    app.rebuild();
    let mut terminal = ratatui::init();
    if let Err(error) = execute!(io::stdout(), EnableMouseCapture) {
        ratatui::restore();
        app.stop_jobs();
        return Err(error);
    }
    let result = (|| {
        let mut dirty = true;
        let mut animated_at = Instant::now();
        while !app.quit {
            while let Ok(msg) = rx.try_recv() {
                app.message(msg);
                dirty = true;
            }
            let before = (
                app.preview_key.clone(),
                app.preview_busy,
                app.preview.len(),
                app.scanning,
            );
            app.tick(&tx);
            dirty |= before
                != (
                    app.preview_key.clone(),
                    app.preview_busy,
                    app.preview.len(),
                    app.scanning,
                );
            if (app.busy || app.scanning) && animated_at.elapsed() >= Duration::from_millis(100) {
                dirty = true;
                animated_at = Instant::now();
            }
            if dirty {
                terminal.draw(|f| app.draw(f))?;
                dirty = false;
            }
            if event::poll(Duration::from_millis(50))? {
                dirty = true;
                match event::read()? {
                    Event::Key(key) => app.key(key, &tx),
                    Event::Mouse(mouse) => app.mouse(mouse, &tx),
                    _ => {}
                }
            }
        }
        Ok(())
    })();
    app.cancel_scan.store(true, Ordering::Relaxed);
    app.cancel_preview.store(true, Ordering::Relaxed);
    app.cancel_detail.store(true, Ordering::Relaxed);
    let _ = execute!(io::stdout(), DisableMouseCapture);
    ratatui::restore();
    app.stop_jobs();
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn every_dialog_survives_tiny_terminal_resize() {
        for width in [0, 1, 2, 4, 8, 12, 20] {
            for height in [0, 1, 2, 3, 5, 8] {
                for state in 0..8 {
                    let mut app = App::new(true);
                    app.editor.text = "界example".into();
                    app.editor.cursor = app.editor.text.len();
                    app.rebuild();
                    match state {
                        1 => {
                            app.update_dialog = true;
                            app.offer = Some(json!({"version":"1.0.0"}));
                        }
                        2 => {
                            app.choices =
                                Some((json!({}), vec![json!({"id":"one","label":"One"})], 0))
                        }
                        3 => {
                            app.prompt =
                                Some(json!({"title":"Rename","prompt":{"placeholder":"Name"}}))
                        }
                        4 | 5 => {
                            app.rows = vec![
                                json!({"id":"preview", "title":"界", "category":"Agents", "session":{"id":"one"}}),
                            ];
                            app.preview = "界 example\nsecond line".into();
                            app.expanded = state == 5;
                        }
                        6 | 7 => {
                            let panes: Vec<_> = (0..20)
                                .map(|i| json!({"id":i.to_string(),"label":"界 pane"}))
                                .collect();
                            app.rows = vec![
                                json!({"id":"tab", "title":"界", "category":"Tabs", "resourcePreview":{"kind":"tab","panes":panes}}),
                            ];
                            app.pane = 19;
                            app.expanded = state == 7;
                        }
                        _ => {}
                    }
                    let mut terminal =
                        Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
                    terminal.draw(|f| app.draw(f)).unwrap();
                }
            }
        }
    }
    #[test]
    fn running_actions_keep_the_terminal_until_completion_and_picker_escape_clears_status() {
        let mut app = App::new(true);
        let (tx, _) = mpsc::channel();
        app.busy = true;
        app.key(
            event::KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
            &tx,
        );
        app.key(event::KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &tx);
        assert!(!app.quit);
        app.busy = false;
        app.choices = Some((json!({}), vec![json!({"id":"one"})], 0));
        app.status = "Original directory unavailable".into();
        app.key(event::KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &tx);
        assert!(app.choices.is_none());
        assert!(app.status.is_empty());
        app.key(
            event::KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
            &tx,
        );
        assert!(app.quit);
    }
    #[test]
    fn action_row_right_edge_does_not_act_as_invisible_preview_button() {
        let mut app = App::new(true);
        app.rows = vec![json!({"id":"action","title":"Action"})];
        app.hitboxes = vec![(Rect::new(0, 5, 40, 1), 0)];
        app.preview_enabled = false;
        let (tx, _) = mpsc::channel();
        for kind in [
            MouseEventKind::Down(MouseButton::Left),
            MouseEventKind::Up(MouseButton::Left),
        ] {
            app.mouse(
                event::MouseEvent {
                    kind,
                    column: 37,
                    row: 5,
                    modifiers: KeyModifiers::NONE,
                },
                &tx,
            );
        }
        assert_eq!(app.status, "Demo: actions are disabled");
        assert!(!app.preview_enabled);
    }
    #[test]
    fn demo_transcript_search_uses_embedded_conversations_only() {
        let mut app = App::new(true);
        app.base.clear();
        app.live = demo_data()["items"].as_array().unwrap().clone();
        app.editor.text = "幂等".into();
        app.editor.cursor = app.editor.text.len();
        app.transcripts = true;
        app.changed();
        app.scan_due = Some(Instant::now());
        let (tx, _) = mpsc::channel();
        app.tick(&tx);
        assert_eq!(app.hits.len(), 1);
        assert!(app.jobs.is_empty());
        assert_eq!(app.hits[0]["session"]["provider"], "claude");
    }
    #[test]
    fn picker_click_only_selects_and_empty_prompt_is_submitted() {
        let mut app = App::new(true);
        let (tx, _) = mpsc::channel();
        app.choices = Some((json!({}), vec![json!({"id":"a"}), json!({"id":"b"})], 0));
        app.choiceboxes = vec![(Rect::new(2, 5, 30, 1), 1)];
        for kind in [
            MouseEventKind::Down(MouseButton::Left),
            MouseEventKind::Up(MouseButton::Left),
        ] {
            app.mouse(
                event::MouseEvent {
                    kind,
                    column: 3,
                    row: 5,
                    modifiers: KeyModifiers::NONE,
                },
                &tx,
            );
        }
        assert_eq!(app.choices.as_ref().unwrap().2, 1);
        assert!(!app.busy);
        assert_ne!(app.status, "Demo: actions are disabled");
        app.choices = None;
        app.prompt = Some(json!({"prompt":{"placeholder":"New name"}}));
        app.select(&tx);
        assert_eq!(app.status, "Demo: actions are disabled");
    }
    #[test]
    fn saved_preview_cache_is_reused_and_updated_timestamp_invalidates_it() {
        let mut app = App::new(true);
        app.base = vec![];
        app.saved = vec![
            json!({"provider":"codex","id":"one","title":"One","updatedAt":2}),
            json!({"provider":"codex","id":"two","title":"Two","updatedAt":1}),
        ];
        app.tab = 2;
        app.rebuild();
        let (tx, _) = mpsc::channel();
        app.tick(&tx);
        assert!(!app.preview_busy);
        assert!(app.preview.is_empty());
        app.message(Message::Preview(
            app.preview_key.clone(),
            Ok("cached text".into()),
        ));
        app.selected = 1;
        app.tick(&tx);
        app.selected = 0;
        app.tick(&tx);
        assert_eq!(app.preview, "cached text");
        app.saved[0]["updatedAt"] = json!(3);
        app.rebuild();
        app.tick(&tx);
        assert!(app.preview.is_empty());
    }
    #[test]
    fn mouse_input_moves_cursor_and_update_notice_opens_dialog() {
        let mut app = App::new(true);
        app.editor.text = "a界bc".into();
        app.editor.cursor = app.editor.text.len();
        app.input_area = Rect::new(2, 1, 50, 1);
        app.update_notice = Rect::new(2, 10, 50, 1);
        app.offer = Some(json!({"version":"1.0.0"}));
        let (tx, _) = mpsc::channel();
        for kind in [
            MouseEventKind::Down(MouseButton::Left),
            MouseEventKind::Up(MouseButton::Left),
        ] {
            app.mouse(
                event::MouseEvent {
                    kind,
                    column: 5,
                    row: 1,
                    modifiers: KeyModifiers::NONE,
                },
                &tx,
            );
        }
        assert_eq!(app.editor.cursor, "a界".len());
        for kind in [
            MouseEventKind::Down(MouseButton::Left),
            MouseEventKind::Up(MouseButton::Left),
        ] {
            app.mouse(
                event::MouseEvent {
                    kind,
                    column: 5,
                    row: 10,
                    modifiers: KeyModifiers::NONE,
                },
                &tx,
            );
        }
        assert!(app.update_dialog);
        assert!(!app.approve_update);
    }
    #[test]
    fn dialog_ui_frames_match_typescript() {
        let cases: Value =
            serde_json::from_str(include_str!("../tests/fixtures/ui-dialog-frames.json")).unwrap();
        for case in cases.as_array().unwrap() {
            let mut app = App::new(true);
            // Render the reference release version without altering its oracle frames.
            app.version = "0.15.1";
            app.demo = false;
            app.base = vec![case["item"].clone()];
            app.status.clear();
            app.sessions_loading = false;
            app.rebuild();
            match s(case, "state") {
                "prompt" => app.prompt = Some(case["item"].clone()),
                "picker" => {
                    app.choices = Some((
                        case["item"].clone(),
                        case["choices"].as_array().unwrap().clone(),
                        0,
                    ));
                    app.status = "Original directory is unavailable.".into();
                }
                state => {
                    app.offer = Some(json!({"version":"0.16.0"}));
                    app.update_dialog = true;
                    app.updated = state == "updated";
                    app.approve_update = state != "update";
                    if state == "retry" {
                        app.update_error = "Installation failed".into();
                    }
                }
            }
            let backend = ratatui::backend::TestBackend::new(
                case["width"].as_u64().unwrap() as u16,
                case["height"].as_u64().unwrap() as u16,
            );
            let mut terminal = Terminal::new(backend).unwrap();
            terminal.draw(|f| app.draw(f)).unwrap();
            let buffer = terminal.backend().buffer();
            let actual = (0..buffer.area.height)
                .map(|y| {
                    (0..buffer.area.width)
                        .map(|x| buffer[(x, y)].symbol())
                        .collect::<String>()
                        .trim_end()
                        .to_owned()
                })
                .collect::<Vec<_>>();
            let expected = s(case, "frame")
                .lines()
                .map(|line| line.trim_end().to_owned())
                .collect::<Vec<_>>();
            assert_eq!(
                actual,
                expected,
                "width {} state {}\nRust:\n{}\nTS:\n{}",
                case["width"],
                case["state"],
                actual.join("\n"),
                expected.join("\n")
            );
        }
    }
    #[test]
    fn preview_ui_frames_match_typescript() {
        let cases: Value =
            serde_json::from_str(include_str!("../tests/fixtures/ui-preview-frames.json")).unwrap();
        for case in cases.as_array().unwrap() {
            let mut app = App::new(true);
            // Render the reference release version without altering its oracle frames.
            app.version = "0.15.1";
            app.demo = false;
            app.base = vec![case["item"].clone()];
            app.status.clear();
            app.sessions_loading = false;
            app.expanded = case["expanded"] == true;
            app.rebuild();
            app.preview = s(case, "preview").into();
            app.detail = s(case, "detail").into();
            let backend = ratatui::backend::TestBackend::new(
                case["width"].as_u64().unwrap() as u16,
                case["height"].as_u64().unwrap() as u16,
            );
            let mut terminal = Terminal::new(backend).unwrap();
            terminal.draw(|f| app.draw(f)).unwrap();
            let buffer = terminal.backend().buffer();
            let actual = (0..buffer.area.height)
                .map(|y| {
                    (0..buffer.area.width)
                        .map(|x| buffer[(x, y)].symbol())
                        .collect::<String>()
                        .trim_end()
                        .to_owned()
                })
                .collect::<Vec<_>>();
            let expected = s(case, "frame")
                .lines()
                .map(|line| line.trim_end().to_owned())
                .collect::<Vec<_>>();
            assert_eq!(
                actual,
                expected,
                "width {} kind {} expanded {}\nRust:\n{}\nTS:\n{}",
                case["width"],
                case["item"]["category"],
                case["expanded"],
                actual.join("\n"),
                expected.join("\n")
            );
        }
    }
    #[test]
    fn static_ui_frames_match_typescript() {
        let fixture: Value =
            serde_json::from_str(include_str!("../tests/fixtures/ui-frames.json")).unwrap();
        for case in fixture["cases"].as_array().unwrap() {
            let mut app = App::new(true);
            // Render the reference release version without altering its oracle frames.
            app.version = "0.15.1";
            app.demo = false;
            app.base = fixture["items"].as_array().unwrap().clone();
            app.status.clear();
            app.sessions_loading = false;
            app.editor.text = s(case, "query").into();
            app.editor.cursor = app.editor.text.len();
            app.tab = if app.editor.text.starts_with(':') {
                4
            } else {
                0
            };
            app.rebuild();
            let backend = ratatui::backend::TestBackend::new(
                case["width"].as_u64().unwrap() as u16,
                case["height"].as_u64().unwrap() as u16,
            );
            let mut terminal = Terminal::new(backend).unwrap();
            terminal.draw(|f| app.draw(f)).unwrap();
            let buffer = terminal.backend().buffer();
            let actual = (0..buffer.area.height)
                .map(|y| {
                    (0..buffer.area.width)
                        .map(|x| buffer[(x, y)].symbol())
                        .collect::<String>()
                        .trim_end()
                        .to_owned()
                })
                .collect::<Vec<_>>();
            let expected = s(case, "frame")
                .lines()
                .map(|line| line.trim_end().to_owned())
                .collect::<Vec<_>>();
            assert_eq!(
                actual,
                expected,
                "width {} query {:?}\nRust:\n{}\nTS:\n{}",
                case["width"],
                case["query"],
                actual.join("\n"),
                expected.join("\n")
            );
        }
    }
    #[test]
    fn preview_matches_typescript_oracle() {
        let fixture: Value =
            serde_json::from_str(include_str!("../tests/fixtures/preview-parity.json")).unwrap();
        for (key, live) in [("transcriptPreview", false), ("paneScreen", true)] {
            for example in fixture[key].as_array().unwrap() {
                for case in example["cases"].as_array().unwrap() {
                    let (lines, offset) = preview_lines(
                        s(example, "text"),
                        s(example, "query"),
                        case["width"].as_u64().unwrap() as usize,
                        case["rows"].as_u64().unwrap() as usize,
                        case["offset"].as_u64().unwrap() as usize,
                        live,
                        Color::Cyan,
                    );
                    let actual:Vec<Vec<Value>>=lines.iter().map(|line|line.spans.iter().map(|span|json!({"text":span.content,"match":span.style.add_modifier.contains(Modifier::BOLD)})).collect()).collect();
                    let expected = if live {
                        &case["expected"]["lines"]
                    } else {
                        &case["expected"]
                    };
                    assert_eq!(
                        json!(actual),
                        *expected,
                        "{} {} {:?}",
                        key,
                        example["name"],
                        case
                    );
                    if live {
                        assert_eq!(json!(offset), case["expected"]["offset"]);
                    }
                }
            }
        }
    }
    #[test]
    fn group_rows_and_viewport_match_typescript_oracle() {
        let f: Value =
            serde_json::from_str(include_str!("../tests/fixtures/ui-behavior-parity.json"))
                .unwrap();
        let rows: Vec<Value> = f["rows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| {
                let mut v = r["item"].clone();
                v["_section"] = r["section"].clone();
                v
            })
            .collect();
        for case in f["groups"].as_array().unwrap() {
            let actual:Vec<_>=tab_rows(rows.clone(),s(case,"tab"),s(case,"query")).iter().map(|v|json!({"id":v["id"],"title":v["title"],"section":section(v),"viewAll":v["viewAll"]})).collect();
            assert_eq!(
                json!(actual),
                case["rows"],
                "tab {} query {:?}",
                case["tab"],
                case["query"]
            );
        }
        let visible = tab_rows(rows, "All", "");
        for case in f["windows"].as_array().unwrap() {
            let actual = result_viewport(
                &visible,
                case["selected"].as_u64().unwrap() as usize,
                case["capacity"].as_u64().unwrap() as usize,
            );
            assert_eq!(
                actual,
                (
                    case["start"].as_u64().unwrap() as usize,
                    case["end"].as_u64().unwrap() as usize
                )
            );
        }
    }
    #[test]
    fn transcript_tab_excludes_metadata_hits_and_counts_unique_results() {
        let mut app = App::new(true);
        app.base.clear();
        app.live = vec![
            json!({"id":"live:test","title":"find me","category":"Agents","session":{"id":"s1","provider":"codex"}}),
        ];
        app.editor.text = "find".into();
        app.transcripts = true;
        app.hits = vec![
            json!({"session":{"id":"s1","provider":"codex"},"excerpt":"find"}),
            json!({"session":{"id":"s2","provider":"codex","title":"other"},"excerpt":"find"}),
            json!({"session":{"id":"s2","provider":"codex","title":"other"},"excerpt":"find"}),
        ];
        app.tab = 5;
        app.rebuild();
        assert_eq!(app.rows.len(), 1);
        assert_eq!(app.counts[0], 2);
        assert_eq!(app.counts[2], 1);
        assert_eq!(app.counts[5], 1);
        let generation = app.generation;
        app.switch_tab(2);
        assert_eq!(app.generation, generation);
        assert_eq!(app.hits.len(), 3);
        assert_eq!(app.rows[0]["id"], "live:test");
    }
    #[test]
    fn extra_categories_get_tabs_and_whitespace_browsing_uses_fixed_order() {
        let mut app = App::new(true);
        app.base = vec![
            json!({"id":"custom","title":"Custom","category":"Custom"}),
            json!({"id":"pane","title":"Pane","category":"Panes"}),
        ];
        app.live.clear();
        app.editor.text = "   ".into();
        app.rebuild();
        assert_eq!(
            app.available_tabs()
                .iter()
                .map(|i| TABS[*i])
                .collect::<Vec<_>>(),
            vec![
                "All",
                "Workspace",
                "Agents",
                "Tabs",
                "Actions",
                "Panes",
                "Custom"
            ]
        );
        assert_eq!(app.rows[0]["id"], "pane");
    }
    #[test]
    fn unicode_editor_keeps_boundaries() {
        let mut e = Editor::default();
        for c in "a中🦀".chars() {
            e.key(KeyCode::Char(c));
        }
        e.key(KeyCode::Left);
        e.key(KeyCode::Backspace);
        assert_eq!(e.text, "a🦀");
        e.key(KeyCode::Delete);
        assert_eq!(e.text, "a");
    }
    #[test]
    fn demo_renders_narrow_and_wide() {
        for (w, h) in [(40, 12), (140, 40), (10, 5)] {
            let mut app = App::new(true);
            app.rebuild();
            let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
            terminal.draw(|f| app.draw(f)).unwrap();
        }
    }
    #[test]
    fn stale_search_results_are_ignored() {
        let mut app = App::new(true);
        app.generation = 2;
        app.message(Message::Hit(
            1,
            json!({"session":{"id":"stale"},"excerpt":"old"}),
        ));
        assert!(app.hits.is_empty());
        app.message(Message::Progress(1, 99, 100));
        assert_eq!(app.scan_progress, (0, 0));
    }
    #[test]
    fn changing_query_cancels_previous_scan() {
        let mut app = App::new(true);
        let old = app.cancel_scan.clone();
        app.transcripts = true;
        app.changed();
        assert!(old.load(Ordering::Relaxed));
        assert!(!app.cancel_scan.load(Ordering::Relaxed));
        assert!(app.scan_due.is_some());
    }
    #[test]
    fn update_dialog_defaults_to_later() {
        let mut app = App::new(true);
        app.offer = Some(json!({"version":"1.0.0"}));
        let (tx, _) = mpsc::channel();
        app.key(
            event::KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL),
            &tx,
        );
        assert!(app.update_dialog);
        assert!(!app.approve_update);
        assert!(!app.busy);
    }
    #[test]
    fn clipping_preserves_terminal_columns() {
        assert_eq!(clip_line("中ab", 3), "中…");
        assert_eq!(clip_line("abcdef", 0), "");
        assert_eq!(clip_line("abc", 3), "abc");
    }
    #[test]
    fn escape_closes_prompt_before_palette() {
        let mut app = App::new(true);
        app.prompt = Some(json!({"id":"rename"}));
        let (tx, _) = mpsc::channel();
        app.key(event::KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &tx);
        assert!(app.prompt.is_none());
        assert!(!app.quit);
    }
    fn item(id: &str, category: &str) -> Value {
        json!({"id":id,"title":id,"category":category,"aliases":[],"shortcuts":[],"invocation":{"kind":"herdr","argv":["test"]}})
    }
    #[test]
    fn all_tab_limits_sections_and_view_all_never_executes() {
        let mut app = App::new(true);
        app.base = (0..8)
            .map(|i| item(&format!("action{i}"), "Actions"))
            .collect();
        app.rebuild();
        assert_eq!(app.rows.len(), 6);
        assert_eq!(app.rows[5]["viewAll"], "Actions");
        app.selected = 5;
        let (tx, _) = mpsc::channel();
        app.select(&tx);
        assert_eq!(app.tab, 4);
        assert_eq!(app.rows.len(), 8);
        assert!(!app.busy);
    }
    #[test]
    fn refresh_preserves_selected_identity() {
        let mut app = App::new(true);
        app.tab = 1;
        app.base = vec![item("alpha", "Workspace"), item("beta", "Workspace")];
        app.rebuild();
        app.selected = 1;
        app.base.insert(0, item("new", "Workspace"));
        app.rebuild();
        assert_eq!(app.rows[app.selected]["id"], "beta");
    }
    #[test]
    fn left_right_edit_text_instead_of_starting_transcript_search() {
        let mut app = App::new(true);
        app.editor = Editor {
            text: "ab中".into(),
            cursor: 5,
        };
        let (tx, _) = mpsc::channel();
        app.key(event::KeyEvent::new(KeyCode::Left, KeyModifiers::NONE), &tx);
        assert_eq!(app.editor.cursor, 2);
        assert!(!app.transcripts);
        app.key(
            event::KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
            &tx,
        );
        assert_eq!(app.editor.text, "abx中");
    }
    #[test]
    fn stale_and_failed_previews_never_show_old_screen() {
        let mut app = App::new(true);
        app.preview_key = "new".into();
        app.preview = "previous output".into();
        app.message(Message::Preview("old".into(), Ok("stale output".into())));
        assert_eq!(app.preview, "previous output");
        app.message(Message::Preview("new".into(), Err("closed".into())));
        assert!(!app.preview.contains("previous"));
        assert!(app.preview_state.contains("unavailable"));
        assert!(!app.preview_busy);
    }
    #[test]
    fn hiding_preview_cancels_pending_read() {
        let mut app = App::new(true);
        app.preview_busy = true;
        app.preview_enabled = false;
        let cancel = app.cancel_preview.clone();
        let (tx, _) = mpsc::channel();
        app.tick(&tx);
        assert!(cancel.load(Ordering::Relaxed));
        assert!(!app.preview_busy);
        assert!(app.preview_key.is_empty());
    }
    #[test]
    fn resume_choices_require_an_explicit_selection() {
        let mut app = App::new(true);
        let selected =
            json!({"id":"saved","invocation":{"kind":"resume-session","session":{"id":"s"}}});
        app.message(Message::Executed(selected,json!({"ok":false,"message":"Directory missing","workspaceChoices":[{"id":"w","cwd":"/tmp","label":"Workspace"}]})));
        assert!(app.choices.is_some());
        assert!(!app.busy);
        assert!(!app.quit);
        let (tx, _) = mpsc::channel();
        app.key(event::KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &tx);
        assert!(app.choices.is_none());
        assert!(!app.quit);
    }
    #[test]
    fn tab_preview_remembers_pane_identity_across_reordering() {
        let mut app = App::new(true);
        app.rows = vec![
            json!({"id":"tab","resourcePreview":{"kind":"tab","panes":[{"id":"a","focused":true},{"id":"b"}]}}),
        ];
        let (tx, _) = mpsc::channel();
        app.tick(&tx);
        assert_eq!(app.pane, 0);
        app.key(event::KeyEvent::new(KeyCode::F(6), KeyModifiers::NONE), &tx);
        app.tick(&tx);
        assert_eq!(app.pane, 1);
        app.rows[0]["resourcePreview"]["panes"] = json!([{"id":"b"},{"id":"a","focused":true}]);
        app.tick(&tx);
        assert_eq!(app.pane, 0);
    }
    #[test]
    fn live_preview_follows_bottom_without_reflow_or_blank_padding() {
        let (lines, offset) = preview_lines(
            "heading\n  界abcd\n$ prompt\n\n",
            "",
            6,
            2,
            0,
            true,
            Color::Cyan,
        );
        assert_eq!(offset, 0);
        assert_eq!(
            lines.iter().map(Line::to_string).collect::<Vec<_>>(),
            vec!["  界a…", "$ pro…"]
        );
        let (lines, _) = preview_lines("one\ntwo\nthree", "", 10, 2, 1, true, Color::Cyan);
        assert_eq!(lines[0].to_string(), "one");
    }
    #[test]
    fn transcript_preview_wraps_and_centers_literal_highlights() {
        let (lines, _) = preview_lines(
            "before\nline\nline\nhello world\nafter",
            "HELLO WORLD",
            8,
            3,
            0,
            false,
            Color::Cyan,
        );
        assert!(lines
            .iter()
            .flat_map(|l| &l.spans)
            .any(|s| s.style.fg == Some(Color::Cyan)));
        assert!(lines.iter().any(|l| l.to_string() == "hello wo"));
        let (lines, _) = preview_lines("👩‍💻hello", "", 3, 3, 0, false, Color::Cyan);
        assert_eq!(lines[0].to_string(), "👩‍💻h");
    }
    #[test]
    fn successful_update_requires_close_instead_of_executing_result() {
        let mut app = App::new(true);
        app.update_dialog = true;
        app.busy = true;
        app.message(Message::Updated(Ok(())));
        assert!(app.update_dialog);
        assert!(app.updated);
        assert!(!app.busy);
        let (tx, _) = mpsc::channel();
        app.key(
            event::KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &tx,
        );
        assert!(app.quit);
        assert!(!app.busy);
    }
    #[test]
    fn failed_update_keeps_retry_dialog_and_message() {
        let mut app = App::new(true);
        app.update_dialog = true;
        app.busy = true;
        app.offer = Some(json!({"version":"99.0.0"}));
        app.message(Message::Updated(Err("network unavailable".into())));
        assert!(app.update_dialog);
        assert!(!app.updated);
        assert_eq!(app.update_error, "network unavailable");
        assert!(!app.busy);
        let (tx, _) = mpsc::channel();
        app.key(event::KeyEvent::new(KeyCode::Left, KeyModifiers::NONE), &tx);
        assert!(app.approve_update);
        app.key(event::KeyEvent::new(KeyCode::Left, KeyModifiers::NONE), &tx);
        assert!(app.approve_update);
    }
    #[test]
    fn transcript_link_click_enters_content_search() {
        let mut app = App::new(true);
        app.editor = Editor {
            text: "needle".into(),
            cursor: 6,
        };
        app.transcript_links = vec![Rect::new(1, 1, 20, 1)];
        let (tx, _) = mpsc::channel();
        for kind in [
            MouseEventKind::Down(MouseButton::Left),
            MouseEventKind::Up(MouseButton::Left),
        ] {
            app.mouse(
                event::MouseEvent {
                    kind,
                    column: 3,
                    row: 1,
                    modifiers: KeyModifiers::NONE,
                },
                &tx,
            );
        }
        assert!(app.transcripts);
        assert!(app.scan_due.is_some());
    }
    #[test]
    fn stale_resource_details_do_not_replace_selected_pane() {
        let mut app = App::new(true);
        app.preview_key = "current".into();
        app.detail = "shell".into();
        app.message(Message::Detail("old".into(), Ok(Some("codex".into()))));
        assert_eq!(app.detail, "shell");
        app.message(Message::Detail("current".into(), Ok(Some("zsh".into()))));
        assert_eq!(app.detail, "zsh");
    }
    #[test]
    fn transcript_only_hits_have_their_own_section_and_keep_live_target() {
        let mut app = App::new(true);
        app.base = vec![];
        app.editor.text = "needle".into();
        app.transcripts = true;
        let session = json!({"id":"s","provider":"codex","title":"unrelated","cwd":"/tmp"});
        app.live = vec![
            json!({"id":"live:agent:p","title":"unrelated","category":"Agents","session":session,"invocation":{"kind":"herdr","argv":["agent","focus","p"]}}),
        ];
        app.hits = vec![json!({"session":session,"excerpt":"needle"})];
        app.rebuild();
        assert_eq!(section(&app.rows[0]), "Transcript matches");
        assert_eq!(app.rows[0]["id"], "live:agent:p");
        assert_eq!(app.rows[0]["invocation"]["kind"], "herdr");
        assert!(app.available_tabs().contains(&5));
    }
    #[test]
    fn relative_activity_never_fabricates_unknown_timestamps() {
        assert_eq!(activity_age(0, 10000), "unknown activity");
        assert_eq!(activity_age(1000, 61000), "1min ago");
        assert_eq!(activity_age(100000, 1000), "0s ago");
    }
    #[test]
    fn early_update_offer_defaults_to_later_but_late_offer_preserves_input() {
        let mut app = App::new(true);
        app.message(Message::Offer(Some(json!({"version":"99.0.0"}))));
        assert!(app.update_dialog);
        assert!(!app.approve_update);
        let mut app = App::new(true);
        let (tx, _) = mpsc::channel();
        app.key(
            event::KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
            &tx,
        );
        app.message(Message::Offer(Some(json!({"version":"99.0.0"}))));
        assert!(!app.update_dialog);
        assert_eq!(app.editor.text, "x");
    }
    #[test]
    fn category_prefix_switches_an_existing_tab_and_removal_returns_all() {
        let mut app = App::new(true);
        app.tab = 4;
        let (tx, _) = mpsc::channel();
        app.key(
            event::KeyEvent::new(KeyCode::Char('@'), KeyModifiers::NONE),
            &tx,
        );
        assert_eq!(app.tab, 1);
        app.key(
            event::KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE),
            &tx,
        );
        assert_eq!(app.tab, 0);
        assert_eq!(unscoped(">>"), ">");
    }
}
