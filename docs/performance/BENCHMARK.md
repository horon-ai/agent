# 성능 측정

성능 회귀를 막는 구조 게이트와 A/B 측정을 다룬다.

## 성능: 구조 게이트와 A/B 측정

이 문서는 성능 회귀를 막는 두 장치를 다룬다. 하나는 모든 PR에서 도는 **구조 게이트**이고, 다른 하나는 같은 기계에서 두 바이너리를 비교하는 **A/B 측정**과 그 결과를 적는 **리포트 템플릿**이다.

구조 게이트는 "빠른가"를 묻지 않는다. "모양이 여전히 유계인가"를 묻는다. 큐가 유입량을 따라 무한히 자라지 않는지, 이력 페이지가 세션 전체를 다시 읽지 않는지, 해시 버퍼가 로그 크기를 따라 커지지 않는지, NDJSON 프레임 리더가 자기 상한을 넘겨 받아들이지 않는지, attach/detach 를 반복해도 파일 서술자와 스레드가 기준선으로 돌아오는지를 본다.

### 왜 시간이 아니라 카운터인가

B0 분포를 고정하기 전에는 지연이나 RSS 임계값을 막는 기준으로 삼을 수 없다. 같은 코드도 기계와 부하에 따라 수치가 달라지고, 그 수치를 경계로 두면 정상 변경이 빨간불이 되거나 진짜 회귀가 초록불이 된다. 카운터는 기계를 타지 않는다. `queue_depth_high_water` 가 64 인지 1000 인지는 어느 노트북에서 재도 같다.

그래서 게이트는 벽시계 시간을 한 번도 읽지 않는다.

### 게이트가 지키는 경계

| 항목 | 카운터 | 경계 |
| --- | --- | --- |
| 큐 깊이와 버퍼 | `queue_depth_high_water`, `queue_bytes_high_water`, `queue_bytes_buffered` | 민 깊이 이하, 상한 이하, 커밋 뒤 0 |
| 대기 요청 | `pending_requests`, `pending_requests_high_water` | 요청 중 1 이상, 응답 뒤 0, 상한 이하 |
| 페이지 수화 범위 | `history_page_replayed`, `history_page_flattened`, `history_index_builds`, `warm_read_bytes` | 데운 페이지는 재생 0, 색인 재구축 0, 페이지 크기만큼만 평탄화, 로그의 1/10 이하만 읽기 |
| 스트리밍 해시 버퍼 | `large_request`, `small_request`, `buffer_bytes` | 세 값이 모두 같고 `HASH_STREAM_BUFFER` 이하 |
| NDJSON 프레임 상한 | `max_frame_bytes`, `accepted_frame_bytes`, `rejected_frame_bytes` | 상한은 예산 값과 같고, 수용 크기는 상한 이하, 거절 크기는 상한 초과이면서 수용 크기의 다음 바이트 |
| 자원 반납 | `fd_retention`, `thread_retention`, `non_returning` | 짧은 소크(20 사이클) 뒤 각각 예산 이하, 반납 실패 0 |
| IRC 메일박스 | `mailbox_high_water`, `mailbox_dropped`, `broadcast_body_allocations`, `waiters_live_after_churn`, `broadcast_inflight_high_water` | 상한 100 이하, 1000건 유입에서 드롭 900, 브로드캐스트 1회당 본문 할당 1회, 타임아웃·취소 뒤 대기자 0, 동시 브로드캐스트 16 이하 |

각 측정은 저장소의 기존 테스트가 직접 인쇄한다. 테스트는 `STRUCTURAL_PERF section=<이름> k=v k=v` 한 줄을 남기고, 게이트는 그 줄을 읽는다. 게이트가 따로 재측정하지 않는 이유는, 같은 경계를 두 구현이 각자 판정하면 둘이 갈라지기 때문이다.

### CI에서 도는 방식

