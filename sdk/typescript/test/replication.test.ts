/**
 * Session replication and cursor-history surface.
 *
 * A mock harness proves the SDK builds the right frames and reads the right
 * replies; the two-HOME daemon surface is exercised separately in Rust. The
 * canonical-bytes case is the one that matters most: `canonical_json` must
 * survive as text, because a u64 above 2^53, `1.0`, and `-0.0` are destroyed
 * by any parse-and-restringify round trip.
 */

import { test } from "node:test";
import assert from "node:assert/strict";
import { HoronAgentClient, HarnessError, type SessionEventPage } from "../dist/index.js";
import { startMockHarness } from "./mock-harness.ts";

const CANONICAL =
  '{"schema_version":1,"payload":{"big":18446744073709551615,"one":1.0,"negative_zero":-0.0}}';

const REPLICATION_CAPABILITIES = [
  "sessions",
  "streaming",
  "session_event_store_v1",
  "session_replication_v1",
  "session_history_cursor_v1",
  "structured_compaction_v1",
];

test("the bridge's four session-store capabilities are all readable by the client", async () => {
  // Plan D12 names four. A client that checks one the bridge never advertises
  // is refused by its own SDK, so the negotiated set is the contract.
  const server = await startMockHarness({ capabilities: REPLICATION_CAPABILITIES });
  const client = await HoronAgentClient.connect({ socketPath: server.socketPath });
  for (const capability of [
    "session_event_store_v1",
    "session_replication_v1",
    "session_history_cursor_v1",
    "structured_compaction_v1",
  ]) {
    assert.equal(client.supports(capability), true, `missing ${capability}`);
  }
  client.close();
  await server.close();
});

test("replication capabilities gate the new methods", async () => {
  const server = await startMockHarness({ capabilities: ["sessions", "streaming"] });
  const client = await HoronAgentClient.connect({ socketPath: server.socketPath });
  for (const call of [
    () => client.listSessionHeads("s1"),
    () => client.selectSessionHead("s1", "h1"),
    () => client.getHistoryPage("s1"),
    () => client.exportSessionEvents("s1"),
    () => client.importSessionEvents({ session_id: "s1", heads: [], events: [] }),
    () => client.exportSessionBlob("s1", "a".repeat(64), 1, 0),
    () => client.sessionBlobStatus("s1", "a".repeat(64), 1),
  ]) {
    await assert.rejects(call, (error: unknown) => {
      assert.ok(error instanceof HarnessError);
      assert.equal((error as HarnessError).code, "unsupported");
      assert.match((error as HarnessError).message, /capability/);
      return true;
    });
  }
  client.close();
  await server.close();
});

test("heads, selection, and history pages round-trip", async () => {
  const server = await startMockHarness({
    capabilities: REPLICATION_CAPABILITIES,
    onRequest(request, send) {
      if (request.req === "list_session_heads" || request.req === "select_session_head") {
        send({
          v: 1,
          reply_to: request.id,
          ev: "session_heads",
          session_id: request.session_id,
          heads: ["h1", "h2"],
          selected_head: request.head_event_id ?? "h1",
        });
        return;
      }
      if (request.req === "get_history_page") {
        send({
          v: 1,
          reply_to: request.id,
          ev: "session_history_page",
          session_id: request.session_id,
          head: request.head_event_id ?? "h1",
          messages: [{ role: "user", content: `limit=${request.limit} before=${request.before_event_id}` }],
          next_cursor: "e0",
        });
        return;
      }
      send({ v: 1, reply_to: request.id, ev: "ok" });
    },
  });
  const client = await HoronAgentClient.connect({ socketPath: server.socketPath });

  const heads = await client.listSessionHeads("s1");
  assert.deepEqual(heads, { session_id: "s1", heads: ["h1", "h2"], selected_head: "h1" });

  const selected = await client.selectSessionHead("s1", "h2");
  assert.equal(selected.selected_head, "h2");

  const page = await client.getHistoryPage("s1", { headEventId: "h1", beforeEventId: "e5", limit: 2 });
  assert.equal(page.head, "h1");
  assert.equal(page.messages[0]?.content, "limit=2 before=e5");
  assert.equal(page.next_cursor, "e0");

  client.close();
  await server.close();
});

