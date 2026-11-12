/**
 * Durable-runtime surface of the TypeScript SDK (protocol minor 14).
 *
 * The properties a mock harness can prove: each new method sends the request
 * the protocol defines and reads the typed reply; a server that never
 * advertised the capability is refused before anything reaches the wire; and
 * a gated event never reaches a consumer that did not negotiate it.
 */

import { test } from "bun:test";
import assert from "node:assert/strict";
import { HoronAgentClient, HarnessError, type RuntimeStateSummary } from "../dist/index.js";
import { startMockHarness, waitUntil } from "./mock-harness.ts";

const MODERN = ["sessions", "durable_runtime_v1", "monitor_v1", "snapshot_marker_v1", "ask_receipts_v1"];
const LEGACY = ["sessions", "streaming"];

const STATE: RuntimeStateSummary = {
  journal: { exists: true, generation: 2, last_seq: 9, bytes: 512, records: 9 },
  runs: [{ run_id: "run-1", started_at: "2026-10-26T00:00:00Z", input_id: "i1" }],
  queue: { live_inputs: 1, undetermined_inputs: 0, live_input_bytes: 12 },
  pending_wakes: 0,
  pending_controls: 1,
};

test("the new methods send the protocol requests and read typed replies", async () => {
  const seen: any[] = [];
  const server = await startMockHarness({
    capabilities: MODERN,
    onRequest: (request, send) => {
      seen.push(request);
      if (request.req === "get_runtime_state") {
        send({ v: 1, reply_to: request.id, ev: "runtime_state", session_id: "s1", state: STATE });
        return;
      }
      if (request.req === "apply_control" || request.req === "get_control_receipt") {
        send({
          v: 1,
          reply_to: request.id,
          ev: "control_receipt",
          session_id: "s1",
          receipt: {
            operation_id: request.operation_id,
            action: request.action ?? "cancel_input",
            outcome: "terminal",
            result: { cancelled: "i1" },
            message: "terminal",
          },
        });
        return;
      }
      if (request.req === "get_snapshot") {
        send({
          v: 1,
          reply_to: request.id,
          ev: "snapshot",
          session_id: "s1",
          snapshot: {
            generation: "gen-1",
            seq: 4,
            marker_id: "marker-1",
            state: STATE,
            messages: [{ role: "user", content: "hi" }],
            truncated: false,
          },
        });
        return;
      }
      if (request.req === "ask_response") {
        assert.equal(request.answer_id, "answer-1", "the answer id must travel");
        send({ v: 1, reply_to: request.id, ev: "ok" });
      }
    },
  });

  const client = await HoronAgentClient.connect({ socketPath: server.socketPath });
  assert.deepEqual(await client.getRuntimeState("s1"), STATE);

  const applied = await client.applyControl("s1", "op-1", "cancel_input", { input_id: "i1" });
  assert.equal(applied.outcome, "terminal");
  assert.equal(applied.operation_id, "op-1");

  const receipt = await client.getControlReceipt("s1", "op-2");
  assert.equal(receipt.operation_id, "op-2");

  const snapshot = await client.getSnapshot("s1", { tail: 20 });
  assert.equal(snapshot.marker_id, "marker-1");
  assert.equal(snapshot.messages?.length, 1);

  await client.respondToAskWithId("s1", "ask-1", "answer-1", [{ question_id: "q1", selected: ["Fast"] }]);

  assert.equal(seen.length, 5, `each call sends one request: ${JSON.stringify(seen.map((r) => r.req))}`);
  const snapshotRequest = seen.find((request) => request.req === "get_snapshot");
  assert.equal(snapshotRequest.tail, 20);
  assert.equal(snapshotRequest.max_bytes, undefined);

  client.close();
  await server.close();
});

test("the legacy answer path still sends no answer id", async () => {
  const seen: any[] = [];
  const server = await startMockHarness({
    capabilities: MODERN,
    onRequest: (request, send) => {
      seen.push(request);
      send({ v: 1, reply_to: request.id, ev: "ok" });
    },
  });
  const client = await HoronAgentClient.connect({ socketPath: server.socketPath });
  await client.respondToAsk("s1", "ask-1", [{ question_id: "q1", selected: ["Fast"] }]);
  assert.equal(seen.length, 1);
  assert.equal(seen[0].req, "ask_response");
  assert.equal("answer_id" in seen[0], false, "an answer without an id must not carry the field");
  client.close();
  await server.close();
});

test("a server without the capability is refused before anything is sent", async () => {
  const seen: any[] = [];
  const server = await startMockHarness({
    capabilities: LEGACY,
    onRequest: (request, send) => {
      seen.push(request);
      send({ v: 1, reply_to: request.id, ev: "ok" });
    },
  });
  const client = await HoronAgentClient.connect({ socketPath: server.socketPath });
  for (const capability of ["durable_runtime_v1", "snapshot_marker_v1", "ask_receipts_v1"]) {
    assert.equal(client.supports(capability), false, `${capability} was not advertised`);
  }

  const calls = [
    () => client.getRuntimeState("s1"),
    () => client.applyControl("s1", "op-1", "cancel_input"),
    () => client.getControlReceipt("s1", "op-1"),
    () => client.getSnapshot("s1"),
    () => client.respondToAskWithId("s1", "ask-1", "answer-1", []),
  ];
  for (const call of calls) {
    await assert.rejects(call, (error: unknown) => {
      assert.ok(error instanceof HarnessError);
      assert.equal((error as HarnessError).code, "unsupported");
      return true;
    });
  }
  assert.deepEqual(seen, [], "a refused call must not reach the wire");
  client.close();
  await server.close();
});

