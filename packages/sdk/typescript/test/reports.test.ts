/**
 * The five requests the SDK used to reach only through the generic `request()`
 * call, and the seven reply fields its types used to drop.
 *
 * The mock harness proves which frame the SDK sends, field by field, and which
 * values it keeps out of the reply. The live e2e script proves the daemon
 * answers those same shapes, so a field renamed on one side cannot pass both.
 */

import { afterEach, test } from "bun:test";
import assert from "node:assert/strict";
import { HoronAgentClient } from "../dist/index.js";
import { inOrder, startMockHarness } from "./mock-harness.ts";

const teardown: Array<() => void | Promise<void>> = [];
afterEach(async () => {
  await inOrder(teardown.splice(0), (close) => Promise.resolve(close()));
});

/** One stored session row, so the four listing fields have values to carry. */
const SESSION = {
  session_id: "s1",
  status: "idle",
  title: "quokka notes",
  saved: true,
  save_label: "research",
  updated_at_ms: 1_700_000_000_000,
  last_active_at_ms: 1_699_999_000_000,
};

/** A row from a daemon that omitted every optional field, as the wire allows. */
const BARE_SESSION = { session_id: "s2", status: "idle" };

const ROUTE = { model: "claude-opus-5", provider: "anthropic", api_method: "messages" };

const USAGE = {
  sessions: [
    {
      session_id: "s1",
      model: "claude-opus-5",
      input: 11,
      output: 7,
      cache_read_input: 3,
      cache_creation_input: 2,
      requests: 1,
    },
  ],
  totals: { input: 11, output: 7, cache_read_input: 3, cache_creation_input: 2, requests: 1, sessions: 1 },
  models: [
    {
      model: "claude-opus-5",
      input: 11,
      output: 7,
      cache_read_input: 3,
      cache_creation_input: 2,
      requests: 1,
      sessions: 1,
    },
  ],
  pricing: "unknown",
  estimated_cost: null,
};

const ARTIFACT = { path: "out/chart.html", kind: "html", bytes: 1280, mtime_ms: 1_700_000_500_000 };

/** One provider call as a daemon that accounts for cache creation reports it. */
const TOKEN_USAGE = { input: 11, output: 7, cache_read_input: 3, cache_creation_input: 2 };

/** The same call from a daemon that predates cache-creation accounting. */
const TOKEN_USAGE_WITHOUT_CREATION = { input: 11, output: 7, cache_read_input: 3 };

async function fixture(tokenUsage: Record<string, unknown> = TOKEN_USAGE) {
  const requests: Array<Record<string, unknown>> = [];
  const server = await startMockHarness({
    capabilities: ["sessions", "streaming"],
    onRequest(request, send) {
      requests.push(request);
      switch (request.req) {
        case "list_sessions":
          send({ v: 1, reply_to: request.id, ev: "sessions", sessions: [SESSION, BARE_SESSION] });
          return;
        case "get_runtime_info":
          send({
            v: 1,
            reply_to: request.id,
            ev: "runtime_info",
            session_id: "s1",
            provider: "anthropic",
            model: "claude-opus-5",
            reasoning_effort: "high",
            auth_method: "oauth",
            routes: [ROUTE],
          });
          return;
        case "search_sessions":
          send({
            v: 1,
            reply_to: request.id,
            ev: "session_search_results",
            query: request.query,
            results: [{ session_id: "s1", role: "user", preview: "...quokka...", at: "2026-01-02T03:04:05Z" }],
          });
          return;
        case "list_artifacts":
          send({
            v: 1,
            reply_to: request.id,
            ev: "artifacts",
            session_id: request.session_id,
            artifacts: [ARTIFACT],
          });
          return;
        case "usage_report":
          send({
            v: 1,
            reply_to: request.id,
            ev: "usage_report",
            ...(request.session_id === undefined ? {} : { session_id: request.session_id }),
            ...USAGE,
          });
          return;
        case "approval_ack":
        case "failover_ack":
          // An ack is a message a client sends, so the bridge confirms it with
          // a bare `ok` and the daemon emits no ack event afterwards. A mock
          // that answered with the event would pass here and fail against
          // the daemon.
          send({ v: 1, reply_to: request.id, ev: "ok" });
          return;
        case "send_message":
          send({ v: 1, reply_to: request.id, ev: "ok" });
          send({ v: 1, ev: "message_accepted", session_id: "s1" });
          send({ v: 1, ev: "token_usage", session_id: "s1", ...tokenUsage });
          send({ v: 1, ev: "turn_done", session_id: "s1" });
          return;
        case "ping":
          send({ v: 1, reply_to: request.id, ev: "pong" });
          return;
        default:
          send({ v: 1, reply_to: request.id, ev: "ok" });
      }
    },
  });
  const client = await HoronAgentClient.connect({ socketPath: server.socketPath });
  teardown.push(async () => {
    await client.close();
    await server.close();
  });
  return { client, requests };
}

/** The frame the SDK put on the wire, without the envelope the transport adds. */
function fields(request: Record<string, unknown>) {
  const { v: _version, id: _id, ...rest } = request;
  return rest;
}

