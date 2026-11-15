import { afterEach, test } from "bun:test";
import assert from "node:assert/strict";
import { HoronAgentClient } from "../dist/index.js";
import { inOrder, startMockHarness } from "./mock-harness.ts";

const teardown: Array<() => void | Promise<void>> = [];
afterEach(async () => {
  await inOrder(teardown.splice(0), (close) => Promise.resolve(close()));
});

type Request = Record<string, unknown> & { req: string; id: number };
type Send = (frame: unknown) => void;

async function fixture(capabilities: string[]) {
  const requests: Request[] = [];
  const server = await startMockHarness({
    capabilities,
    onRequest(req: Request, send: Send) {
      requests.push(req);
      switch (req.req) {
        case "create_session":
          send({ v: 1, reply_to: req.id, ev: "attached", session: { session_id: "s1" } });
          break;
        case "cancel_turn":
          send({
            v: 1,
            reply_to: req.id,
            ev: "cancel_turn_result",
            session_id: req.session_id,
            run_id: req.run_id,
            accepted: false,
            reason: "unknown",
          });
          break;
        case "cancel_tool_call":
        case "cancel_background_terminal":
          send({
            v: 1,
            reply_to: req.id,
            ev: "tool_call_cancel_result",
            session_id: req.session_id,
            call_id: req.call_id,
            accepted: true,
          });
          break;
        case "list_background_work":
          send({
            v: 1,
            reply_to: req.id,
            ev: "background_work_list",
            session_id: req.session_id,
            items: [
              { call_id: "call-4", task_id: "bg_9", kind: "terminal", command: "sleep 30", cwd: "/tmp", output: "aGk=" },
              { call_id: "call-5", task_id: "bg_10", kind: "tool_call" },
            ],
          });
          break;
        default:
          send({ v: 1, reply_to: req.id, ev: "ok" });
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

test("cancelTurn reports the refusal the daemon named", async () => {
  const { client, requests } = await fixture(["cancellable_work_v1"]);

  const result = await client.cancelTurn("s1", "run-7");

  assert.deepEqual(result, { accepted: false, reason: "unknown" });
  const request = requests.find((entry) => entry.req === "cancel_turn");
  assert.equal(request?.run_id, "run-7", "the request names the run, not just the session");
});

test("cancelToolCall and cancelBackgroundTerminal echo the call they stopped", async () => {
  const { client, requests } = await fixture(["cancellable_work_v1"]);

  const tool = await client.cancelToolCall("s1", "call-3");
  const background = await client.cancelBackgroundTerminal("s1", "call-4");

  assert.deepEqual(tool, { callId: "call-3", accepted: true });
  assert.deepEqual(background, { callId: "call-4", accepted: true });
  assert.deepEqual(
    requests.filter((entry) => entry.req.startsWith("cancel_")).map((entry) => entry.req),
    ["cancel_tool_call", "cancel_background_terminal"],
    "the two cancels travel as distinct requests",
  );
});

test("listBackgroundWork decodes the optional fields it was given", async () => {
  const { client } = await fixture(["cancellable_work_v1"]);

  const items = await client.listBackgroundWork("s1");

  assert.equal(items.length, 2);
  assert.equal(items[0].command, "sleep 30");
  assert.equal(items[0].output, "aGk=");
  assert.equal(items[1].command, undefined, "an absent field stays absent");
});

test("every cancellable-work method refuses before writing without the capability", async () => {
  const { client, requests } = await fixture([]);

  await inOrder(
    [
      () => client.cancelTurn("s1", "run-7"),
      () => client.cancelToolCall("s1", "call-3"),
      () => client.cancelBackgroundTerminal("s1", "call-4"),
      () => client.listBackgroundWork("s1"),
    ],
    async (call) => {
      await assert.rejects(call(), /cancellable_work_v1/);
    },
  );
  assert.equal(
    requests.filter((entry) => entry.req.startsWith("cancel") || entry.req === "list_background_work").length,
    0,
    "a refused method must not reach the socket",
  );
});