`.github/workflows/structural-perf.yml` 이 이 게이트를 소유한다. `dev`, `main` 으로 가는 PR과 push, 그리고 수동 실행에서 뜬다.

`ci.yml` 도 같은 브랜치에서 돌지만 이 게이트는 자기 `on:` 블록을 가진 별도 파일로 둔다. 게이트만 따로 다시 돌리거나 수동으로 실행할 수 있고, `ci.yml` 의 빌드·테스트 행렬과 실패가 섞이지 않는다.

이 작업은 릴리스 바이너리를 요구하지 않고, 체크아웃과 툴체인 설치 밖의 네트워크를 쓰지 않으며, 시크릿과 계정을 쓰지 않는다. 측정은 전부 디버그 프로필의 `cargo test` 다.

### 로컬에서 돌리기

전체 게이트를 돌린다.

```bash
python3 scripts/perf/check_structural.py
```

한 구역만 돌린다. 구역 이름은 `queue`, `pending`, `page`, `hash`, `frame`, `soak`, `context`, `irc` 이다.

```bash
python3 scripts/perf/check_structural.py --only page
```

어떤 카고 명령이 실행될지 먼저 본다.

```bash
python3 scripts/perf/check_structural.py --print-plan
```

게이트의 계약 테스트를 돌린다. 경계 위반이 실제로 실패하는지, 측정이 없을 때 통과로 새지 않는지, 리포트 템플릿이 필수 키를 다 갖췄는지를 여기서 확인한다.

```bash
python3 -m unittest -v scripts/perf/test_check_structural.py
```

테스트는 한 번에 하나씩 돈다. 카운터는 프로세스 전역이고 `reset()` 은 그 프로세스에서 다른 테스트가 기록하지 않을 때만 뜻이 있으므로, 게이트는 측정마다 `--test-threads=1 --exact` 로 테스트 하나만 실행한다.

측정은 카고의 디버그 프로필을 쓴다. 릴리스 빌드는 필요 없다.

`irc` 섹션은 메일박스 상한, 넘칠 때 버린 수, 브로드캐스트 본문 할당, 대기자 정리, 동시 브로드캐스트 상한을 본다. 처리량은 시간 예산이 아니라 별도 예제로 잰다. 예제는 대기자에게 직접 전달하는 경로, 즉시 주입을 돌려주는 가짜 전달 경로, 64개 대상 브로드캐스트 세 시나리오를 릴리스 프로필에서 돌고 `IRC_THROUGHPUT scenario=… messages=… elapsed_us=… msgs_per_sec=…` 형식으로 한 줄씩 인쇄한다.

```bash
cargo run --locked --release -p horon-agent-irc-core --example irc_throughput
```

메시지 수는 환경변수 `IRC_THROUGHPUT_MESSAGES`로 바꾼다. 기본값은 100000이다.

### 경계를 올리는 절차

경계는 `scripts/perf/structural_budget.json` 에 커밋되어 있다. 회귀가 자기 상한을 스스로 올릴 수 없게 하려는 것이고, `scripts/ci/check_code_size.py` 의 래칫과 같은 형태다.

경계를 올릴 때는 그 값과 함께 이유를 `notes` 에 적는다. 계약 테스트가 `notes` 의 존재를 확인하므로, 이유 없는 상향은 통과하지 않는다.

### A/B 쌍 측정

구조 게이트는 "모양이 유계인가"까지만 답한다. "이 변경이 실제로 얼마나 줄였는가"는 같은 기계에서 두 바이너리를 번갈아 재야 한다.

#### 1. 두 바이너리를 같은 프로필로 만든다

워크로드는 카운터와 프로세스 수치를 JSON 으로 인쇄해야 한다. 실행기는 워크로드가 인쇄한 `counters` 와 `process` 객체에서 자원 수치를 읽으므로, 그 표면을 그대로 인쇄하는 예제를 양쪽에 만든다.

```bash
cargo build --release -p horon-agent-base --example perf_counters_surface
```

