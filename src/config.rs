//! Configuration, theme and selection-history persistence.

use serde_json::{json, Map, Value};
use std::fs;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

const MAX_HISTORY: usize = 200;
const TOKENS: [&str; 19] = [
    "accent",
    "panel_bg",
    "sidebar_bg",
    "active_row_bg",
    "selection_bg",
    "surface0",
    "surface1",
    "surface_dim",
    "overlay0",
    "overlay1",
    "text",
    "subtext0",
    "mauve",
    "green",
    "yellow",
    "red",
    "blue",
    "teal",
    "peach",
];
const FALLBACK: [(&str, &str); 9] = [
    ("background", "#181825"),
    ("panel", "#313244"),
    ("text", "#cdd6f4"),
    ("muted", "#a6adc8"),
    ("accent", "#89b4fa"),
    ("error", "#f38ba8"),
    ("shortcut", "#94e2d5"),
    ("footer", "#1e1e2e"),
    ("footerText", "#6c7086"),
];

fn config_path() -> PathBuf {
    std::env::var_os("HERDR_CONFIG_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")))
                .join(".config/herdr/config.toml")
        })
}

/// Read the static command catalog and apply bare key remaps from config.toml.
pub fn load_items() -> Vec<Value> {
    let mut items = crate::catalog::default_items();
    let source = fs::read_to_string(config_path()).unwrap_or_default();
    let parsed = source.parse::<toml::Value>().ok();
    let mut remaps = Map::new();
    // Key bindings are top-level Herdr config keys. Preserve `prefix` in labels exactly as the
    // TypeScript palette does; an empty binding intentionally removes the shortcut.
    fn collect_remaps(value: &toml::Value, remaps: &mut Map<String, Value>) {
        if let toml::Value::Table(table) = value {
            for (key, value) in table {
                if let toml::Value::String(text) = value {
                    remaps.insert(key.clone(), Value::String(text.clone()));
                }
                collect_remaps(value, remaps);
            }
        }
    }
    if let Some(parsed) = parsed {
        collect_remaps(&parsed, &mut remaps);
    }
    for item in &mut items {
        let Some(id) = item.get("id").and_then(Value::as_str) else {
            continue;
        };
        let Some(binding) = remaps.get(id).and_then(Value::as_str) else {
            continue;
        };
        item["shortcuts"] = if binding.is_empty() {
            json!([])
        } else {
            json!([binding])
        };
    }
    items
}

fn read_source(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_default()
}

fn normalize_name(name: &str) -> String {
    let mut key = name.trim().to_ascii_lowercase().replace([' ', '_'], "-");
    let aliases = [
        ("catppuccin-mocha", "catppuccin"),
        ("latte", "catppuccin-latte"),
        ("light", "catppuccin-latte"),
        ("tokyonight", "tokyo-night"),
        ("tokyo-day", "tokyo-night-day"),
        ("tokyonight-day", "tokyo-night-day"),
        ("gruvbox-dark", "gruvbox"),
        ("onedark", "one-dark"),
        ("onelight", "one-light"),
        ("solarized-dark", "solarized"),
        ("lotus", "kanagawa-lotus"),
        ("rosepine", "rose-pine"),
        ("rosepine-dawn", "rose-pine-dawn"),
        ("dawn", "rose-pine-dawn"),
    ];
    if let Some((_, value)) = aliases.iter().find(|(alias, _)| *alias == key) {
        key = (*value).into();
    }
    key
}

fn color(value: &str, named_as_reset: bool) -> Option<String> {
    let value = value.trim().to_ascii_lowercase();
    if value.len() == 7
        && value.starts_with('#')
        && value[1..].chars().all(|c| c.is_ascii_hexdigit())
    {
        return Some(value);
    }
    if value.len() == 4
        && value.starts_with('#')
        && value[1..].chars().all(|c| c.is_ascii_hexdigit())
    {
        return Some(format!(
            "#{}{}{}{}{}{}",
            &value[1..2],
            &value[1..2],
            &value[2..3],
            &value[2..3],
            &value[3..4],
            &value[3..4]
        ));
    }
    if let Some(inner) = value.strip_prefix("rgb(").and_then(|v| v.strip_suffix(')')) {
        let parts: Vec<u8> = inner
            .split(',')
            .map(str::trim)
            .map(str::parse)
            .collect::<Result<_, _>>()
            .ok()?;
        if parts.len() == 3 {
            return Some(format!("#{:02x}{:02x}{:02x}", parts[0], parts[1], parts[2]));
        }
    }
    if named_as_reset || ["reset", "default", "none", "transparent"].contains(&value.as_str()) {
        return None;
    }
    let named = [
        ("black", "#000000"),
        ("red", "#cd0000"),
        ("green", "#00cd00"),
        ("yellow", "#cdcd00"),
        ("blue", "#0000ee"),
        ("magenta", "#cd00cd"),
        ("purple", "#cd00cd"),
        ("cyan", "#00cdcd"),
        ("white", "#e5e5e5"),
        ("gray", "#e5e5e5"),
        ("grey", "#e5e5e5"),
        ("darkgray", "#7f7f7f"),
        ("darkgrey", "#7f7f7f"),
        ("lightred", "#ff0000"),
        ("lightgreen", "#00ff00"),
        ("lightyellow", "#ffff00"),
        ("lightblue", "#5c5cff"),
        ("lightmagenta", "#ff00ff"),
        ("lightcyan", "#00ffff"),
    ];
    named
        .iter()
        .find(|(name, _)| *name == value)
        .map(|(_, hex)| (*hex).into())
}

