"""Launching a private horon-agent instance.

``HoronAgentClient.connect()`` attaches to whatever horon-agent is already
running on the machine, which is right for a tool that automates *your*
horon-agent (an editor plugin, a dashboard) and wrong for everything else. An
application embedding horon-agent as an agent engine wants its own instance: its
own sessions, its own state, and no way to disturb the user's live work by
accident.

``launch()`` gives it one. It starts a private daemon and bridge under a
dedicated ``HORON_HOME`` and runtime directory, and shuts them down on
``close()``.
"""

from __future__ import annotations

import asyncio
import atexit
import contextlib
import json
import os
import shutil
import signal
import subprocess
import sys
import tempfile
import time
import warnings
from collections.abc import Awaitable, Callable
from dataclasses import dataclass, field
from pathlib import Path
from typing import Literal

from .binary import bundled_horon_agent_binary
from .errors import HarnessError

WakeMode = Literal["internal", "external"]

INSTANCE_PREFIX = "horon-agent-sdk-instance-"

#: Files inherited from the user's horon-agent home when logins are inherited.
#:
#: Deliberately *not* included: ``auth-refresh-state.json`` and
#: ``auth-validation.json``, which are derived records of past auth failures.
#: Copying them imports another horon-agent's bad day, and a stale
#: ``rejected_refresh_fingerprint`` makes a fresh instance refuse to even attempt
#: a refresh with credentials that work.
CREDENTIAL_FILES = (
    "auth.json",
    "openai-auth.json",
    "antigravity_oauth.json",
    "gemini_oauth.json",
    "config.toml",
)

#: Credentials that must be *shared* with the user's home, not copied.
#:
#: OAuth refresh tokens rotate: redeeming one issues a new one and invalidates
#: the old. Two homes holding copies of the same token therefore fight, and
#: whichever refreshes second is logged out. A copy also goes stale on its own,
#: since the access token it holds expires in hours while an instance may run
#: for days. Sharing the file keeps one rotation record for one set of
#: credentials, which is what the tokens themselves already assume.
SHARED_FILES = frozenset(name for name in CREDENTIAL_FILES if name != "config.toml")

#: horon-agent's own config directory, relative to the platform config root.
#:
#: ``app_config_dir()`` is where provider env files live (``anthropic.env``,
#: ``n.env`` for the horon-agent subscription), and ``HORON_HOME``
#: redirects it to ``$HORON_HOME/config/horon-agent``.
APP_CONFIG_DIRNAME = "horon-agent"

#: Where horon-agent looks for *other* tools' credentials, relative to ``$HOME``.
#:
#: horon-agent can log in by reusing an existing CLI's OAuth store, so a large
#: share of real users have no usable ``~/.horon/auth.json`` at all: the
#: working credentials live in files under ``~/.claude/`` or
#: ``~/.config/github-copilot/``. Under ``HORON_HOME`` these lookups are
#: sandboxed to ``$HORON_HOME/external/``, so an instance that inherits
#: only ``auth.json`` silently has no credentials and fails on the first turn.
#: Linking the recognized credential files makes inheritance mean what it says
#: without exposing either directory wholesale.
EXTERNAL_CREDENTIAL_FILES = (
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
)


@dataclass
class LaunchOptions:
    """Options for starting a private instance."""

    #: Directory holding the instance's state (sessions, logs, credentials).
    #:
    #: Defaults to a fresh temporary directory that is removed on ``close()``.
    #: Pass a stable path to keep sessions across runs.
    horon_agent_home: str | None = None
    #: Working directory for sessions created in this instance.
    working_dir: str | None = None
    #: Share the user's provider logins with the instance. Defaults to ``True``.
    inherit_logins: bool = True
    #: Path to the horon-agent binary. Defaults to the bundled runtime, then PATH.
    binary: str | None = None
    #: Extra environment variables for the instance.
    env: dict[str, str] = field(default_factory=dict)
    #: Default model for spawned swarm workers, unless overridden per spawn.
    swarm_model: str | None = None
    #: Who executes autonomous wake requests.
    wake_mode: WakeMode | None = None
    #: Milliseconds to wait for the socket to appear. Defaults to 30000.
    startup_timeout_ms: int = 30_000
    #: Forward the instance's stderr to this process. Defaults to False.
    inherit_stderr: bool = False
    #: Milliseconds ``close()`` will spend removing an ephemeral instance home.
    cleanup_timeout_ms: int = 30_000


