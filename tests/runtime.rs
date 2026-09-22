use std::process::Command;

#[test]
fn version_and_help_do_not_need_a_terminal_or_herdr() {
    for arg in ["--version", "--help"] {
        let output = Command::new(env!("CARGO_BIN_EXE_herdr-omni"))
            .arg(arg)
            .env("HERDR_BIN_PATH", "/definitely/not/installed")
            .output()
            .unwrap();
        assert!(output.status.success());
        let text = String::from_utf8(output.stdout).unwrap();
        assert!(text.to_lowercase().contains("herdr"));
        if arg == "--version" {
            assert!(text.contains(env!("CARGO_PKG_VERSION")));
        }
    }
}

#[test]
fn plugin_builds_and_launches_the_native_binary() {
    let manifest: toml::Value = include_str!("../herdr-plugin.toml").parse().unwrap();
    assert_eq!(
        manifest["version"].as_str(),
        Some(env!("CARGO_PKG_VERSION"))
    );
    let build = manifest["build"][0]["command"].as_array().unwrap();
    assert_eq!(
        build.iter().map(|arg| arg.as_str()).collect::<Vec<_>>(),
        vec![Some("sh"), Some("scripts/install.sh")]
    );
    assert_eq!(
        manifest["panes"][0]["command"][0].as_str(),
        Some("./bin/herdr-omni")
    );
    assert_eq!(manifest["panes"][0]["placement"].as_str(), Some("popup"));
}

#[test]
fn release_version_gate_rejects_mismatched_tags() {
    let tool = env!("CARGO_BIN_EXE_release-tool");
    assert!(Command::new(tool)
        .args(["check", &format!("v{}", env!("CARGO_PKG_VERSION"))])
        .output()
        .unwrap()
        .status
        .success());
    assert!(!Command::new(tool)
        .args(["check", "v0.0.0"])
        .output()
        .unwrap()
        .status
        .success());
    assert!(!Command::new(tool)
        .args(["check", env!("CARGO_PKG_VERSION")])
        .output()
        .unwrap()
        .status
        .success());
}
