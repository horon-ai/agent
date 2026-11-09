#!/usr/bin/env bash
# Guard that the built Linux binary starts on a bare server.
#
# A GitHub-hosted ubuntu runner ships libssl, a CA bundle, a TTY, and
# XDG_RUNTIME_DIR. A release binary that dynamically links OpenSSL therefore
# passed every test there while failing to load at all on a minimal Debian
# server. Everything below runs inside Docker so the same check works on the
# runner and on a developer's macOS machine.
#
# Usage: check_linux_bare_runtime.sh <path-to-linux-horon-agent-binary>
set -euo pipefail

fail() {
  echo "bare linux runtime: FAIL: $*" >&2
  exit 1
}

if [ "$#" -ne 1 ]; then
  fail "usage: $0 <path-to-linux-horon-agent-binary>"
fi
if [ ! -f "$1" ]; then
  fail "not a file: $1"
fi
binary="$(cd "$(dirname "$1")" && pwd)/$(basename "$1")"

mock_name="horon-bare-runtime-mock-$$"
run_name="horon-bare-runtime-run-$$"

cleanup() {
  docker rm -f "$mock_name" "$run_name" >/dev/null 2>&1 || true
}
trap cleanup EXIT

# The mock speaks just enough of the OpenAI chat API for `run` to complete. The
# runner joins this container's network namespace, so 127.0.0.1 inside both is
# the same loopback and no port is published.
read -r -d '' mock_py <<'PY' || true
import http.server, json

CONTENT = "BARE_LINUX_OK"


class Handler(http.server.BaseHTTPRequestHandler):
    def do_POST(self):
        length = int(self.headers.get("content-length", 0))
        body = json.loads(self.rfile.read(length) or b"{}")
        self.send_response(200)
        if body.get("stream"):
            self.send_header("content-type", "text/event-stream")
            self.end_headers()
            for delta in ({"role": "assistant", "content": ""},
                          {"content": CONTENT}):
                chunk = {"id": "mock", "object": "chat.completion.chunk",
                         "created": 0, "model": "mock-model",
                         "choices": [{"index": 0, "delta": delta,
                                      "finish_reason": None}]}
                self.wfile.write(b"data: " + json.dumps(chunk).encode() + b"\n\n")
            final = {"id": "mock", "object": "chat.completion.chunk",
                     "created": 0, "model": "mock-model",
                     "choices": [{"index": 0, "delta": {},
                                  "finish_reason": "stop"}],
                     "usage": {"prompt_tokens": 5, "completion_tokens": 2,
                               "total_tokens": 7}}
            self.wfile.write(b"data: " + json.dumps(final).encode()
                             + b"\n\ndata: [DONE]\n\n")
        else:
            self.send_header("content-type", "application/json")
            self.end_headers()
            payload = {"id": "mock", "object": "chat.completion", "created": 0,
                       "model": "mock-model",
                       "choices": [{"index": 0, "finish_reason": "stop",
                                    "message": {"role": "assistant",
                                                "content": CONTENT}}],
                       "usage": {"prompt_tokens": 5, "completion_tokens": 2,
                                 "total_tokens": 7}}
            self.wfile.write(json.dumps(payload).encode())

    def log_message(self, *args):
        pass


http.server.HTTPServer(("127.0.0.1", 8099), Handler).serve_forever()
PY

docker run -d --name "$mock_name" python:3.12-slim \
  python3 -c "$mock_py" >/dev/null \
  || fail "could not start the mock provider container"

ready=0
for _ in $(seq 1 60); do
  if docker exec "$mock_name" python3 -c '
import urllib.request
request = urllib.request.Request(
    "http://127.0.0.1:8099/v1/chat/completions",
    data=b"{\"stream\": false}",
    headers={"content-type": "application/json"},
)
try:
    urllib.request.urlopen(request, timeout=2).read()
except Exception:
    raise SystemExit(1)
' >/dev/null 2>&1; then
    ready=1
    break
  fi
  sleep 1
done
if [ "$ready" -ne 1 ]; then
  docker logs "$mock_name" >&2 || true
  fail "mock provider did not become ready"
fi

read -r -d '' inner <<'INNER' || true
set -euo pipefail

fail() {
  echo "bare linux runtime: FAIL: $*" >&2
  exit 1
}

binary=/usr/local/bin/horon-agent

# Strip the pieces a desktop or CI image carries and a bare server does not.
rm -f /usr/lib/*/libssl.so* /usr/lib/*/libcrypto.so*
rm -rf /etc/ssl/certs /usr/share/ca-certificates
unset XDG_RUNTIME_DIR || true

allowed='^(linux-vdso\.so|ld-linux|libc\.so\.6|libm\.so\.6|libgcc_s\.so\.1|libpthread\.so\.0|libdl\.so\.2|librt\.so\.1|libutil\.so\.1)'
ldd_output="$(ldd "$binary" 2>&1)" || fail "ldd could not read the binary: $ldd_output"
while IFS= read -r line; do
  [ -n "$line" ] || continue
  trimmed="${line#"${line%%[![:space:]]*}"}"
  case "$trimmed" in
    *"not found"*)
      fail "unresolved shared library: ${trimmed%% *}"
      ;;
  esac
  name="${trimmed%%[[:space:]]*}"
  base="${name##*/}"
  if [[ ! "$base" =~ $allowed ]]; then
    fail "unexpected shared library dependency: $base"
  fi
done <<< "$ldd_output"

"$binary" --version >/dev/null \
  || fail "horon-agent --version failed"

export HOME=/tmp/bare-home
export HORON_HOME="$HOME/.horon"
mkdir -p "$HORON_HOME"
cat > "$HORON_HOME/config.toml" <<'CONFIG'
[provider]
default_provider = "mock"

[providers.mock]
type = "openai-compatible"
base_url = "http://127.0.0.1:8099/v1"
auth = "none"
requires_api_key = false
default_model = "mock-model"
CONFIG

status=0
output="$(timeout 120 "$binary" --no-update run "say hi" </dev/null 2>&1)" || status=$?
if [ "$status" -ne 0 ]; then
  printf '%s\n' "$output" >&2
  fail "run exited with status $status"
fi
case "$output" in
  *BARE_LINUX_OK*) ;;
  *)
    printf '%s\n' "$output" >&2
    fail "run output did not contain BARE_LINUX_OK"
    ;;
esac
INNER

docker run --rm --name "$run_name" --network "container:$mock_name" \
  -v "$binary:/usr/local/bin/horon-agent:ro" \
  debian:trixie-slim bash -c "$inner" \
  || exit 1

echo "bare linux runtime: ok"
