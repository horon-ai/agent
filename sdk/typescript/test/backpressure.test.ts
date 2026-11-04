/**
 * Write-side backpressure and request admission.
 *
 * A client that ignores the transport's own backpressure signal turns a slow
 * peer into unbounded memory in the caller's process, and an unbounded stream
 * of requests grows the pending map the same way. Both are checked here against
 * a transport the test controls byte for byte.
 */

import { test } from "bun:test";
import assert from "node:assert/strict";
import { HoronAgentClient, HarnessError, type Transport } from "../dist/index.js";
import { waitUntil } from "./mock-harness.ts";

/** An in-memory transport whose write result and drain signal the test drives. */
class ScriptedTransport implements Transport {
  readonly written: string[] = [];
  private readonly dataListeners: ((chunk: Buffer | string) => void)[] = [];
  private readonly drainListeners: (() => void)[] = [];
  private blocked = false;

  onDrain?: (listener: () => void) => void;

  constructor(
    private readonly blockAfter: number | undefined,
    drainable = true,
  ) {
    // A transport that reports backpressure without a drain signal has to be
    // modelled without the method at all: an `onDrain` that silently drops its
    // listener is exactly the trap the client refuses to fall into.
    if (drainable) this.onDrain = (listener) => this.drainListeners.push(listener);
  }

  write(data: string): void | boolean {
    if (this.blocked) {
      // The client must not reach this: a blocked transport is not written to
      // again until it drains.
      throw new Error("written while blocked");
    }
    this.written.push(data);
    if (this.blockAfter !== undefined && this.written.length === this.blockAfter) {
      this.blocked = true;
      return false;
    }
    return true;
  }

  onData(listener: (chunk: Buffer | string) => void): void {
    this.dataListeners.push(listener);
  }

  onClose(): void {}

  close(): void {}

  deliver(frame: unknown): void {
    const text = `${JSON.stringify(frame)}\n`;
    for (const listener of this.dataListeners) listener(text);
  }

  drain(): void {
    this.blocked = false;
    for (const listener of this.drainListeners) listener();
  }

  /** Request ids the client has written so far, in wire order. */
  requestIds(): number[] {
    return this.written
      .map((line) => line.trim())
      .filter((line) => line.length > 0)
      .map((line) => (JSON.parse(line) as { id: number }).id);
  }
}

async function connectWith(transport: ScriptedTransport, maxPendingRequests?: number) {
  const connecting = HoronAgentClient.connect({
    transport,
    maxPendingRequests,
    requestTimeoutMs: 0,
  });
  // The handshake is itself a request, so its reply has to arrive while
  // `connect` is still awaiting it.
  await waitUntil(() => transport.written.length > 0);
  transport.deliver({ v: 1, reply_to: 1, ev: "hello_ok", version: 1, server: "scripted/1", capabilities: [] });
  return connecting;
}

test("a blocked transport holds frames and releases them on drain, in order", async () => {
  const transport = new ScriptedTransport(2);
  const client = await connectWith(transport);
  assert.deepEqual(transport.requestIds(), [1]);

  const first = client.request({ req: "ping" });
  assert.equal(transport.written.length, 2, "the first request is written");

  const second = client.request({ req: "ping" });
  const third = client.request({ req: "ping" });
  assert.deepEqual(
    transport.requestIds(),
    [1, 2],
    "a transport that reported backpressure must not be written to again",
  );

  transport.drain();
  assert.deepEqual(transport.requestIds(), [1, 2, 3, 4], "queued frames flush in order on drain");

  for (const id of [2, 3, 4]) transport.deliver({ v: 1, reply_to: id, ev: "pong" });
  assert.equal((await first).ev, "pong");
  assert.equal((await second).ev, "pong");
  assert.equal((await third).ev, "pong");
  void client.close();
});

test("a transport that returns nothing keeps working unchanged", async () => {
  const transport = new ScriptedTransport(undefined);
  const client = await connectWith(transport);
  const reply = client.request({ req: "ping" });
  assert.deepEqual(transport.requestIds(), [1, 2]);
  transport.deliver({ v: 1, reply_to: 2, ev: "pong" });
  assert.equal((await reply).ev, "pong");
  void client.close();
});

test("a transport that reports backpressure without a drain signal is refused", async () => {
  const transport = new ScriptedTransport(2, false);
  const client = await connectWith(transport);
  const first = client.request({ req: "ping" });
  const second = client.request({ req: "ping" });
  await assert.rejects(second, (error: HarnessError) => error.code === "transport_backpressure");
  transport.deliver({ v: 1, reply_to: 2, ev: "pong" });
  assert.equal((await first).ev, "pong");
  void client.close();
});

test("the outstanding-request limit is enforced and surfaced to the caller", async () => {
  const transport = new ScriptedTransport(undefined);
  const client = await connectWith(transport, 2);

  const inFlight = [client.request({ req: "ping" }), client.request({ req: "ping" })];
  await assert.rejects(client.request({ req: "ping" }), (error: HarnessError) => {
    assert.equal(error.code, "too_many_requests");
    assert.match(error.message, /maxPendingRequests 2/);
    return true;
  });
  assert.equal(transport.requestIds().length, 3, "the refused request never reaches the wire");

  transport.deliver({ v: 1, reply_to: 2, ev: "pong" });
  await inFlight[0];
  // Admission is released by the reply, so the next request is accepted.
  const accepted = client.request({ req: "ping" });
  transport.deliver({ v: 1, reply_to: 4, ev: "pong" });
  assert.equal((await accepted).ev, "pong");
  void client.close();
});