B0 쪽은 비교 기준 커밋(보통 변경 직전의 `dev`)으로 워크트리를 따로 만든다. `<baseline-commit>` 에 그 커밋을 넣는다.

```bash
git worktree add ../agent-b0 <baseline-commit>
```

```bash
cd ../agent-b0 && cargo build --release -p horon-agent-base --example perf_counters_surface
```

프로필이나 기능 집합이 다르면 그 쌍은 변경이 아니라 빌드를 재는 것이므로, 실행기는 그런 쌍을 시간을 재기 전에 거절한다.

#### 2. 두 쪽을 번갈아 잰다

```bash
python3 scripts/perf/ab_perf.py \
  --baseline ../agent-b0/target/release/examples/perf_counters_surface \
  --candidate target/release/examples/perf_counters_surface \
  --work-id "11 - seek history pages instead of replaying the session" \
  --workload '["{binary}"]' \
  --warmup 3 --iterations 10 \
  --require-counters \
  --correctness-cmd "cargo test --locked -p horon-agent-harness-api-server" \
  --rollback-commit <baseline-commit> \
  --emit .omo/evidence/ab-report.json
```

같은 바이너리를 양쪽에 넣으면 대조 실행이 된다. 중앙값 차이가 잡음 대역 안에 있으면 실행기는 개선을 주장하지 않고 `control-no-change` 로 기록한다. 이 대조 실행이 "거짓 개선 없음"의 증거다.

p99 는 표본이 20 개 이상일 때만 나온다. 그 아래에서는 키를 지우지 않고 `null` 과 이유를 남긴다. 워크로드가 보고하지 않은 지표는 0 이 아니라 `null` 이 되고 `counts.missing` 에 이름이 오른다.

#### 3. 리포트를 채운다

측정 결과는 `scripts/perf/report_template.json` 모양으로 적는다. 이 파일은 추적되는 경로에 있고, 계약 테스트가 필수 키의 존재를 직접 확인한다. 사람이 눈으로 세지 않는다.

필수 키는 다음과 같다.

| 항목 | 키 |
| --- | --- |
| 작업 식별자 | `work_id` |
| B0 커밋 | `baseline.git_hash` |
| 후보 커밋 | `candidate.git_hash` |
| 기계와 운영체제 | `machine.machine`, `machine.system` |
| 툴체인 | `toolchain.rustc` |
| 프로필과 기능 | `baseline.profile`, `baseline.features` |
| 픽스처 해시 | `fixtures[].sha256` |
| 반복 수 | `iterations.measured_per_side` |
| 워밍업 | `iterations.warmup` |
| 실패·시간 초과·누락 | `counts.failure`, `counts.timeout`, `counts.missing` |
| 중앙값과 p95 | `timing.baseline.median_ms`, `timing.baseline.p95_ms` |
| p99 | `timing.baseline.p99_ms` (표본이 모자라면 `null`, 키는 남긴다) |
| CPU | `resources.baseline.cpu_user_ms`, `cpu_system_ms` |
| 읽기·쓰기 바이트 | `resources.baseline.read_bytes`, `write_bytes` |
| 할당 최고점 | `resources.baseline.allocated_peak_bytes` |
| 잔존 | `resources.baseline.retained_bytes` |
| RSS 와 PSS | `resources.baseline.rss_bytes`, `pss_bytes` |
| 큐·FD·스레드·태스크 최고점 | `resources.baseline.queue_depth_high_water`, `fd_count`, `thread_count`, `task_high_water` |
| 정합성 스위트 결과 | `correctness.result` |
| 해석 | `interpretation` |
| 채택 결정 | `adoption_decision` |
| 롤백 커밋 | `rollback_commit` |

실행기가 직접 채우지 않는 키가 둘 있다. `allocated_peak_bytes` 와 `task_high_water` 는 소크 리포트(`background_tasks`, 할당자 최고점)에서 온다. 실행기 출력에 없는 값을 0 으로 적지 않고 그 출처에서 채운다.

