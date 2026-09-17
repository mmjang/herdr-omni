import { readFileSync } from "node:fs";
import palettes from "./herdr-palettes.json";

/** Colors the popup paints with, all resolved from Herdr's own theme tokens. */
export interface PaletteTheme {
  background: string;
  panel: string;
  text: string;
  muted: string;
  accent: string;
  /** Error/danger emphasis, resolved from Herdr's `red` token. */
  error: string;
  shortcut: string;
  footer: string;
  footerText: string;
}

/**
 * Herdr's own default palette (catppuccin). Used when config is unreadable, and as the base for
 * theme resolution.
 */
export const fallbackTheme: PaletteTheme = {
  background: "#181825", panel: "#313244", text: "#cdd6f4", muted: "#a6adc8",
  accent: "#89b4fa", error: "#f38ba8", shortcut: "#94e2d5", footer: "#1e1e2e", footerText: "#6c7086",
};

// The base for theme resolution and unknown-name fallback, read straight off the vendored palettes.
const FALLBACK_TOKENS = palettes.themes.catppuccin as Record<string, string>;

// The tokens Herdr accepts in `[theme.custom]`, straight from the vendored palettes.
const TOKENS = palettes.tokens;

// The aliases Herdr accepts (`canonical_theme_name`), vendored alongside the themes they point at.
const THEME_ALIASES = palettes.aliases as Record<string, string>;

function builtinTheme(name: string): Record<string, string> | undefined {
  return (palettes.themes as Record<string, Record<string, string>>)[normalizeThemeName(name)];
}

/** Mirror of Herdr's `canonical_theme_name`: case-folded, spaces/underscores as dashes, aliases accepted. */
export function normalizeThemeName(name: string): string {
  const key = name.trim().toLowerCase().replace(/[ _]+/g, "-");
  return THEME_ALIASES[key] ?? key;
}

/**
 * Built-in light/dark partner pairs, mirroring Herdr's `sibling_theme_names`. Families not
 * listed here (dracula, nord, vesper, terminal) have a single flavour and keep their own name.
 */
const THEME_PAIRS: ReadonlyArray<readonly [dark: string, light: string]> = [
  ["catppuccin", "catppuccin-latte"],
  ["tokyo-night", "tokyo-night-day"],
  ["gruvbox", "gruvbox-light"],
  ["one-dark", "one-light"],
  ["solarized", "solarized-light"],
  ["kanagawa", "kanagawa-lotus"],
  ["rose-pine", "rose-pine-dawn"],
];

/** The family's `[dark, light]` pair, or `[name, name]` when the theme stands alone. */
function themePair(name: string): readonly [string, string] {
  const canonical = normalizeThemeName(name);
  return THEME_PAIRS.find(([dark, light]) => dark === canonical || light === canonical) ?? [canonical, canonical];
}

/** Light-flavoured built-ins; anything else — including unknown names — reads as dark. */
function isLightName(name: string): boolean {
  const canonical = normalizeThemeName(name);
  return THEME_PAIRS.some(([, light]) => light === canonical);
}

/** ratatui's ANSI color slots (0-15) at their xterm default RGB values. */
const NAMED_COLORS: Record<string, string> = {
  black: "#000000", red: "#cd0000", green: "#00cd00", yellow: "#cdcd00", blue: "#0000ee",
  magenta: "#cd00cd", purple: "#cd00cd", cyan: "#00cdcd", white: "#e5e5e5",
  gray: "#e5e5e5", grey: "#e5e5e5", darkgray: "#7f7f7f", darkgrey: "#7f7f7f",
  lightred: "#ff0000", lightgreen: "#00ff00", lightyellow: "#ffff00", lightblue: "#5c5cff",
  lightmagenta: "#ff00ff", lightcyan: "#00ffff",
};

const HEX = /^#[0-9a-f]{6}$/;

function isHexColor(value: string | undefined): value is string {
  return value !== undefined && HEX.test(value.trim().toLowerCase());
}

const RESET_ALIASES = ["reset", "default", "none", "transparent"];

/**
 * Resolve one Herdr color value — hex, `#rgb`, `rgb(r,g,b)`, a name, or a reset alias — to hex.
 * Reset aliases return undefined: the value is the host terminal's own color, which a plugin
 * pane cannot query (Herdr answers no OSC 4/10/11), so the caller picks a neutral instead.
 * With `namedAsReset`, ANSI names (black, gray, lightblue, …) are treated the same way, because
 * their xterm values assume a background we also cannot see.
 */
