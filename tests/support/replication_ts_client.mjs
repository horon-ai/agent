import { pathToFileURL } from "node:url";

const sdk = process.env.TS_SDK;
const { HoronAgentClient } = await import(pathToFileURL(`${sdk}/dist/index.js`).href);

const connect = (socketPath) =>
  HoronAgentClient.connect({
    socketPath,
    clientName: "replication-ts-qa/1.0",
    requestTimeoutMs: 60_000,
  });

const a = await connect(process.env.A_API);
const b = await connect(process.env.B_API);
const report = {
  ok: false,
  canonical_preserved: false,
  duplicate_idempotent: false,
  history_matched: false,
};

try {
  const session = await a.createSession(process.cwd());
  const id = session.session_id;
  await a.sendContextMessage(id, "typescript replication fixture");

  const page = await a.exportSessionEvents(id);
  const imported = await b.importSessionEvents(page);

  const heads = await b.listSessionHeads(id);
  const history = await b.getHistoryPage(id, { limit: 10 });
  const reexport = await b.exportSessionEvents(id);

  const canonical = (p) => p.events.map((event) => [event.event_id, event.canonical_json]);
  report.canonical_preserved =
    JSON.stringify(canonical(reexport)) === JSON.stringify(canonical(page));
  report.history_matched =
    heads.heads.join(",") === page.heads.join(",") &&
    history.messages.some((message) => message.content.includes("typescript replication fixture"));

  const again = await b.importSessionEvents(page);
  report.duplicate_idempotent =
    JSON.stringify(again.accepted_ids) === JSON.stringify(imported.accepted_ids);

  let invalidCursor = false;
  try {
    await b.getHistoryPage(id, { beforeEventId: "00000000-0000-4000-8000-000000000000" });
  } catch (error) {
    invalidCursor = error.code === "invalid_request";
  }

  report.ok =
    report.canonical_preserved &&
    report.duplicate_idempotent &&
    report.history_matched &&
    invalidCursor &&
    imported.accepted_ids.length === page.events.length;
} finally {
  a.close();
  b.close();
}

console.log(JSON.stringify(report));
