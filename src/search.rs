/* Copyright (c) Microsoft Corporation. All rights reserved.
 * Licensed under the MIT License; see src/vendor/vscode/LICENSE.txt.
 * The fuzzy scorer below is a Rust port of the vendored VS Code scorer.
 */

//! Palette filtering and VS Code style fuzzy ranking.
//!
//! Palette items intentionally stay as `serde_json::Value`: the socket protocol has a
//! deliberately loose, camelCase item shape and keeping it opaque here avoids a second
//! schema which can get out of sync with the Herdr protocol.

use serde_json::Value;
use std::cmp::Ordering;

const CATEGORY_ORDER: [&str; 8] = [
    "Workspace",
    "Tabs",
    "Agents",
    "Actions",
    "Panes",
    "Herdr",
    "Custom",
    "Worktrees",
];

fn str_field<'a>(item: &'a Value, key: &str) -> &'a str {
    item.get(key).and_then(Value::as_str).unwrap_or("")
}

fn category(item: &Value) -> &str {
    str_field(item, "category")
}
fn section(item: &Value) -> &str {
    if category(item) == "Worktrees" {
        "Workspace"
    } else {
        category(item)
    }
}
fn is_destination(item: &Value) -> bool {
    matches!(category(item), "Workspace" | "Worktrees")
}
fn is_current(item: &Value) -> bool {
    is_destination(item)
        && item
            .get("currentWorkspace")
            .and_then(Value::as_bool)
            .unwrap_or(false)
}
fn finite_number(value: Option<&Value>) -> f64 {
    value
        .and_then(Value::as_f64)
        .filter(|v| v.is_finite())
        .unwrap_or(0.0)
}
fn last_visited(item: &Value) -> f64 {
    finite_number(item.get("lastVisitedAt")).max(0.0)
}

/// Return a score and the UTF-16 offsets selected by the optimal path.
/// This is the small dynamic-programming scorer used by VS Code's Ctrl+P.
fn score_fuzzy(target: &str, query: &str, allow_non_contiguous: bool) -> (i32, Vec<usize>) {
    let target_units: Vec<u16> = target.encode_utf16().collect();
    let query_units: Vec<u16> = query.encode_utf16().collect();
    let target_lower: Vec<u16> = target.to_lowercase().encode_utf16().collect();
    let query_lower: Vec<u16> = query.to_lowercase().encode_utf16().collect();
    let target_len = target_units.len();
    let query_len = query_units.len();
    if target_len == 0 || query_len == 0 || target_len < query_len {
        return (0, Vec::new());
    }
    let mut scores = vec![0i32; query_len * target_len];
    let mut matches = vec![0i32; query_len * target_len];
    for qi in 0..query_len {
        for ti in 0..target_len {
            let current = qi * target_len + ti;
            let left = if ti > 0 { scores[current - 1] } else { 0 };
            let diag = if qi > 0 && ti > 0 {
                scores[(qi - 1) * target_len + ti - 1]
            } else {
                0
            };
            let sequence = if qi > 0 && ti > 0 {
                matches[(qi - 1) * target_len + ti - 1]
            } else {
                0
            };
            let score = if qi > 0 && diag == 0 {
                0
            } else {
                char_score(
                    query_units[qi],
                    query_lower[qi],
                    &target_units,
                    &target_lower,
                    ti,
                    sequence,
                )
            };
            let contiguous = qi > 0 || starts_with_lower(&target_lower, &query_lower, ti);
            if score > 0 && diag + score >= left && (allow_non_contiguous || contiguous) {
                matches[current] = sequence + 1;
                scores[current] = diag + score;
            } else {
                matches[current] = 0;
                scores[current] = left;
            }
        }
    }
    let mut positions = Vec::new();
    let (mut qi, mut ti) = (query_len as isize - 1, target_len as isize - 1);
    while qi >= 0 && ti >= 0 {
        let current = qi as usize * target_len + ti as usize;
        if matches[current] == 0 {
            ti -= 1;
        } else {
            positions.push(ti as usize);
            qi -= 1;
            ti -= 1;
        }
    }
    positions.reverse();
    (scores[query_len * target_len - 1], positions)
}

fn starts_with_lower(target: &[u16], query: &[u16], index: usize) -> bool {
    index + query.len() <= target.len() && target[index..index + query.len()] == *query
}

