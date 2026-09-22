use herdr_omni::{catalog, config, search, update};
use serde_json::{json, Value};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

static ENV_LOCK: Mutex<()> = Mutex::new(());

fn temp_dir(label: &str) -> PathBuf {
    let suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "herdr-omni-{label}-{}-{suffix}",
        std::process::id()
    ));
    fs::create_dir_all(&path).unwrap();
    path
}

fn set_env(key: &str, value: Option<&Path>) -> Option<std::ffi::OsString> {
    let old = std::env::var_os(key);
    match value {
        Some(value) => std::env::set_var(key, value),
        None => std::env::remove_var(key),
    }
    old
}

fn restore_env(key: &str, old: Option<std::ffi::OsString>) {
    match old {
        Some(value) => std::env::set_var(key, value),
        None => std::env::remove_var(key),
    }
}

#[test]
fn catalog_preserves_the_action_contract() {
    let items = catalog::default_items();
    assert_eq!(items.len(), 36);
    assert!(items.iter().all(|item| item["category"] == "Actions"));
    for id in [
        "new_workspace",
        "new_tab",
        "split_vertical",
        "new_worktree",
        "edit_scrollback",
    ] {
        assert!(
            items.iter().any(|item| item["id"] == id),
            "missing catalog action {id}"
        );
    }
    assert_eq!(
        items
            .iter()
            .find(|item| item["id"] == "edit_scrollback")
            .unwrap()["invocation"],
        json!({"kind":"pane-api","method":"pane.edit_scrollback"})
    );
}

#[test]
fn config_applies_nested_key_remaps_and_empty_bindings() {
    let _guard = ENV_LOCK.lock().unwrap();
    let root = temp_dir("config-remaps");
    let config_path = root.join("config.toml");
    fs::write(
        &config_path,
        "[keys]\nnew_workspace = \"ctrl+shift+n\"\nnext_tab = \"\"\n",
    )
    .unwrap();
    let old = set_env("HERDR_CONFIG_PATH", Some(&config_path));
    let items = config::load_items();
    restore_env("HERDR_CONFIG_PATH", old);
    fs::remove_dir_all(root).unwrap();
    let new_workspace = items
        .iter()
        .find(|item| item["id"] == "new_workspace")
        .unwrap();
    let next_tab = items.iter().find(|item| item["id"] == "next_tab").unwrap();
    assert_eq!(new_workspace["shortcuts"], json!(["ctrl+shift+n"]));
    assert_eq!(next_tab["shortcuts"], json!([]));
}

#[test]
fn theme_resolves_aliases_auto_switch_and_custom_tokens() {
    let _guard = ENV_LOCK.lock().unwrap();
    let root = temp_dir("theme");
    let config_path = root.join("config.toml");
    fs::write(&config_path, "[theme]\nname = \"Latte\"\nauto_switch = true\ndark_name = \"nord\"\nlight_name = \"catppuccin-latte\"\n[theme.custom]\naccent = \"#ABC\"\n[theme.custom.dark]\naccent = \"#123456\"\n").unwrap();
    let old_path = set_env("HERDR_CONFIG_PATH", Some(&config_path));
    let old_style = std::env::var_os("AppleInterfaceStyle");
    std::env::set_var("AppleInterfaceStyle", "light");
    let light = config::load_theme();
    std::env::set_var("AppleInterfaceStyle", "dark");
    let dark = config::load_theme();
    restore_env("AppleInterfaceStyle", old_style);
    restore_env("HERDR_CONFIG_PATH", old_path);
    fs::remove_dir_all(root).unwrap();
    let oracle: Value = serde_json::from_str(include_str!("fixtures/theme-parity.json")).unwrap();
    for key in ["accent", "background", "panel", "text"] {
        assert_eq!(
            light[key], oracle[0]["light"][key],
            "light theme token {key}"
        );
        assert_eq!(dark[key], oracle[0]["dark"][key], "dark theme token {key}");
    }
}

