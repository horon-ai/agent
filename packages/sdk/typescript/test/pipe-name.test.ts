import { test } from "bun:test";
import assert from "node:assert/strict";
import path from "node:path";
import { createHash } from "node:crypto";

/**
 * The SDK derives the Windows pipe name independently of `horon-agent-transport`,
 * because one is Rust and one is TypeScript. A drift between them is invisible
 * on Unix and total on Windows: the client dials a pipe nobody is listening on,
 * so nothing connects at all.
 *
 * `horon-agent-transport`'s `pipe_name_matches_the_typescript_sdk` asserts the same
 * literal strings from the other side.
 */

/** The derivation under test, with the platform's path parser injected. */
function derivePipeName(socketPath: string, parser: path.PlatformPath): string {
  const stem = (parser.parse(socketPath).name.match(/[A-Za-z0-9\-_]/g) ?? []).join("").slice(0, 32) || "horon-agent";
  const normalized = socketPath.replace(/\\/g, "/").toLowerCase();
  const hash = createHash("sha256").update(normalized).digest("hex").slice(0, 16);
  return `\\\\.\\pipe\\${stem}-${hash}`;
}

test("the Windows pipe name matches horon-agent-transport exactly", () => {
  // Parsed with win32 semantics regardless of the host, so the check runs in
  // CI on Linux rather than only on a Windows runner.
  for (const [socketPath, expected] of [
    [
      "C:\\Users\\user\\AppData\\Local\\horon-agent\\run\\horon-agent-api.sock",
      "\\\\.\\pipe\\horon-agent-api-249ba7051747c288",
    ],
    ["C:\\a\\b\\horon-agent.sock", "\\\\.\\pipe\\horon-agent-69098f328f40a6df"],
  ] as const) {
    assert.equal(
      derivePipeName(socketPath, path.win32),
      expected,
      `pipe name for ${socketPath} drifted from horon-agent-transport`,
    );
  }
});

test("case and separators normalize the same way", () => {
  assert.equal(
    derivePipeName("C:\\Temp\\horon-agent\\server.sock", path.win32),
    derivePipeName("c:/temp/HORON-AGENT/server.sock", path.win32),
    "the pipe name must not depend on case or separator style",
  );
});

test("a path with no usable characters still yields a name", () => {
  const derived = derivePipeName("/tmp/!!!.sock", path.posix);
  assert.match(derived, /^\\\\\.\\pipe\\horon-agent-[0-9a-f]{16}$/);
});
