# 성능: 측정 결과와 그 한계

언어: [English](RESULTS.md) | **한국어**

이 문서는 `agent-runtime-performance-2` 계획이 기준점 B0(계획 시작 직전 커밋) 위에서 남긴 측정값을 기록한다. 이 계획은 `dev` 에 성능 커밋 19개를 올렸고, 같은 범위에 이 계획과 무관한 `fix(cli)` 커밋 하나가 더 있지만 아래 수치와는 관계가 없다. 측정 대상은 세션 저장소의 해시와 이력 페이지, SSE 파서, 복제 내보내기, 결과 캐시의 임계값 판정, 그리고 attach/detach 소크다. 측정은 모두 이 기기(Apple M1 Max, 디버그 테스트 프로필)에서 같은 기계의 앞뒤를 번갈아 재는 방식으로 이뤄졌다.

원본 산출물은 전부 `.omo/evidence/agent-runtime-performance-2/` 아래에 있고 그 경로는 `.gitignore:26` 으로 무시된다. 계획은 주요 수치를 PR 본문에 옮겨 적으라고 했지만, 실제로는 대부분의 PR 본문이 한 줄이었다. 그래서 오늘 이 저장소를 클론한 사람은 이 작업의 측정값을 하나도 볼 수 없다. 이 페이지가 그 기록을 추적되는 자리로 옮긴 것이며, 모든 수치는 위 증거 파일에서 그대로 가져왔다.

## 측정 결과

차원 표기는 다음 규칙을 따른다. `MEMORY` 는 할당과 메모리, `TIME` 은 벽시계 시간, `WORK` 는 반복 횟수와 바이트량, `RESOURCES` 는 파일 서술자·스레드·태스크 수다. 한 차원의 개선을 다른 차원의 개선으로 읽지 않기 위해 차원을 따로 적는다.

| 변경 | 차원 | 이전 | 이후 |
| --- | --- | --- | --- |
| 스트리밍 소스 해시, 최고 할당 (200 MiB 로그) | `MEMORY` | 268,435,520 B | 65,600 B (4,092배) |
| 스트리밍 소스 해시, 읽은 바이트 | 변화 없음 | 209,715,200 B | 209,715,200 B |
| 스트리밍 소스 해시, 벽시계 | `TIME` (개선 없음) | 6,434 ms | 6,404 ms |
| 이력 페이지, 평탄화한 메시지 (10만 이벤트, 20개 페이지) | `WORK` | 99,999개 | 20개 |
| 이력 페이지, 읽은 바이트 | `WORK` | 62,169,922 B | 18,554 B |
| 이력 페이지, 벽시계 | `TIME` | 26.2 s | 5.9 ms |
| SSE, 페이로드 12개를 파싱한 횟수 | `WORK` | 24회 | 12회 |
| 복제 내보내기, 직렬화 횟수 (페이지 64 → 256 이벤트) | `WORK` | 129회 | 513회 (이벤트 4배에 직렬화 4.0배) |
| 복제 내보내기, 참조 구현 대비 바이트 (같은 64 → 256) | `WORK` | 8,445,218 → 138,506,722 B (16.4배) | 102,903 → 411,795 B (4.0배) |
| 캐시 facade, 상한 초과 경로 (일시 할당 · 정규 직렬화) | `MEMORY` | 3,146,158 B, 1회 | 0 B, 0회 |
| 캐시 facade, 벽시계 | `TIME` (개선 없음) | MiB당 약 19 ms | MiB당 약 19 ms |
| attach/detach 소크, 200 사이클 뒤 자원 | `RESOURCES` | fd 14, 스레드 25, 태스크 2,141, `shared_context` 0 | fd 14, 스레드 22, 태스크 2,141, `shared_context` 0 |

몇 가지는 표만으로는 오해하기 쉬워서 따로 적는다.

- 소스 해시의 다이제스트는 다섯 픽스처(61 KB, 618 KB, 6.2 MB, 62 MB, 200 MiB)에서 모두 이전과 같다. 바뀐 것은 최고 할당 하나이고, 읽는 바이트와 걸리는 시간은 그대로다.
- 이력 페이지의 `WORK` 와 `TIME` 은 같은 요청의 두 얼굴이다. 10만 이벤트 세션에서 20개짜리 페이지 하나를 받을 때, 이전에는 세션 전체를 재생해 99,999개 메시지를 평탄화하고 62 MB 를 읽었고, 이후에는 색인이 가리키는 20개 레코드만 읽는다.
- 복제 내보내기는 페이지 크기가 4배가 될 때 이전 참조 구현이 바이트를 16.4배로 늘렸고, 지금 구현은 4.0배로 늘린다. 즉 로그 길이가 아니라 페이지 길이에 비례한다. 페이지 바이트는 B0 과 바이트 단위로 같고, 고정 다이제스트 `135cf07d...`(길이 240,882)와 `4a4855d4...`(길이 26,979)가 그대로 재현된다.
- 캐시 facade 는 상한을 넘는지 판단하려고 정규 JSON 을 한 번 직렬화하던 것을 없앴다. 1 MiB 경계는 바이트 단위로 정확하다(1,048,575 B 는 캐시, 1,048,577 B 는 건너뜀). 실제로 캐시하는 경로는 여전히 한 번 직렬화한다.
- 소크는 200 사이클, 10개 창, 588.99초다. 창마다 증가분을 재고, 실패 라운드와 정지 실패(`quiescence_misses`), 반납 실패(`non_returning`)가 모두 비어 있다.

### 같은 페이지를 로그가 커져도 같은 비용으로 받는가

색인이 있는 상태에서 20개짜리 페이지를 받을 때, 로그가 100배 커져도 읽는 바이트는 0.5%만 늘었다. 재생한 메시지는 세 크기 모두 0이다.

| 이벤트 수 | 로그 크기 | 페이지가 읽은 바이트 | 벽시계 |
| --- | --- | --- | --- |
| 1,000 | 514,853 B | 15,375 B | 4.28 ms |
| 10,000 | 5,168,853 B | 15,415 B | 4.68 ms |
| 100,000 | 51,888,853 B | 15,455 B | 4.80 ms |

이 표의 로그 크기는 성장 테스트가 직접 만든 세션이라 앞 표의 10만 이벤트 픽스처(62,155,848 B)와 크기가 다르다. 같은 측정이 아니다.

### 표에 담지 않은 커밋

나머지 커밋은 앞뒤 쌍보다 상한, 측정 표면, 결정을 바꾼 것이다. NDJSON 프레이밍을 UTF-8 경계에서 안전하게 하고 프레임 상한을 넣은 커밋, 훅의 stdin 쓰기를 데드라인 안으로 옮긴 커밋, 도구 지연을 execute 가 아니라 dispatch 전체로 넓힌 커밋이 있다. 이벤트 스트림에 개수·바이트 상한과 보이는 overflow 를 넣은 커밋, 압축 결정 전의 컨텍스트 복제를 없앤 커밋, 브리지 쓰기를 select 루프 밖 유계 큐로 옮긴 커밋도 있다. 문법 오류로 실행되지 않던 벤치마크 스크립트를 고치고 고정 픽스처 생성기를 넣은 커밋, 단계 카운터·할당 프로브·같은 기계 A/B 실행기를 넣은 커밋, p99 키를 항상 남기는 규칙을 넣은 커밋이 측정 표면을 바꿨다. 네 가지 구조 항목을 측정으로 기각한 결정 기록, 구조 게이트·리포트 템플릿·A/B 안내를 넣은 커밋 둘, 색인 헤더 검사의 중복 클로저를 정리한 후속 수정이 나머지다.

## 측정했지만 채택하지 않은 것

네 구조 항목은 측정한 뒤 채택하지 않기로 결정했다. 각 항목에는 그 결정을 뒤집으려면 넘어야 할 숫자가 함께 있다.

| 항목 | 결정 | 결정한 숫자 |
| --- | --- | --- |
| 비활성 세션 축출 정책 | 채택 안 함 | 세션당 fd 0.999개, 스레드 0.998개, RSS 0.41 MB. 1개에서 1,000개까지 이 기울기가 꺾이지 않는다. `shared_context` 항목과 debug job 은 다섯 인구 수 모두에서 0이고, 소크에서 세션 수는 0으로 돌아온다 |
| `globalEvents` 다중화 | 채택 안 함 (프로토콜 변경) | 감시 세션당 소비자 쪽 fd 7.4개와 스레드 3.0개. 데몬 자신의 몫은 세션당 연결 1.0개이고, 브리지는 연결 하나에 세션 하나를 붙인다 |
| 세션 맵 잠금 범위 축소 | 채택 안 함 | 무관한 세션 제거가 import 전체를 기다린다. 1 / 20 / 60 / 150 이벤트에서 861 / 5,561 / 11,255 / 24,499 ms, 비율 1.00 |
| 인터럽트 폴 25 ms 타이머를 통지로 교체 | 채택 안 함 | 최악 29.7 ms(평균 27.6 ms)를 아끼려고, 감시 그룹 라운드당 wakeup 1회라는 비용 구조를 바꾸는 것이다 |

숫자로 기각한 것은 실패가 아니다. 이 계획은 "개선이 안 나오면 그 항목은 '채택 안 함'으로 근거와 함께 끝냅니다. 그것도 성공입니다"를 성공의 정의로 삼았다. 위 표의 마지막 열이 그 근거다.

## 이 숫자들이 말하지 않는 것

이 절이 이 페이지의 목적이다. 아래 문장 중 하나라도 빠지면 이 페이지는 읽는 사람을 잘못된 결론으로 데려간다.

- **소스 해시의 개선은 해시 단계에만 해당한다.** 저장 경로는 여전히 로그 전체를 읽는다. `crates/base/src/session/event_commit.rs:31` 이 `std::fs::read` 로 로그를 통째로 읽어 접두부를 검증하므로, 저장 한 번의 최고 할당은 로그 크기를 따라 계속 커진다(7 KB 로그에서 156,518 B, 553 KB 로그에서 8,293,580 B). "200 MiB 로그의 저장 최고 할당이 65 KB"는 거짓이다.
- **이력 페이지의 개선은 머리가 바뀌지 않은 동안의 반복 페이징에만 해당한다.** 색인은 로그의 신원 스탬프(장치, inode, 길이, mtime, ctime)에 묶여 있어 append 한 번, import 한 번, reset 한 번으로 무효가 된다. 그래서 계속 자라는 세션은 예전 비용을 한 번 더 치르고 색인을 다시 만든다. 그 재구축 비용은 측정되지 않았다. 새 머리에서 페이지를 한 번만 받는 경우는 빨라지지 않는다.
- **캐시의 개선은 할당이지 처리량이 아니다.** 벽시계는 MiB당 약 19 ms 로 양쪽이 같다. 이스케이프 비용이 어느 쪽이든 지배한다.
- **소크는 fd, 스레드, 큐, 대기 요청, 태스크를 단언한다. RSS 는 아니다.** RSS 는 68.4 MB 에서 79.5 MB 로 오른 뒤 평평하게 머물렀고(최고 83.7 MB), 관찰값으로만 남았다.
- **200 사이클 소크는 20 사이클당 자원 1개의 누수를 판별한다.** 계획이 요구한 것은 1,000 사이클이고 그 항목은 `[~]` 다. 1,000 사이클 변형은 `#[ignore]` 로 저장소에 있다.
- **구조 게이트는 프레임 상한을 단언하지 않는다.** 게이트가 읽는 카운터에 프레임 계수가 없다. 프레임 상한은 `crates/harness-api-server/src/framing_tests.rs` 가 지킨다.
- **`shared_context` 는 트리 어디에도 제거 경로가 없다** (`crates/app-core/src/server.rs:771`). 모든 인구 수에서 0으로 측정됐지만, 그 0은 "아무도 쓰지 않았다"는 뜻이지 "누수가 없다"는 뜻이 아니다. 소크도 이 맵이 단조 증가하지 않는지만 본다.
- **복제 내보내기의 직렬화 계수는 페이지 단위로 고정된다.** 256개 상한에서 513회지만, 로그가 커지면 내보내기 시간은 여전히 커진다(1,000 이벤트 278.2 ms, 10,000 이벤트 2,657.2 ms). 바이트가 평평한 것은 페이지 안에서지 전체 내보내기에서가 아니다.
- **캐시 facade 의 워밍 복원 시간은 단발 측정이다.** 페이지 캐시 분산을 포함하고, 같은 케이스에서 레인의 108.6 / 901.8 ms 와 리드의 재실행 108.9 / 499.7 ms 가 그 폭이다. 바이트 단위 경계 판정만 재현 가능한 수치다.
- **네 가지 채택 안 함 결정의 절대 밀리초는 릴리스 수치가 아니다.** 디버그 테스트 프로필과 임시 홈에서 잰 값이고, 전이되는 것은 비율(대기 시간이 import 시간과 같다, 세션 생성 약 1.2 s)이다.
- **`globalEvents` 의 fd 7.4개와 스레드 3.0개는 소비자 쪽 비용이다.** SDK 와 데몬이 같은 프로세스에서 돌아 양 끝이 함께 세어졌다. 100 세션과 1,000 세션 수치는 이 세션당 값에 대한 산술이지 두 번째 측정이 아니다.

## 재현 방법

측정마다 `CARGO_TARGET_DIR` 을 자기 전용 디렉터리로 두고, 한 번에 하나씩만 돌린다. 하나의 타깃 디렉터리에 테스트 바이너리 둘이 동시에 붙으면 매번 다른 실패가 나고, 그 실패는 코드에 대한 정보가 아니다.

```bash
export CARGO_TARGET_DIR="$PWD/.target-measure"
```

측정용 로그는 저장소의 생성기로 만든다. 아래는 200 MiB 로그와 10만 이벤트 로그다.

```bash
python3 scripts/perf/fixtures/gen_event_log.py --bytes 209715200 --seed 20261021 --out /tmp/event-log-200mib/
python3 scripts/perf/fixtures/gen_event_log.py --events 100000 --seed 20261021 --out /tmp/event-log-100000/
```

소스 해시는 프로브 예제가 전체 읽기와 스트리밍 다이제스트를 나란히 인쇄한다.

```bash
cargo build --locked -p horon-agent-base --example source_hash_probe
"$CARGO_TARGET_DIR/debug/examples/source_hash_probe" hash /tmp/event-log-200mib/events.jsonl
```

이력 페이지는 10만 이벤트 픽스처를 환경 변수로 넘겨 재고, 커밋된 500레코드 기준선과의 동일성은 별도 테스트가 본다.

```bash
HISTORY_PAGE_PERF_FIXTURE=/tmp/event-log-100000/events.jsonl \
  cargo test --locked -p horon-agent-harness-api-server --lib history_page_perf -- --nocapture --test-threads=1
cargo test --locked -p horon-agent-harness-api-server --lib pages_match_the_committed_pre_change_baseline -- --nocapture
```

SSE 파서는 청크 크기 네 가지에서 같은 이벤트 열과 파싱 횟수를 인쇄한다.

```bash
cargo test --locked -p horon-agent-provider-openai --lib stream::stream_tool_tests::chunked_replay_yields_the_same_events_at_every_chunk_size -- --nocapture
```

복제 내보내기는 페이지 바이트 동일성과 상한을 저장소 안 테스트가 검증한다. 표에 적은 직렬화 계수(129 / 513)를 인쇄한 QA 하네스는 `.omo/evidence/agent-runtime-performance-2/qa_task14.py` 에 있고 추적되지 않으므로, 클론한 사람은 그 계수를 직접 재현할 수 없다. 다이제스트와 길이는 재현된다.

```bash
cargo test --locked -p horon-agent-session-store replication -- --nocapture
```

캐시 facade 는 테스트가 경계 양쪽의 직렬화 횟수를 인쇄하고, 예제가 할당 최고점을 인쇄한다.

```bash
cargo test --locked -p horon-agent-base --lib event_cache -- --nocapture
cargo build --locked -p horon-agent-base --example facade_cache_probe
```

소크는 200 사이클 기본형과 `#[ignore]` 된 1,000 사이클 변형이 있다.

```bash
HORON_AGENT_WORKSPACE_TRUST=trust \
  cargo test --locked -p horon-agent --test e2e soak_attach_detach::soak_attach_detach_returns_every_counter_to_baseline -- --exact --nocapture
HORON_AGENT_WORKSPACE_TRUST=trust \
  cargo test -p horon-agent --test e2e soak_attach_detach_1000_cycles -- --ignored --exact --nocapture
```

구조 게이트와 A/B 실행기의 사용법, 리포트 템플릿의 필수 키, 경계를 올리는 절차는 [`docs/performance/BENCHMARK.ko.md`](BENCHMARK.ko.md) 에 있다. 게이트 명령은 `python3 scripts/perf/check_structural.py` 이고, A/B 는 `scripts/perf/ab_perf.py` 다.

## 상태

계획의 항목은 17개이고, 13개가 완료, 4개가 부분 완료(`[~]`)다. 부분 완료의 뜻은 다음과 같다.

| 항목 | 상태 | 뜻 |
| --- | --- | --- |
| 7. 두 SDK 의 NDJSON 프레이밍 상한 | `[~]` | 코드와 테스트는 `dev` 에 있고 검증됐다. 이 항목이 지목한 증거 파일이 워크트리와 함께 사라졌고 재구성하지 않았다. 동작은 증명됐고 증거 궤적은 아니다 |
| 12. 보조·결과 캐시 바이트 회계 | `[~]` | 코드는 `dev` 에 있고, 결정적인 줄은 리드가 직접 재실행해 재현했다. 레인의 원본 증거 파일은 사라졌고, 지금 그 자리에 있는 파일은 스스로 `recovered` 라고 밝힌 재구성본이다 |
| 15. attach/detach 소크 | `[~]` | 소크 자체는 유효하지만 계획이 요구한 1,000 사이클이 아니라 200 사이클로 돌았다. 1,000 사이클 변형은 `#[ignore]` 로 저장소에 있다 |
| 17. CI 구조 게이트와 리포트 템플릿 | `[~]` | 게이트는 실제로 돈다(경계 35개, `EXIT=0`, 위반 시 `EXIT=1`, 측정 누락 시 `EXIT=2`, 계약 테스트 40개). 계획의 두 조건이 남았다. 프레임 계수 카운터가 없고, "B0 에서 통과"는 구조적으로 불가능하다(`STRUCTURAL_PERF` 줄을 인쇄하는 코드가 B0 뒤에 들어왔다) |

나머지 13개 항목은 완료다.

## `agent-rust-loading-durable-runtime` 계획의 측정 (별도 계획, 별도 기준)

위 표는 `agent-runtime-performance-2` 계획의 기록입니다. 아래는 뒤에 실행된
`agent-rust-loading-durable-runtime` 계획이 남긴 것이며, 기준 커밋도 측정 도구도 다릅니다.
두 계획의 수치를 한 표에서 비교하지 않습니다.

이 계획의 원본 산출물은 `.omo/evidence/agent-rust-loading-durable-runtime/` 아래에 있습니다.

### 시간 개선: 확인되지 않음