export function resolveColor(value: string, namedAsReset = false): string | undefined {
  const text = value.trim().toLowerCase();
  if (HEX.test(text)) return text;
  const short = /^#([0-9a-f])([0-9a-f])([0-9a-f])$/.exec(text);
  if (short) return "#" + short.slice(1, 4).map(char => char + char).join("");
  const rgb = /^rgb\(\s*(\d{1,3})\s*,\s*(\d{1,3})\s*,\s*(\d{1,3})\s*\)$/.exec(text);
  if (rgb && rgb.slice(1, 4).every(part => Number(part) < 256)) {
    return "#" + rgb.slice(1, 4).map(part => Number(part).toString(16).padStart(2, "0")).join("");
  }
  if (RESET_ALIASES.includes(text)) return undefined;
  return namedAsReset ? undefined : NAMED_COLORS[text];
}

/** `[theme] name` plus the optional `light_name`/`dark_name` pair used by `auto_switch`. */
export interface ThemeSelection {
  name: string;
  autoSwitch: boolean;
  darkName?: string;
  lightName?: string;
}

/** Read just the `[theme]`, `[theme.custom]`, and `[theme.custom.light|dark]` sections of a config.toml. */
export function parseThemeConfig(source: string): { selection?: ThemeSelection; custom: Map<string, string>; customLight: Map<string, string>; customDark: Map<string, string> } {
  const custom = new Map<string, string>();
  const customLight = new Map<string, string>();
  const customDark = new Map<string, string>();
  let section = "";
  let name: string | undefined, darkName: string | undefined, lightName: string | undefined, autoSwitch = false;
  for (const line of source.split("\n")) {
    const header = /^\s*\[([^\]]+)\]\s*(?:#.*)?$/.exec(line)?.[1];
    if (header) { section = header.trim(); continue; }
    if (section !== "theme" && !section.startsWith("theme.custom")) continue;
    const entry = /^\s*([a-z_][a-z0-9_]*)\s*=\s*(.*)$/.exec(line);
    const [, key, rawValue] = entry ?? [];
    if (!key || !rawValue) continue;
    const value = tomlValue(rawValue);
    if (!value) continue;
    if (section === "theme.custom") custom.set(key, value);
    else if (section === "theme.custom.light" && TOKENS.includes(key)) customLight.set(key, value);
    else if (section === "theme.custom.dark" && TOKENS.includes(key)) customDark.set(key, value);
    else if (section === "theme") {
      if (key === "name") name = value;
      else if (key === "auto_switch") autoSwitch = value === "true";
      else if (key === "dark_name") darkName = value;
      else if (key === "light_name") lightName = value;
    }
  }
  return { selection: name ? { name, autoSwitch, darkName, lightName } : undefined, custom, customLight, customDark };
}

