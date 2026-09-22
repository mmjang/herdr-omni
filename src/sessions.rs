//! Native history access for Codex, Claude Code, and OpenCode.
//!
//! The providers have deliberately different on-disk formats.  This module
//! converts them to the small camelCase JSON shape consumed by the palette,
//! and keeps all provider parsing here so the UI does not need a runtime or an
//! SDK installation to browse old conversations.

use crate::process::{isolate, terminate};
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

pub const TRANSCRIPT_RESULT_LIMIT: usize = 100;
pub const TRANSCRIPT_CONTEXT_BEFORE: usize = 300;
pub const TRANSCRIPT_CONTEXT_AFTER: usize = 900;
pub const SESSION_PREVIEW_MAX_CHARS: usize = 8_000;
pub const SESSION_PREVIEW_MAX_MESSAGES: usize = 2;
const SESSION_REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
const MAX_TRANSCRIPT_BYTES: u64 = 64 * 1024 * 1024;
const MAX_TRANSCRIPT_LINE_BYTES: usize = 16 * 1024 * 1024;
const MAX_PROVIDER_OUTPUT_BYTES: usize = 64 * 1024 * 1024;
const MAX_PROVIDER_LINE_BYTES: usize = 16 * 1024 * 1024;
// The TS Codex transport caps its JS string buffer at 64 Mi UTF-16 code units.
// A thread/read response is one line, including tool output we later discard.
const MAX_CODEX_RESPONSE_UNITS: usize = 64 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
struct PreviewMessage {
    role: String,
    text: String,
}

fn home_dir() -> Option<PathBuf> {
    dirs::home_dir().or_else(|| std::env::var_os("HOME").map(PathBuf::from))
}

fn session_parts(session: &Value) -> Option<(&str, &str)> {
    let object = session.as_object()?;
    Some((
        object.get("provider")?.as_str()?,
        object.get("id")?.as_str()?,
    ))
}

fn session_key(session: &Value) -> Option<String> {
    let (provider, id) = session_parts(session)?;
    if provider.is_empty() || id.is_empty() {
        return None;
    }
    Some(format!("{provider}:{id}"))
}

fn string_field<'a>(object: &'a Map<String, Value>, name: &str) -> Option<&'a str> {
    object.get(name).and_then(Value::as_str)
}

fn millis(value: Option<&Value>) -> i64 {
    let Some(value) = value else { return 0 };
    if let Some(number) = value.as_f64().filter(|number| number.is_finite()) {
        // Codex's app-server uses seconds, while OpenCode and the JSONL
        // providers generally use milliseconds.
        return if number.abs() < 100_000_000_000.0 {
            (number * 1_000.0) as i64
        } else {
            number as i64
        };
    }
    let Some(text) = value.as_str() else { return 0 };
    if let Ok(number) = text.parse::<f64>() {
        return millis(Some(&Value::from(number)));
    }
    chrono::DateTime::parse_from_rfc3339(text)
        .map(|date| date.timestamp_millis())
        .unwrap_or(0)
}

fn finite_timestamp(value: i64) -> Value {
    Value::from(value.max(0))
}

