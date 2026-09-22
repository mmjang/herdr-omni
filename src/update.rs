//! Native update checks for the official Herdr Omni plugin.

use crate::process::{isolate, terminate};
use serde_json::{json, Value};
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const OWNER: &str = "mmjang";
const REPOSITORY: &str = "herdr-omni";
const PLUGIN: &str = "herdr-omni";
const CACHE_MS: u64 = 60 * 60 * 1_000;
const SNOOZE_MS: u64 = 24 * 60 * 60 * 1_000;
const LOCK_STALE_MS: u64 = 30 * 60 * 1_000;
const MAX_COMMAND_OUTPUT_BYTES: usize = 4 * 1024 * 1024;
static STATE_WRITE_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_millis() as u64
}
fn state_dir() -> PathBuf {
    std::env::var_os("HERDR_PLUGIN_STATE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .or_else(dirs::home_dir)
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".local/state/herdr-omni")
        })
}
fn state_path(dir: &Path) -> PathBuf {
    dir.join("update.json")
}
fn read_state(dir: &Path) -> Value {
    fs::read_to_string(state_path(dir))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .filter(|v: &Value| v.is_object())
        .unwrap_or_else(|| json!({}))
}
fn write_state(dir: &Path, state: &Value) {
    if fs::create_dir_all(dir).is_err() {
        return;
    }
    let temporary = dir.join(format!(
        "update.json.{}.{}.{}.tmp",
        std::process::id(),
        now(),
        STATE_WRITE_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let Ok(mut file) = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temporary)
    else {
        return;
    };
    if file
        .write_all(&serde_json::to_vec(state).unwrap_or_default())
        .is_err()
        || fs::rename(&temporary, state_path(dir)).is_err()
    {
        let _ = fs::remove_file(temporary);
    }
}
fn command(program: &str, args: &[&str], timeout: Duration) -> Option<(i32, String, String)> {
    command_cancellable(program, args, timeout, None)
}

fn command_cancellable(
    program: &str,
    args: &[&str],
    timeout: Duration,
    cancel: Option<&AtomicBool>,
) -> Option<(i32, String, String)> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| PathBuf::from("."));
    let mut command = Command::new(program);
    command
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .current_dir(home)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    isolate(&mut command);
    let mut child = command.spawn().ok()?;
    let Some(stdout) = child.stdout.take() else {
        terminate(&mut child);
        let _ = child.wait();
        return None;
    };
    let Some(stderr) = child.stderr.take() else {
        terminate(&mut child);
        let _ = child.wait();
        return None;
    };
    let (stdout_sender, stdout_receiver) = mpsc::channel();
    let (stderr_sender, stderr_receiver) = mpsc::channel();
    let stdout_thread = thread::spawn(move || {
        let result = read_command_pipe(stdout);
        let _ = stdout_sender.send(result);
    });
    let stderr_thread = thread::spawn(move || {
        let result = read_command_pipe(stderr);
        let _ = stderr_sender.send(result);
    });
    let mut stdout_result = None;
    let mut stderr_result = None;
    let started = std::time::Instant::now();
    loop {
        if cancel.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
            terminate(&mut child);
            let _ = child.wait();
            let _ = collect_command_output(
                1,
                stdout_result,
                stderr_result,
                stdout_receiver,
                stderr_receiver,
                stdout_thread,
                stderr_thread,
            );
            return None;
        }
        poll_command_pipe(&stdout_receiver, &mut stdout_result);
        poll_command_pipe(&stderr_receiver, &mut stderr_result);
        if stdout_result.as_ref().is_some_and(Result::is_err)
            || stderr_result.as_ref().is_some_and(Result::is_err)
        {
            terminate(&mut child);
            let _ = child.wait();
            let _ = collect_command_output(
                1,
                stdout_result,
                stderr_result,
                stdout_receiver,
                stderr_receiver,
                stdout_thread,
                stderr_thread,
            );
            return None;
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                terminate(&mut child);
                let _ = child.wait();
                return collect_command_output(
                    status.code().unwrap_or(1),
                    stdout_result,
                    stderr_result,
                    stdout_receiver,
                    stderr_receiver,
                    stdout_thread,
                    stderr_thread,
                );
            }
            Ok(None) if started.elapsed() < timeout => thread::sleep(Duration::from_millis(20)),
            _ => {
                terminate(&mut child);
                let _ = child.wait();
                return collect_command_output(
                    1,
                    stdout_result,
                    stderr_result,
                    stdout_receiver,
                    stderr_receiver,
                    stdout_thread,
                    stderr_thread,
                );
            }
        }
    }
}