test("exported canonical bytes are never parsed or re-serialized", async () => {
  const server = await startMockHarness({
    capabilities: REPLICATION_CAPABILITIES,
    onRequest(request, send) {
      if (request.req === "export_session_events") {
        send({
          v: 1,
          reply_to: request.id,
          ev: "session_event_page",
          session_id: request.session_id,
          heads: ["h1"],
          events: [
            {
              event_id: "11111111-1111-4111-8111-111111111111",
              content_hash: "c".repeat(64),
              canonical_json: CANONICAL,
            },
          ],
          record_refs: [
            {
              position: 0,
              event_id: "22222222-2222-4222-8222-222222222222",
              content_hash: "d".repeat(64),
              sha256: "e".repeat(64),
              size: 4096,
            },
          ],
        });
        return;
      }
      if (request.req === "import_session_events") {
        assert.equal(request.events[0].canonical_json, CANONICAL);
        assert.equal(request.protocol_version, 1);
        send({
          v: 1,
          reply_to: request.id,
          ev: "session_import_result",
          session_id: request.session_id,
          accepted_ids: [request.events[0].event_id],
          heads: ["h1"],
        });
        return;
      }
      send({ v: 1, reply_to: request.id, ev: "ok" });
    },
  });
  const client = await HoronAgentClient.connect({ socketPath: server.socketPath });

  const page = await client.exportSessionEvents("s1");
  assert.equal(page.events[0]?.canonical_json, CANONICAL);
  assert.match(page.events[0]!.canonical_json, /18446744073709551615/);
  assert.match(page.events[0]!.canonical_json, /-0\.0/);
  assert.equal(page.record_refs?.[0]?.size, 4096);

  const imported = await client.importSessionEvents(page as SessionEventPage);
  assert.deepEqual(imported.accepted_ids, ["11111111-1111-4111-8111-111111111111"]);
  assert.deepEqual(imported.heads, ["h1"]);

  client.close();
  await server.close();
});

test("typed store details reach the caller and stay optional", async () => {
  const server = await startMockHarness({
    capabilities: REPLICATION_CAPABILITIES,
    onRequest(request, send) {
      if (request.req === "import_session_events") {
        send({
          v: 1,
          reply_to: request.id,
          ev: "error",
          code: "invalid_request",
          message: "same id, different hash",
          details: {
            store_code: "hash_conflict",
            retryable: false,
            accepted_ids: ["a1"],
            conflict_heads: ["h1", "h2"],
          },
        });
        return;
      }
      send({
        v: 1,
        reply_to: request.id,
        ev: "error",
        code: "internal",
        message: "session writer is already owned",
        details: { store_code: "busy", retryable: true },
      });
    },
  });
  const client = await HoronAgentClient.connect({ socketPath: server.socketPath });
  try {
    await assert.rejects(
      () => client.importSessionEvents({ session_id: "s1", heads: [], events: [] }),
      (error: unknown) => {
        assert.ok(error instanceof HarnessError);
        assert.equal((error as HarnessError).code, "invalid_request");
        return true;
      },
    );
    const conflict = await client.request({ req: "import_session_events", session_id: "s1", events: [] });
    assert.equal(conflict.ev, "error");
    const conflictDetails = (conflict as { details?: { store_code?: string; retryable?: boolean } }).details;
    assert.equal(conflictDetails?.store_code, "hash_conflict");
    assert.equal(conflictDetails?.retryable, false);

    const busy = await client.request({
      req: "get_session_blob_status",
      session_id: "s1",
      sha256: "a".repeat(64),
      total_size: 1,
    });
    const busyDetails = (busy as { details?: { store_code?: string; retryable?: boolean } }).details;
    assert.equal(busyDetails?.store_code, "busy");
    assert.equal(busyDetails?.retryable, true);
  } finally {
    client.close();
    await server.close();
  }
});

test("a replication request above the wire envelope is refused locally", async () => {
  const server = await startMockHarness({
    capabilities: REPLICATION_CAPABILITIES,
    onRequest(_request, send) {
      send({ v: 1, ev: "ok" });
    },
  });
  const client = await HoronAgentClient.connect({ socketPath: server.socketPath });
  const oversized = {
    session_id: "s1",
    heads: [],
    events: [
      {
        event_id: "11111111-1111-4111-8111-111111111111",
        content_hash: "c".repeat(64),
        canonical_json: "x".repeat(17 * 1024 * 1024),
      },
    ],
  };
  await assert.rejects(
    () => client.importSessionEvents(oversized),
    (error: unknown) => {
      assert.ok(error instanceof HarnessError);
      assert.equal((error as HarnessError).code, "invalid_request");
      assert.match((error as HarnessError).message, /16/);
      return true;
    },
  );
  client.close();
  await server.close();
});