fn neutral(light: bool) -> Map<String, Value> {
    let values = if light {
        [
            "#ffffff", "#e4e4e4", "#1a1a1a", "#6f6f6f", "#0000ee", "#cd0000", "#008080", "#f2f2f2",
            "#6f6f6f",
        ]
    } else {
        [
            "#000000", "#262626", "#e5e5e5", "#9e9e9e", "#5c5cff", "#ff5555", "#00cdcd", "#121212",
            "#7f7f7f",
        ]
    };
    FALLBACK
        .iter()
        .zip(values)
        .map(|((key, _), value)| ((*key).into(), json!(value)))
        .collect()
}

fn fallback_theme() -> Value {
    Value::Object(
        FALLBACK
            .iter()
            .map(|(k, v)| ((*k).into(), json!(v)))
            .collect(),
    )
}

fn host_is_light() -> bool {
    if let Ok(style) = std::env::var("AppleInterfaceStyle") {
        return !style.eq_ignore_ascii_case("dark");
    }
    if let Ok(hostname) = std::env::var("HOSTNAME") {
        if hostname.contains("-light") {
            return true;
        }
        if hostname.contains("-dark") {
            return false;
        }
    }
    #[cfg(target_os = "macos")]
    {
        if let Ok(output) = std::process::Command::new("defaults")
            .args(["read", "-g", "AppleInterfaceStyle"])
            .output()
        {
            if output.status.success() {
                return !String::from_utf8_lossy(&output.stdout)
                    .to_ascii_lowercase()
                    .contains("dark");
            }
            return true;
        }
        false
    }
    #[cfg(not(target_os = "macos"))]
    false
}

/// Resolve Herdr's configured theme into the nine colors consumed by the popup.
pub fn load_theme() -> Value {
    let source = read_source(&config_path());
    let parsed: toml::Value = match source.parse() {
        Ok(v) => v,
        Err(_) => return fallback_theme(),
    };
    let Some(theme) = parsed.get("theme").and_then(toml::Value::as_table) else {
        return fallback_theme();
    };
    let Some(name) = theme.get("name").and_then(toml::Value::as_str) else {
        return fallback_theme();
    };
    let auto = theme
        .get("auto_switch")
        .and_then(toml::Value::as_bool)
        .unwrap_or(false);
    let host_light = host_is_light();
    let canonical = normalize_name(name);
    let effective = if auto {
        let pair = [
            ("catppuccin", "catppuccin-latte"),
            ("tokyo-night", "tokyo-night-day"),
            ("gruvbox", "gruvbox-light"),
            ("one-dark", "one-light"),
            ("solarized", "solarized-light"),
            ("kanagawa", "kanagawa-lotus"),
            ("rose-pine", "rose-pine-dawn"),
        ];
        let selected: String = if let Some(explicit) = theme
            .get(if host_light {
                "light_name"
            } else {
                "dark_name"
            })
            .and_then(toml::Value::as_str)
        {
            explicit.to_string()
        } else {
            pair.iter()
                .find(|(dark, light)| *dark == canonical || *light == canonical)
                .map(|(dark, light)| {
                    if host_light {
                        theme
                            .get("light_name")
                            .and_then(toml::Value::as_str)
                            .unwrap_or(light)
                            .to_string()
                    } else {
                        theme
                            .get("dark_name")
                            .and_then(toml::Value::as_str)
                            .unwrap_or(dark)
                            .to_string()
                    }
                })
                .unwrap_or(canonical.clone())
        };
        normalize_name(&selected)
    } else {
        canonical
    };
    let palette: Value =
        serde_json::from_str(include_str!("herdr-palettes.json")).unwrap_or(Value::Null);
    let built = palette
        .get("themes")
        .and_then(|v| v.get(&effective))
        .or_else(|| palette.get("themes").and_then(|v| v.get("catppuccin")))
        .and_then(Value::as_object);
    let mut tokens = Map::new();
    if let Some(built) = built {
        for (key, value) in built {
            if let Some(value) = value
                .as_str()
                .and_then(|v| color(v, effective == "terminal"))
            {
                tokens.insert(key.clone(), Value::String(value));
            }
        }
    }
    let mut merge = |table: Option<&toml::map::Map<String, toml::Value>>| {
        if let Some(table) = table {
            for token in TOKENS {
                if let Some(value) = table.get(token).and_then(toml::Value::as_str) {
                    if let Some(value) = color(value, false) {
                        tokens.insert(token.into(), Value::String(value));
                    } else {
                        tokens.remove(token);
                    }
                }
            }
        }
    };
    merge(theme.get("custom").and_then(toml::Value::as_table));
    if auto {
        merge(
            theme
                .get("custom")
                .and_then(|v| v.get(if host_light { "light" } else { "dark" }))
                .and_then(toml::Value::as_table),
        );
    }
    let light_theme = [
        "catppuccin-latte",
        "tokyo-night-day",
        "gruvbox-light",
        "one-light",
        "solarized-light",
        "kanagawa-lotus",
        "rose-pine-dawn",
    ]
    .contains(&effective.as_str());
    let neutral = neutral(light_theme || (effective == "terminal" && host_light));
    let get = |token: &str, slot: &str| {
        tokens
            .get(token)
            .cloned()
            .unwrap_or_else(|| neutral.get(slot).cloned().unwrap_or(Value::Null))
    };
    let mut out = Map::new();
    out.insert("background".into(), get("panel_bg", "background"));
    out.insert("panel".into(), get("surface0", "panel"));
    out.insert("text".into(), get("text", "text"));
    out.insert("muted".into(), get("subtext0", "muted"));
    out.insert("accent".into(), get("accent", "accent"));
    out.insert("error".into(), get("red", "error"));
    out.insert("shortcut".into(), get("teal", "shortcut"));
    out.insert(
        "footer".into(),
        tokens
            .get("sidebar_bg")
            .cloned()
            .unwrap_or_else(|| get("surface_dim", "footer")),
    );
    out.insert("footerText".into(), get("overlay0", "footerText"));
    Value::Object(out)
}