/** A basic TOML string's contents, or a bare value with its trailing comment removed. */
function tomlValue(raw: string): string {
  const text = raw.trim();
  const quoted = /^["']([^"']*)["']/.exec(text);
  if (quoted) return quoted[1]!.trim();
  return text.replace(/\s+#.*$/, "").trim();
}

/** The theme Herdr is most likely painting with right now. */
export function effectiveThemeName(selection: ThemeSelection, hostLight: boolean): string {
  if (!selection.autoSwitch) return normalizeThemeName(selection.name);
  const [dark, light] = themePair(selection.name);
  // Without an explicit partner, Herdr uses the built-in sibling of `name`.
  return normalizeThemeName(hostLight ? selection.lightName ?? light : selection.darkName ?? dark);
}

/**
 * Host light appearance. macOS only writes `AppleInterfaceStyle` in dark mode, so a missing key —
 * `defaults` exiting non-zero — is what light looks like. `dark-notify` names its host
 * `<user>-light-<device>` after the same setting, which is the fallback for a missing `defaults`.
 */
export function isHostLight(env: NodeJS.ProcessEnv = process.env, hostName = env.HOSTNAME ?? ""): boolean {
  if (env.AppleInterfaceStyle !== undefined) return env.AppleInterfaceStyle.toLowerCase() !== "dark";
  if (/-light\b/.test(hostName)) return true;
  if (/-dark\b/.test(hostName)) return false;
  try {
    const child = Bun.spawnSync(["defaults", "read", "-g", "AppleInterfaceStyle"], { stdout: "pipe", stderr: "ignore" });
    // A non-zero exit means the key is absent, which is how a light-mode macOS reads.
    if (child.exitCode !== 0) return true;
    return !/dark/i.test(child.stdout.toString());
  } catch { return false; }
}

/** Neutral colors for `reset` tokens, whose real value is the host terminal's own color. */
export function hostTheme(light: boolean): PaletteTheme {
  return light
    ? { background: "#ffffff", panel: "#e4e4e4", text: "#1a1a1a", muted: "#6f6f6f", accent: "#0000ee", error: "#cd0000", shortcut: "#008080", footer: "#f2f2f2", footerText: "#6f6f6f" }
    : { background: "#000000", panel: "#262626", text: "#e5e5e5", muted: "#9e9e9e", accent: "#5c5cff", error: "#ff5555", shortcut: "#00cdcd", footer: "#121212", footerText: "#7f7f7f" };
}

/** Map resolved Herdr tokens onto the popup's color slots, mirroring Herdr's own surfaces. */
export function composeTheme(tokens: Record<string, string>, neutral: PaletteTheme = fallbackTheme): PaletteTheme {
  const color = (token: string, slot: keyof PaletteTheme) => (isHexColor(tokens[token]) ? tokens[token].trim().toLowerCase() : neutral[slot]);
  return {
    background: color("panel_bg", "background"),
    panel: color("surface0", "panel"),
    text: color("text", "text"),
    muted: color("subtext0", "muted"),
    accent: color("accent", "accent"),
    error: color("red", "error"),
    shortcut: color("teal", "shortcut"),
    // Herdr keeps the sidebar on the terminal background unless `sidebar_bg` names it; the
    // popup floats over a pane, so a reset sidebar falls back to the dim surface instead.
    footer: isHexColor(tokens.sidebar_bg) ? tokens.sidebar_bg.trim().toLowerCase() : color("surface_dim", "footer"),
    footerText: color("overlay0", "footerText"),
  };
}

/**
 * The palette Herdr is rendering with: `[theme] name` (or its `auto_switch` pick) overlaid by
 * `[theme.custom]`, composed the way Herdr composes them. `reset` tokens are unknowable from a
 * plugin pane, so they get neutral colors chosen from the theme's own light/dark family.
 */
export function loadTheme(path = process.env.HERDR_CONFIG_PATH ?? `${process.env.HOME}/.config/herdr/config.toml`, hostLight: () => boolean = isHostLight): PaletteTheme {
  let source = "";
  try { source = readFileSync(path, "utf8"); } catch { return fallbackTheme; }
  const { selection, custom, customLight, customDark } = parseThemeConfig(source);
  if (!selection) return fallbackTheme;
  // Host appearance is only consulted when auto_switch picks a partner, `terminal` needs neutrals,
  // or a `[theme.custom.light|dark]` block applies — Herdr layers those the same way.
  let light: boolean | undefined;
  const hostIsLight = () => (light ??= hostLight());
  const name = selection.autoSwitch ? effectiveThemeName(selection, hostIsLight()) : normalizeThemeName(selection.name);
  // A `terminal` theme follows the host appearance; built-in light families need light fills.
  const neutral = hostTheme(isLightName(name) || (name === "terminal" && hostIsLight()));
  // The mode block applies only under auto_switch, on top of the shared overrides (Herdr PR #2324).
  const mode = selection.autoSwitch ? (hostIsLight() ? customLight : customDark) : new Map<string, string>();
  return themeFor(name, custom, neutral, mode);
}

/**
 * A named theme with `[theme.custom]` overrides on top. `reset` — and, for the `terminal` theme,
 * ANSI names too — means "the host terminal's own color", unknowable from a plugin pane, so those
 * slots fall to `neutral`. Explicit custom values always win, ANSI names included.
 */
export function themeFor(name: string, custom: Map<string, string> = new Map(), neutral: PaletteTheme = fallbackTheme, modeCustom: Map<string, string> = new Map()): PaletteTheme {
  const tokens: Record<string, string> = {};
  const merge = (source: Record<string, string>, namedAsReset: boolean) => {
    for (const [token, value] of Object.entries(source)) {
      if (!TOKENS.includes(token)) continue;
      const resolved = resolveColor(value, namedAsReset);
      if (resolved) tokens[token] = resolved;
      else delete tokens[token]; // an explicit `reset` erases the base value so the neutral fills in
    }
  };
  merge(builtinTheme(name) ?? FALLBACK_TOKENS, normalizeThemeName(name) === "terminal");
  merge(Object.fromEntries(custom), false);
  merge(Object.fromEntries(modeCustom), false);
  return composeTheme(tokens, neutral);
}