측정 JSON 은 `.omo/` 아래에 두고 커밋하지 않는다. 검토자가 판단에 필요한 수치는 PR 본문에 그대로 옮긴다.

### 요청형 벤치마크 워크플로

`.github/workflows/benchmark.yml` 은 수동 실행(`workflow_dispatch`)으로만 뜬다. push, PR, 예약 실행에서는 돌지 않는다. 경쟁 도구를 설치하고 릴리스 빌드를 하며 러너 시간을 수십 분 쓰기 때문이다.

```bash
gh workflow run benchmark.yml --ref dev -f suites=all
gh workflow run benchmark.yml --ref dev -f suites=compare,tools -f competitors=claude_code,codex
```

| 입력 | 기본값 | 뜻 |
| --- | --- | --- |
| `suites` | `all` | `compare`, `startup`, `loading`, `tools`, `reload`, `compile` 중 쉼표로 고른다. 모르는 이름이면 실행이 실패한다(`scripts/perf/bench_select.py`). |
| `competitors` | `jcode,claude_code,codex,opencode,pi` | horon 과 비교할 도구. 설치에 실패한 도구는 `skipped` 로 남는다. |
| `sessions` | `1 5 10` | 메모리 비교의 동시 세션 수 |
| `turns` | `3` | 세션마다 보내는 턴 수 |
| `runner` | `ubuntu-latest` | `macos-persistent` 를 고르면 저장소의 상주 macOS 러너에서 돈다. 수치가 덜 흔들린다. |

모든 묶음은 한 작업, 한 기계에서 돌아 비교 행이 같은 CPU·디스크·부하를 공유한다. 모델이 필요한 측정은 `scripts/perf/mock_llm_server.py`(로컬 가짜 모델 서버)에 붙으므로 시크릿과 API 요금이 없다. 결과는 작업 요약(step summary)의 표와 `benchmark-<run_id>` 아티팩트(JSON, 마크다운)로 남고, 임계값으로 실패하지 않는다. 스크립트 자체가 오류를 낼 때만 실패한다.

새 스크립트는 같은 계약을 따른다. `--json-out` 과 `--markdown-out` 을 받고, 측정 실패는 0 이 아니라 `null` 과 `error` 로 남기며, 도구나 바이너리가 없으면 `skipped` 에 사유를 적고 종료 코드 0 으로 끝난다. 통계는 중앙값, p95, 최소, 최대, 표본 수다.

#### 묶음별 스크립트

| 묶음 | 스크립트 | 재는 것 |
| --- | --- | --- |
| `compare` | `bench_compare.py`, `install_competitors.sh` | horon 과 경쟁 도구의 `--version` 시간, 헤드리스 1회 실행 시간과 최대 RSS, 세션 수에 따른 메모리(첫 세션, 세션당 증가분), 설치 크기. horon 대비 배수를 표에 적는다. |
| `startup` | `bench_startup.py` | `--help`·`--version` 로드, `serve` 소켓 준비, 하네스 브리지 단계별 준비 시간 |
| `loading` | `bench_loading.py` | 고정 시드 픽스처의 세션 로딩, COLD 와 WARM |
| `tools` | `bench_tools.py` | debug 소켓 `tool:` 명령으로 core 도구(read, write, edit, ls, agentgrep, bash) 14개 케이스의 지연 |
| `reload` | `bench_reload_storm.py` | 세션 N 개(기본 1, 10, 40)를 동시에 `subscribe` 해서 `history` 프레임이 올 때까지의 지연 |
| `compile` | `bench_compile.py` | `cargo check` 의 warm no-op, leaf(`src/main.rs`), core(`crates/app-core`), build-meta 터치 후 재빌드 시간. mtime 만 올렸다가 되돌린다. `--quick` 은 no-op 만 잰다. |

#### 경쟁 도구 비교

로컬에서는 다음처럼 돌린다. `--bin name=path` 로 도구를 지정하지 않으면 PATH 에서 찾는다.