측정 도구(`scripts/perf/bench_loading.py` 와 `crates/base/examples/loading_probe.rs`)가 이 계획의
산출물이라, 계획 시작 직전의 기준 커밋에는 같은 도구가 없습니다. 그래서 서로 다른 두 측정
프로그램을 비교하는 대신, 같은 바이너리의 두 설정을 30회씩 교대 측정하는 대조(A/A)를
썼습니다(`aa_control: true`). 결과는 `improvement_claim: false` 입니다.

| 구간 | A p50 / p95 (ms) | B p50 / p95 (ms) |
| --- | --- | --- |
| attach_history | 671.2 / 1579.2 | 691.7 / 2047.0 |
| provider_dispatch | 12.0 / 26.5 | 13.3 / 66.2 |
| resume_ready | 309.9 / 733.9 | 299.2 / 675.1 |
| session_save | 906.8 / 1271.5 | 993.2 / 1412.7 |
| skill_catalog | 0.2 / 0.2 | 0.2 / 1.4 |

같은 바이너리의 두 실행 사이 편차가 이 계획이 주장할 수 있는 효과보다 크고, 방향도 한쪽으로
치우치지 않습니다. 따라서 **벽시계 시간 개선은 주장하지 않습니다.**

### 구조 계수: 측정되고, 상한이 지켜짐

시간 대신 계수로 판정하는 항목입니다. 두 종류의 증거가 있고, 서로 다른 것을 잽니다.

- `python3 scripts/perf/check_structural.py` 가 exit 0 으로 **상한 45개 전부 통과**(실패 0, 누락 0)
  했습니다. 이 게이트가 실제로 읽는 계수는 큐 깊이·대기 요청 수·이력 페이지 비용·해시 버퍼·
  프레임 상한·attach/detach 소크 여섯 묶음입니다.
- 아래 표의 항목은 그 게이트가 읽지 **않습니다**. 각 항목의 이름 붙은 테스트가 직접 단정합니다.

| 항목 | 차원 | 무엇이 바뀌었나 | 고정하는 테스트 |
| --- | --- | --- | --- |
| 저장 검증 | `MEMORY` | 커밋 검증이 파일 전체를 할당하지 않고 고정 64 KiB 버퍼로 스트리밍. 소스 길이는 패스 수만 늘림 | `hash_stream` |
| 요청 스냅샷 | `WORK` | 같은 요청에서 메시지 깊은 복사가 이력 길이를 따라 늘지 않음 | `request_snapshot_reuse` |
| 스킬 목록 | `WORK` | 요청당 overlay 스캔 1회 이하, 섹션 상한 49,152 B(UTF-8) | `skill_snapshot_budget` |
| 연결 준비 | `WORK` | core 밖 내장 도구 생성자 호출 0회, 시작 경로가 prewarm 네트워크를 기다리지 않음 | `loading_deferred_setup` |
| 세션 재개 | `WORK` | 살아 있는 세션 재로드와 재개 중복 저장 제거 | `loading_live_resume` |

`MEMORY` 와 `TIME` 을 섞지 않습니다. 위 표의 어느 항목도 벽시계 개선을 주장하지 않습니다.

### 이 계획이 측정하지 않은 것

- 릴리스 프로필 수치. 모든 측정은 디버그 프로필입니다.
- 다중 기기·운영 규모의 처리량.
- provider 호출의 속도. provider 네트워크 시간은 이 계획의 범위가 아닙니다.

## `agent-irc-bus` 계획의 측정

위 두 기록과 별개로 실행한 `agent-irc-bus` 계획의 측정입니다. 이 계획은 에이전트 사이의 메일박스 버스와 `irc` 도구를 넣었고, 기준 커밋 B0는 `50eed12`(`dev`의 계획 시작 직전 커밋), 후보는 `c9d9d8c`(웨이브 3 병합)입니다. 앞의 두 기록과 이 기록의 수치를 서로 비교하지 않습니다.

원본 산출물은 `.hyper/qa/agent-irc-bus/11-happy.txt`(A/B 보고서 JSON, 처리량 3회, 구조 게이트 `irc` 구역)와 `.hyper/qa/agent-irc-bus/11-failure.txt`(A/A 대조)에 있고, `.hyper/`는 저장소에서 무시되므로 이 절이 추적되는 기록입니다.

### 측정 조건

| 항목 | 값 |
| --- | --- |
| 기계 | Apple M1 Max, 10 코어, 메모리 64 GiB |
| 커널 | `Darwin Alpoxs-MacBook.local 25.4.0 ... RELEASE_ARM64_T6000 arm64` |
| 툴체인 | `rustc 1.99.1`, `cargo 1.99.1` |
| 빌드 프로필 | release, 기능 집합 없음(양쪽 모두 `cargo build --locked --release -p horon-agent-base --example perf_counters_surface`) |
| 워크로드 | `perf_counters_surface` 한 번 실행(실행마다 프로세스를 새로 띄움) |
| 측정 중 부하 | 부하 평균 50.10 / 44.73 / 44.59(1분 / 5분 / 15분), `rustc`·`cargo` 프로세스 4개 |
| 반복 수 | 워밍업 3회, 각 쪽 10회, 두 쪽을 번갈아 실행 |
| B0 바이너리 | `48c69b5c...`, 커밋 `50eed12` |
| 후보 바이너리 | `a82fb191...`, 커밋 `c9d9d8c` |

측정 중 이 계획과 무관한 컴파일이 여러 저장소에서 돌고 있었습니다. 그동안 관측한 부하 평균은 36에서 64 사이였고 `rustc`와 `cargo` 프로세스 수가 4에서 17 사이로 관측됐습니다. 그래서 이 절의 절대 시간은 조용한 기계의 값이 아닙니다.

### A/B: 회귀 없음

`scripts/perf/ab_perf.py`가 exit 0으로 끝났고 채택 판정은 `not-adopted-no-measurable-change`입니다. 회귀를 뜻하는 `reject-regression`이 아니며, 중앙값 차이가 잡음 대역 안이라 개선도 회귀도 주장하지 않는 값입니다.

| 항목 | B0 | 후보 | 차이 |
| --- | --- | --- | --- |
| 중앙값 | 5.23 ms | 5.39 ms | +0.16 ms (+3.09%) |
| p95 | 7.06 ms | 7.10 ms | +0.05 ms |
| 잡음 대역(`max(3*1.4826*MAD, 중앙값의 1%)`) | | | ±1.35 ms, 대역 안 |
| `cpu_user_ms` 중앙값 | 2.22 ms | 2.18 ms | -0.04 ms |
| `cpu_system_ms` 중앙값 | 1.55 ms | 1.60 ms | +0.05 ms |
| `peak_rss_bytes` 중앙값 | 6,045,696 B | 6,029,312 B | -16,384 B |
| `fd_count` / `thread_count` 중앙값 | 3 / 1 | 3 / 1 | 같음 |
| 실패 / 시간 초과 | 0 / 0 | 0 / 0 | |
| `read_bytes` 중앙값 | 0 B | 0 B | 같음 |

실행기는 워크로드가 보고하지 않은 `allocated_bytes`, `pss_bytes`, `retained_bytes`를 0이 아니라 `null`로 두고 `counts.missing`에 세 이름을 올렸습니다. p99는 표본 10개라 20개 미만이라는 이유와 함께 거절됐습니다.

### A/A 대조: 거짓 회귀 없음

같은 후보 바이너리를 양쪽에 넣은 대조 실행은 `control-no-change`로 끝났습니다. 중앙값은 6.48 ms와 6.70 ms로 0.22 ms(3.43%) 차이가 났지만 잡음 대역 ±1.58 ms 안입니다. 즉 위 A/B의 0.16 ms 차이는 이 측정기가 구분할 수 있는 크기가 아니며, 실행기가 회귀를 지어내지 않는다는 증거입니다.

### IRC 처리량

`cargo run --locked --release -p horon-agent-irc-core --example irc_throughput`을 3회 실행했습니다. 메시지 수는 기본값 100,000이고 `broadcast64`는 64의 배수로 내림해 99,968건입니다.

| 시나리오 | 1회차 | 2회차 | 3회차 | 중앙값 | 범위 |
| --- | --- | --- | --- | --- | --- |
| `direct_waiter` (msg/s) | 105,291 | 107,434 | 111,523 | 107,434 | 105,291 ~ 111,523 |
| `direct_deliver` (msg/s) | 2,410,858 | 2,229,008 | 2,392,230 | 2,392,230 | 2,229,008 ~ 2,410,858 |
| `broadcast64` (msg/s) | 3,237,934 | 3,141,178 | 3,141,375 | 3,141,375 | 3,141,178 ~ 3,237,934 |

`direct_waiter`는 수신 작업 64쌍(`WAITER_PAIRS = 64`)이 동시에 `wait`로 기다리는 구조입니다. 각 수신 작업은 첫 라운드 뒤부터 상대에게 답장 의무를 만드는 메시지를 한 번 보내고 나서 `wait`에 들어가고, 드라이버는 64개 대기자가 모두 등록된 것을 확인한 뒤 살아 있는 쌍마다 메시지 하나를 보냅니다. 쌍 안의 왕복은 순서대로지만 64쌍은 동시에 진행합니다. `direct_deliver`는 즉시 주입을 돌려주는 가짜 전달 경로, `broadcast64`는 64개 대상에게 한 본문을 보내는 경로입니다.

todo 6이 처음 잰 `direct_waiter`는 39,128 msg/s(`6-happy.txt`, 2,555,678 us에 100,000건)였고, todo 12가 `wait` 호출마다 붙던 타이머 왕복을 없앤 뒤 이번 측정에서 107,434 msg/s입니다. 약 2.75배입니다. todo 12가 남긴 측정은 124,071 msg/s(`12-happy.txt`)이고, 이번 값은 그보다 낮습니다. 측정 시점의 부하 차이로 보이며 코드 경로는 같습니다.

### 구조 게이트 `irc`

`python3 scripts/perf/check_structural.py --only irc`가 exit 0으로 다섯 경계를 모두 통과했습니다.

| 카운터 | 관측값 | 경계 |
| --- | --- | --- |
| `mailbox_high_water` | 100 | 100 이하 |
| `mailbox_dropped` | 900 | 900과 같음 |
| `broadcast_body_allocations` | 1 | 1과 같음 |
| `waiters_live_after_churn` | 0 | 0과 같음 |
| `broadcast_inflight_high_water` | 16 | 16 이하 |

### 이 숫자들이 말하지 않는 것

- **A/B 워크로드는 버스가 아니라 프로세스 시작과 카운터 인쇄를 잽니다.** `perf_counters_surface`는 데몬을 띄우지 않고 카운터 스냅샷을 한 번 인쇄합니다. IRC 버스는 기본 설정에서 꺼져 있고 이 경로에 들어오지 않으므로, 이 A/B가 말하는 것은 "공용 코드에 회귀가 없다"까지입니다. 버스 자체의 비용은 처리량 표가 답합니다.
- **처리량은 같은 프로세스 내부 측정입니다.** 예제는 `IrcBus`와 가짜 디렉터리·가짜 전달자를 한 프로세스에 두고 잽니다. 데몬 전송, 직렬화, 세션 상태 전이, 모델 호출은 포함하지 않습니다. 실제 에이전트 대화의 응답 시간은 이 수치로 예측할 수 없습니다.
- **`direct_waiter` 수치는 기계 부하에 민감합니다.** 이번 측정의 3회는 105,291에서 111,523 사이이고, todo 6의 39,128(`6-happy.txt`)과 견주면 약 2.75배입니다. todo 12가 남긴 124,071(`12-happy.txt`)과 이번 값의 차이는 측정 시점의 부하 차이로 보이므로, 옮겨 읽을 값은 절대 처리량보다 비율입니다.
- **A/B의 절대 시간은 조용한 기계의 값이 아닙니다.** 측정 중 관측한 부하 평균이 36에서 64 사이였고, 무관한 저장소의 `cargo test`, `cargo clippy`, `cargo build`가 함께 돌았습니다. A/B가 두 쪽을 번갈아 실행하고 A/A가 잡음 대역을 확인했으므로 비교의 방향은 유효하지만, 5 ms대 절대값은 다른 조건에서 재현되지 않습니다.
- **p99는 없습니다.** 표본이 10개라 실행기가 20개 미만을 이유로 거절했습니다.
- **세 지표는 측정되지 않았습니다.** `allocated_bytes`, `pss_bytes`, `retained_bytes`는 워크로드가 보고하지 않아 `null`이고 `counts.missing`에 있습니다. 0으로 읽지 않습니다.
- **구조 게이트는 시간을 재지 않습니다.** 위 다섯 값은 모양이 상한 안에 있는지만 말하고, 빠른지는 말하지 않습니다.

## `platform-adapter-perf` 계획의 측정

위 세 기록과 별개로 실행한 `platform-adapter-perf` 계획의 측정입니다. 이 계획은 OS별 코드를 `crates/platform-*` 다섯 크레이트로 모으고, 세션 저장에서 다시 만들 수 있는 파생 캐시의 fsync를 없애고, HTTPS 클라이언트의 TLS 설정을 프로세스당 한 번만 만들고, 가격표 새로고침을 기본으로 끕니다. 기준 커밋 B0는 `738712a`(`dev`의 계획 시작 직전 커밋), 후보는 `215bfdb`(작업 브랜치)입니다. 앞 세 기록과 이 기록의 수치를 서로 비교하지 않습니다.

원본 산출물은 `.hyper/qa/platform-adapter-perf/after/` 아래에 있습니다. `.hyper/`는 저장소에서 무시되므로 이 절이 추적되는 기록입니다.

| 파일 | 내용 |
| --- | --- |
| `compare-macos.json`, `compare-linux.json`, `compare-linux-times.json` | 세 줄(horon B0, horon 후보, jcode) 비교. Linux `compare-linux.json`은 `--syscalls`, 시간 수치는 `compare-linux-times.json`에서 |
| `fsync-linux.txt` | one-shot 한 건의 fsync·fdatasync 호출 수, cold와 warm |
| `ab-*.json` | 같은 기계 A/B 보고서(실행기 출력 JSON 그대로) |
| `github-baseline/`, `github-candidate/` | GitHub `benchmark.yml` 실행 42294971747(`--ref dev`)과 42294976581(브랜치)의 아티팩트 |
| `structural.log` | `check_structural.py` 전체 출력 |

### 측정 조건

| 항목 | macOS | Linux | GitHub 러너 |
| --- | --- | --- | --- |
| 기계 | Apple M1 Max, 10 코어 | Docker `rust:1-trixie`, linux/aarch64, 10 코어 | `ubuntu-latest`, linux/x86_64, 4 코어 |
| 툴체인 | `rustc 1.99.1` | 컨테이너 배포판 rustc | `stable` (실행 시점) |
| 프로필 | release | release | release |
| 한 측정 안의 두 바이너리 | 같은 기계, 번갈아 실행 | 같음 | 다른 실행, 다른 러너 인스턴스 |
| 경쟁 도구 | jcode v0.99.1 (`7206efd0c`) | 같음 | 같음 |

한 측정 안의 모든 줄이 같은 모의 모델 서버에 붙으므로 API 요금과 시크릿은 쓰지 않습니다. macOS와 Linux는 B0 바이너리와 후보 바이너리를 같은 실행 안에서 쟀고, GitHub은 `dev` 실행과 브랜치 실행을 따로 띄웠습니다. 그래서 아래 표에서 GitHub 열만 "전후"가 아니라 "두 실행"입니다.

### 세 환경 전후

one-shot은 헤드리스 프롬프트 한 건(기동, `ls` 도구 왕복 1회, 최종 응답)의 중앙값입니다. start→first request는 api-bridge 소켓 연결부터 첫 `list_sessions` 응답까지입니다. fsync는 one-shot 한 건이 부른 fsync·fdatasync 호출 수입니다. 메모리는 데몬 프로세스 트리의 idle RSS입니다.

| 지표 | 환경 | B0 `738712a` | 후보 `215bfdb` | 변화 | jcode | 후보 / jcode |
| --- | --- | --- | --- | --- | --- | --- |
| one-shot 중앙값 | macOS | 872.4 ms | 290.4 ms | -66.7% | 294.8 ms | 0.99× |
| one-shot 중앙값 | Linux (Docker) | 207.9 ms | 168.0 ms | -19.2% | 146.3 ms | 1.15× |
| one-shot 중앙값 | GitHub 러너 | 185.3 ms | 499.8 ms | 별도 실행, 비교 불가 | 145.5 ms / 357.8 ms | 비교 불가 |
| start→first request | macOS | 236.4 ms | 78.7 ms | -66.7% | 해당 없음 | 해당 없음 |
| start→first request | Linux (Docker) | 57.0 ms | 57.0 ms | -0.03% | 해당 없음 | 해당 없음 |
| start→first request | GitHub 러너 | 23.7 ms | 25.3 ms | 별도 실행 | 해당 없음 | 해당 없음 |
| fsync·fdatasync 호출 | Linux (Docker) | 175 cold / 170 warm | 40 cold / 35 warm | -77% | 19 cold / 5 warm | 7.0× |
| fsync·fdatasync 호출 | GitHub 러너 | 잴 수 없음 | 40 cold / 35 warm | 잴 수 없음 | 19 / 10 / 5 회 | 3.5× |
| 첫 세션 idle RSS | macOS | 32.73 MiB | 29.52 MiB | -9.8% | 37.34 MiB | 0.79× |
| 첫 세션 idle RSS | Linux (Docker) | 32.05 MiB | 33.37 MiB | +4.1% | 40.29 MiB | 0.83× |
| 세션당 증가분 | macOS | 592 KiB | 320 KiB | -46% | 544 KiB | 0.59× |
| 세션당 증가분 | Linux (Docker) | 1396 KiB | 944 KiB | -32% | 144 KiB | 6.6× |
| 첫 세션 idle RSS | GitHub 러너 | 잴 수 없음 | 39.9 MiB | 잴 수 없음 | 43.0 MiB | 0.93× |
| 세션당 증가분 | GitHub 러너 | 잴 수 없음 | 319 KiB | 잴 수 없음 | 38 KiB | 8.3× |

이 표의 후보 열은 `215bfdb` 바이너리로 잰 값입니다. 그 뒤 브랜치에 쌓인 변경 가운데 시작 경로를 건드린 것은 지연 HTTP 클라이언트(`b6dcfc0`) 하나이고, 시간·메모리 재측정은 아래 "메모리" 절에 있습니다. F2 품질 검토가 찾은 11건의 수정도 이 수치를 바꾸지 않았고, 같은 실행기로 다시 재면 Linux fsync가 cold 40 · warm 35로 값이 같습니다(`F2-review.md`의 "fsync" 절).

fsync의 40 상한 판정은 `bench_compare.py --syscalls` 실행기가 잰 값(cold 40 · warm 35)입니다. todo 6의 자체 드라이버는 HOME과 프롬프트 모양이 달라 같은 후보 바이너리에서 cold 45 · warm 40을 쟀고, 그 실행에서는 40 상한을 warm만 만족합니다(`6-linux-fsync.txt`의 "result against the plan").

호스트 러너의 절대값은 기계마다 달라 뜻이 없으므로 GitHub 열은 "워크플로가 브랜치에서 그대로 돌고 fsync 수도 기록된다"까지만 읽습니다. GitHub B0 실행의 세션 측정은 세 지점 모두 `daemon socket not ready`로 실패했습니다. 그 실행은 `dev`의 `bench_compare.py`를 쓰는데, `serve --debug-socket` 제거에 맞춘 수정(todo 13)이 아직 브랜치에만 있어서입니다. 그래서 GitHub B0의 세션 값은 0으로 읽지 않습니다.

