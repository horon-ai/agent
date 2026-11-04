/**
 * Launching a private horon-agent instance.
 *
 * `HoronAgentClient.connect()` attaches to whatever horon-agent is already running on the
 * machine, which is right for a tool that automates *your* horon-agent (an editor
 * plugin, a dashboard) and wrong for everything else. An application embedding
 * horon-agent as an agent engine wants its own instance: its own sessions, its own
 * state, and no way to disturb the user's live work by accident.
 *
 * `launch()` gives it one. It starts a private daemon and bridge under a
 * dedicated `HORON_AGENT_HOME` and runtime directory, and shuts them down on
 * `close()`.
 */

import { spawn, type ChildProcess } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { HarnessError, nodeErrorCode } from "./errors.js";
import { bundledHoronAgentBinary, platformBinaryPackage } from "./binary.js";

/** Resolve after `ms`, letting the event loop run while this instance waits. */
function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

/**
 * Files inherited from the user's horon-agent home when logins are inherited.
 *
 * Deliberately *not* included: `auth-refresh-state.json` and
 * `auth-validation.json`, which are derived records of past auth failures.
 * Copying them imports another horon-agent's bad day, and a stale
 * `rejected_refresh_fingerprint` makes a fresh instance refuse to even attempt
 * a refresh with credentials that work.
 */
const CREDENTIAL_FILES = [
  "auth.json",
  "openai-auth.json",
  "antigravity_oauth.json",
  "gemini_oauth.json",
  "google_oauth.json",
  "google_credentials.json",
  "config.toml",
];

/**
 * Credentials that must be *shared* with the user's home, not copied.
 *
 * OAuth refresh tokens rotate: redeeming one issues a new one and invalidates
 * the old. Two homes holding copies of the same token therefore fight, and
 * whichever refreshes second is logged out. A copy also goes stale on its own,
 * since the access token it holds expires in hours while an instance may run
 * for days. Sharing the file keeps one rotation record for one set of
 * credentials, which is what the tokens themselves already assume.
 */
const SHARED_FILES = new Set(CREDENTIAL_FILES.filter((name) => name !== "config.toml"));

/**
 * Where horon-agent looks for *other* tools' credentials, relative to `$HOME`.
 *
 * horon-agent can log in by reusing an existing CLI's OAuth store, so a large share
 * of real users have no usable `~/.horon-agent/auth.json` at all: the working
 * credentials live in files under `~/.claude/` or
 * `~/.config/github-copilot/`. Under
 * `HORON_AGENT_HOME` these lookups are sandboxed to `$HORON_AGENT_HOME/external/`, so an
 * instance that inherits only `auth.json` silently has no credentials and
 * fails on the first turn. Linking the recognized credential files makes
 * inheritance mean what it says without exposing either directory wholesale.
 */
/**
 * horon-agent's own config directory, relative to the platform config root.
 *
 * `app_config_dir()` is where provider env files live (`anthropic.env`,
 * `n.env` for the horon-agent subscription), and `HORON_AGENT_HOME` redirects it to
 * `$HORON_AGENT_HOME/config/horon-agent`. It is easy to miss because it is not under
 * `~/.horon-agent` at all, and missing it is not a subtle failure: on a machine
 * whose working credential is a horon-agent subscription, `auth.json` holds only a
 * stale OAuth token, so the instance inherits exactly the credential that does
 * not work and none of the ones that do.
 */
const APP_CONFIG_DIRNAME = "horon-agent";

const EXTERNAL_CREDENTIAL_FILES = [
  ".claude/.credentials.json",
  ".codex/auth.json",
  ".gemini/oauth_creds.json",
  ".cursor/auth.json",
  ".config/cursor/auth.json",
  "AppData/Roaming/Cursor/auth.json",
  ".config/Cursor/User/globalStorage/state.vscdb",
  ".config/cursor/User/globalStorage/state.vscdb",
  "Library/Application Support/Cursor/User/globalStorage/state.vscdb",
  "Library/Application Support/cursor/User/globalStorage/state.vscdb",
  "AppData/Roaming/Cursor/User/globalStorage/state.vscdb",
  "AppData/Roaming/cursor/User/globalStorage/state.vscdb",
  ".config/github-copilot/hosts.json",
  ".config/github-copilot/apps.json",
  ".copilot/config.json",
  ".hermes/auth.json",
  ".pi/agent/auth.json",
  ".openclaw/agent/auth.json",
  ".openclaw/credentials/oauth.json",
  ".local/share/opencode/auth.json",
];