fn poll_command_pipe(
    receiver: &Receiver<Result<Vec<u8>, ()>>,
    result: &mut Option<Result<Vec<u8>, ()>>,
) {
    if result.is_some() {
        return;
    }
    match receiver.try_recv() {
        Ok(value) => *result = Some(value),
        Err(TryRecvError::Disconnected) => *result = Some(Err(())),
        Err(TryRecvError::Empty) => {}
    }
}

fn read_command_pipe<R: Read>(mut pipe: R) -> Result<Vec<u8>, ()> {
    let mut output = Vec::new();
    let mut buffer = [0_u8; 8192];
    loop {
        let count = pipe.read(&mut buffer).map_err(|_| ())?;
        if count == 0 {
            return Ok(output);
        }
        if output.len().saturating_add(count) > MAX_COMMAND_OUTPUT_BYTES {
            return Err(());
        }
        output.extend_from_slice(&buffer[..count]);
    }
}

fn collect_command_output(
    code: i32,
    stdout_result: Option<Result<Vec<u8>, ()>>,
    stderr_result: Option<Result<Vec<u8>, ()>>,
    stdout_receiver: Receiver<Result<Vec<u8>, ()>>,
    stderr_receiver: Receiver<Result<Vec<u8>, ()>>,
    stdout_thread: JoinHandle<()>,
    stderr_thread: JoinHandle<()>,
) -> Option<(i32, String, String)> {
    let stdout = stdout_result
        .or_else(|| stdout_receiver.recv().ok())
        .and_then(Result::ok);
    let stderr = stderr_result
        .or_else(|| stderr_receiver.recv().ok())
        .and_then(Result::ok);
    let _ = stdout_thread.join();
    let _ = stderr_thread.join();
    Some((
        code,
        String::from_utf8_lossy(&stdout?).into_owned(),
        String::from_utf8_lossy(&stderr?).into_owned(),
    ))
}
fn herdr(args: &[&str]) -> Option<(i32, String, String)> {
    herdr_cancellable(args, None)
}

fn herdr_cancellable(args: &[&str], cancel: Option<&AtomicBool>) -> Option<(i32, String, String)> {
    let binary = std::env::var("HERDR_BIN_PATH").unwrap_or_else(|_| "herdr".into());
    command_cancellable(&binary, args, Duration::from_secs(10), cancel)
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Version([String; 3]);

impl Version {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        for (left, right) in self.0.iter().zip(other.0.iter()) {
            match left.len().cmp(&right.len()) {
                std::cmp::Ordering::Equal => match left.as_bytes().cmp(right.as_bytes()) {
                    std::cmp::Ordering::Equal => {}
                    ordering => return ordering,
                },
                ordering => return ordering,
            }
        }
        std::cmp::Ordering::Equal
    }
}

fn parse_version(value: &str) -> Option<Version> {
    let value = value.strip_prefix('v').unwrap_or(value);
    let parts: Vec<_> = value.split('.').collect();
    if parts.len() != 3
        || parts.iter().any(|part| {
            part.is_empty()
                || (part.len() > 1 && part.starts_with('0'))
                || !part.chars().all(|c| c.is_ascii_digit())
        })
    {
        return None;
    }
    Some(Version([
        parts[0].to_string(),
        parts[1].to_string(),
        parts[2].to_string(),
    ]))
}
fn offer(value: &Value) -> Option<(&str, &str)> {
    let version = value.get("version")?.as_str()?;
    let reference = value.get("ref")?.as_str()?;
    if reference == format!("v{version}") && parse_version(reference).is_some() {
        Some((version, reference))
    } else {
        None
    }
}
fn plugin(stdout: &str) -> Option<Value> {
    serde_json::from_str::<Value>(stdout)
        .ok()?
        .get("result")?
        .get("plugins")?
        .as_array()?
        .iter()
        .find(|plugin| plugin.get("plugin_id").and_then(Value::as_str) == Some(PLUGIN))
        .cloned()
}
fn official(plugin: &Value) -> bool {
    let requested_ref_valid = plugin
        .get("source")
        .and_then(|source| source.get("requested_ref"))
        .map(|requested| requested.is_null() || requested.as_str().is_some())
        .unwrap_or(true);
    plugin.get("enabled").and_then(Value::as_bool) == Some(true)
        && plugin
            .get("source")
            .and_then(|v| v.get("kind"))
            .and_then(Value::as_str)
            == Some("github")
        && plugin
            .get("source")
            .and_then(|v| v.get("owner"))
            .and_then(Value::as_str)
            == Some(OWNER)
        && plugin
            .get("source")
            .and_then(|v| v.get("repo"))
            .and_then(Value::as_str)
            == Some(REPOSITORY)
        && plugin
            .get("source")
            .and_then(|v| v.get("resolved_commit"))
            .and_then(Value::as_str)
            .is_some_and(|v| !v.is_empty())
        && plugin
            .get("source")
            .and_then(|v| v.get("subdir"))
            .map(|v| v.is_null() || v.as_str() == Some(""))
            .unwrap_or(true)
        && requested_ref_valid
}
fn eligible() -> Option<Value> {
    eligible_cancellable(None)
}

