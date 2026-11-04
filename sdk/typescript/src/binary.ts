import { createRequire } from "node:module";
import path from "node:path";
import { nodeErrorCode } from "./errors.js";

const require = createRequire(import.meta.url);

const PLATFORM_PACKAGES: Record<string, string> = {
  "linux-x64": "@horon-ai/agent-linux-x64",
  "linux-arm64": "@horon-ai/agent-linux-arm64",
  "darwin-x64": "@horon-ai/agent-darwin-x64",
  "darwin-arm64": "@horon-ai/agent-darwin-arm64",
  "win32-x64": "@horon-ai/agent-win32-x64",
  "win32-arm64": "@horon-ai/agent-win32-arm64",
};

/** The optional npm package containing the runtime for this machine. */
export function platformBinaryPackage(platform = process.platform, arch = process.arch): string | undefined {
  return PLATFORM_PACKAGES[`${platform}-${arch}`];
}

/**
 * Resolve the horon-agent executable installed as an optional platform dependency.
 * Returns undefined on unsupported platforms or when optional dependencies
 * were deliberately omitted, allowing launch() to fall back to PATH.
 */
export function bundledHoronAgentBinary(): string | undefined {
  const packageName = platformBinaryPackage();
  if (!packageName) return undefined;
  try {
    const manifest = require.resolve(`${packageName}/package.json`);
    return path.join(path.dirname(manifest), "bin", process.platform === "win32" ? "horon-agent.exe" : "horon-agent");
  } catch (error) {
    if (nodeErrorCode(error) === "MODULE_NOT_FOUND") return undefined;
    throw error;
  }
}
