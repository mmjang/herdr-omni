//! The Herdr integration boundary.
//!
//! This module deliberately uses `serde_json::Value` at its public boundary.
//! Herdr's wire format is an external contract and keeping it here means the
//! terminal UI does not need to know about the server's snapshot structs.

use chrono::DateTime;
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::os::unix::net::UnixStream;
use std::os::unix::{ffi::OsStrExt, io::FromRawFd};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const MAX_PREVIEW_BYTES: usize = 256 * 1024;
const MAX_SERVER_LOG_CHARS: usize = 8 * 1024 * 1024;
const MAX_SOCKET_RESPONSE_BYTES: usize = 1024 * 1024;
const PIPE_DRAIN_GRACE: Duration = Duration::from_millis(100);
const PIPE_KILL_GRACE: Duration = Duration::from_secs(1);
const SOCKET_TIMEOUT: Duration = Duration::from_secs(3);

use crate::process::{isolate as start_process_group, terminate as kill_process_group};

fn text(value: Option<&Value>) -> String {
    value
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn trimmed(value: Option<&Value>) -> String {
    text(value).trim().to_string()
}

fn records(value: Option<&Value>) -> Vec<&Map<String, Value>> {
    value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_object)
        .collect()
}

fn value_at<'a>(root: &'a Value, path: &[&str]) -> Option<&'a Value> {
    path.iter().try_fold(root, |value, key| value.get(*key))
}

fn explain(stderr: &str, code: Option<i32>) -> String {
    let trimmed = stderr.trim();
    if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
        if let Some(message) = value_at(&value, &["error", "message"]).and_then(Value::as_str) {
            return message.to_string();
        }
    }
    if !trimmed.is_empty() {
        return trimmed.to_string();
    }
    code.map_or_else(
        || "Herdr exited without a status.".to_string(),
        |status| format!("Herdr exited with status {status}."),
    )
}

fn binary() -> String {
    std::env::var("HERDR_BIN_PATH").unwrap_or_else(|_| "herdr".to_string())
}

/// Run Herdr without invoking a shell and decode its JSON response.
pub fn run_herdr(args: &[String]) -> Result<Value, String> {
    run_herdr_cancellable(args, None)
}

fn run_herdr_cancellable(args: &[String], cancel: Option<&AtomicBool>) -> Result<Value, String> {
    let timeout = if args.first().map(String::as_str) == Some("agent")
        && args.get(1).map(String::as_str) == Some("start")
    {
        Duration::from_secs(35)
    } else {
        Duration::from_secs(5)
    };
    let (status, stdout, stderr) = run_command_capture(args, timeout, 4 * 1024 * 1024, cancel)?;
    if !status.success() {
        return Err(explain(&String::from_utf8_lossy(&stderr), status.code()));
    }
    serde_json::from_slice(&stdout).map_err(|_| "Herdr returned unreadable JSON data.".to_string())
}

fn run_command_capture(
    args: &[String],
    timeout: Duration,
    max_bytes: usize,
    cancel: Option<&AtomicBool>,
) -> Result<(std::process::ExitStatus, Vec<u8>, Vec<u8>), String> {
    if cancel.is_some_and(|cancel| cancel.load(Ordering::Relaxed)) {
        return Err("Herdr command cancelled.".to_string());
    }
    let mut command = Command::new(binary());
    command
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    start_process_group(&mut command);
    let mut child = command
        .spawn()
        .map_err(|error| format!("Cannot run Herdr: {error}"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "Cannot run Herdr: stdout unavailable.".to_string())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "Cannot run Herdr: stderr unavailable.".to_string())?;
    let read_pipe = |mut pipe: Box<dyn Read + Send>| {
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        let thread = std::thread::spawn(move || {
            let mut data = Vec::new();
            let mut buffer = [0_u8; 8192];
            let result = loop {
                let count = pipe
                    .read(&mut buffer)
                    .map_err(|_| "Herdr response unavailable.".to_string());
                let count = match count {
                    Ok(count) => count,
                    Err(error) => break Err(error),
                };
                if count == 0 {
                    break Ok(data);
                }
                if data.len() + count > max_bytes {
                    break Err("Herdr response too large.".to_string());
                }
                data.extend_from_slice(&buffer[..count]);
            };
            let _ = sender.send(result);
        });
        (thread, receiver)
    };
    let (stdout_thread, stdout_receiver) = read_pipe(Box::new(stdout));
    let (stderr_thread, stderr_receiver) = read_pipe(Box::new(stderr));
    let started = std::time::Instant::now();
    let status = loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|_| "Cannot run Herdr: wait failed.".to_string())?
        {
            break status;
        }
        if started.elapsed() >= timeout {
            kill_process_group(&mut child);
            let _ = child.wait();
            return Err("Herdr command timed out.".to_string());
        }
        if cancel.is_some_and(|cancel| cancel.load(Ordering::Relaxed)) {
            kill_process_group(&mut child);
            let _ = child.wait();
            return Err("Herdr command cancelled.".to_string());
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let mut stdout = stdout_receiver.recv_timeout(PIPE_DRAIN_GRACE);
    let mut stderr = stderr_receiver.recv_timeout(PIPE_DRAIN_GRACE);
    if stdout.is_err() || stderr.is_err() {
        kill_process_group(&mut child);
        let _ = child.wait();
        if stdout.is_err() {
            stdout = stdout_receiver.recv_timeout(PIPE_KILL_GRACE);
        }
        if stderr.is_err() {
            stderr = stderr_receiver.recv_timeout(PIPE_KILL_GRACE);
        }
    }
    if !matches!(stdout, Err(std::sync::mpsc::RecvTimeoutError::Timeout)) {
        let _ = stdout_thread.join();
    }
    if !matches!(stderr, Err(std::sync::mpsc::RecvTimeoutError::Timeout)) {
        let _ = stderr_thread.join();
    }
    let stdout = stdout.map_err(|_| "Herdr response unavailable.".to_string())??;
    let stderr = stderr.map_err(|_| "Herdr response unavailable.".to_string())??;
    Ok((status, stdout, stderr))
}

fn launch_context_value() -> Option<Value> {
    let raw = std::env::var("HERDR_PLUGIN_CONTEXT_JSON").ok()?;
    let context: Value = serde_json::from_str(&raw).ok()?;
    let target = json!({
        "paneId": context.get("focused_pane_id").and_then(Value::as_str),
        "tabId": context.get("tab_id").and_then(Value::as_str),
        "workspaceId": context.get("workspace_id").and_then(Value::as_str),
    });
    if ["paneId", "tabId", "workspaceId"].iter().all(|key| {
        target
            .get(*key)
            .and_then(Value::as_str)
            .is_some_and(|v| !v.is_empty())
    }) {
        Some(target)
    } else {
        None
    }
}

/// Return the exact pane/tab/workspace that opened the popup.
pub fn launch_context() -> Option<Value> {
    launch_context_value()
}

fn session_target() -> Option<Value> {
    if let Some(target) = launch_context_value() {
        return Some(target);
    }
    let response = run_herdr(&["pane".into(), "current".into(), "--current".into()]).ok()?;
    let pane = value_at(&response, &["result", "pane"])?;
    let pane_id = pane.get("pane_id").and_then(Value::as_str)?;
    let tab_id = pane.get("tab_id").and_then(Value::as_str)?;
    let workspace_id = pane.get("workspace_id").and_then(Value::as_str)?;
    Some(json!({"paneId": pane_id, "tabId": tab_id, "workspaceId": workspace_id}))
}

fn target_string(target: &Value, key: &str) -> Result<String, String> {
    target
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .ok_or_else(|| "Herdr did not report the pane that opened the palette.".to_string())
}

fn command_result(ok: bool, message: impl Into<String>) -> Value {
    json!({"ok": ok, "message": message.into()})
}

/// Connect a filesystem Unix socket without allowing a blocked connect to
/// outlive the action deadline. The standard UnixStream API has no stable
/// connect-timeout method, so use a nonblocking descriptor and poll it.
fn connect_unix_timeout(path: &Path, timeout: Duration) -> io::Result<UnixStream> {
    let bytes = path.as_os_str().as_bytes();
    let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    if bytes.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Unix socket path contains NUL",
        ));
    }
    if bytes.len() + 1 > address.sun_path.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Unix socket path is too long",
        ));
    }
    for (slot, byte) in address.sun_path.iter_mut().zip(bytes.iter().copied()) {
        *slot = byte as libc::c_char;
    }
    let path_offset =
        (&address.sun_path as *const _ as usize).saturating_sub(&address as *const _ as usize);
    let address_len = path_offset + bytes.len() + 1;
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    {
        address.sun_len = address_len as u8;
    }
    address.sun_family = libc::AF_UNIX as libc::sa_family_t;

    // SAFETY: socket/fcntl/connect/poll/getsockopt operate on the descriptor
    // created here, and every error path closes it before returning.
    let fd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let close = || unsafe {
        libc::close(fd);
    };
    let descriptor_flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if descriptor_flags < 0 {
        let error = io::Error::last_os_error();
        close();
        return Err(error);
    }
    if unsafe { libc::fcntl(fd, libc::F_SETFD, descriptor_flags | libc::FD_CLOEXEC) } < 0 {
        let error = io::Error::last_os_error();
        close();
        return Err(error);
    }
    let original_flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if original_flags < 0 {
        let error = io::Error::last_os_error();
        close();
        return Err(error);
    }
    if unsafe { libc::fcntl(fd, libc::F_SETFL, original_flags | libc::O_NONBLOCK) } < 0 {
        let error = io::Error::last_os_error();
        close();
        return Err(error);
    }
    let result = unsafe {
        libc::connect(
            fd,
            &address as *const libc::sockaddr_un as *const libc::sockaddr,
            address_len as libc::socklen_t,
        )
    };
    if result < 0 {
        let error = io::Error::last_os_error();
        let in_progress = matches!(
            error.raw_os_error(),
            Some(code) if code == libc::EINPROGRESS || code == libc::EAGAIN
        );
        if !in_progress {
            close();
            return Err(error);
        }
        let deadline = std::time::Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                close();
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "Unix socket connect timed out",
                ));
            }
            let timeout_ms = remaining.as_millis().clamp(1, i32::MAX as u128) as libc::c_int;
            let mut pollfd = libc::pollfd {
                fd,
                events: libc::POLLOUT,
                revents: 0,
            };
            let polled = unsafe { libc::poll(&mut pollfd, 1, timeout_ms) };
            if polled < 0 {
                let poll_error = io::Error::last_os_error();
                if poll_error.raw_os_error() == Some(libc::EINTR) {
                    continue;
                }
                close();
                return Err(poll_error);
            }
            if polled == 0 {
                close();
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "Unix socket connect timed out",
                ));
            }
            let mut socket_error: libc::c_int = 0;
            let mut socket_error_len = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
            let getsockopt_result = unsafe {
                libc::getsockopt(
                    fd,
                    libc::SOL_SOCKET,
                    libc::SO_ERROR,
                    &mut socket_error as *mut libc::c_int as *mut libc::c_void,
                    &mut socket_error_len,
                )
            };
            if getsockopt_result < 0 {
                let socket_error = io::Error::last_os_error();
                close();
                return Err(socket_error);
            }
            if socket_error != 0 {
                close();
                return Err(io::Error::from_raw_os_error(socket_error));
            }
            break;
        }
    }
    if unsafe { libc::fcntl(fd, libc::F_SETFL, original_flags) } < 0 {
        let error = io::Error::last_os_error();
        close();
        return Err(error);
    }
    // SAFETY: fd is a connected Unix socket owned exclusively by this function.
    Ok(unsafe { UnixStream::from_raw_fd(fd) })
}