/** Ensure a directory below an instance root contains no symlink components. */
function ensureInstanceDirectory(root: string, relative: string): string {
  if (path.isAbsolute(relative) || relative.split(/[\\/]+/u).includes("..")) {
    throw new HarnessError("invalid_instance_home", `unsafe instance path: ${relative}`);
  }

  let current = root;
  for (const part of relative.split(/[\\/]+/u).filter(Boolean)) {
    current = path.join(current, part);
    try {
      const stats = fs.lstatSync(current);
      if (stats.isSymbolicLink()) {
        // Migrate homes made by SDK versions that linked whole credential
        // directories. unlinkSync removes the link itself, never its target.
        fs.unlinkSync(current);
        fs.mkdirSync(current, { mode: 0o700 });
      } else if (!stats.isDirectory()) {
        throw new HarnessError("invalid_instance_home", `instance credential path is not a directory: ${current}`);
      }
    } catch (error) {
      if (nodeErrorCode(error) !== "ENOENT") throw error;
      fs.mkdirSync(current, { mode: 0o700 });
    }
  }
  return current;
}

/** Replace an instance-relative path with a link to one credential file. */
function linkCredentialFile(source: string, root: string, relative: string): boolean {
  let sourceStats;
  try {
    sourceStats = fs.statSync(source);
  } catch {
    return false;
  }
  if (!sourceStats.isFile()) return false;

  const parent = ensureInstanceDirectory(root, path.dirname(relative));
  const destination = path.join(parent, path.basename(relative));
  try {
    fs.unlinkSync(destination);
  } catch {
    // Absent, which is the common case.
  }
  fs.symlinkSync(source, destination);
  return true;
}

export type WakeMode = "internal" | "external";

export interface LaunchOptions {
  /**
   * Directory holding the instance's state (sessions, logs, credentials).
   *
   * Defaults to a fresh temporary directory that is removed on `close()`.
   * Pass a stable path to keep sessions across runs.
   */
  horonAgentHome?: string;
  /** Working directory for sessions created in this instance. */
  workingDir?: string;
  /**
   * Share the user's provider logins with the instance. Defaults to `true`.
   *
   * Without credentials a fresh instance cannot talk to any model, so the
   * default is the one that works. It does mean the embedding application
   * spends the user's provider quota, so pass `false` to start empty and
   * supply credentials yourself.
   */
  inheritLogins?: boolean;
  /** Path to the horon-agent binary. Defaults to the npm-bundled runtime, then `horon-agent` on PATH. */
  binary?: string;
  /** Extra environment variables for the instance. */
  env?: Record<string, string>;
  /**
   * Default model for spawned swarm workers, unless overridden per spawn.
   * Use `inherit` to default to the coordinator's model and auth route. This setting takes
   * precedence over `env.HORON_AGENT_SWARM_MODEL`.
   */
  swarmModel?: string;
  /**
   * Who executes autonomous wake requests. This operator-level setting takes
   * precedence over `env.HORON_AGENT_WAKE_MODE`.
   */
  wakeMode?: WakeMode;
  /** Milliseconds to wait for the socket to appear. Defaults to 30000. */
  startupTimeoutMs?: number;
  /** Forward the instance's stderr to this process. Defaults to false. */
  inheritStderr?: boolean;
  /**
   * Milliseconds `close()` will spend removing an ephemeral instance home.
   *
   * Background work started before shutdown can keep writing for seconds after
   * the daemon is asked to stop, recreating the directory behind a delete.
   * Defaults to 30000; lower it if a caller would rather leak than wait.
   */
  cleanupTimeoutMs?: number;
}

/** A running private horon-agent instance. */
export interface LaunchedInstance {
  /** API socket path to connect to. */
  socketPath: string;
  /** The instance's `HORON_AGENT_HOME`. */
  horonAgentHome: string;
  /** The bridge process. */
  process: ChildProcess;
  /** Stop the instance and clean up anything it created. */
  shutdown(): Promise<void>;
}

/**
 * The user's home directory as the Rust side resolves it: `$HOME` outside
 * Windows. Bun's `os.homedir()` does not follow a runtime change to HOME.
 */
function userHomeDir(): string {
  if (process.platform !== "win32" && process.env.HOME) return process.env.HOME;
  return os.homedir();
}