@dataclass
class LaunchedInstance:
    """A running private horon-agent instance."""

    #: API socket path to connect to.
    socket_path: str
    #: The instance's ``HORON_HOME``.
    horon_agent_home: str
    #: The bridge process.
    process: asyncio.subprocess.Process
    #: Stop the instance and clean up anything it created.
    shutdown: Callable[[], Awaitable[None]]


async def _sleep(ms: float) -> None:
    await asyncio.sleep(ms / 1000)


def _user_home_dir() -> str:
    """The user's home directory as the Rust side resolves it: ``$HOME`` outside Windows."""
    if sys.platform != "win32" and os.environ.get("HOME"):
        return os.environ["HOME"]
    return str(Path.home())


def user_horon_agent_home() -> str:
    """Resolve the user's real horon-agent home, ignoring any instance override."""
    return os.environ.get("HORON_HOME") or os.path.join(_user_home_dir(), ".horon")


def user_app_config_dir() -> str:
    """The user's horon-agent config directory, mirroring ``storage::app_config_dir``."""
    if sys.platform == "darwin":
        return os.path.join(_user_home_dir(), "Library", "Application Support", APP_CONFIG_DIRNAME)
    if sys.platform == "win32":
        app_data = os.environ.get("APPDATA") or os.path.join(_user_home_dir(), "AppData", "Roaming")
        return os.path.join(app_data, APP_CONFIG_DIRNAME)
    xdg = os.environ.get("XDG_CONFIG_HOME") or os.path.join(_user_home_dir(), ".config")
    return os.path.join(xdg, APP_CONFIG_DIRNAME)


def _ensure_instance_directory(root: str, relative: str) -> str:
    """Ensure a directory below an instance root contains no symlink components."""
    if os.path.isabs(relative) or ".." in relative.replace("\\", "/").split("/"):
        raise HarnessError("invalid_instance_home", f"unsafe instance path: {relative}")

    current = root
    for part in (segment for segment in relative.replace("\\", "/").split("/") if segment):
        current = os.path.join(current, part)
        try:
            os.lstat(current)
        except FileNotFoundError:
            os.makedirs(current, mode=0o700, exist_ok=True)
            continue
        if os.path.islink(current):
            # Migrate homes made by SDK versions that linked whole credential
            # directories. unlink removes the link itself, never its target.
            os.unlink(current)
            os.makedirs(current, mode=0o700, exist_ok=True)
        elif not os.path.isdir(current):
            raise HarnessError("invalid_instance_home", f"instance credential path is not a directory: {current}")
    return current


def _link_credential_file(source: str, root: str, relative: str) -> bool:
    """Replace an instance-relative path with a link to one credential file."""
    try:
        if not os.path.isfile(source):
            return False
    except OSError:
        return False

    parent = _ensure_instance_directory(root, os.path.dirname(relative))
    destination = os.path.join(parent, os.path.basename(relative))
    with contextlib.suppress(FileNotFoundError):
        os.unlink(destination)
    os.symlink(source, destination)
    return True


