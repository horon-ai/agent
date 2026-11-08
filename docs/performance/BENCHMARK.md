# 성능 측정

horon의 성능은 세 가지 장치로 본다. 무엇을 묻는지가 서로 다르다.

| 장치 | 묻는 것 | 언제 도는가 | 실패 조건 | 결과가 남는 곳 |
| --- | --- | --- | --- | --- |
| [벤치마크](#벤치마크) | 얼마나 빠르고 얼마나 쓰는가, 다른 에이전트와 비교하면 어떤가 | 수동 실행, `main` 코드 변경 뒤 자동 | 스크립트 오류만 (수치로 실패하지 않음) | 작업 요약, 아티팩트, `RESULTS.md` 자동 구간 |
| [구조 게이트](#구조-게이트) | 큐·버퍼·자원이 여전히 상한 안에 있는가 | `dev`·`main` PR과 push | 카운터가 경계를 넘음 | CI 결과 |
| [A/B 측정](#ab-측정) | 이 변경이 실제로 얼마나 줄였는가 | 성능 변경을 할 때 손으로 | 실행기가 판정 | PR 본문, `RESULTS.md` |

측정 결과와 그 한계는 [`RESULTS.md`](RESULTS.md)에 있다.

## 벤치마크

`.github/workflows/benchmark.yml`이 소유한다. Linux, macOS, Windows를 **OS마다 따로** GitHub 호스티드 러너에서 잰다.

| OS 키 | 러너 | 메모리 지표 | 비고 |
| --- | --- | --- | --- |
| `linux` | `ubuntu-24.04` | RSS, PSS | `fsync` 호출 수도 잰다(`strace`) |
| `macos` | `macos-latest` | RSS | |
| `windows` | `windows-latest` | Working Set | 데몬 연결은 Named Pipe, 하네스 브리지 기동은 측정 안 함(`api-bridge`는 Unix 전용) |

OS마다 `.github/workflows/benchmark-os.yml`을 한 번씩 부르고, 그 안에서 작업을 다음처럼 나눠 돌린다. OS마다 따로 부르므로 한 OS의 측정은 그 OS의 빌드가 끝나자마자 시작하고, 가장 느린 OS의 빌드를 기다리지 않는다.

| 작업 | 수 | 하는 일 |
| --- | --- | --- |
| `build` | OS마다 1개 | `horon-agent`와 `loading_probe` release 바이너리를 한 번 만들어 `bench-bin-<os>-<run_id>` 아티팩트(tar, 1일 보관)로 올린다. `baseline_ref`가 있고 `compare`를 고르면 그 커밋도 따로 빌드해 같은 tar의 `baseline/`에 넣는다. 두 빌드 모두 `release.yml`이 배포 바이너리를 만드는 값(`CARGO_PROFILE_RELEASE_OPT_LEVEL=3`, `CARGO_PROFILE_RELEASE_DEBUG=0`, `CARGO_INCREMENTAL=0`)으로 빌드한다. 저장소의 `[profile.release]`는 로컬 재빌드용(`opt-level = 1`, incremental)이고, 경쟁 도구는 각자 배포한 릴리스 빌드로 재기 때문이다. |
| `measure` | OS × 측정 단위마다 1개 | 바이너리를 내려받아 측정 단위 하나만 잰다. 단위는 `compare`, `startup`, `loading-cold`, `loading-warm`, `tools`, `reload`다. `compare`는 기준 빌드가 있으면 `horon-base` 줄로 같은 작업 안에서 번갈아 잰다. |
| `compile` | OS마다 1개 | 소스를 직접 다시 빌드하므로 다른 측정과 기계를 나누지 않게 따로 돈다. |
| `merge` | OS마다 1개 | 단위별 결과 `bench-part-<os>-<unit>-<run_id>`를 모아 예전과 같은 `benchmark-<os>-<run_id>` 아티팩트로 합친다(`scripts/perf/bench_merge.py`). |

한 묶음의 표에 들어가는 비교 행(horon과 각 경쟁 도구, 각 이벤트 수)은 한 기계에서 함께 잰다. 그래서 그 행들은 CPU·디스크·부하를 공유한다. 서로 다른 묶음끼리, 그리고 OS끼리는 기계가 달라 절대값을 비교하지 않는다. 단위 하나가 실패해도 다른 단위와 다른 OS는 계속 돈다. 그 OS의 `status.txt`는 `success`가 아니게 기록되고, 이미 잰 결과는 아티팩트에 남는다.

### 실행하기

```bash
gh workflow run benchmark.yml --ref dev                                   # 세 OS, 모든 묶음
gh workflow run benchmark.yml --ref dev -f os=windows                     # Windows만
gh workflow run benchmark.yml --ref dev -f os=linux,macos -f suites=compare,tools
gh workflow run benchmark.yml --ref dev -f competitors=claude_code,codex
gh workflow run benchmark.yml --ref dev -f suites=compare -f competitors=omp,omo
```

| 입력 | 기본값 | 뜻 |
| --- | --- | --- |
| `os` | `all` | `linux`, `macos`, `windows` 중 쉼표로 고른다. 고른 OS마다 위 작업이 뜬다. |
| `suites` | `all` | `compare`, `startup`, `loading`, `tools`, `reload`, `compile` 중 쉼표로 고른다. |
| `competitors` | `jcode,claude_code,codex,opencode,pi,omp,omo` | horon과 비교할 도구. `omp`(oh-my-pi)와 `omo`(oh-my-openagent)가 들어 있다. 설치에 실패했거나 실행 런타임이 없는 도구는 `skipped`로 남는다. |
| `sessions` | `1 5 10` | 세션 메모리 비교의 동시 세션 수 |
| `turns` | `3` | 세션마다 보내는 턴 수 |
| `oneshot_runs` | `3` | one-shot 프롬프트를 빌드마다 몇 번 잴지. 두 빌드의 중앙값을 가려야 하면 20 정도로 올린다 |
| `baseline_ref` | 빈 값 | 지정하면 그 커밋을 같은 작업 안에서 따로 빌드해 `horon-base` 줄로 번갈아 잰다. 같은 러너의 전후 비교가 된다 |

모르는 OS나 묶음 이름이면 실행이 바로 실패한다(`scripts/perf/bench_select.py`). 결과는 OS별 `merge` 작업 요약과 `benchmark-<os>-<run_id>` 아티팩트(JSON, 마크다운)로 30일 남는다. 중간 아티팩트 이름은 `benchmark-`로 시작하지 않으므로 `benchmark-docs.yml`의 다운로드에 섞이지 않는다.

시크릿과 API 요금은 없다. 모델이 필요한 측정은 모두 `scripts/perf/mock_llm_server.py`(로컬 가짜 모델 서버)에 붙는다.

### 묶음

| 묶음 | 스크립트 | 재는 것 |
| --- | --- | --- |
| `compare` | `bench_compare.py`, `install_competitors.sh` | horon과 경쟁 도구(`jcode`, `claude_code`, `codex`, `opencode`, `pi`, `omp`, `omo`)의 `--version` 시간, 질문 1건 처리 시간과 최대 메모리, 동시 세션 수에 따른 메모리, 설치 크기, 스킬 호출 시간, MCP 도구 호출 시간과 도구 왕복. Linux는 `fsync` 호출 수도 잰다. |
| `startup` | `bench_startup.py` | `--help`·`--version` 로드, `serve` 소켓 준비, 하네스 브리지 단계별 준비 시간 |
| `loading` | `bench_loading.py` | 고정 시드 픽스처의 세션 로딩, COLD와 WARM |
| `tools` | `bench_tools.py` | debug 소켓으로 core 도구(read, write, edit, ls, agentgrep, bash) 14개 사례의 지연 |
| `reload` | `bench_reload_storm.py` | 세션 N개(1, 10, 40)를 동시에 다시 열어 `history`가 올 때까지의 지연 |
| `compile` | `bench_compile.py` | `cargo check`의 변경 없음, `src/main.rs`·`crates/app-core`·build-meta 수정 뒤 재빌드 시간 |

### 자동 측정

`.github/workflows/benchmark-docs.yml`은 `main`에 push된 커밋의 CI가 성공하면 뜬다. 앞 단계가 막으면 뒤 단계는 돌지 않는다.

1. **코드 변경 확인.** 직전 측정 커밋(`RESULTS.md`의 `<!-- bench-auto:start commit=... -->`) 이후 `src`, `crates`, `Cargo.toml`, `Cargo.lock`, `.cargo`, `rust-toolchain.toml`, `scripts/perf` 중 하나가 바뀌었는지 본다. 기록이 없거나 이력에 없는 커밋이면 부모 커밋과 비교한다. 바뀐 것이 없으면 끝낸다.
2. **구조 게이트 대기.** 같은 커밋의 `structural-perf.yml` push 실행이 성공할 때까지 최대 80분 기다린다.
3. **벤치마크.** `benchmark.yml`을 `os=all`, `suites=all`로 부른다. 세 OS가 따로 돈다.
4. **문서 갱신.** OS별 아티팩트를 모두 받아 `scripts/perf/bench_docs.py render`로 `RESULTS.md`의 `bench-auto` 구간만 다시 쓰고, `main`과 `dev`에 `docs(perf): record benchmark for <sha>` 커밋으로 push한다.

문서 구간에는 OS 요약표(러너, 바이너리, 상태)와 OS별 절이 Linux, macOS, Windows 순서로 들어간다. 실패한 OS는 "일부 실패"로 표시하고 그때까지 잰 값을 남긴다. 결과가 없는 OS도 절을 지우지 않고 "결과 없음"으로 적는다. 구간 밖의 글은 바이트 단위로 그대로 둔다.

문서 커밋은 `GITHUB_TOKEN`으로 push하므로 그 push가 워크플로를 다시 띄우지 않는다. 문서만 바뀐 커밋은 1단계에서 걸러진다. `workflow_run`은 기본 브랜치의 워크플로 파일만 실행하므로 이 파일이 `main`에 있어야 동작한다.

자동 구간의 수치는 회귀 판정 기준이 아니라 추세 기록이다. 손으로 고치지 않는다. 다음 측정이 덮어쓴다.

### 로컬에서 돌리기

모든 스크립트는 Python 표준 라이브러리만 쓰고 세 OS에서 돈다. 임시 HOME과 런타임 폴더를 쓰므로 실제 `~/.horon-agent`를 건드리지 않는다.

```bash
cargo build --release --locked -p horon-agent
python3 scripts/perf/bench_tools.py --binary ./target/release/horon-agent --markdown-out tools.md
python3 scripts/perf/bench_reload_storm.py --binary ./target/release/horon-agent --markdown-out reload.md
python3 scripts/perf/bench_startup.py ./target/release/horon-agent --runs 10
```

경쟁 도구 비교는 도구를 먼저 설치한다. `--bin name=path`로 지정하지 않은 도구는 PATH에서 찾는다.

```bash
bash scripts/perf/install_competitors.sh /tmp/bench-tools jcode,claude_code,codex,opencode,pi,omp,omo
python3 scripts/perf/bench_compare.py --bin horon=./target/release/horon-agent $(cat /tmp/bench-tools/bins.args) \
  --install-failures /tmp/bench-tools/failures.txt \
  --sessions 1 5 10 --turns 3 --json-out compare.json --markdown-out compare.md
```

`install_competitors.sh`는 버전을 고정해 설치한다. jcode는 GitHub 릴리스 `v0.99.1`(Windows는 `.exe`), 나머지는 `npm install --prefix`로 `@anthropic-ai/claude-code@2.1.317`, `@openai/codex@0.166.0`, `opencode-ai@1.19.5`, `@earendil-works/pi-coding-agent@1.5.0`, `@oh-my-pi/pi-coding-agent@18.18.0`, `omo-ai@5.1.64`이다. 버전은 `JCODE_VERSION`, `CLAUDE_VERSION`, `CODEX_VERSION`, `OPENCODE_VERSION`, `PI_VERSION`, `OMP_VERSION`, `OMO_VERSION`으로 바꾼다. Windows에서는 Git Bash로 돌리고, npm 도구는 `.cmd` 실행 파일로 기록한다.

pi는 1.x부터 `@earendil-works` 이름으로 배포되고 MCP 클라이언트를 내장한다(Node 22.19 이상). 0.73.1(`@mariozechner/pi-coding-agent`)에는 MCP 클라이언트가 없어 MCP 행을 잴 수 없었으므로 1.5.0으로 올렸고, 그래서 이 버전부터 pi의 모든 행이 이전 결과와 다른 버전을 잰다. MCP 측정은 `$PI_CODING_AGENT_DIR/mcp.json`에 픽스처 서버를 `"exposure": "direct"`로 넣는다. 기본값 `codemode`는 도구를 모델에 직접 알리지 않아 가짜 모델이 `mcp__bench__bench_echo`를 부를 수 없기 때문이다. 원샷, 스킬, 세션 측정은 `--no-mcp`로 MCP를 끄고 다른 도구와 같은 조건을 맞춘다.

jcode 내려받기는 일시적인 오류(5xx)에 대비한다. 릴리스 다운로드 주소를 5번까지 다시 시도하고, 그래도 실패하면 같은 파일을 GitHub 릴리스 API의 자산 id로 받는다. 어느 쪽으로 받든 릴리스의 `SHA256SUMS`에 적힌 값과 같을 때만 쓴다. 설치에 실패한 도구는 `<dest>/failures.txt`에 `이름<TAB>이유` 한 줄로 남고, `bench_compare.py --install-failures`가 이 파일을 읽어 `compare.md`의 `Skipped:` 줄에 `install failed: <이유>`로 적는다. 측정하지 못한 도구가 하나라도 있으면 `compare.md` 맨 앞에 `> **Warning: N tool(s) were not measured**` 줄이 붙고, GitHub Actions에서는 도구마다 `::warning` 주석과 실행 요약(step summary) 줄이 남아 run 화면에서 바로 보인다. `syscalls`처럼 측정 기능이 빠진 항목은 도구가 아니므로 경고하지 않는다.

### 경쟁 도구 비교의 지표

| 지표 | 뜻 |
| --- | --- |
| `version_ms` | `--version` 실행 N회(기본 5)의 통계 |
| `oneshot_ms` | 헤드리스 질문 1건(기동, `ls` 도구 1회, 최종 응답)의 시간, 그동안의 최대 프로세스 트리 메모리, 도구 왕복 여부 |
| `fsync_calls` | `--syscalls`를 켰을 때 질문 1건이 부르는 `fsync`·`fdatasync` 수. 시간을 재는 실행과 별도로, 같은 횟수만큼 `strace`를 씌운 실행을 더 돌려 센다. Linux에서 `strace`가 있을 때만 재고, 없으면 `skipped`에 사유가 남는다 |
| `sessions_memory` | N개 동시 세션 × K턴 뒤 idle과 턴 직후의 프로세스 트리 메모리. 첫 세션 값과 세션당 증가분(최소제곱 기울기)을 낸다 |
| `install_size_bytes` | 실행 파일 크기. npm 도구는 패키지와 설치된 의존성의 합 |
| `skill_ms` | 헤드리스 질문 1건이 픽스처 스킬을 찾아 본문까지 읽는 시간. `Via` 열이 그 도구가 쓴 경로(`skill-tool`/`skill-uri`/`file-read`)를 말한다 |
| `mcp_ms` | 헤드리스 질문 1건이 픽스처 stdio MCP 서버를 띄우고 `bench_echo`를 한 번 부르는 시간 |
| `mcp_ready_ms` | 프로세스 시작부터 도구 목록에 `bench_echo`가 처음 보인 요청까지의 시간(MCP 서버 시작과 도구 등록). 가짜 모델이 그 요청을 본 시각으로 잰다 |
| `tool_round_trip` | 스킬 본문의 센티널이나 MCP가 돌려준 nonce를 나중 요청에서 실제로 봤을 때만 `true` |
| `request_offsets_ms` | 실행 기록(JSON)마다 남는, 프로세스 시작부터 각 모델 요청이 도착할 때까지의 시간. 간격으로 기동, 도구 왕복, 종료 구간을 OS마다 나눠 본다 |

배수는 horon을 1.0×로 둔다. 도구마다 가짜 모델에 붙는 방법이 다르다. horon과 jcode는 임시 HOME의 `[providers.mock]`과 데몬 하나의 debug 소켓 세션, Claude Code는 `ANTHROPIC_BASE_URL`과 상주 `stream-json` 프로세스, Codex는 `model_providers`와 `app-server`, OpenCode는 `opencode.json`의 `baseURL`과 `serve`, pi와 omp와 omo는 임시 HOME 아래 `models.json`(`providers.mock`, `openai-completions`)을 쓰고 세션은 `--mode rpc`로 맡긴다.

세션 구현이 도구마다 다르므로(데몬 안의 세션과 세션마다 뜨는 프로세스) 절대값보다 세션당 증가분을 비교한다. `strace`는 시스템 호출마다 도구를 멈추므로 씌운 실행의 시간은 호출 수에 비례해 늘어난다. 그래서 `oneshot_ms`는 `strace` 없이 잰 실행에서만 내고, `strace`를 씌운 실행은 `fsync_calls`를 세는 데만 쓴다.

### omp와 omo

두 도구는 pi와 같은 `models.json`·RPC 계약을 쓰므로 같은 하네스를 공유하되 실행 방식과 기본값이 달라 다음을 따로 맞춘다.

| 도구 | 보고서 이름 | npm 패키지 | 고정 버전 | 실행 런타임 | mock 연결 |
| --- | --- | --- | --- | --- | --- |
| `omp` | `oh-my-pi (omp)` | `@oh-my-pi/pi-coding-agent` | `18.18.0` (`OMP_VERSION`) | Bun 1.4.4 이상 (`engines`) | 임시 HOME 아래 `models.json`의 `providers.mock`, `-p --no-session --no-title --model mock/mock-model --no-lsp` |
| `omo` | `oh-my-openagent (omo)` | `omo-ai` | `5.1.64` (`OMO_VERSION`) | Node 24 이상 또는 Bun | 같은 `models.json`, `-p --no-session --model mock/mock-model --tools bash,read` |

- `omp`는 npm CLI를 Bun 스크립트로 배포하므로 compare 작업이 `oven-sh/setup-bun`으로 Bun을 먼저 설치한다. Bun이 없으면 `omp`를, Node도 Bun도 없으면 `omo`를 installer가 `skipped`로 남기고 작업은 계속된다.
- Bun을 GitHub 릴리스에서 받지 못하면 installer가 npm 레지스트리의 `bun`(`BUN_VERSION`, 기본 1.4.4)으로 대신 설치한다.
- `omo`의 기본 도구 목록에는 셸 도구(`bash`)가 없고 `--tools`는 허용 목록이라, `bash`와 `read`를 명시해야 mock 모델이 도구를 부를 수 있다.
- `omp`는 `--no-title` 없이 실행하면 제목 생성용 모델 요청이 한 번 더 나가 원샷 요청 수가 달라진다. `--no-lsp`는 언어 서버 내려받기를 막는다.
- 두 도구 모두 agent 디렉터리에 모델 목록과 base URL을 캐시하므로(omp는 `agent.db`, `models.db`) 측정마다 새 agent 디렉터리를 만든다. 세션 측정에서는 세션마다 따로 만든다.
- 텔레메트리, 버전 확인, mock 서버 밖 네트워크는 `PI_OFFLINE=1`, `PI_SKIP_VERSION_CHECK=1`, `PI_TELEMETRY=0`, `DISABLE_TELEMETRY=1`, `DO_NOT_TRACK=1`로 끈다. 부모 환경의 `OMP_*`, `OMO_*`, `SENPI_*`, `PI_CONFIG_DIR`, API 키는 지우고 홈·설정 경로는 임시 디렉터리 아래만 가리킨다.
- 두 도구는 기본 비교 목록(`benchmark.yml`의 `competitors` 기본값과 `bench_compare.py`의 `DEFAULT_TOOLS`)에 들어 있다. Linux, macOS, Windows에서 `version_ms`·`oneshot`·`sessions_memory`가 모두 측정된 뒤에 넣었다. 측정 대상은 `benchmark-os.yml`이 입력 목록을 `--tools`로 넘겨 정하므로 `-f competitors=jcode,pi`처럼 빼는 것도 된다.

### 스킬 호출과 MCP 도구 호출

`compare` 묶음은 `--scenarios`로 시나리오를 고른다. 기본값은 `oneshot,sessions,skill,mcp`이고, 부분집합을 주면 빠진 시나리오는 실제로 돌지 않으며 보고서에도 표가 나오지 않는다. 모르는 이름은 실행을 바로 실패시킨다.

- `skill`: 프롬프트가 `[bench:skill]`로 시작한다. 가짜 모델은 그 마커를 보고 스킬 전용 도구(`Skill`, `skill`, `skills.read`, `skill_manage`)가 있으면 그것을 부르고, 없으면 설명에 `skill://`가 있는 읽기 도구에 `skill://bench-skill`을 넘기고, 그것도 없으면 읽기 도구나 셸 도구로 프롬프트가 준 `SKILL.md` 절대 경로를 연다. 어느 경로였는지는 요청 기록의 `via`와 보고서의 `Via` 열에 남는다.
- `mcp`: 프롬프트가 `[bench:mcp]`로 시작한다. 가짜 모델은 이름이 `bench_echo`로 끝나는 도구를 찾아 `{"text": "bench-nonce-N"}`으로 한 번 부른다. 그 도구가 Responses 네임스페이스 안에 있으면 함수 이름은 `bench_echo`, 네임스페이스는 별도 `namespace` 필드로 보낸다(Codex가 받는 `function_call` 모양). 도구 목록에 서버 도구가 없을 때는 각 에이전트가 문서로 정한 경로를 따른다. 고정 도구 `mcp_call`(`mcp-call`), `action` 열거값에 `call`이 있는 관리 도구 `mcp`(`mcp-action`), 프롬프트가 지연 도구로 적은 `mcp__<서버>__bench_echo`를 `ToolSearch`의 `select:<이름>`으로 먼저 불러오는 경로(`tool-search`), omp처럼 프롬프트에 `xd://...bench_echo` 라우트만 적는 도구는 그 라우트를 `write` 도구로 부르는 경로(`mcp-route`)다.
- 도구를 불렀는데 센티널이나 nonce가 돌아오지 않으면 마지막 도구 결과의 끝 400자가 실행 기록의 `result_excerpt`에 남는다. 샌드박스 거부나 셸 오류를 다시 돌리지 않고 확인하기 위한 값이다.
- 왕복은 가정하지 않는다. 스킬은 `SKILL.md` 본문의 `BENCH-SKILL-SENTINEL`을, MCP는 모델이 넘긴 nonce를 나중 요청에서 봐야 `tool_round_trip`이 `true`가 된다. 도구를 부르지 않고 `mock-ok`만 돌아온 실행은 `false`로 남는다.
- 두 시나리오 모두 프로세스 시작부터 마지막 응답까지를 잰다. 그래서 스킬 탐색, MCP 서버 시작, 도구 등록이 숫자에 들어간다. `mcp_ready_ms`는 그중 서버 준비 구간만 따로 보여 준다.

픽스처는 저장소에 들어 있다.

- `scripts/perf/fixtures/bench_skill/SKILL.md`: frontmatter `name: bench-skill`과 본문 센티널 한 줄.
- `scripts/perf/fixtures/bench_mcp_server.py`: 의존성 없는 stdio JSON-RPC 서버. `initialize`, `tools/list`(도구 `bench_echo`), `tools/call`(받은 `text`를 그대로 돌려줌)에 답하고, 모르는 메서드는 `-32601`, 알 수 없는 도구는 `-32602`로 거절하며, 표준 입력이 닫히면 종료 코드 0으로 끝난다. 각 에이전트가 자기 설정으로 직접 띄우며 하네스가 대신 띄우지 않는다.

도구마다 설정 표면이 달라 하네스가 다음처럼 배치한다. 실제로 어떤 경로를 탔는지는 보고서의 `Via` 열이 말한다.

| 도구 | 스킬 픽스처 위치와 호출 | MCP 설정 |
| --- | --- | --- |
| `horon` | 작업 폴더 `.horon-agent/skills/bench-skill/SKILL.md`를 `read` 도구로 읽음(`file-read`) | `<HORON_AGENT_HOME>/mcp.json`의 `servers`. 도구 목록이 캐시 때문에 고정이라 서버 도구는 고정 도구 `mcp_call`(`server: bench`, `tool: bench_echo`)로 부른다(`mcp-call`). 이 시나리오만 `HORON_AGENT_RUN_MCP=1` |
| `jcode` | `<JCODE_HOME>/skills`의 픽스처를 `skill_manage`로 호출(`skill-tool`) | `<JCODE_HOME>/mcp.json`과 작업 폴더 `.jcode/mcp.json`의 `mcpServers`. OpenAI 호환 제공자에는 MCP 도구를 목록에 넣지 않고(`[tools] mcp_tools = "auto"`의 기본 동작) `mcp` 도구의 `action: "call"`, `server: bench`, `tool: bench_echo`로 부른다(`mcp-action`) |
| `claude_code` | `CLAUDE_CONFIG_DIR/skills`의 픽스처를 `Skill` 도구로 호출(`skill-tool`) | `--mcp-config <파일> --strict-mcp-config`. `--mcp-config` 서버는 기본으로 백그라운드에서 연결되어 첫 요청이 도구를 모른 채 나가므로, 문서가 안내하는 `MCP_CONNECTION_NONBLOCKING=0`으로 시작 때 서버를 기다린다. 도구 검색은 기본값 그대로라 프롬프트가 `mcp__bench__bench_echo`를 지연 도구로 적고, 가짜 모델은 `ToolSearch`(`select:mcp__bench__bench_echo`)로 스키마를 불러온 뒤 도구를 부른다. 그래서 모델 요청이 하나 더 든다 |
| `codex` | 작업 폴더 `.agents/skills`의 픽스처를 셸 도구로 `cat`(`file-read`). Codex는 이 위치를 프로젝트 스킬로 찾아 프롬프트에 올린다. 샌드박스는 끄지 않고 러너에서 동작하게 맞춘다. Linux 러너는 Codex 문서의 Ubuntu 24.04 절차(bubblewrap 설치, AppArmor `bwrap-userns-restrict` 적재)를 먼저 밟는다. 그러지 않으면 명령마다 `bwrap: loopback: Failed RTM_NEWADDR: Operation not permitted`로 실패한다. Windows는 모든 Codex 실행에 `-c windows.sandbox="unelevated"`를 준다. 지정이 없으면 새 러너에는 준비된 샌드박스가 없어 `-s read-only` 명령이 `rejected: blocked by policy`로 거부되고, `unelevated`는 관리자 설정 없이 쓰도록 문서가 안내하는 방식이다. 또 Windows의 측정 폴더는 `tempfile.mkdtemp`가 아니라 `os.mkdir`로 만들어 `%TEMP%`의 ACL을 물려받는다. `mkdtemp`의 0o700은 CPython 3.12.4 이상에서 SYSTEM, Administrators, 소유자만 허용하는 보호 ACL이 되고, 관리자 러너에서는 소유자가 Administrators 그룹이라 관리자 권한을 뺀 샌드박스 토큰이 그 아래를 `Access to the path ... is denied`로 읽지 못한다 | `$CODEX_HOME/config.toml`의 `[mcp_servers.bench]`와 `tools.bench_echo.approval_mode = "approve"`. 서버는 네임스페이스 도구 `mcp__bench`로 보이고, `name: bench_echo`, `namespace: mcp__bench`로 부른다(`mcp-tool`) |
| `opencode` | `XDG_CONFIG_HOME/opencode/skills`의 픽스처를 `skill` 도구로 호출(`skill-tool`) | `opencode.json`의 `mcp.bench`(`type: local`) |
| `pi` | 프롬프트의 스킬 목록과 `read` 도구로 `SKILL.md`를 읽음(`file-read`) | `$PI_CODING_AGENT_DIR/mcp.json`의 `mcpServers`, `"exposure": "direct"`. `mcp__bench__bench_echo`를 바로 부른다(`mcp-tool`) |
| `omp` | 에이전트 디렉터리 `<agent>/skills`의 픽스처를 `read`로 읽음(`file-read`) | `<agent>/mcp.json`의 `mcpServers`. 프롬프트의 `xd://...bench_echo` 라우트를 `write`로 호출(`mcp-route`) |
| `omo` | 에이전트 디렉터리 `<agent>/skills`의 픽스처를 `read`로 읽음(`file-read`) | `<agent>/mcp.json`의 `mcpServers`, 함수 `mcp_bench_bench_echo`. `--tools`는 허용 목록이고 MCP 도구 이름이 빌드마다 달라질 수 있어 이 시나리오만 `--tools`를 주지 않고 기본 도구 집합(연결된 서버 도구 포함)을 쓴다 |

시나리오마다 도구 루트 아래 `<시나리오>/run<N>`에 별도 홈과 작업 폴더를 만든다. 같은 홈을 다시 쓰면 앞 실행이 남긴 MCP 스키마 캐시나 스킬 힌트가 준비 비용을 건너뛰게 된다. 스킬과 MCP 픽스처도 해당 시나리오의 홈에만 두므로 다른 시나리오의 숫자에 섞이지 않는다. 실행이 끝나면 그 루트는 통째로 지운다.

### 측정의 한계

- 가짜 모델이므로 실제 모델 지연과 토큰 비용은 들어가지 않는다. 재는 것은 에이전트 자체의 기동, 메모리, 도구 왕복 비용이다.
- 호스티드 러너의 절대값은 기계마다 달라 기준선이 아니다. 같은 실행·같은 OS 안의 상대 비교와 추세만 읽는다.
- OS마다 메모리 지표가 다르다. Linux PSS, macOS RSS, Windows Working Set을 같은 수치로 비교하지 않는다.
- Windows의 bash 도구는 `cmd.exe`로 돈다. `bash (true)`는 `exit 0`, `bash (ls fixture tree)`는 `dir /b tree`로 같은 일을 시킨다.
- 도구 지연은 debug 소켓 왕복과 `Agent::execute_tool` 시간이다. 모델 턴, 승인, 프롬프트 조립은 들어가지 않는다.
- reload storm을 한 레벨에서 반복하면 앞서 연 세션이 데몬에 남아 두 번째가 느려진다. 절대값은 `--iterations 1`로 본다.
- 컴파일 벤치는 mtime으로 cargo fingerprint를 깨뜨린다. 해시 기반 fingerprint에서는 변경 없음과 같은 값이 나온다.
- macOS 임시 폴더 경로는 길어서 Unix 소켓 경로 104바이트 상한을 넘길 수 있다. 스크립트는 `/tmp` 아래 짧은 폴더를 쓴다.

### 새 측정 스크립트의 계약

`--json-out`과 `--markdown-out`을 받는다. 측정 실패는 0이 아니라 `null`과 `error`로 남긴다. 도구나 바이너리가 없으면 `skipped`에 사유를 적고 종료 코드 0으로 끝난다. 통계는 중앙값, p95, 최소, 최대, 표본 수다. 데몬 연결·프로세스 종료·메모리는 `bench_common.py`의 `ipc_connect`, `new_group_kwargs`, `terminate_pgroup`, `process_tree_rss_bytes`를 써서 세 OS에서 같은 코드로 돈다.

## 구조 게이트

`.github/workflows/structural-perf.yml`이 소유한다. `dev`·`main` PR과 push, 수동 실행에서 뜬다. 릴리스 빌드, 시크릿, 외부 네트워크 없이 디버그 프로필의 `cargo test`만 쓴다.

게이트는 "빠른가"가 아니라 "모양이 여전히 유계인가"를 묻는다. 그래서 시간을 읽지 않고 카운터만 읽는다. 지연이나 메모리는 기계마다 달라서, 경계로 두면 정상 변경이 실패하거나 진짜 회귀가 통과한다. 카운터는 어느 기계에서 재도 같다.

| 항목 | 카운터 | 경계 |
| --- | --- | --- |
| 큐 깊이와 버퍼 | `queue_depth_high_water`, `queue_bytes_high_water`, `queue_bytes_buffered` | 큐 깊이 이하, 상한 이하, 커밋 뒤 0 |
| 대기 요청 | `pending_requests`, `pending_requests_high_water` | 요청 중 1 이상, 응답 뒤 0, 상한 이하 |
| 페이지 범위 | `history_page_replayed`, `history_page_flattened`, `history_index_builds`, `warm_read_bytes` | 데운 페이지는 재생 0, 색인 재구축 0, 페이지 크기만큼만 평탄화, 로그의 1/10 이하만 읽기 |
| 스트리밍 해시 버퍼 | `large_request`, `small_request`, `buffer_bytes` | 세 값이 같고 `HASH_STREAM_BUFFER` 이하 |
| NDJSON 프레임 상한 | `max_frame_bytes`, `accepted_frame_bytes`, `rejected_frame_bytes` | 상한은 예산 값과 같고, 수용은 상한 이하, 거절은 상한 초과 |
| 자원 반납 | `fd_retention`, `thread_retention`, `non_returning` | 20사이클 소크 뒤 예산 이하, 반납 실패 0 |
| IRC 메일박스 | `mailbox_high_water`, `mailbox_dropped`, `broadcast_body_allocations`, `waiters_live_after_churn`, `broadcast_inflight_high_water` | 상한 100 이하, 1000건 유입에 드롭 900, 브로드캐스트당 본문 할당 1회, 정리 뒤 대기자 0, 동시 브로드캐스트 16 이하 |
| 스킬 목록과 작업 폴더 스킬 | `skill_catalog_file_reads`, `skill_catalog_bytes`, `skill_warmup_file_reads`, `skill_unchanged_request_file_reads`, `skill_unchanged_request_dir_reads` | 스킬 200개 목록에서 스킬당 파일 읽기 1회 이하·8 KiB 이하(본문 32 KiB를 통째로 읽으면 실패), 바뀌지 않은 작업 폴더 요청 9회의 파일·폴더 읽기 0, 예열 읽기 1 이상 |
| MCP 사용자 설정 | `mcp_config_warmup_reads`, `mcp_config_user_rereads` | 예열 읽기 1 이상, 예열 뒤 바뀌지 않은 사용자 설정을 다시 읽는 횟수 0 |

각 값은 저장소의 기존 테스트가 `STRUCTURAL_PERF section=<이름> k=v ...` 한 줄로 인쇄하고, 게이트는 그 줄을 읽는다. 카운터가 프로세스 전역이므로 측정마다 `--test-threads=1 --exact`로 테스트 하나만 돌린다.

```bash
python3 scripts/perf/check_structural.py                 # 전체
python3 scripts/perf/check_structural.py --only page     # queue, pending, page, hash, frame, soak, context, irc, skill, mcp_config
python3 scripts/perf/check_structural.py --print-plan    # 실행할 cargo 명령만 보기
python3 -m unittest -v scripts/perf/test_check_structural.py   # 게이트의 계약 테스트
```

IRC 처리량은 시간 예산이 아니라 별도 예제로 잰다. 메시지 수는 `IRC_THROUGHPUT_MESSAGES`(기본 100000)로 바꾼다.

```bash
cargo run --locked --release -p horon-agent-irc-core --example irc_throughput
```

**경계를 올릴 때.** 경계는 `scripts/perf/structural_budget.json`에 커밋돼 있다. 회귀가 자기 상한을 스스로 올리지 못하게 하려는 것이다. 올릴 때는 이유를 `notes`에 적고 별도 커밋으로 한다. 계약 테스트가 `notes`를 확인한다.

## A/B 측정

구조 게이트는 "유계인가"까지만 답한다. "얼마나 줄였는가"는 같은 기계에서 두 바이너리를 번갈아 재서 답한다.

**1. 같은 프로필로 두 바이너리를 만든다.** 비교 기준(B0, 보통 변경 직전 `dev`)은 별도 워크트리에서 빌드한다. 프로필이나 기능 집합이 다른 쌍은 실행기가 거절한다.

```bash
cargo build --release -p horon-agent-base --example perf_counters_surface
git worktree add ../agent-b0 <baseline-commit>
cd ../agent-b0 && cargo build --release -p horon-agent-base --example perf_counters_surface
```

**2. 번갈아 잰다.**

```bash
python3 scripts/perf/ab_perf.py \
  --baseline ../agent-b0/target/release/examples/perf_counters_surface \
  --candidate target/release/examples/perf_counters_surface \
  --work-id "11 - seek history pages instead of replaying the session" \
  --workload '["{binary}"]' --warmup 3 --iterations 10 --require-counters \
  --correctness-cmd "cargo test --locked -p horon-agent-harness-api-server" \
  --rollback-commit <baseline-commit> --emit .omo/evidence/ab-report.json
```

- 같은 바이너리를 양쪽에 넣으면 대조 실행(A/A)이 된다. 차이가 잡음 대역 안이면 `control-no-change`, 밖이면 `control-drift`로 기록한다. 두 바이너리가 같으므로 대역 밖 차이는 실행기 잡음이고, 대조 실행은 어느 쪽이든 `adopt-pending-correctness`나 `reject-regression`을 내지 않는다. 이것이 "거짓 개선 없음"의 증거다.
- p99는 표본 20개 이상일 때만 낸다. 모자라면 키를 남기고 `null`과 이유를 적는다.
- 워크로드가 보고하지 않은 지표는 0이 아니라 `null`이 되고 `counts.missing`에 오른다.

**3. 리포트를 채운다.** 결과는 `scripts/perf/report_template.json` 모양으로 적는다. 필수 키는 계약 테스트가 확인한다.

| 항목 | 키 |
| --- | --- |
| 작업, 커밋 | `work_id`, `baseline.git_hash`, `candidate.git_hash`, `rollback_commit` |
| 환경 | `machine.machine`, `machine.system`, `toolchain.rustc`, `baseline.profile`, `baseline.features`, `fixtures[].sha256` |
| 반복 | `iterations.measured_per_side`, `iterations.warmup`, `counts.failure`, `counts.timeout`, `counts.missing` |
| 시간 | `timing.baseline.median_ms`, `p95_ms`, `p99_ms` |
| 자원 | `resources.baseline.cpu_user_ms`, `cpu_system_ms`, `read_bytes`, `write_bytes`, `allocated_peak_bytes`, `retained_bytes`, `rss_bytes`, `pss_bytes`, `queue_depth_high_water`, `fd_count`, `thread_count`, `task_high_water` |
| 판단 | `correctness.result`, `interpretation`, `adoption_decision` |

`allocated_peak_bytes`와 `task_high_water`는 실행기가 아니라 소크 리포트에서 채운다. 측정 JSON은 `.omo/` 아래에 두고 커밋하지 않는다. 판단에 필요한 수치는 PR 본문과 `RESULTS.md`에 옮긴다.

### 스킬·MCP 설정 로딩 A/B

로딩 경로는 `loading_probe` 예제와 `scripts/perf/bench_loading.py`로 잰다. 픽스처의 `config.toml`이 `[mcp] claude_user = true`를 켜므로 두 바이너리가 같은 `~/.claude.json`을 읽는 조건에서 비교한다. `warm` 모드가 A/B용이고, `--compare`는 `cold` 리포트나 모드가 섞인 쌍을 거절한다.

```bash
cargo build --release --locked -p horon-agent-base --example loading_probe
python3 scripts/perf/bench_loading.py --binary target/release/examples/loading_probe --mode warm \
  --events 0 --skills 0,200,1000 --skill-body-bytes 8192 --mcp-config-bytes 300000 \
  --warmup 3 --iterations 30 --emit candidate-warm.json
python3 scripts/perf/bench_loading.py --binary target/release/examples/loading_probe --mode warm \
  --events 0 --skills 0,200,1000 --skill-body-bytes 0 --mcp-config-bytes 0 \
  --warmup 3 --iterations 30 --emit candidate-warm-small.json
python3 scripts/perf/bench_loading.py --compare baseline-warm.json candidate-warm.json
```

기준선은 같은 명령을 기준 커밋의 워크트리에서 빌드한 `loading_probe`로 돌린 결과다. 판정은 카운터가 먼저다. `skill_catalog`의 `skill_bytes_read`, `skill_request_overlay`의 `skill_file_reads`, `mcp_config_load`의 `mcp_config_reads`가 기준선보다 늘면 실패다. 시간은 같은 쌍을 번갈아 다시 재도 기준선의 p95 범위를 넘을 때만 후퇴로 본다.

판정용 측정은 개발 기기가 아니라 `.github/workflows/loading-ab.yml`에서 한다. 이 워크플로는 `ubuntu-24.04` 잡 하나 안에서 기준 커밋과 후보 커밋을 각각 체크아웃해 릴리스로 빌드하고, 위 두 조건(본문 8 KiB와 `~/.claude.json` 300 KB, 둘 다 0)을 후보 쪽 `bench_loading.py`로 번갈아 잰 뒤 `--compare`한다. 이어 "Check counters" 단계가 세 카운터 조건을 확인한다. 본문이 있는 스킬 목록은 기준선보다 적게 읽고, 작업 폴더 스킬은 반복 요청에서 다시 읽지 않고, `~/.claude.json`이 있는 조건의 MCP 설정은 기준선보다 적게 읽어야 한다. 하나라도 어기면 잡이 실패한다. `bench_tools.py`도 두 바이너리로 돌리고, 결과는 `loading-ab-<run_id>` 아티팩트로 30일 남는다.

```bash
gh workflow run loading-ab.yml --ref dev -f base_ref=23007c9b -f head_ref=dev
```

`base_ref` 기본값 `23007c9b`는 로딩 카운터가 처음 들어간 커밋이다. 그보다 오래된 커밋은 리포트 `schema_version`이 달라 `--compare`가 비교를 거절하고 잡이 실패한다. `workflow_dispatch` 워크플로이므로 파일이 기본 브랜치(`main`)에 있어야 실행할 수 있다.

## 하지 않는 것

- 지연이나 메모리에 수치 임계값을 두지 않는다. B0 분포가 고정된 뒤에 따로 정한다.
- 릴리스 바이너리나 계정을 게이트의 조건으로 두지 않는다.
- `scripts/test/fast.sh`의 시작 예산과 기존 벤치마크 스크립트 게이트를 다시 만들지 않는다.
- 페이지 캐시를 버리지 않고, 표본이 모자란 p99를 주장하지 않으며, OS가 다른 메모리 지표를 같은 수치로 비교하지 않는다.

## 부록: jcode 벤치마크와의 대응

jcode(`1jehuang/jcode`, 2026-11-05 커밋 `c725fa3`)의 `scripts/` 벤치마크를 다음과 같이 판정했다.

| jcode | horon | 판정 |
| --- | --- | --- |
| `benchmark_tools.sh` | `bench_tools.py` | 이식. socat 없이 Python으로 소켓에 붙는다. |
| `bench_reload_storm.py` | `bench_reload_storm.py` | 이식. 세션은 `fixtures/gen_event_log.py`로 만든다. |
| `bench_compile.sh`, `bench_selfdev_checkpoints.sh`, `bench_selfdev_build.sh` | `bench_compile.py` | 이식. `cargo clean` 콜드 모드는 CI 캐시를 지워 뺐다. |
| `bench_headless_memory.py`, `bench_memory_cli.py`, `bench_jcode_maki.py` | `bench_compare.py` | 경쟁 도구 비교로 이식. 헤드리스 경로로 잰다. |
| `bench_startup.py`, `check_startup_budget.sh`, `memory_regression_gate.sh`, `analyze_runtime_memory_log.py` | `bench_startup.py`, `check_startup.sh`, `check_memory.sh`, `analyze_memory_log.py` | 이미 있음 |
| `jcode_memory_snapshot.py`, `repro_idle_session_rss.py` | `bench_common.py`, `bench_reload_storm.py` | 같은 축을 다른 스크립트가 잰다. |
| TUI 화면 측정(`bench_startup_visible_ready.py`, `bench_resume_picker.py`, `startup_frames.sh` 등) | 없음 | 제외. horon은 헤드리스다. |
| `bench_resume_suite.py` | 없음 | 제외. 재개 지연은 reload storm이 잰다. |
| 실제 모델·유료 API·외부 하네스가 필요한 것(`benchmark_swarm.py`, `run_terminal_bench_*`, `benchmark_discovery*.py`, `benchmark_browser_handoff.*` 등) | 없음 | 제외 |
| `analyze_root_crate.py`, `analyze_openai_ws_cache.py` | 없음 | 제외. 해당 로그 줄이 없다. |