/** Resolve the user's real horon-agent home, ignoring any instance override. */
export function userHoronAgentHome(): string {
  return process.env.HORON_AGENT_HOME ?? path.join(userHomeDir(), ".horon-agent");
}

/** The user's horon-agent config directory, mirroring `storage::app_config_dir`. */
export function userAppConfigDir(): string {
  if (process.platform === "darwin") {
    return path.join(userHomeDir(), "Library", "Application Support", APP_CONFIG_DIRNAME);
  }
  if (process.platform === "win32") {
    const appData = process.env.APPDATA ?? path.join(userHomeDir(), "AppData", "Roaming");
    return path.join(appData, APP_CONFIG_DIRNAME);
  }
  const xdg = process.env.XDG_CONFIG_HOME ?? path.join(userHomeDir(), ".config");
  return path.join(xdg, APP_CONFIG_DIRNAME);
}

/**
 * Give a launched instance the user's provider logins.
 *
 * Credential files are symlinked so token rotation stays coherent (see
 * {@link SHARED_FILES}); everything else is copied, so the instance can edit
 * its own configuration without touching the user's. Copies are owner-only.
 *
 * Returns the names actually inherited, so a caller can tell "inherited
 * nothing" from "inherited something".
 */
export function inheritCredentials(fromHome: string, toHome: string): string[] {
  fs.mkdirSync(toHome, { recursive: true, mode: 0o700 });
  const toStats = fs.lstatSync(toHome);
  if (toStats.isSymbolicLink() || !toStats.isDirectory()) {
    throw new HarnessError(
      "invalid_instance_home",
      `instance home must be a real directory, not a link or file: ${toHome}`,
    );
  }
  if (fs.existsSync(fromHome) && fs.realpathSync(fromHome) === fs.realpathSync(toHome)) {
    throw new HarnessError("invalid_instance_home", "instance home must be different from the user's horon-agent home");
  }

  const inherited: string[] = [];
  for (const name of CREDENTIAL_FILES) {
    const source = path.join(fromHome, name);
    if (!fs.existsSync(source)) continue;
    const destination = path.join(toHome, name);
    if (SHARED_FILES.has(name)) {
      // A reused `horonAgentHome` already has these links, and symlinkSync throws
      // EEXIST rather than replacing. Relinking also repoints a stale link
      // from an older run, so replace rather than skip.
      linkCredentialFile(source, toHome, name);
    } else {
      fs.copyFileSync(source, destination);
      fs.chmodSync(destination, 0o600);
    }
    inherited.push(name);
  }

  // horon-agent's provider env files live in its platform config directory, which
  // `HORON_AGENT_HOME` moves to `$HORON_AGENT_HOME/config/horon-agent`. Link only the env files:
  // caches and usage data are not credentials and must stay instance-private.
  // Most importantly, never link the directory itself. A buggy recursive
  // cleanup can descend through a directory link and delete the user's files;
  // a file link can only ever be unlinked at the instance-side path.
  const userConfig = userAppConfigDir();
  try {
    for (const entry of fs.readdirSync(userConfig, { withFileTypes: true })) {
      if ((!entry.isFile() && !entry.isSymbolicLink()) || !entry.name.endsWith(".env")) continue;
      const relative = `config/${APP_CONFIG_DIRNAME}/${entry.name}`;
      if (linkCredentialFile(path.join(userConfig, entry.name), toHome, relative)) {
        inherited.push(relative);
      }
    }
  } catch {
    // No app config directory is a normal fresh-install state.
  }

  // Other CLIs' credential stores, which horon-agent reads directly and which
  // `HORON_AGENT_HOME` redirects to `$HORON_AGENT_HOME/external/`. Share only the exact
  // credential files. Linking whole directories would also expose transcripts,
  // configuration, and anything those tools add in the future.
  for (const relative of EXTERNAL_CREDENTIAL_FILES) {
    const source = path.join(userHomeDir(), relative);
    const instanceRelative = path.join("external", relative);
    if (linkCredentialFile(source, toHome, instanceRelative)) {
      inherited.push(`external/${relative}`);
    }
  }

  // OpenClaw's current store is per-agent. Enumerate one level of agent IDs,
  // then link only the two credential filenames its loader recognizes.
  const openclawAgents = path.join(userHomeDir(), ".openclaw", "agents");
  try {
    for (const agent of fs.readdirSync(openclawAgents, { withFileTypes: true })) {
      if (!agent.isDirectory()) continue;
      for (const name of ["auth-profiles.json", "auth.json"]) {
        const relative = path.join(".openclaw", "agents", agent.name, "agent", name);
        if (linkCredentialFile(path.join(userHomeDir(), relative), toHome, path.join("external", relative))) {
          inherited.push(`external/${relative}`);
        }
      }
    }
  } catch {
    // OpenClaw is optional.
  }
  return inherited;
}

