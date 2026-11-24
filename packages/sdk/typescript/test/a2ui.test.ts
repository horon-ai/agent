import { test } from "bun:test";
import assert from "node:assert/strict";
import {
  A2UI_BASIC_CATALOG_V1_0,
  A2UI_VERSION_V1_0,
  type A2uiSessionCapabilities,
  HarnessError,
  HoronAgentClient,
} from "../dist/index.js";
import { startMockHarness } from "./mock-harness.ts";

const DEFAULT_CAPS: A2uiSessionCapabilities = {
  version: A2UI_VERSION_V1_0,
  supported_catalog_ids: [A2UI_BASIC_CATALOG_V1_0],
  accepts_inline_catalogs: false,
};

test("a2ui capability stays omitted by default on connect, createSession, and attachSession", async () => {
  const received: any[] = [];
  const server = await startMockHarness({
    capabilities: ["a2ui_v1"],
    onRequest: (req, send) => {
      received.push(req);
      if (req.req === "create_session") {
        send({ v: 1, reply_to: req.id, ev: "attached", session: { session_id: "c1" } });
        return;
      }
      if (req.req === "attach_session") {
        send({ v: 1, reply_to: req.id, ev: "attached", session: { session_id: String(req.session_id) } });
        return;
      }
      send({ v: 1, reply_to: req.id, ev: "ok" });
    },
  });
  let client: HoronAgentClient | undefined;
  try {
    client = await HoronAgentClient.connect({ socketPath: server.socketPath });
    assert.equal(client.supports("a2ui_v1"), true);
    await client.createSession();
    await client.attachSession("existing");
    assert.equal("a2ui" in received[0], false);
    assert.equal("a2ui" in received[1], false);
  } finally {
    await client?.close();
    await server.close();
  }
});

test("a2ui connect, createSession, attachSession, configureA2ui, sendA2uiEvent, and TurnResult collection", async () => {
  const envelope = {
    version: "v0.9",
    createSurface: { surfaceId: "main", catalogId: A2UI_BASIC_CATALOG_V1_0 },
  };
  const received: any[] = [];
  let server!: Awaited<ReturnType<typeof startMockHarness>>;
  server = await startMockHarness({
    capabilities: ["a2ui_v1"],
    onRequest: (req, send) => {
      received.push(req);
      if (req.req === "create_session") {
        send({ v: 1, reply_to: req.id, ev: "attached", session: { session_id: "s1" } });
        return;
      }
      if (req.req === "attach_session") {
        send({ v: 1, reply_to: req.id, ev: "attached", session: { session_id: String(req.session_id) } });
        return;
      }
      if (req.req === "send_message") {
        send({ v: 1, reply_to: req.id, ev: "ok" });
        send({ v: 1, ev: "message_accepted", session_id: "s1" });
        setTimeout(() => {
          server.broadcast({
            v: 1,
            ev: "a2ui_envelope",
            session_id: "s1",
            tool_call_id: "call-1",
            sequence: 0,
            envelope,
            fallback_text: "Card text",
          });
          server.broadcast({ v: 1, ev: "text_delta", session_id: "s1", text: "Done" });
          server.broadcast({ v: 1, ev: "turn_done", session_id: "s1" });
        }, 5);
        return;
      }
      send({ v: 1, reply_to: req.id, ev: "ok" });
    },
  });
  let client: HoronAgentClient | undefined;
  try {
    client = await HoronAgentClient.connect({ socketPath: server.socketPath, a2ui: DEFAULT_CAPS });
    await client.createSession({ a2ui: DEFAULT_CAPS });
    await client.attachSession("s1", { a2ui: DEFAULT_CAPS });
    await client.configureA2ui("s1", DEFAULT_CAPS);
    await client.configureA2ui("s1", null);
    await client.sendA2uiEvent("s1", { version: "v0.9", action: { name: "ok", surfaceId: "main" } }, { n: 1 });
    const seenEvents: string[] = [];
    const turn = await client.run("s1", "show", {
      onEvent: (ev) => seenEvents.push(ev.ev),
    });

    assert.deepEqual(received[0].a2ui, DEFAULT_CAPS);
    assert.deepEqual(received[1].a2ui, DEFAULT_CAPS);
    assert.deepEqual(received[2], {
      v: 1,
      id: received[2].id,
      req: "configure_a2ui",
      session_id: "s1",
      capabilities: DEFAULT_CAPS,
    });
    assert.deepEqual(received[3], {
      v: 1,
      id: received[3].id,
      req: "configure_a2ui",
      session_id: "s1",
      capabilities: null,
    });
    assert.equal(received[4].req, "a2ui_event");
    assert.deepEqual(received[4].data_model, { n: 1 });
    assert.ok(seenEvents.includes("a2ui_envelope"));
    assert.deepEqual(turn.a2uiEnvelopes, [{ toolCallId: "call-1", sequence: 0, envelope, fallbackText: "Card text" }]);
  } finally {
    await client?.close();
    await server.close();
  }
});

test("a2ui methods fail unsupported and client drops unnegotiated a2ui_envelope", async () => {
  let server!: Awaited<ReturnType<typeof startMockHarness>>;
  server = await startMockHarness({
    capabilities: [],
    onRequest: (req, send) => {
      if (req.req === "send_message") {
        send({ v: 1, reply_to: req.id, ev: "ok" });
        send({ v: 1, ev: "message_accepted", session_id: "s1" });
        setTimeout(() => {
          server.broadcast({
            v: 1,
            ev: "a2ui_envelope",
            session_id: "s1",
            tool_call_id: "c1",
            sequence: 0,
            envelope: { version: "v0.9" },
          });
          server.broadcast({ v: 1, ev: "turn_done", session_id: "s1" });
        }, 5);
        return;
      }
      send({ v: 1, reply_to: req.id, ev: "ok" });
    },
  });
  let client: HoronAgentClient | undefined;
  try {
    client = await HoronAgentClient.connect({ socketPath: server.socketPath });
    await assert.rejects(
      () => client!.configureA2ui("s1", DEFAULT_CAPS),
      (e: unknown) => e instanceof HarnessError && e.code === "unsupported",
    );
    await assert.rejects(
      () => client!.sendA2uiEvent("s1", { version: "v0.9" }),
      (e: unknown) => e instanceof HarnessError && e.code === "unsupported",
    );
    const turn = await client.run("s1", "hi");
    assert.deepEqual(turn.a2uiEnvelopes, []);
  } finally {
    await client?.close();
    await server.close();
  }
});