fn eligible_cancellable(cancel: Option<&AtomicBool>) -> Option<Value> {
    let (code, stdout, _) =
        herdr_cancellable(&["plugin", "list", "--plugin", PLUGIN, "--json"], cancel)?;
    if code != 0 {
        return None;
    }
    let plugin = plugin(&stdout)?;
    if !official(&plugin) {
        return None;
    }
    let state = read_state(&state_dir());
    if let Some(requested) = plugin
        .get("source")
        .and_then(|v| v.get("requested_ref"))
        .and_then(Value::as_str)
    {
        let marker = state.get("updaterInstall");
        if marker.and_then(|v| v.get("ref")).and_then(Value::as_str) != Some(requested)
            || marker
                .and_then(|v| v.get("resolved_commit"))
                .and_then(Value::as_str)
                != plugin
                    .get("source")
                    .and_then(|v| v.get("resolved_commit"))
                    .and_then(Value::as_str)
        {
            return None;
        }
    }
    Some(plugin)
}

fn installed_plugin() -> Option<Value> {
    let (code, stdout, _) = herdr(&["plugin", "list", "--plugin", PLUGIN, "--json"])?;
    if code != 0 {
        return None;
    }
    let plugin = plugin(&stdout)?;
    official(&plugin).then_some(plugin)
}

fn stale_lock(path: &Path) -> bool {
    let age = |created: u64| now().saturating_sub(created) >= LOCK_STALE_MS;
    if let Ok(source) = fs::read_to_string(path) {
        if let Ok(value) = serde_json::from_str::<Value>(&source) {
            let Some(created) = value.get("createdAt").and_then(Value::as_u64) else {
                return false;
            };
            if !age(created) {
                return false;
            }
            if let Some(pid) = value.get("pid").and_then(Value::as_u64) {
                if pid == u64::from(std::process::id()) {
                    return false;
                }
                return Command::new("kill")
                    .args(["-0", &pid.to_string()])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()
                    .map(|status| !status.success())
                    .unwrap_or(false);
            }
            return true;
        }
    }
    fs::metadata(path)
        .ok()
        .and_then(|metadata| metadata.modified().ok())
        .and_then(|modified| modified.elapsed().ok())
        .is_some_and(|elapsed| elapsed.as_millis() as u64 >= LOCK_STALE_MS)
}

fn acquire_lock(dir: &Path) -> Result<PathBuf, String> {
    fs::create_dir_all(dir).map_err(|_| "Unable to start update.".to_string())?;
    let lock = dir.join("update.lock");
    for _ in 0..2 {
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&lock)
        {
            Ok(file) => {
                drop(file);
                let _ = fs::write(
                    &lock,
                    serde_json::to_vec(&json!({"pid":std::process::id(),"createdAt":now()}))
                        .unwrap_or_default(),
                );
                return Ok(lock);
            }
            Err(_) if stale_lock(&lock) => {
                let _ = fs::remove_file(&lock);
            }
            Err(_) => return Err("Another update is already in progress.".into()),
        }
    }
    Err("Another update is already in progress.".into())
}

/// Check GitHub tags and return the highest stable version newer than `current`.
pub fn check(current: &str) -> Option<Value> {
    check_inner(current, None)
}

/// Cancellable update check for callers that own a worker thread.
pub fn check_cancellable(current: &str, cancel: &AtomicBool) -> Option<Value> {
    check_inner(current, Some(cancel))
}