/// Match the TypeScript provider's one-line labels while retaining readable
/// transcript formatting in excerpts.
fn clean_text(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if (character <= '\u{1f}') || ('\u{7f}'..='\u{9f}').contains(&character) {
                ' '
            } else {
                character
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn clean_transcript(value: &str) -> String {
    let normalized = value.replace("\r\n", "\n").replace('\r', "\n");
    let mut cleaned = String::with_capacity(normalized.len());
    for character in normalized.chars() {
        if character == '\t' {
            cleaned.push_str("    ");
        } else if (character <= '\u{08}')
            || ('\u{0b}'..='\u{1f}').contains(&character)
            || ('\u{7f}'..='\u{9f}').contains(&character)
        {
            cleaned.push(' ');
        } else {
            cleaned.push(character);
        }
    }
    cleaned.trim().to_string()
}

fn format_transcript(value: &str) -> String {
    let text = clean_transcript(value);
    if text.starts_with('{') || text.starts_with('[') {
        if let Ok(parsed) = serde_json::from_str::<Value>(&text) {
            if let Ok(pretty) = serde_json::to_string_pretty(&parsed) {
                return clean_transcript(&pretty);
            }
        }
    }
    text
}

fn bounded_preview(messages: &[PreviewMessage], max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    let rendered: Vec<String> = messages
        .iter()
        .filter_map(|message| {
            let text = clean_transcript(&message.text);
            (!text.is_empty()).then(|| format!("{}: {text}", message.role))
        })
        .collect();
    let start = rendered.len().saturating_sub(SESSION_PREVIEW_MAX_MESSAGES);
    let recent = &rendered[start..];
    let mut selected: Vec<String> = Vec::new();
    let mut length = 0usize;
    for message in recent.iter().rev() {
        let separator = usize::from(!selected.is_empty()) * 2;
        if message.chars().count() + length + separator <= max_chars {
            length += message.chars().count() + separator;
            selected.push(message.clone());
            continue;
        }
        if selected.is_empty() {
            let (label, body) = message
                .split_once(": ")
                .map(|(label, body)| (format!("{label}: "), body))
                .unwrap_or_else(|| ("message: ".to_string(), message.as_str()));
            let label_len = label.chars().count();
            if label_len >= max_chars {
                return label.chars().take(max_chars).collect();
            }
            let available = max_chars - label_len;
            let body_count = body.chars().count();
            let truncated = body_count > available;
            let mut result = format!(
                "{}{}",
                label,
                body.chars().take(available).collect::<String>()
            );
            if truncated && available > 0 {
                result.pop();
                result.push('…');
            }
            return result.chars().take(max_chars).collect();
        }
        break;
    }
    selected.reverse();
    selected.join("\n\n")
}

fn transcript_terms(query: &str) -> Option<String> {
    let phrase = clean_text(query).to_lowercase();
    (!phrase.is_empty()).then_some(phrase)
}

fn utf16_slice(text: &str, start: usize, end: usize) -> String {
    let units: Vec<u16> = text.encode_utf16().collect();
    let start = start.min(units.len());
    let end = end.min(units.len()).max(start);
    String::from_utf16_lossy(&units[start..end])
}

pub(crate) fn transcript_excerpt(messages: &[String], query: &str) -> Option<String> {
    let term = transcript_terms(query)?;
    for (index, message) in messages.iter().enumerate() {
        let formatted = format_transcript(message);
        let text = if formatted.to_lowercase().contains(&term) {
            formatted
        } else {
            clean_transcript(message)
        };
        let lower = text.to_lowercase();
        let Some(first_byte) = lower.find(&term) else {
            continue;
        };
        // JavaScript's String#slice uses UTF-16 code-unit offsets. Matching
        // those offsets keeps excerpts identical around CJK and emoji.
        let first = lower[..first_byte].encode_utf16().count();
        let term_len = term.encode_utf16().count();
        let text_len = text.encode_utf16().count();
        let start = first.saturating_sub(TRANSCRIPT_CONTEXT_BEFORE);
        let end = (first + term_len + TRANSCRIPT_CONTEXT_AFTER).min(text_len);
        let before = if start == 0 && index > 0 {
            let previous = format_transcript(&messages[index - 1]);
            let previous_len = previous.encode_utf16().count();
            utf16_slice(
                &previous,
                previous_len.saturating_sub(TRANSCRIPT_CONTEXT_BEFORE),
                previous_len,
            )
        } else {
            String::new()
        };
        let after = if end == text_len && index + 1 < messages.len() {
            let next = format_transcript(&messages[index + 1]);
            utf16_slice(&next, 0, TRANSCRIPT_CONTEXT_AFTER)
        } else {
            String::new()
        };
        let center = format!(
            "{}{}{}",
            if start > 0 { "…" } else { "" },
            utf16_slice(&text, start, end),
            if end < text_len { "…" } else { "" }
        );
        return Some(
            [before, center, after]
                .into_iter()
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
                .join("\n\n"),
        );
    }
    None
}

fn json_text(value: Option<&Value>) -> Vec<String> {
    match value {
        Some(Value::String(text)) => vec![text.clone()],
        Some(Value::Array(values)) => values
            .iter()
            .filter_map(|value| {
                let object = value.as_object()?;
                (matches!(
                    object.get("type").and_then(Value::as_str),
                    Some("text" | "output_text")
                ))
                .then(|| {
                    object
                        .get("text")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
                .flatten()
            })
            .collect(),
        _ => Vec::new(),
    }
}

#[derive(Clone)]
struct ClaudeEntry {
    value: Value,
    uuid: String,
    parent_uuid: Option<String>,
    index: usize,
    kind: String,
    sidechain: bool,
    team_name: bool,
    is_meta: bool,
}

fn claude_active_entries(lines: &[Value]) -> Vec<Value> {
    let mut entries = HashMap::<String, ClaudeEntry>::new();
    for (index, line) in lines.iter().enumerate() {
        let Some(object) = line.as_object() else {
            continue;
        };
        let Some(kind) = object.get("type").and_then(Value::as_str) else {
            continue;
        };
        if !matches!(
            kind,
            "user" | "assistant" | "progress" | "system" | "attachment"
        ) {
            continue;
        }
        let Some(uuid) = object.get("uuid").and_then(Value::as_str) else {
            continue;
        };
        entries.insert(
            uuid.to_string(),
            ClaudeEntry {
                value: line.clone(),
                uuid: uuid.to_string(),
                parent_uuid: object
                    .get("parentUuid")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                index,
                kind: kind.to_string(),
                sidechain: object.get("isSidechain").and_then(Value::as_bool) == Some(true),
                team_name: object.get("teamName").is_some(),
                is_meta: object.get("isMeta").and_then(Value::as_bool) == Some(true),
            },
        );
    }

    // Compact boundaries rewrite the active parent chain in the same way as
    // the SDK before it chooses a leaf branch.
    let boundaries: Vec<ClaudeEntry> = entries
        .values()
        .filter(|entry| entry.kind == "system")
        .filter(|entry| {
            entry.value.get("subtype").and_then(Value::as_str) == Some("compact_boundary")
        })
        .cloned()
        .collect();
    for boundary in boundaries {
        let Some(metadata) = boundary
            .value
            .get("compactMetadata")
            .and_then(Value::as_object)
        else {
            continue;
        };
        if let Some(preserved) = metadata.get("preservedMessages").and_then(Value::as_object) {
            let uuids: Vec<String> = preserved
                .get("uuids")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect();
            let Some(anchor) = preserved.get("anchorUuid").and_then(Value::as_str) else {
                continue;
            };
            if uuids.is_empty() || uuids.iter().any(|uuid| !entries.contains_key(uuid)) {
                continue;
            }
            let mut parent = anchor.to_string();
            for uuid in &uuids {
                if let Some(entry) = entries.get_mut(uuid) {
                    entry.parent_uuid = Some(parent);
                }
                parent = uuid.clone();
            }
            let first = &uuids[0];
            let last = uuids.last().expect("non-empty preserved message list");
            for entry in entries.values_mut() {
                if entry.parent_uuid.as_deref() == Some(anchor) && &entry.uuid != first {
                    entry.parent_uuid = Some(last.clone());
                }
            }
        } else if let Some(preserved) = metadata.get("preservedSegment").and_then(Value::as_object)
        {
            let Some(head) = preserved.get("headUuid").and_then(Value::as_str) else {
                continue;
            };
            let Some(tail) = preserved.get("tailUuid").and_then(Value::as_str) else {
                continue;
            };
            if let Some(entry) = entries.get_mut(head) {
                entry.parent_uuid = Some(boundary.uuid.clone());
            }
            for entry in entries.values_mut() {
                if entry.parent_uuid.as_deref() == Some(boundary.uuid.as_str())
                    && entry.uuid != head
                {
                    entry.parent_uuid = Some(tail.to_string());
                }
            }
        }
    }

    let children: HashSet<String> = entries
        .values()
        .filter_map(|entry| entry.parent_uuid.clone())
        .collect();
    let mut candidates = Vec::new();
    for entry in entries.values() {
        if children.contains(&entry.uuid) {
            continue;
        }
        let mut current = entry;
        let mut visited = HashSet::new();
        loop {
            if !visited.insert(current.uuid.clone()) {
                break;
            }
            if matches!(current.kind.as_str(), "user" | "assistant") {
                candidates.push(current.clone());
                break;
            }
            let Some(parent) = current
                .parent_uuid
                .as_ref()
                .and_then(|uuid| entries.get(uuid))
            else {
                break;
            };
            current = parent;
        }
    }
    let active: Vec<&ClaudeEntry> = candidates
        .iter()
        .filter(|entry| !entry.sidechain && !entry.team_name && !entry.is_meta)
        .collect();
    let selected = active
        .into_iter()
        .max_by_key(|entry| entry.index)
        .or_else(|| candidates.iter().max_by_key(|entry| entry.index));
    let Some(selected) = selected else {
        return Vec::new();
    };

    let mut chain = Vec::new();
    let mut current = entries.get(&selected.uuid);
    let mut visited = HashSet::new();
    while let Some(entry) = current {
        if !visited.insert(entry.uuid.clone()) {
            break;
        }
        chain.push(entry.clone());
        current = entry
            .parent_uuid
            .as_ref()
            .and_then(|uuid| entries.get(uuid));
    }
    chain.reverse();
    chain
        .into_iter()
        .filter(|entry| {
            matches!(entry.kind.as_str(), "user" | "assistant")
                && !entry.sidechain
                && !entry.team_name
                && !entry.is_meta
        })
        .map(|entry| entry.value)
        .collect()
}

fn claude_messages(lines: &[Value]) -> Vec<PreviewMessage> {
    claude_active_entries(lines)
        .into_iter()
        .filter_map(|line| {
            let object = line.as_object()?;
            let role = object.get("type").and_then(Value::as_str)?;
            let message = object.get("message").and_then(Value::as_object)?;
            let content = json_text(message.get("content"));
            (!content.is_empty()).then(|| {
                content
                    .into_iter()
                    .map(|text| PreviewMessage {
                        role: role.to_string(),
                        text,
                    })
                    .collect::<Vec<_>>()
            })
        })
        .flatten()
        .collect()
}

fn codex_thread_messages(thread: &Value) -> Vec<PreviewMessage> {
    thread
        .get("turns")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .flat_map(|turn| {
            turn.get("items")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
        })
        .flat_map(|item| match item.get("type").and_then(Value::as_str) {
            Some("agentMessage") => item
                .get("text")
                .and_then(Value::as_str)
                .map(|text| {
                    vec![PreviewMessage {
                        role: "assistant".into(),
                        text: text.into(),
                    }]
                })
                .unwrap_or_default(),
            Some("userMessage") => item
                .get("content")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|part| {
                    (part.get("type").and_then(Value::as_str) == Some("text"))
                        .then(|| part.get("text").and_then(Value::as_str))
                        .flatten()
                        .map(|text| PreviewMessage {
                            role: "user".into(),
                            text: text.into(),
                        })
                })
                .collect(),
            _ => Vec::new(),
        })
        .collect()
}

fn codex_source_hidden(source: &Value) -> bool {
    const ALLOWED: &[&str] = &["cli", "vscode", "appServer", "exec"];
    if let Some(source) = source.as_str() {
        return !ALLOWED.contains(&source);
    }
    let Some(object) = source.as_object() else {
        return true;
    };
    let kind = object
        .get("type")
        .and_then(Value::as_str)
        .or_else(|| object.get("kind").and_then(Value::as_str));
    kind.is_none_or(|kind| !ALLOWED.contains(&kind))
}

fn next_codex_cursor(next: &Value, seen: &mut HashSet<String>) -> Result<Option<String>, String> {
    let Some(next_text) = next.as_str().filter(|cursor| !cursor.is_empty()) else {
        return Ok(None);
    };
    if !seen.insert(next_text.to_string()) {
        return Err("Invalid Codex history pagination".to_string());
    }
    Ok(Some(next_text.to_string()))
}

fn list_codex_appserver_incremental<F>(
    cancel: Option<&AtomicBool>,
    publish: F,
) -> Result<(), String>
where
    F: FnMut(Vec<Value>),
{
    list_codex_appserver_incremental_with_program("codex", cancel, publish)
}

fn list_codex_appserver_incremental_with_program<F>(
    program: &str,
    cancel: Option<&AtomicBool>,
    mut publish: F,
) -> Result<(), String>
where
    F: FnMut(Vec<Value>),
{
    let mut client = CodexClient::connect_with_program(program, cancel)?;
    let mut cursor = Value::Null;
    let mut seen = HashSet::new();
    let mut seen_cursors = HashSet::new();
    loop {
        let result = client.call_cancellable(
            "thread/list",
            json!({
                "limit": 100,
                "cursor": cursor,
                "sortKey": "updated_at",
                "sourceKinds": ["cli", "vscode", "appServer", "exec"]
            }),
            cancel,
        )?;
        let data = result
            .get("data")
            .and_then(Value::as_array)
            .ok_or_else(|| "Invalid Codex session list".to_string())?;
        let mut page_sessions = Vec::new();
        for thread in data {
            let Some(object) = thread.as_object() else {
                continue;
            };
            if object.get("source").is_some_and(codex_source_hidden) {
                continue;
            }
            let Some(id) = object.get("id").and_then(Value::as_str) else {
                continue;
            };
            if !seen.insert(id.to_string()) {
                continue;
            }
            let title = object
                .get("name")
                .and_then(Value::as_str)
                .or_else(|| object.get("preview").and_then(Value::as_str))
                .filter(|title| !title.is_empty())
                .unwrap_or("Codex session");
            let cwd = object.get("cwd").and_then(Value::as_str).unwrap_or("");
            page_sessions.push(make_session(
                "codex",
                id.to_string(),
                title.to_string(),
                cwd.to_string(),
                millis(object.get("updatedAt")),
            ));
        }
        if !page_sessions.is_empty() {
            publish(page_sessions);
        }
        let next = result.get("nextCursor").cloned().unwrap_or(Value::Null);
        let Some(next_text) = next_codex_cursor(&next, &mut seen_cursors)? else {
            break;
        };
        cursor = Value::String(next_text);
    }
    Ok(())
}

fn read_json_lines_cancellable(
    path: &Path,
    cancel: Option<&AtomicBool>,
) -> Result<Vec<Value>, String> {
    let file = File::open(path).map_err(|error| error.to_string())?;
    if file.metadata().map_err(|error| error.to_string())?.len() > MAX_TRANSCRIPT_BYTES {
        return Err("Transcript exceeds the native reader limit".into());
    }
    let mut values = Vec::new();
    for line in BufReader::new(file).lines() {
        if cancel.is_some_and(|cancel| cancel.load(Ordering::Relaxed)) {
            break;
        }
        if let Ok(line) = line {
            if line.len() <= MAX_TRANSCRIPT_LINE_BYTES && !line.trim().is_empty() {
                if let Ok(value) = serde_json::from_str::<Value>(&line) {
                    values.push(value);
                }
            }
        }
    }
    Ok(values)
}

fn walk_files_cancel(
    root: &Path,
    suffix: Option<&str>,
    result: &mut Vec<PathBuf>,
    cancel: Option<&AtomicBool>,
) {
    if cancel.is_some_and(|cancel| cancel.load(Ordering::Relaxed)) {
        return;
    }
    let Ok(root_metadata) = fs::symlink_metadata(root) else {
        return;
    };
    if root_metadata.file_type().is_symlink() {
        return;
    }
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        if cancel.is_some_and(|cancel| cancel.load(Ordering::Relaxed)) {
            return;
        }
        let path = entry.path();
        let Ok(metadata) = fs::symlink_metadata(&path) else {
            continue;
        };
        if metadata.file_type().is_symlink() {
            continue;
        }
        if metadata.is_dir() {
            walk_files_cancel(&path, suffix, result, cancel);
        } else if suffix
            .is_none_or(|suffix| path.extension().and_then(|ext| ext.to_str()) == Some(suffix))
        {
            result.push(path);
        }
    }
}

fn file_millis(path: &Path, lines: &[Value]) -> i64 {
    fs::metadata(path)
        .ok()
        .and_then(|metadata| metadata.modified().ok())
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_millis() as i64)
        .or_else(|| {
            lines
                .iter()
                .filter_map(|line| line.as_object()?.get("timestamp"))
                .map(|value| millis(Some(value)))
                .max()
        })
        .unwrap_or(0)
}

fn make_session(provider: &str, id: String, title: String, cwd: String, updated_at: i64) -> Value {
    json!({
        "provider": provider,
        "id": id,
        "title": if title.trim().is_empty() { format!("{} session", title_provider(provider)) } else { title },
        "cwd": cwd,
        "updatedAt": finite_timestamp(updated_at),
    })
}

fn title_provider(provider: &str) -> &str {
    match provider {
        "codex" => "Codex",
        "claude" => "Claude",
        "opencode" => "OpenCode",
        _ => provider,
    }
}

fn command_available(command: &str) -> bool {
    let command_path = Path::new(command);
    if command_path.components().count() > 1 {
        return executable_file(command_path);
    }
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
        .any(|directory| executable_file(&directory.join(command)))
}

/// Run a provider's read-only command with a hard deadline and output cap.
/// The reader lives in a short-lived thread because `ChildStdout` is blocking;
/// the caller can still kill a stuck provider without waiting on a pipe read.
fn run_bounded_command_status<A: AsRef<OsStr>>(
    program: &str,
    args: &[A],
    cancel: Option<&AtomicBool>,
) -> Result<(ExitStatus, String), String> {
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    isolate(&mut command);
    let mut child = command
        .spawn()
        .map_err(|_| format!("{program} history unavailable"))?;
    let stdout = match child.stdout.take() {
        Some(stdout) => stdout,
        None => {
            terminate(&mut child);
            let _ = child.wait();
            return Err("Provider output unavailable".to_string());
        }
    };
    let (sender, receiver) = mpsc::channel();
    let reader_handle = thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let mut bytes = Vec::new();
        let mut chunk = [0u8; 32 * 1024];
        loop {
            match std::io::Read::read(&mut reader, &mut chunk) {
                Ok(0) => break,
                Ok(size) => {
                    if bytes.len().saturating_add(size) > MAX_PROVIDER_OUTPUT_BYTES {
                        let _ = sender.send(Err("Provider history output too large".to_string()));
                        return;
                    }
                    bytes.extend_from_slice(&chunk[..size]);
                }
                Err(error) => {
                    let _ = sender.send(Err(error.to_string()));
                    return;
                }
            }
        }
        let _ = sender.send(String::from_utf8(bytes).map_err(|error| error.to_string()));
    });
    let deadline = Instant::now() + SESSION_REQUEST_TIMEOUT;
    let output = loop {
        if cancel.is_some_and(|cancel| cancel.load(Ordering::Relaxed)) {
            terminate(&mut child);
            let _ = child.wait();
            let _ = reader_handle.join();
            return Err(format!("{program} history cancelled"));
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            terminate(&mut child);
            let _ = child.wait();
            let _ = reader_handle.join();
            return Err(format!("{program} history timed out"));
        }
        match receiver.recv_timeout(remaining.min(Duration::from_millis(50))) {
            Ok(Ok(output)) => break output,
            Ok(Err(error)) => {
                terminate(&mut child);
                let _ = child.wait();
                let _ = reader_handle.join();
                return Err(error);
            }
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => {
                terminate(&mut child);
                let _ = child.wait();
                let _ = reader_handle.join();
                return Err(format!("{program} history unavailable"));
            }
        }
    };
    let status = loop {
        if cancel.is_some_and(|cancel| cancel.load(Ordering::Relaxed)) {
            terminate(&mut child);
            let _ = child.wait();
            let _ = reader_handle.join();
            return Err(format!("{program} history cancelled"));
        }
        if Instant::now() >= deadline {
            terminate(&mut child);
            let _ = child.wait();
            let _ = reader_handle.join();
            return Err(format!("{program} history timed out"));
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => thread::sleep(Duration::from_millis(10)),
            Err(_) => {
                terminate(&mut child);
                let _ = child.wait();
                let _ = reader_handle.join();
                return Err(format!("{program} history unavailable"));
            }
        }
    };
    let _ = reader_handle.join();
    Ok((status, output))
}

fn read_bounded_line<R: BufRead>(reader: &mut R, limit: usize) -> io::Result<Option<String>> {
    let mut bytes = Vec::new();
    loop {
        let chunk = reader.fill_buf()?;
        if chunk.is_empty() {
            if bytes.is_empty() {
                return Ok(None);
            }
            break;
        }
        let newline = chunk.iter().position(|byte| *byte == b'\n');
        let take = newline.map_or(chunk.len(), |index| index + 1);
        let max_bytes = if newline.is_some() {
            limit.saturating_add(1)
        } else {
            limit
        };
        if bytes.len().saturating_add(take) > max_bytes {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "provider line too large",
            ));
        }
        bytes.extend_from_slice(&chunk[..take]);
        reader.consume(take);
        if newline.is_some() {
            break;
        }
    }
    if bytes.last() == Some(&b'\n') {
        bytes.pop();
    }
    if bytes.last() == Some(&b'\r') {
        bytes.pop();
    }
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "provider output is not UTF-8"))
}

fn read_codex_line<R: BufRead>(reader: &mut R, limit_units: usize) -> io::Result<Option<String>> {
    // Each UTF-16 unit needs at most three UTF-8 bytes. Bound allocation before
    // decoding, then apply the reference transport's actual Unicode limit.
    let line = read_bounded_line(reader, limit_units.saturating_mul(3))?;
    if line
        .as_ref()
        .is_some_and(|line| line.encode_utf16().count() > limit_units)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Codex response too large",
        ));
    }
    Ok(line)
}

