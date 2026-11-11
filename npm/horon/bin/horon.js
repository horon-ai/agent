#!/usr/bin/env node
// Launch the Horon CLI installed through npm.
//
// Each supported platform ships its binaries in its own package, so this
// launcher only picks the right package, forwards the arguments and
// environment, and hands the terminal to the child. No CLI behaviour lives
// here: the headless agent and the terminal client are the Rust binaries.
"use strict";

const path = require("node:path");
const { spawnSync } = require("node:child_process");

// Platform key -> npm package that carries that platform's binaries.
const PACKAGES = {
  "linux-x64": "@horon-ai/horon-linux-x64",
  "linux-arm64": "@horon-ai/horon-linux-arm64",
  "darwin-x64": "@horon-ai/horon-darwin-x64",
  "darwin-arm64": "@horon-ai/horon-darwin-arm64",
  "win32-x64": "@horon-ai/horon-win32-x64",
  "win32-arm64": "@horon-ai/horon-win32-arm64",
};

// The npm install cannot fall back to a source build, so a missing optional
// dependency is a reinstall with optional dependencies enabled, not a crash.
const INSTALL_HINT =
  "reinstall with optional dependencies enabled: npm install -g --include=optional @horon-ai/horon";

function platformKey(platform = process.platform, arch = process.arch) {
  return `${platform}-${arch}`;
}

function entryName(platform) {
  return platform === "win32" ? "horon-agent.exe" : "horon-agent";
}

// Absolute path of the CLI binary inside the platform package. The platform
// packages declare no `exports` map, so resolving their package.json yields the
// directory npm installed them into.
function resolveEntry(platform, arch) {
  const name = PACKAGES[platformKey(platform, arch)];
  if (name === undefined) {
    return undefined;
  }
  return path.join(path.dirname(require.resolve(`${name}/package.json`)), "bin", entryName(platform));
}

function fail(message) {
  process.stderr.write(`horon: ${message}\n`);
  return 1;
}

function main(argv = process.argv.slice(2), platform = process.platform, arch = process.arch) {
  const key = platformKey(platform, arch);
  const name = PACKAGES[key];
  if (name === undefined) {
    return fail(
      `${key} is not a supported platform. Supported: ${Object.keys(PACKAGES).join(", ")}. ${INSTALL_HINT}`,
    );
  }

  let entry;
  try {
    entry = resolveEntry(platform, arch);
  } catch (error) {
    if (error.code !== "MODULE_NOT_FOUND") {
      throw error;
    }
    return fail(`the platform package ${name} is not installed. ${INSTALL_HINT}`);
  }

  // The npm package pins a version and ships `horon-tui` beside the agent, and
  // the binary's own updater would replace both with a different release.
  const env = { ...process.env, HORON_AGENT_NO_AUTO_UPDATE: "1" };

  // The child owns the terminal while it runs. Ignoring the signals here keeps
  // an interrupt from killing the wrapper before the CLI can handle it.
  const ignore = () => {};
  process.on("SIGINT", ignore);
  process.on("SIGTERM", ignore);
  const result = spawnSync(entry, argv, { stdio: "inherit", env });
  process.removeListener("SIGINT", ignore);
  process.removeListener("SIGTERM", ignore);

  if (result.error) {
    return fail(`could not start ${entry} from ${name}: ${result.error.message}`);
  }
  if (result.signal) {
    process.kill(process.pid, result.signal);
    return 1;
  }
  return result.status === null ? 1 : result.status;
}

module.exports = { PACKAGES, INSTALL_HINT, platformKey, entryName, resolveEntry, main };

if (require.main === module) {
  process.exitCode = main();
}