### 같은 기계 A/B: 시간 경로에 회귀 없음

`scripts/perf/ab_perf.py`는 두 바이너리를 블록 단위로 번갈아 실행하고 잡음 대역 `max(3*1.4826*MAD, 중앙값의 1%)`을 함께 냅니다. 대역을 넘는 차이만 개선이나 회귀로 부릅니다.

한 줄이 한 경로입니다. one-shot과 start→first request, 세션 경로는 준비(임시 HOME, 모의 모델 주소, 데몬 기동)를 대신 해 주는 작은 프로브를 거치므로 이 표의 절대값은 앞 환경 표보다 프로브 기동분만큼 큽니다. 두 쪽이 같은 프로브를 쓰므로 비교 방향은 그대로입니다.

| 경로 | 환경 | B0 | 후보 | 차이 | 잡음 대역 | 판정 |
| --- | --- | --- | --- | --- | --- | --- |
| one-shot | macOS | 1121.85 ms | 435.79 ms | -61.2% | ±93.95 ms | 개선 |
| start→first request | macOS | 236.36 ms | 78.72 ms | -66.7% | ±10.36 ms | 개선 |
| 세션 경로 2개 | macOS | 3295.26 ms | 2259.88 ms | -31.4% | ±131.13 ms | 개선 |
| `--version` 로드 | macOS | 7.76 ms | 7.52 ms | -3.0% | ±1.36 ms | 대역 안 |
| `perf_counters_surface` | macOS | 4.74 ms | 4.80 ms | +1.3% | ±1.41 ms | 대역 안 |
| one-shot | Linux | 265.02 ms | 217.62 ms | -17.9% | ±14.61 ms | 개선 |
| `--version` 로드 | Linux | 2.60 ms | 2.63 ms | +1.2% | ±0.39 ms | 대역 안 |
| `perf_counters_surface` | Linux | 0.46 ms | 0.47 ms | +1.6% | ±0.08 ms | 대역 안 |
| start→first request | Linux | 57.03 ms | 57.02 ms | -0.03% | ±2.93 ms | 대역 안 |
| 세션 경로 1개 | Linux | 14882 ms | 14836 ms | -0.3% | ±148.82 ms | 대역 안 |
| 세션 경로 2개 | Linux | 14936 ms | 14858 ms | -0.5% | ±149.36 ms | 대역 안 |

도구 지연과 기동 단계는 같은 기계에서 따로 확인했습니다. `bench_tools.py`의 14개 케이스를 3회씩 번갈아 돌린 결과 중앙값 차이는 -15.7%에서 +5.4% 사이이고, 커진 케이스는 `ls`(+5.4%), `agentgrep`(+1.1%), `read (repo Cargo.toml)`(+0.9%)입니다. `ls`는 B0의 세 회차가 2.21 / 2.46 / 3.57 ms이고 후보가 2.58 / 2.59 / 2.64 ms라 B0 자신의 회차 폭 안에 들어갑니다. `bench_startup.py`의 서버 준비 중앙값은 10.8~10.9 ms(B0)와 11.0 ms(후보), 브리지 총 시간은 18.1~18.6 ms와 17.9~18.6 ms로 양쪽이 겹칩니다. GitHub 후보 실행에서 변경과 무관한 도구 케이스까지 20~40% 높았던 것은 러너 인스턴스 차이이고, 같은 기계에서는 재현되지 않습니다.

macOS가 크게 줄어든 것은 이 계획이 노린 두 비용이 macOS에만 있었기 때문입니다. 파생 캐시를 포함한 저장마다 `F_FULLFSYNC`를 걸던 경로가 원본으로 줄었고, reqwest 클라이언트마다 키체인 인증서를 읽던 경로가 프로세스당 한 번 생성으로 바뀌었습니다. 대조로 `--version` 로드와 `perf_counters_surface`는 두 환경 모두 대역 안입니다. 이 두 경로는 저장도 TLS도 지나지 않으므로, 공용 코드에 회귀가 생겼다면 여기서 먼저 보였을 자리입니다.

### 메모리: Linux 데몬 상주량의 처음 수치와, 다시 잰 뒤의 결론

시간 경로는 어디서도 느려지지 않았습니다. 메모리는 Linux 데몬 상주량(`rss`)이 늘어난 것으로 처음 기록했는데, 그 측정 방법 자체가 두 빌드를 비교할 수 없다는 것을 뒤에 확인했습니다. 처음 수치를 먼저 남기고, 재측정과 원인 수정을 그 아래 적습니다.

| 측정 | 환경 | B0 | 후보 | 차이 |
| --- | --- | --- | --- | --- |
| 데몬 idle, 세션 0개 | macOS | 27.82 MiB | 24.09 MiB | -3.73 MiB (-13.4%) |
| 데몬 idle, 세션 1개 | macOS | 32.34 MiB | 28.95 MiB | -3.39 MiB (-10.5%) |
| 데몬 idle, 세션 0개 | Linux | 27.88 MiB | 29.01 MiB | +1.13 MiB (+4.1%) |
| 데몬 idle, 세션 1개 | Linux | 31.74 MiB | 32.75 MiB | +1.01 MiB (+3.2%) |
| one-shot 최고 RSS (8회) | Linux | 32.46 MiB | 32.87 MiB | +0.41 MiB (+1.3%, 범위 겹침) |
| 같은 바이너리 대조 (A/A) | Linux | 27.83 MiB | 27.87 MiB | +0.03 MiB (+0.1%) |

Linux의 차이는 힙이 아니라 실행 파일 상주 페이지입니다. 세션 1개 상태의 `/proc/<pid>/smaps`를 매핑 종류별로 더하면 다음과 같습니다.

| 매핑 | B0 | 후보 | 차이 |
| --- | --- | --- | --- |
| 실행 파일 `r-xp` | 26176 KiB | 27136 KiB | +960 KiB |
| 실행 파일 `r--p` | 1508 KiB | 1508 KiB | 0 |
| 익명 `rw-p` | 1796 KiB | 1730 KiB | -66 KiB |
| 그 밖 | 1784 KiB | 1784 KiB | 0 |

같은 측정을 한 번 더 돌리면 `r-xp` 차이는 +960에서 +1472 KiB, 익명 차이는 -66에서 +156 KiB 사이에서 나옵니다. 두 번 모두 방향은 같고 크기는 실행 파일 쪽이 압도적입니다. 세션이 없을 때도 같은 방향으로 `r-xp` +1152 KiB, 익명 +10 KiB입니다. 보조 페이지 폴트 중앙값은 1574와 1584로 사실상 같고 주 페이지 폴트는 양쪽 0입니다. 바이너리는 276,928 B 커졌고 `.text`는 100,576 B 커졌으므로, 늘어난 상주량은 파일 크기보다 크게 나옵니다. 즉 더 많은 코드 페이지가 실제로 닿습니다. 세션당 증가분은 반대로 B0 1396 KiB에서 944 KiB로 줄었습니다.

#### 다시 잰 결과: 이 차이는 코드가 아니라 페이지 캐시였다

처음 수치는 같은 파일을 양쪽에 쓰는 A/A 대조만 했기 때문에 "이 바이너리 하나에서는 측정이 반복된다"만 보증합니다. 같은 소스를 독립적으로 두 번 빌드해 같은 방법으로 재면 그 차이보다 큰 폭으로 흔들립니다.

| 대조 | rss 중앙값 | `r-xp` 중앙값 |
| --- | --- | --- |
| 같은 커밋, 빌드 1 vs 빌드 2 | 22.22 vs 21.63 MiB | 22068 vs 22028 KiB |
| 같은 커밋, 빌드 1 vs 빌드 3 | 22.22 vs 26.33 MiB | 22068 vs 21948 KiB |
| 후보 커밋, 빌드 1 vs 빌드 2 | 27.00 vs 23.17 MiB | 22576 vs 22716 KiB |
| 같은 바이트, 경로만 다름 | 14.39 vs 26.85 MiB | 11276 vs 24076 KiB |

원인은 `r-xp` 상주 페이지가 실행한 코드가 아니라 커널이 폴트마다 함께 매핑한 페이지 캐시 폴리오라는 점입니다. 상주 구간의 중앙 길이는 32~48페이지(64 KiB 폴리오)이고, 실제 실행 구간은 1~2페이지입니다. 그래서 같은 소스의 두 빌드도 상주량이 8 MiB까지 갈립니다.

측정을 캐시에 흔들리지 않는 값으로 바꿨습니다. callgrind 명령어 덤프(`--dump-instr=yes`)에서 실행한 4 KiB 코드 페이지 수를 세고, `rss`는 시작 전마다 `posix_fadvise(DONTNEED)`로 페이지 캐시를 비우고 `fault_around_bytes`를 4096으로 고정해 쟀습니다.

| 빌드 | 실행 코드 페이지 | `rss` 중앙값(통제 측정) |
| --- | --- | --- |
| 기준선 738712a #1 | 1765 | 26.42 MiB |
| 기준선 #2 | 1765 | 26.37 MiB |
| 후보 f5f5868 #1 | 1771 | 26.92 MiB |
| 후보 #2 | 1778 | - |
| 후보 #3 | 1771 | - |

한쪽 안의 빌드 간 폭(기준선 1759~1765)이 양쪽 차이(1765 vs 1771)와 같거나 큽니다. 즉 처음 기록한 "+1.0~1.3 MiB 실행 코드 상주"는 이 변경에 귀속되는 회귀가 아닙니다.

#### 실제로 고친 것: 시작할 때 TLS 신뢰 저장소를 만들던 경로

`serve`는 시작할 때 도구 레지스트리를 만듭니다. 그때 `WebFetchTool::new`, `WebSearchTool::new`, `DiscoverToolsTool::new`, `NotificationDispatcher::new`가 각각 `shared_http_client()`를 들고 있었고, 그 클라이언트를 만드는 일은 신뢰 저장소 449개 앵커를 전부 파싱하는 일입니다.

    shared_http_client -> ClientBuilder::build -> openssl_probe::probe
      -> rustls_native_certs::load_pem_certs
      = 시작 instructions의 83%, 실행 코드 페이지 878개 중 194개

네 생성자가 클라이언트를 들지 않게 바꿨습니다. `shared_http_client()`는 이미 프로세스 전역 `OnceLock`이므로, 요청을 보내는 지점에서 그대로 부르면 됩니다.

| 쪽 | 실행 코드 페이지 | `rss` 중앙값 | `r-xp` 중앙값 |
| --- | --- | --- | --- |
| 기준선 | 1765 | 26.42 MiB | 22068 KiB |
| 수정 전 후보 | 1771 | 26.92 MiB | 22576 KiB |
| 수정 후 후보 | 1755 | 26.59 MiB | 22208 KiB |

수정 후 실행 코드 페이지는 기준선보다 적습니다(1755 vs 1765). 남은 `rss` 차이 +0.17 MiB는 위에서 본 같은 소스 빌드 간 폭(통제 측정에서 최대 8 MiB, 같은 쌍에서 0.6 MiB) 안이며, 실행 코드 차이로는 나타나지 않습니다.

시작 경로의 나머지는 양쪽이 같습니다. idle까지의 시스템 호출은 5416 vs 5428(53종 중 46종 동일), 연 파일은 364 vs 365입니다.

### 구조 게이트

`python3 scripts/perf/check_structural.py`가 exit 0으로 55개 경계를 모두 통과했습니다(실패 0, 누락 0). 이 게이트는 벽시계 시간을 읽지 않으므로 위 시간 수치와는 다른 축을 지킵니다.

### 이 숫자들이 말하지 않는 것

- **GitHub 러너의 두 실행은 전후 비교가 아닙니다.** 서로 다른 호스티드 러너 인스턴스이고, 브랜치의 워크플로만 `--syscalls`를 켜므로 후보 실행에만 strace가 붙습니다. 후보 실행에서 이 변경과 무관한 바이너리까지 일제히 느려졌습니다: jcode 145.5 → 357.8 ms, Claude Code 452.1 → 913.9 ms, Codex CLI 369.4 → 2273.1 ms, OpenCode 2.98 → 5.81 s, pi 928.4 → 2109.5 ms. 바뀌지 않은 실행 파일이 두 배 넘게 느려졌으므로 두 실행의 차이는 이 계획이 아니라 러너와 계측에서 옵니다. 같은 Docker 기계에서 잰 strace 비용은 one-shot당 +255에서 +259 ms였고, 후보 실행의 변경 없는 도구 지연도 일률적으로 높습니다(`bash (true)` 2.2 → 3.1 ms, `read (tiny file)` 0.5 → 0.6 ms). 이 계획의 회귀 판정은 같은 기계에서 잰 macOS와 Docker Linux 두 환경으로 합니다.
- **macOS의 -66.7%는 이 계획의 두 비용에만 해당합니다.** 저장 경로와 클라이언트 생성 경로를 지나지 않는 `--version` 로드와 `perf_counters_surface`는 대역 안입니다. 모델 지연, 도구 실행 시간, 프롬프트 조립 시간은 이 수치에 들어 있지 않습니다.
- **fsync 호출 수는 Linux 컨테이너에서만 셉니다.** macOS에는 `strace`가 없고, 이 계획은 macOS의 기본 동기화를 바꾸지 않았습니다(설정 `[storage] sync = "fast"`로만 일반 fsync).
- **처음 잰 Linux 데몬 RSS +1.0~1.3 MiB는 회귀가 아니었습니다.** 그 값은 실행한 코드가 아니라 커널이 폴트마다 함께 매핑한 페이지 캐시 폴리오를 재고 있었습니다. 같은 소스의 두 빌드도 더 크게 흔들리므로 그 수치로 이 변경을 판정할 수 없습니다. 캐시에 흔들리지 않는 실행 코드 페이지로 다시 재면 후보가 기준선보다 적고(수정 후 1755 vs 1765), 통제한 `rss` 차이는 같은 소스 빌드 폭 안입니다. 대신 시작할 때 TLS 신뢰 저장소를 만들던 경로를 찾아 없앴습니다(위 "실제로 고친 것").
- **첫 세션 RSS와 세션당 증가분은 다른 축입니다.** Linux 후보는 첫 세션 값이 +1.3 MiB 높고 세션당 증가분은 -452 KiB입니다. 두 값을 하나의 "메모리"로 합치지 않습니다.
- **`perf_counters_surface`는 데몬을 띄우지 않습니다.** 이 워크로드가 말하는 것은 "공용 코드에 회귀가 없다"까지이고, 저장 경로나 TLS 경로의 비용은 one-shot 표가 답합니다.
- **p99는 없습니다.** 표본이 12개 이하라 실행기가 20개 미만을 이유로 거절했습니다.

## `cache-context-session-opt` 계획의 측정

이 계획은 OpenAI 호환 요청에 `prompt_cache_key`와 보존 기간을 싣고(`ee9f46bc`), 같은 입력이면 그 키를 다시 계산하지 않으며(`263ab39e`), 압축 꼬리를 회복 대역(기준의 0.8)에 맞추고(`6e7f5052`), 묶음 저장의 로그 동기화를 묶음 끝 한 번으로 합칩니다(`626eebb3`). 성능은 GitHub Actions에서만 잽니다. 후보는 `dev`의 `e95b8773`이고, 측정은 2026-11-07의 `benchmark.yml` 실행 42371020359(`--ref dev`, linux·macos·windows 모두 성공)와 `structural-perf.yml` 실행 42371014382(성공)입니다. 비교 기준은 위 `platform-adapter-perf` 절에 기록된 같은 러너 값입니다.

| 지표 | 환경 | 이전 기록 | 후보 `e95b8773` | 변화 |
| --- | --- | --- | --- | --- |
| one-shot fsync·fdatasync 호출 (표본 3) | GitHub Linux 러너 | 40 cold / 35 warm | 32 cold / 27 warm | -20% cold, -23% warm |
| 그중 fsync / fdatasync (cold) | GitHub Linux 러너 | 해당 없음 | 16 / 16 | 해당 없음 |
| jcode 대비 배수 (warm) | GitHub Linux 러너 | 7.0× (35 / 5) | 5.4× (27 / 5) | 차이 축소 |
| one-shot 시간 중앙값 | GitHub macOS 러너 | 해당 없음 | 175.52 ms (jcode 341.8 ms) | 해당 없음 |
| one-shot 시간 중앙값 | GitHub Windows 러너 | 해당 없음 | 269.02 ms (jcode 175.37 ms) | 해당 없음 |
| 구조 성능 게이트 | GitHub Linux 러너 | 통과 | 통과 | 회귀 없음 |

줄어든 호출은 로그 줄의 fdatasync입니다. 3건 묶음 저장의 `LogFileSync`가 3에서 1로 줄어든 것은 `crates/session-store/tests/batch_sync.rs`가 고정합니다.

### 이 숫자들이 말하지 않는 것

- **one-shot 시간은 표본이 3개입니다.** 이 절은 OS별 현재 값을 적을 뿐이고, 같은 러너의 이전 시간 값과 A/B로 비교한 것이 아닙니다. 아래 자동 측정 절이 `main`에서 같은 묶음을 계속 다시 잽니다.
- **캐시 키의 효과는 여기 없습니다.** 벤치마크는 가짜 모델 서버를 쓰므로 캐시 적중이 생기지 않습니다. 적중은 실제 모델 회사의 `cached_tokens`로만 보입니다. 키 계산 비용은 세션 안에서 입력이 바뀌지 않으면 비교 한 번으로 끝나도록 메모했습니다(`crates/provider-openai/src/chat/prompt_cache_memo.rs`).
- **압축 회복 대역은 시간 지표가 아닙니다.** 24k 창의 남기는 꼬리가 10,800에서 3,625 토큰으로 줄고 200k 창은 140,000 / 20,000 그대로라는 것은 단위 테스트가 확인합니다.

## `jcode-parity-optimization` 계획의 측정

이 계획은 기본 실행에서 가격표 읽기와 두 번째 설정 파싱을 건너뛰고(`76a510fd`), 세션마다 저장 스레드를 두던 것을 최대 4개짜리 공용 writer 풀로 바꾸고(`c75dd149`), 더티 표식을 디렉터리 fsync 하나로 확정하고(`af13232d`), 30분 넘게 쉰 세션의 파생 캐시를 놓습니다(`1266059d`). 성능은 GitHub Actions에서만 잽니다. `benchmark.yml`의 `baseline_ref` 입력으로 기준 `main`(`b62a79c9`)과 후보를 **한 잡 안에서 같은 러너, 같은 가짜 모델 서버로 번갈아** 쟀으므로 아래 이전/이후는 같은 기계의 A/B입니다. 실행은 2026-11-08의 42388003895(linux·macos, 세션 1·10·100, 턴 3, one-shot 표본 3)와 42389421614(macos, one-shot 표본 20)입니다.

