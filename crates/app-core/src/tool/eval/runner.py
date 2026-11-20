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
        self.owner = os.getpid()
        self.current = None
        with open(os.devnull, "rb") as empty:
            os.dup2(empty.fileno(), 0)
        os.dup2(2, 1)

    def _send(self, kind, cell_id, **fields):
        frame = json.dumps({"type": kind, "id": cell_id, **fields}, ensure_ascii=True)
        self.writer.write(frame.encode("ascii") + b"\n")
        self.writer.flush()

    def start(self, cell_id):
        with self.lock:
            self._send("started", cell_id)
            self.current = cell_id

    def finish(self, ok):
        with self.lock:
            self._send("done", self.current, ok=ok)
            self.current = None

    def reject(self, text):
        with self.lock:
            self._send("error", None, text=text)

    def write_text(self, kind, text):
        # A fork child must never use the parent's wire or its inherited lock.
        if os.getpid() == self.owner:
            with self.lock:
                if self.current is not None:
                    self._send(kind, self.current, text=text)
                    return
        os.write(2, text.encode("utf-8"))

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
    def __init__(self, channel, kind):
        self.channel = channel
        self.kind = kind

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
            self.channel.write_text(self.kind, chunk)
        return len(text)


async def evaluate(code, namespace):
    tree = ast.parse(code, filename="<horon eval>", mode="exec")
    last = tree.body.pop() if tree.body and isinstance(tree.body[-1], ast.Expr) else None
    flags = ast.PyCF_ALLOW_TOP_LEVEL_AWAIT
    body = compile(tree, "<horon eval>", "exec", flags=flags, dont_inherit=True)
    if body.co_flags & inspect.CO_COROUTINE:
        await eval(body, namespace)
    else:
        exec(body, namespace)
    if last is not None:
        expression = compile(ast.Expression(last.value), "<horon eval>", "eval", flags=flags, dont_inherit=True)
        value = eval(expression, namespace)
        if expression.co_flags & inspect.CO_COROUTINE:
            value = await value
        if value is not None:
            print(repr(value))


def execute(channel, request, namespace, loop, stdout, stderr):
    channel.start(request["id"])
    original_stdout, original_stderr = sys.stdout, sys.stderr
    sys.stdout, sys.stderr = stdout, stderr
    ok = False
    try:
        loop.run_until_complete(evaluate(request["code"], namespace))
        ok = True
    except BaseException as error:
        stream = CellStream(channel, "error")
        traceback.print_exception(type(error), error, error.__traceback__, limit=-12, file=stream)
    finally:
        sys.stdout, sys.stderr = original_stdout, original_stderr
    channel.finish(ok)


def main():
    channel = Channel()
    if sys.version_info < (3, 10):
        channel.reject("Python eval requires Python 3.10 or later")
        return 2
    namespace = {"__name__": "__main__"}
    stdout = CellStream(channel, "stdout")
    stderr = CellStream(channel, "stderr")
    loop = asyncio.new_event_loop()
    asyncio.set_event_loop(loop)
    try:
        while True:
            try:
                request = channel.request()
            except ValueError as error:
                channel.reject(str(error))
                return 2
            if request is None:
                return 0
            execute(channel, request, namespace, loop, stdout, stderr)
    finally:
        loop.close()
        channel.writer.close()
        channel.reader.close()


if __name__ == "__main__":
    sys.exit(main())
