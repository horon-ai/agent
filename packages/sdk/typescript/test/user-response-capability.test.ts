import { test } from "bun:test";
import assert from "node:assert/strict";
import net from "node:net";
import os from "node:os";
import path from "node:path";
import fs from "node:fs";
import { HoronAgentClient, NdjsonDecoder } from "../dist/index.js";
import { inOrder } from "./mock-harness.ts";

/** One hello handshake against a fresh mock server, with the flag on or off. */
async function check(enabled: boolean): Promise<void> {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "horon-agent-response-test-"));
  const socketPath = path.join(dir, "api.sock");
  let hello: any;
  const server = net.createServer((socket) => {
    const decoder = new NdjsonDecoder();
    socket.on("data", (chunk) => {
      for (const frame of decoder.push(chunk)) {
        hello = frame;
        socket.write(
          `${JSON.stringify({
            v: 1,
            reply_to: hello.id,
            ev: "hello_ok",
            version: 1,
            server: "response-test",
            capabilities: [],
          })}\n`,
        );
      }
    });
  });
  await new Promise<void>((resolve) => server.listen(socketPath, resolve));
  let client: HoronAgentClient | undefined;
  try {
    client = await HoronAgentClient.connect({
      socketPath,
      ...(enabled ? { supportsUserResponse: true } : {}),
    });
    assert.equal(hello.req, "hello");
    assert.equal(hello.supports_user_response ?? false, enabled);
  } finally {
    void client?.close();
    await new Promise<void>((resolve) => server.close(() => resolve()));
    fs.rmSync(dir, { recursive: true, force: true });
  }
}

test("inline user response capability is explicitly opted in at hello", async () => {
  // Both configurations run in turn: each one needs its own socket and client,
  // so the steps are sequenced rather than awaited inside a loop.
  await inOrder([false, true], (enabled) => check(enabled));
});
