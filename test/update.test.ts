import { existsSync, mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { homedir, tmpdir } from "node:os";
import { join } from "node:path";
import { expect, test } from "bun:test";
import { checkForUpdate, dismissUpdate, installUpdate, setUpdateDependenciesForTests, type UpdateOffer } from "../src/update";

const source = (extra: Record<string, unknown> = {}): Record<string, unknown> => ({ kind: "github", owner: "mmjang", repo: "herdr-omni", resolved_commit: "abcdef1234567890", ...extra });
const listed = (sourceValue: Record<string, unknown> = source(), version = "0.5.1") => JSON.stringify({ result: { plugins: [{ plugin_id: "herdr-omni", name: "Herdr Omni", enabled: true, version, source: sourceValue }] } });
const result = (stdout: string, code = 0) => ({ code, stdout, stderr: "" });
const tempState = () => mkdtempSync(join(tmpdir(), "herdr-omni-update-"));

test("checks only the official enabled plugin and selects the highest stable tag", async () => {
  const stateDir = tempState();
  let listCalls = 0;
  let fetchCalls = 0;
  const restore = setUpdateDependenciesForTests({
    stateDir,
    runHerdr: async () => { listCalls++; return result(listed()); },
    runGit: async () => { fetchCalls++; return result("a refs/tags/v0.5.9\nb refs/tags/v1.0.0-rc.1\nc refs/tags/v0.6.0\n"); },
  });
  try {
    expect(await checkForUpdate("0.5.1")).toEqual({ version: "0.6.0", ref: "v0.6.0" });
    expect(await checkForUpdate("0.5.1")).toEqual({ version: "0.6.0", ref: "v0.6.0" });
    expect(fetchCalls).toBe(1);
    expect(listCalls).toBe(2);
  } finally { restore(); }
});

test("skips local and user-pinned installations", async () => {
  for (const pluginSource of [
    { kind: "path", path: "/tmp/herdr-omni" },
    source({ owner: "someone-else" }),
    source({ requested_ref: "v0.5.1" }),
  ]) {
    const restore = setUpdateDependenciesForTests({
      stateDir: tempState(),
      runHerdr: async () => result(listed(pluginSource)),
      runGit: async () => { throw new Error("network should not be used"); },
    });
    try { expect(await checkForUpdate("0.5.1")).toBeUndefined(); } finally { restore(); }
  }
});

test("snoozes one exact version for a day", async () => {
  const stateDir = tempState();
  let clock = 10_000;
  const offer: UpdateOffer = { version: "0.6.0", ref: "v0.6.0" };
  const restore = setUpdateDependenciesForTests({
    stateDir,
    now: () => clock,
    runHerdr: async () => result(listed()),
    runGit: async () => result(`sha refs/tags/${offer.ref}\n`),
  });
  try {
    dismissUpdate(offer);
    expect(await checkForUpdate("0.5.1")).toBeUndefined();
    clock += 24 * 60 * 60 * 1_000;
    expect(await checkForUpdate("0.5.1")).toEqual(offer);
  } finally { restore(); }
});

test("records updater ownership of a version-tag pin after install", async () => {
  const stateDir = tempState();
  let calls = 0;
  const offer: UpdateOffer = { version: "0.6.0", ref: "v0.6.0" };
  const restore = setUpdateDependenciesForTests({
    stateDir,
    runHerdr: async (argv, _timeout, cwd) => {
      expect(cwd).toBe(homedir());
      if (argv[1] === "install") {
        expect(argv).toEqual(["plugin", "install", "mmjang/herdr-omni", "--ref", "v0.6.0", "--yes"]);
        return result("");
      }
      calls++;
      return result(listed(calls === 1 ? source() : source({ requested_ref: offer.ref, resolved_commit: "fedcba9876543210" }), calls === 1 ? "0.5.1" : offer.version));
    },
    runGit: async () => result("sha refs/tags/v0.7.0\n"),
  });
  try {
    expect(await installUpdate(offer)).toEqual({ ok: true, message: "" });
    expect(JSON.parse(readFileSync(join(stateDir, "update.json"), "utf8")).updaterInstall).toEqual({ ref: offer.ref, resolved_commit: "fedcba9876543210" });
    expect(await checkForUpdate("0.5.1")).toEqual({ version: "0.7.0", ref: "v0.7.0" });
  } finally { restore(); }
});

test("offline checks fail quietly and can retry without caching a false negative", async () => {
  let checks = 0;
  const restore = setUpdateDependenciesForTests({
    stateDir: tempState(),
    runHerdr: async () => result(listed()),
    runGit: async () => ++checks === 1 ? result("", 1) : result("sha refs/tags/v0.6.0\n"),
  });
  try {
    expect(await checkForUpdate("0.5.1")).toBeUndefined();
    expect(await checkForUpdate("0.5.1")).toEqual({ version: "0.6.0", ref: "v0.6.0" });
  } finally { restore(); }
});

test("closing the popup cancels an in-flight check and ignores its later result", async () => {
  const controller = new AbortController();
  let ready!: () => void;
  let finish!: (result: { code: number; stdout: string; stderr: string }) => void;
  const started = new Promise<void>(resolve => { ready = resolve; });
  const restore = setUpdateDependenciesForTests({
    stateDir: tempState(),
    runHerdr: async () => result(listed()),
    runGit: async (_argv, _timeout, _cwd, signal) => {
      expect(signal).toBe(controller.signal);
      ready();
      return new Promise(resolve => { finish = resolve; });
    },
  });
  try {
    const check = checkForUpdate("0.5.1", controller.signal);
    await started;
    controller.abort();
    expect(await check).toBeUndefined();
    finish(result("sha refs/tags/v0.6.0\n"));
  } finally { restore(); }
});

test("rejects a pin changed after an updater install", async () => {
  const stateDir = tempState();
  writeFileSync(join(stateDir, "update.json"), JSON.stringify({ updaterInstall: { ref: "v0.6.0", resolved_commit: "old" } }));
  const restore = setUpdateDependenciesForTests({
    stateDir,
    runHerdr: async () => result(listed(source({ requested_ref: "v0.6.0", resolved_commit: "new" }), "0.6.0")),
    runGit: async () => { throw new Error("network should not be used"); },
  });
  try { expect(await checkForUpdate("0.5.1")).toBeUndefined(); } finally { restore(); }
});

test("cleans the lock after an install failure", async () => {
  const stateDir = tempState();
  const restore = setUpdateDependenciesForTests({
    stateDir,
    runHerdr: async argv => argv[1] === "install" ? result("", 1) : result(listed()),
  });
  try {
    expect((await installUpdate({ version: "0.6.0", ref: "v0.6.0" })).ok).toBe(false);
    expect(existsSync(join(stateDir, "update.lock"))).toBe(false);
  } finally { restore(); }
});

test("rejects a second install while the first one owns the lock", async () => {
  const stateDir = tempState();
  const offer = { version: "0.6.0", ref: "v0.6.0" } as const;
  let calls = 0;
  let release!: () => void;
  let entered!: () => void;
  const enteredPromise = new Promise<void>(resolve => { entered = resolve; });
  const restore = setUpdateDependenciesForTests({
    stateDir,
    runHerdr: async argv => {
      if (argv[1] === "install") return result("");
      calls++;
      if (calls === 1) {
        entered();
        return new Promise(resolve => { release = () => resolve(result(listed())); });
      }
      return result(listed(source({ requested_ref: offer.ref, resolved_commit: "new" }), offer.version));
    },
  });
  try {
    const first = installUpdate(offer);
    await enteredPromise;
    expect((await installUpdate(offer)).ok).toBe(false);
    release();
    expect(await first).toEqual({ ok: true, message: "" });
  } finally { restore(); }
});

test("rejects an offer that is no longer newer at install time", async () => {
  const stateDir = tempState();
  let installs = 0;
  const restore = setUpdateDependenciesForTests({
    stateDir,
    runHerdr: async argv => { if (argv[1] === "install") installs++; return result(listed(source(), "0.7.0")); },
  });
  try {
    expect((await installUpdate({ version: "0.6.0", ref: "v0.6.0" })).ok).toBe(false);
    expect(installs).toBe(0);
  } finally { restore(); }
});
