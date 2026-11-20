"""Run one Python cell at a time over a private NDJSON channel."""

import ast
import asyncio
import contextvars
import inspect
import io
import json
import os
import sys
import threading
import traceback

MAX_FRAME = 256 * 1024
MAX_TOOL_CALLS = 64
TEXT_CHUNK = 4096  # At most 12 JSON bytes per character, including surrogate pairs.


def encode_frame(kind, cell_id, **fields):
    frame = json.dumps({"type": kind, "id": cell_id, **fields}, ensure_ascii=True, allow_nan=False)
    return frame.encode("ascii") + b"\n"


class ProtocolError(Exception):
    pass


class Channel:
    def __init__(self):
        # Ordinary Python streams and inherited stdio cannot write to the wire.
        self.reader = os.fdopen(os.dup(0), "rb")
        self.writer = os.fdopen(os.dup(1), "wb")
        self.lock = threading.Lock()
        self.owner = os.getpid()
        self.thread = threading.get_ident()
        self.cell_context = contextvars.ContextVar("horon_eval_cell", default=None)
        self.seq = 0
        self.current = None
        with open(os.devnull, "rb") as empty:
            os.dup2(empty.fileno(), 0)
        os.dup2(2, 1)

    def _write(self, frame):
        self.writer.write(frame)
        self.writer.flush()

    def start(self, cell_id):
        frame = encode_frame("started", cell_id)
        with self.lock:
            self._write(frame)
            self.current = cell_id
            self.seq = 0

    def finish(self, ok):
        frame = encode_frame("done", self.current, ok=ok)
        with self.lock:
            self._write(frame)
            self.current = None

    def reject(self, text):
        frame = encode_frame("error", None, text=text)
        with self.lock:
            self._write(frame)

    def write_text(self, kind, text):
        # A fork child must never use the parent's wire or its inherited lock.
        cell_id = self.current
        if cell_id is not None and os.getpid() == self.owner:
            frame = encode_frame(kind, cell_id, text=text)
            with self.lock:
                if self.current == cell_id:
                    self._write(frame)
                    return
        os.write(2, text.encode("utf-8"))

    def read_frame(self):
        line = self.reader.readline(MAX_FRAME + 1)
        if not line:
            return None
        if len(line) > MAX_FRAME or not line.endswith(b"\n"):
            raise ValueError("Invalid or oversized eval frame")
        try:
            frame = json.loads(line)
        except (ValueError, RecursionError):
            raise ValueError("Invalid eval JSON") from None
        if not isinstance(frame, dict):
            raise ValueError("Expected an eval frame object")
        return frame

    def request(self):
        request = self.read_frame()
        if request is None:
            return None
        cell_id = request.get("id")
        if type(cell_id) is not int or not 0 <= cell_id < 2**64:
            raise ValueError("Invalid eval cell ID")
        if not isinstance(request.get("code"), str):
            raise ValueError("Expected eval code as text")
        return request

    def call_tool(self, name, args):
        if os.getpid() != self.owner or threading.get_ident() != self.thread:
            raise RuntimeError("Python tools must be called on the active cell thread")
        cell_id = self.cell_context.get()
        if cell_id is None or cell_id != self.current:
            raise RuntimeError("Python tool call belongs to a finished cell")
        if self.seq >= MAX_TOOL_CALLS:
            raise RuntimeError("Python eval allows at most 64 tool calls per cell")
        seq = self.seq + 1
        frame = encode_frame("tool_call", cell_id, seq=seq, name=name, args=args)
        if len(frame) > MAX_FRAME:
            raise ValueError("Python tool request exceeds the 256 KiB frame limit")
        with self.lock:
            self._write(frame)
            self.seq = seq
        try:
            reply = self.read_frame()
        except ValueError as error:
            raise ProtocolError("Malformed Python tool response") from error
        if (
            reply is None
            or reply.get("type") != "tool_result"
            or type(reply.get("id")) is not int
            or reply["id"] != cell_id
            or type(reply.get("seq")) is not int
            or reply["seq"] != seq
            or type(reply.get("ok")) is not bool
            or not isinstance(reply.get("text"), str)
        ):
            raise ProtocolError("Unexpected Python tool response")
        if not reply["ok"]:
            raise RuntimeError(reply["text"])
        return reply["text"]


class ToolProxy:
    def __init__(self, channel):
        self._channel = channel

    def __getattr__(self, name):
        if name.startswith("_"):
            raise AttributeError(name)

        async def call(args=None, **kwargs):
            if args is None:
                args = kwargs
            elif not isinstance(args, dict) or kwargs:
                raise TypeError("Pass one JSON object or keyword arguments to the tool")
            return self._channel.call_tool(name, args)

        return call


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


def evaluate(code, namespace, loop):
    tree = ast.parse(code, filename="<horon eval>", mode="exec")
    last = tree.body.pop() if tree.body and isinstance(tree.body[-1], ast.Expr) else None
    flags = ast.PyCF_ALLOW_TOP_LEVEL_AWAIT
    body = compile(tree, "<horon eval>", "exec", flags=flags, dont_inherit=True)
    expression = None
    if last is not None:
        expression = compile(ast.Expression(last.value), "<horon eval>", "eval", flags=flags, dont_inherit=True)
    if body.co_flags & inspect.CO_COROUTINE or (expression and expression.co_flags & inspect.CO_COROUTINE):
        value = loop.run_until_complete(evaluate_async(body, expression, namespace))
    else:
        exec(body, namespace)
        value = eval(expression, namespace) if expression else None
    if value is not None:
        print(repr(value))


async def evaluate_async(body, expression, namespace):
    result = eval(body, namespace)
    if body.co_flags & inspect.CO_COROUTINE:
        await result
    if expression is None:
        return None
    result = eval(expression, namespace)
    return await result if expression.co_flags & inspect.CO_COROUTINE else result


def execute(channel, request, namespace, loop, stdout, stderr):
    channel.start(request["id"])
    token = channel.cell_context.set(request["id"])
    original_stdout, original_stderr = sys.stdout, sys.stderr
    sys.stdout, sys.stderr = stdout, stderr
    ok = False
    try:
        evaluate(request["code"], namespace, loop)
        ok = True
    except ProtocolError:
        raise
    except BaseException as error:
        stream = CellStream(channel, "error")
        traceback.print_exception(type(error), error, error.__traceback__, limit=-12, file=stream)
    finally:
        channel.cell_context.reset(token)
        sys.stdout, sys.stderr = original_stdout, original_stderr
    channel.finish(ok)


def main():
    channel = Channel()
    if sys.version_info < (3, 10):
        channel.reject("Python eval requires Python 3.10 or later")
        return 2
    namespace = {"__name__": "__main__", "tool": ToolProxy(channel)}
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
    except ProtocolError as error:
        channel.reject(str(error))
        return 2
    finally:
        loop.close()
        channel.writer.close()
        channel.reader.close()


if __name__ == "__main__":
    sys.exit(main())