/**
 * Pid of the daemon serving an instance, read from its own server registry.
 *
 * Synchronous on purpose: the only caller is a process "exit" handler, which
 * cannot await, so shelling out to the CLI is not available. horon-agent records
 * every server in `$HORON_AGENT_HOME/servers.json` keyed by socket path, and an
 * instance's registry lists only that instance's daemon, so this can never
 * resolve to the server the user is running themselves.
 */
function readDaemonPidSync(horonAgentHome: string, runtimeDir: string): number | undefined {
  let raw: string;
  try {
    raw = fs.readFileSync(path.join(horonAgentHome, "servers.json"), "utf8");
  } catch {
    return undefined;
  }
  let registry: Record<string, { socket?: string; pid?: number }>;
  try {
    registry = JSON.parse(raw);
  } catch {
    return undefined;
  }
  const socket = path.join(runtimeDir, "horon-agent.sock");
  for (const entry of Object.values(registry)) {
    if (entry?.socket === socket && typeof entry.pid === "number" && entry.pid > 1) {
      return entry.pid;
    }
  }
  return undefined;
}

/** Wait briefly for a newly started daemon to publish its registry entry. */
async function waitForDaemonPid(
  horonAgentHome: string,
  runtimeDir: string,
  timeoutMs = 2000,
): Promise<number | undefined> {
  const deadline = Date.now() + timeoutMs;
  // Poll, in a sequence rather than a loop: the registry read has to wait out
  // each interval, and the wait is the only thing a loop would reorder.
  const attempt = async (): Promise<number | undefined> => {
    const pid = readDaemonPidSync(horonAgentHome, runtimeDir);
    if (pid !== undefined) return pid;
    await sleep(50);
    if (Date.now() >= deadline) return undefined;
    return attempt();
  };
  return attempt();
}

export async function waitForDaemonPidForTest(
  horonAgentHome: string,
  runtimeDir: string,
  timeoutMs?: number,
): Promise<number | undefined> {
  return waitForDaemonPid(horonAgentHome, runtimeDir, timeoutMs);
}

/**
 * Stop an instance's daemon and wait for it to actually be gone.
 *
 * The pid comes from the instance's own `servers.json` rather than from
 * `horon-agent server stop`: spawning a second horon-agent binary to read a pid out of a
 * JSON file costs five seconds of process startup, and `close()` blocking that
 * long makes the SDK feel broken. The registry is scoped to this instance's
 * socket path, so this can never signal the server the user is running.
 */
async function stopInstanceDaemon(_binary: string, horonAgentHome: string, runtimeDir: string): Promise<void> {
  // The API socket can become connectable just before the daemon writes its
  // servers.json entry. close() may therefore run during this small startup
  // window, so do not silently give up after a single registry read.
  const pid = await waitForDaemonPid(horonAgentHome, runtimeDir);
  if (pid === undefined) return;

  const signal = (sig: NodeJS.Signals) => {
    try {
      // Negative pid: the daemon leads its own group after setsid(), and its
      // helper children have to go too or they keep writing into the home.
      process.kill(-pid, sig);
    } catch {
      try {
        process.kill(pid, sig);
      } catch {
        // Already gone.
      }
    }
  };

  signal("SIGTERM");

  // Wait for a clean exit, but not for long. The daemon's own shutdown does
  // background work (flushing a multi-megabyte search index, among other
  // things) that can take fifteen seconds. Nothing there is worth preserving:
  // the instance is ephemeral and about to be deleted, so a short graceful
  // window followed by SIGKILL is both faster and more predictable.
  const gracePeriod = Date.now() + 2000;
  if (await waitForProcessExit(pid, gracePeriod)) return;

  signal("SIGKILL");

  // SIGKILL is not instant: the kernel still has to tear the process down, and
  // returning before it is gone races the delete that follows.
  const hardDeadline = Date.now() + 5000;
  await waitForProcessExit(pid, hardDeadline);
}

