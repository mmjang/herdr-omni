import { expect, test } from "bun:test";
import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import palettes from "../src/herdr-palettes.json";
import {
  composeTheme, effectiveThemeName, loadTheme, normalizeThemeName,
  parseThemeConfig, resolveColor, themeFor, fallbackTheme, hostTheme,
} from "../src/theme";

/** Every token name Herdr accepts in `[theme.custom]`, straight from the vendored palettes. */
const TOKENS = palettes.tokens;

test("reads the theme name and custom tokens from config.toml", () => {
  const { selection, custom } = parseThemeConfig(`
[theme]
name = "catppuccin-latte"
auto_switch = false

[theme.custom]
accent = "#1c6b48"
panel_bg = "#f1f0e9"
surface0 = "#e7e5db"
overlay1 = "#999999"
subtext0 = "#999999"

[ui.toast]
delivery = "system"
`);

  expect(selection).toEqual({ name: "catppuccin-latte", autoSwitch: false, darkName: undefined, lightName: undefined });
  // Tokens with digits in their names must survive the key pattern.
  expect([...custom]).toEqual([["accent", "#1c6b48"], ["panel_bg", "#f1f0e9"], ["surface0", "#e7e5db"], ["overlay1", "#999999"], ["subtext0", "#999999"]]);
});

test("keeps top-level keys out of the theme section", () => {
  const { selection } = parseThemeConfig('onboarding = false\nname = "not-a-theme"\n[theme]\nname = "nord"\n');
  expect(selection?.name).toBe("nord");
});

test("tolerates trailing comments and unquoted values", () => {
  const { selection, custom } = parseThemeConfig('[theme]\nname = "nord" # built-in\nauto_switch = true # follow the host\n\n[theme.custom]\naccent = "#f5c2e7"\n');
  expect(selection).toEqual({ name: "nord", autoSwitch: true, darkName: undefined, lightName: undefined });
  expect([...custom]).toEqual([["accent", "#f5c2e7"]]);
});

test("accepts trailing comments on table headers without leaking sections", () => {
  const { selection, custom } = parseThemeConfig(`
[theme] # follow desktop
name = "nord"
[theme.custom]# overrides
accent = "#ff0000"
[ui] # unrelated settings
name = "dracula"
accent = "#00ff00"
`);
  expect(selection?.name).toBe("nord");
  expect([...custom]).toEqual([["accent", "#ff0000"]]);
});

test("resolves every color shape Herdr accepts", () => {
  expect(resolveColor("#AB12CD")).toBe("#ab12cd");
  expect(resolveColor("#abc")).toBe("#aabbcc");
  expect(resolveColor("rgb(255, 85, 85)")).toBe("#ff5555");
  expect(resolveColor("lightblue")).toBe("#5c5cff");
  expect(resolveColor("reset")).toBeUndefined();
  expect(resolveColor("rgb(999, 0, 0)")).toBeUndefined();
  expect(resolveColor("chartreuse")).toBeUndefined();
});

test("canonicalizes theme names like Herdr does", () => {
  expect(normalizeThemeName("Catppuccin Mocha")).toBe("catppuccin");
  expect(normalizeThemeName("tokyo_night")).toBe("tokyo-night");
  expect(normalizeThemeName("Latte")).toBe("catppuccin-latte");
  expect(normalizeThemeName("Dawn")).toBe("rose-pine-dawn");
});

test("follows auto_switch to the light or dark partner theme", () => {
  const selection = { name: "catppuccin", autoSwitch: true, darkName: "tokyo-night", lightName: "catppuccin-latte" };
  expect(effectiveThemeName(selection, true)).toBe("catppuccin-latte");
  expect(effectiveThemeName(selection, false)).toBe("tokyo-night");
  // Without an explicit pair, Herdr falls back to the built-in sibling of the base name.
  expect(effectiveThemeName({ name: "tokyo-night", autoSwitch: true }, true)).toBe("tokyo-night-day");
  expect(effectiveThemeName({ name: "tokyo-night", autoSwitch: true }, false)).toBe("tokyo-night");
  // one-dark carries its suffix on the dark side; the pair is explicit, not suffix-derived.
  expect(effectiveThemeName({ name: "one-dark", autoSwitch: true }, true)).toBe("one-light");
  expect(effectiveThemeName({ name: "one-light", autoSwitch: true }, false)).toBe("one-dark");
  expect(effectiveThemeName({ name: "gruvbox", autoSwitch: false }, true)).toBe("gruvbox");
});

test("overlays [theme.custom] on top of the built-in palette", () => {
  const nord = themeFor("nord");
  const overridden = themeFor("nord", new Map([["accent", "#ff0000"], ["panel_bg", "#f1f0e9"]]));
  expect(overridden.accent).toBe("#ff0000");
  expect(overridden.background).toBe("#f1f0e9");
  expect(overridden.text).toBe(nord.text);
});

test("a custom reset token falls back to the neutral host colors, not the base theme", () => {
  const neutral = hostTheme(true);
  const explicit = themeFor("nord", new Map([["panel_bg", "reset"]]), neutral);
  expect(explicit.background).toBe(neutral.background);
  expect(themeFor("nord", new Map([["panel_bg", "#123456"]]), neutral).background).toBe("#123456");
});

test("the terminal theme paints with neutral host colors", () => {
  const light = themeFor("terminal", new Map(), hostTheme(true));
  expect(light).toEqual(hostTheme(true));
  // Explicit values still win, even in ANSI-name form.
  expect(themeFor("terminal", new Map([["accent", "yellow"]]), hostTheme(true)).accent).toBe("#cdcd00");
});