fn run_bounded_command_cancellable<A: AsRef<OsStr>>(
    program: &str,
    args: &[A],
    cancel: Option<&AtomicBool>,
) -> Result<String, String> {
    let (status, output) = run_bounded_command_status(program, args, cancel)?;
    if !status.success() {
        return Err(format!("{program} history command failed"));
    }
    Ok(output)
}

fn basic_auth(username: &str, password: &str) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let bytes = format!("{username}:{password}").into_bytes();
    let mut encoded = String::new();
    for chunk in bytes.chunks(3) {
        let a = chunk[0] as u32;
        let b = chunk.get(1).copied().unwrap_or(0) as u32;
        let c = chunk.get(2).copied().unwrap_or(0) as u32;
        encoded.push(TABLE[((a >> 2) & 63) as usize] as char);
        encoded.push(TABLE[(((a << 4) | (b >> 4)) & 63) as usize] as char);
        encoded.push(if chunk.len() > 1 {
            TABLE[(((b << 2) | (c >> 6)) & 63) as usize] as char
        } else {
            '='
        });
        encoded.push(if chunk.len() > 2 {
            TABLE[(c & 63) as usize] as char
        } else {
            '='
        });
    }
    encoded
}

fn query_escape(value: &str) -> String {
    value.bytes().fold(String::new(), |mut escaped, byte| {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            escaped.push(byte as char);
        } else {
            escaped.push_str(&format!("%{byte:02X}"));
        }
        escaped
    })
}

fn http_json_cancellable(
    port: u16,
    path: &str,
    username: &str,
    password: &str,
    cancel: Option<&AtomicBool>,
    overall_deadline: Option<Instant>,
) -> Result<(Value, Option<String>), String> {
    let address = ("127.0.0.1", port)
        .to_socket_addrs()
        .map_err(|_| "OpenCode global history unavailable".to_string())?
        .next()
        .ok_or_else(|| "OpenCode global history unavailable".to_string())?;
    let connect_timeout = overall_deadline
        .map(|deadline| deadline.saturating_duration_since(Instant::now()))
        .unwrap_or(SESSION_REQUEST_TIMEOUT);
    if connect_timeout.is_zero() {
        return Err("OpenCode history timed out".to_string());
    }
    let mut stream = TcpStream::connect_timeout(&address, connect_timeout)
        .map_err(|_| "OpenCode global history unavailable".to_string())?;
    stream
        .set_read_timeout(Some(
            overall_deadline
                .map(|deadline| {
                    deadline
                        .saturating_duration_since(Instant::now())
                        .min(cancel.map_or(SESSION_REQUEST_TIMEOUT, |_| Duration::from_millis(50)))
                })
                .unwrap_or_else(|| {
                    cancel.map_or(SESSION_REQUEST_TIMEOUT, |_| Duration::from_millis(50))
                }),
        ))
        .map_err(|_| "OpenCode global history unavailable".to_string())?;
    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAuthorization: Basic {}\r\nConnection: close\r\n\r\n",
        basic_auth(username, password)
    );
    std::io::Write::write_all(&mut stream, request.as_bytes())
        .map_err(|_| "OpenCode global history unavailable".to_string())?;
    let read_deadline =
        overall_deadline.unwrap_or_else(|| Instant::now() + SESSION_REQUEST_TIMEOUT);
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 32 * 1024];
    loop {
        if Instant::now() >= read_deadline {
            return Err("OpenCode history timed out".to_string());
        }
        if cancel.is_some_and(|cancel| cancel.load(Ordering::Relaxed)) {
            return Err("OpenCode history cancelled".to_string());
        }
        let size = match std::io::Read::read(&mut stream, &mut chunk) {
            Ok(size) => size,
            Err(error)
                if cancel.is_some()
                    && matches!(
                        error.kind(),
                        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                    ) =>
            {
                if Instant::now() >= read_deadline {
                    return Err("OpenCode history timed out".to_string());
                }
                continue;
            }
            Err(_) => return Err("OpenCode global history unavailable".to_string()),
        };
        if size == 0 {
            break;
        }
        if bytes.len().saturating_add(size) > MAX_PROVIDER_OUTPUT_BYTES {
            return Err("OpenCode global history output too large".to_string());
        }
        bytes.extend_from_slice(&chunk[..size]);
    }
    let split = bytes
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or_else(|| "Invalid OpenCode global history response".to_string())?;
    let headers = String::from_utf8_lossy(&bytes[..split]);
    if !headers.starts_with("HTTP/1.1 200") && !headers.starts_with("HTTP/1.0 200") {
        return Err("OpenCode global history unavailable".to_string());
    }
    let body = &bytes[split + 4..];
    let body = if headers.lines().any(|line| {
        line.split_once(':').is_some_and(|(name, value)| {
            name.eq_ignore_ascii_case("transfer-encoding")
                && value.to_ascii_lowercase().contains("chunked")
        })
    }) {
        decode_chunked(body)?
    } else {
        body.to_vec()
    };
    let value =
        serde_json::from_slice(&body).map_err(|_| "Invalid OpenCode session list".to_string())?;
    let next = headers
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("x-next-cursor")
                .then(|| value.trim().to_string())
        })
        .filter(|cursor| !cursor.is_empty());
    Ok((value, next))
}

fn decode_chunked(body: &[u8]) -> Result<Vec<u8>, String> {
    let mut result = Vec::new();
    let mut offset = 0usize;
    loop {
        let Some(end) = body[offset..]
            .windows(2)
            .position(|window| window == b"\r\n")
        else {
            return Err("Invalid OpenCode global history response".to_string());
        };
        let line_end = offset + end;
        let size_text = String::from_utf8_lossy(&body[offset..line_end]);
        let size_text = size_text.split(';').next().unwrap_or("").trim();
        let size = usize::from_str_radix(size_text, 16)
            .map_err(|_| "Invalid OpenCode global history response".to_string())?;
        offset = line_end + 2;
        if size == 0 {
            return Ok(result);
        }
        if size > body.len().saturating_sub(offset + 2)
            || result.len().saturating_add(size) > MAX_PROVIDER_OUTPUT_BYTES
        {
            return Err("OpenCode global history output too large".to_string());
        }
        result.extend_from_slice(&body[offset..offset + size]);
        offset += size;
        if body.get(offset..offset + 2) != Some(b"\r\n") {
            return Err("Invalid OpenCode global history response".to_string());
        }
        offset += 2;
    }
}

#[cfg(test)]
fn fetch_opencode_pages(port: u16, password: &str) -> Result<Vec<Value>, String> {
    fetch_opencode_pages_cancellable(port, password, None)
}

fn fetch_opencode_pages_cancellable(
    port: u16,
    password: &str,
    cancel: Option<&AtomicBool>,
) -> Result<Vec<Value>, String> {
    let overall_deadline = Instant::now() + SESSION_REQUEST_TIMEOUT;
    let mut rows = Vec::new();
    let mut cursor: Option<String> = None;
    let mut seen = HashSet::new();
    let mut cursors = HashSet::new();
    loop {
        if let Some(cursor) = &cursor {
            if !cursors.insert(cursor.clone()) {
                return Err("Invalid OpenCode history pagination".to_string());
            }
        }
        let mut path = "/experimental/session?roots=true&limit=1000".to_string();
        if let Some(cursor) = &cursor {
            path.push_str("&cursor=");
            path.push_str(&query_escape(cursor));
        }
        let (page, next) = http_json_cancellable(
            port,
            &path,
            "omni",
            password,
            cancel,
            Some(overall_deadline),
        )?;
        let page = page
            .as_array()
            .ok_or_else(|| "Invalid OpenCode session list".to_string())?;
        for row in page {
            let Some(object) = row.as_object() else {
                continue;
            };
            let Some(id) = object.get("id").and_then(Value::as_str) else {
                continue;
            };
            if !valid_opencode_id(id) || !seen.insert(id.to_string()) {
                continue;
            }
            let title = object
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or("OpenCode session");
            let cwd = object
                .get("directory")
                .and_then(Value::as_str)
                .unwrap_or("");
            let updated = object
                .get("time")
                .and_then(|time| time.get("updated"))
                .or_else(|| object.get("updated"));
            rows.push(make_session(
                "opencode",
                id.to_string(),
                if title.is_empty() {
                    "OpenCode session".into()
                } else {
                    title.into()
                },
                cwd.into(),
                millis(updated),
            ));
        }
        let Some(next) = next else { break };
        cursor = Some(next);
    }
    Ok(rows)
}

fn opencode_password() -> Result<String, String> {
    let mut random =
        File::open("/dev/urandom").map_err(|_| "OpenCode history unavailable".to_string())?;
    let mut bytes = [0u8; 24];
    std::io::Read::read_exact(&mut random, &mut bytes)
        .map_err(|_| "OpenCode history unavailable".to_string())?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn opencode_global_rows_cancellable(cancel: Option<&AtomicBool>) -> Result<Vec<Value>, String> {
    let password = opencode_password()?;
    let mut command = Command::new("opencode");
    command
        .args(["serve", "--pure", "--hostname", "127.0.0.1", "--port", "0"])
        .env("OPENCODE_SERVER_USERNAME", "omni")
        .env("OPENCODE_SERVER_PASSWORD", &password)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    isolate(&mut command);
    let mut child = command
        .spawn()
        .map_err(|_| "OpenCode history unavailable".to_string())?;
    let stdout = match child.stdout.take() {
        Some(stdout) => stdout,
        None => {
            terminate(&mut child);
            let _ = child.wait();
            return Err("OpenCode history unavailable".to_string());
        }
    };
    let (sender, receiver) = mpsc::sync_channel(1);
    thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        while let Ok(Some(line)) = read_bounded_line(&mut reader, MAX_PROVIDER_LINE_BYTES) {
            if sender.send(line).is_err() {
                break;
            }
        }
    });
    let deadline = Instant::now() + SESSION_REQUEST_TIMEOUT;
    let port = loop {
        if cancel.is_some_and(|cancel| cancel.load(Ordering::Relaxed)) {
            terminate(&mut child);
            let _ = child.wait();
            return Err("OpenCode history cancelled".to_string());
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            terminate(&mut child);
            let _ = child.wait();
            return Err("OpenCode history timed out".to_string());
        }
        let line = match receiver.recv_timeout(remaining.min(Duration::from_millis(50))) {
            Ok(line) => line,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => {
                terminate(&mut child);
                let _ = child.wait();
                return Err("OpenCode history unavailable".to_string());
            }
        };
        let Some(prefix) = line.split("server listening on http://127.0.0.1:").nth(1) else {
            continue;
        };
        let Some(port_text) = prefix.split_whitespace().next() else {
            continue;
        };
        if let Ok(port) = port_text.parse::<u16>() {
            break port;
        }
    };
    let result = fetch_opencode_pages_cancellable(port, &password, cancel);
    terminate(&mut child);
    let _ = child.wait();
    result
}

struct CodexClient {
    child: Child,
    receiver: Receiver<Result<String, String>>,
    next_id: u64,
}

impl CodexClient {
    fn connect() -> Result<Self, String> {
        Self::connect_cancellable(None)
    }

    fn connect_cancellable(cancel: Option<&AtomicBool>) -> Result<Self, String> {
        Self::connect_with_program("codex", cancel)
    }