| 지표 | 환경 | 이전 `b62a79c9` | 이후 | jcode v0.99.1 |
| --- | --- | --- | --- | --- |
| 세션 하나가 늘 때 메모리 (기울기) | GitHub Linux 러너 | 254.7 KB | 149.6 KB | 57.4 KB |
| 세션 하나가 늘 때 메모리 (기울기) | GitHub macOS 러너 | 249.6 KB | 185.1 KB | 153.4 KB |
| 세션당 스레드 | GitHub Linux 러너 | 1.14 | 0.16 | 0.07 |
| 세션당 스레드 | GitHub macOS 러너 | 1.34 | 0.26 | 측정 불가 |
| 100세션 턴 뒤 트리 RSS | GitHub Linux 러너 | 96.2 MB | 79.2 MB | 65.5 MB |
| 100세션 턴 뒤 트리 RSS | GitHub macOS 러너 | 84.5 MB | 75.4 MB | 70.1 MB |
| one-shot fsync·fdatasync 호출 (cold, 첫 표본) | GitHub Linux 러너 | 32 | 27 | 19 |
| one-shot fsync·fdatasync 호출 (warm, 마지막 표본) | GitHub Linux 러너 | 27 | 22 | 5 |
| one-shot 시간 중앙값 (표본 3) | GitHub Linux 러너 | 367.0 ms | 363.9 ms | 288.7 ms |
| one-shot 시간 중앙값 (표본 20) | GitHub macOS 러너 | 219.3 ms | 215.8 ms | 312.9 ms |

세션당 fd는 두 환경 모두 이전과 이후가 1.00으로 같습니다.

### 이 숫자들이 말하지 않는 것

- **표본 3개의 macOS 중앙값은 회귀처럼 보였습니다.** 42388003895에서 이후 384.3 ms, 이전 254.4 ms였지만 표본이 이후 616·384·245 ms, 이전 594·254·235 ms로 흩어져 있었습니다. 같은 비교를 표본 20개로 다시 잰 42389421614에서 중앙값 215.8 대 219.3 ms, 최솟값 174.7 대 173.5 ms로 차이가 없었습니다. 그래서 `benchmark.yml`에 `oneshot_runs` 입력을 두었습니다.
- **jcode와의 차이는 아직 남아 있습니다.** Linux에서 세션 하나가 늘 때 메모리는 jcode의 약 2.6배이고 one-shot은 1.26배 느립니다. 이 계획은 그 차이를 줄였을 뿐 없애지 않았습니다.
- **세션당 스레드 기준(기울기 0.1 미만)은 Linux에서만 맞았습니다.** 세션 1·10·100 세 점의 기울기는 Linux 0.16, macOS 0.26이었습니다. 세션 500을 더한 42397804493(같은 러너 A/B, one-shot 표본 20)에서는 Linux 0.05(이전 1.04, jcode 0.02), macOS 0.40(이전 1.45)입니다. Linux의 세 점 값은 적은 세션 수에서 런타임 스레드가 섞인 잡음이었지만, macOS는 500세션에서도 0.1을 넘습니다. writer 풀은 최대 4개이므로 macOS에서 남은 기울기는 다른 곳(데몬의 블로킹 작업 스레드로 보임)에서 오며, 원인은 확인하지 않았습니다. 500세션 턴 뒤 트리 RSS는 Linux 289.2 → 189.9 MB(jcode 159.9 MB), macOS 226.8 → 161.7 MB(jcode 156.8 MB)입니다.
- **macOS one-shot 중앙값은 두 번 잰 결과의 방향이 엇갈립니다.** 표본 20개로 42389421614에서는 이후 215.8 대 이전 219.3 ms, 42397804493에서는 199.4 대 188.7 ms(+5.7%)였고 최솟값은 두 번 모두 같았습니다(174.7 대 173.5, 168.7 대 168.6 ms). 같은 실행의 Linux는 410.6 대 407.6 ms입니다. 회귀로 판정하지 않았지만 호스티드 러너의 잡음 대역이 약 ±6%라는 뜻이기도 합니다.
- **`ab_perf.py` 경로별 A/B는 돌리지 않았습니다.** 이 계획의 시간 비회귀 판정은 위 one-shot 표(같은 러너 번갈아 측정)와 구조 성능 게이트로 했고, `perf_counters_surface`·start→first request 같은 경로별 A/B는 기록에 없습니다.
- **hibernation의 효과는 위 표에 없습니다.** 벤치마크는 턴 직후 잠깐 쉬고 재므로 30분 창이 지나지 않습니다. 해제가 실제로 일어나는지는 `crates/app-core/src/server/idle_hibernation_tests.rs`가 확인합니다.

## `skill-mcp-loading-omp` 계획의 측정

스킬 목록, 작업 폴더 스킬, MCP 사용자 설정 로딩을 바꾼 계획입니다. 판정은 `.github/workflows/loading-ab.yml` 한 번의 실행으로 했습니다. 개발 기기에서 잰 수치는 판정에 쓰지 않았습니다.

### 측정 조건

