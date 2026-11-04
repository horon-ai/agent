from __future__ import annotations

import os
import pathlib
import tempfile
from typing import Any

import pytest

from horon_agent_sdk import (
    HarnessError,
    HoronAgentClient,
    LaunchOptions,
    inherit_credentials,
    launch_instance,
    user_app_config_dir,
    user_horon_agent_home,
)
from horon_agent_sdk.launch import (
    INSTANCE_PREFIX,
    remove_instance_home_for_test,
    wait_for_daemon_pid_for_test,
)


def test_removing_an_instance_home_never_follows_links_to_real_credentials() -> None:
    """Cleanup must never follow a symlink out of the instance home.

    Current inheritance links files only, but cleanup must also be safe against
    a legacy, corrupted, or attacker-injected directory link. Following that link
    would delete the user's logins while "removing a temp directory", so this
    test builds the hostile shape and checks the target survives.
    """
    instance = tempfile.mkdtemp(prefix=INSTANCE_PREFIX)
    precious = tempfile.mkdtemp(prefix="horon-agent-precious-test-")
    pathlib.Path(precious, "auth.json").write_text('{"token":"keep me"}')

    os.makedirs(os.path.join(instance, "external"))
    os.symlink(precious, os.path.join(instance, "external", "linked-home"))
    os.symlink(os.path.join(precious, "auth.json"), os.path.join(instance, "auth.json"))
    pathlib.Path(instance, "own-state.json").write_text("{}")

    # Exercise the same routine ``close()`` uses on an ephemeral instance.
    remove_instance_home_for_test(instance)

    assert not os.path.exists(instance), "the instance home should be gone"
    assert os.path.exists(os.path.join(precious, "auth.json")), "the user's credentials must survive"
    assert pathlib.Path(precious, "auth.json").read_text() == '{"token":"keep me"}'

    for entry in os.listdir(precious):
        pathlib.Path(precious, entry).unlink()
    os.rmdir(precious)


def test_cleanup_refuses_arbitrary_directories_even_when_asked_directly() -> None:
    arbitrary = tempfile.mkdtemp(prefix="not-a-horon-agent-instance-")
    pathlib.Path(arbitrary, "keep").write_text("safe")

    remove_instance_home_for_test(arbitrary)

    assert pathlib.Path(arbitrary, "keep").read_text() == "safe"
    pathlib.Path(arbitrary, "keep").unlink()
    os.rmdir(arbitrary)


async def test_daemon_shutdown_lookup_waits_for_a_delayed_registry_entry() -> None:
    home = tempfile.mkdtemp(prefix="horon-agent-daemon-race-test-")
    runtime_dir = os.path.join(home, "run")
    os.makedirs(runtime_dir)

    import asyncio
    import json

    def publish() -> None:
        pathlib.Path(home, "servers.json").write_text(
            json.dumps({"instance": {"socket": os.path.join(runtime_dir, "horon-agent.sock"), "pid": 4242}})
        )

    asyncio.get_running_loop().call_later(0.1, publish)
    assert await wait_for_daemon_pid_for_test(home, runtime_dir, 1000) == 4242
    for entry in os.listdir(home):
        target = os.path.join(home, entry)
        if os.path.isdir(target):
            os.rmdir(target)
        else:
            os.unlink(target)
    os.rmdir(home)


def test_rotating_credentials_are_shared_so_token_refresh_stays_coherent(tmp_path: pathlib.Path) -> None:
    source = tmp_path / "user"
    instance = tmp_path / "instance"
    source.mkdir()
    (source / "auth.json").write_text('{"refresh":"v1"}')
    (source / "config.toml").write_text("[auth]\n")
    # Derived failure state must never be inherited: a stale rejection record
    # makes a fresh instance refuse to refresh credentials that work.
    (source / "auth-refresh-state.json").write_text('{"claude":{"rejected":1}}')

    inherited = inherit_credentials(str(source), str(instance))

    assert os.path.islink(instance / "auth.json"), "auth.json must be shared, since tokens rotate"
    assert not os.path.islink(instance / "config.toml"), "config.toml must be copied so the instance can edit it"
    assert not (instance / "auth-refresh-state.json").exists(), "derived auth failure state must not be inherited"
    assert "auth.json" in inherited

    # A rotation on either side must be visible to the other.
    (source / "auth.json").write_text('{"refresh":"v2"}')
    assert (instance / "auth.json").read_text() == '{"refresh":"v2"}'


def test_credential_inheritance_refuses_to_use_the_users_home_as_the_instance_home(tmp_path: pathlib.Path) -> None:
    home = tmp_path / "same-home"
    home.mkdir()
    (home / "auth.json").write_text('{"token":"must survive"}')

    with pytest.raises(HarnessError) as caught:
        inherit_credentials(str(home), str(home))
    assert caught.value.code == "invalid_instance_home"
    assert (home / "auth.json").read_text() == '{"token":"must survive"}'