def inherit_credentials(from_home: str, to_home: str) -> list[str]:
    """Give a launched instance the user's provider logins.

    Credential files are symlinked so token rotation stays coherent (see
    ``SHARED_FILES``); everything else is copied, so the instance can edit its
    own configuration without touching the user's. Copies are owner-only.

    Returns the names actually inherited, so a caller can tell "inherited
    nothing" from "inherited something".
    """
    os.makedirs(to_home, mode=0o700, exist_ok=True)
    if os.path.islink(to_home) or not os.path.isdir(to_home):
        raise HarnessError(
            "invalid_instance_home",
            f"instance home must be a real directory, not a link or file: {to_home}",
        )
    if os.path.exists(from_home) and os.path.realpath(from_home) == os.path.realpath(to_home):
        raise HarnessError("invalid_instance_home", "instance home must be different from the user's horon-agent home")

    inherited: list[str] = []
    for name in CREDENTIAL_FILES:
        source = os.path.join(from_home, name)
        if not os.path.exists(source):
            continue
        destination = os.path.join(to_home, name)
        if name in SHARED_FILES:
            # A reused ``horon_agent_home`` already has these links, and symlink
            # raises FileExistsError rather than replacing. Relinking also
            # repoints a stale link from an older run, so replace rather than skip.
            _link_credential_file(source, to_home, name)
        else:
            shutil.copyfile(source, destination)
            os.chmod(destination, 0o600)
        inherited.append(name)

    # horon-agent's provider env files live in its platform config directory, which
    # ``HORON_HOME`` moves to ``$HORON_HOME/config/horon-agent``. Link only the
    # env files: caches and usage data are not credentials and must stay
    # instance-private. Most importantly, never link the directory itself. A
    # buggy recursive cleanup can descend through a directory link and delete
    # the user's files; a file link can only ever be unlinked at the
    # instance-side path.
    user_config = user_app_config_dir()
    try:
        for entry in os.scandir(user_config):
            if not entry.name.endswith(".env") or not (entry.is_file() or entry.is_symlink()):
                continue
            relative = f"config/{APP_CONFIG_DIRNAME}/{entry.name}"
            if _link_credential_file(os.path.join(user_config, entry.name), to_home, relative):
                inherited.append(relative)
    except OSError:
        # No app config directory is a normal fresh-install state.
        pass

    # Other CLIs' credential stores, which horon-agent reads directly and which
    # ``HORON_HOME`` redirects to ``$HORON_HOME/external/``. Share only
    # the exact credential files. Linking whole directories would also expose
    # transcripts, configuration, and anything those tools add in the future.
    for relative in EXTERNAL_CREDENTIAL_FILES:
        source = os.path.join(_user_home_dir(), relative)
        instance_relative = os.path.join("external", relative)
        if _link_credential_file(source, to_home, instance_relative):
            inherited.append(f"external/{relative}")

    # OpenClaw's current store is per-agent. Enumerate one level of agent IDs,
    # then link only the two credential filenames its loader recognizes.
    openclaw_agents = os.path.join(_user_home_dir(), ".openclaw", "agents")
    try:
        for agent in os.scandir(openclaw_agents):
            if not agent.is_dir():
                continue
            for name in ("auth-profiles.json", "auth.json"):
                relative = os.path.join(".openclaw", "agents", agent.name, "agent", name)
                source = os.path.join(_user_home_dir(), relative)
                if _link_credential_file(source, to_home, os.path.join("external", relative)):
                    inherited.append(f"external/{relative}")
    except OSError:
        # OpenClaw is optional.
        pass
    return inherited


def read_daemon_pid(horon_agent_home: str, runtime_dir: str) -> int | None:
    """Pid of the daemon serving an instance, read from its own server registry.

    Synchronous on purpose: the only caller is an exit handler, which cannot
    await. horon-agent records every server in ``$HORON_HOME/servers.json``
    keyed by socket path, and an instance's registry lists only that instance's
    daemon, so this can never resolve to the server the user is running
    themselves.
    """
    try:
        raw = Path(horon_agent_home, "servers.json").read_text(encoding="utf-8")
    except OSError:
        return None
    try:
        registry = json.loads(raw)
    except ValueError:
        return None
    if not isinstance(registry, dict):
        return None
    socket_path = os.path.join(runtime_dir, "horon-agent.sock")
    for entry in registry.values():
        if not isinstance(entry, dict):
            continue
        pid = entry.get("pid")
        if entry.get("socket") == socket_path and isinstance(pid, int) and not isinstance(pid, bool) and pid > 1:
            return pid
    return None


async def _wait_for_daemon_pid(horon_agent_home: str, runtime_dir: str, timeout_ms: float = 2000) -> int | None:
    """Wait briefly for a newly started daemon to publish its registry entry."""
    deadline = _monotonic_ms() + timeout_ms
    while True:
        pid = read_daemon_pid(horon_agent_home, runtime_dir)
        if pid is not None:
            return pid
        await _sleep(50)
        if _monotonic_ms() >= deadline:
            return None