    fn connect_with_program(program: &str, cancel: Option<&AtomicBool>) -> Result<Self, String> {
        let mut command = Command::new(program);
        command
            .args(["app-server", "--listen", "stdio://"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        isolate(&mut command);
        let mut child = command
            .spawn()
            .map_err(|_| "Codex history unavailable".to_string())?;
        let stdout = match child.stdout.take() {
            Some(stdout) => stdout,
            None => {
                terminate(&mut child);
                let _ = child.wait();
                return Err("Codex history unavailable".to_string());
            }
        };
        let (sender, receiver) = mpsc::sync_channel(1);
        thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                let response = match read_codex_line(&mut reader, MAX_CODEX_RESPONSE_UNITS) {
                    Ok(Some(line)) => Ok(line),
                    Ok(None) => break,
                    Err(error) => Err(format!("Codex history read failed: {error}")),
                };
                let failed = response.is_err();
                if sender.send(response).is_err() || failed {
                    break;
                }
            }
        });
        let mut client = Self {
            child,
            receiver,
            next_id: 0,
        };
        client.call_cancellable(
            "initialize",
            json!({"clientInfo":{"name":"herdr_omni_history","version":"1.0"}}),
            cancel,
        )?;
        if cancel.is_some_and(|cancel| cancel.load(Ordering::Relaxed)) {
            return Err("Codex history cancelled".to_string());
        }
        client.send(json!({"method":"initialized"}))?;
        Ok(client)
    }

    fn send(&mut self, request: Value) -> Result<(), String> {
        let stdin = self
            .child
            .stdin
            .as_mut()
            .ok_or_else(|| "Codex history unavailable".to_string())?;
        serde_json::to_writer(&mut *stdin, &request)
            .map_err(|_| "Codex history unavailable".to_string())?;
        std::io::Write::write_all(stdin, b"\n").map_err(|_| "Codex history unavailable".to_string())
    }

    fn call_cancellable(
        &mut self,
        method: &str,
        params: Value,
        cancel: Option<&AtomicBool>,
    ) -> Result<Value, String> {
        self.next_id += 1;
        let id = self.next_id;
        if cancel.is_some_and(|cancel| cancel.load(Ordering::Relaxed)) {
            return Err("Codex history cancelled".to_string());
        }
        self.send(json!({"id":id,"method":method,"params":params}))?;
        let deadline = Instant::now() + SESSION_REQUEST_TIMEOUT;
        loop {
            if cancel.is_some_and(|cancel| cancel.load(Ordering::Relaxed)) {
                return Err("Codex history cancelled".to_string());
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err("Codex history timed out".to_string());
            }
            let line = match self
                .receiver
                .recv_timeout(remaining.min(if cancel.is_some() {
                    Duration::from_millis(50)
                } else {
                    remaining
                })) {
                Ok(line) => line?,
                Err(RecvTimeoutError::Timeout) if cancel.is_some() => continue,
                Err(RecvTimeoutError::Timeout) => return Err("Codex history timed out".to_string()),
                Err(RecvTimeoutError::Disconnected) => {
                    return Err("Codex history unavailable".to_string())
                }
            };
            let response: Value =
                serde_json::from_str(&line).map_err(|_| "Codex history unavailable".to_string())?;
            if response.get("id").and_then(Value::as_u64) != Some(id) {
                continue;
            }
            if response.get("error").is_some_and(|error| !error.is_null()) {
                return Err("Codex history request failed".to_string());
            }
            return Ok(response.get("result").cloned().unwrap_or(Value::Null));
        }
    }
}

impl Drop for CodexClient {
    fn drop(&mut self) {
        terminate(&mut self.child);
        let _ = self.child.wait();
    }
}

fn executable_file(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::metadata(path)
            .map(|metadata| metadata.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn claude_root(home: &Path) -> PathBuf {
    std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".claude"))
}

fn codex_prefilter_roots(home: &Path) -> Vec<PathBuf> {
    let root = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".codex"));
    vec![root.join("sessions"), root.join("archived_sessions")]
}

fn path_basename(path: &str) -> &str {
    path.trim_end_matches('/')
        .rsplit('/')
        .next()
        .filter(|part| !part.is_empty())
        .unwrap_or("")
}

fn saved_session_item(session: &Value) -> Value {
    let provider = session
        .get("provider")
        .and_then(Value::as_str)
        .unwrap_or("session");
    let id = session.get("id").and_then(Value::as_str).unwrap_or("");
    let title = clean_text(session.get("title").and_then(Value::as_str).unwrap_or(""));
    let cwd = session.get("cwd").and_then(Value::as_str).unwrap_or("");
    let title = title.chars().take(160).collect::<String>();
    let title = if title.is_empty() {
        provider.to_string()
    } else {
        title
    };
    let project = clean_text(path_basename(cwd));
    let project = if project.is_empty() {
        "Unknown project".to_string()
    } else {
        project
    };
    let display_title = format!("{} - {}", title, project);
    json!({
        "id": format!("saved:agent:{provider}:{id}"),
        "title": display_title,
        "category": "Agents",
        "icon": "◈",
        "description": cwd,
        "aliases": [provider],
        "searchPaths": [cwd],
        "shortcuts": [],
        "priority": 5,
        "session": session,
        "savedSession": true,
        "invocation": {"kind": "resume-session", "session": session},
    })
}

#[cfg(test)]
fn list_claude_root(root: &Path) -> Vec<Value> {
    list_claude_root_cancellable(root, None)
}

fn list_claude_cancellable(home: &Path, cancel: Option<&AtomicBool>) -> Vec<Value> {
    list_claude_root_cancellable(&claude_root(home), cancel)
}

fn list_claude_root_cancellable(root: &Path, cancel: Option<&AtomicBool>) -> Vec<Value> {
    let mut paths = Vec::new();
    walk_files_cancel(&root.join("projects"), Some("jsonl"), &mut paths, cancel);
    paths.sort();
    let mut sessions = HashMap::<String, (i64, Value)>::new();
    for path in paths {
        if cancel.is_some_and(|cancel| cancel.load(Ordering::Relaxed)) {
            break;
        }
        let Ok(lines) = read_json_lines_cancellable(&path, cancel) else {
            continue;
        };
        let messages = claude_messages(&lines);
        if messages.is_empty() {
            continue;
        }
        if lines
            .iter()
            .any(|line| line.get("isSidechain").and_then(Value::as_bool) == Some(true))
        {
            continue;
        }
        let id = lines
            .iter()
            .find_map(|line| {
                line.as_object()
                    .and_then(|object| string_field(object, "sessionId"))
            })
            .or_else(|| path.file_stem().and_then(|name| name.to_str()))
            .unwrap_or_default()
            .to_string();
        if id.is_empty() {
            continue;
        }
        let title = ["customTitle", "summary", "firstPrompt"]
            .iter()
            .find_map(|field| {
                lines.iter().rev().find_map(|line| {
                    line.as_object()
                        .and_then(|object| string_field(object, field))
                })
            })
            .map(clean_text)
            .filter(|text| !text.is_empty())
            .or_else(|| {
                messages
                    .iter()
                    .find(|message| message.role == "user")
                    .map(|message| clean_text(&message.text))
            })
            .unwrap_or_else(|| "Claude session".into());
        let cwd = lines
            .iter()
            .find_map(|line| {
                line.as_object()
                    .and_then(|object| string_field(object, "cwd"))
            })
            .unwrap_or_default()
            .to_string();
        let updated = file_millis(&path, &lines);
        let session = make_session("claude", id.clone(), title, cwd, updated);
        if sessions.get(&id).is_none_or(|(old, _)| updated > *old) {
            sessions.insert(id, (updated, session));
        }
    }
    let mut result: Vec<_> = sessions.into_values().map(|(_, value)| value).collect();
    result.sort_by(|a, b| {
        b.get("updatedAt")
            .and_then(Value::as_i64)
            .cmp(&a.get("updatedAt").and_then(Value::as_i64))
    });
    result
}

