use std::env;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
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
        "herdr-omni-installer-{label}-{}-{suffix}",
        std::process::id()
    ))
}

fn executable(path: &Path, body: &str) {
    fs::write(path, body).unwrap();
    let mut permissions = fs::metadata(path).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).unwrap();
}

fn sha256(path: &Path) -> String {
    let output = Command::new("sha256sum")
        .arg(path)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .or_else(|| {
            Command::new("shasum")
                .args(["-a", "256", path.to_str().unwrap()])
                .output()
                .ok()
                .filter(|output| output.status.success())
        })
        .expect("no SHA-256 tool available");
    String::from_utf8(output.stdout)
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .to_string()
}

struct Harness {
    root: PathBuf,
    fake_bin: PathBuf,
    payload: PathBuf,
    checksum: PathBuf,
    url_log: PathBuf,
    cargo_marker: PathBuf,
    payload_marker: PathBuf,
    original_path: String,
}

impl Harness {
    fn new(label: &str) -> Self {
        let root = temp_path(label);
        let fake_bin = root.join("fake-bin");
        fs::create_dir_all(root.join("scripts")).unwrap();
        fs::create_dir_all(root.join("bin")).unwrap();
        fs::create_dir_all(&fake_bin).unwrap();
        fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/install.sh"),
            root.join("scripts/install.sh"),
        )
        .unwrap();
        fs::write(
            root.join("herdr-plugin.toml"),
            format!(
                "id = \"herdr-omni\"\nversion = \"{}\"\n",
                env!("CARGO_PKG_VERSION")
            ),
        )
        .unwrap();

        let payload = root.join("payload");
        executable(
            &payload,
            &format!(
                "#!/bin/sh\nif [ -n \"${{PAYLOAD_EXECUTED:-}}\" ]; then : > \"$PAYLOAD_EXECUTED\"; fi\nif [ \"$1\" = \"--version\" ]; then printf '%s\\n' 'herdr-omni {}'; else printf '%s\\n' payload; fi\n",
                env!("CARGO_PKG_VERSION")
            ),
        );
        let checksum = root.join("payload.sha256");
        let digest = sha256(&payload);
        fs::write(&checksum, format!("{digest}  -\n")).unwrap();

        let url_log = root.join("urls.log");
        let cargo_marker = root.join("cargo-invoked");
        let payload_marker = root.join("payload-executed");
        executable(
            &fake_bin.join("uname"),
            r#"#!/bin/sh
case "$1" in
  -s) printf '%s\n' "${FAKE_UNAME_S:-Darwin}" ;;
  -m) printf '%s\n' "${FAKE_UNAME_M:-x86_64}" ;;
  *) exit 1 ;;
esac
"#,
        );
        executable(
            &fake_bin.join("curl"),
            r#"#!/bin/sh
output=
url=
next_output=0
for arg in "$@"; do
  if [ "$next_output" = 1 ]; then output=$arg; next_output=0; continue; fi
  case "$arg" in
    -o) next_output=1 ;;
    http://*|https://*) url=$arg ;;
  esac
done
printf '%s\n' "$url" >> "$URL_LOG"
if [ "${FAIL_DOWNLOAD:-0}" = 1 ]; then exit 22; fi
if printf '%s' "$url" | grep -q '\.sha256$'; then
  if [ "${BAD_CHECKSUM:-0}" = 1 ]; then printf '%s\n' 0000000000000000000000000000000000000000000000000000000000000000 > "$output"; else cp "$CHECKSUM" "$output"; fi
else
  cp "$PAYLOAD" "$output"
fi
"#,
        );
        executable(
            &fake_bin.join("wget"),
            r#"#!/bin/sh
output=
url=
next_output=0
for arg in "$@"; do
  if [ "$next_output" = 1 ]; then output=$arg; next_output=0; continue; fi
  case "$arg" in
    -O) next_output=1 ;;
    http://*|https://*) url=$arg ;;
  esac