fn char_score(
    query: u16,
    query_lower: u16,
    target: &[u16],
    target_lower: &[u16],
    index: usize,
    sequence: i32,
) -> i32 {
    let target_char = target_lower[index];
    if !(query_lower == target_char
        || ((query_lower == '/' as u16 || query_lower == '\\' as u16)
            && (target_char == '/' as u16 || target_char == '\\' as u16)))
    {
        return 0;
    }
    let mut score = 1;
    if sequence > 0 {
        score += 6 * sequence.min(3) + 3 * (sequence - 3).max(0);
    }
    if query == target[index] {
        score += 1;
    }
    if index == 0 {
        score += 8;
    } else {
        score += match target[index - 1] {
            47 | 92 => 5,
            95 | 45 | 46 | 32 | 39 | 34 | 58 => 4,
            _ if is_upper(target[index]) && sequence == 0 => 2,
            _ => 0,
        };
    }
    score
}

fn is_upper(unit: u16) -> bool {
    (b'A' as u16..=b'Z' as u16).contains(&unit)
}

fn utf16_to_codepoints(value: &str, offsets: &[usize]) -> Vec<usize> {
    let mut map = Vec::with_capacity(value.encode_utf16().count());
    for (index, character) in value.chars().enumerate() {
        map.extend(std::iter::repeat_n(index, character.len_utf16()));
    }
    offsets
        .iter()
        .filter_map(|offset| map.get(*offset).copied())
        .collect()
}

pub fn fuzzy_score(query: &str, value: &str) -> f64 {
    if query.is_empty() {
        return 0.0;
    }
    let (score, _) = score_fuzzy(value, query, true);
    if score == 0 {
        f64::NEG_INFINITY
    } else {
        score as f64
            + if value.to_lowercase() == query.to_lowercase() {
                100.0
            } else {
                0.0
            }
    }
}

/// Return title character positions selected by the scorer. Useful to render highlighted labels.
pub fn matching_positions(query: &str, title: &str) -> Vec<usize> {
    let query = if query.starts_with(['>', ':', '@']) {
        &query[1..]
    } else {
        query
    };
    let mut out = Vec::new();
    for token in query.split_whitespace() {
        let (score, positions) = score_fuzzy(title, token, true);
        if score > 0 {
            out.extend(utf16_to_codepoints(title, &positions));
        }
    }
    out.sort_unstable();
    out.dedup();
    out
}

fn search_fields(item: &Value, token: &str) -> f64 {
    let lower = token.to_lowercase();
    // TypeScript uses nullish coalescing here: an explicitly empty searchTitle
    // intentionally hides the display title from keyword search.
    let title = item
        .get("searchTitle")
        .and_then(Value::as_str)
        .unwrap_or_else(|| str_field(item, "title"));
    let mut best = fuzzy_score(token, title);
    if let Some(id) = item
        .get("session")
        .and_then(|s| s.get("id"))
        .and_then(Value::as_str)
    {
        if id.to_lowercase().contains(&lower) {
            best = best.max(1000.0);
        }
    }
    if let Some(aliases) = item.get("aliases").and_then(Value::as_array) {
        for alias in aliases.iter().filter_map(Value::as_str) {
            best = best.max(fuzzy_score(token, alias) - 25.0);
        }
    }
    if let Some(paths) = item.get("searchPaths").and_then(Value::as_array) {
        for path in paths
            .iter()
            .filter_map(Value::as_str)
            .filter(|p| p.to_lowercase().contains(&lower))
        {
            best = best.max(fuzzy_score(token, path) - 25.0);
        }
    }
    best
}

fn history_value(history: &Value, item: &Value) -> f64 {
    let id = str_field(item, "id");
    let key = if id.starts_with("live:") {
        format!("live@{}:{}", socket_scope(), id)
    } else {
        id.to_string()
    };
    finite_number(history.get(&key))
}

fn socket_scope() -> String {
    let socket = std::env::var("HERDR_SOCKET_PATH").unwrap_or_else(|_| "default".into());
    socket
        .bytes()
        .flat_map(|b| {
            if b.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&b) {
                vec![b as char]
            } else {
                format!("%{b:02X}").chars().collect()
            }
        })
        .collect()
}

fn category_order(value: &str) -> usize {
    CATEGORY_ORDER
        .iter()
        .position(|x| *x == value)
        .unwrap_or(CATEGORY_ORDER.len())
}
fn requested_category(item: &Value, category_filter: &str) -> bool {
    match category_filter {
        "" | "All" => true,
        "Workspace" => matches!(category(item), "Workspace" | "Worktrees"),
        other => category(item) == other,
    }
}