async def wait_for_daemon_pid_for_test(
    horon_agent_home: str,
    runtime_dir: str,
    timeout_ms: float | None = None,
) -> int | None:
    """Test seam for :func:`_wait_for_daemon_pid`."""
    return await _wait_for_daemon_pid(horon_agent_home, runtime_dir, 2000 if timeout_ms is None else timeout_ms)


def _monotonic_ms() -> float:
    return time.monotonic() * 1000


def process_exists(pid: int) -> bool:
    """True while ``pid`` is still a live process."""
    try:
        # Signal 0 tests for existence without touching the process.
        os.kill(pid, 0)
    except OSError:
        return False
    return True


def _signal_process(pid: int, sig: int) -> None:
    try:
        # Negative pid: the daemon leads its own group after setsid(), and its
        # helper children have to go too or they keep writing into the home.
        os.kill(-pid, sig)
    except OSError:
        try:
            os.kill(pid, sig)
        except OSError:
            # Already gone.
            pass


async def _wait_for_process_exit(pid: int, deadline: float) -> bool:
    """True once ``pid`` is gone, false when ``deadline`` passes first."""
    while True:
        if _monotonic_ms() >= deadline:
            return False
        if not process_exists(pid):
            return True
        await _sleep(50)


async def _stop_instance_daemon(horon_agent_home: str, runtime_dir: str) -> None:
    """Stop an instance's daemon and wait for it to actually be gone.

    The pid comes from the instance's own ``servers.json`` rather than from
    ``horon-agent server stop``: spawning a second horon-agent binary to read a
    pid out of a JSON file costs seconds of process startup, and ``close()``
    blocking that long makes the SDK feel broken. The registry is scoped to this
    instance's socket path, so this can never signal the server the user is
    running.
    """
    # The API socket can become connectable just before the daemon writes its
    # servers.json entry. close() may therefore run during this small startup
    # window, so do not silently give up after a single registry read.
    pid = await _wait_for_daemon_pid(horon_agent_home, runtime_dir)
    if pid is None:
        return

    if sys.platform == "win32":
        subprocess.run(
            ["taskkill", "/T", "/F", "/PID", str(pid)],
            check=False,
            capture_output=True,
        )
        await _wait_for_process_exit(pid, _monotonic_ms() + 5000)
        return

    _signal_process(pid, signal.SIGTERM)

    # Wait for a clean exit, but not for long. The daemon's own shutdown does
    # background work (flushing a multi-megabyte search index, among other
    # things) that can take fifteen seconds. Nothing there is worth preserving:
    # the instance is ephemeral and about to be deleted, so a short graceful
    # window followed by SIGKILL is both faster and more predictable.
    if await _wait_for_process_exit(pid, _monotonic_ms() + 2000):
        return

    _signal_process(pid, signal.SIGKILL)

    # SIGKILL is not instant: the kernel still has to tear the process down, and
    # returning before it is gone races the delete that follows.
    await _wait_for_process_exit(pid, _monotonic_ms() + 5000)


def _ephemeral_roots() -> set[str]:
    """Directories this module may create ephemeral instance homes in.

    ``tempfile.gettempdir()`` is the default, but on macOS it is
    ``/var/folders/<hash>/T``, which is long enough that the instance's API
    socket path exceeds ``sun_path`` and the daemon cannot bind it. ``/tmp``
    (``/private/tmp`` after resolution) is the short spelling of the same idea.
    """
    roots = {os.path.realpath(tempfile.gettempdir())}
    for candidate in ("/tmp", "/private/tmp", "/var/tmp"):
        if os.path.isdir(candidate):
            roots.add(os.path.realpath(candidate))
    return roots