def test_login_inheritance_creates_file_links_never_credential_directory_links(
    monkeypatch: pytest.MonkeyPatch, tmp_path: pathlib.Path
) -> None:
    home = tmp_path / "home"
    config = tmp_path / "config"
    monkeypatch.setenv("HOME", str(home))
    monkeypatch.setenv("XDG_CONFIG_HOME", str(config))
    user_config = pathlib.Path(user_app_config_dir())
    source = home / ".horon-agent"
    instance = tmp_path / "instance"
    (home / ".claude").mkdir(parents=True)
    user_config.mkdir(parents=True)
    source.mkdir(parents=True)
    (home / ".claude" / ".credentials.json").write_text("{}")
    (user_config / "n.env").write_text("TOKEN=test")
    (user_config / "usage.json").write_text("{}")
    legacy_target = tmp_path / "legacy-linked-config"
    legacy_target.mkdir()
    (legacy_target / "precious").write_text("untouched")
    instance.mkdir()
    os.symlink(legacy_target, instance / "config")

    inherited = inherit_credentials(str(source), str(instance))

    assert "config/horon-agent/n.env" in inherited
    assert "external/.claude/.credentials.json" in inherited
    assert os.path.islink(instance / "config" / "horon-agent" / "n.env")
    assert os.path.islink(instance / "external" / ".claude" / ".credentials.json")
    assert not os.path.islink(instance / "config" / "horon-agent")
    assert not os.path.islink(instance / "external" / ".claude")
    assert not (instance / "config" / "horon-agent" / "usage.json").exists()
    assert (legacy_target / "precious").read_text() == "untouched", (
        "migrating an old directory link must not modify its target"
    )


def test_the_users_horon_agent_home_is_resolved_independently_of_an_instance() -> None:
    assert len(user_horon_agent_home()) > 0
    assert os.path.isabs(user_horon_agent_home())


async def test_a_missing_horon_agent_binary_is_catchable_and_leaks_nothing() -> None:
    root = tempfile.gettempdir()
    before = [name for name in os.listdir(root) if name.startswith(INSTANCE_PREFIX)]

    with pytest.raises(HarnessError) as caught:
        await HoronAgentClient.launch(
            LaunchOptions(binary="horon-agent-definitely-not-installed", startup_timeout_ms=5000)
        )
    assert caught.value.code == "horon_agent_not_found"
    assert "not installed, or not on PATH" in str(caught.value)

    after = [name for name in os.listdir(root) if name.startswith(INSTANCE_PREFIX)]
    assert after == before, "a failed launch must not leave an instance home behind"


@pytest.mark.skipif(os.name == "nt", reason="the capture script is a POSIX shell script")
@pytest.mark.parametrize(
    ("env_name", "env_value", "option_name", "option_value", "expected"),
    [
        ("HORON_AGENT_SWARM_MODEL", "unwanted-model", "swarm_model", "inherit", "inherit"),
        ("HORON_AGENT_WAKE_MODE", "internal", "wake_mode", "external", "external"),
    ],
)
async def test_explicit_options_override_the_generic_environment(
    tmp_path: pathlib.Path,
    env_name: str,
    env_value: str,
    option_name: str,
    option_value: str,
    expected: str,
) -> None:
    binary = tmp_path / "capture-env"
    captured = tmp_path / "captured.txt"
    binary.write_text(f'#!/bin/sh\nprintf \'%s\' "${env_name}" > "$CAPTURE_PATH"\nexit 1\n')
    binary.chmod(0o700)

    with pytest.raises(HarnessError):
        await launch_instance(
            LaunchOptions(
                binary=str(binary),
                horon_agent_home=str(tmp_path / "instance"),
                inherit_logins=False,
                startup_timeout_ms=2000,
                env={"CAPTURE_PATH": str(captured), env_name: env_value},
                **{option_name: option_value},
            )
        )
    assert captured.read_text() == expected


@pytest.mark.skipif(os.name == "nt", reason="the capture script is a POSIX shell script")
async def test_private_instances_opt_out_of_auto_update_and_pass_the_bridge_arguments(
    tmp_path: pathlib.Path,
) -> None:
    binary = tmp_path / "capture-env"
    captured = tmp_path / "captured.txt"
    binary.write_text(
        '#!/bin/sh\nprintf \'%s|%s\' "${HORON_AGENT_NO_AUTO_UPDATE-unset}" "$*" > "$CAPTURE_PATH"\nexit 1\n'
    )
    binary.chmod(0o700)
    instance_home = tmp_path / "instance"

    with pytest.raises(HarnessError):
        await launch_instance(
            LaunchOptions(
                binary=str(binary),
                horon_agent_home=str(instance_home),
                inherit_logins=False,
                startup_timeout_ms=2000,
                env={"CAPTURE_PATH": str(captured)},
            )
        )

    socket_path = instance_home / "run" / "horon-agent-api.sock"
    assert captured.read_text() == f"1|--no-update api-bridge --api-socket {socket_path}"


async def test_launch_reports_startup_failure_with_the_runtimes_stderr(tmp_path: pathlib.Path) -> None:
    binary = tmp_path / "failing"
    binary.write_text('#!/bin/sh\necho "provider credentials are missing" >&2\nexit 3\n')
    binary.chmod(0o700)

    with pytest.raises(HarnessError) as caught:
        await launch_instance(
            LaunchOptions(
                binary=str(binary),
                horon_agent_home=str(tmp_path / "instance"),
                inherit_logins=False,
                startup_timeout_ms=2000,
            )
        )
    assert caught.value.code == "startup_failed"
    assert "provider credentials are missing" in caught.value.message


def test_instance_home_property_is_none_before_launch() -> None:
    client = HoronAgentClient(transport=_NoopTransport())
    assert client.instance_home is None


class _NoopTransport:
    on_drain = None

    def write(self, data: str) -> bool | None:
        return None

    def on_data(self, listener: Any) -> None:
        pass

    def on_close(self, listener: Any) -> None:
        pass

    def close(self) -> None:
        pass
