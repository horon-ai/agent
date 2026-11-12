"""Streaming chat example.

Requires a running bridge::

    horon-agent api-bridge

Usage: python examples/stream_chat.py "your prompt"
"""

from __future__ import annotations

import asyncio
import os
import sys

from horon_agent_sdk import ConnectOptions, HoronAgentClient


async def main() -> None:
    prompt = " ".join(sys.argv[1:]) or "Say hello in five words."

    client = await HoronAgentClient.connect(ConnectOptions(client_name="stream-chat-example/0.1"))
    print(f"connected to {client.server} ({', '.join(client.capabilities)})")

    session = await client.create_session(os.getcwd())
    session_id = session["session_id"]
    print(f"session {session_id}\n")

    stream = client.events(session_id)
    await client.send_message(session_id, prompt)

    try:
        async for event in stream:
            kind = event.get("ev")
            if kind == "model_info":
                print(f"[model] {event.get('provider')}/{event.get('model')}\n")
            elif kind == "text_delta":
                print(event.get("text", ""), end="", flush=True)
            elif kind == "tool_start":
                print(f"\n[tool] {event.get('name')}")
            elif kind == "permission_request":
                print(f"\n[permission] {event.get('tool_name')}: {event.get('description')} -> allow")
                await client.respond_to_permission(session_id, str(event.get("request_id")), "allow")
            elif kind == "background_progress":
                print(f"\n[bg {event.get('task_id')}] {event.get('summary')}")
            elif kind == "token_usage":
                print(f"\n\n[tokens] in={event.get('input')} out={event.get('output')}")
            elif kind == "turn_done":
                return
            elif kind == "error":
                print(f"\n[error] {event.get('code')}: {event.get('message')}")
                return
    finally:
        await stream.aclose()
        await client.close()


if __name__ == "__main__":
    asyncio.run(main())