```bash
bash scripts/perf/install_competitors.sh /tmp/bench-tools jcode,claude_code,codex,opencode,pi
python3 scripts/perf/bench_compare.py --bin horon=./target/release/horon-agent $(cat /tmp/bench-tools/bins.args) \
  --sessions 1 5 10 --turns 3 --json-out compare.json --markdown-out compare.md
```

`install_competitors.sh` 는 jcode 를 GitHub 릴리스(`v0.99.1`)에서, 나머지를 `npm install --prefix` 로 버전을 고정해 설치한다(`@anthropic-ai/claude-code@2.1.317`, `@openai/codex@0.166.0`, `opencode-ai@1.19.5`, `@mariozechner/pi-coding-agent@0.73.1`). 버전은 `JCODE_VERSION`, `CLAUDE_VERSION`, `CODEX_VERSION`, `OPENCODE_VERSION`, `PI_VERSION` 으로 바꾼다.

| 지표 | 뜻 |
| --- | --- |
| `version_ms` | `--version` 실행 N 회(기본 5)의 통계 |
| `oneshot_ms` | 헤드리스 프롬프트 1건(기동, `ls` 도구 1회, 최종 응답)을 끝내는 시간, 그동안의 최대 프로세스 트리 RSS, 도구 왕복 여부(mock `/__stats` 근거) |
| `sessions_memory` | N 개 동시 세션 × K 턴 뒤 idle 과 턴 직후의 프로세스 트리 RSS(Linux 는 PSS 도). 첫 세션 값과 세션당 증가분(최소제곱 기울기)을 낸다. 실패한 측정점은 기울기에서 빼고, 음수 기울기는 0 으로 적는다. |
| `install_size_bytes` | 실행 파일 크기. npm 도구는 패키지와 설치된 의존성(플랫폼별 선택 의존성 포함)의 합 |

배수는 모두 horon 을 1.0× 로 둔다. 도구마다 가짜 모델에 붙는 방법이 다르다. horon 과 jcode 는 임시 HOME 의 `[providers.mock]` 와 데몬 1개의 debug 소켓 세션을 쓴다. Claude Code 는 `ANTHROPIC_BASE_URL` 과 상주 `stream-json` 프로세스를 쓴다. Codex 는 `model_providers` 설정과 `app-server` 를 쓴다. OpenCode 는 `opencode.json` 의 `baseURL` 과 `serve` 를 쓰고, pi 는 `models.json` 과 `--mode rpc` 를 쓴다.

가짜 모델이므로 실제 모델 지연과 토큰 비용은 들어가지 않는다. 이 비교가 재는 것은 에이전트 자체의 기동, 메모리, 도구 왕복 비용이다. 세션의 구현이 도구마다 다르므로(데몬 안의 세션과 세션마다 상주하는 프로세스) 절대값보다 세션당 증가분을 비교한다. OpenCode 는 새 캐시에서 `serve` 의 첫 메시지에 응답하지 않아 `run` 1회로 예열한 뒤 잰다. horon 세션 측정은 `HORON_AGENT_DEBUG_CONTROL=1` 이 필요하다.

#### 측정의 한계

- 도구 지연은 debug 소켓 왕복과 `Agent::execute_tool` 시간이다. 모델 턴, 승인, 프롬프트 조립은 들어가지 않는다. `edit` 케이스는 매번 파일을 원본으로 되돌리므로 쓰기 비용을 함께 잰다.
- reload storm 을 한 레벨에서 반복하면 앞서 재개한 세션이 데몬에 남아 두 번째가 느려진다. 절대값은 `--iterations 1` 로 보고, 레벨 간 누적은 `--reuse-daemon` 으로 본다.
- 컴파일 벤치는 mtime 으로 cargo fingerprint 를 깨뜨린다. 해시 기반 fingerprint 설정에서는 터치가 재빌드를 일으키지 않아 no-op 과 같은 값이 나온다.
- GitHub 호스티드 러너의 절대값은 기계마다 달라 뜻이 없다. 같은 실행 안의 상대 비교와 추세만 본다.
- macOS 의 임시 폴더 경로는 길다. 임시 루트 아래 Unix 소켓 경로가 104바이트를 넘으면 `serve` 가 소켓을 열지 못하므로 스크립트는 짧은 접두사의 임시 폴더를 쓴다.