#[test]
fn history_preserves_other_socket_entries_when_recording() {
    let _guard = ENV_LOCK.lock().unwrap();
    let root = temp_dir("history");
    let old_state = set_env("XDG_STATE_HOME", Some(&root));
    let old_socket = std::env::var_os("HERDR_SOCKET_PATH");
    std::env::set_var("HERDR_SOCKET_PATH", "/tmp/herdr-a.sock");
    config::record_selection("settings");
    config::record_selection("live:agent:w1:p1");
    std::env::set_var("HERDR_SOCKET_PATH", "/tmp/herdr-b.sock");
    assert!(config::load_history()["settings"].is_number());
    assert!(config::load_history()["live@%2Ftmp%2Fherdr-a.sock:live:agent:w1:p1"].is_null());
    config::record_selection("live:agent:w2:p2");
    std::env::set_var("HERDR_SOCKET_PATH", "/tmp/herdr-a.sock");
    let history = config::load_history();
    restore_env("HERDR_SOCKET_PATH", old_socket);
    restore_env("XDG_STATE_HOME", old_state);
    fs::remove_dir_all(root).unwrap();
    assert!(history["live@%2Ftmp%2Fherdr-a.sock:live:agent:w1:p1"].is_number());
    assert!(history["live@%2Ftmp%2Fherdr-b.sock:live:agent:w2:p2"].is_null());
}

fn item(id: &str, title: &str, category: &str) -> Value {
    json!({"id":id,"title":title,"category":category,"aliases":[],"shortcuts":[]})
}