fn check_inner(current: &str, cancel: Option<&AtomicBool>) -> Option<Value> {
    let current_version = parse_version(current)?;
    if cancel.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
        return None;
    }
    eligible_cancellable(cancel)?;
    if cancel.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
        return None;
    }
    let dir = state_dir();
    let mut state = read_state(&dir);
    let stamp = now();
    let cached = state.get("cache").filter(|cache| {
        cache.get("currentVersion").and_then(Value::as_str) == Some(current)
            && stamp.saturating_sub(cache.get("checkedAt").and_then(Value::as_u64).unwrap_or(0))
                < CACHE_MS
    });
    let candidate = if let Some(cache) = cached {
        cache.get("offer").cloned()
    } else {
        let (code, stdout, _) = command_cancellable(
            "git",
            &[
                "ls-remote",
                "--tags",
                "https://github.com/mmjang/herdr-omni.git",
                "refs/tags/v*",
            ],
            Duration::from_secs(5),
            cancel,
        )?;
        if cancel.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
            return None;
        }
        // A network/timeout failure is a transient negative result. Keep the previous cache
        // untouched so the next check can retry instead of suppressing discovery for an hour.
        if code != 0 {
            return None;
        }
        let mut best: Option<(Version, String)> = None;
        for line in stdout.lines() {
            let Some(tag) = line
                .split_whitespace()
                .nth(1)
                .and_then(|v| v.strip_prefix("refs/tags/"))
            else {
                continue;
            };
            let Some(version) = parse_version(tag) else {
                continue;
            };
            if best
                .as_ref()
                .is_none_or(|(old, _)| version.cmp(old) == std::cmp::Ordering::Greater)
            {
                best = Some((version, tag.into()));
            }
        }
        let value = best.map(|(_, reference)| json!({"version": reference.trim_start_matches('v'), "ref": reference}));
        let mut fresh = read_state(&dir);
        fresh["cache"] = json!({"checkedAt": stamp, "currentVersion": current, "offer": value});
        write_state(&dir, &fresh);
        state = fresh;
        value
    }?;
    if cancel.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
        return None;
    }
    let (version, reference) = offer(&candidate)?;
    if parse_version(reference)?.cmp(&current_version) != std::cmp::Ordering::Greater {
        return None;
    }
    let snooze = state.get("snooze");
    if snooze
        .and_then(|v| v.get("version"))
        .and_then(Value::as_str)
        == Some(version)
        && stamp.saturating_sub(
            snooze
                .and_then(|v| v.get("dismissedAt"))
                .and_then(Value::as_u64)
                .unwrap_or(0),
        ) < SNOOZE_MS
    {
        return None;
    }
    Some(candidate)
}

/// Install a validated offer and verify Herdr reports the requested version/ref afterwards.
pub fn install(value: &Value) -> Result<(), String> {
    let Some((version, reference)) = offer(value) else {
        return Err("Invalid update offer.".into());
    };
    let dir = state_dir();
    let lock = acquire_lock(&dir)?;
    let result = (|| {
        let before = eligible()
            .ok_or_else(|| "herdr-omni is no longer eligible for automatic updates.".to_string())?;
        let installed = before
            .get("version")
            .and_then(Value::as_str)
            .and_then(parse_version)
            .ok_or_else(|| "Unable to determine the installed version.".to_string())?;
        if parse_version(reference)
            .is_none_or(|next| next.cmp(&installed) != std::cmp::Ordering::Greater)
        {
            return Err("This update is no longer newer than the installed version.".into());
        }
        let (code, _, stderr) = command(
            &std::env::var("HERDR_BIN_PATH").unwrap_or_else(|_| "herdr".into()),
            &[
                "plugin",
                "install",
                "mmjang/herdr-omni",
                "--ref",
                reference,
                "--yes",
            ],
            Duration::from_secs(60),
        )
        .ok_or_else(|| "Unable to install the update.".to_string())?;
        if code != 0 {
            return Err(if stderr.is_empty() {
                "Unable to install the update.".into()
            } else {
                stderr
            });
        }
        let after = installed_plugin().ok_or_else(|| "Update finished but could not be verified. Check herdr plugin list and reopen Omni.".to_string())?;
        let pinned = after
            .get("source")
            .and_then(|v| v.get("requested_ref"))
            .and_then(Value::as_str);
        if pinned != Some(reference)
            || after
                .get("version")
                .and_then(Value::as_str)
                .and_then(parse_version)
                != parse_version(version)
        {
            return Err("Update finished but could not be verified. Check herdr plugin list and reopen Omni.".into());
        }
        let mut state = read_state(&dir);
        state["updaterInstall"] =
            json!({"ref": reference, "resolved_commit": after["source"]["resolved_commit"]});
        write_state(&dir, &state);
        Ok(())
    })();
    let _ = fs::remove_file(lock);
    result
}

pub fn dismiss(value: &Value) {
    let Some((version, _)) = offer(value) else {
        return;
    };
    let dir = state_dir();
    let snooze = json!({"version": version, "dismissedAt": now()});
    // Read immediately before writing; atomic replacement prevents partial files, while
    // concurrent state updates remain last-writer-wins.
    let mut state = read_state(&dir);
    state["snooze"] = snooze;
    write_state(&dir, &state);
}