fn socket_request(method: &str, params: Value) -> Value {
    let Some(path) = std::env::var_os("HERDR_SOCKET_PATH") else {
        return command_result(false, "Herdr did not provide its session socket path.");
    };
    let deadline = std::time::Instant::now() + SOCKET_TIMEOUT;
    let id = format!(
        "omni-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default()
    );
    let connect_timeout = deadline.saturating_duration_since(std::time::Instant::now());
    if connect_timeout.is_zero() {
        return command_result(
            false,
            "Herdr API timed out; the action may not have completed.",
        );
    }
    let mut socket = match connect_unix_timeout(Path::new(&path), connect_timeout) {
        Ok(socket) => socket,
        Err(error) => return command_result(false, format!("Cannot reach Herdr: {error}")),
    };
    let write_timeout = deadline.saturating_duration_since(std::time::Instant::now());
    if write_timeout.is_zero() {
        return command_result(
            false,
            "Herdr API timed out; the action may not have completed.",
        );
    }
    let _ = socket.set_write_timeout(Some(write_timeout));
    let request = json!({"id": id, "method": method, "params": params});
    if let Err(error) = writeln!(socket, "{request}") {
        return command_result(false, format!("Cannot reach Herdr: {error}"));
    }
    let mut reader = BufReader::new(socket);
    loop {
        let mut line = Vec::new();
        loop {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                return command_result(
                    false,
                    "Herdr API timed out; the action may not have completed.",
                );
            }
            let _ = reader.get_mut().set_read_timeout(Some(remaining));
            let available = match reader.fill_buf() {
                Ok(available) => {
                    if available.is_empty() {
                        return command_result(
                            false,
                            "Herdr closed the connection before confirming the action.",
                        );
                    }
                    available
                }
                Err(error) if error.kind() == std::io::ErrorKind::TimedOut => {
                    return command_result(
                        false,
                        "Herdr API timed out; the action may not have completed.",
                    )
                }
                Err(_) => return command_result(false, "Herdr returned unreadable API data."),
            };
            if let Some(newline) = available.iter().position(|byte| *byte == b'\n') {
                if line.len() + newline + 1 > MAX_SOCKET_RESPONSE_BYTES {
                    return command_result(false, "Herdr returned an oversized API response.");
                }
                line.extend_from_slice(&available[..=newline]);
                reader.consume(newline + 1);
                break;
            }
            if line.len() + available.len() > MAX_SOCKET_RESPONSE_BYTES {
                return command_result(false, "Herdr returned an oversized API response.");
            }
            line.extend_from_slice(available);
            let consumed = available.len();
            reader.consume(consumed);
        }
        let response = match serde_json::from_slice::<Value>(&line) {
            Ok(response) => response,
            Err(_) => return command_result(false, "Herdr returned unreadable API data."),
        };
        if response.get("id").and_then(Value::as_str) != Some(id.as_str()) {
            continue;
        }
        if let Some(error) = response.get("error").filter(|error| !error.is_null()) {
            return command_result(
                false,
                error
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("Herdr rejected the action."),
            );
        }
        return if response
            .get("result")
            .is_some_and(|result| !result.is_null() && result != &Value::Bool(false))
        {
            command_result(true, "")
        } else {
            command_result(false, "Herdr returned an invalid API response.")
        };
    }
}

fn worktree_create_args(workspace: &str, input: &str) -> Vec<String> {
    let branch = input.trim();
    let mut args = vec!["worktree", "create", "--workspace", workspace]
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<_>>();
    if !branch.is_empty() {
        args.extend(["--branch".to_string(), branch.to_string()]);
    }
    args.push("--focus".to_string());
    args
}