test("a gated event never reaches a consumer that did not negotiate it", async () => {
  const server = await startMockHarness({
    capabilities: LEGACY,
    onRequest: (request, send) => {
      if (request.req !== "ping") return;
      send({
        v: 1,
        ev: "snapshot_marker",
        session_id: "s1",
        marker: { generation: "gen-1", seq: 1, marker_id: "marker-1" },
      });
      send({ v: 1, ev: "session_status", session_id: "s1", status: "idle" });
      send({ v: 1, reply_to: request.id, ev: "pong" });
    },
  });
  const client = await HoronAgentClient.connect({ socketPath: server.socketPath });
  const kinds: string[] = [];
  client.on("event", (event: any) => kinds.push(event.ev));
  await client.ping();
  await waitUntil(() => kinds.length > 0);
  assert.deepEqual(kinds, ["session_status"], `the marker must be dropped: ${JSON.stringify(kinds)}`);
  client.close();
  await server.close();
});

test("a gated event reaches a consumer that negotiated it", async () => {
  const server = await startMockHarness({
    capabilities: MODERN,
    onRequest: (request, send) => {
      if (request.req !== "ping") return;
      send({
        v: 1,
        ev: "snapshot_marker",
        session_id: "s1",
        marker: { generation: "gen-1", seq: 1, marker_id: "marker-1" },
      });
      send({ v: 1, reply_to: request.id, ev: "pong" });
    },
  });
  const client = await HoronAgentClient.connect({ socketPath: server.socketPath });
  const markers: any[] = [];
  client.on("snapshot_marker", (event: any) => markers.push(event));
  await client.ping();
  await waitUntil(() => markers.length > 0);
  assert.equal(markers.length, 1);
  assert.equal(markers[0].marker.marker_id, "marker-1");
  client.close();
  await server.close();
});


test("snapshotWithBufferedEvents keeps an event that raced the reply", async () => {
  const server = await startMockHarness({
    capabilities: MODERN,
    onRequest: (request, send) => {
      if (request.req !== "get_snapshot") return;
      // The live event arrives BEFORE the reply, which is exactly the race the
      // helper exists for: a consumer that subscribed after awaiting the
      // snapshot would never see it.
      send({
        v: 1,
        ev: "snapshot_marker",
        session_id: "s1",
        marker: { generation: "gen-1", seq: 4, marker_id: "marker-1" },
      });
      send({ v: 1, ev: "session_status", session_id: "s1", status: "idle" });
      send({
        v: 1,
        reply_to: request.id,
        ev: "snapshot",
        session_id: "s1",
        snapshot: {
          generation: "gen-1",
          seq: 4,
          marker_id: "marker-1",
          state: STATE,
          messages: [{ role: "user", content: "hi" }],
          truncated: false,
        },
      });
    },
  });

  const client = await HoronAgentClient.connect({ socketPath: server.socketPath });
  const result = await client.snapshotWithBufferedEvents("s1", { tail: 20 });
  assert.equal(result.snapshot.marker_id, "marker-1");
  const kinds = result.events.map((event: any) => event.ev);
  assert.ok(kinds.includes("snapshot_marker"), `the marker is buffered: ${JSON.stringify(kinds)}`);
  assert.ok(
    kinds.includes("session_status"),
    `the racing live event is not lost: ${JSON.stringify(kinds)}`,
  );
  assert.ok(
    kinds.indexOf("snapshot_marker") < kinds.indexOf("session_status"),
    `the marker keeps its place in the order: ${JSON.stringify(kinds)}`,
  );
  client.close();
  await server.close();
});

test("snapshotWithBufferedEvents refuses rather than buffering without a marker", async () => {
  const server = await startMockHarness({
    capabilities: MODERN,
    onRequest: (request, send) => {
      if (request.req !== "get_snapshot") return;
      // No marker, ever. The helper must not hold a partial view forever.
      for (let index = 0; index < 40; index += 1) {
        send({
          v: 1,
          ev: "session_status",
          session_id: "s1",
          status: "idle",
          padding: "x".repeat(4096),
        });
      }
      send({
        v: 1,
        reply_to: request.id,
        ev: "snapshot",
        session_id: "s1",
        snapshot: {
          generation: "gen-1",
          seq: 4,
          marker_id: "marker-1",
          state: STATE,
          messages: [],
          truncated: false,
        },
      });
    },
  });

  const client = await HoronAgentClient.connect({ socketPath: server.socketPath });
  await assert.rejects(
    () => client.snapshotWithBufferedEvents("s1", { tail: 20, maxBufferedBytes: 1024 }),
    /reconnect before trusting this view/,
  );
  client.close();
  await server.close();
});
