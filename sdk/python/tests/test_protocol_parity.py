"""Cross-SDK protocol parity.

The Python protocol module is hand-written, so the realistic failure mode is the
Rust or TypeScript surface growing a variant that Python never learns about.
Reading the other sources directly fails the moment they drift, which is the only
check that catches an additive change nobody remembered to mirror.
"""

from __future__ import annotations

import pathlib
import re

import pytest

from horon_agent_sdk import (
    ADVERTISED_CAPABILITIES,
    API_VERSION_MAJOR,
    API_VERSION_MINOR,
    CAPABILITY_SURFACES,
    KNOWN_EVENT_KINDS,
    KNOWN_REQUEST_KINDS,
)

REPO_ROOT = pathlib.Path(__file__).resolve().parents[3]
TS_PROTOCOL = REPO_ROOT / "sdk/typescript/src/protocol.ts"
RUST_API_SRC = REPO_ROOT / "crates/harness-api/src"

requires_typescript = pytest.mark.skipif(not TS_PROTOCOL.exists(), reason="vendored build without the sdk/ tree")


def ts_tag_set(name: str) -> list[str]:
    source = TS_PROTOCOL.read_text(encoding="utf-8")
    match = re.search(rf"export const {name} = \[(.*?)\] as const;", source, re.DOTALL)
    assert match is not None, f"{name} not found in {TS_PROTOCOL}"
    return re.findall(r'"([a-z0-9_]+)"', match.group(1))


@requires_typescript
def test_request_tags_match_the_typescript_sdk() -> None:
    assert sorted(KNOWN_REQUEST_KINDS) == sorted(ts_tag_set("KNOWN_REQUEST_KINDS"))


@requires_typescript
def test_event_tags_match_the_typescript_sdk() -> None:
    assert sorted(KNOWN_EVENT_KINDS) == sorted(ts_tag_set("KNOWN_EVENT_KINDS"))


@requires_typescript
def test_protocol_versions_match_the_typescript_sdk() -> None:
    source = TS_PROTOCOL.read_text(encoding="utf-8")
    major = re.search(r"export const API_VERSION_MAJOR = (\d+);", source)
    minor = re.search(r"export const API_VERSION_MINOR = (\d+);", source)
    assert major is not None and minor is not None
    assert API_VERSION_MAJOR == int(major.group(1))
    assert API_VERSION_MINOR == int(minor.group(1))


@pytest.mark.skipif(not RUST_API_SRC.exists(), reason="vendored build without crates/")
def test_protocol_version_matches_the_rust_constant() -> None:
    source = (RUST_API_SRC / "lib.rs").read_text(encoding="utf-8")
    major = re.search(r"API_VERSION_MAJOR: u32 = (\d+);", source)
    minor = re.search(r"API_VERSION_MINOR: u32 = (\d+);", source)
    assert major is not None and minor is not None
    assert API_VERSION_MAJOR == int(major.group(1))
    assert API_VERSION_MINOR == int(minor.group(1))


@pytest.mark.skipif(not RUST_API_SRC.exists(), reason="vendored build without crates/")
def test_capability_surfaces_match_the_rust_crate() -> None:
    source = (RUST_API_SRC / "lib.rs").read_text(encoding="utf-8")
    assert "durable_runtime_v1" in source
    for capability, surfaces in CAPABILITY_SURFACES:
        assert f'"{capability}"' in source, f"{capability} is not defined in the Rust crate"
        for surface in surfaces:
            assert f'"{surface}"' in source, f"{surface} is not defined in the Rust crate"
    assert ADVERTISED_CAPABILITIES == tuple(capability for capability, _ in CAPABILITY_SURFACES)


@requires_typescript
def test_socket_environment_keys_match_the_typescript_resolver() -> None:
    source = (REPO_ROOT / "sdk/typescript/src/sockets.ts").read_text(encoding="utf-8")
    python_source = (REPO_ROOT / "sdk/python/src/horon_agent_sdk/sockets.py").read_text(encoding="utf-8")
    for key in (
        "HORON_AGENT_RUNTIME_DIR",
        "XDG_RUNTIME_DIR",
        "HORON_AGENT_API_SOCKET",
        "HORON_AGENT_SOCKET",
    ):
        assert key in source, f"{key} missing from the TypeScript resolver"
        assert key in python_source, f"{key} missing from the Python resolver"
    assert "horon-agent-api.sock" in source and "horon-agent-api.sock" in python_source