fn history_path() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| PathBuf::from("."));
    std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".local/state"))
        .join("herdr-palette/history.json")
}
fn history_key(id: &str) -> String {
    if !id.starts_with("live:") {
        return id.into();
    }
    let socket = std::env::var("HERDR_SOCKET_PATH").unwrap_or_else(|_| "default".into());
    let encoded: String = socket
        .bytes()
        .flat_map(|b| {
            if b.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&b) {
                vec![b as char]
            } else {
                format!("%{b:02X}").chars().collect()
            }
        })
        .collect();
    format!("live@{encoded}:{id}")
}

fn valid_history(value: &Value) -> Vec<(String, f64)> {
    valid_history_map(value.as_object())
}

fn valid_history_map(object: Option<&Map<String, Value>>) -> Vec<(String, f64)> {
    let mut values: Vec<_> = object
        .into_iter()
        .flat_map(|obj| obj.iter())
        .filter_map(|(key, value)| {
            value
                .as_f64()
                .filter(|n| n.is_finite())
                .map(|n| (key.clone(), n))
        })
        .collect();
    values.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    values.truncate(MAX_HISTORY);
    values
}

pub fn load_history() -> Value {
    let value = fs::read_to_string(history_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(Value::Null);
    let prefix = format!(
        "live@{}:",
        std::env::var("HERDR_SOCKET_PATH")
            .unwrap_or_else(|_| "default".into())
            .bytes()
            .flat_map(
                |b| if b.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&b) {
                    vec![b as char]
                } else {
                    format!("%{b:02X}").chars().collect()
                }
            )
            .collect::<String>()
    );
    let map: Map<String, Value> = valid_history(&value)
        .into_iter()
        .filter(|(key, _)| !key.starts_with("live@") || key.starts_with(&prefix))
        .map(|(key, value)| (key, json!(value)))
        .collect();
    Value::Object(map)
}

pub fn record_selection(id: &str) {
    if id.is_empty() {
        return;
    }
    let path = history_path();
    // Read the complete file here. `load_history` intentionally hides live entries from other
    // Herdr sockets, while a write must preserve them for the session that created them.
    let raw = fs::read_to_string(&path)
        .ok()
        .and_then(|source| serde_json::from_str(&source).ok())
        .unwrap_or(Value::Null);
    let mut history: Map<String, Value> = valid_history(&raw)
        .into_iter()
        .map(|(key, value)| (key, json!(value)))
        .collect();
    let latest = valid_history_map(Some(&history))
        .into_iter()
        .map(|(_, value)| value)
        .fold(0.0, f64::max);
    let timestamp = chrono::Utc::now().timestamp_millis() as f64;
    let timestamp = timestamp.max(latest + 1.0);
    history.insert(history_key(id), json!(timestamp));
    let bounded: Map<String, Value> = valid_history_map(Some(&history))
        .into_iter()
        .map(|(key, value)| (key, json!(value)))
        .collect();
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let temporary = path.with_extension(format!(
        "json.{}.{}.tmp",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
    ));
    let Ok(mut file) = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temporary)
    else {
        return;
    };
    let bytes = serde_json::to_vec_pretty(&Value::Object(bounded)).unwrap_or_default();
    if file.write_all(&bytes).is_err() || fs::rename(&temporary, &path).is_err() {
        let _ = fs::remove_file(&temporary);
    }
}