fn worktree_open_args(workspace: &str, input: &str) -> Vec<String> {
    let value = input.trim();
    let flag = if value.starts_with('/') || value.starts_with('~') || value.starts_with('.') {
        "--path"
    } else {
        "--branch"
    };
    let expanded = if value == "~" {
        dirs::home_dir()
            .map(|path| path.to_string_lossy().to_string())
            .unwrap_or_else(|| value.to_string())
    } else if let Some(rest) = value.strip_prefix("~/") {
        dirs::home_dir()
            .map(|path| path.join(rest).to_string_lossy().to_string())
            .unwrap_or_else(|| value.to_string())
    } else {
        value.to_string()
    };
    vec![
        "worktree",
        "open",
        "--workspace",
        workspace,
        flag,
        &expanded,
        "--focus",
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}

fn step_ring(
    ids: &[String],
    current: &str,
    step: i64,
    alone: &str,
    missing: &str,
) -> Result<String, String> {
    if ids.len() < 2 {
        return Err(alone.to_string());
    }
    let Some(index) = ids.iter().position(|id| id == current) else {
        return Err(missing.to_string());
    };
    let next = (index as i64 + step).rem_euclid(ids.len() as i64) as usize;
    Ok(ids[next].clone())
}

fn neighbor(target: &Value, kind: &str, step: i64) -> Result<String, String> {
    let args = match kind {
        "tab" => vec![
            "tab".to_string(),
            "list".to_string(),
            "--workspace".to_string(),
            target_string(target, "workspaceId")?,
        ],
        "workspace" => vec!["workspace".to_string(), "list".to_string()],
        _ => vec!["agent".to_string(), "list".to_string()],
    };
    let response = run_herdr(&args)?;
    let values = match kind {
        "tab" => records(value_at(&response, &["result", "tabs"]))
            .into_iter()
            .filter_map(|v| {
                v.get("tab_id")
                    .and_then(Value::as_str)
                    .map(ToString::to_string)
            })
            .collect::<Vec<_>>(),
        "workspace" => records(value_at(&response, &["result", "workspaces"]))
            .into_iter()
            .filter_map(|v| {
                v.get("workspace_id")
                    .and_then(Value::as_str)
                    .map(ToString::to_string)
            })
            .collect::<Vec<_>>(),
        _ => records(value_at(&response, &["result", "agents"]))
            .into_iter()
            .filter_map(|v| {
                v.get("pane_id")
                    .and_then(Value::as_str)
                    .map(ToString::to_string)
            })
            .collect::<Vec<_>>(),
    };
    if kind == "agent" {
        if values.is_empty() {
            return Err("No agents are running.".to_string());
        }
        let current = target_string(target, "paneId")?;
        if values.len() == 1 {
            return if values[0] == current {
                Err("Only one agent is running.".to_string())
            } else {
                Ok(values[0].clone())
            };
        }
        let Some(index) = values.iter().position(|id| id == &current) else {
            return Ok(if step < 0 {
                values.last().cloned().unwrap_or_default()
            } else {
                values.first().cloned().unwrap_or_default()
            });
        };
        let next = (index as i64 + step).rem_euclid(values.len() as i64) as usize;
        return Ok(values[next].clone());
    }
    let (current, alone, missing) = match kind {
        "tab" => (
            target_string(target, "tabId")?,
            "This workspace only has one tab.",
            "Herdr did not report the tab that opened the palette.",
        ),
        "workspace" => (
            target_string(target, "workspaceId")?,
            "Only one workspace is open.",
            "Herdr did not report the workspace that opened the palette.",
        ),
        _ => unreachable!("agent neighbor handled above"),
    };
    step_ring(&values, &current, step, alone, missing)
}

fn need_input(input: &str, label: &str) -> Result<(), String> {
    if input.trim().is_empty() {
        Err(format!("Enter a {label}."))
    } else {
        Ok(())
    }
}

fn resolve_action(
    action: &str,
    target: &Value,
    step: i64,
    input: &str,
) -> Result<Vec<String>, String> {
    let pane = target_string(target, "paneId")?;
    let tab = target_string(target, "tabId")?;
    let workspace = target_string(target, "workspaceId")?;
    let args: Vec<String> = match action {
        "create-tab" => vec![
            "tab".into(),
            "create".into(),
            "--workspace".into(),
            workspace,
            "--focus".into(),
        ],
        "close-pane" => vec!["pane".into(), "close".into(), pane],
        "close-tab" => vec!["tab".into(), "close".into(), tab],
        "close-workspace" => vec!["workspace".into(), "close".into(), workspace],
        "focus-tab" => vec!["tab".into(), "focus".into(), neighbor(target, "tab", step)?],
        "focus-workspace" => vec![
            "workspace".into(),
            "focus".into(),
            neighbor(target, "workspace", step)?,
        ],
        "focus-agent" => vec![
            "agent".into(),
            "focus".into(),
            neighbor(target, "agent", step)?,
        ],
        "rename-pane" => {
            need_input(input, "pane name")?;
            vec![
                "pane".into(),
                "rename".into(),
                pane,
                input.trim().to_string(),
            ]
        }
        "rename-pane-clear" => vec!["pane".into(), "rename".into(), pane, "--clear".into()],
        "rename-tab" => {
            need_input(input, "tab name")?;
            vec!["tab".into(), "rename".into(), tab, input.trim().to_string()]
        }
        "rename-workspace" => {
            need_input(input, "workspace name")?;
            vec![
                "workspace".into(),
                "rename".into(),
                workspace,
                input.trim().to_string(),
            ]
        }
        "move-pane-new-tab" => vec![
            "pane".into(),
            "move".into(),
            pane,
            "--new-tab".into(),
            "--focus".into(),
        ],
        "move-pane-new-workspace" => vec![
            "pane".into(),
            "move".into(),
            pane,
            "--new-workspace".into(),
            "--focus".into(),
        ],
        "worktree-create" => return Ok(worktree_create_args(&workspace, input)),
        "worktree-open" => {
            need_input(input, "branch or path")?;
            return Ok(worktree_open_args(&workspace, input));
        }
        "worktree-remove" => {
            if !input.trim().eq_ignore_ascii_case("yes") {
                return Err("Type \"yes\" to remove this worktree.".to_string());
            }
            vec![
                "worktree".into(),
                "remove".into(),
                "--workspace".into(),
                workspace,
            ]
        }
        _ => return Err("Herdr did not recognize this action.".to_string()),
    };
    Ok(args)
}

fn clean_text(value: &str) -> String {
    value
        .chars()
        .map(|c| {
            if ('\u{0}'..='\u{1f}').contains(&c) || ('\u{7f}'..='\u{9f}').contains(&c) {
                ' '
            } else {
                c
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn command_result_error(message: String) -> Value {
    command_result(false, message)
}

fn saved_session(value: Option<&Value>) -> Option<(String, String, String, String)> {
    let value = value?.as_object()?;
    let provider = value.get("provider")?.as_str()?.to_string();
    let id = value.get("id")?.as_str()?.to_string();
    let title = value
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let cwd = value
        .get("cwd")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    Some((provider, id, title, cwd))
}

fn valid_session(provider: &str, id: &str) -> bool {
    matches!(provider, "codex" | "claude" | "opencode")
        && !id.is_empty()
        && id.len() <= 200
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        && id.chars().next().is_some_and(|c| c.is_ascii_alphanumeric())
}

fn executable_on_path(name: &str) -> bool {
    if name.contains('/') {
        return Path::new(name).is_file();
    }
    let paths = std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
        .unwrap_or_default();
    paths.into_iter().any(|directory| {
        let candidate = directory.join(name);
        candidate.is_file()
    })
}

fn canonical_path(path: &str) -> Option<PathBuf> {
    if !Path::new(path).is_absolute() {
        return None;
    }
    fs::canonicalize(path)
        .ok()
        .or_else(|| Some(PathBuf::from(path.trim_end_matches('/'))))
}

fn path_is_within(root: &Path, path: &Path) -> bool {
    path == root || path.strip_prefix(root).is_ok()
}

fn find_session_workspace(snapshot: &Value, cwd: &str) -> Option<String> {
    let path = canonical_path(cwd)?;
    let workspaces = records(snapshot.get("workspaces"));
    let ids = workspaces
        .iter()
        .filter_map(|workspace| workspace.get("workspace_id").and_then(Value::as_str))
        .collect::<HashSet<_>>();
    let mut matches = HashMap::<String, usize>::new();
    for workspace in &workspaces {
        let id = text(workspace.get("workspace_id"));
        let checkout = workspace
            .get("worktree")
            .and_then(Value::as_object)
            .and_then(|w| w.get("checkout_path"))
            .and_then(Value::as_str);
        let Some(root) = checkout.and_then(canonical_path) else {
            continue;
        };
        if root == Path::new("/") || !path_is_within(&root, &path) {
            continue;
        }
        matches.insert(id, root.to_string_lossy().len());
    }
    let mut exact = HashSet::new();
    let panes = records(snapshot.get("panes"));
    let agents = records(snapshot.get("agents"));
    for pane in panes.into_iter().chain(agents) {
        let id = text(pane.get("workspace_id"));
        if !ids.contains(id.as_str()) {
            continue;
        }
        for key in ["cwd", "foreground_cwd"] {
            if pane
                .get(key)
                .and_then(Value::as_str)
                .and_then(canonical_path)
                .as_deref()
                == Some(path.as_path())
            {
                exact.insert(id.clone());
            }
        }
    }
    let longest = matches.values().copied().max()?;
    let candidates = if exact.is_empty() {
        matches
            .into_iter()
            .filter(|(_, length)| *length == longest)
            .map(|(id, _)| id)
            .collect::<HashSet<_>>()
    } else {
        exact
    };
    workspaces.into_iter().find_map(|workspace| {
        let id = text(workspace.get("workspace_id"));
        candidates.contains(&id).then_some(id)
    })
}

fn git_worktree_contains(cwd: &str, original: &str) -> bool {
    let mut command = Command::new("git");
    command
        .args(["-C", cwd, "worktree", "list", "--porcelain", "-z"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    start_process_group(&mut command);
    let Ok(mut child) = command.spawn() else {
        return false;
    };
    let Some(stdout) = child.stdout.take() else {
        return false;
    };
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    let reader = thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let mut bytes = Vec::new();
        let mut chunk = [0_u8; 8192];
        let result = loop {
            let count = match reader.read(&mut chunk) {
                Ok(count) => count,
                Err(_) => break Err(()),
            };
            if count == 0 {
                break Ok(bytes);
            }
            if bytes.len() + count > 1024 * 1024 {
                break Err(());
            }
            bytes.extend_from_slice(&chunk[..count]);
        };
        let _ = sender.send(result);
    });
    let started = std::time::Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if started.elapsed() < Duration::from_secs(1) => {
                thread::sleep(Duration::from_millis(5))
            }
            _ => {
                kill_process_group(&mut child);
                let _ = child.wait();
                break None;
            }
        }
    };
    let mut output = receiver.recv_timeout(PIPE_DRAIN_GRACE);
    if output.is_err() {
        kill_process_group(&mut child);
        let _ = child.wait();
        output = receiver.recv_timeout(PIPE_KILL_GRACE);
    }
    if !matches!(output, Err(std::sync::mpsc::RecvTimeoutError::Timeout)) {
        let _ = reader.join();
    }
    let Ok(Ok(output)) = output else {
        return false;
    };
    status.is_some_and(|status| {
        status.success()
            && String::from_utf8_lossy(&output)
                .split('\0')
                .any(|field| field == format!("worktree {original}"))
    })
}

fn resume_workspace_choices(snapshot: &Value, original: &str, current: &str) -> Vec<Value> {
    let workspaces = records(snapshot.get("workspaces"));
    let panes = records(snapshot.get("panes"));
    let agents = records(snapshot.get("agents"));
    let mut choices = Vec::<(i64, Value)>::new();
    for workspace in workspaces {
        let id = text(workspace.get("workspace_id"));
        let label = clean_text(&if trimmed(workspace.get("label")).is_empty() {
            id.clone()
        } else {
            text(workspace.get("label"))
        });
        let mut paths = Vec::new();
        if let Some(path) = workspace
            .get("worktree")
            .and_then(Value::as_object)
            .and_then(|w| w.get("checkout_path"))
            .and_then(Value::as_str)
        {
            paths.push(path.to_string());
        }
        for pane in panes.iter().chain(agents.iter()) {
            if text(pane.get("workspace_id")) == id {
                for key in ["foreground_cwd", "cwd"] {
                    if let Some(path) = pane.get(key).and_then(Value::as_str) {
                        paths.push(path.to_string());
                    }
                }
            }
        }
        let mut best: Option<(i64, Value)> = None;
        let mut seen = HashSet::new();
        for cwd in paths {
            if !seen.insert(cwd.clone())
                || !Path::new(&cwd).is_absolute()
                || !Path::new(&cwd).is_dir()
            {
                continue;
            }
            let same_repository = git_worktree_contains(&cwd, original);
            let name_match = Path::new(&cwd).file_name() == Path::new(original).file_name();
            let score = if same_repository {
                3
            } else if name_match {
                2
            } else if id == current {
                1
            } else {
                0
            };
            let reason = if same_repository {
                "Same Git repository"
            } else if name_match {
                "Matching project folder"
            } else if score > 0 {
                "Current workspace"
            } else {
                "Available directory"
            };
            let choice = json!({"id": id, "label": label, "cwd": cwd, "reason": reason});
            if best.as_ref().is_none_or(|(old, _)| score > *old) {
                best = Some((score, choice));
            }
        }
        if let Some((score, choice)) = best {
            choices.push((score, choice));
        }
    }
    choices.sort_by_key(|(score, _)| std::cmp::Reverse(*score));
    choices.into_iter().map(|(_, choice)| choice).collect()
}

fn focus_pane(pane_id: &str) -> Value {
    socket_request("pane.focus", json!({"pane_id": pane_id}))
}

fn resume_saved_session(
    session: &Value,
    confirmed_workspace: Option<&str>,
    destination: Option<&Value>,
) -> Value {
    let Some((provider, id, title, original_cwd)) = saved_session(Some(session)) else {
        return command_result_error("Invalid saved session identifier.".to_string());
    };
    if !valid_session(&provider, &id) {
        return command_result_error("Invalid saved session identifier.".to_string());
    }
    let snapshot_response = match run_herdr(&["api".into(), "snapshot".into()]) {
        Ok(response) => response,
        Err(_) => {
            return command_result_error(
                "Cannot verify whether this session is already open. Try again.".to_string(),
            )
        }
    };
    let Some(state) = value_at(&snapshot_response, &["result", "snapshot"]) else {
        return command_result_error("Herdr returned an unreadable agent list.".to_string());
    };
    let Some(agents) = state.get("agents").and_then(Value::as_array) else {
        return command_result_error("Herdr returned an unreadable agent list.".to_string());
    };
    for agent in agents.iter().filter_map(Value::as_object) {
        let agent_session = agent.get("agent_session").and_then(Value::as_object);
        let agent_provider = agent_session
            .and_then(|s| s.get("agent"))
            .and_then(Value::as_str)
            .or_else(|| agent.get("agent").and_then(Value::as_str));
        if agent_session
            .and_then(|s| s.get("kind"))
            .and_then(Value::as_str)
            == Some("id")
            && agent_session
                .and_then(|s| s.get("value"))
                .and_then(Value::as_str)
                == Some(id.as_str())
            && agent_provider == Some(provider.as_str())
        {
            if let Some(pane) = agent.get("pane_id").and_then(Value::as_str) {
                return focus_pane(pane);
            }
        }
    }
    if !executable_on_path(&provider) {
        return command_result_error(format!("{provider} is not installed or not on PATH."));
    }
    let original_exists = Path::new(&original_cwd).is_dir();
    let Some(target) = session_target() else {
        return command_result_error("Cannot identify the workspace that opened Omni.".to_string());
    };
    let target_workspace = match target_string(&target, "workspaceId") {
        Ok(value) => value,
        Err(error) => return command_result_error(error),
    };
    let mut cwd = original_cwd.clone();
    let mut workspace_id = find_session_workspace(state, &original_cwd);
    if !original_exists || destination.is_some() {
        let choices = resume_workspace_choices(state, &original_cwd, &target_workspace);
        let chosen = destination.and_then(|destination| {
            let id = destination.get("id").and_then(Value::as_str)?;
            let path = destination.get("cwd").and_then(Value::as_str)?;
            choices.iter().find(|choice| {
                choice.get("id").and_then(Value::as_str) == Some(id)
                    && choice.get("cwd").and_then(Value::as_str) == Some(path)
            })
        });
        let Some(chosen) = chosen else {
            let message = if choices.is_empty() {
                "No available workspace directory found. Open a workspace with an existing directory and try again."
            } else {
                "Original directory unavailable. Choose a workspace directory. Deleted files and Git changes will not be restored."
            };
            let mut result = command_result_error(message.to_string());
            if !choices.is_empty() {
                result["workspaceChoices"] = Value::Array(choices);
            }
            return result;
        };
        workspace_id = chosen
            .get("id")
            .and_then(Value::as_str)
            .map(ToString::to_string);
        cwd = chosen
            .get("cwd")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
    }
    let workspace_id = match workspace_id {
        Some(workspace_id) => workspace_id,
        None => {
            let current = records(state.get("workspaces"))
                .into_iter()
                .find(|workspace| text(workspace.get("workspace_id")) == target_workspace);
            let Some(current) = current else {
                return command_result_error(
                    "The workspace that opened Omni is no longer available.".to_string(),
                );
            };
            if confirmed_workspace != Some(target_workspace.as_str()) {
                let label = clean_text(&if trimmed(current.get("label")).is_empty() {
                    target_workspace.clone()
                } else {
                    text(current.get("label"))
                });
                let mut result = command_result(false, format!("No workspace matches {original_cwd}. Resume in current workspace “{label}”? The session will keep its original project directory."));
                result["confirmWorkspace"] = json!({"id": target_workspace, "label": label});
                return result;
            }
            target_workspace.clone()
        }
    };
    let label = clean_text(&title);
    let label = if label.len() > 100 {
        label.chars().take(100).collect::<String>()
    } else if label.is_empty() {
        provider.clone()
    } else {
        label
    };
    let created = run_herdr(&[
        "tab".into(),
        "create".into(),
        "--workspace".into(),
        workspace_id.clone(),
        "--cwd".into(),
        cwd.clone(),
        "--label".into(),
        label,
        "--no-focus".into(),
    ]);
    let created = match created {
        Ok(value) => value,
        Err(error) => return command_result_error(error),
    };
    let Some(pane) =
        value_at(&created, &["result", "root_pane", "pane_id"]).and_then(Value::as_str)
    else {
        return command_result_error("A tab was created, but Herdr did not return its pane. Check the new tab before retrying.".to_string());
    };
    let mut agent_args = match provider.as_str() {
        "codex" => vec!["resume".to_string(), id.clone()],
        "opencode" => vec!["--session".to_string(), id.clone()],
        _ => vec!["--resume".to_string(), id.clone()],
    };
    if provider == "codex" && destination.is_some() {
        agent_args.extend(["--cd".to_string(), cwd]);
    }
    let mut args = vec![
        "agent".into(),
        "start".into(),
        format!("omni-{}", unique_suffix()),
        "--kind".into(),
        provider,
        "--pane".into(),
        pane.to_string(),
        "--".into(),
    ];
    args.extend(agent_args);
    match run_herdr(&args) {
        Ok(_) => focus_pane(pane),
        Err(error) => command_result_error(format!(
            "Resume could not be confirmed in {pane}. Check the new tab before retrying: {error}"
        )),
    }
}

fn unique_suffix() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    format!("{:08x}", nanos & 0xffff_ffff)
}

/// Execute one palette item. The returned value is the JSON CommandResult
/// consumed by the Rust UI (`ok`, `message`, and optional confirmation data).
pub fn execute(item: &Value, input: &str) -> Value {
    let Some(invocation) = item.get("invocation").and_then(Value::as_object) else {
        return command_result_error("Invalid palette item invocation.".to_string());
    };
    match invocation
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or_default()
    {
        "resume-session" => {
            if invocation
                .get("fallbackWorkspaceId")
                .and_then(Value::as_str)
                .is_some()
                && !input.trim().eq_ignore_ascii_case("yes")
            {
                return command_result(
                    false,
                    "Type \"yes\" to resume here, or press Esc to cancel.",
                );
            }
            resume_saved_session(
                invocation.get("session").unwrap_or(&Value::Null),
                invocation
                    .get("fallbackWorkspaceId")
                    .and_then(Value::as_str),
                invocation.get("destination"),
            )
        }
        "pane-api" => {
            let Some(target) = session_target() else {
                return command_result_error(
                    "Herdr did not report the pane that opened the palette.".to_string(),
                );
            };
            let pane_id = match target_string(&target, "paneId") {
                Ok(value) => value,
                Err(error) => return command_result_error(error),
            };
            socket_request(
                invocation
                    .get("method")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
                json!({"pane_id": pane_id}),
            )
        }
        "shortcut" => {
            let keys = item
                .get("shortcuts")
                .and_then(Value::as_array)
                .map(|keys| {
                    keys.iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(" / ")
                })
                .unwrap_or_default();
            let limitation = "Herdr does not expose this client action to plugins.";
            command_result(
                false,
                if keys.is_empty() {
                    format!("{limitation} Configure its Herdr keybinding first.")
                } else {
                    format!("Close Omni (Esc), then press {keys}. {limitation}")
                },
            )
        }
        "herdr" => {
            let mut args = invocation
                .get("argv")
                .and_then(Value::as_array)
                .map(|args| {
                    args.iter()
                        .filter_map(Value::as_str)
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            if args.first().map(String::as_str) == Some("pane")
                && args.iter().any(|arg| arg == "--current")
            {
                let Some(target) = session_target() else {
                    return command_result_error(
                        "Herdr did not report the pane that opened the palette.".to_string(),
                    );
                };
                let pane_id = match target_string(&target, "paneId") {
                    Ok(value) => value,
                    Err(error) => return command_result_error(error),
                };
                args = args
                    .into_iter()
                    .flat_map(|arg| {
                        if arg == "--current" {
                            vec!["--pane".to_string(), pane_id.clone()]
                        } else {
                            vec![arg]
                        }
                    })
                    .collect();
            }
            if args.first().map(String::as_str) == Some("agent")
                && args.get(1).map(String::as_str) == Some("focus")
            {
                return focus_pane(args.get(2).map(String::as_str).unwrap_or_default());
            }
            match run_herdr(&args) {
                Ok(_) => command_result(true, ""),
                Err(error) => command_result_error(error),
            }
        }
        "resolve" => {
            let Some(target) = session_target() else {
                return command_result_error(
                    "Herdr did not report the pane that opened the palette.".to_string(),
                );
            };
            let action = invocation
                .get("action")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let step = invocation.get("step").and_then(Value::as_i64).unwrap_or(1);
            let args = match resolve_action(action, &target, step, input) {
                Ok(args) => args,
                Err(error) => return command_result_error(error),
            };
            if args.first().map(String::as_str) == Some("agent")
                && args.get(1).map(String::as_str) == Some("focus")
            {
                return focus_pane(args.get(2).map(String::as_str).unwrap_or_default());
            }
            match run_herdr(&args) {
                Ok(_) => command_result(true, ""),
                Err(error) => command_result_error(error),
            }
        }
        _ => command_result_error("Invalid palette item invocation.".to_string()),
    }
}

fn display_number(value: Option<&Value>) -> String {
    match value {
        Some(Value::Number(number)) => number.to_string(),
        _ => "0".to_string(),
    }
}

fn status(value: Option<&Value>) -> Option<String> {
    let status = text(value);
    ["blocked", "done", "working", "idle", "unknown"]
        .contains(&status.as_str())
        .then_some(status)
}

fn useful(value: Option<&Value>) -> String {
    trimmed(value)
}

fn unique_strings(values: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut seen = HashSet::new();
    values
        .into_iter()
        .filter(|value| !value.is_empty() && seen.insert(value.clone()))
        .collect()
}

fn focused_by_tab(snapshot: &Value) -> HashMap<String, String> {
    records(snapshot.get("layouts"))
        .into_iter()
        .filter_map(|layout| {
            let tab = text(layout.get("tab_id"));
            let pane = text(layout.get("focused_pane_id"));
            (!tab.is_empty() && !pane.is_empty()).then_some((tab, pane))
        })
        .collect()
}

fn pane_records<'a>(
    panes: Vec<&'a Map<String, Value>>,
    agents: Vec<&'a Map<String, Value>>,
) -> Vec<&'a Map<String, Value>> {
    let source = if panes.is_empty() { agents } else { panes };
    let mut seen = HashSet::new();
    source
        .into_iter()
        .filter(|pane| {
            let id = text(pane.get("pane_id"));
            !id.is_empty() && seen.insert(id)
        })
        .collect()
}

fn preview_pane(
    pane: &Map<String, Value>,
    agent: Option<&Map<String, Value>>,
    focus: &HashMap<String, String>,
) -> Value {
    let id = {
        let value = text(pane.get("pane_id"));
        if value.is_empty() {
            text(agent.and_then(|a| a.get("pane_id")))
        } else {
            value
        }
    };
    let known_agent = [
        useful(pane.get("agent")),
        useful(pane.get("display_agent")),
        useful(agent.and_then(|a| a.get("agent"))),
        useful(agent.and_then(|a| a.get("display_agent"))),
    ]
    .into_iter()
    .find(|value| !value.is_empty())
    .unwrap_or_default();
    let label = [
        useful(pane.get("label")),
        useful(pane.get("terminal_title_stripped")),
        useful(agent.and_then(|a| a.get("label"))),
        useful(agent.and_then(|a| a.get("terminal_title_stripped"))),
        known_agent.clone(),
        id.clone(),
    ]
    .into_iter()
    .find(|value| !value.is_empty())
    .unwrap_or_default();
    let tab_id = {
        let value = text(pane.get("tab_id"));
        if value.is_empty() {
            text(agent.and_then(|a| a.get("tab_id")))
        } else {
            value
        }
    };
    let known_status = status(pane.get("agent_status"))
        .or_else(|| status(agent.and_then(|a| a.get("agent_status"))));
    let cwd = [
        text(pane.get("foreground_cwd")),
        text(pane.get("cwd")),
        text(agent.and_then(|a| a.get("foreground_cwd"))),
        text(agent.and_then(|a| a.get("cwd"))),
    ]
    .into_iter()
    .find(|value| !value.is_empty())
    .unwrap_or_default();
    let focused = focus
        .get(&tab_id)
        .map(|value| value == &id)
        .unwrap_or_else(|| {
            pane.get("focused")
                .and_then(Value::as_bool)
                .unwrap_or(false)
                || agent
                    .and_then(|a| a.get("focused"))
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
        });
    let mut result = json!({"id": id, "label": label, "cwd": cwd, "focused": focused});
    if !known_agent.is_empty() {
        result["agent"] = Value::String(known_agent);
    }
    if let Some(status) = known_status {
        result["status"] = Value::String(status);
    }
    result
}

fn preview_panes(
    source: &[&Map<String, Value>],
    agents: &HashMap<String, &Map<String, Value>>,
    workspace: &str,
    tab: Option<&str>,
    focus: &HashMap<String, String>,
) -> Vec<Value> {
    source
        .iter()
        .filter(|pane| {
            text(pane.get("workspace_id")) == workspace
                && tab
                    .map(|tab| text(pane.get("tab_id")) == tab)
                    .unwrap_or(true)
        })
        .map(|pane| preview_pane(pane, agents.get(&text(pane.get("pane_id"))).copied(), focus))
        .collect()
}

fn workspace_paths(source: &[&Map<String, Value>], workspace: &str) -> Vec<String> {
    unique_strings(
        source
            .iter()
            .filter(|pane| text(pane.get("workspace_id")) == workspace)
            .flat_map(|pane| [text(pane.get("foreground_cwd")), text(pane.get("cwd"))]),
    )
}

#[allow(clippy::too_many_arguments)]
fn live_item(
    id: &str,
    title: String,
    category: &str,
    description: String,
    icon: &str,
    aliases: Vec<String>,
    invocation: Value,
    search_title: String,
    search_paths: Vec<String>,
) -> Value {
    json!({"id": format!("live:{id}"), "title": title, "category": category, "description": description, "icon": icon, "aliases": unique_strings(aliases), "searchTitle": search_title, "searchPaths": unique_strings(search_paths), "shortcuts": [], "invocation": invocation})
}

fn history_path() -> PathBuf {
    if let Ok(path) = std::env::var("HERDR_CONFIG_PATH") {
        return Path::new(&path)
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("herdr-server.log");
    }
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")))
        .join(".config")
        .join("herdr")
        .join("herdr-server.log")
}

fn workspace_visits() -> HashMap<String, i64> {
    let path = history_path();
    let Ok(metadata) = fs::metadata(&path) else {
        return HashMap::new();
    };
    let Ok(mut file) = fs::File::open(&path) else {
        return HashMap::new();
    };
    let start = metadata.len().saturating_sub(MAX_SERVER_LOG_CHARS as u64);
    if file.seek(SeekFrom::Start(start)).is_err() {
        return HashMap::new();
    }
    let mut bytes = Vec::with_capacity((metadata.len() - start) as usize);
    if file.read_to_end(&mut bytes).is_err() {
        return HashMap::new();
    }
    if start > 0 {
        if let Some(newline) = bytes.iter().position(|byte| *byte == b'\n') {
            bytes.drain(..=newline);
        } else {
            bytes.clear();
        }
    }
    let source = String::from_utf8_lossy(&bytes);
    let mut visits = HashMap::new();
    for line in source.lines() {
        if !line.contains("event=\"workspace.focus\"") || !line.contains("outcome=\"ok\"") {
            continue;
        }
        let timestamp = line.split_whitespace().next().unwrap_or_default();
        let Ok(timestamp) = DateTime::parse_from_rfc3339(timestamp) else {
            continue;
        };
        let timestamp = timestamp.timestamp_millis();
        if timestamp <= 0 {
            continue;
        }
        let Some(start) = line.find("workspace_id=\"") else {
            continue;
        };
        let rest = &line[start + "workspace_id=\"".len()..];
        let Some(end) = rest.find('"') else {
            continue;
        };
        let id = &rest[..end];
        if visits.get(id).copied().unwrap_or_default() < timestamp {
            visits.insert(id.to_string(), timestamp);
        }
    }
    visits
}

fn items_from_snapshot(snapshot: &Value, current_workspace: &str) -> Vec<Value> {
    let visits = workspace_visits();
    items_from_snapshot_with_visits(snapshot, current_workspace, &visits)
}

fn items_from_snapshot_with_visits(
    snapshot: &Value,
    current_workspace: &str,
    visits: &HashMap<String, i64>,
) -> Vec<Value> {
    let workspaces = records(snapshot.get("workspaces"));
    let tabs = records(snapshot.get("tabs"));
    let agents = records(snapshot.get("agents"));
    let source_panes = pane_records(records(snapshot.get("panes")), agents.clone());
    let agents_by_pane: HashMap<String, &Map<String, Value>> = agents
        .iter()
        .filter_map(|agent| {
            let id = text(agent.get("pane_id"));
            (!id.is_empty()).then_some((id, *agent))
        })
        .collect();
    let focus = focused_by_tab(snapshot);
    let workspace_labels: HashMap<String, String> = workspaces
        .iter()
        .map(|workspace| {
            (
                text(workspace.get("workspace_id")),
                text(workspace.get("label")),
            )
        })
        .collect();
    let tab_labels: HashMap<String, String> = tabs
        .iter()
        .map(|tab| (text(tab.get("tab_id")), text(tab.get("label"))))
        .collect();
    let mut result = Vec::new();
    for workspace in workspaces {
        let workspace_id = text(workspace.get("workspace_id"));
        let label = {
            let value = text(workspace.get("label"));
            if value.is_empty() {
                workspace_id.clone()
            } else {
                value
            }
        };
        let worktree = workspace.get("worktree").and_then(Value::as_object);
        let checkout = text(worktree.and_then(|w| w.get("checkout_path")));
        let branch = useful(worktree.and_then(|w| w.get("branch")));
        let tabs_preview = tabs.iter().filter(|tab| text(tab.get("workspace_id")) == workspace_id).map(|tab| {
            let id = text(tab.get("tab_id"));
            let tab_label = { let value = useful(tab.get("label")); if value.is_empty() { id.clone() } else { value } };
            json!({"id": id, "label": tab_label, "panes": preview_panes(&source_panes, &agents_by_pane, &workspace_id, Some(&id), &focus)})
        }).collect::<Vec<_>>();
        let pane_count = workspace
            .get("pane_count")
            .and_then(Value::as_i64)
            .map(|v| v.max(0))
            .unwrap_or_else(|| {
                preview_panes(&source_panes, &agents_by_pane, &workspace_id, None, &focus).len()
                    as i64
            });
        let paths = if !checkout.is_empty() {
            vec![checkout.clone()]
        } else {
            workspace_paths(&source_panes, &workspace_id)
        };
        let mut resource = json!({"kind": "workspace", "workspaceId": workspace_id, "paths": paths, "tabs": tabs_preview, "paneCount": pane_count});
        if !branch.is_empty() {
            resource["branch"] = Value::String(branch);
        }
        let details = format!(
            "{} tabs · {} panes",
            display_number(workspace.get("tab_count")),
            display_number(workspace.get("pane_count"))
        );
        let description = if checkout.is_empty() {
            details
        } else {
            format!("{details} · {checkout}")
        };
        let repo = text(worktree.and_then(|w| w.get("repo_name")));
        let mut item = live_item(
            &format!("workspace:{workspace_id}"),
            label.clone(),
            "Workspace",
            description,
            "◇",
            vec![repo],
            json!({"kind": "herdr", "argv": ["workspace", "focus", workspace_id]}),
            text(workspace.get("label")),
            if checkout.is_empty() {
                Vec::new()
            } else {
                vec![checkout]
            },
        );
        item["resourcePreview"] = resource;
        item["currentWorkspace"] = Value::Bool(workspace_id == current_workspace);
        if let Some(visited) = visits.get(&workspace_id) {
            item["lastVisitedAt"] = json!(*visited);
        }
        result.push(item);
    }
    for tab in tabs {
        let label = useful(tab.get("label"));
        if label.is_empty() || label.chars().all(char::is_numeric) {
            continue;
        }
        let tab_id = text(tab.get("tab_id"));
        let workspace_id = text(tab.get("workspace_id"));
        let workspace = workspace_labels
            .get(&workspace_id)
            .cloned()
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| workspace_id.clone());
        let title = [workspace.clone(), label.clone()]
            .into_iter()
            .filter(|v| !v.is_empty())
            .collect::<Vec<_>>()
            .join(" → ");
        let search_title = [
            workspace_labels
                .get(&workspace_id)
                .cloned()
                .unwrap_or_default(),
            label.clone(),
        ]
        .into_iter()
        .filter(|v| !v.is_empty())
        .collect::<Vec<_>>()
        .join(" → ");
        let mut item = live_item(
            &format!("tab:{tab_id}"),
            title,
            "Tabs",
            format!(
                "{} panes · {}",
                display_number(tab.get("pane_count")),
                text(tab.get("agent_status"))
            ),
            "▣",
            Vec::new(),
            json!({"kind": "herdr", "argv": ["tab", "focus", tab_id]}),
            search_title,
            Vec::new(),
        );
        let tab_workspace_id = text(tab.get("workspace_id"));
        let tab_id_for_preview = text(tab.get("tab_id"));
        item["resourcePreview"] = json!({"kind": "tab", "workspaceId": tab_workspace_id.clone(), "workspaceLabel": workspace, "panes": preview_panes(&source_panes, &agents_by_pane, &tab_workspace_id, Some(&tab_id_for_preview), &focus)});
        result.push(item);
    }
    for agent in agents {
        let pane_id = text(agent.get("pane_id"));
        let workspace_id = text(agent.get("workspace_id"));
        let tab_id = text(agent.get("tab_id"));
        let kind = {
            let value = text(agent.get("display_agent"));
            if value.is_empty() {
                let value = text(agent.get("agent"));
                if value.is_empty() {
                    "Agent".to_string()
                } else {
                    value
                }
            } else {
                value
            }
        };
        let session_name = [
            text(agent.get("title")),
            text(agent.get("terminal_title_stripped")),
            text(agent.get("name")),
            kind.clone(),
        ]
        .into_iter()
        .find(|v| !v.is_empty())
        .unwrap_or(kind.clone());
        let workspace = workspace_labels
            .get(&workspace_id)
            .cloned()
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| workspace_id.clone());
        let agent_status = text(agent.get("agent_status"));
        let mut item = live_item(
            &format!("agent:{pane_id}"),
            format!("{session_name} - {workspace}"),
            "Agents",
            format!(
                "{agent_status} · {}",
                tab_labels.get(&tab_id).cloned().unwrap_or(tab_id)
            ),
            "◈",
            vec![kind.clone()],
            json!({"kind": "herdr", "argv": ["agent", "focus", pane_id]}),
            [
                session_name.clone(),
                workspace_labels
                    .get(&workspace_id)
                    .cloned()
                    .unwrap_or_default(),
            ]
            .into_iter()
            .filter(|value| !value.is_empty())
            .collect::<Vec<_>>()
            .join(" - "),
            vec![text(agent.get("cwd"))],
        );
        let priority = ["blocked", "done", "working", "idle", "unknown"]
            .iter()
            .position(|value| *value == agent_status)
            .unwrap_or(4);
        item["priority"] = json!(priority);
        item["agentStatus"] = json!(["blocked", "done", "working", "idle", "unknown"][priority]);
        if !pane_id.is_empty() {
            item["livePaneId"] = Value::String(pane_id);
        }
        item["currentWorkspace"] = Value::Bool(workspace_id == current_workspace);
        if let Some(session) = agent.get("agent_session").and_then(Value::as_object) {
            let provider = {
                let value = text(session.get("agent"));
                if value.is_empty() {
                    text(agent.get("agent"))
                } else {
                    value
                }
            };
            let session_id = text(session.get("value"));
            if session.get("kind").and_then(Value::as_str) == Some("id")
                && matches!(provider.as_str(), "codex" | "claude" | "opencode")
                && !session_id.is_empty()
            {
                item["session"] = json!({"provider": provider, "id": session_id, "title": session_name, "cwd": text(agent.get("cwd")), "updatedAt": 0});
            }
        }
        result.push(item);
    }
    result
}

