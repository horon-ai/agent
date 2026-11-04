import { test } from "bun:test";
import assert from "node:assert/strict";
import {
  FrameTooLargeError,
  HoronAgentClient,
  MAX_FRAME_BYTES,
  MAX_RESIDUAL_BYTES,
  NdjsonDecoder,
} from "../dist/index.js";
import { startMockHarness } from "./mock-harness.ts";

/** Korean + emoji + a CJK character: every one is multi-byte in UTF-8. */
const SPLIT_PAYLOAD = "가나다😀한글";

function frameFor(text: string): Buffer {
  return Buffer.from(`${JSON.stringify({ v: 1, ev: "text_delta", session_id: "s1", text })}\n`, "utf8");
}

function rejection(run: () => void, limit: "frame" | "remainder"): FrameTooLargeError {
  let caught: unknown;
  try {
    run();
  } catch (error) {
    caught = error;
  }
  if (!(caught instanceof FrameTooLargeError)) {
    throw new Error(`expected FrameTooLargeError, got ${String(caught)}`);
  }
  assert.equal(caught.limit, limit);
  return caught;
}

type Event = { ev: string; [key: string]: unknown };

async function runFrames(events: Event[]) {
  const server = await startMockHarness({
    onRequest(request, send) {
      if (request.req !== "send_message") return;
      send({ v: 1, ev: "message_accepted", session_id: "s1" });
      for (const event of events) send({ v: 1, session_id: "s1", ...event });
      send({ v: 1, ev: "turn_done", session_id: "s1" });
    },
  });
  const client = await HoronAgentClient.connect({ socketPath: server.socketPath });
  try {
    return await client.run("s1", "investigate");
  } finally {
    await client.close();
    await server.close();
  }
}

test("a frame split at every byte offset decodes to the original text", () => {
  const wire = frameFor(SPLIT_PAYLOAD);
  const expected = [{ v: 1, ev: "text_delta", session_id: "s1", text: SPLIT_PAYLOAD }];
  for (let split = 1; split < wire.length; split += 1) {
    const decoder = new NdjsonDecoder();
    const frames = [...decoder.push(wire.subarray(0, split)), ...decoder.push(wire.subarray(split))];
    assert.deepEqual(frames, expected, `split at byte ${split} of ${wire.length}`);
    assert.equal((frames[0] as { text: string }).text, SPLIT_PAYLOAD, `split at byte ${split}`);
    assert.ok(!JSON.stringify(frames).includes("\uFFFD"), `split at byte ${split} produced U+FFFD`);
  }
});

test("a frame split byte by byte decodes to the original text", () => {
  const wire = frameFor(SPLIT_PAYLOAD);
  const decoder = new NdjsonDecoder();
  const frames: unknown[] = [];
  for (const byte of wire) frames.push(...decoder.push(Buffer.from([byte])));
  assert.deepEqual(frames, [{ v: 1, ev: "text_delta", session_id: "s1", text: SPLIT_PAYLOAD }]);
});

test("the frame cap is the bridge's 64 MiB and the residual cap does not exceed it", () => {
  assert.equal(MAX_FRAME_BYTES, 64 * 1024 * 1024);
  assert.ok(MAX_RESIDUAL_BYTES <= MAX_FRAME_BYTES);
});

test("an oversized frame is rejected and the next valid frame still decodes", () => {
  const decoder = new NdjsonDecoder({ maxFrameBytes: 64 });
  const oversized = Buffer.from(`${JSON.stringify({ ev: "text_delta", text: "x".repeat(200) })}\n`);
  const error = rejection(() => decoder.push(oversized), "frame");
  assert.equal(error.limitBytes, 64);
  assert.ok(error.observedBytes > 64);
  assert.equal(decoder.bufferedBytes, 0);
  assert.deepEqual(decoder.push(Buffer.from('{"ev":"pong"}\n')), [{ ev: "pong" }]);
});

test("the residual buffer never exceeds its cap and refuses an unterminated frame", () => {
  const cap = 1024;
  const decoder = new NdjsonDecoder({ maxFrameBytes: cap, maxResidualBytes: cap });
  const chunk = Buffer.from("y".repeat(100));
  const error = rejection(() => {
    for (let pushed = 0; pushed < 100; pushed += 1) decoder.push(chunk);
  }, "remainder");
  assert.ok(error.observedBytes <= cap + chunk.length, "refused before concatenating past one chunk");
  assert.ok(decoder.bufferedBytes <= cap, `held ${decoder.bufferedBytes} bytes, cap ${cap}`);
  assert.deepEqual(decoder.push(Buffer.from('{"ev":"ok"}\n')), [{ ev: "ok" }]);
});

test("a rejected frame does not leak its half character into the next stream", () => {
  const decoder = new NdjsonDecoder({ maxFrameBytes: 64, maxResidualBytes: 64 });
  const korean = Buffer.from('{"text":"가', "utf8");
  decoder.push(korean.subarray(0, korean.length - 2));
  rejection(() => decoder.push(Buffer.from("z".repeat(128))), "remainder");
  const next = Buffer.from('{"text":"나"}\n', "utf8");
  const frames = [...decoder.push(next.subarray(0, 3)), ...decoder.push(next.subarray(3))];
  assert.deepEqual(frames, [{ text: "나" }]);
});

