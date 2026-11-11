import { afterEach, test } from "bun:test";
import assert from "node:assert/strict";
import { HoronAgentClient } from "../dist/index.js";
import { inOrder, startMockHarness } from "./mock-harness.ts";

const teardown: Array<() => void | Promise<void>> = [];
afterEach(async () => {
  await inOrder(teardown.splice(0), (close) => Promise.resolve(close()));
});

const MODES = { follow_up_mode: "all", steering_mode: "one-at-a-time", interrupt_mode: "wait" };

async function fixture() {
  const requests: any[] = [];
  const server = await startMockHarness({
    capabilities: [],
    onRequest(req, send) {
      requests.push(req);
      if (req.req === "create_session") send({ v: 1, reply_to: req.id, ev: "attached", session: { session_id: "s1" } });
      else if (req.req === "get_message_modes")
        send({ v: 1, reply_to: req.id, ev: "message_modes", session_id: req.session_id, modes: MODES });
      else send({ v: 1, reply_to: req.id, ev: "ok" });
    },
  });
  const client = await HoronAgentClient.connect({ socketPath: server.socketPath });
  teardown.push(async () => {
    await client.close();
    await server.close();
  });
  return { client, requests };
}

test("getMessageModes returns the daemon's delivery modes", async () => {
  const { client, requests } = await fixture();

  const modes = await client.getMessageModes("s1");

  assert.deepEqual(modes, MODES);
  const request = requests.find((entry) => entry.req === "get_message_modes");
  assert.equal(request?.session_id, "s1", "the request names the session it was asked about");
});
