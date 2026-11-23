"""Package-control contract tests for the Python SDK."""

from __future__ import annotations

from collections.abc import Callable
from typing import Any

import pytest

from horon_agent_sdk import (
    ConnectOptions,
    HarnessError,
    HoronAgentClient,
    PackageControlReceipt,
    PackageControlRequest,
)
from mock_harness import start_mock_harness


async def test_package_control_sends_request_and_returns_typed_receipt() -> None:
    seen: list[dict[str, Any]] = []

    def on_request(request: dict[str, Any], send: Callable[[dict[str, Any]], None]) -> None:
        seen.append(request)
        if request["req"] == "package_control":
            receipt: PackageControlReceipt = {
                "operation_id": request["operation_id"],
                "generation": 4,
                "replayed": False,
                "changed": ["hello"],
                "skipped": [{"name": "broken", "reason": "missing manifest"}],
                "catalog": [
                    {
                        "kind": "plugin",
                        "id": "hello",
                        "version": "0.1.0",
                        "name": "Hello",
                        "scope": {"kind": "project", "root": "/tmp/ws"},
                        "state": "installed",
                        "generation": 4,
                    }
                ],
            }
            send({"v": 1, "reply_to": request["id"], "ev": "package_control", "receipt": receipt})

    server = await start_mock_harness(capabilities=["package_control_v1"], on_request=on_request)
    client = await HoronAgentClient.connect(ConnectOptions(socket_path=server.socket_path))
    try:
        request: PackageControlRequest = {
            "operation_id": "op-pkg-1",
            "operation": {
                "kind": "install",
                "directory": "/tmp/pkg",
                "scope": {"kind": "project", "root": "/tmp/ws"},
                "replace": True,
            },
        }
        receipt = await client.package_control(request)
        assert receipt["operation_id"] == "op-pkg-1"
        assert receipt["generation"] == 4
        assert receipt["changed"] == ["hello"]
        assert receipt["skipped"] == [{"name": "broken", "reason": "missing manifest"}]
        assert receipt["catalog"][0]["state"] == "installed"
        assert receipt["catalog"][0]["scope"] == {"kind": "project", "root": "/tmp/ws"}
        assert seen == [
            {
                "v": 1,
                "id": seen[0]["id"],
                "req": "package_control",
                "operation_id": "op-pkg-1",
                "operation": {
                    "kind": "install",
                    "directory": "/tmp/pkg",
                    "scope": {"kind": "project", "root": "/tmp/ws"},
                    "replace": True,
                },
            }
        ]
    finally:
        await client.close()
        await server.close()


async def test_package_control_refuses_without_capability_before_writing() -> None:
    seen: list[dict[str, Any]] = []

    def on_request(request: dict[str, Any], send: Callable[[dict[str, Any]], None]) -> None:
        seen.append(request)
        send({"v": 1, "reply_to": request["id"], "ev": "ok"})

    server = await start_mock_harness(capabilities=["sessions"], on_request=on_request)
    client = await HoronAgentClient.connect(ConnectOptions(socket_path=server.socket_path))
    try:
        with pytest.raises(HarnessError) as caught:
            await client.package_control({"operation_id": "op-1", "operation": {"kind": "status"}})
        assert caught.value.code == "unsupported"
        assert seen == []
    finally:
        await client.close()
        await server.close()
