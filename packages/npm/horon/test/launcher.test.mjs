// Tests for the npm launcher. The platform package is faked in a temporary
// node_modules tree, because the real one is a release binary this test cannot
// download or run.
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

// The fake platform package holds a Node script standing in for the release
// binary, so these tests need a POSIX shebang like the real archives assume.
const posixOnly = { skip: process.platform === "win32" ? "fake entry is a shebang script" : false };

const launcher = path.join(path.dirname(fileURLToPath(import.meta.url)), "..", "bin", "horon.js");

// One temporary tree per test: <root>/node_modules/@horon-ai/horon/bin/horon.js
// plus the platform package the launcher would find.
function fixture({ platform = process.platform, arch = process.arch, entry = true } = {}) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "horon-launcher-"));
  const wrapperDir = path.join(root, "node_modules", "@horon-ai", "horon", "bin");
  fs.mkdirSync(wrapperDir, { recursive: true });
  fs.copyFileSync(launcher, path.join(wrapperDir, "horon.js"));

  const key = `${platform}-${arch}`;
  const packageDir = path.join(root, "node_modules", "@horon-ai", `horon-${key}`);
  fs.mkdirSync(path.join(packageDir, "bin"), { recursive: true });
  fs.writeFileSync(
    path.join(packageDir, "package.json"),
    JSON.stringify({ name: `@horon-ai/horon-${key}`, version: "0.0.0" }),
  );
  if (entry) {
    // Prints what the launcher passed through and exits 7, so a passing test
    // proves both the argument and the environment hand-off.
    const entryPath = path.join(packageDir, "bin", platform === "win32" ? "horon-agent.exe" : "horon-agent");
    fs.writeFileSync(
      entryPath,
      [
        "#!/usr/bin/env node",
        "const args = process.argv.slice(2).join(',')",
        "const marker = process.env.HORON_TEST_MARKER ?? null",
        "process.stdout.write(JSON.stringify({ args, marker, noAutoUpdate: process.env.HORON_AGENT_NO_AUTO_UPDATE }))",
        "process.exit(7)",
        "",
      ].join("\n"),
    );
    fs.chmodSync(entryPath, 0o755);
  }
  return { root, key, launcherPath: path.join(wrapperDir, "horon.js") };
}

function run(file, args, { cwd, env = {} } = {}) {
  return spawnSync(process.execPath, [file, ...args], {
    encoding: "utf8",
    cwd,
    env: { ...process.env, NODE_PATH: "", ...env },
  });
}

test("forwards arguments, disables auto-update, and returns the child status", posixOnly, () => {
  const { root, key } = fixture();
  const result = run(path.join(root, "node_modules", "@horon-ai", "horon", "bin", "horon.js"), [
    "--version",
    "run",
    "--cwd",
    "/tmp/example",
  ]);
  assert.equal(result.status, 7, result.stderr);
  assert.deepEqual(JSON.parse(result.stdout), {
    args: "--version,run,--cwd,/tmp/example",
    marker: null,
    noAutoUpdate: "1",
  });
  // The launcher resolved the platform package for this machine and nothing else.
  assert.equal(key, `${process.platform}-${process.arch}`);
});

test("passes the caller's environment through and overrides only the update switch", posixOnly, () => {
  const { root } = fixture();
  const result = run(
    path.join(root, "node_modules", "@horon-ai", "horon", "bin", "horon.js"),
    [],
    { env: { HORON_AGENT_NO_AUTO_UPDATE: "0", HORON_TEST_MARKER: "kept" } },
  );
  assert.equal(result.status, 7, result.stderr);
  const seen = JSON.parse(result.stdout);
  assert.equal(seen.marker, "kept");
  assert.equal(seen.noAutoUpdate, "1");
});

test("a missing platform package names it and points at optional dependencies", () => {
  const { root } = fixture({ entry: false });
  // Removing the package makes `require.resolve` fail the way an
  // `--omit=optional` install does.
  fs.rmSync(path.join(root, "node_modules", "@horon-ai", `horon-${process.platform}-${process.arch}`), {
    recursive: true,
  });
  const result = run(path.join(root, "node_modules", "@horon-ai", "horon", "bin", "horon.js"), []);
  assert.equal(result.status, 1, result.stderr);
  assert.match(result.stderr, new RegExp(`@horon-ai/horon-${process.platform}-${process.arch}`));
  assert.match(result.stderr, /--include=optional/);
});

test("an unsupported platform names it, lists the supported ones, and exits 1", () => {
  const { launcherPath } = fixture();
  const script = [
    'Object.defineProperty(process, "platform", { value: "aix" })',
    `const launcher = require(${JSON.stringify(launcherPath)})`,
    "process.exit(launcher.main())",
    "",
  ].join("\n");
  const result = spawnSync(process.execPath, ["-e", script], { encoding: "utf8" });
  assert.equal(result.status, 1, result.stderr);
  assert.match(result.stderr, /aix-.* is not a supported platform/);
  assert.match(result.stderr, /linux-x64/);
  assert.match(result.stderr, /win32-arm64/);
  assert.match(result.stderr, /--include=optional/);
});
