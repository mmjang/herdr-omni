use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn temp_project(label: &str) -> PathBuf {
    let suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "herdr-release-{label}-{}-{suffix}",
        std::process::id()
    ));
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("Cargo.toml"), "[package] # package metadata\nname = \"fixture\"\nversion = '1.2.3' # keep this comment\nedition = \"2021\"\n\n[dependencies]\nexample = { version = \"9.9\" }\n").unwrap();
    fs::write(
        root.join("herdr-plugin.toml"),
        "version = \"0.1.0\" # stale plugin\nid = \"fixture\"\n",
    )
    .unwrap();
    fs::write(
        root.join("Cargo.lock"),
        "version = 3\n\n[[package]]\nname = \"fixture\"\nversion = \"1.2.3\"\n",
    )
    .unwrap();
    root
}

fn mock_command(root: &Path, name: &str, body: &str) -> PathBuf {
    let path = root.join(name);
    let log = root.parent().unwrap_or(root).join("commands.log");
    fs::write(
        &path,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" >> '{}'\n{}\n",
            log.display(),
            body
        ),
    )
    .unwrap();
    let status = Command::new("chmod")
        .args(["+x", path.to_str().unwrap()])
        .status()
        .unwrap();
    assert!(status.success());
    path
}

fn run_tool(root: &Path, args: &[&str], bin_dir: Option<&Path>) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_release-tool"));
    command.current_dir(root).args(args);
    if let Some(bin_dir) = bin_dir {
        command.env("PATH", format!("{}:/bin:/usr/bin", bin_dir.display()));
    }
    command.output().unwrap()
}

#[test]
fn bump_syncs_mismatched_plugin_and_preserves_toml_formatting() {
    let root = temp_project("bump");
    let bin = root.join("bin");
    fs::create_dir_all(&bin).unwrap();
    mock_command(&bin, "cargo", "exit 0");
    let result = run_tool(&root, &["bump", "patch"], Some(&bin));
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&result.stdout).trim(), "1.2.4");
    let cargo = fs::read_to_string(root.join("Cargo.toml")).unwrap();
    assert!(cargo.contains("version = '1.2.4' # keep this comment"));
    assert!(cargo.contains("# keep this comment\nedition = \"2021\""));
    assert!(cargo.contains("example = { version = \"9.9\" }"));
    let plugin = fs::read_to_string(root.join("herdr-plugin.toml")).unwrap();
    assert!(plugin.contains("version = \"1.2.4\" # stale plugin"));
    assert!(plugin.contains("# stale plugin\nid = \"fixture\""));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn check_rejects_manifest_or_tag_mismatch() {
    let root = temp_project("check");
    let good = run_tool(&root, &["check", "v1.2.3"], None);
    assert!(!good.status.success(), "stale plugin must fail check");
    fs::write(
        root.join("herdr-plugin.toml"),
        "version = \"1.2.3\"\nid = \"fixture\"\n",
    )
    .unwrap();
    let good = run_tool(&root, &["check", "v1.2.3"], None);
    assert!(
        good.status.success(),
        "{}",
        String::from_utf8_lossy(&good.stderr)
    );
    let bad_tag = run_tool(&root, &["check", "v1.2.4"], None);
    assert!(!bad_tag.status.success());
    let _ = fs::remove_dir_all(root);
}

#[test]
fn release_checks_clean_state_and_runs_exact_git_sequence() {
    let root = temp_project("release");
    let bin = root.join("bin");
    fs::create_dir_all(&bin).unwrap();
    mock_command(&bin, "cargo", "exit 0");
    mock_command(
        &bin,
        "git",
        "if [ \"$1\" = status ]; then exit 0; fi\nexit 0",
    );
    let result = run_tool(&root, &["release", "minor"], Some(&bin));
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let commands = fs::read_to_string(root.join("commands.log")).unwrap();
    assert!(commands.contains("status\n--porcelain\n--untracked-files=no\n"));
    assert!(commands.contains("check\n--quiet\n"));
    assert!(commands.contains("add\nCargo.toml\nCargo.lock\nherdr-plugin.toml\n"));
    assert!(commands.contains("commit\n-m\nRelease v1.3.0\n"));
    assert!(commands.contains("tag\n-a\nv1.3.0\n-m\nRelease v1.3.0\n"));
    assert!(commands.contains("push\n--follow-tags\n"));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn release_refuses_tracked_changes_before_bumping() {
    let root = temp_project("dirty");
    let bin = root.join("bin");
    fs::create_dir_all(&bin).unwrap();
    mock_command(&bin, "cargo", "exit 0");
    mock_command(
        &bin,
        "git",
        "if [ \"$1\" = status ]; then echo ' M Cargo.toml'; exit 0; fi\nexit 0",
    );
    let result = run_tool(&root, &["release", "patch"], Some(&bin));
    assert!(!result.status.success());
    assert!(fs::read_to_string(root.join("Cargo.toml"))
        .unwrap()
        .contains("1.2.3"));
    let _ = fs::remove_dir_all(root);
}
