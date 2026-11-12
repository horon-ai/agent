import { afterEach, test } from "bun:test";
import assert from "node:assert/strict";
import { HoronAgentClient, NdjsonDecoder } from "../dist/index.js";
import { inOrder, startMockHarness } from "./mock-harness.ts";

const teardown: Array<() => void | Promise<void>> = [];
afterEach(async () => {
  await inOrder(teardown.splice(0), (close) => Promise.resolve(close()));
});

const FAILED = { call_id: "c1", name: "bash", output: "exit status 2", error: "exit status 2", verdict: "failed" };

/** Runs one turn against a harness that answers `send_message` with the given `tool_done` events. */
async function turnWith(toolDone: Array<Record<string, unknown>>) {
  const server = await startMockHarness({
    onRequest(request, send) {
      if (request.req !== "send_message") return;
      send({ v: 1, reply_to: request.id, ev: "ok" });
      send({ v: 1, ev: "message_accepted", session_id: "s1" });
      for (const event of toolDone) send({ v: 1, ev: "tool_done", session_id: "s1", ...event });
      send({ v: 1, ev: "turn_done", session_id: "s1" });
    },
  });
  const client = await HoronAgentClient.connect({ socketPath: server.socketPath });
  teardown.push(async () => {
    await client.close();
    await server.close();
  });
  return client.run("s1", "hi");
}

test("a failed verdict reaches the tool call", async () => {
  const turn = await turnWith([FAILED]);

  assert.deepEqual(turn.toolCalls, [
    { callId: "c1", name: "bash", output: "exit status 2", error: "exit status 2", verdict: "failed" },
  ]);
});

test("a tool_done without the key has no verdict", async () => {
  const turn = await turnWith([{ call_id: "c1", name: "bash", output: "ok" }]);

  assert.equal(turn.toolCalls.length, 1);
  assert.equal(turn.toolCalls[0]?.verdict, undefined, "an absent verdict must not read as ok");
  assert.equal(
    Object.hasOwn(turn.toolCalls[0] ?? {}, "verdict"),
    false,
    "the call keeps the shape it had before the field",
  );
});

test("an unfamiliar verdict name arrives unchanged and the turn goes on", async () => {
  const turn = await turnWith([{ call_id: "c0", name: "bash", output: "later", verdict: "deferred" }, FAILED]);

  assert.deepEqual(
    turn.toolCalls.map((call) => call.verdict),
    ["deferred", "failed"],
    "a name from a newer daemon is passed through, and the call after it still arrives",
  );
});

/** The `tool_done` frame this SDK declared before protocol 1.19. */
interface ToolDoneBeforeVerdict {
  v: number;
  ev: "tool_done";
  session_id: string;
  call_id: string;
  name: string;
  output: string;
  error?: string;
}

test("a decoder that predates the verdict still reads the frame", () => {
  const line = `${JSON.stringify({ v: 1, ev: "tool_done", session_id: "s1", ...FAILED })}\n`;
  assert.ok(line.includes('"verdict":"failed"'), "the frame under test must carry the verdict");

  const frames = new NdjsonDecoder<ToolDoneBeforeVerdict>().push(line);

  assert.equal(frames.length, 1);
  const older = frames[0];
  assert.equal(older?.ev, "tool_done");
  assert.equal(older?.call_id, "c1");
  assert.equal(older?.name, "bash");
  assert.equal(older?.output, "exit status 2");
  assert.equal(older?.error, "exit status 2");
});