#### jcode 벤치마크와의 대응

jcode(`1jehuang/jcode`, 2026-11-05 커밋 `c725fa3`)의 `scripts/` 벤치마크를 다음과 같이 판정했다.

| jcode | horon | 판정 |
| --- | --- | --- |
| `benchmark_tools.sh` | `bench_tools.py` | 이식. socat 없이 Python 소켓을 쓴다. |
| `bench_reload_storm.py` | `bench_reload_storm.py` | 이식. 세션은 `fixtures/gen_event_log.py` 로 만든다. |
| `bench_compile.sh`, `bench_selfdev_checkpoints.sh`, `bench_selfdev_build.sh` | `bench_compile.py` | 이식. commit blast 는 `build_meta_touch` 로 잰다. `cargo clean` 콜드 모드는 CI 캐시를 지워 뺐다. |
| `bench_headless_memory.py`, `bench_memory_cli.py`, `bench_jcode_maki.py` | `bench_compare.py` | 경쟁 도구 비교로 이식. TUI 대신 헤드리스 경로로 잰다. |
| `bench_startup.py`, `check_startup_budget.sh`, `memory_regression_gate.sh`, `analyze_runtime_memory_log.py` | `bench_startup.py`, `check_startup.sh`, `check_memory.sh`, `analyze_memory_log.py` | 이미 있음 |
| `jcode_memory_snapshot.py`, `repro_idle_session_rss.py` | `bench_common.py`, `bench_reload_storm.py` | 같은 축을 다른 스크립트가 잰다. |
| `bench_startup_visible_ready.py`, `bench_resume_picker.py`, `memory_probe.sh`, `repro_startup_lag.py`, `startup_acceptance*.sh`, `startup_frames.sh`, 입력·깜빡임 재현기 | 없음 | 제외. TUI 화면을 재는데 horon 은 헤드리스다. |
| `bench_resume_suite.py` | 없음 | 제외. `resume_bench` 마커를 내는 Rust 테스트가 없고, 재개 지연은 reload storm 이 잰다. |
| `benchmark_swarm.py`, `benchmark_takehome.py`, `run_terminal_bench_*`, `audit_terminal_bench_submission.py` | 없음 | 제외. 실제 모델과 유료 API, 외부 하네스가 필요하다. |
| `benchmark_discovery*.py`, `benchmark_attribution.py` | 없음 | 제외. 호스팅 카탈로그와 실제 모델 호출이 필요하다. |
| `benchmark_browser_handoff.*` | 없음 | 제외. 실제 브라우저와 모델 판단이 필요하고 이 빌드에는 browser 도구가 없다. |
| `analyze_root_crate.py`, `analyze_openai_ws_cache.py` | 없음 | 제외. horon 은 이미 크레이트로 나뉘어 있고, 그 로그 줄을 내지 않는다. |

### 하지 않는 것

- 릴리스 바이너리나 계정을 게이트의 조건으로 두지 않는다.
- 지연이나 RSS 에 수치 임계값을 두지 않는다. B0 분포가 고정된 뒤에 별도로 정한다.
- `scripts/test/fast.sh` 의 시작 예산과 기존 벤치마크 스크립트 게이트를 다시 만들지 않는다. 그 둘은 그대로 자기 자리에서 돈다.
- 페이지 캐시를 버리지 않고, 표본이 모자란 p99 를 주장하지 않으며, 맥의 RSS 를 리눅스의 PSS 와 같은 수치로 비교하지 않는다.