fn items_from_worktrees(worktrees: Option<&Value>, current_workspace: &str) -> Vec<Value> {
    records(worktrees).into_iter().filter(|worktree| text(worktree.get("open_workspace_id")).is_empty()).map(|worktree| {
        let path = text(worktree.get("path"));
        let branch = useful(worktree.get("branch"));
        let label = { let value = text(worktree.get("label")); if !value.is_empty() { value } else if !branch.is_empty() { branch.clone() } else { Path::new(&path).file_name().and_then(|v| v.to_str()).unwrap_or(&path).to_string() } };
        let mut item = live_item(&format!("workspace:worktree:{path}"), label.clone(), "Workspace", format!("{} · {path}", if branch.is_empty() { "detached" } else { &branch }), "◇", vec![branch.clone()], json!({"kind": "herdr", "argv": ["worktree", "open", "--workspace", current_workspace, "--path", path, "--focus"]}), label, vec![path.clone()]);
        item["resourcePreview"] = if branch.is_empty() { json!({"kind": "worktree", "path": path}) } else { json!({"kind": "worktree", "path": path, "branch": branch}) };
        item
    }).collect()
}

/// Fetch live Herdr workspaces, tabs, agents, and unopened worktrees.
pub fn load_live_items() -> Result<Vec<Value>, String> {
    load_live_items_with_cancel(None)
}

