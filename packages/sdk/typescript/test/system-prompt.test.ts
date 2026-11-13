import { afterEach, test } from "bun:test";
import assert from "node:assert/strict";
import { HoronAgentClient, type CreateSessionOptions } from "../dist/index.js";
import { inOrder, startMockHarness } from "./mock-harness.ts";

// bun:test has no per-test context object, so a test registers its teardown here and
// the afterEach below drains it once the test that pushed it has finished.
const teardown: Array<() => void | Promise<void>> = [];
afterEach(async () => {
  await inOrder(teardown.splice(0), (close) => Promise.resolve(close()));
});

const cases: { name: string; options?: string | CreateSessionOptions; prompt?: string; workingDir?: string }[] = [
  {
    name: "provided",
    options: { workingDir: "/project", systemPrompt: "Review precisely.\nKeep Unicode: λ" },
    prompt: "Review precisely.\nKeep Unicode: λ",
    workingDir: "/project",
  },
  { name: "empty", options: { systemPrompt: "" }, prompt: "" },
  { name: "omitted", options: { workingDir: "/project" }, workingDir: "/project" },
  { name: "undefined", options: { systemPrompt: undefined } },
  { name: "default options", options: {} },
  { name: "no arguments" },
  { name: "legacy working directory", options: "/legacy", workingDir: "/legacy" },
];

for (const { name, options, prompt, workingDir } of cases) {
  test(`createSession system prompt wire behavior: ${name}`, async () => {
    const requests: any[] = [];
    const server = await startMockHarness({
      onRequest(req, send) {
        requests.push(req);
        send({ v: 1, reply_to: req.id, ev: "attached", session: { session_id: "created" } });
      },
    });
    teardown.push(() => server.close());
    const client = await HoronAgentClient.connect({ socketPath: server.socketPath });
    teardown.push(() => client.close());
    assert.equal((await client.createSession(options)).session_id, "created");
    assert.equal(requests.length, 1);
    const wire = requests[0];
    assert.equal(wire.req, "create_session");
    assert.equal(wire.working_dir, workingDir);
    assert.equal(wire.system_prompt, prompt);
    assert.equal(Object.hasOwn(wire, "system_prompt"), prompt !== undefined);
    assert.equal(Object.hasOwn(wire, "systemPrompt"), false);
  });
}
