# Releasing `horon-agent-sdk` (Python)

The release is one workflow run plus one PyPI-side setting. This document says
which, so the release is a short checklist rather than a research exercise.

## Artifacts

One version, eight files:

- `horon_agent_sdk-<version>.tar.gz` — the sdist.
- `horon_agent_sdk-<version>-py3-none-any.whl` — the pure wheel. No runtime; a
  consumer of this wheel needs `horon-agent` on `PATH` or an explicit
  `binary=`.
- Six platform wheels, each with the runtime under
  `horon_agent_sdk/_bin/`:

  | Wheel tag              | Release archive                        |
  | ---------------------- | -------------------------------------- |
  | `manylinux2014_x86_64` | `horon-agent-linux-x86_64.tar.gz`      |
  | `manylinux2014_aarch64`| `horon-agent-linux-aarch64.tar.gz`     |
  | `macosx_10_12_x86_64`  | `horon-agent-macos-x86_64.tar.gz`      |
  | `macosx_11_0_arm64`    | `horon-agent-macos-aarch64.tar.gz`     |
  | `win_amd64`            | `horon-agent-windows-x86_64.tar.gz`    |
  | `win_arm64`            | `horon-agent-windows-aarch64.tar.gz`   |

The Linux wheels keep both `horon-agent-linux-<arch>` and its
`horon-agent-linux-<arch>.bin` payload. The launcher resolves the payload next
to itself, so dropping either file breaks `launch()` on Linux.

## Publishing with the workflow (preferred)

`.github/workflows/publish-python-sdk.yml`, "Publish Python SDK",
`workflow_dispatch` with one input: `horon_agent_release_tag`, an existing
horon-agent release tag whose six archives should be bundled.

1. One-time setup on PyPI: add a **trusted publisher** for project
   `horon-agent-sdk`, owner `horon-ai`, repository `agent`, workflow
   `publish-python-sdk.yml`, environment `pypi`. No API token is stored in the
   repository; the job authenticates with `id-token: write`.
2. In GitHub, create the `pypi` environment (Settings → Environments) and add
   whatever reviewers or branch rules the release should require.
3. Run the workflow with the release tag. It validates the SDK
   (`uv sync --locked`, ruff, mypy, pytest), enforces the Rust/TypeScript/Python
   surface parity test, downloads the six archives into `sdk-runtime-assets/`,
   runs `bash scripts/sdk/build_python_wheels.sh sdk-runtime-assets sdk-python-dist`,
   and publishes with `uv publish --trusted-publishing always sdk-python-dist/*`.

The build script writes only the eight artifacts above. It fails before
building if any release archive is missing, so a partial upload cannot look
like a successful release.

## Manual fallback

Use this when trusted publishing is unavailable (a first release before the
publisher exists, or an emergency from a laptop). It needs PyPI credentials.

```bash
cd sdk/python
uv sync --locked
uv run ruff check .
uv run ruff format --check .
uv run mypy src
uv run pytest

cd ../..
mkdir -p sdk-runtime-assets
gh release download <horon_agent_release_tag> --dir sdk-runtime-assets \
  --pattern 'horon-agent-linux-x86_64.tar.gz' \
  --pattern 'horon-agent-linux-aarch64.tar.gz' \
  --pattern 'horon-agent-macos-x86_64.tar.gz' \
  --pattern 'horon-agent-macos-aarch64.tar.gz' \
  --pattern 'horon-agent-windows-x86_64.tar.gz' \
  --pattern 'horon-agent-windows-aarch64.tar.gz'

bash scripts/sdk/build_python_wheels.sh sdk-runtime-assets sdk-python-dist

# A token from https://pypi.org/manage/account/token/ with upload scope for this
# project. Never commit it.
UV_PUBLISH_TOKEN=pypi-... uv publish sdk-python-dist/*
```

Both routes run from the **repository root**: the script resolves its own
location and accepts relative paths.

## Verifying a published release

```bash
cd "$(mktemp -d)"
python3 -m venv .venv
./.venv/bin/pip install horon-agent-sdk

./.venv/bin/python - <<'PY'
import asyncio
from horon_agent_sdk import HoronAgentClient, bundled_horon_agent_binary

print("bundled runtime:", bundled_horon_agent_binary())

async def main() -> None:
    client = await HoronAgentClient.launch()
    try:
        session = await client.create_session()
        print("session:", session["session_id"])
    finally:
        await client.close()

asyncio.run(main())
PY
```

`bundled_horon_agent_binary()` must print a path inside `site-packages`, and the
launch must succeed without `horon-agent` being installed separately. On a
machine where the platform wheel was not selected (an unsupported platform, or
`--no-binary`), it prints `None`; that is the pure wheel and needs a runtime on
`PATH`.

## Versioning

Semver against protocol v1 (see the README's stability section):

- **Patch** for fixes that change no types and no wire shape.
- **Minor** for new methods, events, and optional fields.
- **Major** only for a protocol major bump, which the handshake rejects rather
  than half-supporting.

`version` lives in `sdk/python/pyproject.toml`. It must match
`sdk/typescript/package.json`: `cargo test -p horon-agent-sdk parity` compares
the public client surfaces, and the release notes treat the two SDKs as one
version line.

## Guards that run on a release

- `cargo test -p horon-agent-sdk parity` — every shared capability exists in
  Rust, TypeScript, and Python; neither SDK has an untriaged public method.
- `uv run pytest` — includes `tests/test_protocol_parity.py`, which reads
  `sdk/typescript/src/protocol.ts` and fails if the request/event tag sets or
  protocol versions diverge.
- `cargo test -p harness-api` — fails if a Rust event or request variant, a
  payload field, or an external credential location is missing from the SDK.

## Platform support

Linux and macOS are exercised end to end. Windows is wired up (named pipe
derivation is pinned by tests on both sides, and a `win_amd64`/`win_arm64`
wheel ships the runtime) but has no live CI coverage yet. Do not describe it as
supported until something actually runs there.