/// Fetch live resources while allowing a caller to terminate both Herdr
/// subprocesses when the selected palette request becomes stale.
pub fn load_live_items_cancellable(cancel: &AtomicBool) -> Result<Vec<Value>, String> {
    load_live_items_with_cancel(Some(cancel))
}

fn load_live_items_with_cancel(cancel: Option<&AtomicBool>) -> Result<Vec<Value>, String> {
    let snapshot_response = run_herdr_cancellable(&["api".into(), "snapshot".into()], cancel)?;
    let snapshot = value_at(&snapshot_response, &["result", "snapshot"])
        .ok_or_else(|| "Herdr returned an unreadable session snapshot.".to_string())?;
    if !snapshot.get("workspaces").is_some_and(Value::is_array) {
        return Err("Herdr returned an unreadable session snapshot.".to_string());
    }
    let current_workspace = launch_context_value()
        .and_then(|value| {
            value
                .get("workspaceId")
                .and_then(Value::as_str)
                .map(ToString::to_string)
        })
        .or_else(|| {
            let value = text(snapshot.get("focused_workspace_id"));
            (!value.is_empty()).then_some(value)
        })
        .unwrap_or_default();
    let mut items = items_from_snapshot(snapshot, &current_workspace);
    if cancel.is_some_and(|cancel| cancel.load(Ordering::Relaxed)) {
        return Err("Live item loading cancelled.".to_string());
    }
    if !current_workspace.is_empty() {
        match run_herdr_cancellable(
            &[
                "worktree".into(),
                "list".into(),
                "--workspace".into(),
                current_workspace.clone(),
            ],
            cancel,
        ) {
            Ok(worktrees) => {
                let additions = items_from_worktrees(
                    value_at(&worktrees, &["result", "worktrees"]),
                    &current_workspace,
                );
                let mut seen = items
                    .iter()
                    .map(|item| text(item.get("id")))
                    .collect::<HashSet<_>>();
                for item in additions {
                    if seen.insert(text(item.get("id"))) {
                        items.push(item);
                    }
                }
            }
            Err(error) if cancel.is_some_and(|cancel| cancel.load(Ordering::Relaxed)) => {
                return Err(error);
            }
            Err(_) => {}
        }
    }
    Ok(items)
}