/**
 * True once `pid` is gone, false when `deadline` passes first.
 *
 * Recursion rather than a loop: each check waits out its interval before the
 * next one, and the caller branches on whether the process left on its own.
 */
async function waitForProcessExit(pid: number, deadline: number): Promise<boolean> {
  if (Date.now() >= deadline) return false;
  if (!processExists(pid)) return true;
  await sleep(50);
  return waitForProcessExit(pid, deadline);
}

/** True while `pid` is still a live process. */
function processExists(pid: number): boolean {
  try {
    // Signal 0 tests for existence without touching the process.
    process.kill(pid, 0);
    return true;
  } catch {
    return false;
  }
}

/**
 * Delete an ephemeral instance home, refusing to follow symlinks out of it.
 *
 * The home contains symlinks that point at the user's real credential files.
 * Production inheritance never links directories, but cleanup still treats an
 * injected or legacy directory link as hostile: links are unlinked and never
 * descended into, and anything unexpected is left in place rather than
 * force-removed.
 */
export function removeInstanceHomeForTest(home: string): void {
  removeInstanceHome(home);
}

function removeInstanceHome(home: string): void {
  // Only homes created by this module are eligible for recursive cleanup.
  // Persistent caller-supplied homes are never passed here, but keep the guard
  // local to the destructive primitive so a future call site cannot turn a
  // lifecycle bug into deletion of an arbitrary directory.
  const resolvedHome = path.resolve(home);
  const tempRoot = path.resolve(os.tmpdir());
  if (path.dirname(resolvedHome) !== tempRoot || !path.basename(resolvedHome).startsWith("horon-agent-sdk-instance-")) {
    return;
  }

  try {
    removeTree(home);
  } catch {
    // A stray file is a leaked temp directory; deleting the wrong thing is
    // unrecoverable. Prefer the leak.
  }
}

/** Recursively unlink a tree, never descending through a symlink. */
function removeTree(target: string): void {
  let stats;
  try {
    stats = fs.lstatSync(target);
  } catch {
    return;
  }

  // `lstat`, not `stat`: a symlink must be reported as a link so it is
  // unlinked rather than descended into. Swapping in `stat` here deletes the
  // user's real credential store.
  if (stats.isSymbolicLink()) {
    fs.unlinkSync(target);
    return;
  }

  if (stats.isDirectory()) {
    for (const entry of fs.readdirSync(target)) {
      removeTree(path.join(target, entry));
    }
    fs.rmdirSync(target);
    return;
  }
  fs.unlinkSync(target);
}

/**
 * Start a private horon-agent instance and return once its API socket is accepting
 * connections.
 * Private instances never auto-update: they run the binary that launched them.
 */
