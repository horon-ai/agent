"""Run one Python cell at a time over a private NDJSON channel."""

import ast
import asyncio
import inspect
import io
import json
import os
import sys
import threading
import traceback

MAX_FRAME = 256 * 1024
TEXT_CHUNK = 4096  # At most 12 JSON bytes per character, including surrogate pairs.


class Channel:
    def __init__(self):
        # Ordinary Python streams and inherited stdio cannot write to the wire.
        self.reader = os.fdopen(os.dup(0), "rb")
        self.writer = os.fdopen(os.dup(1), "wb")
        self.lock = threading.Lock()
        with open(os.devnull, "rb") as empty:
            os.dup2(empty.fileno(), 0)
        os.dup2(2, 1)

    def send(self, kind, cell_id, **fields):
        frame = json.dumps({"type": kind, "id": cell_id, **fields}, ensure_ascii=True)
        with self.lock:
            self.writer.write(frame.encode("ascii") + b"\n")
            self.writer.flush()

    def request(self):
        line = self.reader.readline(MAX_FRAME + 1)
        if not line:
            return None
        if len(line) > MAX_FRAME or not line.endswith(b"\n"):
            raise ValueError("Invalid or oversized eval frame")
        try:
            request = json.loads(line)
        except (ValueError, RecursionError):
            raise ValueError("Invalid eval JSON") from None
        if not isinstance(request, dict):
            raise ValueError("Expected an eval request object")
        cell_id = request.get("id")
        if type(cell_id) is not int or not 0 <= cell_id < 2**64:
            raise ValueError("Invalid eval cell ID")
        if not isinstance(request.get("code"), str):
            raise ValueError("Expected eval code as text")
        return request


class CellStream(io.TextIOBase):
    def __init__(self, channel, kind, cell_id):
        self.channel = channel
        self.kind = kind
        self.cell_id = cell_id

    @property
    def encoding(self):
        return "utf-8"

    def writable(self):
        return True

    def write(self, text):
        if not isinstance(text, str):
            raise TypeError("write() argument must be str")
        for start in range(0, len(text), TEXT_CHUNK):
            chunk = text[start : start + TEXT_CHUNK]
            # Rust's UTF-8 strings cannot represent lone Python surrogates.
            chunk = chunk.encode("utf-8", "backslashreplace").decode("utf-8")
            self.channel.send(self.kind, self.cell_id, text=chunk)
        return len(text)


def evaluate(code, namespace, loop):
    tree = ast.parse(code, filename="<horon eval>", mode="exec")
    last = tree.body.pop() if tree.body and isinstance(tree.body[-1], ast.Expr) else None
    flags = ast.PyCF_ALLOW_TOP_LEVEL_AWAIT
    body = compile(tree, "<horon eval>", "exec", flags=flags, dont_inherit=True)
    if body.co_flags & inspect.CO_COROUTINE:
        loop.run_until_complete(eval(body, namespace))
    else:
        exec(body, namespace)
    if last is not None:
        expression = compile(ast.Expression(last.value), "<horon eval>", "eval", flags=flags, dont_inherit=True)
        value = eval(expression, namespace)
        if expression.co_flags & inspect.CO_COROUTINE:
            value = loop.run_until_complete(value)
        if value is not None:
            print(repr(value))


def execute(channel, request, namespace, loop):
    cell_id = request["id"]
    channel.send("started", cell_id)
    original_stdout, original_stderr = sys.stdout, sys.stderr
    sys.stdout = CellStream(channel, "stdout", cell_id)
    sys.stderr = CellStream(channel, "stderr", cell_id)
    ok = False
    try:
        evaluate(request["code"], namespace, loop)
        ok = True
    except (Exception, SystemExit, KeyboardInterrupt) as error:
        stream = CellStream(channel, "error", cell_id)
        traceback.print_exception(type(error), error, error.__traceback__, limit=12, file=stream)
    finally:
        sys.stdout, sys.stderr = original_stdout, original_stderr
    channel.send("done", cell_id, ok=ok)


def main():
    channel = Channel()
    if sys.version_info < (3, 10):
        channel.send("error", None, text="Python eval requires Python 3.10 or later")
        return 2
    namespace = {"__name__": "__main__"}
    loop = asyncio.new_event_loop()
    asyncio.set_event_loop(loop)
    try:
        while True:
            try:
                request = channel.request()
            except ValueError as error:
                channel.send("error", None, text=str(error))
                return 2
            if request is None:
                return 0
            execute(channel, request, namespace, loop)
    finally:
        loop.close()
        channel.writer.close()
        channel.reader.close()


if __name__ == "__main__":
    sys.exit(main())
