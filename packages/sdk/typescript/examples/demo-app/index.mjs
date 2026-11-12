import { HoronAgentClient } from "@horon-ai/agent-sdk";

const prompt = process.argv.slice(2).join(" ") || "Describe this directory in one sentence.";

const client = await HoronAgentClient.launch({
  workingDir: process.cwd(),
  binary: process.env.HORON_AGENT_BINARY,
});

try {
  console.log(`Connected to ${client.server}`);

  const session = await client.createSession();
  console.log(`Session: ${session.session_id}\n`);

  const turn = await client.run(session.session_id, prompt, {
    autoApprove: true,
    onEvent(event) {
      if (event.ev === "text_delta") process.stdout.write(event.text);
      if (event.ev === "tool_start") process.stdout.write(`\n[tool: ${event.name}]\n`);
    },
  });

  console.log(`\n\nCompleted with ${turn.toolCalls.length} tool call(s).`);
} finally {
  await client.close();
}
