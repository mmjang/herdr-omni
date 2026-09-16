import { createInterface } from "node:readline";
for await (const line of createInterface({ input: process.stdin })) {
  const request = JSON.parse(line);
  if (request.id === undefined) continue;
  if (request.method === "hang") continue;
  if (request.method === "malformed") { console.log("not json"); continue; }
  if (request.method === "error") { console.log(JSON.stringify({ id: request.id, error: { message: "Unsupported" } })); continue; }
  // Stream chunks to cover transport framing, and interleave a notification.
  console.log(JSON.stringify({ method: "notice" }));
  const response = JSON.stringify({ id: request.id, result: { method: request.method } }) + "\n";
  process.stdout.write(response.slice(0, 5));
  await Bun.sleep(1);
  process.stdout.write(response.slice(5));
}