- 실행: [run 42461323656](https://github.com/horon-ai/agent/actions/runs/42461323656), 2026-11-08, `ubuntu-24.04` 잡 하나에서 두 쪽을 릴리스로 빌드하고 번갈아 잼.
- 기준선 `23007c9b`(로딩 카운터가 처음 들어간 커밋), 후보 `83f2b6c7`. 툴체인 `rustc 1.99.1`.
- `bench_loading.py --mode warm --events 0 --skills 0,200,1000`, 예열 3회, 측정 30회씩. 두 조건: 본문 8 KiB와 `~/.claude.json` 300 KB, 그리고 둘 다 0. 픽스처가 `[mcp] claude_user = true`를 켜서 두 쪽이 같은 파일을 읽습니다.
- 측정 단계 하나는 요청 9회분입니다(`REPEAT_REQUESTS`).

### 카운터: 세 조건 모두 참

| 픽스처 (본문 8 KiB, MCP 300 KB) | 카운터 | 기준선 | 후보 |
| --- | --- | --- | --- |
| 스킬 200개 | `skill_catalog` `skill_bytes_read` | 1,656,290 | 819,200 |
| 스킬 1000개 | `skill_catalog` `skill_bytes_read` | 8,281,890 | 4,096,000 |
| 스킬 0·200·1000개 | `mcp_config_load` `mcp_config_reads` | 9 | 0 |
| 스킬 0·200·1000개 | `skill_request_overlay` `skill_file_reads` | 0 | 0 |

본문이 0인 조건의 `skill_bytes_read`(23,490 / 117,890)는 두 쪽이 같습니다. 앞머리만 있는 파일이라 줄일 본문이 없습니다. `skill_request_overlay`의 파일 읽기는 기준선도 0입니다. 기준선은 요청 범위 안에서 같은 목록을 다시 쓰고, 대신 요청마다 폴더를 다시 훑습니다. 그 비용은 아래 시간에 나옵니다.

### 시간 (p50, 괄호는 p95, ms)

| 단계 | 조건 | 기준선 | 후보 |
| --- | --- | --- | --- |
| `skill_request_overlay` | 스킬 200, 본문 8 KiB | 28.30 (29.09) | 4.79 (4.96) |
| `skill_request_overlay` | 스킬 1000, 본문 8 KiB | 149.03 (153.92) | 26.46 (27.83) |
| `skill_catalog` | 스킬 200, 본문 8 KiB | 4.41 (4.53) | 3.00 (3.11) |
| `skill_catalog` | 스킬 1000, 본문 8 KiB | 21.88 (22.64) | 15.00 (15.32) |
| `skill_catalog` | 스킬 1000, 본문 0 | 15.71 (15.98) | 14.82 (15.28) |
| `mcp_config_load` | MCP 300 KB | 3.46 (3.71) | 0.22 (0.26) |

`bench_tools.py`의 14개 사례는 모두 후보 중앙값이 기준선과 같거나 작습니다(표본 5개씩, 같은 잡).

### 측정 중에 고친 것

- 첫 실행([run 42458666340](https://github.com/horon-ai/agent/actions/runs/42458666340))에서 본문 0 조건의 `skill_catalog`가 기준선 p95를 넘었습니다(스킬 1000개 9.65 → 13.52 ms). 원인은 이름 충돌 처리가 스킬마다 앞서 받은 목록을 처음부터 훑던 것과, 충돌이 없어도 스킬마다 namespace 문자열과 비교 키를 만들던 것입니다. `abd7618e`, `83f2b6c7`에서 이름 색인, 충돌 때만 namespace 계산, 스레드 하나 줄이기, 첫 4 KiB 안의 앞머리는 힙 버퍼 없이 파싱하기로 고쳤고, 위 표가 고친 뒤의 결과입니다.
- 벤치 스크립트의 MCP 픽스처 확인이 "측정 단계에서 파일을 다시 읽었는가"를 보던 것을 "그 파일의 서버가 로드됐는가"로 바꿨습니다(`b3a053b3`). 파싱 캐시가 바뀌지 않은 파일을 다시 읽지 않는 것이 이번 변경의 목적이기 때문입니다.

### 이 숫자들이 말하지 않는 것

- 실행마다 러너가 달라 같은 기준선도 실행 사이에 수 ms씩 움직였습니다(본문 0, 스킬 1000개 `skill_catalog` 기준선 9.65 / 15.62 / 15.71 ms). 비교는 같은 실행 안의 두 쪽끼리만 했습니다.
- 콜드 시작(파일 캐시가 빈 상태)은 재지 않았습니다.
- Windows는 ctime이 없어, 같은 크기로 고치고 수정 시각을 되돌린 스킬 파일은 다음 요청에 보이지 않을 수 있습니다(`docs/runtime/PROMPT.md`).

## fsync 감축과 jcode와의 구조적 차이 (#47)

세 변경을 넣었습니다. 명시 선택을 한 번도 하지 않은 세션은 `local-head-explicit.json`에 `false`를 쓰지 않고(A), 세션 writer는 이미 있는 `sessions/` 루트의 부모를 열 때마다 다시 flush하지 않습니다(B). 반대로 영속 런타임 저널은 새로 만든 폴더의 항목을 부모에 확정하도록 고쳤습니다(C). C 전에는 정전 뒤 이미 `Ok`를 돌려준 `runtime.jsonl`이 폴더째 사라질 수 있었습니다. 계약은 [`../protocol/SESSION_STORE.ko.md`](../protocol/SESSION_STORE.ko.md)에 있습니다. 측정은 2026-11-09 `benchmark.yml` 실행 42531992327이고, 기준 `dev`(`c3e3ceeb`)와 후보(`d9d2c836`)를 한 잡 안에서 같은 러너로 번갈아 쟀습니다. one-shot 표본은 5개입니다.

| 지표 | 환경 | 이전 `c3e3ceeb` | 이후 `d9d2c836` | jcode v0.99.1 |
| --- | --- | --- | --- | --- |
| one-shot fsync·fdatasync 호출 (warm) | GitHub Linux 러너 | 16 | 15 | 5 |
| one-shot 시간 중앙값 (최소–최대) | GitHub Linux 러너 | 60.5 ms (54.2–133.1) | 57.7 ms (54.3–111.4) | 146.9 ms |
| one-shot 시간 중앙값 (최소–최대) | GitHub macOS 러너 | 98.3 ms (90.3–389.4) | 115.1 ms (96.1–393.0) | 362.4 ms |
| one-shot 시간 중앙값 (최소–최대) | GitHub Windows 러너 | 257.5 ms (235.0–562.6) | 284.8 ms (225.8–468.6) | 146.9 ms |
| 스킬 호출 시간 중앙값 (최소–최대) | GitHub Windows 러너 | 243.3 ms (220.2–260.9) | 264.6 ms (235.0–366.3) | 205.9 ms |
| MCP 호출 시간 중앙값 (최소–최대) | GitHub Linux 러너 | 72.3 ms (70.2–144.0) | 101.8 ms (72.3–288.0) | 169.0 ms |
| MCP 호출 시간 중앙값 (최소–최대) | GitHub Windows 러너 | 287.5 ms (276.9–302.2) | 299.3 ms (277.5–350.7) | 317.8 ms |
| 세션 하나가 늘 때 메모리, 턴 뒤 | GitHub Linux 러너 | 516.7 KB | 404.2 KB | 218.0 KB |
| 세션 하나가 늘 때 메모리, 턴 뒤 | GitHub Windows 러너 | 378.5 KB | 353.8 KB | 324.9 KB |

시간 칸의 이전·이후 차이는 모두 최소–최대 범위 안에 있으므로 동률로 읽습니다. 이 변경은 시간 경로에 flush를 더하지 않고 줄이기만 합니다(C의 flush는 저널 폴더를 처음 만들 때 한 번뿐이고 append마다 들지 않습니다).

Windows one-shot과 스킬 호출은 이 변경과 관계없이 jcode보다 느립니다(#44, #45). 이 표는 그 차이를 줄였다고 말하지 않습니다.

### 남는 10회 차이는 구조적 차이입니다

- jcode의 one-shot fsync는 전부 사용량 통계 SQLite(`model-usage-v1.sqlite3`, WAL)에서 나옵니다. 대화 기록(세션 저널과 스냅샷)은 `append_json_line_fast`와 `write_json_fast`로 쓰며 flush하지 않습니다. 그래서 jcode는 프로세스 충돌에는 안전하지만 전원 차단 시 커널이 아직 내려쓰지 않은 대화를 잃습니다.
- horon의 15회는 모두 원본의 확정입니다. 이벤트 로그 커밋마다 1회, 세션 생성 때 폴더 항목, 로컬 상태 파일(`local-runtime.json`)과 그 폴더, 영속 런타임 저널과 그 폴더입니다.
- AGENTS.md 방침에 따라 다음은 넣지 않았습니다. 턴 끝에 한 번만 flush하는 그룹 커밋(도구 결과 기록이 확정 전 상태로 남음), 비동기 flush 스레드(`Ok` 뒤에 확정되므로 계약 약화), `sync_file_range`와 `O_DSYNC`(내구성이 아니거나 횟수가 늘어남).
- 더 줄이려면 로컬 상태 파일과 이벤트 로그를 한 기록으로 합치는 저장 형식 변경이 필요합니다. 별도 설계 과제입니다.

### 이 숫자들이 말하지 않는 것

- **fsync 호출 수는 Linux에서만 셉니다.** macOS에는 `strace`가 없습니다. 세 변경은 macOS `F_FULLFSYNC`와 Windows `FlushFileBuffers`에도 같은 비율로 적용되지만 그 횟수는 재지 않았습니다.
- **첫 실행(cold)은 1회 늘 수 있습니다.** C는 `durable-runtime/`을 처음 만들 때 홈 폴더 항목도 확정하므로, 홈마다 한 번 더 듭니다.

## 닫기 저장과 세션당 메모리: 원인 진단과 구조적 차이 (#44, #45, #47, #48)

Windows 단발 실행·스킬 호출의 열세와 세션당 메모리 증가를 진단 기록으로 나눈 뒤, 데이터 안전을 깎지 않는 수정 하나만 넣었습니다. 실행 끝의 닫기는 상태만 바꾸는데도, 보조 기록(`AuxiliaryRecorded`)에 상태·pid·마지막 활동 시각(`runtime_history`)이 함께 들어 있어서 매번 이벤트 로그 커밋, 바인딩 확정, 파생 캐시 재작성을 모두 했습니다. 이 값은 읽는 곳이 없고 원본은 `local-runtime.json`이므로 보조 기록에서 뺐습니다([`../protocol/SESSION_STORE.ko.md`](../protocol/SESSION_STORE.ko.md)). 측정은 2026-11-09 `benchmark.yml` 실행 42572765871이고, 기준 `dev`(`fca59cd6`)와 후보(`1dd5cd2d`)를 한 잡 안에서 같은 러너로 번갈아 쟀습니다. 표본은 5개입니다.

| 지표 | 환경 | 이전 | 이후 | jcode v0.99.1 |
| --- | --- | --- | --- | --- |
| one-shot fsync·fdatasync 호출 (warm) | GitHub Linux 러너 | 15 | 14 | 5 |
| one-shot 마지막 요청 → 종료 (중앙값) | GitHub Linux 러너 | 11.5 ms | 8.2 ms | 4.4 ms |
| one-shot 마지막 요청 → 종료 (중앙값) | GitHub Windows 러너 | 61.8 ms | 46.5 ms | 11.9 ms |
| one-shot 시간 중앙값 (최소–최대) | GitHub Linux 러너 | 61.5 ms (60–63) | 58.7 ms (57–100) | 144.9 ms |
| one-shot 시간 중앙값 (최소–최대) | GitHub Windows 러너 | 255.8 ms (224–303) | 224.3 ms (220–305) | 151.7 ms |
| 스킬 호출 시간 중앙값 (최소–최대) | GitHub Linux 러너 | 57.0 ms (55–60) | 53.4 ms (52–54) | 56.3 ms (55–58) |
| 스킬 호출 시간 중앙값 (최소–최대) | GitHub Windows 러너 | 260.5 ms (256–306) | 250.6 ms (233–263) | 232.7 ms (211–256) |
| MCP 호출 시간 중앙값 (최소–최대) | GitHub Windows 러너 | 301.6 ms (281–363) | 269.8 ms (255–279) | 337.2 ms |

Linux 스킬 호출은 범위가 겹치지 않아 jcode보다 빠르게 되었습니다. Windows 시간 칸은 이전·이후 범위가 겹치므로 개선 방향으로만 읽습니다.

### Windows 종료 구간의 남은 몫

진단 빌드(실행 42568847961, 구간별 시간 로그)로 나눈 Windows one-shot 종료 구간은 최종 응답 저장 15 ms(이벤트 로그 확정 약 8 ms, 파생 캐시 재작성 약 4.5 ms), `run.stopped` 저널 기록 10 ms, 닫기 저장 18 ms, 프로세스 정리 약 4 ms였습니다. 이번 수정은 닫기 저장만 바인딩 확정 하나로 줄였습니다. 남은 몫은 아래 이유로 줄이지 않았습니다.

- 이벤트 로그와 `run.stopped` 저널은 원본입니다. Windows `FlushFileBuffers`는 호출마다 수 ms가 들지만 확정을 빼면 정전 시 대화와 실행 상태를 잃습니다. jcode는 대화 기록을 확정하지 않으므로 이 차이는 구조적 차이입니다.
- 커밋마다 다시 쓰는 파생 캐시(약 4.3 ms, flush 없음)는 턴 중간에 붙는 다른 클라이언트가 읽으므로 쓰는 주기를 바꾸지 않았습니다.
- 도구 왕복의 bash 실행(Windows 약 23 ms)은 벤치마크의 `ls`를 `cmd.exe /D /S /C`로 실행하는 비용입니다. 셸을 바꾸지 않고는 줄일 수 없습니다.
- 시작 구간의 첫 HTTP 클라이언트 생성은 5 ms로, 인증서 저장소 읽기가 원인이라는 추정은 측정으로 기각했습니다.

### 세션당 메모리 (#48)

실행 42568289247(Linux, 턴 직후, 세션 1·5·10의 기울기)에서 세션 하나가 늘 때 RSS 477 KB 가운데 익명 메모리가 440 KB, 할당자가 살아 있다고 보고한 데이터가 234 KB, `allocator:purge` 뒤 RSS가 417 KB였습니다. heaptrack(진단 실행 42571121373)으로 세션 1개와 10개를 비교하면 살아 있는 힙이 세션당 약 247 KB 늘고, 가장 큰 증가 지점도 세션 생성 29 KB, 잠근 도구 목록 10 KB, 이벤트 위치 색인 8 KB 정도로 모두 세션이 실제로 쓰는 데이터입니다. 누수는 없습니다.

- 반환해도 60 KB만 줄었으므로, 나머지 약 200 KB는 할당자 영역 안의 단편화입니다. 반환을 더 자주 해도 효과가 작습니다.
- macOS에서는 살아 있는 데이터 증가가 horon 140 KB로 jcode 191 KB보다 작고, 이번 실행의 Windows 턴 뒤 증가도 horon 310 KB로 jcode 396 KB보다 작습니다. Linux 차이는 할당자(glibc)의 단편화에서 오며, 할당자를 바꾸는 것은 다른 경로의 회귀 위험이 커서 이번에 하지 않았습니다.

## 자동 측정: `main` 최신 측정 커밋

`main` 에 올라간 커밋이 CI 와 구조 성능 게이트를 모두 통과하고 직전 측정 이후 에이전트 코드(`src`, `crates`, `Cargo.toml`, `Cargo.lock`, `.cargo`, `rust-toolchain.toml`, `scripts/perf`)가 바뀌었으면, `.github/workflows/benchmark-docs.yml` 이 Linux·macOS·Windows에서 전체 벤치마크 묶음을 OS마다 따로 돌리고 아래 구간을 `main` 과 `dev` 에서 함께 다시 씁니다. OS마다 절이 하나씩 있습니다. 측정 방법과 묶음별 뜻은 [`BENCHMARK.ko.md`](BENCHMARK.ko.md) 의 "자동 측정"에 있습니다.

<!-- bench-auto:start commit=5b83dc36c46688abc5dbadc0f02ca99520488a11 -->

이 구간은 `.github/workflows/benchmark-docs.yml` 이 자동으로 다시 씁니다. 손으로 고치지 않습니다.

- 커밋: `5b83dc36c46688abc5dbadc0f02ca99520488a11`
- 측정 시각: 2026-11-10 00:39 UTC
- 실행: <https://github.com/horon-ai/agent/actions/runs/42723443025> (원본 JSON 은 OS별 `benchmark-<os>-<run_id>` 아티팩트)

OS마다 다른 GitHub 호스티드 러너에서 따로 잽니다. 기계가 다르므로 OS끼리 절대값을 비교하지 않고, 같은 OS 안의 상대 비교(horon 대비 배수)와 커밋 사이의 추세만 읽습니다.

| OS | 러너 | 바이너리 | 상태 |
| --- | --- | --- | --- |
| Linux | `ubuntu-24.04` | `horon-agent v0.1.0-dev (5b83dc3)` | 성공 |
| macOS | `macos-latest` | `horon-agent v0.1.0-dev (5b83dc3)` | 성공 |
| Windows | `windows-latest` | `horon-agent v0.1.0-dev (5b83dc3)` | 성공 |

### Linux

#### Coding agent comparison (headless, mock model)

Host: linux/x86_64, 4 CPUs, Python 3.12.16. Baseline: **horon**.

| Tool | Version | `--version` (median) | Install size |
|---|---|---|---|
| horon | horon-agent v0.1.0-dev (5b83dc3) | 3.4 ms | 110.7 MB |
| jcode | jcode v0.99.1 (7206efd0c) | 7.8 ms | 163.4 MB |
| Claude Code | 2.1.317 (Claude Code) | 6.8 ms | 632.2 MB |
| Codex CLI | codex-cli 0.166.0 | 32.9 ms | 435.0 MB |
| OpenCode | 1.19.5 | 386.7 ms | 613.4 MB |
| pi | 1.5.0 | 184.3 ms | 108.9 MB |
| oh-my-pi (omp) | omp/18.18.0 | 589.8 ms | 412.6 MB |
| oh-my-openagent (omo) | omo 5.1.64 (engine: senpi 2026.11.5-1) | 94.6 ms | 520.6 MB |

**One-shot headless prompt** (includes startup, one `ls` tool call, final answer)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Tool round-trip | fsync calls | Note |
|---|---|---|---|---|---|---|---|---|
| horon | 66.4 ms | 52.0 ms–302.5 ms | baseline | 39.1 MB | baseline | yes | 14 | — |
| jcode | 139.4 ms | 139.3 ms–143.3 ms | 2.10× more | 42.2 MB | 1.08× more | yes | 5 | — |
| Claude Code | 384.7 ms | 373.9 ms–394.4 ms | 5.79× more | 206.3 MB | 5.27× more | yes | — | — |
| Codex CLI | 1.29 s | 624.0 ms–1.57 s | 19.47× more | 202.3 MB | 5.17× more | yes | 6 | — |
| OpenCode | 2.18 s | 2.17 s–2.45 s | 32.89× more | 596.4 MB | 15.25× more | yes | 4 | — |
| pi | 304.0 ms | 301.7 ms–311.4 ms | 4.58× more | 114.1 MB | 2.92× more | yes | — | — |
| oh-my-pi (omp) | 1.50 s | 1.47 s–1.67 s | 22.64× more | 356.4 MB | 9.11× more | yes | 67 | — |
| oh-my-openagent (omo) | 5.12 s | 4.91 s–5.49 s | 77.11× more | 665.9 MB | 17.02× more | yes | 36 | — |

**Skill invocation** (process start to final answer, including skill discovery and one load of the fixture skill)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | Note |
|---|---|---|---|---|---|---|---|---|
| horon | 48.0 ms | 47.1 ms–54.6 ms | baseline | 34.3 MB | baseline | skill-uri | yes | — |
| jcode | 50.0 ms | 49.0 ms–50.6 ms | 1.04× more | 40.7 MB | 1.19× more | skill-tool | yes | — |
| Claude Code | 359.4 ms | 356.2 ms–370.8 ms | 7.48× more | 203.8 MB | 5.94× more | skill-tool | yes | — |
| Codex CLI | 1.10 s | 886.2 ms–1.64 s | 22.85× more | 202.0 MB | 5.88× more | file-read | yes | — |
| OpenCode | 2.65 s | 2.64 s–2.70 s | 55.17× more | 596.8 MB | 17.38× more | skill-tool | yes | — |
| pi | 412.2 ms | 411.6 ms–412.8 ms | 8.59× more | 125.7 MB | 3.66× more | file-read | yes | — |
| oh-my-pi (omp) | 1.49 s | 1.47 s–1.51 s | 31.07× more | 339.3 MB | 9.88× more | file-read | yes | — |
| oh-my-openagent (omo) | 5.64 s | 5.29 s–5.87 s | 117.38× more | 659.7 MB | 19.21× more | file-read | yes | — |

**MCP tool call** (process start to final answer, including starting the fixture stdio MCP server and one `bench_echo` call)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | MCP ready | Note |
|---|---|---|---|---|---|---|---|---|---|
| horon | 65.4 ms | 63.1 ms–65.4 ms | baseline | 43.7 MB | baseline | mcp-call | yes | 50.0 ms | — |
| jcode | 166.0 ms | 165.4 ms–166.8 ms | 2.54× more | 53.9 MB | 1.23× more | mcp-action | yes | 140.8 ms | — |
| Claude Code | 450.5 ms | 449.8 ms–457.0 ms | 6.89× more | 225.2 MB | 5.15× more | mcp-tool | yes | 325.7 ms | — |
| Codex CLI | 804.1 ms | 704.9 ms–910.1 ms | 12.30× more | 200.4 MB | 4.59× more | mcp-tool | yes | 284.9 ms | — |
| OpenCode | 2.39 s | 2.38 s–2.41 s | 36.53× more | 573.1 MB | 13.11× more | mcp-tool | yes | 2.20 s | — |
| pi | 449.8 ms | 441.5 ms–455.9 ms | 6.88× more | 139.5 MB | 3.19× more | mcp-tool | yes | 397.4 ms | — |
| oh-my-pi (omp) | 1.51 s | 1.46 s–1.51 s | 23.04× more | 353.2 MB | 8.08× more | mcp-route | yes | 1.41 s | — |
| oh-my-openagent (omo) | 5.63 s | 5.48 s–5.65 s | 86.05× more | 662.9 MB | 15.17× more | mcp-tool | yes | 2.81 s | — |

**Session memory** (idle after 3 turn(s) per session; sizes are process-tree sums, RSS; PSS also recorded on Linux). "Each additional" is the least-squares slope across the measured session counts; 0 B means growth stayed below RSS sampling noise. Threads and descriptors come from the daemon's own `server:memory` after the turns.

| Tool | First session | Each additional | vs horon (additional) | Threads/session | FDs/session | First (post-turn) | Each additional (post-turn) |
|---|---|---|---|---|---|---|---|
| horon | 36.5 MB | 138.1 KB | baseline | 0.44 | 1.00 | 43.7 MB | 417.9 KB |
| jcode | 43.2 MB | 50.8 KB | 2.72× less | 0.11 | — | 46.6 MB | 205.5 KB |
| Claude Code | 151.8 MB | 149.5 MB | 1,108× more | — | — | 187.3 MB | 180.7 MB |
| Codex CLI | 128.5 MB | 133.3 MB | 988.16× more | — | — | 162.3 MB | 164.9 MB |
| OpenCode | 243.0 MB | 543.0 KB | 3.93× more | — | — | 448.0 MB | 22.0 MB |
| pi | 106.8 MB | 108.6 MB | 805.44× more | — | — | 129.8 MB | 124.5 MB |
| oh-my-pi (omp) | 334.4 MB | 84.0 MB | 622.56× more | — | — | 356.6 MB | 357.1 MB |
| oh-my-openagent (omo) | 292.8 MB | 35.1 MB | 260.30× more | — | — | 585.4 MB | 606.6 MB |

Threads/descriptors unavailable: jcode (server:memory did not report fd_count)

**Post-turn tree RSS per session count**

| Tool | 1 session(s) | 5 session(s) | 10 session(s) |
|---|---|---|---|
| horon | 43.7 MB | 46.3 MB | 47.5 MB |
| jcode | 46.6 MB | 47.6 MB | 48.4 MB |
| Claude Code | 187.3 MB | 922.1 MB | 1.8 GB |
| Codex CLI | 162.3 MB | 812.4 MB | 1.6 GB |
| OpenCode | 448.0 MB | 460.8 MB | 641.0 MB |
| pi | 129.8 MB | 636.9 MB | 1.2 GB |
| oh-my-pi (omp) | 356.6 MB | 1.8 GB | 3.5 GB |
| oh-my-openagent (omo) | 585.4 MB | 3.0 GB | 5.9 GB |

#### 기동

```text
Benchmarking: /home/runner/work/agent/agent/bench-bin/horon-agent
============================================================

--help (binary load):
  Min:    3.19 ms
  Max:    3.45 ms
  Mean:   3.29 ms
  Median: 3.27 ms
  Stdev:  0.09 ms

--version:
  Min:    3.01 ms
  Max:    3.13 ms
  Mean:   3.08 ms
  Median: 3.08 ms
  Stdev:  0.05 ms

Measuring isolated server startup (10 runs)...

Server ready (isolated socket connectable):
  Min:    20.46 ms
  Max:    20.58 ms
  Mean:   20.50 ms
  Median: 20.48 ms
  Stdev:  0.04 ms

Measuring harness bridge readiness (10 runs)...

Bridge stage: api socket connectable:
  Min:    20.47 ms
  Max:    20.67 ms
  Mean:   20.62 ms
  Median: 20.64 ms
  Stdev:  0.06 ms

Bridge stage: hello_ok:
  Min:    21.01 ms
  Max:    21.31 ms
  Mean:   21.18 ms
  Median: 21.17 ms
  Stdev:  0.08 ms

Bridge stage: no-op reply:
  Min:    21.36 ms
  Max:    21.66 ms
  Mean:   21.54 ms
  Median: 21.53 ms
  Stdev:  0.08 ms

Bridge total (socket -> no-op):
  Min:    21.36 ms
  Max:    21.66 ms
  Mean:   21.54 ms
  Median: 21.53 ms
  Stdev:  0.08 ms

============================================================
Summary:
  Binary load median:      ~3.3 ms
  Server ready median:     ~20.5 ms
  Bridge total median:     ~21.5 ms
```

#### 세션 로딩 (COLD)

```text
binary   /home/runner/work/agent/agent/bench-bin/examples/loading_probe (53e241d3845a)
mode     cold
protocol COLD: every measured run gets its own freshly copied HOME; no derived cache from an earlier run exists, so each sample is a first load of the fixture.
fixtures /tmp/horon-loading-bench-m3x9sjsh
note     --warmup 3 is ignored in cold mode: every sample gets a fresh HOME
fixture events=0 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    0.015/0.015    p95    0.015/0.027    delta  -0.000 ms  control-no-change
  skill_request_overlay p50    0.105/0.106    p95    0.119/0.121    delta  +0.001 ms  control-no-change
  mcp_config_load   p50    0.144/0.144    p95    0.181/0.182    delta  +0.000 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  session_save      p50    7.940/7.829    p95    9.416/8.076    delta  -0.111 ms  control-no-change
  total store_read_bytes p50 23074  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 23052..23088 (7 distinct values)
  SPREAD   b session_save store_read_bytes 23056..23084 (7 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    2.680/2.682    p95    2.763/2.719    delta  +0.002 ms  control-no-change
  skill_request_overlay p50    4.643/4.654    p95    4.940/4.720    delta  +0.010 ms  control-no-change
  mcp_config_load   p50    0.150/0.148    p95    0.170/0.160    delta  -0.002 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  session_save      p50   10.448/10.219   p95   15.883/14.346   delta  -0.229 ms  control-no-change
  total store_read_bytes p50 23072  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 23068..23080 (4 distinct values)
  SPREAD   b session_save store_read_bytes 23068..23084 (5 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.015/0.015    p95    0.015/0.016    delta  +0.000 ms  control-no-change
  skill_request_overlay p50    0.105/0.105    p95    0.135/0.121    delta  -0.000 ms  control-no-change
  mcp_config_load   p50    0.145/0.145    p95    0.175/0.178    delta  -0.000 ms  control-no-change
  attach_history    p50  292.287/290.663  p95  297.372/294.677  delta  -1.624 ms  control-no-change
  resume_ready      p50   52.916/53.140   p95   53.611/53.801   delta  +0.224 ms  control-no-change
  provider_dispatch p50    1.333/1.330    p95    1.352/1.348    delta  -0.002 ms  control-no-change
  session_save      p50    7.475/7.383    p95    8.316/7.992    delta  -0.092 ms  control-no-change
  total store_read_bytes p50 5311392  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189187..4189191 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099129..1099133 (3 distinct values)
  SPREAD   a session_save store_read_bytes 23062..23094 (8 distinct values)
  SPREAD   b attach_history store_read_bytes 4189187..4189191 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099129..1099133 (3 distinct values)
  SPREAD   b session_save store_read_bytes 23061..23078 (6 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    2.677/2.667    p95    2.700/2.764    delta  -0.010 ms  control-no-change
  skill_request_overlay p50    4.636/4.675    p95    4.685/4.751    delta  +0.040 ms  control-no-change
  mcp_config_load   p50    0.148/0.148    p95    0.161/0.155    delta  -0.000 ms  control-no-change
  attach_history    p50  294.034/294.008  p95  300.292/296.932  delta  -0.026 ms  control-no-change
  resume_ready      p50   53.426/53.231   p95   54.374/54.209   delta  -0.195 ms  control-no-change
  provider_dispatch p50    1.311/1.311    p95    1.367/1.337    delta  +0.000 ms  control-no-change
  session_save      p50    7.345/7.481    p95    7.608/7.535    delta  +0.136 ms  control-no-change
  total store_read_bytes p50 5311394  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189183..4189191 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099125..1099133 (2 distinct values)
  SPREAD   a session_save store_read_bytes 23062..23082 (6 distinct values)
  SPREAD   b attach_history store_read_bytes 4189187..4189191 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099129..1099133 (3 distinct values)
  SPREAD   b session_save store_read_bytes 23058..23085 (8 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.015/0.015    p95    0.033/0.015    delta  -0.000 ms  control-no-change
  skill_request_overlay p50    0.106/0.105    p95    0.107/0.106    delta  -0.000 ms  control-no-change
  mcp_config_load   p50    0.143/0.144    p95    0.145/0.156    delta  +0.000 ms  control-no-change
  attach_history    p50 3759.549/3723.891 p95 3778.085/3755.222 delta -35.658 ms  control-no-change
  resume_ready      p50  577.433/576.007  p95  589.691/594.669  delta  -1.426 ms  control-no-change
  provider_dispatch p50   14.360/14.270   p95   15.074/14.646   delta  -0.090 ms  control-no-change
  session_save      p50    7.758/7.702    p95    8.112/8.228    delta  -0.056 ms  control-no-change
  total store_read_bytes p50 53417606  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184284..42184289 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210238..11210243 (4 distinct values)
  SPREAD   a session_save store_read_bytes 23067..23089 (7 distinct values)
  SPREAD   b attach_history store_read_bytes 42184287..42184289 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210242..11210243 (2 distinct values)
  SPREAD   b session_save store_read_bytes 23057..23085 (7 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    2.694/2.686    p95    2.760/2.776    delta  -0.008 ms  control-no-change
  skill_request_overlay p50    4.633/4.671    p95    4.701/4.721    delta  +0.037 ms  control-no-change
  mcp_config_load   p50    0.148/0.148    p95    0.149/0.150    delta  +0.001 ms  control-no-change
  attach_history    p50 3769.200/3773.231 p95 3848.018/3803.927 delta  +4.031 ms  control-no-change
  resume_ready      p50  579.601/577.243  p95  600.012/584.939  delta  -2.358 ms  control-no-change
  provider_dispatch p50   14.866/14.494   p95   16.757/14.977   delta  -0.372 ms  control-no-change
  session_save      p50    7.739/7.765    p95    8.011/8.638    delta  +0.026 ms  control-no-change
  total store_read_bytes p50 53417603  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184285..42184289 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210239..11210243 (2 distinct values)
  SPREAD   a session_save store_read_bytes 23065..23085 (5 distinct values)
  SPREAD   b attach_history store_read_bytes 42184287..42184289 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210242..11210243 (2 distinct values)
  SPREAD   b session_save store_read_bytes 23065..23089 (7 distinct values)
report   bench-results/loading-cold.json
mode cold, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### 세션 로딩 (WARM)

```text
binary   /home/runner/work/agent/agent/bench-bin/examples/loading_probe (53e241d3845a)
mode     warm
protocol WARM: one HOME per side, --warmup discarded runs per side, then measured runs alternate between the two sides on those same HOMEs.
fixtures /tmp/horon-loading-bench-b9qc7_j3
fixture events=0 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    0.015/0.015    p95    0.015/0.017    delta  -0.000 ms  control-no-change
  skill_request_overlay p50    0.105/0.105    p95    0.138/0.135    delta  -0.000 ms  control-no-change
  mcp_config_load   p50    0.144/0.143    p95    0.162/0.145    delta  -0.001 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  session_save      p50    8.191/8.110    p95    9.899/10.782   delta  -0.081 ms  control-no-change
  total store_read_bytes p50 23797  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23061, 23073, 23081, 23779, 23795, 23799, 24505, 24509, 24513, 24517]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23069, 23073, 23081, 23783, 23795, 23799, 24501, 24505, 24509]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23061..24517 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23069..24509 (9 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    2.818/2.814    p95    2.874/2.850    delta  -0.004 ms  control-no-change
  skill_request_overlay p50    4.820/4.781    p95    5.054/4.839    delta  -0.039 ms  control-no-change
  mcp_config_load   p50    0.149/0.149    p95    0.150/0.171    delta  -0.000 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  session_save      p50    9.620/9.673    p95   10.364/10.608   delta  +0.053 ms  control-no-change
  total store_read_bytes p50 23799  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23061, 23069, 23081, 23775, 23795, 23803, 24497, 24505, 24509, 24525]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23059, 23069, 23077, 23781, 23795, 23799, 24495, 24511, 24517]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23061..24525 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23059..24517 (9 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.015/0.015    p95    0.018/0.015    delta  -0.000 ms  control-no-change
  skill_request_overlay p50    0.106/0.106    p95    0.117/0.133    delta  -0.000 ms  control-no-change
  mcp_config_load   p50    0.143/0.144    p95    0.159/0.145    delta  +0.001 ms  control-no-change
  attach_history    p50  100.180/99.663   p95  101.359/102.761  delta  -0.517 ms  control-no-change
  resume_ready      p50   54.058/53.749   p95   54.965/58.935   delta  -0.309 ms  control-no-change
  provider_dispatch p50    1.367/1.358    p95    1.417/1.387    delta  -0.009 ms  control-no-change
  session_save      p50    8.225/8.458    p95    8.327/13.971   delta  +0.234 ms  control-drift
  total store_read_bytes p50 2841365  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23784, 23800, 24494, 24514, 25220, 25242, 25942, 25944, 26668, 26670]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23764, 23786, 24506, 24516, 25230, 25238, 25936, 25940, 26652, 26666]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23784..26670 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23764..26666 (10 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    2.767/2.759    p95    2.822/3.525    delta  -0.008 ms  control-no-change
  skill_request_overlay p50    4.708/4.694    p95    4.769/4.902    delta  -0.014 ms  control-no-change
  mcp_config_load   p50    0.148/0.153    p95    0.156/0.169    delta  +0.005 ms  control-drift
  attach_history    p50  100.004/98.988   p95  101.924/101.176  delta  -1.015 ms  control-no-change
  resume_ready      p50   53.366/53.490   p95   54.207/54.424   delta  +0.124 ms  control-no-change
  provider_dispatch p50    1.319/1.336    p95    1.363/1.360    delta  +0.016 ms  control-no-change
  session_save      p50    9.606/9.621    p95   11.144/10.927   delta  +0.015 ms  control-no-change
  total store_read_bytes p50 2841358  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23776, 23788, 24498, 24510, 25228, 25236, 25950, 26660, 26672]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23790, 23800, 24516, 24518, 25220, 25224, 25950, 25958, 26648, 26672]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23776..26672 (9 distinct values)
  SPREAD   b session_save store_read_bytes 23790..26672 (10 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.015/0.015    p95    0.015/0.017    delta  +0.000 ms  control-no-change
  skill_request_overlay p50    0.106/0.106    p95    0.123/0.121    delta  -0.000 ms  control-no-change
  mcp_config_load   p50    0.143/0.144    p95    0.150/0.160    delta  +0.001 ms  control-no-change
  attach_history    p50 1122.773/1120.722 p95 1139.095/1131.803 delta  -2.051 ms  control-no-change
  resume_ready      p50  615.954/629.026  p95  638.204/653.133  delta +13.072 ms  control-no-change
  provider_dispatch p50   15.492/16.500   p95   17.903/17.592   delta  +1.008 ms  control-no-change
  session_save      p50    8.133/8.227    p95    9.847/69.590   delta  +0.094 ms  control-no-change
  total store_read_bytes p50 28640377  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23790, 24508, 24524, 25226, 25230, 25952, 25956, 26658, 26662]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23780, 23800, 24514, 25226, 25232, 25944, 25950, 26658, 26664]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23790..26662 (9 distinct values)
  SPREAD   b session_save store_read_bytes 23780..26664 (9 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    2.713/2.710    p95    2.814/2.786    delta  -0.003 ms  control-no-change
  skill_request_overlay p50    4.671/4.648    p95    4.784/4.734    delta  -0.022 ms  control-no-change
  mcp_config_load   p50    0.148/0.149    p95    0.161/0.164    delta  +0.000 ms  control-no-change
  attach_history    p50 1100.027/1100.661 p95 1127.828/1134.934 delta  +0.635 ms  control-no-change
  resume_ready      p50  612.823/596.618  p95  638.358/636.591  delta -16.205 ms  control-no-change
  provider_dispatch p50   15.062/15.021   p95   15.886/16.064   delta  -0.041 ms  control-no-change
  session_save      p50    9.491/9.586    p95   10.091/10.684   delta  +0.095 ms  control-no-change
  total store_read_bytes p50 28640389  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23788, 23798, 24510, 24520, 25238, 25242, 25940, 25948, 26658, 26660]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23786, 23798, 24516, 24520, 25234, 25942, 25948, 26654, 26664]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23788..26660 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23786..26664 (9 distinct values)
