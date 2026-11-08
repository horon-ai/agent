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
| `build` | OS마다 1개 | `horon-agent`와 `loading_probe` release 바이너리를 한 번 만들어 `bench-bin-<os>-<run_id>` 아티팩트(tar, 1일 보관)로 올린다. `baseline_ref`가 있고 `compare`를 고르면 그 커밋도 따로 빌드해 같은 tar의 `baseline/`에 넣는다. |
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
```

| 입력 | 기본값 | 뜻 |
| --- | --- | --- |
| `os` | `all` | `linux`, `macos`, `windows` 중 쉼표로 고른다. 고른 OS마다 위 작업이 뜬다. |
| `suites` | `all` | `compare`, `startup`, `loading`, `tools`, `reload`, `compile` 중 쉼표로 고른다. |
| `competitors` | `jcode,claude_code,codex,opencode,pi` | horon과 비교할 도구. 설치에 실패한 도구는 `skipped`로 남는다. |
| `sessions` | `1 5 10` | 세션 메모리 비교의 동시 세션 수 |
| `turns` | `3` | 세션마다 보내는 턴 수 |
| `oneshot_runs` | `3` | one-shot 프롬프트를 빌드마다 몇 번 잴지. 두 빌드의 중앙값을 가려야 하면 20 정도로 올린다 |
| `baseline_ref` | 빈 값 | 지정하면 그 커밋을 같은 작업 안에서 따로 빌드해 `horon-base` 줄로 번갈아 잰다. 같은 러너의 전후 비교가 된다 |

모르는 OS나 묶음 이름이면 실행이 바로 실패한다(`scripts/perf/bench_select.py`). 결과는 OS별 `merge` 작업 요약과 `benchmark-<os>-<run_id>` 아티팩트(JSON, 마크다운)로 30일 남는다. 중간 아티팩트 이름은 `benchmark-`로 시작하지 않으므로 `benchmark-docs.yml`의 다운로드에 섞이지 않는다.

시크릿과 API 요금은 없다. 모델이 필요한 측정은 모두 `scripts/perf/mock_llm_server.py`(로컬 가짜 모델 서버)에 붙는다.

### 묶음

| 묶음 | 스크립트 | 재는 것 |
| --- | --- | --- |
| `compare` | `bench_compare.py`, `install_competitors.sh` | horon과 경쟁 도구의 `--version` 시간, 질문 1건 처리 시간과 최대 메모리, 동시 세션 수에 따른 메모리, 설치 크기. Linux는 `fsync` 호출 수도 잰다. |
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
bash scripts/perf/install_competitors.sh /tmp/bench-tools jcode,claude_code,codex,opencode,pi
python3 scripts/perf/bench_compare.py --bin horon=./target/release/horon-agent $(cat /tmp/bench-tools/bins.args) \
  --sessions 1 5 10 --turns 3 --json-out compare.json --markdown-out compare.md
```

`install_competitors.sh`는 버전을 고정해 설치한다. jcode는 GitHub 릴리스 `v0.99.1`(Windows는 `.exe`), 나머지는 `npm install --prefix`로 `@anthropic-ai/claude-code@2.1.317`, `@openai/codex@0.166.0`, `opencode-ai@1.19.5`, `@mariozechner/pi-coding-agent@0.73.1`이다. 버전은 `JCODE_VERSION`, `CLAUDE_VERSION`, `CODEX_VERSION`, `OPENCODE_VERSION`, `PI_VERSION`으로 바꾼다. Windows에서는 Git Bash로 돌리고, npm 도구는 `.cmd` 실행 파일로 기록한다.

### 경쟁 도구 비교의 지표

| 지표 | 뜻 |
| --- | --- |
| `version_ms` | `--version` 실행 N회(기본 5)의 통계 |
| `oneshot_ms` | 헤드리스 질문 1건(기동, `ls` 도구 1회, 최종 응답)의 시간, 그동안의 최대 프로세스 트리 메모리, 도구 왕복 여부 |
| `fsync_calls` | `--syscalls`를 켰을 때 질문 1건이 부르는 `fsync`·`fdatasync` 수. Linux에서 `strace`가 있을 때만 재고, 없으면 `skipped`에 사유가 남는다 |
| `sessions_memory` | N개 동시 세션 × K턴 뒤 idle과 턴 직후의 프로세스 트리 메모리. 첫 세션 값과 세션당 증가분(최소제곱 기울기)을 낸다 |
| `install_size_bytes` | 실행 파일 크기. npm 도구는 패키지와 설치된 의존성의 합 |

배수는 horon을 1.0×로 둔다. 도구마다 가짜 모델에 붙는 방법이 다르다. horon과 jcode는 임시 HOME의 `[providers.mock]`과 데몬 하나의 debug 소켓 세션, Claude Code는 `ANTHROPIC_BASE_URL`과 상주 `stream-json` 프로세스, Codex는 `model_providers`와 `app-server`, OpenCode는 `opencode.json`의 `baseURL`과 `serve`, pi는 `models.json`과 `--mode rpc`를 쓴다.

세션 구현이 도구마다 다르므로(데몬 안의 세션과 세션마다 뜨는 프로세스) 절대값보다 세션당 증가분을 비교한다. `strace`를 켠 실행의 시간은 느려지므로 시간 비교에 쓰지 않는다.

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

- 같은 바이너리를 양쪽에 넣으면 대조 실행(A/A)이 된다. 차이가 잡음 대역 안이면 `control-no-change`로 기록한다. 이것이 "거짓 개선 없음"의 증거다.
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