function frameFor(requests: Array<Record<string, unknown>>, req: string) {
  const found = requests.findLast((entry) => entry.req === req);
  assert.ok(found, `no ${req} frame was sent`);
  return found;
}

test("searchSessions sends search_sessions and returns the hits", async () => {
  const { client, requests } = await fixture();

  const hits = await client.searchSessions("quokka");

  const frame = fields(frameFor(requests, "search_sessions"));
  assert.deepEqual(frame, { req: "search_sessions", query: "quokka" });
  assert.equal(Object.hasOwn(frame, "limit"), false, "an omitted limit must not reach the wire");
  assert.equal(hits.length, 1);
  assert.equal(hits[0]?.session_id, "s1");
  assert.equal(hits[0]?.role, "user");
});

test("searchSessions carries the limit when one is given", async () => {
  const { client, requests } = await fixture();

  await client.searchSessions("quokka", 5);

  assert.deepEqual(fields(frameFor(requests, "search_sessions")), {
    req: "search_sessions",
    query: "quokka",
    limit: 5,
  });
});

test("listArtifacts names the session and returns its artifacts", async () => {
  const { client, requests } = await fixture();

  const artifacts = await client.listArtifacts("s1");

  assert.deepEqual(fields(frameFor(requests, "list_artifacts")), { req: "list_artifacts", session_id: "s1" });
  assert.deepEqual(artifacts, [ARTIFACT]);
});

test("usageReport omits the session for an account-wide report", async () => {
  const { client, requests } = await fixture();

  const report = await client.usageReport();

  const frame = fields(frameFor(requests, "usage_report"));
  assert.deepEqual(frame, { req: "usage_report" });
  assert.equal(Object.hasOwn(frame, "session_id"), false, "an account-wide report names no session");
  assert.deepEqual(report.totals, USAGE.totals);
  assert.deepEqual(report.models, USAGE.models);
  assert.equal(report.pricing, "unknown");
  assert.equal(report.estimated_cost, null);
});

test("usageReport names the session when one is given", async () => {
  const { client, requests } = await fixture();

  const report = await client.usageReport("s1");

  assert.deepEqual(fields(frameFor(requests, "usage_report")), { req: "usage_report", session_id: "s1" });
  assert.equal(report.session_id, "s1");
  assert.deepEqual(report.sessions, USAGE.sessions);
});

test("ackApproval sends the request id it retires and returns the receipt", async () => {
  const { client, requests } = await fixture();

  const receipt = await client.ackApproval("s1", "req-1");

  assert.deepEqual(fields(frameFor(requests, "approval_ack")), {
    req: "approval_ack",
    session_id: "s1",
    request_id: "req-1",
  });
  assert.deepEqual(receipt, { requestId: "req-1" });
});

test("ackFailover carries the whole announcement key", async () => {
  const { client, requests } = await fixture();

  const receipt = await client.ackFailover("s1", "chain-1", "model-a", "model-b", 2);

  assert.deepEqual(fields(frameFor(requests, "failover_ack")), {
    req: "failover_ack",
    session_id: "s1",
    chain_id: "chain-1",
    from: "model-a",
    to: "model-b",
    attempt: 2,
  });
  assert.deepEqual(receipt, { chainId: "chain-1", from: "model-a", to: "model-b", attempt: 2 });
});

test("listSessions keeps saved, its label, and both timestamps", async () => {
  const { client } = await fixture();

  const sessions = await client.listSessions();

  const saved = sessions[0];
  assert.equal(saved?.saved, true);
  assert.equal(saved?.save_label, "research");
  assert.equal(saved?.updated_at_ms, 1_700_000_000_000);
  assert.equal(saved?.last_active_at_ms, 1_699_999_000_000);
  const bare = sessions[1];
  assert.equal(bare?.saved, undefined, "an omitted flag means unsaved, and undefined says so");
  assert.equal(Object.hasOwn(bare ?? {}, "save_label"), false);
  assert.equal(Object.hasOwn(bare ?? {}, "updated_at_ms"), false);
  assert.equal(Object.hasOwn(bare ?? {}, "last_active_at_ms"), false);
});

test("getRuntimeInfo keeps the reasoning effort and the auth method", async () => {
  const { client } = await fixture();

  const info = await client.getRuntimeInfo("s1");

  assert.equal(info.model, "claude-opus-5");
  assert.equal(info.reasoningEffort, "high");
  assert.equal(info.authMethod, "oauth");
});

test("a turn keeps the cache-creation tokens", async () => {
  const { client } = await fixture();

  const turn = await client.run("s1", "hi");

  assert.deepEqual(turn.usage, { input: 11, output: 7, cacheReadInput: 3, cacheCreationInput: 2 });
});

test("a turn without cache-creation accounting keeps its old shape", async () => {
  const { client } = await fixture(TOKEN_USAGE_WITHOUT_CREATION);

  const turn = await client.run("s1", "hi");

  assert.equal(turn.usage?.cacheCreationInput, undefined);
  assert.equal(
    Object.hasOwn(turn.usage ?? {}, "cacheCreationInput"),
    false,
    "the usage keeps the shape it had before the field",
  );
});