report   bench-results/loading-warm.json
mode warm, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### Tool latency

| Tool case | median | p95 | min | max | samples |
|---|---|---|---|---|---|
| read (tiny file) | 0.3 ms | 0.4 ms | 0.2 ms | 0.4 ms | 5 |
| read (1000 lines) | 0.4 ms | 0.4 ms | 0.3 ms | 0.4 ms | 5 |
| read (10 lines @ offset) | 0.4 ms | 0.4 ms | 0.3 ms | 0.4 ms | 5 |
| read (repo Cargo.toml) | 0.4 ms | 0.4 ms | 0.3 ms | 0.4 ms | 5 |
| write (small) | 1.5 ms | 4.8 ms | 1.4 ms | 4.8 ms | 5 |
| write (10KB) | 1.9 ms | 2.0 ms | 1.7 ms | 2.0 ms | 5 |
| edit (simple replace) | 1.6 ms | 1.6 ms | 1.5 ms | 1.6 ms | 5 |
| ls (scripts dir) | 0.8 ms | 0.9 ms | 0.6 ms | 0.9 ms | 5 |
| agentgrep (grep in tree) | 1.6 ms | 2.1 ms | 1.5 ms | 2.1 ms | 5 |
| agentgrep (find **/*.rs) | 1.2 ms | 1.2 ms | 1.1 ms | 1.2 ms | 5 |
| bash (true) | 1.7 ms | 1.8 ms | 1.7 ms | 1.8 ms | 5 |
| bash (echo hello) | 1.7 ms | 1.8 ms | 1.6 ms | 1.8 ms | 5 |
| bash (ls fixture tree) | 2.7 ms | 2.8 ms | 2.7 ms | 2.8 ms | 5 |
| invalid tool (dispatch only) | 0.1 ms | 0.1 ms | 0.1 ms | 0.1 ms | 5 |

#### Reload storm

1 resume(s) per level; each level resumes sessions no other level has opened, from a freshly started daemon.

| Concurrent resumes | history median | p95 | max | storm wall | failures |
|---|---|---|---|---|---|
| 1 | 56.4 ms | 56.4 ms | 56.4 ms | 56.6 ms | 0 |
| 10 | 178.4 ms | 210.6 ms | 210.6 ms | 211.1 ms | 0 |
| 40 | 692.8 ms | 892.9 ms | 895.1 ms | 897.2 ms | 0 |

#### Compile loop

| Checkpoint | Touched | median | min | max | samples |
|---|---|---|---|---|---|
| warm_noop | — | 247.6 ms | 247.6 ms | 247.6 ms | 1 |
| leaf_touch | src/main.rs | 390.1 ms | 390.1 ms | 390.1 ms | 1 |
| core_touch | crates/app-core/src/lib.rs | 21.55 s | 21.55 s | 21.55 s | 1 |
| build_meta_touch | crates/build-meta/src/lib.rs | 29.59 s | 29.59 s | 29.59 s | 1 |

### macOS

#### Coding agent comparison (headless, mock model)

Host: darwin/arm64, 3 CPUs, Python 3.12.10. Baseline: **horon**.

Skipped: syscalls (strace not available)

| Tool | Version | `--version` (median) | Install size |
|---|---|---|---|
| horon | horon-agent v0.1.0-dev (5b83dc3) | 13.7 ms | 93.1 MB |
| jcode | jcode v0.99.1 (7206efd0c) | 17.6 ms | 136.3 MB |
| Claude Code | 2.1.317 (Claude Code) | 28.4 ms | 596.8 MB |
| Codex CLI | codex-cli 0.166.0 | 104.6 ms | 346.5 MB |
| OpenCode | 1.19.5 | 628.1 ms | 344.6 MB |
| pi | 1.5.0 | 427.3 ms | 108.9 MB |
| oh-my-pi (omp) | omp/18.18.0 | 730.4 ms | 273.0 MB |
| oh-my-openagent (omo) | omo 5.1.64 (engine: senpi 2026.11.5-1) | 197.0 ms | 502.2 MB |

**One-shot headless prompt** (includes startup, one `ls` tool call, final answer)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Tool round-trip | Note |
|---|---|---|---|---|---|---|---|
| horon | 211.4 ms | 186.0 ms–802.2 ms | baseline | 31.4 MB | baseline | yes | — |
| jcode | 428.1 ms | 390.2 ms–716.3 ms | 2.02× more | 34.3 MB | 1.09× more | yes | — |
| Claude Code | 611.2 ms | 608.7 ms–1.21 s | 2.89× more | 197.1 MB | 6.28× more | yes | — |
| Codex CLI | 1.24 s | 1.12 s–3.10 s | 5.86× more | 144.6 MB | 4.61× more | yes | — |
| OpenCode | 4.32 s | 4.23 s–8.91 s | 20.42× more | 762.9 MB | 24.30× more | yes | — |
| pi | 592.2 ms | 519.0 ms–610.3 ms | 2.80× more | 123.3 MB | 3.93× more | yes | — |
| oh-my-pi (omp) | 2.10 s | 1.85 s–2.41 s | 9.94× more | 411.5 MB | 13.11× more | yes | — |
| oh-my-openagent (omo) | 9.14 s | 8.32 s–9.85 s | 43.25× more | 902.2 MB | 28.74× more | yes | — |

**Skill invocation** (process start to final answer, including skill discovery and one load of the fixture skill)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | Note |
|---|---|---|---|---|---|---|---|---|
| horon | 122.4 ms | 101.0 ms–131.7 ms | baseline | 22.7 MB | baseline | skill-uri | yes | — |
| jcode | 307.0 ms | 277.1 ms–342.9 ms | 2.51× more | 25.4 MB | 1.12× more | skill-tool | yes | — |
| Claude Code | 1.23 s | 1.13 s–1.27 s | 10.02× more | 203.2 MB | 8.96× more | skill-tool | yes | — |
| Codex CLI | 817.0 ms | 693.1 ms–825.2 ms | 6.68× more | 134.8 MB | 5.94× more | file-read | yes | — |
| OpenCode | 4.31 s | 4.22 s–4.77 s | 35.19× more | 714.1 MB | 31.47× more | skill-tool | yes | — |
| pi | 645.8 ms | 605.9 ms–664.5 ms | 5.28× more | 133.8 MB | 5.90× more | file-read | yes | — |
| oh-my-pi (omp) | 2.31 s | 2.06 s–2.55 s | 18.86× more | 400.7 MB | 17.66× more | file-read | yes | — |
| oh-my-openagent (omo) | 10.28 s | 10.16 s–12.81 s | 83.99× more | 886.7 MB | 39.08× more | file-read | yes | — |

**MCP tool call** (process start to final answer, including starting the fixture stdio MCP server and one `bench_echo` call)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | MCP ready | Note |
|---|---|---|---|---|---|---|---|---|---|
| horon | 157.1 ms | 125.5 ms–168.7 ms | baseline | 19.3 MB | baseline | mcp-call | yes | 111.6 ms | — |
| jcode | 420.4 ms | 354.2 ms–442.2 ms | 2.68× more | 52.7 MB | 2.73× more | mcp-action | yes | 387.2 ms | — |
| Claude Code | 1.18 s | 755.1 ms–1.37 s | 7.50× more | 219.4 MB | 11.36× more | mcp-tool | yes | 807.4 ms | — |
| Codex CLI | 1.05 s | 672.0 ms–1.24 s | 6.69× more | 142.4 MB | 7.37× more | mcp-tool | yes | 693.4 ms | — |
| OpenCode | 5.47 s | 5.39 s–6.24 s | 34.82× more | 693.5 MB | 35.91× more | mcp-tool | yes | 4.75 s | — |
| pi | 677.8 ms | 643.9 ms–776.2 ms | 4.31× more | 150.5 MB | 7.79× more | mcp-tool | yes | 600.1 ms | — |
| oh-my-pi (omp) | 2.08 s | 1.97 s–2.20 s | 13.21× more | 413.8 MB | 21.43× more | mcp-route | yes | 1.89 s | — |
| oh-my-openagent (omo) | — | — | — | — | — | mcp-tool | no | — | failed: the fixture sentinel never came back |

**Session memory** (idle after 3 turn(s) per session; sizes are process-tree sums, RSS; PSS also recorded on Linux). "Each additional" is the least-squares slope across the measured session counts; 0 B means growth stayed below RSS sampling noise. Threads and descriptors come from the daemon's own `server:memory` after the turns.

| Tool | First session | Each additional | vs horon (additional) | Threads/session | FDs/session | First (post-turn) | Each additional (post-turn) |
|---|---|---|---|---|---|---|---|
| horon | 28.4 MB | 176.5 KB | baseline | 0.45 | 1.00 | 34.2 MB | 373.9 KB |
| jcode | 36.5 MB | 219.8 KB | 1.25× more | — | — | 40.2 MB | 313.6 KB |
| Claude Code | 156.8 MB | 134.4 MB | 779.91× more | — | — | 188.3 MB | 180.3 MB |
| Codex CLI | 88.4 MB | 81.3 MB | 471.53× more | — | — | 142.4 MB | 103.7 MB |
| OpenCode | 262.1 MB | 318.4 KB | 1.80× more | — | — | 548.9 MB | 13.2 MB |
| pi | 119.7 MB | 115.5 MB | 669.81× more | — | — | 149.2 MB | 147.9 MB |
| oh-my-pi (omp) | 375.4 MB | 136.3 MB | 790.65× more | — | — | 417.9 MB | 321.6 MB |
| oh-my-openagent (omo) | 59.7 MB | 43.5 MB | 252.30× more | — | — | 814.2 MB | 330.6 MB |

Threads/descriptors unavailable: jcode (server:memory did not report thread_count, fd_count)

**Post-turn tree RSS per session count**

| Tool | 1 session(s) | 5 session(s) | 10 session(s) |
|---|---|---|---|
| horon | 34.2 MB | 35.9 MB | 37.5 MB |
| jcode | 40.2 MB | 41.6 MB | 43.0 MB |
| Claude Code | 188.3 MB | 920.9 MB | 1.8 GB |
| Codex CLI | 142.4 MB | 536.3 MB | 1.0 GB |
| OpenCode | 548.9 MB | 597.8 MB | 667.5 MB |
| pi | 149.2 MB | 739.0 MB | 1.4 GB |
| oh-my-pi (omp) | 417.9 MB | 1.8 GB | 3.2 GB |
| oh-my-openagent (omo) | 814.2 MB | 3.1 GB | 3.8 GB |

#### 기동

```text
Benchmarking: /Users/runner/work/agent/agent/bench-bin/horon-agent
============================================================

--help (binary load):
  Min:    9.12 ms
  Max:    21.09 ms
  Mean:   12.91 ms
  Median: 10.41 ms
  Stdev:  4.44 ms

--version:
  Min:    11.27 ms
  Max:    19.21 ms
  Mean:   14.71 ms
  Median: 14.17 ms
  Stdev:  2.67 ms

Measuring isolated server startup (10 runs)...

Server ready (isolated socket connectable):
  Min:    26.34 ms
  Max:    99.09 ms
  Mean:   66.48 ms
  Median: 76.45 ms
  Stdev:  29.54 ms

Measuring harness bridge readiness (10 runs)...

Bridge stage: api socket connectable:
  Min:    38.56 ms
  Max:    117.43 ms
  Mean:   74.67 ms
  Median: 70.68 ms
  Stdev:  24.36 ms

Bridge stage: hello_ok:
  Min:    40.26 ms
  Max:    118.29 ms
  Mean:   75.78 ms
  Median: 71.56 ms
  Stdev:  24.32 ms

Bridge stage: no-op reply:
  Min:    41.15 ms
  Max:    118.76 ms
  Mean:   76.41 ms
  Median: 72.22 ms
  Stdev:  24.17 ms

Bridge total (socket -> no-op):
  Min:    41.15 ms
  Max:    118.76 ms
  Mean:   76.41 ms
  Median: 72.22 ms
  Stdev:  24.17 ms

============================================================
Summary:
  Binary load median:      ~10.4 ms
  Server ready median:     ~76.4 ms
  Bridge total median:     ~72.2 ms
```

#### 세션 로딩 (COLD)

```text
binary   /Users/runner/work/agent/agent/bench-bin/examples/loading_probe (998dc45c31cc)
mode     cold
protocol COLD: every measured run gets its own freshly copied HOME; no derived cache from an earlier run exists, so each sample is a first load of the fixture.
fixtures /var/folders/h5/t85nd63d71x27_8gl5gw3l_w0000gn/T/horon-loading-bench-be5qwqmv
note     --warmup 3 is ignored in cold mode: every sample gets a fresh HOME
fixture events=0 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    0.048/0.050    p95    0.097/0.125    delta  +0.002 ms  control-no-change
  skill_request_overlay p50    0.248/0.350    p95    0.404/0.611    delta  +0.102 ms  control-no-change
  mcp_config_load   p50    0.295/0.418    p95    0.519/0.669    delta  +0.122 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.001    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.001    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.001    delta  +0.000 ms  control-no-change
  session_save      p50   24.561/23.661   p95   32.555/60.806   delta  -0.901 ms  control-no-change
  total store_read_bytes p50 22948  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 22934..22962 (7 distinct values)
  SPREAD   b session_save store_read_bytes 22934..22962 (7 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    4.867/4.484    p95   11.185/9.119    delta  -0.383 ms  control-no-change
  skill_request_overlay p50    7.177/6.400    p95   11.834/9.147    delta  -0.777 ms  control-no-change
  mcp_config_load   p50    0.291/0.301    p95    0.330/0.563    delta  +0.011 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  session_save      p50   24.193/22.515   p95   34.521/27.138   delta  -1.678 ms  control-no-change
  total store_read_bytes p50 22948  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 22942..22954 (5 distinct values)
  SPREAD   b session_save store_read_bytes 22934..22962 (6 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.032/0.039    p95    0.062/0.101    delta  +0.007 ms  control-no-change
  skill_request_overlay p50    0.267/0.313    p95    0.930/0.844    delta  +0.045 ms  control-no-change
  mcp_config_load   p50    0.314/0.306    p95    1.053/0.688    delta  -0.008 ms  control-no-change
  attach_history    p50  384.964/426.101  p95  564.226/735.251  delta +41.137 ms  control-no-change
  resume_ready      p50   75.235/87.870   p95  112.692/127.603  delta +12.635 ms  control-no-change
  provider_dispatch p50    1.492/1.428    p95   24.128/1.982    delta  -0.065 ms  control-no-change
  session_save      p50   25.765/41.612   p95  146.447/227.288  delta +15.847 ms  control-no-change
  total store_read_bytes p50 5311310  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189205..4189207 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099144..1099145 (2 distinct values)
  SPREAD   a session_save store_read_bytes 22944..22971 (8 distinct values)
  SPREAD   b attach_history store_read_bytes 4189205..4189207 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099144..1099145 (2 distinct values)
  SPREAD   b session_save store_read_bytes 22944..22964 (7 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    4.577/4.732    p95    6.929/7.556    delta  +0.155 ms  control-no-change
  skill_request_overlay p50    5.939/5.690    p95   10.146/8.452    delta  -0.248 ms  control-no-change
  mcp_config_load   p50    0.282/0.299    p95    0.356/0.519    delta  +0.016 ms  control-no-change
  attach_history    p50  357.389/352.965  p95  549.469/404.598  delta  -4.424 ms  control-no-change
  resume_ready      p50   77.211/69.497   p95  106.706/97.922   delta  -7.714 ms  control-no-change
  provider_dispatch p50    1.409/1.270    p95    2.976/4.220    delta  -0.139 ms  control-no-change
  session_save      p50   23.168/26.213   p95   42.438/66.970   delta  +3.046 ms  control-no-change
  total store_read_bytes p50 5311304  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189205..4189207 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099144..1099145 (2 distinct values)
  SPREAD   a session_save store_read_bytes 22931..22968 (8 distinct values)
  SPREAD   b attach_history store_read_bytes 4189203..4189207 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099141..1099145 (2 distinct values)
  SPREAD   b session_save store_read_bytes 22948..22968 (7 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.027/0.029    p95    0.118/0.065    delta  +0.002 ms  control-no-change
  skill_request_overlay p50    0.221/0.233    p95    0.295/0.263    delta  +0.012 ms  control-no-change
  mcp_config_load   p50    0.254/0.261    p95    0.324/0.523    delta  +0.007 ms  control-no-change
  attach_history    p50 3896.423/3714.200 p95 4437.078/4159.849 delta -182.224 ms  control-no-change
  resume_ready      p50  828.682/726.905  p95 1034.275/819.001  delta -101.778 ms  control-no-change
  provider_dispatch p50   11.303/10.262   p95   16.053/18.471   delta  -1.041 ms  control-no-change
  session_save      p50   22.066/21.352   p95   54.186/41.620   delta  -0.713 ms  control-no-change
  total store_read_bytes p50 53417519  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184303..42184305 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210254..11210255 (2 distinct values)
  SPREAD   a session_save store_read_bytes 22947..22975 (7 distinct values)
  SPREAD   b attach_history store_read_bytes 42184301..42184305 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210251..11210255 (2 distinct values)
  SPREAD   b session_save store_read_bytes 22947..22967 (5 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    5.217/4.744    p95   10.854/7.448    delta  -0.473 ms  control-no-change
  skill_request_overlay p50    6.028/6.625    p95   33.621/10.699   delta  +0.597 ms  control-no-change
  mcp_config_load   p50    0.287/0.300    p95    0.402/0.845    delta  +0.013 ms  control-no-change
  attach_history    p50 4125.040/4320.475 p95 5325.197/5805.214 delta +195.435 ms  control-no-change
  resume_ready      p50  855.608/775.218  p95 1019.985/1080.706 delta -80.389 ms  control-no-change
  provider_dispatch p50   12.539/11.434   p95   22.314/16.455   delta  -1.105 ms  control-no-change
  session_save      p50   28.032/29.740   p95  106.695/260.842  delta  +1.708 ms  control-no-change
  total store_read_bytes p50 53417511  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184299..42184303 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210250..11210254 (2 distinct values)
  SPREAD   a session_save store_read_bytes 22934..22970 (8 distinct values)
  SPREAD   b attach_history store_read_bytes 42184299..42184303 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210250..11210254 (3 distinct values)
  SPREAD   b session_save store_read_bytes 22952..22974 (8 distinct values)
report   bench-results/loading-cold.json
mode cold, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### 세션 로딩 (WARM)

```text
binary   /Users/runner/work/agent/agent/bench-bin/examples/loading_probe (998dc45c31cc)
mode     warm
protocol WARM: one HOME per side, --warmup discarded runs per side, then measured runs alternate between the two sides on those same HOMEs.
fixtures /var/folders/h5/t85nd63d71x27_8gl5gw3l_w0000gn/T/horon-loading-bench-wyjgv9n8
fixture events=0 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    0.024/0.024    p95    0.033/0.029    delta  -0.000 ms  control-no-change
  skill_request_overlay p50    0.216/0.213    p95    0.229/0.236    delta  -0.003 ms  control-no-change
  mcp_config_load   p50    0.251/0.253    p95    0.264/0.267    delta  +0.002 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.001/0.000    delta  +0.000 ms  control-no-change
  session_save      p50   11.693/11.676   p95   13.265/13.959   delta  -0.017 ms  control-no-change
  total store_read_bytes p50 23669  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [22935, 22947, 22951, 23657, 23665, 23673, 24363, 24371, 24379]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [22947, 22951, 22955, 23653, 23665, 24375, 24383, 24391]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 22935..24379 (9 distinct values)
  SPREAD   b session_save store_read_bytes 22947..24391 (8 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    4.024/4.024    p95    4.925/4.609    delta  -0.001 ms  control-no-change
  skill_request_overlay p50    5.098/5.430    p95    5.772/16.932   delta  +0.332 ms  control-no-change
  mcp_config_load   p50    0.256/0.262    p95    0.437/0.292    delta  +0.006 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.001/0.000    delta  -0.000 ms  control-no-change
  session_save      p50   13.232/14.848   p95   14.843/28.912   delta  +1.616 ms  control-no-change
  total store_read_bytes p50 23667  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [22943, 22955, 23657, 23661, 23673, 24371, 24373, 24383]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [22925, 22935, 22939, 23659, 23661, 23669, 24367, 24369, 24371, 24377]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 22943..24383 (8 distinct values)
  SPREAD   b session_save store_read_bytes 22925..24377 (10 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.022/0.021    p95    0.025/0.028    delta  -0.000 ms  control-no-change
  skill_request_overlay p50    0.210/0.197    p95    0.232/0.217    delta  -0.013 ms  control-no-change
  mcp_config_load   p50    0.237/0.227    p95    0.274/0.247    delta  -0.010 ms  control-no-change
  attach_history    p50   86.498/85.844   p95   91.692/90.597   delta  -0.653 ms  control-no-change
  resume_ready      p50   51.958/53.352   p95   55.338/58.092   delta  +1.394 ms  control-no-change
  provider_dispatch p50    0.925/0.938    p95    0.960/1.007    delta  +0.013 ms  control-no-change
  session_save      p50    9.987/10.002   p95   11.067/11.306   delta  +0.015 ms  control-no-change
  total store_read_bytes p50 2841256  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23672, 23678, 24372, 24394, 25094, 25102, 25790, 25808, 26512, 26516]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23674, 24378, 24388, 25092, 25098, 25806, 25812, 26526, 26532]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23672..26516 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23674..26532 (9 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    3.497/3.650    p95    4.401/3.975    delta  +0.153 ms  control-no-change
  skill_request_overlay p50    4.410/4.598    p95    5.247/5.175    delta  +0.189 ms  control-no-change
  mcp_config_load   p50    0.227/0.235    p95    0.247/0.275    delta  +0.008 ms  control-no-change
  attach_history    p50   86.993/86.141   p95  102.284/93.271   delta  -0.852 ms  control-no-change
  resume_ready      p50   53.162/52.522   p95   57.335/56.077   delta  -0.640 ms  control-no-change
  provider_dispatch p50    0.984/0.957    p95    1.253/1.039    delta  -0.027 ms  control-no-change
  session_save      p50   11.939/11.888   p95   13.545/13.369   delta  -0.052 ms  control-no-change
  total store_read_bytes p50 2841249  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23665, 23669, 24375, 24389, 25091, 25095, 25809, 25813, 26519, 26531]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23666, 23674, 24376, 24384, 25090, 25102, 25804, 25808, 26526, 26534]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23665..26531 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23666..26534 (10 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.025/0.024    p95    0.041/0.050    delta  -0.001 ms  control-no-change
  skill_request_overlay p50    0.210/0.202    p95    0.251/0.238    delta  -0.007 ms  control-no-change
  mcp_config_load   p50    0.236/0.245    p95    0.251/0.318    delta  +0.009 ms  control-no-change
  attach_history    p50  936.566/927.053  p95 1004.908/995.131  delta  -9.514 ms  control-no-change
  resume_ready      p50  601.145/617.405  p95  828.440/638.096  delta +16.260 ms  control-no-change
  provider_dispatch p50    9.388/9.537    p95   11.298/10.070   delta  +0.150 ms  control-no-change
  session_save      p50   12.794/13.215   p95  112.505/31.308   delta  +0.422 ms  control-no-change
  total store_read_bytes p50 28640276  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23673, 23677, 24387, 25101, 25815, 25823, 26529]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23665, 23669, 24375, 24387, 25075, 25107, 25801, 25805, 26527, 26531]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23673..26529 (7 distinct values)
  SPREAD   b session_save store_read_bytes 23665..26531 (10 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    3.408/3.805    p95    4.146/4.015    delta  +0.397 ms  control-drift
  skill_request_overlay p50    4.554/4.888    p95    5.525/5.269    delta  +0.335 ms  control-no-change
  mcp_config_load   p50    0.248/0.247    p95    0.294/0.260    delta  -0.001 ms  control-no-change
  attach_history    p50  924.909/936.334  p95  968.093/1022.728 delta +11.425 ms  control-no-change
  resume_ready      p50  580.287/597.998  p95  605.926/665.264  delta +17.711 ms  control-no-change
  provider_dispatch p50    9.055/9.173    p95   10.863/10.875   delta  +0.117 ms  control-no-change
  session_save      p50   13.410/14.293   p95   17.412/17.043   delta  +0.883 ms  control-no-change
  total store_read_bytes p50 28640278  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23669, 23681, 24387, 24391, 25101, 25105, 25811, 25815, 26529]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23677, 23685, 24383, 24391, 25105, 25109, 25811, 25823, 26529, 26537]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23669..26529 (9 distinct values)
  SPREAD   b session_save store_read_bytes 23677..26537 (10 distinct values)
report   bench-results/loading-warm.json
mode warm, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### Tool latency

| Tool case | median | p95 | min | max | samples |
|---|---|---|---|---|---|
| read (tiny file) | 0.3 ms | 0.6 ms | 0.2 ms | 0.6 ms | 5 |
| read (1000 lines) | 0.6 ms | 0.8 ms | 0.6 ms | 0.8 ms | 5 |
| read (10 lines @ offset) | 0.6 ms | 0.7 ms | 0.5 ms | 0.7 ms | 5 |
| read (repo Cargo.toml) | 0.3 ms | 0.5 ms | 0.3 ms | 0.5 ms | 5 |
| write (small) | 2.5 ms | 3.0 ms | 1.8 ms | 3.0 ms | 5 |
| write (10KB) | 2.4 ms | 3.0 ms | 2.3 ms | 3.0 ms | 5 |
| edit (simple replace) | 2.6 ms | 2.8 ms | 2.0 ms | 2.8 ms | 5 |
| ls (scripts dir) | 0.7 ms | 0.8 ms | 0.7 ms | 0.8 ms | 5 |
| agentgrep (grep in tree) | 4.8 ms | 6.3 ms | 3.7 ms | 6.3 ms | 5 |
| agentgrep (find **/*.rs) | 1.6 ms | 2.1 ms | 1.5 ms | 2.1 ms | 5 |
| bash (true) | 7.4 ms | 10.0 ms | 5.4 ms | 10.0 ms | 5 |
| bash (echo hello) | 5.1 ms | 8.3 ms | 4.7 ms | 8.3 ms | 5 |
| bash (ls fixture tree) | 9.6 ms | 15.1 ms | 7.9 ms | 15.1 ms | 5 |
| invalid tool (dispatch only) | 0.1 ms | 0.2 ms | 0.1 ms | 0.2 ms | 5 |

