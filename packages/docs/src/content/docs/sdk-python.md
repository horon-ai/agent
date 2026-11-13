---
title: Python SDK
description: "horon-agent-sdk for Python 3.10+: launch a private instance or connect to a running daemon over protocol v1."
section: Build
order: 2
---

`horon-agent-sdk` is the Python client for the harness API. It connects over the
Unix socket (a named pipe on Windows), speaks protocol v1, and exposes the same
capability surface as the Rust and TypeScript SDKs.

## Install

Neither SDK is on PyPI yet, so install from a source checkout:

```sh
cd packages/sdk/python
uv sync
uv run python -c "import horon_agent_sdk"
```

The published shape is fixed: `scripts/sdk/build_python_wheels.sh` produces a pure
wheel, an sdist, and one platform wheel per supported platform. Platform wheels
carry the runtime; the pure wheel and the sdist do not, so `launch()` falls back
to `horon-agent` on `PATH`.

Requirements: Python 3.10 or newer on Linux, macOS or Windows. The runtime
dependency is `jsonschema`, used only by structured output and session-tool input
validation.

## First run

```python
import asyncio

from horon_agent_sdk import HoronAgentClient


async def main() -> None:
    client = await HoronAgentClient.launch()
    try:
        session = await client.create_session()
        turn = await client.run(session["session_id"], "say hi")
        print(turn.text)
    finally:
        await client.close()


asyncio.run(main())
```

## Two ways to use it

`HoronAgentClient.launch()` starts a **private instance**: its own state
directory, sessions and socket. Use it when your application embeds the agent as
an engine; `close()` stops the daemon and removes the temporary state directory.

`HoronAgentClient.connect()` attaches to the daemon **already running** on the
machine. Use it for an editor plugin, a dashboard, or a hotkey tool that automates
the user's own sessions; anything done there is visible in their terminal.

```python
client = await HoronAgentClient.launch()
session = await client.create_session()  # working_dir=None -> runtime default
await client.send_message(session["session_id"], "hello")
```

## Full system prompt override

```python
from horon_agent_sdk import CreateSessionOptions

session = await client.create_session_with_options(
    CreateSessionOptions(system_prompt="You are a concise code reviewer.")
)
```

The override replaces the entire assembled system prompt and is immutable after
session creation. Omit it to keep normal assembly.