export async function launchInstance(options: LaunchOptions = {}): Promise<LaunchedInstance> {
  const binary = options.binary ?? bundledHoronAgentBinary() ?? "horon-agent";
  const ephemeral = options.horonAgentHome === undefined;
  const horonAgentHome = options.horonAgentHome ?? fs.mkdtempSync(path.join(os.tmpdir(), "horon-agent-sdk-instance-"));
  fs.mkdirSync(horonAgentHome, { recursive: true, mode: 0o700 });

  // The runtime directory holds the sockets. Keeping it inside the instance
  // home is what makes the instance private: the daemon binds its socket
  // there rather than in the shared $XDG_RUNTIME_DIR, so a launched instance
  // and the user's own horon-agent cannot collide or find each other.
  const runtimeDir = path.join(horonAgentHome, "run");
  fs.mkdirSync(runtimeDir, { recursive: true, mode: 0o700 });
  const socketPath = path.join(runtimeDir, "horon-agent-api.sock");

  // A reused `horonAgentHome` still holds the previous run's socket files. The
  // startup loop waits for the API socket to *appear*, so a leftover one makes
  // launch() return immediately against a socket nothing is listening on, and
  // the first request fails with ECONNREFUSED. Clearing them is safe: a live
  // instance on this home would mean two daemons sharing one state directory,
  // which is already unsupported.
  for (const stale of ["horon-agent-api.sock", "horon-agent.sock", "horon-agent-debug.sock", "horon-agent.sock.hash"]) {
    try {
      fs.unlinkSync(path.join(runtimeDir, stale));
    } catch {
      // Absent, which is the normal case for a fresh home.
    }
  }

  if (options.inheritLogins ?? true) {
    inheritCredentials(userHoronAgentHome(), horonAgentHome);
  }

  const child = spawn(
    binary,
    // `--no-update` turns off the bridge's own update check, which also runs for
    // source builds.
    ["--no-update", "api-bridge", "--api-socket", socketPath],
    {
      cwd: options.workingDir ?? process.cwd(),
      env: {
        ...process.env,
        HORON_AGENT_HOME: horonAgentHome,
        HORON_AGENT_RUNTIME_DIR: runtimeDir,
        HORON_AGENT_API_SOCKET: socketPath,
        HORON_AGENT_SOCKET: path.join(runtimeDir, "horon-agent.sock"),
        // An empty private home always looks out of date, so the release updater
        // would replace and restart the runtime mid-session: the client's turn fails
        // and the restarted server is orphaned. The variable is inherited by every
        // horon-agent process the instance spawns (argv is not), so it keeps the release
        // auto-installer off in those too. The shared runtime still updates.
        HORON_AGENT_NO_AUTO_UPDATE: "1",
        ...options.env,
        ...(options.swarmModel === undefined ? {} : { HORON_AGENT_SWARM_MODEL: options.swarmModel }),
        ...(options.wakeMode === undefined ? {} : { HORON_AGENT_WAKE_MODE: options.wakeMode }),
      },
      stdio: ["ignore", "ignore", options.inheritStderr ? "inherit" : "pipe"],
      detached: false,
    },
  );

  // Keep the last of stderr: when startup fails, the reason is in there, and
  // "timed out" alone sends the caller looking in the wrong place.
  let stderr = "";
  child.stderr?.on("data", (chunk: Buffer) => {
    stderr = (stderr + chunk.toString()).slice(-4000);
  });

  let exited: { code: number | null; signal: string | null } | undefined;
  child.once("exit", (code, signal) => {
    exited = { code, signal };
  });

  // A spawn failure (horon-agent not installed, which is the most likely first-run
  // problem) emits "error" on the child. Node treats an unlistened "error" as
  // a fatal throw from deep inside child_process, so without this the caller
  // cannot catch it at all: their process dies with a raw ENOENT stack instead
  // of being told to install horon-agent.
  let spawnError: NodeJS.ErrnoException | undefined;
  child.once("error", (error: NodeJS.ErrnoException) => {
    spawnError = error;
    exited = { code: null, signal: null };
  });

  const shutdown = async (): Promise<void> => {
    // A spawn that never started has no daemon to stop and nothing to wait
    // for. Running the full shutdown here would spend its whole budget trying
    // to talk to a daemon that does not exist, and the instance home would be
    // left behind by the very error path that is supposed to clean it up.
    if (spawnError) {
      if (ephemeral) removeInstanceHome(horonAgentHome);
      return;
    }

    // Stop the daemon *before* the bridge, not after.
    //
    // The instance's daemon is a separate process that the bridge spawned, so
    // killing the bridge does not stop it: it keeps running, keeps writing
    // sessions and caches into the instance home, and outlives close(). It is
    // asked to stop over its own socket, and that socket goes away when the
    // daemon starts shutting down, so doing this after the bridge dies races
    // a window where `server stop` reports "no running server found" and the
    // daemon is simply leaked.
    await stopInstanceDaemon(binary, horonAgentHome, runtimeDir);

    if (exited === undefined) {
      child.kill("SIGTERM");
      // Give it a moment to unwind before insisting.
      await new Promise<void>((resolve) => {
        const timer = setTimeout(() => {
          child.kill("SIGKILL");
          resolve();
        }, 3000);
        timer.unref?.();
        child.once("exit", () => {
          clearTimeout(timer);
          resolve();
        });
      });
    }

    if (ephemeral) {
      // Even after the daemon is asked to stop, work it already started can
      // land after the first delete: the session-search indexer writes a
      // multi-megabyte file, and that write finishes seconds later, recreating
      // the directory behind a delete that had already succeeded. So deleting
      // once and seeing an empty result proves nothing. Require the home to
      // stay gone across a settle window, and keep trying for long enough to
      // outlast a slow flush.
      const deadline = Date.now() + (options.cleanupTimeoutMs ?? 30_000);
      // Delete, then require the home to stay gone across a settle window.
      // Recursion rather than a loop: the settle window only starts once a
      // delete has landed, and moving on early has to skip the rest of it.
      const removeUntilGone = async (): Promise<void> => {
        if (Date.now() >= deadline) return;
        removeInstanceHome(horonAgentHome);
        await sleep(250);
        if (fs.existsSync(horonAgentHome)) return removeUntilGone();
        await sleep(750);
        if (!fs.existsSync(horonAgentHome)) return;
        return removeUntilGone();
      };
      await removeUntilGone();
      // Out of time. A leaked temp directory is a much smaller problem than a
      // close() that never returns, but it should not be silent.
      if (fs.existsSync(horonAgentHome)) {
        process.emitWarning(
          `horon-agent instance home was still being written to and could not be removed: ${horonAgentHome}`,
        );
      }
    }
  };

  // A consumer who crashes, or simply forgets close(), would otherwise leave
  // the daemon running forever: a server embedding horon-agent would accumulate one
  // instance per restart, each holding a temp directory and a model
  // connection. Node runs "exit" handlers on a normal exit and after an
  // uncaught exception, which covers everything short of SIGKILL.
  const reapOnExit = () => {
    try {
      if (exited === undefined) child.kill("SIGTERM");
    } catch {
      // Already gone.
    }
    // The daemon calls setsid(), so it leads its own session and no signal
    // aimed at the bridge can reach it. Killing the bridge alone is exactly
    // the leak this handler exists to prevent.
    const pid = readDaemonPidSync(horonAgentHome, runtimeDir);
    if (pid !== undefined) {
      // SIGKILL, not SIGTERM: an exit handler cannot await, so there is no
      // chance to wait for a graceful shutdown, and a SIGTERM'd daemon would
      // still be flushing when this process is gone, recreating the very
      // directory being removed below.
      try {
        // Negative pid: the daemon leads a group, so its helpers go with it.
        process.kill(-pid, "SIGKILL");
      } catch {
        try {
          process.kill(pid, "SIGKILL");
        } catch {
          // Already gone.
        }
      }
    }

    // Remove the home too. Reaping the daemon alone still leaves a temp
    // directory per crash, which for a server that restarts is the same
    // unbounded growth in a different resource.
    if (ephemeral) {
      try {
        removeInstanceHome(horonAgentHome);
      } catch {
        // Best-effort: an exit handler must not throw.
      }
    }
  };
  process.once("exit", reapOnExit);

  const deadline = Date.now() + (options.startupTimeoutMs ?? 30_000);
  // Poll for the API socket. Recursion rather than a loop: every check is
  // sequenced behind the previous wait, and the failure paths below leave the
  // wait rather than continuing it.
  const awaitStartup = async (): Promise<LaunchedInstance | undefined> => {
    if (Date.now() >= deadline) return undefined;
    const failure = spawnError;
    if (failure) {
      process.removeListener("exit", reapOnExit);
      await shutdown();
      const binaryName = binary;
      const platformPackage = platformBinaryPackage();
      throw new HarnessError(
        "horon_agent_not_found",
        failure.code === "ENOENT"
          ? `could not run \`${binaryName}\`: horon-agent is not installed, or not on PATH. ` +
              (platformPackage
                ? `The bundled runtime package (${platformPackage}) is missing. Reinstall without ` +
                  "--omit=optional, install horon-agent from https://horon-agent.sh, or pass `binary` with its full path."
                : "Install horon-agent from https://horon-agent.sh, or pass `binary` with its full path.")
          : `could not run \`${binaryName}\`: ${failure.message}`,
      );
    }
    const ended = exited;
    if (ended) {
      process.removeListener("exit", reapOnExit);
      await shutdown();
      throw new HarnessError(
        "startup_failed",
        `horon-agent exited during startup (code ${ended.code}, signal ${ended.signal})` +
          (stderr ? `:\n${stderr.trim()}` : ""),
      );
    }
    if (fs.existsSync(socketPath)) {
      return {
        socketPath,
        horonAgentHome,
        process: child,
        shutdown: async () => {
          process.removeListener("exit", reapOnExit);
          await shutdown();
        },
      };
    }
    await sleep(50);
    return awaitStartup();
  };

  const started = await awaitStartup();
  if (started) return started;

  process.removeListener("exit", reapOnExit);
  await shutdown();
  throw new HarnessError(
    "startup_timeout",
    `horon-agent did not create its API socket at ${socketPath} within the startup timeout` +
      (stderr ? `:\n${stderr.trim()}` : ""),
  );
}