#### Reload storm

1 resume(s) per level; each level resumes sessions no other level has opened, from a freshly started daemon.

| Concurrent resumes | history median | p95 | max | storm wall | failures |
|---|---|---|---|---|---|
| 1 | 108.2 ms | 108.2 ms | 108.2 ms | 108.4 ms | 0 |
| 10 | 332.8 ms | 334.6 ms | 334.6 ms | 335.6 ms | 0 |
| 40 | 1.23 s | 1.23 s | 1.23 s | 1.23 s | 0 |

#### Compile loop

| Checkpoint | Touched | median | min | max | samples |
|---|---|---|---|---|---|
| warm_noop | — | 993.1 ms | 993.1 ms | 993.1 ms | 1 |
| leaf_touch | src/main.rs | 532.4 ms | 532.4 ms | 532.4 ms | 1 |
| core_touch | crates/app-core/src/lib.rs | 55.24 s | 55.24 s | 55.24 s | 1 |
| build_meta_touch | crates/build-meta/src/lib.rs | 55.79 s | 55.79 s | 55.79 s | 1 |

### Windows

#### Coding agent comparison (headless, mock model)

Host: windows/AMD64, 4 CPUs, Python 3.12.10. Baseline: **horon**.

Skipped: syscalls (strace not available)

| Tool | Version | `--version` (median) | Install size |
|---|---|---|---|
| horon | horon-agent v0.1.0-dev (5b83dc3) | 10.7 ms | 92.8 MB |
| jcode | jcode v0.99.1 (7206efd0c) | 12.6 ms | 128.4 MB |
| Claude Code | 2.1.317 (Claude Code) | 33.8 ms | 645.2 MB |
| Codex CLI | codex-cli 0.166.0 | 87.8 ms | 479.4 MB |
| OpenCode | 1.19.5 | 469.0 ms | 605.3 MB |
| pi | 1.5.0 | 240.1 ms | 108.9 MB |
| oh-my-pi (omp) | omp/18.18.0 | 584.1 ms | 291.2 MB |
| oh-my-openagent (omo) | omo 5.1.64 (engine: senpi 2026.11.5-1) | 173.2 ms | 514.8 MB |

**One-shot headless prompt** (includes startup, one `ls` tool call, final answer)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Tool round-trip | Note |
|---|---|---|---|---|---|---|---|
| horon | 349.7 ms | 309.1 ms–587.5 ms | baseline | 32.8 MB | baseline | yes | — |
| jcode | 120.8 ms | 117.8 ms–202.8 ms | 2.89× less | 31.8 MB | 1.03× less | yes | — |
| Claude Code | 1.09 s | 1.07 s–1.70 s | 3.13× more | 273.8 MB | 8.34× more | yes | — |
| Codex CLI | 3.50 s | 1.38 s–4.24 s | 10.00× more | 3.2 GB | 100.00× more | yes | — |
| OpenCode | 4.88 s | 4.61 s–6.79 s | 13.96× more | 828.9 MB | 25.25× more | yes | — |
| pi | 452.0 ms | 417.4 ms–496.6 ms | 1.29× more | 125.4 MB | 3.82× more | yes | — |
| oh-my-pi (omp) | 3.21 s | 2.69 s–3.65 s | 9.18× more | 399.2 MB | 12.16× more | yes | — |
| oh-my-openagent (omo) | 25.26 s | 17.12 s–27.30 s | 72.23× more | 544.0 MB | 16.57× more | yes | — |

**Skill invocation** (process start to final answer, including skill discovery and one load of the fixture skill)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | Note |
|---|---|---|---|---|---|---|---|---|
| horon | 207.5 ms | 193.3 ms–214.8 ms | baseline | 25.6 MB | baseline | skill-uri | yes | — |
| jcode | 182.4 ms | 179.1 ms–303.8 ms | 1.14× less | 23.8 MB | 1.08× less | skill-tool | yes | — |
| Claude Code | 1.36 s | 826.4 ms–2.05 s | 6.56× more | 237.0 MB | 9.26× more | skill-tool | yes | — |
| Codex CLI | 2.26 s | 2.01 s–2.40 s | 10.89× more | 265.0 MB | 10.36× more | file-read | yes | — |
| OpenCode | 6.18 s | 6.04 s–6.84 s | 29.79× more | 3.2 GB | 128.61× more | skill-tool | yes | — |
| pi | 591.9 ms | 553.8 ms–620.9 ms | 2.85× more | 97.8 MB | 3.82× more | file-read | yes | — |
| oh-my-pi (omp) | 3.40 s | 3.38 s–4.19 s | 16.38× more | 415.1 MB | 16.23× more | file-read | yes | — |
| oh-my-openagent (omo) | 28.88 s | 20.38 s–30.31 s | 139.21× more | 532.9 MB | 20.83× more | file-read | yes | — |

**MCP tool call** (process start to final answer, including starting the fixture stdio MCP server and one `bench_echo` call)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | MCP ready | Note |
|---|---|---|---|---|---|---|---|---|---|
| horon | 241.7 ms | 226.3 ms–258.9 ms | baseline | 37.2 MB | baseline | mcp-call | yes | 133.8 ms | — |
| jcode | 313.9 ms | 283.4 ms–322.3 ms | 1.30× more | 36.1 MB | 1.03× less | mcp-action | yes | 215.6 ms | — |
| Claude Code | 1.13 s | 1.03 s–1.14 s | 4.68× more | 273.2 MB | 7.35× more | mcp-tool | yes | 926.7 ms | — |
| Codex CLI | 850.5 ms | 769.4 ms–974.8 ms | 3.52× more | 177.7 MB | 4.78× more | mcp-tool | yes | 594.7 ms | — |
| OpenCode | 6.22 s | 5.95 s–6.23 s | 25.73× more | 3.2 GB | 89.12× more | mcp-tool | yes | 4.89 s | — |
| pi | 622.1 ms | 616.1 ms–677.5 ms | 2.57× more | 118.1 MB | 3.18× more | mcp-tool | yes | 504.7 ms | — |
| oh-my-pi (omp) | 3.88 s | 3.63 s–4.53 s | 16.06× more | 422.0 MB | 11.35× more | mcp-route | yes | 2.95 s | — |
| oh-my-openagent (omo) | 27.39 s | 26.89 s–30.35 s | 113.29× more | 558.4 MB | 15.02× more | mcp-tool | yes | 16.95 s | — |

**Session memory** (idle after 3 turn(s) per session; sizes are process-tree sums, RSS; PSS also recorded on Linux). "Each additional" is the least-squares slope across the measured session counts; 0 B means growth stayed below RSS sampling noise. Threads and descriptors come from the daemon's own `server:memory` after the turns.