fn valid_opencode_id(id: &str) -> bool {
    id.strip_prefix("ses_")
        .is_some_and(|suffix| (1..=196).contains(&suffix.len()))
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

fn opencode_export_messages(value: &Value, id: &str) -> Result<Vec<PreviewMessage>, String> {
    let info_id = value
        .get("info")
        .and_then(|info| info.get("id"))
        .and_then(Value::as_str)
        .ok_or_else(|| "Invalid OpenCode session export".to_string())?;
    if info_id != id {
        return Err("Invalid OpenCode session export".to_string());
    }
    let messages = value
        .get("messages")
        .and_then(Value::as_array)
        .ok_or_else(|| "Invalid OpenCode session export".to_string())?;
    let mut result = Vec::new();
    for message in messages {
        let role = message
            .get("info")
            .and_then(|info| info.get("role"))
            .and_then(Value::as_str);
        let Some(role) = role.filter(|role| *role == "user" || *role == "assistant") else {
            continue;
        };
        let Some(parts) = message.get("parts").and_then(Value::as_array) else {
            continue;
        };
        for part in parts {
            if part.get("type").and_then(Value::as_str) != Some("text")
                || part.get("ignored").and_then(Value::as_bool) == Some(true)
                || part.get("synthetic").and_then(Value::as_bool) == Some(true)
            {
                continue;
            }
            if let Some(text) = part.get("text").and_then(Value::as_str) {
                result.push(PreviewMessage {
                    role: role.into(),
                    text: text.into(),
                });
            }
        }
    }
    Ok(result)
}

fn opencode_cli_messages_cancellable(
    id: &str,
    cancel: Option<&AtomicBool>,
) -> Result<Vec<PreviewMessage>, String> {
    if !valid_opencode_id(id) {
        return Err("Invalid OpenCode session ID".to_string());
    }
    let output = run_bounded_command_cancellable("opencode", &["export", id], cancel)?;
    let value: Value =
        serde_json::from_str(&output).map_err(|_| "Invalid OpenCode session export".to_string())?;
    opencode_export_messages(&value, id)
}

const OPENCODE_READ_CONCURRENCY: usize = 4;
type OpenCodeReader =
    Arc<dyn Fn(&str, &AtomicBool) -> Result<Vec<PreviewMessage>, String> + Send + Sync>;
type OpenCodeResult = (usize, Result<Vec<PreviewMessage>, String>);

struct OpenCodePool {
    receiver: Option<Receiver<OpenCodeResult>>,
    tasks: Option<mpsc::SyncSender<usize>>,
    next_index: usize,
    total_ids: usize,
    cancel: Arc<AtomicBool>,
    handles: Vec<JoinHandle<()>>,
    pending: HashMap<usize, Result<Vec<PreviewMessage>, String>>,
}

impl OpenCodePool {
    fn new(ids: &[String]) -> Self {
        Self::new_with_reader(
            ids,
            Arc::new(|id, cancel| opencode_cli_messages_cancellable(id, Some(cancel))),
        )
    }

    fn new_with_reader(ids: &[String], reader: OpenCodeReader) -> Self {
        let ids = Arc::new(ids.to_vec());
        let cancel = Arc::new(AtomicBool::new(false));
        let (task_sender, task_receiver) = mpsc::sync_channel::<usize>(OPENCODE_READ_CONCURRENCY);
        let task_receiver = Arc::new(Mutex::new(task_receiver));
        let (sender, receiver) = mpsc::sync_channel::<OpenCodeResult>(OPENCODE_READ_CONCURRENCY);
        let mut handles = Vec::new();
        for _ in 0..ids.len().min(OPENCODE_READ_CONCURRENCY) {
            let task_receiver = Arc::clone(&task_receiver);
            let ids = Arc::clone(&ids);
            let cancel = Arc::clone(&cancel);
            let reader = Arc::clone(&reader);
            let sender = sender.clone();
            handles.push(std::thread::spawn(move || loop {
                if cancel.load(Ordering::Relaxed) {
                    break;
                }
                let index = {
                    let receiver = task_receiver.lock().expect("OpenCode task lock");
                    match receiver.recv() {
                        Ok(index) => index,
                        Err(_) => break,
                    }
                };
                let result = reader(&ids[index], &cancel);
                if sender.send((index, result)).is_err() {
                    break;
                }
            }));
        }
        drop(sender);
        let mut pool = Self {
            receiver: Some(receiver),
            tasks: Some(task_sender),
            next_index: 0,
            total_ids: ids.len(),
            cancel,
            handles,
            pending: HashMap::new(),
        };
        for _ in 0..ids.len().min(OPENCODE_READ_CONCURRENCY) {
            let _ = pool.dispatch_next();
        }
        pool
    }

    fn dispatch_next(&mut self) -> Result<(), String> {
        if self.next_index >= self.total_ids {
            return Ok(());
        }
        let index = self.next_index;
        self.next_index += 1;
        self.tasks
            .as_ref()
            .ok_or_else(|| "OpenCode history cancelled".to_string())?
            .send(index)
            .map_err(|_| "OpenCode history cancelled".to_string())
    }

    fn take(
        &mut self,
        index: usize,
        external_cancel: Option<&AtomicBool>,
    ) -> Result<Vec<PreviewMessage>, String> {
        loop {
            if external_cancel.is_some_and(|cancel| cancel.load(Ordering::Relaxed)) {
                self.cancel.store(true, Ordering::Relaxed);
                return Err("OpenCode history cancelled".to_string());
            }
            if let Some(result) = self.pending.remove(&index) {
                self.dispatch_next()?;
                return result;
            }
            let receiver = self
                .receiver
                .as_ref()
                .ok_or_else(|| "OpenCode history unavailable".to_string())?;
            match receiver.recv_timeout(Duration::from_millis(50)) {
                Ok((received, result)) if received == index => {
                    self.dispatch_next()?;
                    return result;
                }
                Ok((received, result)) => {
                    self.pending.insert(received, result);
                }
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => {
                    return Err("OpenCode history unavailable".to_string())
                }
            }
        }
    }
}

impl Drop for OpenCodePool {
    fn drop(&mut self) {
        // Disconnect workers before joining them. A worker may be blocked on
        // the bounded result queue; dropping the receiver makes its send fail
        // immediately after cancellation.
        self.tasks.take();
        self.receiver.take();
        self.cancel.store(true, Ordering::Relaxed);
        for handle in self.handles.drain(..) {
            let _ = handle.join();
        }
    }
}

fn provider_messages(session: &Value) -> Result<Vec<PreviewMessage>, String> {
    provider_messages_cancellable(session, None)
}

fn provider_messages_cancellable(
    session: &Value,
    cancel: Option<&AtomicBool>,
) -> Result<Vec<PreviewMessage>, String> {
    let (provider, id) =
        session_parts(session).ok_or_else(|| "Invalid saved session".to_string())?;
    if !command_available(provider) {
        return Err(format!("{provider} history unavailable"));
    }
    let home = home_dir().ok_or_else(|| "Home directory unavailable".to_string())?;
    match provider {
        "codex" => CodexClient::connect_cancellable(cancel).and_then(|mut client| {
            let result = client.call_cancellable(
                "thread/read",
                json!({"threadId": id, "includeTurns": true}),
                cancel,
            )?;
            let thread = result
                .get("thread")
                .ok_or_else(|| "Invalid Codex session export".to_string())?;
            Ok(codex_thread_messages(thread))
        }),
        "claude" => find_provider_messages(&claude_root(&home).join("projects"), id, cancel),
        "opencode" => opencode_cli_messages_cancellable(id, cancel),
        _ => Err("Unsupported session provider".to_string()),
    }
}

fn find_provider_messages(
    root: &Path,
    id: &str,
    cancel: Option<&AtomicBool>,
) -> Result<Vec<PreviewMessage>, String> {
    let mut paths = Vec::new();
    walk_files_cancel(root, Some("jsonl"), &mut paths, cancel);
    let mut best = None;
    for path in paths {
        if cancel.is_some_and(|cancel| cancel.load(Ordering::Relaxed)) {
            return Err("Claude history cancelled".to_string());
        }
        let filename = path
            .file_stem()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        if filename != id {
            continue;
        }
        let lines = read_json_lines_cancellable(&path, cancel)?;
        if cancel.is_some_and(|cancel| cancel.load(Ordering::Relaxed)) {
            return Err("Claude history cancelled".to_string());
        }
        let updated = file_millis(&path, &lines);
        if best
            .as_ref()
            .is_none_or(|(previous, _)| updated > *previous)
        {
            best = Some((updated, claude_messages(&lines)));
        }
    }
    best.map(|(_, messages)| messages)
        .ok_or_else(|| "Session transcript unavailable".to_string())
}

const TRANSCRIPT_RG_THREADS: &str = "4";

fn transcript_compact_phrase(phrase: &str) -> String {
    ["{", "}", "[", "]", ",", ":"]
        .iter()
        .fold(phrase.to_string(), |value, punctuation| {
            value
                .replace(&format!(" {punctuation}"), punctuation)
                .replace(&format!("{punctuation} "), punctuation)
        })
}

fn transcript_patterns(phrase: &str) -> Option<Vec<String>> {
    let compact = transcript_compact_phrase(phrase);
    let encoded = serde_json::to_string(phrase).ok()?;
    let encoded_compact = serde_json::to_string(&compact).ok()?;
    let mut patterns = vec![
        phrase.to_string(),
        compact,
        encoded.clone(),
        encoded[1..encoded.len().saturating_sub(1)].to_string(),
        encoded_compact.clone(),
        encoded_compact[1..encoded_compact.len().saturating_sub(1)].to_string(),
    ];
    patterns.sort();
    patterns.dedup();
    Some(patterns)
}

fn transcript_rg_args(
    mode: &str,
    glob: &str,
    fixed: bool,
    patterns: &[String],
    paths: &[PathBuf],
) -> Vec<String> {
    let mut args = vec![
        mode.to_string(),
        "--null".to_string(),
        "--hidden".to_string(),
        "--no-ignore".to_string(),
        "--ignore-case".to_string(),
        "--glob".to_string(),
        glob.to_string(),
        "--threads".to_string(),
        TRANSCRIPT_RG_THREADS.to_string(),
    ];
    if fixed {
        args.push("--fixed-strings".to_string());
    }
    for pattern in patterns {
        args.push("-e".to_string());
        args.push(pattern.clone());
    }
    args.push("--".to_string());
    args.extend(paths.iter().map(|path| path.to_string_lossy().into_owned()));
    args
}

fn transcript_rg_paths(output: &str) -> Vec<PathBuf> {
    output
        .split('\0')
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .collect()
}

fn transcript_file_id(provider: &str, file: &Path, ids: &[String]) -> Option<String> {
    let name = file.file_name()?.to_str()?;
    if provider == "claude" {
        return ids.iter().find(|id| name == format!("{id}.jsonl")).cloned();
    }
    ids.iter()
        .find(|id| name == format!("{id}.jsonl") || name.ends_with(&format!("-{id}.jsonl")))
        .cloned()
}

fn regex_escape(value: &str) -> String {
    value
        .chars()
        .flat_map(|character| {
            if ".*+?^${}()|[]\\".contains(character) {
                vec!['\\', character]
            } else {
                vec![character]
            }
        })
        .collect()
}

/// Port of the TypeScript coarse transcript filter. `None` means that the
/// corpus/layout or an rg probe was not trustworthy, so callers must scan all
/// sessions. `Some(empty)` is a safe, authoritative no-hit result.
fn prefilter_transcript_sessions(
    provider: &str,
    sessions: &[&Value],
    query: &str,
    roots: &[PathBuf],
    cancel: Option<&AtomicBool>,
) -> Option<HashSet<String>> {
    prefilter_transcript_sessions_with_command("rg", provider, sessions, query, roots, cancel)
}

fn prefilter_transcript_sessions_with_command(
    rg_program: &str,
    provider: &str,
    sessions: &[&Value],
    query: &str,
    roots: &[PathBuf],
    cancel: Option<&AtomicBool>,
) -> Option<HashSet<String>> {
    if !command_available(rg_program) || roots.is_empty() || sessions.is_empty() {
        return None;
    }
    let phrase = transcript_terms(query)?;
    let mut ids = sessions
        .iter()
        .filter_map(|session| {
            session_parts(session)
                .filter(|(kind, _)| *kind == provider)
                .map(|(_, id)| id.to_string())
        })
        .collect::<Vec<_>>();
    if ids.is_empty() {
        return None;
    }
    if provider == "codex" {
        ids.sort_by_key(|id| std::cmp::Reverse(id.len()));
    }
    let glob = if provider == "codex" {
        "rollout-*.jsonl"
    } else {
        "*.jsonl"
    };
    let mut existing_roots = Vec::new();
    let mut files = Vec::new();
    for root in roots {
        let args = vec![
            "--files".to_string(),
            "--null".to_string(),
            "--hidden".to_string(),
            "--no-ignore".to_string(),
            "--glob".to_string(),
            glob.to_string(),
            "--threads".to_string(),
            TRANSCRIPT_RG_THREADS.to_string(),
            "--".to_string(),
            root.to_string_lossy().into_owned(),
        ];
        let (status, output) = run_bounded_command_status(rg_program, &args, cancel).ok()?;
        match status.code() {
            Some(0) => {
                existing_roots.push(root.clone());
                files.extend(transcript_rg_paths(&output));
            }
            Some(1) => existing_roots.push(root.clone()),
            Some(2) => {}
            _ => return None,
        }
    }
    if existing_roots.is_empty() || files.is_empty() {
        return None;
    }

    let patterns = transcript_patterns(&phrase)?;
    let args = transcript_rg_args(
        "--files-with-matches",
        glob,
        true,
        &patterns,
        &existing_roots,
    );
    let (status, output) = run_bounded_command_status(rg_program, &args, cancel).ok()?;
    if status.code() == Some(1) {
        return Some(HashSet::new());
    }
    if status.code() != Some(0) {
        return None;
    }
    let matched_files = transcript_rg_paths(&output);
    if matched_files.is_empty() {
        return Some(HashSet::new());
    }
    let mapped_corpus = files
        .iter()
        .filter_map(|file| transcript_file_id(provider, file, &ids))
        .collect::<HashSet<_>>();
    if mapped_corpus.is_empty() {
        return None;
    }
    let broad_ids = matched_files
        .iter()
        .filter_map(|file| transcript_file_id(provider, file, &ids))
        .collect::<HashSet<_>>();
    if broad_ids.is_empty() {
        return Some(HashSet::new());
    }

    let marker = if provider == "codex" {
        r#""type"[[:space:]]*:[[:space:]]*"message""#
    } else {
        r#""type"[[:space:]]*:[[:space:]]*"(?:user|assistant)""#
    };
    let marker_args = transcript_rg_args(
        "--files-with-matches",
        glob,
        false,
        &[marker.to_string()],
        &matched_files,
    );
    let Ok((marker_status, marker_output)) =
        run_bounded_command_status(rg_program, &marker_args, cancel)
    else {
        return Some(broad_ids);
    };
    if marker_status.code() != Some(0) {
        return Some(broad_ids);
    }
    let structured_ids = transcript_rg_paths(&marker_output)
        .iter()
        .filter_map(|file| transcript_file_id(provider, file, &ids))
        .collect::<HashSet<_>>();
    if !broad_ids.iter().all(|id| structured_ids.contains(id)) {
        return Some(broad_ids);
    }
    let conversation_patterns = patterns
        .iter()
        .map(|pattern| {
            let escaped = regex_escape(pattern);
            format!("(?:{marker}.*{escaped}|{escaped}.*{marker})")
        })
        .collect::<Vec<_>>();
    let conversation_args = transcript_rg_args(
        "--files-with-matches",
        glob,
        false,
        &conversation_patterns,
        &matched_files,
    );
    let Ok((conversation_status, conversation_output)) =
        run_bounded_command_status(rg_program, &conversation_args, cancel)
    else {
        return Some(broad_ids);
    };
    match conversation_status.code() {
        Some(0) => Some(
            transcript_rg_paths(&conversation_output)
                .iter()
                .filter_map(|file| transcript_file_id(provider, file, &ids))
                .collect(),
        ),
        Some(1) => Some(HashSet::new()),
        _ => Some(broad_ids),
    }
}

/// Build the filename-to-session index once for an interactive search.  A
/// search may contain hundreds of saved sessions, so walking each provider's
/// tree for every item would otherwise make the scan quadratic.
fn transcript_path_index(
    root: &Path,
    query: Option<&str>,
    cancel: Option<&AtomicBool>,
) -> (HashMap<String, PathBuf>, bool) {
    let mut paths = Vec::new();
    walk_files_cancel(root, Some("jsonl"), &mut paths, cancel);
    let mut prefiltered = false;
    if let Some(query) = query.filter(|query| !clean_text(query).is_empty()) {
        if command_available("rg") {
            let phrase = clean_text(query);
            let root_arg = root.to_string_lossy().into_owned();
            let args = vec![
                "--files-with-matches".to_string(),
                "--null".to_string(),
                "--hidden".to_string(),
                "--no-ignore".to_string(),
                "--ignore-case".to_string(),
                "--fixed-strings".to_string(),
                "-e".to_string(),
                phrase,
                "--".to_string(),
                root_arg,
            ];
            if let Ok(output) = run_bounded_command_cancellable("rg", &args, cancel) {
                let matched: HashSet<PathBuf> = output
                    .split('\0')
                    .filter(|path| !path.is_empty())
                    .map(PathBuf::from)
                    .map(|path| {
                        if path.is_absolute() {
                            path
                        } else {
                            root.join(path)
                        }
                    })
                    .collect();
                if !matched.is_empty() {
                    let before = paths.len();
                    paths.retain(|path| matched.contains(path));
                    prefiltered = paths.len() < before;
                }
            }
        }
    }
    let mut index = HashMap::new();
    let mut timestamps = HashMap::<String, i64>::new();
    for path in paths {
        if cancel.is_some_and(|cancel| cancel.load(Ordering::Relaxed)) {
            break;
        }
        let filename = path
            .file_stem()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        if !filename.is_empty() {
            let Ok(lines) = read_json_lines_cancellable(&path, cancel) else {
                continue;
            };
            if cancel.is_some_and(|cancel| cancel.load(Ordering::Relaxed)) {
                break;
            }
            let updated = file_millis(&path, &lines);
            if timestamps
                .get(filename)
                .is_none_or(|previous| updated > *previous)
            {
                timestamps.insert(filename.to_string(), updated);
                index.insert(filename.to_string(), path);
            }
        }
    }
    (index, prefiltered)
}

fn indexed_provider_messages(
    session: &Value,
    claude: &HashMap<String, PathBuf>,
    codex_client: Option<&mut CodexClient>,
    cancel: Option<&AtomicBool>,
) -> Result<Vec<PreviewMessage>, String> {
    let (provider, id) =
        session_parts(session).ok_or_else(|| "Invalid saved session".to_string())?;
    if !command_available(provider) {
        return Err(format!("{provider} history unavailable"));
    }
    if provider == "opencode" {
        return opencode_cli_messages_cancellable(id, cancel);
    }
    if provider == "codex" {
        if let Some(client) = codex_client {
            let result = client.call_cancellable(
                "thread/read",
                json!({"threadId": id, "includeTurns": true}),
                cancel,
            )?;
            let thread = result
                .get("thread")
                .ok_or_else(|| "Invalid Codex session export".to_string())?;
            return Ok(codex_thread_messages(thread));
        }
        return Err("Codex history unavailable".to_string());
    }
    let path = claude
        .get(id)
        .ok_or_else(|| "Session transcript unavailable".to_string())?;
    let lines = read_json_lines_cancellable(path, cancel)?;
    Ok(claude_messages(&lines))
}

/// List all provider sessions. Missing providers are treated as unavailable
/// and do not hide sessions found in another provider's store.
type ProviderWorker =
    Box<dyn FnOnce(&AtomicBool, mpsc::SyncSender<Result<Vec<Value>, String>>) + Send>;

fn run_provider_workers<F>(
    cancel: &AtomicBool,
    mut publish: F,
    workers: Vec<ProviderWorker>,
) -> Result<(), String>
where
    F: FnMut(Vec<Value>),
{
    let (sender, receiver) = mpsc::sync_channel::<Result<Vec<Value>, String>>(8);
    thread::scope(|scope| {
        for worker in workers {
            let sender = sender.clone();
            scope.spawn(move || worker(cancel, sender));
        }
        drop(sender);
        let mut first_error = None;
        for result in receiver {
            match result {
                Ok(sessions) if !cancel.load(Ordering::Relaxed) => publish(sessions),
                Ok(_) => return Ok(()),
                Err(error) if !cancel.load(Ordering::Relaxed) => {
                    first_error.get_or_insert(error);
                }
                Err(_) => return Ok(()),
            }
        }
        first_error.map_or(Ok(()), Err)
    })
}

pub fn list_sessions_incremental<F>(publish: F) -> Result<(), String>
where
    F: FnMut(Vec<Value>),
{
    let cancel = AtomicBool::new(false);
    list_sessions_incremental_cancellable(&cancel, publish)
}

/// Incrementally list providers while allowing a UI shutdown to terminate
/// each child process and its receive loop within the polling interval.
pub fn list_sessions_incremental_cancellable<F>(
    cancel: &AtomicBool,
    publish: F,
) -> Result<(), String>
where
    F: FnMut(Vec<Value>),
{
    let home = home_dir().ok_or_else(|| "Home directory unavailable".to_string())?;
    let mut workers = Vec::new();
    if command_available("codex") {
        workers.push(Box::new(
            |cancel: &AtomicBool, sender: mpsc::SyncSender<Result<Vec<Value>, String>>| {
                let mut receiver_closed = false;
                let result = list_codex_appserver_incremental(Some(cancel), |page| {
                    if sender.send(Ok(page)).is_err() {
                        receiver_closed = true;
                        cancel.store(true, Ordering::Relaxed);
                    }
                });
                if let Err(error) = result {
                    if !receiver_closed && !cancel.load(Ordering::Relaxed) {
                        let _ = sender.send(Err(error));
                    }
                }
            },
        ) as ProviderWorker);
    }
    if command_available("claude") {
        let claude_home = home.clone();
        workers.push(Box::new(
            move |cancel: &AtomicBool, sender: mpsc::SyncSender<Result<Vec<Value>, String>>| {
                let _ = sender.send(Ok(list_claude_cancellable(&claude_home, Some(cancel))));
            },
        ) as ProviderWorker);
    }
    if command_available("opencode") {
        workers.push(Box::new(
            |cancel: &AtomicBool, sender: mpsc::SyncSender<Result<Vec<Value>, String>>| {
                let _ = sender.send(opencode_global_rows_cancellable(Some(cancel)));
            },
        ) as ProviderWorker);
    }
    run_provider_workers(cancel, publish, workers)
}

pub fn list_sessions() -> Result<Vec<Value>, String> {
    let mut sessions = Vec::new();
    let provider_error = list_sessions_incremental(|batch| sessions.extend(batch)).err();
    let mut seen = HashSet::new();
    sessions.retain(|session| session_key(session).is_some_and(|key| seen.insert(key)));
    sessions.sort_by(|a, b| {
        b.get("updatedAt")
            .and_then(Value::as_i64)
            .cmp(&a.get("updatedAt").and_then(Value::as_i64))
    });
    provider_error.map_or(Ok(sessions), Err)
}

/// Return the bounded latest exchange for one saved session.
pub fn preview(session: &Value) -> Result<String, String> {
    let messages = provider_messages(session)?;
    Ok(bounded_preview(&messages, SESSION_PREVIEW_MAX_CHARS))
}

/// Cancellable boundary for UI callers switching between saved sessions.
/// Provider transports are bounded independently; cancellation prevents a
/// stale completed read from being rendered after the selection changed.
pub fn preview_cancellable(session: &Value, cancel: &AtomicBool) -> Result<String, String> {
    if cancel.load(Ordering::Relaxed) {
        return Ok(String::new());
    }
    let messages = provider_messages_cancellable(session, Some(cancel))?;
    if cancel.load(Ordering::Relaxed) {
        return Ok(String::new());
    }
    Ok(bounded_preview(&messages, SESSION_PREVIEW_MAX_CHARS))
}

/// Search only user and assistant text and return bounded contextual hits.
pub fn search(sessions: &[Value], query: &str) -> Result<Vec<Value>, String> {
    if transcript_terms(query).is_none() {
        return Ok(Vec::new());
    }
    let home = home_dir().ok_or_else(|| "Home directory unavailable".to_string())?;
    let session_refs = sessions.iter().collect::<Vec<_>>();
    let claude_prefiltered = prefilter_transcript_sessions(
        "claude",
        &session_refs,
        query,
        &[claude_root(&home).join("projects")],
        None,
    );
    let claude = transcript_path_index(&claude_root(&home).join("projects"), None, None).0;
    let codex_prefiltered = prefilter_transcript_sessions(
        "codex",
        &session_refs,
        query,
        &codex_prefilter_roots(&home),
        None,
    );
    let sessions_to_scan: Vec<&Value> = sessions
        .iter()
        .filter(|session| {
            session_parts(session).is_none_or(|(provider, id)| match provider {
                "claude" => claude_prefiltered
                    .as_ref()
                    .is_none_or(|ids| ids.contains(id)),
                "codex" => codex_prefiltered
                    .as_ref()
                    .is_none_or(|ids| ids.contains(id)),
                _ => true,
            })
        })
        .collect();
    let needs_codex = sessions_to_scan
        .iter()
        .any(|session| session_parts(session).is_some_and(|(provider, _)| provider == "codex"));
    let mut provider_error = None;
    let mut codex_client = if needs_codex {
        match CodexClient::connect() {
            Ok(client) => Some(client),
            Err(error) => {
                provider_error = Some(error);
                None
            }
        }
    } else {
        None
    };
    let opencode_ids = sessions_to_scan
        .iter()
        .filter_map(|session| {
            session_parts(session)
                .filter(|(provider, _)| *provider == "opencode")
                .map(|(_, id)| id.to_string())
        })
        .collect::<Vec<_>>();
    let mut opencode_pool = OpenCodePool::new(&opencode_ids);
    let mut opencode_index = 0usize;
    let mut hits = Vec::new();
    for session in sessions_to_scan {
        let messages = if session_parts(session).is_some_and(|(provider, _)| provider == "opencode")
        {
            let result = opencode_pool.take(opencode_index, None);
            opencode_index += 1;
            result
        } else {
            match indexed_provider_messages(session, &claude, codex_client.as_mut(), None) {
                Ok(messages) => Ok(messages),
                Err(error) => {
                    provider_error.get_or_insert(error);
                    continue;
                }
            }
        };
        if let Err(error) = messages {
            provider_error.get_or_insert(error);
            continue;
        }
        let messages = messages.unwrap_or_default();
        let text: Vec<String> = messages.into_iter().map(|message| message.text).collect();
        if let Some(excerpt) = transcript_excerpt(&text, query) {
            hits.push(json!({ "session": session, "excerpt": excerpt }));
            if hits.len() >= TRANSCRIPT_RESULT_LIMIT {
                break;
            }
        }
    }
    provider_error.map_or(Ok(hits), Err)
}

/// Search with cancellation and incremental callbacks for interactive UIs.
/// A callback receives the same `{session, excerpt}` object returned by
/// [`search`], while progress is reported after every attempted transcript.
pub fn search_cancellable<F, P>(
    sessions: &[Value],
    query: &str,
    cancel: &AtomicBool,
    mut publish: F,
    mut progress: P,
) -> Result<(), String>
where
    F: FnMut(Value),
    P: FnMut(usize, usize),
{
    if transcript_terms(query).is_none() {
        return Ok(());
    }
    let home = home_dir().ok_or_else(|| "Home directory unavailable".to_string())?;
    let session_refs = sessions.iter().collect::<Vec<_>>();
    let claude_prefiltered = prefilter_transcript_sessions(
        "claude",
        &session_refs,
        query,
        &[claude_root(&home).join("projects")],
        Some(cancel),
    );
    let claude = transcript_path_index(&claude_root(&home).join("projects"), None, Some(cancel)).0;
    if cancel.load(Ordering::Relaxed) {
        return Ok(());
    }
    let codex_prefiltered = prefilter_transcript_sessions(
        "codex",
        &session_refs,
        query,
        &codex_prefilter_roots(&home),
        Some(cancel),
    );
    let sessions_to_scan: Vec<&Value> = sessions
        .iter()
        .filter(|session| {
            session_parts(session).is_none_or(|(provider, id)| match provider {
                "claude" => claude_prefiltered
                    .as_ref()
                    .is_none_or(|ids| ids.contains(id)),
                "codex" => codex_prefiltered
                    .as_ref()
                    .is_none_or(|ids| ids.contains(id)),
                _ => true,
            })
        })
        .collect();
    let needs_codex = sessions_to_scan
        .iter()
        .any(|session| session_parts(session).is_some_and(|(provider, _)| provider == "codex"));
    let mut provider_error = None;
    let mut codex_client = if needs_codex {
        match CodexClient::connect_cancellable(Some(cancel)) {
            Ok(client) => Some(client),
            Err(_error) if cancel.load(Ordering::Relaxed) => return Ok(()),
            Err(error) => {
                provider_error = Some(error);
                None
            }
        }
    } else {
        None
    };
    let opencode_ids = sessions_to_scan
        .iter()
        .filter_map(|session| {
            session_parts(session)
                .filter(|(provider, _)| *provider == "opencode")
                .map(|(_, id)| id.to_string())
        })
        .collect::<Vec<_>>();
    let mut opencode_pool = OpenCodePool::new(&opencode_ids);
    let mut opencode_index = 0usize;
    let mut hits = 0usize;
    let total = sessions_to_scan.len();
    for (index, session) in sessions_to_scan.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            return Ok(());
        }
        let result = if session_parts(session).is_some_and(|(provider, _)| provider == "opencode") {
            let result = opencode_pool.take(opencode_index, Some(cancel));
            opencode_index += 1;
            result
        } else {
            indexed_provider_messages(session, &claude, codex_client.as_mut(), Some(cancel))
        };
        match result {
            Ok(messages) => {
                let text: Vec<String> = messages.into_iter().map(|message| message.text).collect();
                if let Some(excerpt) = transcript_excerpt(&text, query) {
                    publish(json!({ "session": session, "excerpt": excerpt }));
                    hits += 1;
                    if hits >= TRANSCRIPT_RESULT_LIMIT {
                        progress(index + 1, total);
                        return Ok(());
                    }
                }
            }
            Err(error) if !cancel.load(Ordering::Relaxed) => {
                provider_error.get_or_insert(error);
            }
            Err(_) => return Ok(()),
        }
        progress(index + 1, total);
    }
    provider_error.map_or(Ok(()), Err)
}

