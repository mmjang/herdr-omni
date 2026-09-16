import { expect, test } from "bun:test";
import { OpenCodeHistory } from "../src/opencode-history";

function history(script: string, timeout = 2000) {
  return new OpenCodeHistory([process.execPath, "-e", script, "--"], timeout);
}
const row = { id: "ses_test123", title: "Example", time: { updated: 123456 }, directory: "/repo/example" };

function server(pages: unknown[]) {
  return history(`
    if (JSON.stringify(process.argv.slice(1)) !== JSON.stringify(["serve","--pure","--hostname","127.0.0.1","--port","0"])) process.exit(2);
    const pages = ${JSON.stringify(pages)};
    const s = Bun.serve({hostname:"127.0.0.1",port:0,fetch(req){
      const u=new URL(req.url);
      if(u.pathname!=="/experimental/session" || u.searchParams.has("directory") || u.searchParams.get("roots")!=="true") return new Response("",{status:400});
      if(req.headers.get("authorization")!=="Basic "+Buffer.from("omni:"+process.env.OPENCODE_SERVER_PASSWORD).toString("base64")) return new Response("",{status:401});
      const n=Number(u.searchParams.get("cursor")||0);
      return Response.json(pages[n],{headers:n+1<pages.length?{"x-next-cursor":String(n+1)}:{}});
    }});
    console.log("opencode server listening on http://127.0.0.1:"+s.port);
  `);
}

test("OpenCode lists validated CLI records with millisecond timestamps", async () => {
  const rows = [row, row, null, { ...row, id: "--flag" }, { ...row, id: "ses_bad", time: { updated: "123" } }];
  const adapter = server([rows]);
  expect(await adapter.list()).toEqual([{ provider: "opencode", id: row.id, title: row.title, cwd: row.directory, updatedAt: row.time.updated }]);
  adapter.close();
});

test("OpenCode accepts empty history and rejects malformed list output", async () => {
  expect(await server([[]]).list()).toEqual([]);
  await expect(server([{}]).list()).rejects.toThrow();
});

test("OpenCode paginates global history across projects including deleted worktrees", async () => {
  const first = Array.from({length: 1000}, (_, i) => ({...row, id: `ses_${i}`}));
  const adapter = server([first, [{...row, id:"ses_deleted",directory:"/deleted/worktree"}]]);
  const sessions = await adapter.list();
  expect(sessions).toHaveLength(1001);
  expect(sessions.at(-1)?.cwd).toBe("/deleted/worktree");
  adapter.close();
});

test("OpenCode exports conversation text without tool input or output", async () => {
  const data = { info: { id: row.id }, messages: [
    { info: { role: "user" }, parts: [{ type: "text", text: "Question" }, { type: "text", text: "Hidden", synthetic: true }] },
    { info: { role: "assistant" }, parts: [
      { type: "text", text: "Answer" }, { type: "reasoning", text: "Private reasoning" },
      { type: "text", text: "Ignored", ignored: true },
      { type: "tool", state: { status: "completed", input: { secret: "input" }, output: "Tool result" } },
      { type: "tool", state: { status: "running", output: "Not complete" } },
      { type: "file", url: "private-file" }, null,
    ] },
    { info: { role: "system" }, parts: [{ type: "text", text: "System" }] },
  ] };
  const adapter = history(`if (JSON.stringify(process.argv.slice(1)) !== JSON.stringify(["export", "ses_test123"])) process.exit(2); console.error("Exporting session"); console.log(${JSON.stringify(JSON.stringify(data))})`);
  expect(await adapter.read(row.id)).toEqual(["Question", "Answer"]);
  await expect(adapter.read("--evil")).rejects.toThrow("Invalid OpenCode session ID");
  adapter.close();
});

test("OpenCode rejects wrong exports, missing executable and nonzero exits", async () => {
  await expect(history('console.log(JSON.stringify({info:{id:"ses_other"},messages:[]}))').read(row.id)).rejects.toThrow("Invalid OpenCode session export");
  await expect(new OpenCodeHistory(["/nonexistent/opencode-test"]).list()).rejects.toThrow("unavailable");
  await expect(history("process.exit(2)").list()).rejects.toThrow("command failed");
});

test("OpenCode times out and close cancels every pending request", async () => {
  await expect(history("setInterval(() => {}, 1000)", 50).list()).rejects.toThrow("timed out");
  const adapter = history("setInterval(() => {}, 1000)");
  const first = adapter.list(), second = adapter.list();
  const checks = Promise.allSettled([first, second]);
  adapter.close();
  for (const result of await checks) {
    expect(result.status).toBe("rejected");
    if (result.status === "rejected") expect(result.reason.message).toContain("closed");
  }
  await expect(adapter.list()).rejects.toThrow("closed");
});