test("a large frame releases its storage and keeps the trailing remainder intact", () => {
  const decoder = new NdjsonDecoder();
  const big = { ev: "text_delta", text: "가".repeat(200_000) };
  const wire = Buffer.concat([
    Buffer.from(`${JSON.stringify(big)}\n`, "utf8"),
    Buffer.from('{"ev":"po', "utf8"),
  ]);
  assert.deepEqual(decoder.push(wire), [big]);
  assert.equal(decoder.bufferedBytes, Buffer.byteLength('{"ev":"po'));
  assert.deepEqual(decoder.push(Buffer.from('ng"}\n', "utf8")), [{ ev: "pong" }]);
});

test("run separates narration and final answer without splitting interleaved reasoning", async () => {
  const turn = await runFrames([
    { ev: "text_delta", message_id: "m1", text: "Checking the logs." },
    { ev: "text_done", message_id: "m1" },
    { ev: "tool_start", call_id: "c1", name: "read" },
    { ev: "tool_done", call_id: "c1", name: "read", output: "logs" },
    { ev: "text_delta", message_id: "m2", text: "The root cause is " },
    { ev: "reasoning_delta", text: "verify this" },
    { ev: "reasoning_done" },
    { ev: "text_delta", message_id: "m2", text: "the retry loop." },
    { ev: "text_done", message_id: "m2" },
  ]);
  assert.equal(turn.text, "Checking the logs.The root cause is the retry loop.");
  assert.equal(turn.finalText, "The root cause is the retry loop.");
  assert.equal(turn.reasoning, "verify this");
  assert.deepEqual(turn.messages, [
    { messageId: "m1", text: "Checking the logs." },
    { messageId: "m2", text: "The root cause is the retry loop." },
  ]);
});

test("message ids correlate interleaved chunks and out-of-order completion", async () => {
  const turn = await runFrames([
    { ev: "text_delta", message_id: "a", text: "First " },
    { ev: "text_delta", message_id: "b", text: "Second." },
    { ev: "text_delta", message_id: "a", text: "message." },
    { ev: "text_done", message_id: "b" },
    { ev: "text_done", message_id: "a" },
    { ev: "text_done", message_id: "a" },
  ]);
  assert.deepEqual(turn.messages, [
    { messageId: "a", text: "First message." },
    { messageId: "b", text: "Second." },
  ]);
  assert.equal(turn.finalText, "Second.");
});

test("text_done supports sequential messages without optional ids", async () => {
  const turn = await runFrames([
    { ev: "text_delta", text: "First." },
    { ev: "text_done" },
    { ev: "text_delta", text: "Last." },
    { ev: "text_done" },
  ]);
  assert.deepEqual(turn.messages, [{ text: "First." }, { text: "Last." }]);
  assert.equal(turn.finalText, "Last.");
});

test("older bridges retain whole-turn text without inventing boundaries", async () => {
  const turn = await runFrames([
    { ev: "text_delta", text: "The root cause is " },
    { ev: "reasoning_delta", text: "thinking" },
    { ev: "text_delta", text: "the retry loop." },
  ]);
  assert.equal(turn.finalText, "The root cause is the retry loop.");
  assert.equal(turn.text, turn.finalText);
  assert.deepEqual(turn.messages, []);
});

test("text corrections remove discarded output, including already completed messages", async () => {
  const turn = await runFrames([
    { ev: "text_delta", message_id: "old", text: "Discarded." },
    { ev: "text_done", message_id: "old" },
    { ev: "text_replace", message_id: "old", text: "" },
    { ev: "text_delta", message_id: "new", text: "Answer with leaked tool wrapper" },
    { ev: "text_replace", message_id: "new", text: "Answer." },
    { ev: "text_done", message_id: "new" },
  ]);
  assert.equal(turn.text, "Answer.");
  assert.equal(turn.finalText, "Answer.");
  assert.deepEqual(turn.messages, [{ messageId: "new", text: "Answer." }]);
});

test("reasoning-only turns have no phantom text messages", async () => {
  const turn = await runFrames([
    { ev: "reasoning_delta", text: "thinking" },
    { ev: "reasoning_done" },
    { ev: "text_done" },
  ]);
  assert.equal(turn.text, "");
  assert.equal(turn.finalText, "");
  assert.deepEqual(turn.messages, []);
});

test("runStructured validates the final framed message rather than narration", async () => {
  const server = await startMockHarness({
    onRequest(request, send) {
      if (request.req !== "send_message") return;
      send({ v: 1, ev: "message_accepted", session_id: "s1" });
      send({ v: 1, ev: "text_delta", session_id: "s1", message_id: "narration", text: "Let me check." });
      send({ v: 1, ev: "text_done", session_id: "s1", message_id: "narration" });
      send({ v: 1, ev: "text_delta", session_id: "s1", message_id: "answer", text: '{"ok":true}' });
      send({ v: 1, ev: "text_done", session_id: "s1", message_id: "answer" });
      send({ v: 1, ev: "turn_done", session_id: "s1" });
    },
  });
  const client = await HoronAgentClient.connect({ socketPath: server.socketPath });
  try {
    const result = await client.runStructured("s1", "Check", {
      schema: { type: "object", properties: { ok: { type: "boolean" } }, required: ["ok"] },
      maxRetries: 0,
    });
    assert.deepEqual(result.data, { ok: true });
    assert.equal(result.text, 'Let me check.{"ok":true}');
    assert.equal(result.attempts[0].text, '{"ok":true}');
  } finally {
    await client.close();
    await server.close();
  }
});