/// Merge live palette records with saved records by provider and ID.
pub fn merge_sessions(mut live: Vec<Value>, sessions: &[Value]) -> Vec<Value> {
    let mut known = HashSet::new();
    let mut activity = HashMap::<String, i64>::new();
    for session in sessions {
        if let Some(key) = session_key(session) {
            let updated = session
                .get("updatedAt")
                .and_then(Value::as_i64)
                .unwrap_or(0);
            activity
                .entry(key)
                .and_modify(|current| *current = (*current).max(updated))
                .or_insert(updated);
        }
    }
    for item in &mut live {
        let Some(session) = item.get("session") else {
            continue;
        };
        if let Some(key) = session_key(session) {
            known.insert(key.clone());
            let session_activity = session
                .get("updatedAt")
                .and_then(Value::as_i64)
                .filter(|updated| *updated > 0)
                .unwrap_or(0);
            let live_activity = item
                .get("lastActiveAt")
                .and_then(Value::as_i64)
                .filter(|updated| *updated > 0)
                .unwrap_or(0);
            let updated = session_activity
                .max(live_activity)
                .max(activity.get(&key).copied().unwrap_or(0));
            if let Some(object) = item.as_object_mut() {
                object.insert("lastActiveAt".into(), finite_timestamp(updated));
            }
        }
    }
    for session in sessions {
        if let Some(key) = session_key(session) {
            if !known.contains(&key) {
                live.push(saved_session_item(session));
            }
        }
    }
    live
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_root(label: &str) -> PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "herdr-omni-sessions-{label}-{}-{nonce}",
            std::process::id()
        ))
    }

    fn remove_fixture(path: &Path) {
        let _ = fs::remove_dir_all(path);
    }

    #[cfg(unix)]
    fn fake_codex_page_server(path: &Path) {
        fs::write(
            path,
            r##"#!/bin/sh
page=0
while IFS= read -r line; do
  case "$line" in
    *'"method":"initialize"'*) printf '%s\n' '{"id":1,"result":{}}' ;;
    *'"method":"thread/list"'*)
      if [ "$page" -eq 0 ]; then
        printf '%s\n' '{"id":2,"result":{"data":[{"id":"first","source":"cli","name":"first","updatedAt":1}],"nextCursor":"page2"}}'
        page=1
      else
        sleep 0.15
        printf '%s\n' '{"id":3,"result":{"data":[{"id":"second","source":"cli","name":"second","updatedAt":2}],"nextCursor":null}}'
      fi
      ;;
  esac