| Tool | First session | Each additional | vs horon (additional) | Threads/session | FDs/session | First (post-turn) | Each additional (post-turn) |
|---|---|---|---|---|---|---|---|
| horon | 22.6 MB | 123.9 KB | baseline | — | — | 29.6 MB | 495.0 KB |
| jcode | 22.0 MB | 49.6 KB | 2.50× less | — | — | 26.9 MB | 301.3 KB |
| Claude Code | 154.3 MB | 175.0 MB | 1,447× more | — | — | 188.9 MB | 189.7 MB |
| Codex CLI | 127.8 MB | 98.0 MB | 810.09× more | — | — | 151.0 MB | 124.9 MB |
| OpenCode | 234.9 MB | 1.8 MB | 14.73× more | — | — | 433.7 MB | 18.2 MB |
| pi | 90.6 MB | 492.4 MB | 4,071× more | — | — | 100.8 MB | 512.5 MB |
| oh-my-pi (omp) | 321.7 MB | 556.3 MB | 4,599× more | — | — | 345.9 MB | 991.2 MB |
| oh-my-openagent (omo) | 144.9 MB | 138.2 MB | 1,142× more | — | — | 411.0 MB | 420.1 MB |

Threads/descriptors unavailable: horon (server:memory did not report thread_count, fd_count), jcode (server:memory did not report thread_count, fd_count)

**Post-turn tree RSS per session count**

| Tool | 1 session(s) | 5 session(s) | 10 session(s) |
|---|---|---|---|
| horon | 29.6 MB | 32.2 MB | 34.0 MB |
| jcode | 26.9 MB | 28.3 MB | 29.6 MB |
| Claude Code | 188.9 MB | 942.4 MB | 1.9 GB |
| Codex CLI | 151.0 MB | 648.6 MB | 1.2 GB |
| OpenCode | 433.7 MB | 460.2 MB | 594.0 MB |
| pi | 100.8 MB | 504.8 MB | 4.5 GB |
| oh-my-pi (omp) | 345.9 MB | 1.7 GB | 8.9 GB |
| oh-my-openagent (omo) | 411.0 MB | 2.0 GB | 3.7 GB |

**Errors recorded during measurement**

- oh-my-openagent (omo) @ 10 sessions: turn 1 session 0: timeout
- oh-my-openagent (omo) @ 10 sessions: turn 1 session 1: timeout
- oh-my-openagent (omo) @ 10 sessions: turn 1 session 2: timeout
- oh-my-openagent (omo) @ 10 sessions: turn 1 session 3: timeout
- oh-my-openagent (omo) @ 10 sessions: turn 1 session 4: timeout
- oh-my-openagent (omo) @ 10 sessions: turn 1 session 5: timeout
- oh-my-openagent (omo) @ 10 sessions: turn 1 session 6: timeout
- oh-my-openagent (omo) @ 10 sessions: turn 1 session 7: timeout
- oh-my-openagent (omo) @ 10 sessions: turn 1 session 8: timeout
- oh-my-openagent (omo) @ 10 sessions: turn 1 session 9: timeout

#### 기동

```text
Benchmarking: D:/a/agent/agent/bench-bin/horon-agent.exe
============================================================

--help (binary load):
  Min:    14.06 ms
  Max:    14.54 ms
  Mean:   14.29 ms
  Median: 14.29 ms
  Stdev:  0.15 ms

--version:
  Min:    13.72 ms
  Max:    15.37 ms
  Mean:   14.09 ms
  Median: 13.99 ms
  Stdev:  0.48 ms

Measuring isolated server startup (10 runs)...

Server ready (isolated socket connectable):
  Min:    22.28 ms
  Max:    27.41 ms
  Mean:   22.93 ms
  Median: 22.34 ms
  Stdev:  1.59 ms

Measuring harness bridge readiness (10 runs)...
  skipped: api-bridge is Unix-only, so Windows has no harness bridge to time

Bridge stage: api socket connectable: No successful runs

Bridge stage: hello_ok: No successful runs

Bridge stage: no-op reply: No successful runs

Bridge total (socket -> no-op): No successful runs

============================================================
Summary:
  Binary load median:      ~14.3 ms
  Server ready median:     ~22.3 ms
  Bridge total median:     n/a
```

#### 세션 로딩 (COLD)

```text
binary   D:\a\agent\agent\bench-bin\examples\loading_probe.exe (c76f097b42b9)
mode     cold
protocol COLD: every measured run gets its own freshly copied HOME; no derived cache from an earlier run exists, so each sample is a first load of the fixture.
fixtures C:\Users\RUNNER~1\AppData\Local\Temp\horon-loading-bench-9dchmzwu
note     --warmup 3 is ignored in cold mode: every sample gets a fresh HOME
fixture events=0 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    0.078/0.077    p95    0.089/0.093    delta  -0.000 ms  control-no-change
  skill_request_overlay p50    0.569/0.597    p95    0.637/0.664    delta  +0.029 ms  control-no-change
  mcp_config_load   p50    0.703/0.691    p95    0.789/0.773    delta  -0.012 ms  control-no-change
  attach_history    p50    0.001/0.001    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.001/0.000    p95    0.001/0.001    delta  -0.000 ms  control-no-change
  session_save      p50   48.611/48.512   p95   51.166/66.735   delta  -0.099 ms  control-no-change
  total store_read_bytes p50 22954  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 22930..22964 (7 distinct values)
  SPREAD   b session_save store_read_bytes 22914..22968 (9 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50   10.541/10.526   p95   10.982/11.129   delta  -0.016 ms  control-no-change
  skill_request_overlay p50   30.014/29.454   p95   32.049/34.662   delta  -0.560 ms  control-no-change
  mcp_config_load   p50    0.737/0.710    p95    0.785/0.770    delta  -0.027 ms  control-no-change
  attach_history    p50    0.001/0.001    p95    0.001/0.002    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  session_save      p50   65.542/68.123   p95  156.199/95.640   delta  +2.581 ms  control-no-change
  total store_read_bytes p50 22944  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 22922..22968 (8 distinct values)
  SPREAD   b session_save store_read_bytes 22926..22964 (6 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.079/0.074    p95    0.106/0.097    delta  -0.005 ms  control-no-change
  skill_request_overlay p50    0.576/0.569    p95    0.918/0.704    delta  -0.007 ms  control-no-change
  mcp_config_load   p50    0.701/0.693    p95    0.944/0.893    delta  -0.008 ms  control-no-change
  attach_history    p50  324.518/324.149  p95  394.079/503.131  delta  -0.369 ms  control-no-change
  resume_ready      p50  103.914/104.072  p95  117.814/117.620  delta  +0.158 ms  control-no-change
  provider_dispatch p50    1.221/1.206    p95    1.527/1.278    delta  -0.015 ms  control-no-change
  session_save      p50   44.049/42.606   p95  303.738/48.094   delta  -1.444 ms  control-no-change
  total store_read_bytes p50 5311208  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189147..4189153 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099099..1099105 (3 distinct values)
  SPREAD   a session_save store_read_bytes 22922..22971 (10 distinct values)
  SPREAD   b attach_history store_read_bytes 4189150..4189153 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099102..1099105 (2 distinct values)
  SPREAD   b session_save store_read_bytes 22920..22975 (10 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50   10.925/10.874   p95   11.595/11.473   delta  -0.052 ms  control-no-change
  skill_request_overlay p50   29.613/29.433   p95   31.995/37.172   delta  -0.180 ms  control-no-change
  mcp_config_load   p50    0.715/0.739    p95    0.767/0.838    delta  +0.024 ms  control-no-change
  attach_history    p50  309.108/315.074  p95  350.458/334.696  delta  +5.966 ms  control-no-change
  resume_ready      p50  100.355/99.582   p95  114.364/104.238  delta  -0.772 ms  control-no-change
  provider_dispatch p50    1.140/1.158    p95    1.234/1.233    delta  +0.017 ms  control-no-change
  session_save      p50   47.035/42.999   p95   72.969/74.672   delta  -4.036 ms  control-no-change
  total store_read_bytes p50 5311202  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189153..4189155 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099105..1099106 (2 distinct values)
  SPREAD   a session_save store_read_bytes 22910..22976 (8 distinct values)
  SPREAD   b attach_history store_read_bytes 4189153..4189155 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099105..1099106 (2 distinct values)
  SPREAD   b session_save store_read_bytes 22907..22972 (10 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.072/0.073    p95    0.092/0.096    delta  +0.001 ms  control-no-change
  skill_request_overlay p50    0.563/0.597    p95    0.599/0.656    delta  +0.034 ms  control-no-change
  mcp_config_load   p50    0.682/0.686    p95    0.724/0.734    delta  +0.004 ms  control-no-change
  attach_history    p50 3612.211/3607.919 p95 3652.574/3716.060 delta  -4.292 ms  control-no-change
  resume_ready      p50 1070.304/1075.943 p95 1079.849/1104.042 delta  +5.638 ms  control-no-change
  provider_dispatch p50   14.225/13.510   p95   14.768/14.818   delta  -0.715 ms  control-no-change
  session_save      p50   42.535/42.635   p95   45.397/53.851   delta  +0.100 ms  control-no-change
  total store_read_bytes p50 53417430  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 22935..22967 (8 distinct values)
  SPREAD   b attach_history store_read_bytes 42184249..42184253 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210212..11210216 (3 distinct values)
  SPREAD   b session_save store_read_bytes 22959..22975 (6 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50   10.848/10.922   p95   11.602/14.482   delta  +0.075 ms  control-no-change
  skill_request_overlay p50   29.426/30.088   p95   33.033/35.851   delta  +0.663 ms  control-no-change
  mcp_config_load   p50    0.718/0.746    p95    0.753/0.774    delta  +0.028 ms  control-no-change
  attach_history    p50 3700.463/3655.867 p95 3796.686/3849.272 delta -44.596 ms  control-no-change
  resume_ready      p50 1064.725/1086.031 p95 1088.508/1208.354 delta +21.305 ms  control-no-change
  provider_dispatch p50   13.835/14.012   p95   14.603/14.774   delta  +0.178 ms  control-no-change
  session_save      p50   40.765/41.806   p95  555.195/67.086   delta  +1.041 ms  control-no-change
  total store_read_bytes p50 53417434  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184247..42184253 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210210..11210216 (3 distinct values)
  SPREAD   a session_save store_read_bytes 22937..22979 (6 distinct values)
  SPREAD   b attach_history store_read_bytes 42184247..42184253 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210211..11210216 (3 distinct values)
  SPREAD   b session_save store_read_bytes 22941..22991 (7 distinct values)
report   bench-results\loading-cold.json
mode cold, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### 세션 로딩 (WARM)

```text
binary   D:\a\agent\agent\bench-bin\examples\loading_probe.exe (c76f097b42b9)
mode     warm
protocol WARM: one HOME per side, --warmup discarded runs per side, then measured runs alternate between the two sides on those same HOMEs.
fixtures C:\Users\RUNNER~1\AppData\Local\Temp\horon-loading-bench-6_l67sal
fixture events=0 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    0.066/0.065    p95    0.077/0.088    delta  -0.002 ms  control-no-change
  skill_request_overlay p50    0.528/0.518    p95    0.558/0.533    delta  -0.010 ms  control-no-change
  mcp_config_load   p50    0.619/0.638    p95    0.695/0.686    delta  +0.019 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.001/0.000    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  session_save      p50   30.041/28.386   p95   38.826/33.094   delta  -1.655 ms  control-no-change
  total store_read_bytes p50 23635  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [22937, 22941, 22953, 22957, 23626, 23644, 24319, 24331, 24347]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [22945, 22947, 22957, 23636, 23640, 23652, 24311, 24326, 24347, 24363]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 22937..24347 (9 distinct values)
  SPREAD   b session_save store_read_bytes 22945..24363 (10 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50   10.212/10.265   p95   10.660/10.833   delta  +0.053 ms  control-no-change
  skill_request_overlay p50   27.551/27.549   p95   28.077/33.541   delta  -0.003 ms  control-no-change
  mcp_config_load   p50    0.652/0.654    p95    0.762/0.678    delta  +0.002 ms  control-no-change
  attach_history    p50    0.001/0.001    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.001/0.001    delta  -0.000 ms  control-no-change
  session_save      p50   36.772/37.202   p95   45.398/51.080   delta  +0.430 ms  control-no-change
  total store_read_bytes p50 23626  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [22908, 22929, 22943, 22959, 23610, 23641, 23646, 23649, 24325, 24364]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [22895, 22941, 22949, 23632, 23652, 23660, 24327, 24335, 24367]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 22908..24364 (10 distinct values)
  SPREAD   b session_save store_read_bytes 22895..24367 (9 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.070/0.069    p95    0.091/0.080    delta  -0.001 ms  control-no-change
  skill_request_overlay p50    0.520/0.523    p95    0.561/0.563    delta  +0.003 ms  control-no-change
  mcp_config_load   p50    0.636/0.652    p95    0.691/0.697    delta  +0.016 ms  control-no-change
  attach_history    p50  135.834/136.793  p95  138.564/142.467  delta  +0.959 ms  control-no-change
  resume_ready      p50  100.546/100.113  p95  104.936/102.125  delta  -0.433 ms  control-no-change
  provider_dispatch p50    1.100/1.073    p95    1.196/1.172    delta  -0.027 ms  control-no-change
  session_save      p50   27.540/29.459   p95   29.292/33.954   delta  +1.919 ms  control-no-change
  total store_read_bytes p50 2841143  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23633, 23651, 24313, 24350, 25061, 25065, 25748, 25760, 26447, 26457]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23610, 23675, 24298, 24349, 25056, 25063, 25770, 25771, 26441, 26454]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23633..26457 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23610..26454 (10 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50   10.258/10.181   p95   10.809/13.673   delta  -0.077 ms  control-no-change
  skill_request_overlay p50   27.387/27.606   p95   28.016/38.490   delta  +0.220 ms  control-no-change
  mcp_config_load   p50    0.681/0.672    p95    0.721/1.226    delta  -0.009 ms  control-no-change
  attach_history    p50  134.286/137.671  p95  140.613/202.500  delta  +3.386 ms  control-no-change
  resume_ready      p50   99.409/101.451  p95  105.174/153.100  delta  +2.042 ms  control-no-change
  provider_dispatch p50    1.007/1.121    p95    1.152/1.244    delta  +0.113 ms  control-no-change
  session_save      p50   35.434/34.119   p95   56.844/41.158   delta  -1.316 ms  control-no-change
  total store_read_bytes p50 2841134  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23602, 23671, 24317, 24359, 25049, 25058, 25733, 25766, 26422, 26429]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23653, 23656, 24348, 24359, 25019, 25057, 25760, 25764, 26410, 26447]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23602..26429 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23653..26447 (10 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.070/0.071    p95    0.125/0.100    delta  +0.001 ms  control-no-change
  skill_request_overlay p50    0.534/0.565    p95    0.613/0.645    delta  +0.031 ms  control-no-change
  mcp_config_load   p50    0.705/0.699    p95    0.750/0.781    delta  -0.005 ms  control-no-change
  attach_history    p50 1510.527/1491.208 p95 1645.138/1599.523 delta -19.319 ms  control-no-change
  resume_ready      p50 1084.137/1067.250 p95 1098.133/1128.318 delta -16.887 ms  control-no-change
  provider_dispatch p50   13.625/13.071   p95   14.729/14.054   delta  -0.554 ms  control-no-change
  session_save      p50   30.468/31.221   p95   79.601/33.708   delta  +0.753 ms  control-no-change
  total store_read_bytes p50 28640142  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23662, 23666, 24353, 24361, 25026, 25064, 25723, 25779, 26448, 26474]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23653, 23660, 24326, 24330, 25046, 25051, 25749, 25750, 26438, 26465]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23662..26474 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23653..26465 (10 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50   10.445/10.193   p95   10.749/10.866   delta  -0.252 ms  control-no-change
  skill_request_overlay p50   29.084/28.141   p95   31.219/30.086   delta  -0.943 ms  control-no-change
  mcp_config_load   p50    0.674/0.675    p95    0.750/0.700    delta  +0.001 ms  control-no-change
  attach_history    p50 1487.756/1489.256 p95 1544.717/1537.499 delta  +1.500 ms  control-no-change
  resume_ready      p50 1058.177/1073.504 p95 1075.562/1114.027 delta +15.327 ms  control-no-change
  provider_dispatch p50   13.755/14.243   p95   14.788/15.091   delta  +0.488 ms  control-no-change
  session_save      p50   28.931/28.367   p95   30.968/33.802   delta  -0.564 ms  control-no-change
  total store_read_bytes p50 28640134  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23658, 23662, 24328, 24337, 25033, 25040, 25738, 25749, 26444, 26449]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23678, 23679, 24330, 24344, 25047, 25061, 25748, 25756, 26438, 26443]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23658..26449 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23678..26443 (10 distinct values)
report   bench-results\loading-warm.json
mode warm, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### Tool latency

| Tool case | median | p95 | min | max | samples |
|---|---|---|---|---|---|
| read (tiny file) | 2.7 ms | 2.7 ms | 2.2 ms | 2.7 ms | 5 |
| read (1000 lines) | 2.6 ms | 2.7 ms | 2.1 ms | 2.7 ms | 5 |
| read (10 lines @ offset) | 2.6 ms | 2.7 ms | 2.1 ms | 2.7 ms | 5 |
| read (repo Cargo.toml) | 2.7 ms | 2.7 ms | 2.2 ms | 2.7 ms | 5 |
| write (small) | 15.0 ms | 58.5 ms | 10.4 ms | 58.5 ms | 5 |
| write (10KB) | 12.4 ms | 18.0 ms | 9.9 ms | 18.0 ms | 5 |
| edit (simple replace) | 9.9 ms | 73.0 ms | 9.9 ms | 73.0 ms | 5 |
| ls (scripts dir) | 2.7 ms | 2.7 ms | 2.2 ms | 2.7 ms | 5 |
| agentgrep (grep in tree) | 5.2 ms | 5.3 ms | 4.8 ms | 5.3 ms | 5 |
| agentgrep (find **/*.rs) | 5.2 ms | 5.2 ms | 4.2 ms | 5.2 ms | 5 |
| bash (true) | 7.8 ms | 7.8 ms | 7.3 ms | 7.8 ms | 5 |
| bash (echo hello) | 7.8 ms | 10.0 ms | 7.3 ms | 10.0 ms | 5 |
| bash (ls fixture tree) | 10.4 ms | 10.5 ms | 9.9 ms | 10.5 ms | 5 |
| invalid tool (dispatch only) | 2.6 ms | 2.7 ms | 2.6 ms | 2.7 ms | 5 |

#### Reload storm

1 resume(s) per level; each level resumes sessions no other level has opened, from a freshly started daemon.

| Concurrent resumes | history median | p95 | max | storm wall | failures |
|---|---|---|---|---|---|
| 1 | 226.8 ms | 226.8 ms | 226.8 ms | 227.0 ms | 0 |
| 10 | 920.1 ms | 921.4 ms | 921.4 ms | 922.9 ms | 0 |
| 40 | 2.42 s | 3.33 s | 3.33 s | 3.34 s | 0 |

#### Compile loop

| Checkpoint | Touched | median | min | max | samples |
|---|---|---|---|---|---|
| warm_noop | — | 545.1 ms | 545.1 ms | 545.1 ms | 1 |
| leaf_touch | src\main.rs | 750.1 ms | 750.1 ms | 750.1 ms | 1 |
| core_touch | crates\app-core\src\lib.rs | 23.92 s | 23.92 s | 23.92 s | 1 |
| build_meta_touch | crates\build-meta\src\lib.rs | 31.64 s | 31.64 s | 31.64 s | 1 |

<!-- bench-auto:end -->