#[test]
fn search_matches_scopes_paths_and_category_aliases() {
    let items = vec![
        item("live:workspace:w1", "native_shell", "Workspace"),
        json!({"id":"live:worktree:w1","title":"checkout","category":"Worktrees","aliases":[],"searchPaths":["/repo/feature/payments"]}),
        json!({"id":"live:agent:w1:p1","title":"Review checkout","category":"Agents","aliases":["codex"],"savedSession":true}),
        item("split_vertical", "Split pane right", "Actions"),
    ];
    let empty = json!({});
    assert_eq!(
        search::search(&items, "@", "All", &empty)
            .iter()
            .map(|v| v["id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["live:workspace:w1", "live:worktree:w1"]
    );
    assert_eq!(
        search::search(&items, ">codex", "All", &empty)
            .iter()
            .map(|v| v["id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["live:agent:w1:p1"]
    );
    assert_eq!(
        search::search(&items, "payments", "Workspace", &empty)
            .iter()
            .map(|v| v["id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["live:worktree:w1"]
    );
    assert_eq!(
        search::search(&items, ":split", "All", &empty)
            .iter()
            .map(|v| v["id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["split_vertical"]
    );
}

#[test]
fn fuzzy_scorer_matches_unicode_highlight_positions() {
    assert_eq!(search::matching_positions("rv", "🚀 Review"), vec![2, 4]);
    assert_eq!(search::matching_positions("世界", "🚀 世界"), vec![2, 3]);
    assert_eq!(search::matching_positions("aa", "banana"), vec![3, 5]);
    assert!(search::fuzzy_score("xyz", "ordering-service").is_infinite());
}

fn executable(path: &Path, source: &str) {
    fs::write(path, source).unwrap();
    let mut permissions = fs::metadata(path).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).unwrap();
}

#[test]
fn update_check_install_and_pin_eligibility_use_native_commands() {
    let _guard = ENV_LOCK.lock().unwrap();
    let root = temp_dir("update");
    let bin = root.join("bin");
    fs::create_dir(&bin).unwrap();
    let mode = root.join("installed");
    let herdr = bin.join("herdr");
    executable(
        &herdr,
        &format!(
            r#"#!/bin/sh
if [ "$2" = "install" ]; then touch '{}'; exit 0; fi
if [ -f '{}' ]; then
  printf '%s' '{{"result":{{"plugins":[{{"plugin_id":"herdr-omni","enabled":true,"version":"0.6.0","source":{{"kind":"github","owner":"mmjang","repo":"herdr-omni","resolved_commit":"newcommit","requested_ref":"v0.6.0"}}}}]}}}}'
else
  printf '%s' '{{"result":{{"plugins":[{{"plugin_id":"herdr-omni","enabled":true,"version":"0.5.1","source":{{"kind":"github","owner":"mmjang","repo":"herdr-omni","resolved_commit":"oldcommit"}}}}]}}}}'
fi
"#,
            mode.display(),
            mode.display()
        ),
    );
    let git = bin.join("git");
    executable(
        &git,
        "#!/bin/sh\nprintf '%s\\n' 'sha refs/tags/v0.6.0' 'sha refs/tags/v0.6.0-rc.1'\n",
    );
    let old_state = set_env("HERDR_PLUGIN_STATE_DIR", Some(&root.join("state")));
    let old_bin = std::env::var_os("HERDR_BIN_PATH");
    let old_path = std::env::var_os("PATH");
    std::env::set_var("HERDR_BIN_PATH", &herdr);
    std::env::set_var(
        "PATH",
        format!(
            "{}:{}",
            bin.display(),
            old_path.as_deref().unwrap_or_default().to_string_lossy()
        ),
    );
    let discovered = update::check("0.5.1").unwrap();
    assert_eq!(discovered, json!({"version":"0.6.0","ref":"v0.6.0"}));
    update::dismiss(&discovered);
    assert!(update::check("0.5.1").is_none());
    // A dismissed offer remains valid after install is explicitly requested.
    update::install(&discovered).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&fs::read_to_string(root.join("state/update.json")).unwrap())
            .unwrap()["updaterInstall"]["ref"],
        "v0.6.0"
    );
    restore_env("PATH", old_path);
    restore_env("HERDR_BIN_PATH", old_bin);
    restore_env("HERDR_PLUGIN_STATE_DIR", old_state);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn update_rejects_invalid_offers_and_existing_locks() {
    let _guard = ENV_LOCK.lock().unwrap();
    assert!(update::install(&json!({"version":"1.2","ref":"v1.2"})).is_err());
    let root = temp_dir("update-lock");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("update.lock"), "active").unwrap();
    let old_state = set_env("HERDR_PLUGIN_STATE_DIR", Some(&root));
    assert!(update::install(&json!({"version":"0.6.0","ref":"v0.6.0"})).is_err());
    restore_env("HERDR_PLUGIN_STATE_DIR", old_state);
    fs::remove_dir_all(root).unwrap();

    let stale_root = temp_dir("update-stale-lock");
    fs::write(
        stale_root.join("update.lock"),
        json!({"pid":999_999_999,"createdAt":0}).to_string(),
    )
    .unwrap();
    let old_state = set_env("HERDR_PLUGIN_STATE_DIR", Some(&stale_root));
    let old_binary = std::env::var_os("HERDR_BIN_PATH");
    std::env::set_var("HERDR_BIN_PATH", stale_root.join("missing-herdr"));
    assert!(update::install(&json!({"version":"0.6.0","ref":"v0.6.0"})).is_err());
    assert!(
        !stale_root.join("update.lock").exists(),
        "failed install must release reclaimed lock"
    );
    restore_env("HERDR_BIN_PATH", old_binary);
    restore_env("HERDR_PLUGIN_STATE_DIR", old_state);
    fs::remove_dir_all(stale_root).unwrap();
}

#[test]
fn update_does_not_cache_a_failed_tag_lookup() {
    let _guard = ENV_LOCK.lock().unwrap();
    let root = temp_dir("update-retry");
    let bin = root.join("bin");
    fs::create_dir(&bin).unwrap();
    let marker = root.join("git-called");
    let herdr = bin.join("herdr");
    executable(&herdr, "#!/bin/sh\nprintf '%s' '{\"result\":{\"plugins\":[{\"plugin_id\":\"herdr-omni\",\"enabled\":true,\"version\":\"0.5.1\",\"source\":{\"kind\":\"github\",\"owner\":\"mmjang\",\"repo\":\"herdr-omni\",\"resolved_commit\":\"commit\"}}]}}'\n");
    let git = bin.join("git");
    executable(&git, &format!("#!/bin/sh\nif [ ! -f '{}' ]; then touch '{}'; exit 1; fi\nprintf '%s\\n' 'sha refs/tags/v0.6.0'\n", marker.display(), marker.display()));
    let old_state = set_env("HERDR_PLUGIN_STATE_DIR", Some(&root.join("state")));
    let old_bin = std::env::var_os("HERDR_BIN_PATH");
    let old_path = std::env::var_os("PATH");
    std::env::set_var("HERDR_BIN_PATH", &herdr);
    std::env::set_var(
        "PATH",
        format!(
            "{}:{}",
            bin.display(),
            old_path.as_deref().unwrap_or_default().to_string_lossy()
        ),
    );
    assert!(update::check("0.5.1").is_none());
    assert_eq!(
        update::check("0.5.1"),
        Some(json!({"version":"0.6.0","ref":"v0.6.0"}))
    );
    restore_env("PATH", old_path);
    restore_env("HERDR_BIN_PATH", old_bin);
    restore_env("HERDR_PLUGIN_STATE_DIR", old_state);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn update_ignores_valid_stdout_from_failed_plugin_listing() {
    let _guard = ENV_LOCK.lock().unwrap();
    let root = temp_dir("update-herdr-status");
    let bin = root.join("bin");
    fs::create_dir(&bin).unwrap();
    let git_called = root.join("git-called");
    let herdr = bin.join("herdr");
    executable(
        &herdr,
        "#!/bin/sh
printf '%s' '{\"result\":{\"plugins\":[{\"plugin_id\":\"herdr-omni\",\"enabled\":true,\"version\":\"0.5.1\",\"source\":{\"kind\":\"github\",\"owner\":\"mmjang\",\"repo\":\"herdr-omni\",\"resolved_commit\":\"commit\"}}]}}'
exit 1
",
    );
    let git = bin.join("git");
    executable(
        &git,
        &format!(
            "#!/bin/sh\ntouch '{}'\nprintf '%s\\n' 'sha refs/tags/v0.6.0'\n",
            git_called.display()
        ),
    );
    let old_state = set_env("HERDR_PLUGIN_STATE_DIR", Some(&root.join("state")));
    let old_bin = std::env::var_os("HERDR_BIN_PATH");
    let old_path = std::env::var_os("PATH");
    std::env::set_var("HERDR_BIN_PATH", &herdr);
    std::env::set_var(
        "PATH",
        format!(
            "{}:{}",
            bin.display(),
            old_path.as_deref().unwrap_or_default().to_string_lossy()
        ),
    );
    assert!(update::check("0.5.1").is_none());
    assert!(
        !git_called.exists(),
        "failed plugin listing must stop eligibility"
    );
    restore_env("PATH", old_path);
    restore_env("HERDR_BIN_PATH", old_bin);
    restore_env("HERDR_PLUGIN_STATE_DIR", old_state);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn update_caches_negative_lookup_and_accepts_large_versions() {
    let _guard = ENV_LOCK.lock().unwrap();
    let root = temp_dir("update-cache-parity");
    let bin = root.join("bin");
    fs::create_dir(&bin).unwrap();
    let count = root.join("git-count");
    let herdr = bin.join("herdr");
    executable(&herdr, "#!/bin/sh\nprintf '%s' '{\"result\":{\"plugins\":[{\"plugin_id\":\"herdr-omni\",\"enabled\":true,\"version\":\"0.5.1\",\"source\":{\"kind\":\"github\",\"owner\":\"mmjang\",\"repo\":\"herdr-omni\",\"resolved_commit\":\"commit\"}}]}}'\n");
    let git = bin.join("git");
    executable(
        &git,
        &format!(
            "#!/bin/sh\nn=$(cat '{}' 2>/dev/null || echo 0)\necho $((n + 1)) > '{}'\nprintf '%s\\n' 'sha refs/tags/v0.5.1'\n",
            count.display(),
            count.display()
        ),
    );
    let old_state = set_env("HERDR_PLUGIN_STATE_DIR", Some(&root.join("state")));
    let old_bin = std::env::var_os("HERDR_BIN_PATH");
    let old_path = std::env::var_os("PATH");
    std::env::set_var("HERDR_BIN_PATH", &herdr);
    std::env::set_var(
        "PATH",
        format!(
            "{}:{}",
            bin.display(),
            old_path.as_deref().unwrap_or_default().to_string_lossy()
        ),
    );
    assert!(update::check("0.5.1").is_none());
    assert!(update::check("0.5.1").is_none());
    assert_eq!(fs::read_to_string(&count).unwrap().trim(), "1");
    assert_eq!(
        fs::metadata(root.join("state/update.json"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    restore_env("PATH", old_path);
    restore_env("HERDR_BIN_PATH", old_bin);
    restore_env("HERDR_PLUGIN_STATE_DIR", old_state);
    fs::remove_dir_all(root).unwrap();

    let root = temp_dir("update-large-version");
    let bin = root.join("bin");
    fs::create_dir(&bin).unwrap();
    let herdr = bin.join("herdr");
    executable(&herdr, "#!/bin/sh\nprintf '%s' '{\"result\":{\"plugins\":[{\"plugin_id\":\"herdr-omni\",\"enabled\":true,\"version\":\"0.5.1\",\"source\":{\"kind\":\"github\",\"owner\":\"mmjang\",\"repo\":\"herdr-omni\",\"resolved_commit\":\"commit\"}}]}}'\n");
    let git = bin.join("git");
    executable(
        &git,
        "#!/bin/sh\nprintf '%s\\n' 'sha refs/tags/v18446744073709551616.0.0'\n",
    );
    let old_state = set_env("HERDR_PLUGIN_STATE_DIR", Some(&root.join("state")));
    let old_bin = std::env::var_os("HERDR_BIN_PATH");
    let old_path = std::env::var_os("PATH");
    std::env::set_var("HERDR_BIN_PATH", &herdr);
    std::env::set_var("PATH", format!("{}:/bin:/usr/bin", bin.display()));
    assert_eq!(
        update::check("0.5.1"),
        Some(json!({
            "version": "18446744073709551616.0.0",
            "ref": "v18446744073709551616.0.0"
        }))
    );
    restore_env("PATH", old_path);
    restore_env("HERDR_BIN_PATH", old_bin);
    restore_env("HERDR_PLUGIN_STATE_DIR", old_state);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn update_cache_write_preserves_state_changed_during_tag_lookup() {
    let _guard = ENV_LOCK.lock().unwrap();
    let root = temp_dir("update-cache-merge");
    let bin = root.join("bin");
    let state = root.join("state");
    fs::create_dir(&bin).unwrap();
    fs::create_dir(&state).unwrap();
    let herdr = bin.join("herdr");
    executable(&herdr, "#!/bin/sh\nprintf '%s' '{\"result\":{\"plugins\":[{\"plugin_id\":\"herdr-omni\",\"enabled\":true,\"version\":\"0.5.1\",\"source\":{\"kind\":\"github\",\"owner\":\"mmjang\",\"repo\":\"herdr-omni\",\"resolved_commit\":\"commit\"}}]}}'\n");
    let git = bin.join("git");
    executable(
        &git,
        &format!(
            "#!/bin/sh\nprintf '%s' '{{\"updaterInstall\":{{\"ref\":\"v0.6.0\",\"resolved_commit\":\"during-check\"}}}}' > '{}/update.json'\nprintf '%s\\n' 'sha refs/tags/v0.6.0'\n",
            state.display()
        ),
    );
    let old_state = set_env("HERDR_PLUGIN_STATE_DIR", Some(&state));
    let old_bin = std::env::var_os("HERDR_BIN_PATH");
    let old_path = std::env::var_os("PATH");
    std::env::set_var("HERDR_BIN_PATH", &herdr);
    std::env::set_var(
        "PATH",
        format!(
            "{}:{}",
            bin.display(),
            old_path.as_deref().unwrap_or_default().to_string_lossy()
        ),
    );
    assert_eq!(
        update::check("0.5.1"),
        Some(json!({"version":"0.6.0","ref":"v0.6.0"}))
    );
    let persisted: Value =
        serde_json::from_str(&fs::read_to_string(state.join("update.json")).unwrap()).unwrap();
    assert_eq!(persisted["updaterInstall"]["ref"], "v0.6.0");
    assert_eq!(
        persisted["updaterInstall"]["resolved_commit"],
        "during-check"
    );
    assert_eq!(
        persisted["cache"]["offer"],
        json!({"version":"0.6.0","ref":"v0.6.0"})
    );
    restore_env("PATH", old_path);
    restore_env("HERDR_BIN_PATH", old_bin);
    restore_env("HERDR_PLUGIN_STATE_DIR", old_state);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn update_rejects_failed_post_install_listing_even_with_valid_json() {
    let _guard = ENV_LOCK.lock().unwrap();
    let root = temp_dir("update-install-status");
    let bin = root.join("bin");
    fs::create_dir(&bin).unwrap();
    let installed = root.join("installed");
    let herdr = bin.join("herdr");
    executable(
        &herdr,
        &format!(
            "#!/bin/sh\nif [ \"$2\" = install ]; then touch '{}'; exit 0; fi\nif [ -f '{}' ]; then printf '%s' '{{\"result\":{{\"plugins\":[{{\"plugin_id\":\"herdr-omni\",\"enabled\":true,\"version\":\"0.6.0\",\"source\":{{\"kind\":\"github\",\"owner\":\"mmjang\",\"repo\":\"herdr-omni\",\"resolved_commit\":\"new\",\"requested_ref\":\"v0.6.0\"}}}}]}}}}'; exit 1; fi\nprintf '%s' '{{\"result\":{{\"plugins\":[{{\"plugin_id\":\"herdr-omni\",\"enabled\":true,\"version\":\"0.5.1\",\"source\":{{\"kind\":\"github\",\"owner\":\"mmjang\",\"repo\":\"herdr-omni\",\"resolved_commit\":\"old\"}}}}]}}}}'\n",
            installed.display(),
            installed.display()
        ),
    );
    let old_state = set_env("HERDR_PLUGIN_STATE_DIR", Some(&root.join("state")));
    let old_bin = std::env::var_os("HERDR_BIN_PATH");
    std::env::set_var("HERDR_BIN_PATH", &herdr);
    let error = update::install(&json!({"version":"0.6.0","ref":"v0.6.0"})).unwrap_err();
    assert!(error.contains("could not be verified"));
    restore_env("HERDR_BIN_PATH", old_bin);
    restore_env("HERDR_PLUGIN_STATE_DIR", old_state);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn update_check_bounds_large_command_output() {
    let _guard = ENV_LOCK.lock().unwrap();
    let root = temp_dir("update-large-output");
    let herdr = root.join("herdr");
    executable(&herdr, "#!/bin/sh\nhead -c 5000000 /dev/zero\n");
    let old_state = set_env("HERDR_PLUGIN_STATE_DIR", Some(&root.join("state")));
    let old_binary = std::env::var_os("HERDR_BIN_PATH");
    std::env::set_var("HERDR_BIN_PATH", &herdr);

    let started = Instant::now();
    assert!(update::check_cancellable("0.5.1", &AtomicBool::new(false)).is_none());
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "oversized output took too long to terminate"
    );

    restore_env("HERDR_BIN_PATH", old_binary);
    restore_env("HERDR_PLUGIN_STATE_DIR", old_state);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn update_check_cancellation_terminates_descendants_holding_pipes() {
    let _guard = ENV_LOCK.lock().unwrap();
    let root = temp_dir("update-cancel-descendant");
    let marker = root.join("started");
    let herdr = root.join("herdr");
    executable(
        &herdr,
        &format!(
            "#!/bin/sh\ntouch '{}'\n(sleep 30)&\nprintf '%s' '{{\"result\":{{\"plugins\":[]}}}}'\nsleep 30\n",
            marker.display()
        ),
    );
    let old_state = set_env("HERDR_PLUGIN_STATE_DIR", Some(&root.join("state")));
    let old_binary = std::env::var_os("HERDR_BIN_PATH");
    std::env::set_var("HERDR_BIN_PATH", &herdr);

    let cancel = Arc::new(AtomicBool::new(false));
    let worker_cancel = Arc::clone(&cancel);
    let worker = thread::spawn(move || update::check_cancellable("0.5.1", &worker_cancel));
    let wait_started = Instant::now();
    while !marker.exists() && wait_started.elapsed() < Duration::from_secs(1) {
        thread::sleep(Duration::from_millis(10));
    }
    assert!(marker.exists(), "mock Herdr did not start");
    let cancelled_at = Instant::now();
    cancel.store(true, Ordering::Relaxed);
    assert!(worker.join().unwrap().is_none());
    assert!(
        cancelled_at.elapsed() < Duration::from_secs(2),
        "cancellation waited for a descendant holding a pipe"
    );

    restore_env("HERDR_BIN_PATH", old_binary);
    restore_env("HERDR_PLUGIN_STATE_DIR", old_state);
    fs::remove_dir_all(root).unwrap();
}