#[derive(Clone)]
struct Match {
    item: Value,
    index: usize,
    score: f64,
    recent: f64,
}

/// Search items using `category` (All, Workspace, Agents, Tabs, Actions) and the `>`, `@`, `:` scopes.
pub fn search(items: &[Value], query: &str, category_filter: &str, history: &Value) -> Vec<Value> {
    let agent_scope = query.starts_with('>');
    let action_scope = query.starts_with(':');
    let workspace_scope = query.starts_with('@');
    let body = if agent_scope || action_scope || workspace_scope {
        &query[1..]
    } else {
        query
    };
    let tokens: Vec<&str> = body.split_whitespace().collect();
    let browsing = tokens.is_empty();
    let mut found = Vec::<Match>::new();
    for (index, item) in items.iter().enumerate() {
        if !requested_category(item, category_filter) {
            continue;
        }
        if agent_scope
            && !str_field(item, "id").starts_with("live:agent:")
            && item.get("savedSession").and_then(Value::as_bool) != Some(true)
        {
            continue;
        }
        if action_scope && category(item) != "Actions" {
            continue;
        }
        if workspace_scope && !is_destination(item) {
            continue;
        }
        let mut score = 0.0;
        let mut matched = true;
        for token in &tokens {
            let value = search_fields(item, token);
            if !value.is_finite() {
                matched = false;
                break;
            }
            score += value;
        }
        if !matched {
            continue;
        }
        let recent = if is_destination(item) || category(item) == "Actions" {
            0.0
        } else {
            history_value(history, item)
        };
        found.push(Match {
            item: item.clone(),
            index,
            score,
            recent,
        });
    }
    found.sort_by(|a, b| compare_matches(a, b, browsing));
    // Matching rows are grouped into sections, with the best matching section first.
    let mut groups: Vec<(String, Vec<Match>)> = Vec::new();
    for row in found {
        let name = section(&row.item).to_string();
        if let Some((_, group)) = groups.iter_mut().find(|(s, _)| *s == name) {
            group.push(row);
        } else {
            groups.push((name, vec![row]));
        }
    }
    groups.sort_by(|a, b| {
        b.1[0]
            .score
            .partial_cmp(&a.1[0].score)
            .unwrap_or(Ordering::Equal)
            .then_with(|| category_order(&a.0).cmp(&category_order(&b.0)))
    });
    groups
        .into_iter()
        .flat_map(|(_, rows)| rows.into_iter().map(|row| row.item))
        .collect()
}

fn compare_matches(a: &Match, b: &Match, browsing: bool) -> Ordering {
    b.score
        .partial_cmp(&a.score)
        .unwrap_or(Ordering::Equal)
        .then_with(|| {
            let sa = section(&a.item);
            let sb = section(&b.item);
            if sa != sb {
                return category_order(sa).cmp(&category_order(sb));
            }
            if category(&a.item) == "Agents" && category(&b.item) == "Agents" {
                return finite_number(b.item.get("lastActiveAt"))
                    .max(finite_number(
                        b.item.get("session").and_then(|s| s.get("updatedAt")),
                    ))
                    .partial_cmp(
                        &finite_number(a.item.get("lastActiveAt")).max(finite_number(
                            a.item.get("session").and_then(|s| s.get("updatedAt")),
                        )),
                    )
                    .unwrap_or(Ordering::Equal);
            }
            if browsing
                && sa == sb
                && matches!(category(&a.item), "Workspace" | "Worktrees" | "Tabs")
            {
                if is_destination(&a.item)
                    && is_destination(&b.item)
                    && is_current(&a.item) != is_current(&b.item)
                {
                    return is_current(&a.item).cmp(&is_current(&b.item));
                }
                let visited = last_visited(&b.item)
                    .partial_cmp(&last_visited(&a.item))
                    .unwrap_or(Ordering::Equal);
                if visited != Ordering::Equal {
                    return visited;
                }
                if category(&a.item) == "Tabs" && category(&b.item) == "Tabs" {
                    return b.recent.partial_cmp(&a.recent).unwrap_or(Ordering::Equal);
                }
            }
            if !browsing && sa == sb && sa != "Agents" {
                if is_destination(&a.item) && is_destination(&b.item) {
                    return last_visited(&b.item)
                        .partial_cmp(&last_visited(&a.item))
                        .unwrap_or(Ordering::Equal);
                }
                return b.recent.partial_cmp(&a.recent).unwrap_or(Ordering::Equal);
            }
            a.index.cmp(&b.index)
        })
}
