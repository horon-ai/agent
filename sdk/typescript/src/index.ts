/**
 * TypeScript SDK for the horon-agent harness API.
 *
 * ```ts
 * import { HoronAgentClient } from "@horon-ai/agent-sdk";
 * const client = await HoronAgentClient.connect({ clientName: "my-app/1.0" });
 * const session = await client.createSession(process.cwd());
 * const turn = await client.run(session.session_id, "hello");
 * console.log(turn.text);
 * client.close();
 * ```
 */

export * from "./protocol.js";
export * from "./sockets.js";
export * from "./framing.js";
export { HarnessError } from "./errors.js";
export { launchInstance, inheritCredentials, userHoronAgentHome, userAppConfigDir } from "./launch.js";
export type { LaunchOptions, LaunchedInstance } from "./launch.js";
export { bundledHoronAgentBinary, platformBinaryPackage } from "./binary.js";
export { HoronAgentClient, unixSocketTransport } from "./client.js";
export type { SdkTool, SessionToolsOptions, ToolExecutionContext, ToolResult } from "./tools.js";
export type {
  AssistantTextMessage,
  ConnectOptions,
  CreateSessionOptions,
  FileContent,
  FileStatus,
  GlobalEventsOptions,
  RunOptions,
  RunStructuredOptions,
  RuntimeInfo,
  SendMessageOptions,
  StructuredTurnResult,
  Transport,
  TurnResult,
} from "./client.js";
export { StructuredOutputError } from "./structured.js";
export type { StructuredOutputAttempt, StructuredOutputSchema, StructuredValidationIssue } from "./structured.js";
