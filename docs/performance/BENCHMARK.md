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

한 구역만 돌린다. 구역 이름은 `queue`, `pending`, `page`, `hash`, `frame`, `soak` 이다.

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

### 하지 않는 것

- 릴리스 바이너리나 계정을 게이트의 조건으로 두지 않는다.
- 지연이나 RSS 에 수치 임계값을 두지 않는다. B0 분포가 고정된 뒤에 별도로 정한다.
- `scripts/test/fast.sh` 의 시작 예산과 기존 벤치마크 스크립트 게이트를 다시 만들지 않는다. 그 둘은 그대로 자기 자리에서 돈다.
- 페이지 캐시를 버리지 않고, 표본이 모자란 p99 를 주장하지 않으며, 맥의 RSS 를 리눅스의 PSS 와 같은 수치로 비교하지 않는다.