test("maps Herdr surfaces onto the popup slots", () => {
  // A distinct value per token pins each slot to the token it should follow.
  const tokens = Object.fromEntries(TOKENS.map((token, index) => [token, `#${index.toString(16).padStart(2, "0")}${index.toString(16).padStart(2, "0")}${index.toString(16).padStart(2, "0")}`]));
  const theme = composeTheme(tokens);
  expect(theme).toEqual({
    background: tokens.panel_bg, panel: tokens.surface0, text: tokens.text, muted: tokens.subtext0,
    accent: tokens.accent, error: tokens.red, shortcut: tokens.teal, footer: tokens.sidebar_bg, footerText: tokens.overlay0,
  });
  // An unset or reset sidebar background takes the dim surface instead of the popup background.
  const { sidebar_bg, ...withoutSidebar } = tokens;
  expect(composeTheme(withoutSidebar).footer).toBe(tokens.surface_dim);
});

test("uses neutral colors for slots a theme cannot answer", () => {
  const neutral = hostTheme(true);
  expect(composeTheme({}, neutral)).toEqual(neutral);
});

test("loads the effective theme from a config file", () => {
  const path = join(mkdtempSync(join(tmpdir(), "herdr-palette-theme-")), "config.toml");
  writeFileSync(path, '[theme]\nname = "catppuccin-latte"\n\n[theme.custom]\naccent = "#1c6b48"\nteal = "#2993a3"\n');

  const theme = loadTheme(path, () => false);
  expect(theme.accent).toBe("#1c6b48");
  expect(theme.shortcut).toBe("#2993a3");
  // Untouched tokens keep the light partner theme's own values.
  expect(theme.background).toBe(themeFor("catppuccin-latte").background);
  expect(theme.muted).toBe(themeFor("catppuccin-latte").muted);
});

test("reads per-mode override blocks into their own maps", () => {
  const { custom, customLight, customDark } = parseThemeConfig(`
[theme]
name = "catppuccin-latte"
auto_switch = true

[theme.custom]
accent = "#fe640b"

[theme.custom.light]
text = "#2e3145"

[theme.custom.dark] # comments apply here too
text = "#cdd6f4"
not_a_token = "ignored"

[theme.custom.other]
text = "#ff0000"
`);
  expect([...custom]).toEqual([["accent", "#fe640b"]]);
  expect([...customLight]).toEqual([["text", "#2e3145"]]);
  expect([...customDark]).toEqual([["text", "#cdd6f4"]]);
});

test("follows an auto_switch config to the host appearance", () => {
  const path = join(mkdtempSync(join(tmpdir(), "herdr-palette-theme-")), "config.toml");
  writeFileSync(path, '[theme]\nname = "catppuccin"\nauto_switch = true\ndark_name = "tokyo-night"\nlight_name = "catppuccin-latte"\n');

  expect(loadTheme(path, () => true).text).toBe(themeFor("catppuccin-latte").text);
  expect(loadTheme(path, () => false).text).toBe(themeFor("tokyo-night").text);
});

test("layers per-mode overrides over the shared ones by host appearance", () => {
  const path = join(mkdtempSync(join(tmpdir(), "herdr-palette-theme-")), "config.toml");
  writeFileSync(path, `
[theme]
name = "catppuccin-latte"
auto_switch = true
light_name = "catppuccin-latte"
dark_name = "tokyo-night"

[theme.custom]
accent = "#111111"

[theme.custom.light]
accent = "#222222"

[theme.custom.dark]
accent = "#333333"
`);

  expect(loadTheme(path, () => true).accent).toBe("#222222");
  expect(loadTheme(path, () => false).accent).toBe("#333333");
});

test("manual mode ignores per-mode blocks, like Herdr", () => {
  const path = join(mkdtempSync(join(tmpdir(), "herdr-palette-theme-")), "config.toml");
  writeFileSync(path, '[theme]\nname = "nord"\n\n[theme.custom.light]\naccent = "#222222"\n');

  expect(loadTheme(path, () => true).accent).toBe(themeFor("nord").accent);
});

test("the vendored palettes carry exactly the known tokens with resolvable colors", () => {
  // The file is maintained by hand against Herdr's state.rs, so pin its shape: every theme has
  // every token, and every value is a hex color, an ANSI name, or `reset`.
  expect(Object.keys(palettes.themes).length).toBeGreaterThan(0);
  for (const [name, tokens] of Object.entries(palettes.themes)) {
    expect(Object.keys(tokens).sort(), name).toEqual([...TOKENS].sort());
    for (const [token, value] of Object.entries(tokens)) {
      expect(value === "reset" || resolveColor(value) !== undefined, `${name}.${token}: ${value}`).toBe(true);
    }
  }
  // Aliases mirror Herdr's canonical_theme_name and must point at vendored themes.
  for (const [alias, target] of Object.entries(palettes.aliases)) {
    expect(palettes.themes, alias).toHaveProperty(target);
  }
});

test("falls back to Herdr's default palette when config is missing or themeless", () => {
  expect(loadTheme("/nonexistent/config.toml")).toEqual(fallbackTheme);
  const path = join(mkdtempSync(join(tmpdir(), "herdr-palette-theme-")), "config.toml");
  writeFileSync(path, 'prefix = "ctrl+a"\n');
  expect(loadTheme(path)).toEqual(fallbackTheme);
});