done
"##,
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(path).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn codex_protocol_publishes_first_page_before_slow_second_page() {
        let root = fixture_root("codex-pages");
        fs::create_dir_all(&root).unwrap();
        let command = root.join("codex");
        fake_codex_page_server(&command);
        let started = Instant::now();
        let mut pages = Vec::new();
        list_codex_appserver_incremental_with_program(command.to_str().unwrap(), None, |page| {
            pages.push(page)
        })
        .unwrap();
        assert_eq!(
            pages
                .iter()
                .flat_map(|page| page.iter())
                .filter_map(|session| session.get("id").and_then(Value::as_str))
                .collect::<Vec<_>>(),
            vec!["first", "second"]
        );
        assert!(started.elapsed() >= Duration::from_millis(120));
        remove_fixture(&root);
    }

    #[test]
    fn codex_line_limit_counts_utf16_instead_of_utf8_bytes() {
        let mut reader = BufReader::new(io::Cursor::new("界😀\nnext\n".as_bytes()));
        assert_eq!(
            read_codex_line(&mut reader, 3).unwrap(),
            Some("界😀".into())
        );
        assert_eq!(
            read_codex_line(&mut reader, 4).unwrap(),
            Some("next".into())
        );
        let mut reader = BufReader::new(io::Cursor::new("界😀\n".as_bytes()));
        assert!(read_codex_line(&mut reader, 2).is_err());
        let mut reader = BufReader::new(io::Cursor::new(b"1234567890\n"));
        assert!(read_codex_line(&mut reader, 3).is_err());
    }

    #[test]
    fn bounded_line_reader_rejects_before_growing_past_limit() {
        let mut reader = BufReader::new(std::io::Cursor::new(b"123456789\n".to_vec()));
        assert!(read_bounded_line(&mut reader, 3).is_err());
        let mut reader = BufReader::new(std::io::Cursor::new(b"ok\r\n".to_vec()));
        assert_eq!(
            read_bounded_line(&mut reader, 3).unwrap(),
            Some("ok".into())
        );
    }

    #[test]
    fn bounded_command_cancellation_reaps_child_after_stdout_eof() {
        let cancel = Arc::new(AtomicBool::new(false));
        let trigger = Arc::clone(&cancel);
        let watcher = thread::spawn(move || {
            thread::sleep(Duration::from_millis(100));
            trigger.store(true, Ordering::Relaxed);
        });
        let started = Instant::now();
        let result =
            run_bounded_command_status("/bin/sh", &["-c", "exec 1>&-; sleep 30"], Some(&cancel));
        watcher.join().unwrap();
        assert!(result.is_err());
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[cfg(unix)]
    #[test]
    fn filesystem_walker_skips_symlink_cycles() {
        let root = fixture_root("symlink-cycle");
        fs::create_dir_all(root.join("projects/a")).unwrap();
        std::os::unix::fs::symlink("..", root.join("projects/a/loop")).unwrap();
        fs::write(root.join("projects/a/session.jsonl"), "{}\n").unwrap();
        let mut paths = Vec::new();
        walk_files_cancel(&root.join("projects"), Some("jsonl"), &mut paths, None);
        assert_eq!(paths.len(), 1);
        remove_fixture(&root);
    }

    #[test]
    fn codex_cursor_seen_set_rejects_non_adjacent_cycles() {
        let mut seen = HashSet::new();
        assert_eq!(
            next_codex_cursor(&Value::String("a".into()), &mut seen).unwrap(),
            Some("a".into())
        );
        assert_eq!(
            next_codex_cursor(&Value::String("b".into()), &mut seen).unwrap(),
            Some("b".into())
        );
        assert!(next_codex_cursor(&Value::String("a".into()), &mut seen).is_err());
    }

    #[test]
    fn provider_discovery_publishes_pages_before_slow_provider_completion() {
        let cancel = AtomicBool::new(false);
        let gate = Arc::new(std::sync::Barrier::new(2));
        let codex_gate = Arc::clone(&gate);
        let mut published = Vec::new();
        let workers: Vec<ProviderWorker> = vec![
            Box::new(move |_, sender| {
                let _ = sender.send(Ok(vec![json!({"provider":"codex", "id":"first"})]));
                codex_gate.wait();
                let _ = sender.send(Ok(vec![json!({"provider":"codex", "id":"second"})]));
            }),
            Box::new(|_, sender| {
                let _ = sender.send(Ok(vec![json!({"provider":"claude", "id":"other"})]));
            }),
        ];
        let mut saw_first = false;
        let mut saw_other = false;
        let mut released = false;
        run_provider_workers(
            &cancel,
            |batch| {
                let session = &batch[0];
                let label = format!(
                    "{}:{}",
                    session["provider"].as_str().unwrap(),
                    session["id"].as_str().unwrap()
                );
                saw_first |= label == "codex:first";
                saw_other |= label == "claude:other";
                published.push(label);
                if saw_first && saw_other && !released {
                    released = true;
                    gate.wait();
                }
            },
            workers,
        )
        .unwrap();
        let first = published
            .iter()
            .position(|label| label == "codex:first")
            .unwrap();
        let other = published
            .iter()
            .position(|label| label == "claude:other")
            .unwrap();
        let second = published
            .iter()
            .position(|label| label == "codex:second")
            .unwrap();
        assert!(first < second && other < second);
    }

    fn write_jsonl(path: &Path, lines: &[Value]) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let body = lines
            .iter()
            .map(|line| serde_json::to_string(line).unwrap())
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(path, format!("{body}\n")).unwrap();
    }

    fn prefilter_fixture_ids(
        provider: &str,
        root: &Path,
        sessions: &[&Value],
        query: &str,
    ) -> Option<HashSet<String>> {
        prefilter_transcript_sessions_with_command(
            "rg",
            provider,
            sessions,
            query,
            &[root.to_path_buf()],
            None,
        )
    }

    #[test]
    fn missing_rg_preserves_full_provider_scan_fallback() {
        let session = json!({"provider":"claude","id":"one"});
        assert!(prefilter_transcript_sessions_with_command(
            "/definitely/missing/ripgrep",
            "claude",
            &[&session],
            "needle",
            &[PathBuf::from("/unused")],
            None
        )
        .is_none());
    }
    #[test]
    fn transcript_prefilter_matches_json_escaped_phrases() {
        if !command_available("rg") {
            eprintln!("Skipping real-rg probe test: ripgrep is not installed; CI installs it.");
            return;
        }
        let root = fixture_root("prefilter-escaped");
        let first = json!({"provider":"claude", "id":"one"});
        let second = json!({"provider":"claude", "id":"two"});
        write_jsonl(
            &root.join("one.jsonl"),
            &[json!({"type":"user", "message":{"content":"quote \\\"value\\\""}})],
        );
        write_jsonl(
            &root.join("two.jsonl"),
            &[json!({"type":"user", "message":{"content":"other"}})],
        );
        let sessions = vec![&first, &second];
        assert_eq!(
            prefilter_fixture_ids("claude", &root, &sessions, "quote \\\"value\\\""),
            Some(HashSet::from(["one".to_string()]))
        );
        remove_fixture(&root);
    }

    #[test]
    fn transcript_prefilter_reports_safe_no_hit_and_unknown_layout_fallback() {
        if !command_available("rg") {
            eprintln!("Skipping real-rg probe test: ripgrep is not installed; CI installs it.");
            return;
        }
        let root = fixture_root("prefilter-no-hit");
        let known = json!({"provider":"claude", "id":"known"});
        write_jsonl(
            &root.join("known.jsonl"),
            &[json!({"type":"user", "message":{"content":"ordinary"}})],
        );
        let sessions = vec![&known];
        assert_eq!(
            prefilter_fixture_ids("claude", &root, &sessions, "missing"),
            Some(HashSet::new())
        );
        remove_fixture(&root);

        let root = fixture_root("prefilter-unknown");
        write_jsonl(
            &root.join("unrelated.jsonl"),
            &[json!({"type":"user", "message":{"content":"needle"}})],
        );
        assert_eq!(
            prefilter_fixture_ids("claude", &root, &sessions, "needle"),
            None
        );
        remove_fixture(&root);
    }

    #[test]
    fn transcript_prefilter_excludes_tool_only_hits_when_conversation_shape_is_known() {
        if !command_available("rg") {
            eprintln!("Skipping real-rg probe test: ripgrep is not installed; CI installs it.");
            return;
        }
        let root = fixture_root("prefilter-tool");
        let session = json!({"provider":"claude", "id":"one"});
        write_jsonl(
            &root.join("one.jsonl"),
            &[
                json!({"type":"user", "message":{"content":"ordinary"}}),
                json!({"type":"tool", "input":"needle"}),
            ],
        );
        assert_eq!(
            prefilter_fixture_ids("claude", &root, &[&session], "needle"),
            Some(HashSet::new())
        );
        remove_fixture(&root);
    }

    #[test]
    fn transcript_prefilter_maps_codex_rollout_names() {
        if !command_available("rg") {
            eprintln!("Skipping real-rg probe test: ripgrep is not installed; CI installs it.");
            return;
        }
        let root = fixture_root("prefilter-codex");
        let session = json!({"provider":"codex", "id":"thread-123"});
        write_jsonl(
            &root.join("rollout-2026-01-01-thread-123.jsonl"),
            &[json!({"type":"message", "text":"needle"})],
        );
        assert_eq!(
            prefilter_transcript_sessions_with_command(
                "rg",
                "codex",
                &[&session],
                "needle",
                std::slice::from_ref(&root),
                None,
            ),
            Some(HashSet::from(["thread-123".to_string()]))
        );
        remove_fixture(&root);
    }

    #[test]
    fn opencode_pool_publishes_early_result_without_reordering_input() {
        let ids = vec!["fast".to_string(), "slow".to_string()];
        let reader: OpenCodeReader = Arc::new(|id, cancel| {
            if id == "slow" {
                for _ in 0..10 {
                    if cancel.load(Ordering::Relaxed) {
                        return Err("cancelled".into());
                    }
                    thread::sleep(Duration::from_millis(20));
                }
            }
            Ok(vec![PreviewMessage {
                role: "assistant".into(),
                text: id.into(),
            }])
        });
        let started = Instant::now();
        let mut pool = OpenCodePool::new_with_reader(&ids, reader);
        assert_eq!(pool.take(0, None).unwrap()[0].text, "fast");
        assert!(started.elapsed() < Duration::from_millis(150));
        assert_eq!(pool.take(1, None).unwrap()[0].text, "slow");
    }

    #[test]
    fn opencode_pool_cancellation_stops_slow_reader() {
        let ids = vec!["slow".to_string()];
        let reader: OpenCodeReader = Arc::new(|_, cancel| {
            while !cancel.load(Ordering::Relaxed) {
                thread::sleep(Duration::from_millis(10));
            }
            Err("cancelled".into())
        });
        let external = AtomicBool::new(false);
        let mut pool = OpenCodePool::new_with_reader(&ids, reader);
        external.store(true, Ordering::Relaxed);
        assert!(pool.take(0, Some(&external)).is_err());
    }

    #[test]
    fn opencode_pool_buffers_out_of_order_completion() {
        let ids = vec!["slow".to_string(), "fast".to_string()];
        let reader: OpenCodeReader = Arc::new(|id, _| {
            if id == "slow" {
                thread::sleep(Duration::from_millis(50));
            }
            Ok(vec![PreviewMessage {
                role: "assistant".into(),
                text: id.into(),
            }])
        });
        let mut pool = OpenCodePool::new_with_reader(&ids, reader);
        assert_eq!(pool.take(0, None).unwrap()[0].text, "slow");
        assert_eq!(pool.take(1, None).unwrap()[0].text, "fast");
    }

    #[test]
    fn opencode_pool_drop_disconnects_full_result_queue() {
        let ids = (0..32)
            .map(|index| format!("id-{index}"))
            .collect::<Vec<_>>();
        let active = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let peak = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let reader_active = Arc::clone(&active);
        let reader_peak = Arc::clone(&peak);
        let reader: OpenCodeReader = Arc::new(move |id, _| {
            let current = reader_active.fetch_add(1, Ordering::Relaxed) + 1;
            reader_peak.fetch_max(current, Ordering::Relaxed);
            thread::sleep(Duration::from_millis(5));
            reader_active.fetch_sub(1, Ordering::Relaxed);
            Ok(vec![PreviewMessage {
                role: "assistant".into(),
                text: id.into(),
            }])
        });
        let started = Instant::now();
        let pool = OpenCodePool::new_with_reader(&ids, reader);
        thread::sleep(Duration::from_millis(50));
        drop(pool);
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(peak.load(Ordering::Relaxed) <= OPENCODE_READ_CONCURRENCY);
    }

    #[test]
    fn transcript_matching_is_literal_and_contiguous() {
        assert!(transcript_excerpt(
            &["Payment CALLBACK timeout".into()],
            "payment callback timeout"
        )
        .is_some());
        assert!(transcript_excerpt(
            &["callback eventually hit timeout".into()],
            "callback timeout"
        )
        .is_none());
        assert!(transcript_excerpt(&["say foo.bar()".into()], "foo.bar()").is_some());
        assert!(transcript_excerpt(&["Please FIX IT NOW, thanks".into()], "fix it now").is_some());
        assert!(transcript_excerpt(&["say \"fix it now\"".into()], "\"fix it now\"").is_some());
        assert!(transcript_excerpt(&["say fix it now".into()], "\"fix it now\"").is_none());
        assert!(
            transcript_excerpt(&["payment".into(), "timeout".into()], "payment timeout").is_none()
        );
        assert!(transcript_excerpt(&["我们处理支付回调失败".into()], "支付回调").is_some());
        assert!(transcript_excerpt(&["hello\u{1b} world".into()], "world")
            .is_some_and(|excerpt| !excerpt.contains('\u{1b}')));
    }

    #[test]
    fn provider_extractors_drop_tools_and_reasoning() {
        let lines = vec![
            json!({"type":"user", "uuid":"u1", "message":{"content":"question"}}),
            json!({"type":"assistant", "uuid":"a1", "parentUuid":"u1", "message":{"content":[{"type":"text","text":"answer"},{"type":"thinking","thinking":"private"}]}}),
            json!({"type":"user", "uuid":"u2", "parentUuid":"a1", "message":{"content":[{"type":"tool_result","content":"secret"}]}}),
        ];
        assert_eq!(
            claude_messages(&lines),
            vec![
                PreviewMessage {
                    role: "user".into(),
                    text: "question".into()
                },
                PreviewMessage {
                    role: "assistant".into(),
                    text: "answer".into()
                },
            ]
        );
    }

    #[test]
    fn claude_messages_follow_active_branch_and_filter_meta_entries() {
        let lines = vec![
            json!({"type":"user","uuid":"u1","message":{"content":"root"}}),
            json!({"type":"assistant","uuid":"a1","parentUuid":"u1","message":{"content":"answer"}}),
            json!({"type":"user","uuid":"meta","parentUuid":"a1","isMeta":true,"message":{"content":"hidden meta"}}),
            json!({"type":"user","uuid":"side","parentUuid":"a1","isSidechain":true,"message":{"content":"hidden sidechain"}}),
            json!({"type":"user","uuid":"active","parentUuid":"a1","message":{"content":"active branch"}}),
        ];
        let messages = claude_messages(&lines);
        assert_eq!(
            messages
                .iter()
                .map(|message| message.text.as_str())
                .collect::<Vec<_>>(),
            vec!["root", "answer", "active branch"]
        );
    }

    #[test]
    fn codex_app_server_thread_shape_keeps_only_conversation_items() {
        let thread = json!({"turns":[{"items":[
            {"type":"userMessage","content":[{"type":"text","text":"question"}]},
            {"type":"agentMessage","text":"answer"},
            {"type":"reasoning","text":"private"},
            {"type":"commandExecution","aggregatedOutput":"tool output"}
        ]}]});
        assert_eq!(
            codex_thread_messages(&thread),
            vec![
                PreviewMessage {
                    role: "user".into(),
                    text: "question".into()
                },
                PreviewMessage {
                    role: "assistant".into(),
                    text: "answer".into()
                },
            ]
        );
    }

    #[test]
    fn opencode_export_future_shape_excludes_synthetic_and_tools() {
        let export = json!({"info":{"id":"ses_test123"},"messages":[
            {"info":{"role":"user"},"parts":[{"type":"text","text":"question"},{"type":"text","text":"hidden","synthetic":true}]},
            {"info":{"role":"assistant"},"parts":[{"type":"text","text":"answer"},{"type":"tool","output":"secret"}]}
        ]});
        let messages = opencode_export_messages(&export, "ses_test123").unwrap();
        assert_eq!(
            messages
                .iter()
                .map(|message| message.text.as_str())
                .collect::<Vec<_>>(),
            vec!["question", "answer"]
        );
        assert!(opencode_export_messages(&json!({"messages": []}), "ses_test123").is_err());
        assert!(opencode_export_messages(
            &json!({"info":{"id":"ses_other"},"messages": []}),
            "ses_test123"
        )
        .is_err());
        assert!(valid_opencode_id("ses_a"));
        assert!(!valid_opencode_id(&format!("ses_{}", "x".repeat(197))));
    }

    #[test]
    fn preview_keeps_latest_two_and_bounds_long_latest_message() {
        let messages = vec![
            PreviewMessage {
                role: "user".into(),
                text: "old".into(),
            },
            PreviewMessage {
                role: "assistant".into(),
                text: "answer".into(),
            },
            PreviewMessage {
                role: "user".into(),
                text: "latest".into(),
            },
        ];
        assert_eq!(
            bounded_preview(&messages, 100),
            "assistant: answer\n\nuser: latest"
        );
        assert!(
            bounded_preview(
                &[PreviewMessage {
                    role: "assistant".into(),
                    text: "x".repeat(100)
                }],
                12
            )
            .chars()
            .count()
                <= 12
        );
    }

    #[test]
    fn merge_deduplicates_by_provider_and_id() {
        let live =
            vec![json!({"id":"live", "session":{"provider":"codex", "id":"one", "updatedAt":4}})];
        let saved = vec![
            json!({"provider":"codex", "id":"one", "title":"same", "updatedAt":8}),
            json!({"provider":"claude", "id":"one"}),
        ];
        let merged = merge_sessions(live, &saved);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0]["lastActiveAt"], 8);
        assert_eq!(merged[1]["session"]["provider"], "claude");
    }

    #[test]
    fn saved_shape_and_merge_activity_match_palette_oracle() {
        let saved = json!({
            "provider":"claude", "id":"same", "title":"  Checkout\nflow ",
            "cwd":"/repo/shop/", "updatedAt":0
        });
        let item = saved_session_item(&saved);
        assert_eq!(item["id"], "saved:agent:claude:same");
        assert_eq!(item["title"], "Checkout flow - shop");
        assert_eq!(item["aliases"], json!(["claude"]));
        assert_eq!(item["category"], "Agents");

        let live = vec![json!({
            "id":"live", "session":{"provider":"claude", "id":"same", "updatedAt":0}
        })];
        let merged = merge_sessions(live, std::slice::from_ref(&saved));
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0]["lastActiveAt"], 0);

        let raw_live = vec![json!({"provider":"claude", "id":"same"})];
        assert_eq!(
            merge_sessions(raw_live, &[json!({"provider":"claude", "id":"same"})]).len(),
            2
        );
        let duplicates = merge_sessions(Vec::new(), &[saved.clone(), saved]);
        assert_eq!(duplicates.len(), 2);
    }

    #[test]
    fn generated_typescript_behavior_oracle_matches_native_shapes() {
        let fixture: Value = serde_json::from_str(include_str!(
            "../tests/fixtures/session-behavior-parity.json"
        ))
        .unwrap();
        for case in fixture["saved"].as_array().unwrap() {
            assert_eq!(
                saved_session_item(&case["input"]),
                case["output"],
                "saved session oracle case"
            );
        }
        for case in fixture["merge"].as_array().unwrap() {
            let input = &case["input"];
            let live = input["live"].as_array().unwrap().to_vec();
            let sessions = input["sessions"].as_array().unwrap().to_vec();
            assert_eq!(
                Value::Array(merge_sessions(live, &sessions)),
                case["output"]
            );
        }
        for case in fixture["terms"].as_array().unwrap() {
            let output = transcript_terms(case["query"].as_str().unwrap()).map_or_else(
                || Value::Array(Vec::new()),
                |term| Value::Array(vec![Value::String(term)]),
            );
            assert_eq!(output, case["output"]);
        }
        for case in fixture["excerpts"].as_array().unwrap() {
            let messages = case["input"]["messages"]
                .as_array()
                .unwrap()
                .iter()
                .map(|message| message.as_str().unwrap().to_string())
                .collect::<Vec<_>>();
            let output = transcript_excerpt(&messages, case["input"]["query"].as_str().unwrap())
                .map_or(Value::Null, Value::String);
            assert_eq!(output, case["output"]);
        }
    }

    #[test]
    fn cancellable_search_stops_before_callbacks() {
        let cancel = AtomicBool::new(true);
        let mut published = 0;
        let mut progress = 0;
        search_cancellable(
            &[json!({"provider":"codex", "id":"one"})],
            "needle",
            &cancel,
            |_| published += 1,
            |_, _| progress += 1,
        )
        .unwrap();
        assert_eq!((published, progress), (0, 0));
    }

    #[test]
    fn claude_listing_uses_later_summary_and_hides_sidechains_and_tool_only_files() {
        let root = fixture_root("claude");
        let project = root.join("projects/repo");
        fs::create_dir_all(&project).unwrap();
        fs::write(
            project.join("normal.jsonl"),
            r#"{"type":"user","uuid":"normal-u","sessionId":"normal-id","cwd":"/repo","message":{"content":"first prompt"}}
{"type":"summary","summary":"A useful summary"}
{"type":"customTitle","customTitle":"Checkout investigation"}
{"type":"assistant","uuid":"normal-a","parentUuid":"normal-u","message":{"content":[{"type":"text","text":"answer"},{"type":"tool_use","input":{"secret":"omit"}}]}}
"#,
        )
        .unwrap();
        fs::write(
            project.join("sidechain.jsonl"),
            r#"{"type":"user","uuid":"side-u","sessionId":"side-id","isSidechain":true,"message":{"content":"child"}}
"#,
        )
        .unwrap();
        fs::write(
            project.join("tools-only.jsonl"),
            r#"{"type":"user","uuid":"tools-u","sessionId":"tools-id","message":{"content":[{"type":"tool_result","content":"omit"}]}}
"#,
        )
        .unwrap();
        let listed = list_claude_root(&root);
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0]["id"], "normal-id");
        assert_eq!(listed[0]["title"], "Checkout investigation");
        assert_eq!(listed[0]["cwd"], "/repo");
        remove_fixture(&root);
    }

    #[test]
    fn opencode_global_pages_use_basic_auth_and_chunked_pagination() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let password = "test-password";
        let expected_auth = format!("Basic {}", basic_auth("omni", password));
        let server = std::thread::spawn(move || {
            for (index, incoming) in listener.incoming().take(2).enumerate() {
                let mut stream = incoming.unwrap();
                let mut request = [0u8; 4096];
                let size = stream.read(&mut request).unwrap();
                let request = String::from_utf8_lossy(&request[..size]);
                assert!(request.contains(&format!("Authorization: {expected_auth}")));
                if index == 0 {
                    assert!(request.contains("limit=1000"));
                    assert!(!request.contains("cursor="));
                } else {
                    assert!(request.contains("cursor=next%20page"));
                }
                let (body, cursor) = if index == 0 {
                    (
                        r#"[{"id":"ses_first","title":"First","directory":"/one","time":{"updated":1000}}]"#,
                        Some("next page"),
                    )
                } else {
                    (
                        r#"[{"id":"ses_second","title":"Second","directory":"/two","time":{"updated":2000}}]"#,
                        None,
                    )
                };
                let encoded = format!("{:X}\r\n{}\r\n0\r\n\r\n", body.len(), body);
                let header = cursor
                    .map(|cursor| format!("X-Next-Cursor: {cursor}\r\n"))
                    .unwrap_or_default();
                let response = format!(
                    "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n{header}Connection: close\r\n\r\n{encoded}"
                );
                stream.write_all(response.as_bytes()).unwrap();
            }
        });
        let rows = fetch_opencode_pages(port, password).unwrap();
        server.join().unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["id"], "ses_first");
        assert_eq!(rows[1]["id"], "ses_second");
    }
}