done
printf '%s\n' "$url" >> "$URL_LOG"
if [ "${FAIL_DOWNLOAD:-0}" = 1 ]; then exit 22; fi
if printf '%s' "$url" | grep -q '\.sha256$'; then cp "$CHECKSUM" "$output"; else cp "$PAYLOAD" "$output"; fi
"#,
        );
        executable(
            &fake_bin.join("cargo"),
            r#"#!/bin/sh
printf invoked > "$CARGO_MARKER"
exit 99
"#,
        );
        let original_path = env::var("PATH").unwrap();
        Self {
            root,
            fake_bin,
            payload,
            checksum,
            url_log,
            cargo_marker,
            payload_marker,
            original_path,
        }
    }

    fn command(&self, os: &str, arch: &str) -> Command {
        let mut command = Command::new("sh");
        command
            .args(["scripts/install.sh"])
            .current_dir(&self.root)
            .env(
                "PATH",
                format!("{}:{}", self.fake_bin.display(), self.original_path),
            )
            .env("FAKE_UNAME_S", os)
            .env("FAKE_UNAME_M", arch)
            .env("PAYLOAD", &self.payload)
            .env("CHECKSUM", &self.checksum)
            .env("URL_LOG", &self.url_log)
            .env("CARGO_MARKER", &self.cargo_marker)
            .env("PAYLOAD_EXECUTED", &self.payload_marker)
            .env_remove("HERDR_OMNI_BUILD_FROM_SOURCE")
            .env_remove("BAD_CHECKSUM")
            .env_remove("FAIL_DOWNLOAD");
        command
    }

    fn output_binary(&self) -> PathBuf {
        self.root.join("bin/herdr-omni")
    }

    fn refresh_checksum(&self) {
        fs::write(&self.checksum, format!("{}  -\n", sha256(&self.payload))).unwrap();
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn installer_maps_supported_platforms_to_release_assets_without_cargo() {
    let _guard = env_lock();
    for (os, arch, target) in [
        ("Darwin", "x86_64", "x86_64-apple-darwin"),
        ("Darwin", "arm64", "aarch64-apple-darwin"),
        ("Linux", "x86_64", "x86_64-unknown-linux-musl"),
        ("Linux", "aarch64", "aarch64-unknown-linux-musl"),
    ] {
        let harness = Harness::new(target);
        let old = harness.output_binary();
        executable(&old, "#!/bin/sh\nprintf old\n");
        let output = harness.command(os, arch).output().unwrap();
        assert!(
            output.status.success(),
            "installer failed for {target}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let version = Command::new(&old).arg("--version").output().unwrap();
        assert_eq!(
            String::from_utf8_lossy(&version.stdout).trim(),
            format!("herdr-omni {}", env!("CARGO_PKG_VERSION"))
        );
        let urls = fs::read_to_string(&harness.url_log).unwrap();
        assert!(urls.contains(&format!(
            "https://github.com/mmjang/herdr-omni/releases/download/v{}/herdr-omni-{target}",
            env!("CARGO_PKG_VERSION")
        )));
        assert!(urls.lines().any(|url| url.ends_with(".sha256")));
        assert!(!harness.cargo_marker.exists());
    }
}

#[test]
fn checksum_failure_preserves_existing_binary() {
    let _guard = env_lock();
    let harness = Harness::new("checksum-failure");
    let output_binary = harness.output_binary();
    executable(&output_binary, "#!/bin/sh\nprintf sentinel\n");
    let mut command = harness.command("Linux", "x86_64");
    command.env("BAD_CHECKSUM", "1");
    let output = command.output().unwrap();
    assert!(!output.status.success());
    assert_eq!(
        fs::read_to_string(output_binary).unwrap(),
        "#!/bin/sh\nprintf sentinel\n"
    );
    assert!(!harness.cargo_marker.exists());
    assert!(!harness.payload_marker.exists());
}

#[test]
fn version_mismatch_preserves_existing_binary() {
    let _guard = env_lock();
    let harness = Harness::new("version-mismatch");
    executable(
        &harness.payload,
        "#!/bin/sh\n: > \"$PAYLOAD_EXECUTED\"\nprintf '%s\\n' 'herdr-omni 0.0.0'\n",
    );
    harness.refresh_checksum();
    let output_binary = harness.output_binary();
    executable(&output_binary, "#!/bin/sh\nprintf sentinel\n");
    let output = harness.command("Linux", "x86_64").output().unwrap();
    assert!(!output.status.success());
    assert_eq!(
        fs::read_to_string(output_binary).unwrap(),
        "#!/bin/sh\nprintf sentinel\n"
    );
    assert!(!harness.cargo_marker.exists());
    assert!(harness.payload_marker.exists());
}

#[test]
fn unavailable_asset_fails_without_cargo_fallback_or_replacement() {
    let _guard = env_lock();
    let harness = Harness::new("unavailable-asset");
    let output_binary = harness.output_binary();
    executable(&output_binary, "#!/bin/sh\nprintf sentinel\n");
    let mut command = harness.command("Linux", "x86_64");
    command.env("FAIL_DOWNLOAD", "1");
    let output = command.output().unwrap();
    assert!(!output.status.success());
    assert_eq!(
        fs::read_to_string(output_binary).unwrap(),
        "#!/bin/sh\nprintf sentinel\n"
    );
    assert!(!harness.cargo_marker.exists());
    assert!(!harness.payload_marker.exists());
}

#[test]
fn unsupported_platform_fails_before_download_or_cargo() {
    let _guard = env_lock();
    for (os, arch) in [("FreeBSD", "x86_64"), ("Linux", "riscv64")] {
        let harness = Harness::new(&format!("unsupported-{os}-{arch}"));
        let output_binary = harness.output_binary();
        executable(&output_binary, "#!/bin/sh\nprintf sentinel\n");
        let output = harness.command(os, arch).output().unwrap();
        assert!(!output.status.success());
        assert_eq!(
            fs::read_to_string(output_binary).unwrap(),
            "#!/bin/sh\nprintf sentinel\n"
        );
        assert!(!harness.url_log.exists());
        assert!(!harness.cargo_marker.exists());
    }
}