def _ephemeral_home() -> str:
    """Create a temporary instance home whose socket path fits the platform limit.

    Unix domain socket paths are capped (104 bytes on macOS, 108 on Linux). The
    home name, ``run`` directory, and socket filename are fixed, so the only
    variable is the root. Pick the shortest writable temp root that leaves room,
    and fall back to the platform default if none does.
    """
    socket_name = "horon-agent-api.sock"
    # mkdtemp adds an eight-character suffix after the prefix.
    overhead = len("/") + len(INSTANCE_PREFIX) + 8 + len("/run/") + len(socket_name) + 1
    candidates = [os.path.realpath(tempfile.gettempdir())]
    for candidate in ("/tmp", "/private/tmp", "/var/tmp"):
        if os.path.isdir(candidate):
            resolved = os.path.realpath(candidate)
            if resolved not in candidates:
                candidates.append(resolved)
    candidates.sort(key=len)
    for root in candidates:
        if not os.access(root, os.W_OK):
            continue
        if len(root) + overhead <= 100:
            return tempfile.mkdtemp(prefix=INSTANCE_PREFIX, dir=root)
    return tempfile.mkdtemp(prefix=INSTANCE_PREFIX)


def remove_instance_home(home: str) -> None:
    """Delete an ephemeral instance home, refusing to follow symlinks out of it.

    The home contains symlinks that point at the user's real credential files.
    Production inheritance never links directories, but cleanup still treats an
    injected or legacy directory link as hostile: links are unlinked and never
    descended into, and anything unexpected is left in place rather than
    force-removed.
    """
    # Only homes created by this module are eligible for recursive cleanup.
    # Persistent caller-supplied homes are never passed here, but keep the guard
    # local to the destructive primitive so a future call site cannot turn a
    # lifecycle bug into deletion of an arbitrary directory.
    resolved_home = os.path.realpath(home)
    if os.path.dirname(resolved_home) not in _ephemeral_roots() or not os.path.basename(resolved_home).startswith(
        INSTANCE_PREFIX
    ):
        return

    try:
        _remove_tree(home)
    except OSError:
        # A stray file is a leaked temp directory; deleting the wrong thing is
        # unrecoverable. Prefer the leak.
        pass


def remove_instance_home_for_test(home: str) -> None:
    """Test seam for :func:`remove_instance_home`."""
    remove_instance_home(home)


def _remove_tree(target: str) -> None:
    """Recursively unlink a tree, never descending through a symlink."""
    try:
        os.lstat(target)
    except FileNotFoundError:
        return

    # ``lstat``, not ``stat``: a symlink must be reported as a link so it is
    # unlinked rather than descended into.
    if os.path.islink(target):
        os.unlink(target)
        return
    if os.path.isdir(target):
        for entry in os.listdir(target):
            _remove_tree(os.path.join(target, entry))
        os.rmdir(target)
        return
    os.unlink(target)


