//! Versioning and release helper.
//!
//! `bump` edits manifests locally. `release` is the explicit publishing
//! workflow: it requires a clean tracked worktree, commits the version bump,
//! tags it, and pushes the tag. No command here creates a GitHub release.

use std::{error::Error, fs, process::Command};

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("check") => check(args.get(2).ok_or("usage: release-tool check <vVERSION>")?),
        Some("bump") => {
            let level = args.get(2).map(String::as_str).unwrap_or("minor");
            println!("{}", bump(level)?);
            Ok(())
        }
        Some("release") => release(args.get(2).map(String::as_str).unwrap_or("minor")),
        _ => Err("usage: release-tool check <vVERSION> | bump [major|minor|patch] | release [major|minor|patch]".into()),
    }
}

fn read_manifests() -> Result<(String, String), Box<dyn Error>> {
    Ok((
        fs::read_to_string("Cargo.toml")?,
        fs::read_to_string("herdr-plugin.toml")?,
    ))
}

fn manifest_version(content: &str, package_manifest: bool) -> Result<String, Box<dyn Error>> {
    let parsed: toml::Value = content.parse()?;
    let value = if package_manifest {
        parsed
            .get("package")
            .and_then(|package| package.get("version"))
    } else {
        parsed.get("version")
    };
    value
        .and_then(toml::Value::as_str)
        .map(ToString::to_string)
        .ok_or_else(|| {
            if package_manifest {
                "missing Cargo version"
            } else {
                "missing plugin version"
            }
            .into()
        })
}

/// Replace only the version key in the intended TOML scope while retaining
/// comments, spacing, ordering, and line endings.
fn edit_version(
    content: &str,
    package_manifest: bool,
    next: &str,
) -> Result<String, Box<dyn Error>> {
    let scope = if package_manifest {
        Some("package")
    } else {
        None
    };
    let mut section = String::new();
    let mut replaced = false;
    let mut output = String::with_capacity(content.len() + next.len());
    for chunk in content.split_inclusive('\n') {
        let line = chunk.strip_suffix('\n').unwrap_or(chunk);
        let line_without_cr = line.strip_suffix('\r').unwrap_or(line);
        let newline = &chunk[line_without_cr.len()..];
        let trimmed = line_without_cr.trim();
        let header = trimmed
            .split_once('#')
            .map(|(header, _)| header.trim())
            .unwrap_or(trimmed);
        if header.starts_with('[') && header.ends_with(']') {
            section = if header.starts_with("[[") {
                header.to_string()
            } else {
                header.trim_matches(['[', ']']).to_string()
            };
        }
        let in_scope = match scope {
            Some(scope) => section == scope,
            None => section.is_empty(),
        };
        if !replaced && in_scope && !trimmed.starts_with('#') {
            if let Some(equal) = line_without_cr.find('=') {
                if line_without_cr[..equal].trim() == "version" {
                    let rhs_start = equal + 1;
                    let rhs = &line_without_cr[rhs_start..];
                    let quote = rhs
                        .find(['"', '\''])
                        .ok_or("version must be a TOML string")?;
                    let delimiter = rhs.as_bytes()[quote] as char;
                    let end = rhs[quote + 1..]
                        .find(delimiter)
                        .ok_or("unterminated version string")?
                        + quote
                        + 1;
                    output.push_str(&line_without_cr[..rhs_start + quote + 1]);
                    output.push_str(next);
                    output.push_str(&rhs[end..]);
                    output.push_str(newline);
                    replaced = true;
                    continue;
                }
            }
        }
        output.push_str(line);
        output.push('\n');
    }
    if !content.ends_with('\n') && output.ends_with('\n') {
        output.pop();
    }
    if !replaced {
        return Err(if package_manifest {
            "missing Cargo version"
        } else {
            "missing plugin version"
        }
        .into());
    }
    if manifest_version(&output, package_manifest)? != next {
        return Err("edited manifest version did not persist".into());
    }
    Ok(output)
}

fn next_version(version: &str, level: &str) -> Result<String, Box<dyn Error>> {
    let parts = version
        .split('.')
        .map(str::parse::<u64>)
        .collect::<Result<Vec<_>, _>>()?;
    if parts.len() != 3 {
        return Err("expected major.minor.patch".into());
    }
    let (mut major, mut minor, mut patch) = (parts[0], parts[1], parts[2]);
    match level {
        "major" => {
            major += 1;
            minor = 0;
            patch = 0;
        }
        "minor" => {
            minor += 1;
            patch = 0;
        }
        "patch" => patch += 1,
        _ => return Err("bump level must be major, minor, or patch".into()),
    }
    Ok(format!("{major}.{minor}.{patch}"))
}

fn command_success(program: &str, args: &[&str], failure: &str) -> Result<(), Box<dyn Error>> {
    if !Command::new(program).args(args).status()?.success() {
        return Err(failure.into());
    }
    Ok(())
}

fn bump(level: &str) -> Result<String, Box<dyn Error>> {
    let (cargo, plugin) = read_manifests()?;
    let current = manifest_version(&cargo, true)?;
    let next = next_version(&current, level)?;
    let cargo_next = edit_version(&cargo, true, &next)?;
    let plugin_next = edit_version(&plugin, false, &next)?;
    fs::write("Cargo.toml", cargo_next)?;
    fs::write("herdr-plugin.toml", plugin_next)?;
    command_success(
        "cargo",
        &["check", "--quiet"],
        "cargo check failed after version bump",
    )?;
    Ok(next)
}

fn check(tag: &str) -> Result<(), Box<dyn Error>> {
    let (cargo, plugin) = read_manifests()?;
    let version = manifest_version(&cargo, true)?;
    let plugin_version = manifest_version(&plugin, false)?;
    if tag != format!("v{version}") || plugin_version != version {
        return Err("release tag, Cargo.toml, and herdr-plugin.toml versions must agree".into());
    }
    println!("release {tag} matches both manifests");
    Ok(())
}

fn require_clean_tracked_worktree() -> Result<(), Box<dyn Error>> {
    let output = Command::new("git")
        .args(["status", "--porcelain", "--untracked-files=no"])
        .output()?;
    if !output.status.success() {
        return Err("cannot inspect Git worktree".into());
    }
    if !output.stdout.is_empty() {
        return Err(
            "Git worktree has tracked changes; commit or stash them before releasing".into(),
        );
    }
    Ok(())
}

fn release(level: &str) -> Result<(), Box<dyn Error>> {
    require_clean_tracked_worktree()?;
    let version = bump(level)?;
    let tag = format!("v{version}");
    command_success(
        "git",
        &["add", "Cargo.toml", "Cargo.lock", "herdr-plugin.toml"],
        "git add failed",
    )?;
    command_success(
        "git",
        &["commit", "-m", &format!("Release {tag}")],
        "git commit failed",
    )?;
    let release_message = format!("Release {tag}");
    command_success(
        "git",
        &["tag", "-a", &tag, "-m", &release_message],
        "git tag failed",
    )?;
    command_success("git", &["push", "--follow-tags"], "git push failed")?;
    println!("released {tag}");
    Ok(())
}
