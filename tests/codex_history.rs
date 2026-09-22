use herdr_omni::sessions::search_cancellable;
use serde_json::{json, Value};
use std::env;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

fn env_lock() -> std::sync::MutexGuard<'static, ()> {
    ENV_LOCK.get_or_init(|| Mutex::new(())).lock().unwrap()
}

fn temp_path(label: &str) -> PathBuf {
    let suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    env::temp_dir().join(format!(
        "herdr-omni-codex-history-{label}-{}-{suffix}",
        std::process::id()
    ))
}

fn executable(path: &Path, body: &str) {
    fs::write(path, body).unwrap();
    let mut permissions = fs::metadata(path).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).unwrap();
}

fn response(id: u64, session_id: &str, output: Option<String>, needle: &str) -> String {
    let mut items = Vec::new();
    if let Some(output) = output {
        items.push(json!({
            "type": "commandExecution",
            "command": "cat large-output",
            "aggregatedOutput": output
        }));
    }
    items.push(json!({
        "type": "userMessage",
        "content": [{"type": "text", "text": format!("{needle} user") }]
    }));
    items.push(json!({
        "type": "agentMessage",
        "text": format!("{needle} assistant")
    }));
    serde_json::to_string(&json!({
        "id": id,
        "error": Value::Null,
        "result": {
            "thread": {
                "id": session_id,
                "turns": [{"items": items}]
            }
        }
    }))
    .unwrap()
}

#[test]
fn codex_history_ignores_large_tool_output_and_keeps_conversation_hits() {
    let _guard = env_lock();
    let root = temp_path("large-tool-output");
    let codex_home = root.join(".codex");
    let sessions_root = codex_home.join("sessions");
    let fake_bin = root.join("bin");
    fs::create_dir_all(&sessions_root).unwrap();
    fs::create_dir_all(&fake_bin).unwrap();

    let first_id = "thread-first";
    let second_id = "thread-second";
    fs::write(
        sessions_root.join("rollout-2026-01-01-thread-first.jsonl"),
        "{\"type\":\"message\",\"role\":\"user\",\"content\":\"needle first\"}\n",
    )
    .unwrap();
    fs::write(
        sessions_root.join("rollout-2026-01-02-thread-second.jsonl"),
        "{\"type\":\"message\",\"role\":\"user\",\"content\":\"needle second\"}\n",
    )
    .unwrap();

    let first_response = root.join("first-response.json");
    let second_response = root.join("second-response.json");
    fs::write(
        &first_response,
        response(
            2,
            first_id,
            Some("x".repeat(17 * 1024 * 1024)),
            "needle first",
        ),
    )
    .unwrap();
    fs::write(
        &second_response,
        response(3, second_id, None, "needle second"),
    )
    .unwrap();

    let codex = fake_bin.join("codex");
    executable(
        &codex,
        r##"#!/bin/sh
while IFS= read -r line; do
  case "$line" in
    *'"method":"initialize"'*) printf '%s\n' '{"id":1,"error":null,"result":{}}' ;;
    *'"threadId":"thread-first"'*) cat "$FIRST_RESPONSE"; printf '\n' ;;
    *'"threadId":"thread-second"'*) cat "$SECOND_RESPONSE"; printf '\n' ;;
  esac
done
"##,
    );

    let sessions = vec![
        json!({"provider":"codex","id":first_id,"title":"First","cwd":"/repo"}),
        json!({"provider":"codex","id":second_id,"title":"Second","cwd":"/repo"}),
    ];
    let original_home = env::var_os("HOME");
    let original_codex_home = env::var_os("CODEX_HOME");
    let original_claude_config = env::var_os("CLAUDE_CONFIG_DIR");
    let original_path = env::var_os("PATH");
    env::set_var("HOME", &root);
    env::set_var("CODEX_HOME", &codex_home);
    env::set_var("CLAUDE_CONFIG_DIR", root.join(".claude"));
    let path = format!(
        "{}:{}",
        fake_bin.display(),
        original_path
            .as_deref()
            .unwrap_or_else(|| Path::new("/usr/bin").as_os_str())
            .to_string_lossy()
    );
    env::set_var("PATH", path);
    env::set_var("FIRST_RESPONSE", &first_response);
    env::set_var("SECOND_RESPONSE", &second_response);

    let cancel = AtomicBool::new(false);
    let mut hits = Vec::new();
    let mut progress = Vec::new();
    let result = search_cancellable(
        &sessions,
        "needle",
        &cancel,
        |hit| hits.push(hit),
        |done, total| progress.push((done, total)),
    );

    match original_home {
        Some(value) => env::set_var("HOME", value),
        None => env::remove_var("HOME"),
    }
    match original_codex_home {
        Some(value) => env::set_var("CODEX_HOME", value),
        None => env::remove_var("CODEX_HOME"),
    }
    match original_claude_config {
        Some(value) => env::set_var("CLAUDE_CONFIG_DIR", value),
        None => env::remove_var("CLAUDE_CONFIG_DIR"),
    }
    match original_path {
        Some(value) => env::set_var("PATH", value),
        None => env::remove_var("PATH"),
    }
    env::remove_var("FIRST_RESPONSE");
    env::remove_var("SECOND_RESPONSE");
    let _ = fs::remove_dir_all(&root);

    result.unwrap();
    assert_eq!(
        hits.iter()
            .map(|hit| hit["session"]["id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec![first_id, second_id]
    );
    for (hit, label) in hits.iter().zip(["first", "second"]) {
        let excerpt = hit["excerpt"].as_str().unwrap();
        assert!(excerpt.contains(&format!("needle {label} user")));
        assert!(excerpt.contains(&format!("needle {label} assistant")));
        assert!(
            !excerpt.contains("xxxxxxxx"),
            "tool output leaked into excerpt"
        );
    }
    assert_eq!(progress.last().copied(), Some((2, 2)));
}