async def launch_instance(options: LaunchOptions | None = None) -> LaunchedInstance:
    """Start a private horon-agent instance and return once its API socket accepts.

    Private instances never auto-update: they run the binary that launched them.
    """
    options = options or LaunchOptions()
    binary = options.binary or bundled_horon_agent_binary() or "horon-agent"
    ephemeral = options.horon_agent_home is None
    horon_agent_home = options.horon_agent_home or _ephemeral_home()
    os.makedirs(horon_agent_home, mode=0o700, exist_ok=True)

    # The runtime directory holds the sockets. Keeping it inside the instance
    # home is what makes the instance private: the daemon binds its socket there
    # rather than in the shared $XDG_RUNTIME_DIR, so a launched instance and the
    # user's own horon-agent cannot collide or find each other.
    runtime_dir = os.path.join(horon_agent_home, "run")
    os.makedirs(runtime_dir, mode=0o700, exist_ok=True)
    socket_path = os.path.join(runtime_dir, "horon-agent-api.sock")

    # A reused ``horon_agent_home`` still holds the previous run's socket files.
    # The startup loop waits for the API socket to *appear*, so a leftover one
    # makes launch() return immediately against a socket nothing is listening
    # on, and the first request fails with ECONNREFUSED. Clearing them is safe:
    # a live instance on this home would mean two daemons sharing one state
    # directory, which is already unsupported.
    for stale in (
        "horon-agent-api.sock",
        "horon-agent.sock",
        "horon-agent-debug.sock",
        "horon-agent.sock.hash",
    ):
        with contextlib.suppress(FileNotFoundError):
            os.unlink(os.path.join(runtime_dir, stale))

    if options.inherit_logins:
        inherit_credentials(user_horon_agent_home(), horon_agent_home)

    env = dict(os.environ)
    env.update(
        {
            "HORON_HOME": horon_agent_home,
            "HORON_AGENT_RUNTIME_DIR": runtime_dir,
            "HORON_AGENT_API_SOCKET": socket_path,
            "HORON_AGENT_SOCKET": os.path.join(runtime_dir, "horon-agent.sock"),
            # An empty private home always looks out of date, so the release
            # updater would replace and restart the runtime mid-session: the
            # client's turn fails and the restarted server is orphaned. The
            # variable is inherited by every horon-agent process the instance
            # spawns (argv is not), so it keeps the release auto-installer off in
            # those too. The shared runtime still updates.
            "HORON_AGENT_NO_AUTO_UPDATE": "1",
        }
    )
    env.update(options.env)
    if options.swarm_model is not None:
        env["HORON_AGENT_SWARM_MODEL"] = options.swarm_model
    if options.wake_mode is not None:
        env["HORON_AGENT_WAKE_MODE"] = options.wake_mode

    # Keep the last of stderr: when startup fails, the reason is in there, and
    # "timed out" alone sends the caller looking in the wrong place.
    stderr_tail: list[str] = []

    def keep_stderr(text: str) -> None:
        current = "".join(stderr_tail) + text
        stderr_tail[:] = [current[-4000:]]

    try:
        process = await asyncio.create_subprocess_exec(
            binary,
            # ``--no-update`` turns off the bridge's own update check, which also
            # runs for source builds.
            "--no-update",
            "api-bridge",
            "--api-socket",
            socket_path,
            cwd=options.working_dir or os.getcwd(),
            env=env,
            stdin=asyncio.subprocess.DEVNULL,
            stdout=asyncio.subprocess.DEVNULL,
            stderr=None if options.inherit_stderr else asyncio.subprocess.PIPE,
            start_new_session=False,
        )
    except (FileNotFoundError, NotADirectoryError) as error:
        if ephemeral:
            remove_instance_home(horon_agent_home)
        raise HarnessError(
            "horon_agent_not_found",
            f"could not run `{binary}`: horon-agent is not installed, or not on PATH. "
            "Reinstall a platform wheel of horon-agent-sdk (which carries the runtime), "
            "install horon-agent from https://horon-agent.sh, or pass `binary` with its full path.",
        ) from error
    except OSError as error:
        if ephemeral:
            remove_instance_home(horon_agent_home)
        raise HarnessError("horon_agent_not_found", f"could not run `{binary}`: {error}") from error

    stderr_task: asyncio.Task[None] | None = None
    if process.stderr is not None:

        async def pump_stderr() -> None:
            while True:
                chunk = await process.stderr.read(4096)  # type: ignore[union-attr]
                if not chunk:
                    return
                keep_stderr(chunk.decode("utf-8", "replace"))

        stderr_task = asyncio.get_running_loop().create_task(pump_stderr())

    async def collect_stderr() -> str:
        if stderr_task is not None:
            with contextlib.suppress(BaseException):
                await asyncio.wait_for(asyncio.shield(stderr_task), timeout=1.0)
        return "".join(stderr_tail).strip()

    stopped = False
    reaping = False

    async def shutdown() -> None:
        nonlocal stopped
        if stopped:
            return
        stopped = True
        _unregister_exit(reap_on_exit)
        # Stop the daemon *before* the bridge, not after.
        #
        # The instance's daemon is a separate process that the bridge spawned, so
        # killing the bridge does not stop it: it keeps running, keeps writing
        # sessions and caches into the instance home, and outlives close(). It is
        # asked to stop over its own socket, and that socket goes away when the
        # daemon starts shutting down, so doing this after the bridge dies races
        # a window where `server stop` reports "no running server found" and the
        # daemon is simply leaked.
        await _stop_instance_daemon(horon_agent_home, runtime_dir)

        if process.returncode is None:
            with contextlib.suppress(ProcessLookupError):
                process.terminate()
            try:
                await asyncio.wait_for(process.wait(), timeout=3)
            except asyncio.TimeoutError:
                with contextlib.suppress(ProcessLookupError):
                    process.kill()
                with contextlib.suppress(BaseException):
                    await process.wait()

        if stderr_task is not None:
            stderr_task.cancel()
            with contextlib.suppress(BaseException):
                await stderr_task

        if ephemeral:
            # Even after the daemon is asked to stop, work it already started can
            # land after the first delete: the session-search indexer writes a
            # multi-megabyte file, and that write finishes seconds later,
            # recreating the directory behind a delete that had already
            # succeeded. So deleting once and seeing an empty result proves
            # nothing. Require the home to stay gone across a settle window, and
            # keep trying for long enough to outlast a slow flush.
            deadline = _monotonic_ms() + options.cleanup_timeout_ms
            while _monotonic_ms() < deadline:
                remove_instance_home(horon_agent_home)
                await _sleep(250)
                if os.path.exists(horon_agent_home):
                    continue
                await _sleep(750)
                if not os.path.exists(horon_agent_home):
                    break
            # Out of time. A leaked temp directory is a much smaller problem than
            # a close() that never returns, but it should not be silent.
            if os.path.exists(horon_agent_home):
                warnings.warn(
                    f"horon-agent instance home was still being written to and could not be removed: "
                    f"{horon_agent_home}",
                    ResourceWarning,
                    stacklevel=2,
                )

    def reap_on_exit() -> None:
        nonlocal reaping
        if reaping:
            return
        reaping = True
        try:
            if process.returncode is None:
                process.terminate()
        except OSError:
            pass
        # The daemon calls setsid(), so it leads its own session and no signal
        # aimed at the bridge can reach it. Killing the bridge alone is exactly
        # the leak this handler exists to prevent.
        pid = read_daemon_pid(horon_agent_home, runtime_dir)
        if pid is not None:
            # SIGKILL, not SIGTERM: an exit handler cannot await, so there is no
            # chance to wait for a graceful shutdown, and a SIGTERM'd daemon
            # would still be flushing when this process is gone, recreating the
            # very directory being removed below.
            if sys.platform == "win32":
                subprocess.run(["taskkill", "/T", "/F", "/PID", str(pid)], check=False, capture_output=True)
            else:
                _signal_process(pid, signal.SIGKILL)
        # Remove the home too. Reaping the daemon alone still leaves a temp
        # directory per crash, which for a server that restarts is the same
        # unbounded growth in a different resource.
        if ephemeral:
            with contextlib.suppress(OSError):
                remove_instance_home(horon_agent_home)

    _register_exit(reap_on_exit)

    # A consumer who crashes, or simply forgets close(), would otherwise leave
    # the daemon running forever: a server embedding horon-agent would
    # accumulate one instance per restart, each holding a temp directory and a
    # model connection. An exit handler runs on a normal exit and after an
    # uncaught exception, which covers everything short of SIGKILL.
    deadline = _monotonic_ms() + options.startup_timeout_ms
    while True:
        if _monotonic_ms() >= deadline:
            _unregister_exit(reap_on_exit)
            await shutdown()
            detail = await collect_stderr()
            raise HarnessError(
                "startup_timeout",
                f"horon-agent did not create its API socket at {socket_path} within the startup timeout"
                + (f":\n{detail}" if detail else ""),
            )
        if process.returncode is not None:
            _unregister_exit(reap_on_exit)
            await shutdown()
            detail = await collect_stderr()
            raise HarnessError(
                "startup_failed",
                f"horon-agent exited during startup (code {process.returncode})" + (f":\n{detail}" if detail else ""),
            )
        if os.path.exists(socket_path):
            return LaunchedInstance(
                socket_path=socket_path,
                horon_agent_home=horon_agent_home,
                process=process,
                shutdown=shutdown,
            )
        await _sleep(50)


_exit_hooks: set[Callable[[], None]] = set()


def _register_exit(hook: Callable[[], None]) -> None:
    _exit_hooks.add(hook)
    atexit.register(hook)


def _unregister_exit(hook: Callable[[], None]) -> None:
    if hook in _exit_hooks:
        _exit_hooks.discard(hook)
        atexit.unregister(hook)