fn run_raw(args: &[String], timeout: Duration, max_bytes: usize) -> Result<(i32, Vec<u8>), String> {
    run_raw_cancellable(args, timeout, max_bytes, None)
}

fn run_raw_cancellable(
    args: &[String],
    timeout: Duration,
    max_bytes: usize,
    cancel: Option<&AtomicBool>,
) -> Result<(i32, Vec<u8>), String> {
    let mut command = Command::new(binary());
    command
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    start_process_group(&mut command);
    let mut child = command
        .spawn()
        .map_err(|_| "Herdr response unavailable.".to_string())?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "Herdr response unavailable.".to_string())?;
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    let reader = std::thread::spawn(move || {
        let mut reader = std::io::BufReader::new(stdout);
        let mut data = Vec::new();
        let mut buffer = [0_u8; 8192];
        let result = loop {
            let count = match reader.read(&mut buffer) {
                Ok(count) => count,
                Err(_) => break Err("read".to_string()),
            };
            if count == 0 {
                break Ok(data);
            }
            if data.len() + count > max_bytes {
                break Err("oversized".to_string());
            }
            data.extend_from_slice(&buffer[..count]);
        };
        let _ = sender.send(result);
    });
    let started = std::time::Instant::now();
    loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|_| "Herdr response unavailable.".to_string())?
        {
            let mut data = receiver.recv_timeout(PIPE_DRAIN_GRACE);
            if data.is_err() {
                kill_process_group(&mut child);
                let _ = child.wait();
                data = receiver.recv_timeout(PIPE_KILL_GRACE);
            }
            let _ = reader.join();
            let data = data.map_err(|_| "Herdr response unavailable.".to_string())??;
            return Ok((status.code().unwrap_or(-1), data));
        }
        if started.elapsed() >= timeout {
            kill_process_group(&mut child);
            let _ = child.wait();
            return Err("timed out".to_string());
        }
        if cancel.is_some_and(|cancel| cancel.load(Ordering::Relaxed)) {
            kill_process_group(&mut child);
            let _ = child.wait();
            return Err("cancelled".to_string());
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn strip_terminal_sequences(text: &[u8]) -> String {
    let chars = String::from_utf8_lossy(text).chars().collect::<Vec<_>>();
    let mut clean = String::new();
    let mut index = 0;
    while index < chars.len() {
        let code = chars[index] as u32;
        if code == 0x1b {
            let next = chars.get(index + 1).map(|c| *c as u32);
            if next == Some(0x5b) {
                index += 2;
                while index < chars.len() {
                    let c = chars[index] as u32;
                    index += 1;
                    if (0x40..=0x7e).contains(&c) {
                        break;
                    }
                }
                continue;
            }
            if matches!(next, Some(0x5d | 0x50 | 0x58 | 0x5e | 0x5f)) {
                index += 2;
                while index < chars.len() {
                    let c = chars[index] as u32;
                    index += 1;
                    if c == 0x07 || c == 0x9c {
                        break;
                    }
                    if c == 0x1b && chars.get(index).map(|c| *c as u32) == Some(0x5c) {
                        index += 1;
                        break;
                    }
                }
                continue;
            }
            index += 1;
            while index < chars.len() {
                let c = chars[index] as u32;
                index += 1;
                if (0x30..=0x7e).contains(&c) {
                    break;
                }
                if !(0x20..=0x2f).contains(&c) {
                    break;
                }
            }
            continue;
        }
        if code == 0x9b {
            index += 1;
            while index < chars.len() {
                let c = chars[index] as u32;
                index += 1;
                if (0x40..=0x7e).contains(&c) {
                    break;
                }
            }
            continue;
        }
        if matches!(code, 0x9d | 0x90 | 0x98 | 0x9e | 0x9f) {
            index += 1;
            while index < chars.len() {
                let c = chars[index] as u32;
                index += 1;
                if c == 0x07 || c == 0x9c {
                    break;
                }
                if c == 0x1b && chars.get(index).map(|c| *c as u32) == Some(0x5c) {
                    index += 1;
                    break;
                }
            }
            continue;
        }
        if code == 0x0a
            || code == 0x09
            || (code >= 0x20 && code != 0x7f && !(0x80..=0x9f).contains(&code))
        {
            clean.push(chars[index]);
        }
        index += 1;
    }
    clean
}

fn preview_pane_id(item: &Value, pane_index: usize) -> Option<String> {
    if let Some(id) = item
        .get("livePaneId")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
    {
        return Some(id.to_string());
    }
    let resource = item.get("resourcePreview")?.as_object()?;
    match resource.get("kind").and_then(Value::as_str) {
        Some("tab") => resource
            .get("panes")?
            .as_array()?
            .get(pane_index)?
            .get("id")
            .and_then(Value::as_str)
            .map(ToString::to_string),
        Some("workspace") => resource
            .get("tabs")?
            .as_array()?
            .iter()
            .flat_map(|tab| {
                tab.get("panes")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
            })
            .nth(pane_index)?
            .get("id")
            .and_then(Value::as_str)
            .map(ToString::to_string),
        _ => None,
    }
}

/// Read the selected live pane without changing Herdr focus.
pub fn preview(item: &Value, pane_index: usize) -> Result<String, String> {
    if let Some(resource) = item.get("resourcePreview") {
        let kind = resource.get("kind").and_then(Value::as_str);
        if kind == Some("worktree") {
            let path = clean_text(&text(resource.get("path")));
            let branch = resource
                .get("branch")
                .and_then(Value::as_str)
                .map(clean_text)
                .unwrap_or_else(|| "unavailable".to_string());
            return Ok(format!(
                "Path: {}\nBranch: {}\n\nNot open in a workspace.\nEnter opens this worktree.",
                if path.is_empty() {
                    "unavailable"
                } else {
                    &path
                },
                branch
            ));
        }
        if kind == Some("workspace") {
            let paths = resource
                .get("paths")
                .and_then(Value::as_array)
                .map(|paths| {
                    paths
                        .iter()
                        .filter_map(Value::as_str)
                        .map(clean_text)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let path_text = if paths.len() == 1 {
                format!("Path: {}", paths[0])
            } else if paths.is_empty() {
                "Paths:\n  unavailable".to_string()
            } else {
                format!(
                    "Paths:\n{}",
                    paths
                        .iter()
                        .map(|path| format!("  {path}"))
                        .collect::<Vec<_>>()
                        .join("\n")
                )
            };
            let tabs = resource
                .get("tabs")
                .and_then(Value::as_array)
                .filter(|tabs| !tabs.is_empty())
                .map(|tabs| {
                    tabs.iter()
                        .map(|tab| {
                            let label = clean_text(&text(tab.get("label")));
                            let panes = tab
                                .get("panes")
                                .and_then(Value::as_array)
                                .cloned()
                                .unwrap_or_default();
                            let mut lines = vec![format!("▣ {label} · {} panes", panes.len())];
                            for pane in &panes {
                                let mut pane_label = clean_text(&text(pane.get("label")));
                                if let Some(agent) = pane
                                    .get("agent")
                                    .and_then(Value::as_str)
                                    .filter(|agent| !agent.is_empty())
                                {
                                    let status = pane
                                        .get("status")
                                        .and_then(Value::as_str)
                                        .unwrap_or("unknown");
                                    pane_label.push_str(&format!(
                                        " · {} [{}]",
                                        clean_text(agent),
                                        status
                                    ));
                                }
                                lines.push(format!("  {pane_label}"));
                            }
                            if panes.is_empty() {
                                lines.push("  Pane details unavailable".to_string());
                            }
                            lines.join("\n")
                        })
                        .collect::<Vec<_>>()
                        .join("\n\n")
                })
                .unwrap_or_else(|| "No tabs available".to_string());
            let branch = resource
                .get("branch")
                .and_then(Value::as_str)
                .map(clean_text)
                .unwrap_or_else(|| "unavailable".to_string());
            return Ok(format!("Branch: {}\n{}\n\n{}", branch, path_text, tabs));
        }
        if kind == Some("tab")
            && resource
                .get("panes")
                .and_then(Value::as_array)
                .is_none_or(|panes| panes.is_empty())
        {
            return Err("No pane details available".to_string());
        }
    }
    let pane =
        preview_pane_id(item, pane_index).ok_or_else(|| "Pane preview unavailable.".to_string())?;
    let args = vec![
        "pane".into(),
        "read".into(),
        pane,
        "--source".into(),
        "visible".into(),
        "--format".into(),
        "text".into(),
    ];
    let (code, output) =
        run_raw(&args, Duration::from_secs(3), MAX_PREVIEW_BYTES).map_err(|error| {
            if error == "timed out" {
                "Pane preview unavailable (timed out).".to_string()
            } else if error == "oversized" {
                "Pane preview unavailable (response too large).".to_string()
            } else {
                "Pane preview unavailable.".to_string()
            }
        })?;
    if code != 0 {
        return Err("Pane preview unavailable.".to_string());
    }
    Ok(strip_terminal_sequences(&output))
}

/// Cancellation-aware live pane preview used when selection or visibility changes.
pub fn preview_cancellable(
    item: &Value,
    pane_index: usize,
    cancel: &AtomicBool,
) -> Result<String, String> {
    if item.get("resourcePreview").is_some_and(|resource| {
        matches!(
            resource.get("kind").and_then(Value::as_str),
            Some("workspace" | "worktree")
        )
    }) {
        return preview(item, pane_index);
    }
    let pane =
        preview_pane_id(item, pane_index).ok_or_else(|| "Pane preview unavailable.".to_string())?;
    let args = vec![
        "pane".into(),
        "read".into(),
        pane,
        "--source".into(),
        "visible".into(),
        "--format".into(),
        "text".into(),
    ];
    let (code, output) = run_raw_cancellable(
        &args,
        Duration::from_secs(3),
        MAX_PREVIEW_BYTES,
        Some(cancel),
    )
    .map_err(|error| match error.as_str() {
        "timed out" => "Pane preview unavailable (timed out).".to_string(),
        "oversized" => "Pane preview unavailable (response too large).".to_string(),
        "cancelled" => "Pane preview cancelled.".to_string(),
        _ => "Pane preview unavailable.".to_string(),
    })?;
    if code != 0 {
        return Err("Pane preview unavailable.".to_string());
    }
    Ok(strip_terminal_sequences(&output))
}

fn parse_resource_detail(payload: &Value, target: &Value) -> Option<String> {
    let result = payload.get("result")?.as_object()?;
    match target.get("kind").and_then(Value::as_str) {
        Some("workspace") => {
            let rows = result.get("worktrees").and_then(Value::as_array);
            let id = target.get("workspaceId").and_then(Value::as_str);
            let path = target.get("path").and_then(Value::as_str);
            let source_path = result
                .get("source")
                .and_then(|s| s.get("source_checkout_path"))
                .and_then(Value::as_str)
                .or_else(|| {
                    payload
                        .get("source")
                        .and_then(|s| s.get("source_checkout_path"))
                        .and_then(Value::as_str)
                });
            let rows = rows?;
            let row = rows
                .iter()
                .find(|row| row.get("open_workspace_id").and_then(Value::as_str) == id)
                .or_else(|| {
                    path.and_then(|path| {
                        rows.iter()
                            .find(|row| row.get("path").and_then(Value::as_str) == Some(path))
                    })
                })
                .or_else(|| {
                    source_path.and_then(|path| {
                        rows.iter()
                            .find(|row| row.get("path").and_then(Value::as_str) == Some(path))
                    })
                })?;
            if row.get("is_detached").and_then(Value::as_bool) == Some(true) {
                Some("detached HEAD".to_string())
            } else {
                row.get("branch")
                    .and_then(Value::as_str)
                    .filter(|branch| !branch.is_empty())
                    .map(ToString::to_string)
            }
        }
        Some("pane") => {
            let info = result.get("process_info")?.as_object()?;
            if info.get("pane_id").and_then(Value::as_str)
                != target.get("paneId").and_then(Value::as_str)
            {
                return None;
            }
            let mut names = Vec::new();
            for process in info
                .get("foreground_processes")?
                .as_array()?
                .iter()
                .filter_map(Value::as_object)
            {
                let values = process
                    .get("names")
                    .and_then(Value::as_array)
                    .map(|values| {
                        values
                            .iter()
                            .filter_map(Value::as_str)
                            .filter(|value| !value.is_empty())
                            .map(ToString::to_string)
                            .collect::<Vec<_>>()
                    })
                    .filter(|values| !values.is_empty())
                    .or_else(|| {
                        process
                            .get("names")
                            .and_then(Value::as_str)
                            .filter(|value| !value.is_empty())
                            .map(|value| vec![value.to_string()])
                    })
                    .unwrap_or_else(|| {
                        process
                            .get("name")
                            .and_then(Value::as_str)
                            .or_else(|| process.get("argv0").and_then(Value::as_str))
                            .map(|value| vec![value.to_string()])
                            .unwrap_or_default()
                    });
                for name in values {
                    if !names.contains(&name) {
                        names.push(name);
                    }
                }
            }
            (!names.is_empty()).then_some(names.join(" | "))
        }
        _ => None,
    }
}

/// Read branch or foreground process metadata for a live resource.
pub fn resource_details(target: &Value) -> Result<Option<String>, String> {
    resource_details_with_cancel(target, None)
}

fn resource_details_with_cancel(
    target: &Value,
    cancel: Option<&AtomicBool>,
) -> Result<Option<String>, String> {
    let id = target
        .get("workspaceId")
        .or_else(|| target.get("paneId"))
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .ok_or_else(|| "Resource details unavailable.".to_string())?;
    let args = if target.get("kind").and_then(Value::as_str) == Some("workspace") {
        vec![
            "worktree".into(),
            "list".into(),
            "--workspace".into(),
            id.to_string(),
        ]
    } else {
        vec![
            "pane".into(),
            "process-info".into(),
            "--pane".into(),
            id.to_string(),
        ]
    };
    let (code, output) =
        run_raw_cancellable(&args, Duration::from_secs(3), MAX_PREVIEW_BYTES, cancel).map_err(
            |error| {
                format!(
                    "Resource details unavailable{}.",
                    if error == "timed out" {
                        " (timed out)"
                    } else if error == "oversized" {
                        " (response too large)"
                    } else if error == "cancelled" {
                        " (cancelled)"
                    } else {
                        ""
                    }
                )
            },
        )?;
    if code != 0 {
        return Err("Resource details unavailable.".to_string());
    }
    let payload = serde_json::from_slice::<Value>(&output)
        .map_err(|_| "Resource details unavailable.".to_string())?;
    Ok(parse_resource_detail(&payload, target))
}

/// Cancellation-aware resource metadata read for selection and visibility changes.
pub fn resource_details_cancellable(
    target: &Value,
    cancel: &AtomicBool,
) -> Result<Option<String>, String> {
    resource_details_with_cancel(target, Some(cancel))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Shutdown;
    use std::os::unix::net::UnixListener;
    use std::sync::{Arc, Mutex, OnceLock};
    use std::thread;

    static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        ENV_LOCK.get_or_init(|| Mutex::new(())).lock().unwrap()
    }

    fn temp_path(label: &str) -> PathBuf {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "herdr-omni-{label}-{}-{suffix}",
            std::process::id()
        ))
    }

    fn mock_herdr(label: &str, body: &str) -> (PathBuf, PathBuf, PathBuf) {
        let directory = temp_path(label);
        fs::create_dir_all(&directory).unwrap();
        let command = directory.join("herdr");
        let log = directory.join("argv");
        let script = format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\nprintf '%s' '{}'\n",
            log.display(),
            body
        );
        fs::write(&command, script).unwrap();
        let status = Command::new("chmod")
            .args(["+x", command.to_str().unwrap()])
            .status()
            .unwrap();
        assert!(status.success());
        (directory, command, log)
    }

    fn mock_herdr_script(label: &str, body: &str) -> (PathBuf, PathBuf) {
        let directory = temp_path(label);
        fs::create_dir_all(&directory).unwrap();
        let command = directory.join("herdr");
        let script = format!("#!/bin/sh\n{body}\n");
        fs::write(&command, script).unwrap();
        let status = Command::new("chmod")
            .args(["+x", command.to_str().unwrap()])
            .status()
            .unwrap();
        assert!(status.success());
        (directory, command)
    }

    fn remove_tree(path: &Path) {
        let _ = fs::remove_dir_all(path);
    }

    #[test]
    fn live_snapshot_keeps_explicit_targets_and_numeric_tab_filtering() {
        let snapshot = json!({
            "workspaces": [{"workspace_id":"w1", "label":"Shop", "pane_count":1}],
            "tabs": [
                {"tab_id":"t1", "workspace_id":"w1", "label":"1"},
                {"tab_id":"t2", "workspace_id":"w1", "label":"Development"}
            ],
            "panes": [{"pane_id":"p1", "workspace_id":"w1", "tab_id":"t2", "label":"Terminal", "cwd":"/repo"}],
            "agents": []
        });
        let items = items_from_snapshot(&snapshot, "w1");
        assert_eq!(
            items
                .iter()
                .filter(|item| item["category"] == "Tabs")
                .count(),
            1
        );
        assert!(items
            .iter()
            .any(|item| item["invocation"]["argv"] == json!(["workspace", "focus", "w1"])));
        assert_eq!(
            items
                .iter()
                .find(|item| item["category"] == "Tabs")
                .unwrap()["resourcePreview"]["panes"][0]["id"],
            "p1"
        );
    }

    #[test]
    fn live_mapping_matches_typescript_oracle_fixture() {
        let fixture: Value =
            serde_json::from_str(include_str!("../tests/fixtures/live-parity.json")).unwrap();
        for case in fixture["snapshots"].as_array().unwrap() {
            let actual = items_from_snapshot_with_visits(
                &case["snapshot"],
                case["current"].as_str().unwrap_or_default(),
                &HashMap::new(),
            );
            assert_eq!(
                actual,
                case["expected"].as_array().unwrap().clone(),
                "snapshot oracle mismatch"
            );
        }
        let worktrees = fixture["worktrees"].as_object().unwrap();
        let actual = items_from_worktrees(
            Some(&worktrees["rows"]),
            worktrees["current"].as_str().unwrap_or_default(),
        );
        assert_eq!(
            actual,
            worktrees["expected"].as_array().unwrap().clone(),
            "worktree oracle mismatch"
        );
    }

    #[test]
    fn preview_formats_unopened_worktree_without_running_herdr() {
        let item = json!({"resourcePreview":{"kind":"worktree","path":"/repo/feature","branch":"feature/ui"}});
        let result = preview(&item, 0).unwrap();
        assert!(result.contains("Path: /repo/feature"));
        assert!(result.contains("Branch: feature/ui"));
        assert!(result.contains("Not open in a workspace."));
    }

    #[test]
    fn command_runner_does_not_wait_for_exited_descendants() {
        let _guard = env_lock();
        let (directory, command) = mock_herdr_script(
            "descendant-runner",
            "printf '%s' '{\"result\":{}}'\n(sleep 2) &",
        );
        std::env::set_var("HERDR_BIN_PATH", &command);
        let started = std::time::Instant::now();
        assert!(run_herdr(&[]).is_ok());
        assert!(started.elapsed() < Duration::from_secs(1));
        std::env::remove_var("HERDR_BIN_PATH");
        remove_tree(&directory);
    }

    #[test]
    fn raw_runner_does_not_wait_for_exited_descendants() {
        let _guard = env_lock();
        let (directory, command) =
            mock_herdr_script("descendant-raw", "printf visible\n(sleep 2) &");
        std::env::set_var("HERDR_BIN_PATH", &command);
        let started = std::time::Instant::now();
        assert_eq!(
            preview(&json!({"livePaneId":"pane"}), 0).unwrap(),
            "visible"
        );
        assert!(started.elapsed() < Duration::from_secs(1));
        std::env::remove_var("HERDR_BIN_PATH");
        remove_tree(&directory);
    }

    #[test]
    fn resource_overview_matches_typescript_oracle() {
        let fixture: Value = serde_json::from_str(include_str!(
            "../tests/fixtures/resource-overview-parity.json"
        ))
        .unwrap();
        for case in fixture.as_array().unwrap() {
            let item = json!({"resourcePreview": case["resource"]});
            let actual = preview(&item, 0).unwrap();
            assert_eq!(
                actual,
                case["expected"].as_str().unwrap(),
                "resource overview oracle mismatch: {}",
                case["name"]
            );
        }
    }

    #[test]
    fn terminal_filter_preserves_unicode_and_visible_whitespace() {
        assert_eq!(
            strip_terminal_sequences("\x1b[31m  你好🙂\x1b[0m\n\tline\n".as_bytes()),
            "  你好🙂\n\tline\n"
        );
    }

    #[test]
    fn session_ids_accept_only_safe_provider_identifiers() {
        assert!(valid_session("codex", "thread_123-a"));
        assert!(!valid_session("codex", "x; rm -rf /"));
        assert!(!valid_session("unknown", "thread_123"));
    }

    #[test]
    fn execute_uses_exact_argv_without_shell_interpolation() {
        let _guard = env_lock();
        let (directory, command, log) = mock_herdr("argv", r#"{"result":{}}"#);
        std::env::set_var("HERDR_BIN_PATH", &command);
        let item = json!({"shortcuts":[],"invocation":{"kind":"herdr","argv":["pane","rename","pane; echo injected"]}});
        assert_eq!(execute(&item, ""), json!({"ok":true,"message":""}));
        let argv = fs::read_to_string(&log)
            .unwrap()
            .lines()
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        assert_eq!(argv, ["pane", "rename", "pane; echo injected"]);
        std::env::remove_var("HERDR_BIN_PATH");
        remove_tree(&directory);
    }

    #[test]
    fn resolve_actions_replace_host_current_with_launch_pane() {
        let _guard = env_lock();
        let (directory, command, log) = mock_herdr("resolve", r#"{"result":{}}"#);
        std::env::set_var("HERDR_BIN_PATH", &command);
        std::env::set_var(
            "HERDR_PLUGIN_CONTEXT_JSON",
            r#"{"focused_pane_id":"host-pane","tab_id":"host-tab","workspace_id":"host-workspace"}"#,
        );
        let item = json!({"shortcuts":[],"invocation":{"kind":"herdr","argv":["pane","read","--current"]}});
        assert_eq!(execute(&item, ""), json!({"ok":true,"message":""}));
        let argv = fs::read_to_string(&log)
            .unwrap()
            .lines()
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        assert_eq!(argv, ["pane", "read", "--pane", "host-pane"]);
        std::env::remove_var("HERDR_BIN_PATH");
        std::env::remove_var("HERDR_PLUGIN_CONTEXT_JSON");
        remove_tree(&directory);
    }

    #[test]
    fn socket_actions_send_targeted_json_request() {
        let _guard = env_lock();
        let socket_path = temp_path("socket");
        let listener = UnixListener::bind(&socket_path).unwrap();
        std::env::set_var("HERDR_SOCKET_PATH", &socket_path);
        std::env::set_var(
            "HERDR_PLUGIN_CONTEXT_JSON",
            r#"{"focused_pane_id":"host-pane","tab_id":"host-tab","workspace_id":"host-workspace"}"#,
        );
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = String::new();
            BufReader::new(&mut stream).read_line(&mut request).unwrap();
            let request: Value = serde_json::from_str(&request).unwrap();
            assert_eq!(request["method"], "pane.edit_scrollback");
            assert_eq!(request["params"], json!({"pane_id":"host-pane"}));
            writeln!(stream, "{}", json!({"id":"other-request","result":{}})).unwrap();
            writeln!(stream, "{}", json!({"id":request["id"],"result":{}})).unwrap();
            let _ = stream.shutdown(Shutdown::Both);
        });
        let item = json!({"invocation":{"kind":"pane-api","method":"pane.edit_scrollback"}});
        assert_eq!(execute(&item, ""), json!({"ok":true,"message":""}));
        server.join().unwrap();
        std::env::remove_var("HERDR_SOCKET_PATH");
        std::env::remove_var("HERDR_PLUGIN_CONTEXT_JSON");
        let _ = fs::remove_file(socket_path);
    }

    #[test]
    fn socket_actions_reject_oversized_responses() {
        let _guard = env_lock();
        let socket_path = temp_path("socket-oversized");
        let listener = UnixListener::bind(&socket_path).unwrap();
        std::env::set_var("HERDR_SOCKET_PATH", &socket_path);
        std::env::set_var(
            "HERDR_PLUGIN_CONTEXT_JSON",
            r#"{"focused_pane_id":"host-pane","tab_id":"host-tab","workspace_id":"host-workspace"}"#,
        );
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = String::new();
            BufReader::new(&mut stream).read_line(&mut request).unwrap();
            stream
                .write_all(&vec![b'x'; MAX_SOCKET_RESPONSE_BYTES + 1])
                .unwrap();
            let _ = stream.shutdown(Shutdown::Both);
        });
        let item = json!({"invocation":{"kind":"pane-api","method":"pane.edit_scrollback"}});
        assert_eq!(
            execute(&item, ""),
            json!({"ok":false,"message":"Herdr returned an oversized API response."})
        );
        server.join().unwrap();
        std::env::remove_var("HERDR_SOCKET_PATH");
        std::env::remove_var("HERDR_PLUGIN_CONTEXT_JSON");
        let _ = fs::remove_file(socket_path);
    }

    #[test]
    fn agent_neighbor_matches_typescript_edge_cases() {
        let _guard = env_lock();
        let target = json!({"paneId":"current","tabId":"tab","workspaceId":"workspace"});
        let cases = [
            (r#"{"result":{"agents":[]}}"#, 1_i64, "empty"),
            (
                r#"{"result":{"agents":[{"pane_id":"other"}]}}"#,
                1,
                "one-other",
            ),
            (
                r#"{"result":{"agents":[{"pane_id":"first"},{"pane_id":"last"}]}}"#,
                -1,
                "stale",
            ),
        ];
        for (index, (body, step, expected)) in cases.into_iter().enumerate() {
            let (directory, command, _) = mock_herdr(&format!("agent-neighbor-{index}"), body);
            std::env::set_var("HERDR_BIN_PATH", &command);
            let actual = neighbor(&target, "agent", step);
            match expected {
                "one-other" => assert_eq!(actual.unwrap(), "other"),
                "stale" => assert_eq!(actual.unwrap(), "last"),
                "empty" => assert_eq!(actual.unwrap_err(), "No agents are running."),
                _ => unreachable!(),
            }
            remove_tree(&directory);
        }
        std::env::remove_var("HERDR_BIN_PATH");
    }

    #[test]
    fn resume_choices_include_agent_working_directories() {
        let directory = temp_path("agent-choice");
        fs::create_dir_all(&directory).unwrap();
        let snapshot = json!({
            "workspaces": [{"workspace_id":"w1","label":"Workspace"}],
            "panes": [],
            "agents": [{"workspace_id":"w1","cwd":directory.to_string_lossy()}]
        });
        let choices = resume_workspace_choices(&snapshot, "/missing/original", "w1");
        assert_eq!(choices.len(), 1);
        assert_eq!(choices[0]["id"], "w1");
        assert_eq!(choices[0]["cwd"], directory.to_string_lossy().as_ref());
        remove_tree(&directory);
    }

    #[test]
    fn live_mapping_filters_open_worktrees_and_preserves_semantic_title() {
        let snapshot =
            json!({"workspaces":[{"workspace_id":"w1","label":"Shop"}],"tabs":[],"agents":[]});
        let items = items_from_snapshot(&snapshot, "w1");
        let worktrees = json!([
            {"path":"/repo/open","label":"open","branch":"main","open_workspace_id":"w1"},
            {"path":"/repo/new","label":"new","branch":"feature/ui"}
        ]);
        let destinations = items_from_worktrees(Some(&worktrees), "w1");
        assert_eq!(destinations.len(), 1);
        assert_eq!(destinations[0]["title"], "new");
        assert_eq!(destinations[0]["searchTitle"], "new");
        assert_eq!(items[0]["searchTitle"], "Shop");
    }

    #[test]
    fn workspace_history_only_accepts_successful_focus_events() {
        let _guard = env_lock();
        let directory = temp_path("history");
        fs::create_dir_all(&directory).unwrap();
        let config = directory.join("config.toml");
        fs::write(&config, "").unwrap();
        fs::write(directory.join("herdr-server.log"), "2026-09-22T10:00:00Z event=\"workspace.focus\" outcome=\"ok\" workspace_id=\"w1\"\n2026-09-22T11:00:00Z event=\"workspace.focus\" outcome=\"error\" workspace_id=\"w1\"\n2026-09-22T12:00:00Z event=\"tab.focus\" outcome=\"ok\" workspace_id=\"w2\"\n").unwrap();
        std::env::set_var("HERDR_CONFIG_PATH", &config);
        let expected = DateTime::parse_from_rfc3339("2026-09-22T10:00:00Z")
            .unwrap()
            .timestamp_millis();
        assert_eq!(workspace_visits().get("w1"), Some(&expected));
        let mut oversized = "🙂".repeat(MAX_SERVER_LOG_CHARS / 4 + 64);
        oversized.push('\n');
        oversized.push_str("2026-09-22T13:00:00Z event=\"workspace.focus\" outcome=\"ok\" workspace_id=\"w-你好\"\n");
        fs::write(directory.join("herdr-server.log"), oversized).unwrap();
        assert!(workspace_visits().contains_key("w-你好"));
        std::env::remove_var("HERDR_CONFIG_PATH");
        remove_tree(&directory);
    }

    #[test]
    fn resume_rejects_provider_and_session_injection_before_running_commands() {
        let item = json!({"invocation":{"kind":"resume-session","session":{"provider":"codex","id":"bad; echo injected","title":"x","cwd":"/tmp"}}});
        assert_eq!(
            execute(&item, ""),
            json!({"ok":false,"message":"Invalid saved session identifier."})
        );
        let unknown = json!({"invocation":{"kind":"resume-session","session":{"provider":"evil","id":"safe","title":"x","cwd":"/tmp"}}});
        assert_eq!(
            execute(&unknown, ""),
            json!({"ok":false,"message":"Invalid saved session identifier."})
        );
    }

    #[test]
    fn cancellation_kills_process_group_and_reader_promptly() {
        let _guard = env_lock();
        let (directory, command) = mock_herdr_script("cancel", "sleep 10");
        std::env::set_var("HERDR_BIN_PATH", &command);
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = cancel.clone();
        let item = json!({"livePaneId":"pane"});
        let started = std::time::Instant::now();
        let worker = thread::spawn(move || preview_cancellable(&item, 0, &flag));
        thread::sleep(Duration::from_millis(40));
        cancel.store(true, Ordering::Relaxed);
        let result = worker.join().unwrap();
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "cancellation took too long: {result:?}"
        );
        assert!(result.unwrap_err().contains("cancel"));
        std::env::remove_var("HERDR_BIN_PATH");
        remove_tree(&directory);
    }

    #[test]
    fn cancellable_live_loading_stops_snapshot_command_promptly() {
        let _guard = env_lock();
        let (directory, command) = mock_herdr_script("live-cancel", "sleep 10");
        std::env::set_var("HERDR_BIN_PATH", &command);
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = cancel.clone();
        let started = std::time::Instant::now();
        let worker = thread::spawn(move || load_live_items_cancellable(&flag));
        thread::sleep(Duration::from_millis(40));
        cancel.store(true, Ordering::Relaxed);
        let result = worker.join().unwrap();
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "cancellation took too long: {result:?}"
        );
        assert!(result.unwrap_err().contains("cancel"));
        std::env::remove_var("HERDR_BIN_PATH");
        remove_tree(&directory);
    }

    #[test]
    fn resource_details_uses_exact_identity_and_hides_process_arguments() {
        let _guard = env_lock();
        let (directory, command, log) = mock_herdr(
            "details",
            r#"{"result":{"process_info":{"pane_id":"pane-1","foreground_processes":[{"name":"node","argv":["node","--secret"]},{"argv0":"zsh"},{"names":"fish"}]}}}"#,
        );
        std::env::set_var("HERDR_BIN_PATH", &command);
        let target = json!({"kind":"pane","paneId":"pane-1"});
        assert_eq!(
            resource_details(&target).unwrap(),
            Some("node | zsh | fish".to_string())
        );
        let argv = fs::read_to_string(&log)
            .unwrap()
            .lines()
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        assert_eq!(argv, ["pane", "process-info", "--pane", "pane-1"]);
        std::env::remove_var("HERDR_BIN_PATH");
        remove_tree(&directory);
    }

    #[test]
    fn resource_details_filters_empty_process_names_before_fallback() {
        let payload = json!({
            "result": {
                "process_info": {
                    "pane_id": "pane-1",
                    "foreground_processes": [{"names":[""],"name":"zsh"}]
                }
            }
        });
        assert_eq!(
            parse_resource_detail(&payload, &json!({"kind":"pane","paneId":"pane-1"})),
            Some("zsh".to_string())
        );
    }

    #[test]
    fn cancellable_resource_details_kills_metadata_command() {
        let _guard = env_lock();
        let (directory, command) = mock_herdr_script("details-cancel", "sleep 10");
        std::env::set_var("HERDR_BIN_PATH", &command);
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = cancel.clone();
        let target = json!({"kind":"pane","paneId":"pane"});
        let started = std::time::Instant::now();
        let worker = thread::spawn(move || resource_details_cancellable(&target, &flag));
        thread::sleep(Duration::from_millis(40));
        cancel.store(true, Ordering::Relaxed);
        let result = worker.join().unwrap();
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "cancellation took too long: {result:?}"
        );
        assert!(result.unwrap_err().contains("cancelled"));
        std::env::remove_var("HERDR_BIN_PATH");
        remove_tree(&directory);
    }
}
