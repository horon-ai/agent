# 성능: 측정 결과와 그 한계

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

구조 게이트와 A/B 실행기의 사용법, 리포트 템플릿의 필수 키, 경계를 올리는 절차는 [`docs/performance/BENCHMARK.md`](BENCHMARK.md) 에 있다. 게이트 명령은 `python3 scripts/perf/check_structural.py` 이고, A/B 는 `scripts/perf/ab_perf.py` 다.

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

## 자동 측정: `main` 최신 측정 커밋

`main` 에 올라간 커밋이 CI 와 구조 성능 게이트를 모두 통과하고 직전 측정 이후 에이전트 코드(`src`, `crates`, `Cargo.toml`, `Cargo.lock`, `.cargo`, `rust-toolchain.toml`, `scripts/perf`)가 바뀌었으면, `.github/workflows/benchmark-docs.yml` 이 Linux·macOS·Windows에서 전체 벤치마크 묶음을 OS마다 따로 돌리고 아래 구간을 `main` 과 `dev` 에서 함께 다시 씁니다. OS마다 절이 하나씩 있습니다. 측정 방법과 묶음별 뜻은 [`BENCHMARK.md`](BENCHMARK.md) 의 "자동 측정"에 있습니다.

<!-- bench-auto:start commit=4a636d3f84aaf0d461b0730b85a06e27352b9b84 -->

이 구간은 `.github/workflows/benchmark-docs.yml` 이 자동으로 다시 씁니다. 손으로 고치지 않습니다.

- 커밋: `4a636d3f84aaf0d461b0730b85a06e27352b9b84`
- 측정 시각: 2026-11-08 08:14 UTC
- 실행: <https://github.com/horon-ai/agent/actions/runs/42459731881> (원본 JSON 은 OS별 `benchmark-<os>-<run_id>` 아티팩트)

OS마다 다른 GitHub 호스티드 러너에서 따로 잽니다. 기계가 다르므로 OS끼리 절대값을 비교하지 않고, 같은 OS 안의 상대 비교(horon 대비 배수)와 커밋 사이의 추세만 읽습니다.

| OS | 러너 | 바이너리 | 상태 |
| --- | --- | --- | --- |
| Linux | `ubuntu-24.04` | `horon-agent v0.98.1-dev (4a636d3)` | 성공 |
| macOS | `macos-latest` | `horon-agent v0.98.1-dev (4a636d3)` | 성공 |
| Windows | `windows-latest` | `horon-agent v0.98.1-dev (4a636d3)` | 성공 |

### Linux

#### Coding agent comparison (headless, mock model)

Host: linux/x86_64, 4 CPUs, Python 3.12.16. Baseline: **horon**.

| Tool | Version | `--version` (median) | Install size |
|---|---|---|---|
| horon | horon-agent v0.98.1-dev (4a636d3) | 3.4 ms | 106.7 MB |
| jcode | jcode v0.99.1 (7206efd0c) | 8.9 ms | 163.4 MB |
| Claude Code | 2.1.317 (Claude Code) | 9.0 ms | 632.2 MB |
| Codex CLI | codex-cli 0.166.0 | 36.3 ms | 435.0 MB |
| OpenCode | 1.19.5 | 357.8 ms | 613.4 MB |
| pi | 0.73.1 | 574.2 ms | 142.7 MB |

**One-shot headless prompt** (includes startup, one `ls` tool call, final answer)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Tool round-trip | fsync calls | Note |
|---|---|---|---|---|---|---|---|---|
| horon | 349.1 ms | 347.2 ms–481.1 ms | baseline | 49.1 MB | baseline | yes | 22 | — |
| jcode | 289.1 ms | 272.4 ms–308.5 ms | 1.21× less | 46.1 MB | 1.07× less | yes | 10 | — |
| Claude Code | 578.8 ms | 576.9 ms–615.3 ms | 1.66× more | 207.3 MB | 4.22× more | yes | 9 | — |
| Codex CLI | 911.3 ms | 911.2 ms–1.12 s | 2.61× more | 208.7 MB | 4.25× more | yes | 3 | — |
| OpenCode | 4.03 s | 3.93 s–4.25 s | 11.54× more | 956.8 MB | 19.48× more | yes | 4 | — |
| pi | 1.41 s | 1.41 s–1.43 s | 4.05× more | 164.4 MB | 3.35× more | yes | — | — |

**Session memory** (idle after 3 turn(s) per session; sizes are process-tree sums, RSS; PSS also recorded on Linux). "Each additional" is the least-squares slope across the measured session counts; 0 B means growth stayed below RSS sampling noise. Threads and descriptors come from the daemon's own `server:memory` after the turns.

| Tool | First session | Each additional | vs horon (additional) | Threads/session | FDs/session | First (post-turn) | Each additional (post-turn) |
|---|---|---|---|---|---|---|---|
| horon | 40.3 MB | 179.5 KB | baseline | 0.23 | 1.00 | 45.8 MB | 558.6 KB |
| jcode | 42.9 MB | 116.7 KB | 1.54× less | 0.23 | — | 46.6 MB | 246.3 KB |
| Claude Code | 151.5 MB | 149.7 MB | 853.80× more | — | — | 188.0 MB | 180.9 MB |
| Codex CLI | 164.2 MB | 131.0 MB | 747.42× more | — | — | 160.4 MB | 162.9 MB |
| OpenCode | 247.3 MB | 13.7 MB | 78.17× more | — | — | 452.9 MB | 58.6 MB |
| pi | 146.5 MB | 125.8 MB | 717.88× more | — | — | 151.4 MB | 165.3 MB |

Threads/descriptors unavailable: jcode (server:memory did not report fd_count)

**Post-turn tree RSS per session count**

| Tool | 1 session(s) | 5 session(s) | 10 session(s) |
|---|---|---|---|
| horon | 45.8 MB | 48.2 MB | 50.8 MB |
| jcode | 46.6 MB | 47.7 MB | 48.8 MB |
| Claude Code | 188.0 MB | 917.5 MB | 1.8 GB |
| Codex CLI | 160.4 MB | 806.3 MB | 1.6 GB |
| OpenCode | 452.9 MB | 521.0 MB | 968.4 MB |
| pi | 151.4 MB | 806.0 MB | 1.6 GB |

#### 기동

```text
Benchmarking: /home/runner/work/agent/agent/target/release/horon-agent
============================================================

--help (binary load):
  Min:    3.22 ms
  Max:    3.60 ms
  Mean:   3.34 ms
  Median: 3.29 ms
  Stdev:  0.12 ms

--version:
  Min:    3.07 ms
  Max:    3.29 ms
  Mean:   3.14 ms
  Median: 3.12 ms
  Stdev:  0.06 ms

Measuring isolated server startup (10 runs)...

Server ready (isolated socket connectable):
  Min:    20.47 ms
  Max:    20.59 ms
  Mean:   20.51 ms
  Median: 20.50 ms
  Stdev:  0.04 ms

Measuring harness bridge readiness (10 runs)...

Bridge stage: api socket connectable:
  Min:    20.51 ms
  Max:    20.85 ms
  Mean:   20.65 ms
  Median: 20.64 ms
  Stdev:  0.09 ms

Bridge stage: hello_ok:
  Min:    21.10 ms
  Max:    21.39 ms
  Mean:   21.25 ms
  Median: 21.26 ms
  Stdev:  0.09 ms

Bridge stage: no-op reply:
  Min:    21.51 ms
  Max:    21.79 ms
  Mean:   21.66 ms
  Median: 21.67 ms
  Stdev:  0.08 ms

Bridge total (socket -> no-op):
  Min:    21.51 ms
  Max:    21.79 ms
  Mean:   21.66 ms
  Median: 21.67 ms
  Stdev:  0.08 ms

============================================================
Summary:
  Binary load median:      ~3.3 ms
  Server ready median:     ~20.5 ms
  Bridge total median:     ~21.7 ms
```

#### 세션 로딩 (COLD)

```text
binary   /home/runner/work/agent/agent/target/release/examples/loading_probe (b66b1b992a91)
mode     cold
protocol COLD: every measured run gets its own freshly copied HOME; no derived cache from an earlier run exists, so each sample is a first load of the fixture.
fixtures /tmp/horon-loading-bench-9r8bhm_q
note     --warmup 3 is ignored in cold mode: every sample gets a fresh HOME
fixture events=0 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    0.018/0.016    p95    0.020/0.021    delta  -0.002 ms  control-no-change
  skill_request_overlay p50    0.109/0.111    p95    0.113/0.136    delta  +0.002 ms  control-no-change
  mcp_config_load   p50    0.151/0.151    p95    0.170/0.190    delta  +0.000 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  session_save      p50    7.342/7.237    p95    7.516/7.618    delta  -0.105 ms  control-no-change
  total store_read_bytes p50 23391  full_source_hashes p50 1  skill_dir_reads p50 10
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 23379..23399 (6 distinct values)
  SPREAD   b session_save store_read_bytes 23387..23407 (4 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    2.563/2.573    p95    2.600/2.625    delta  +0.010 ms  control-no-change
  skill_request_overlay p50   24.395/24.261   p95   24.594/24.619   delta  -0.134 ms  control-no-change
  mcp_config_load   p50    0.165/0.164    p95    0.167/0.171    delta  -0.001 ms  control-no-change
  attach_history    p50    0.001/0.001    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.001    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  session_save      p50    8.864/8.777    p95   19.105/9.037    delta  -0.088 ms  control-no-change
  total store_read_bytes p50 23387  full_source_hashes p50 1  skill_dir_reads p50 10
  total skill_file_reads p50 2000  skill_bytes_read p50 243900  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 23375..23407 (8 distinct values)
  SPREAD   b session_save store_read_bytes 23367..23399 (5 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.016/0.016    p95    0.019/0.019    delta  +0.000 ms  control-no-change
  skill_request_overlay p50    0.113/0.112    p95    0.143/0.123    delta  -0.001 ms  control-no-change
  mcp_config_load   p50    0.156/0.155    p95    0.174/0.170    delta  -0.001 ms  control-no-change
  attach_history    p50  319.153/318.952  p95  343.172/320.673  delta  -0.201 ms  control-no-change
  resume_ready      p50   50.887/50.850   p95   51.610/51.198   delta  -0.037 ms  control-no-change
  provider_dispatch p50    1.328/1.329    p95    1.384/1.363    delta  +0.001 ms  control-no-change
  session_save      p50    7.423/7.514    p95   77.896/24.468   delta  +0.091 ms  control-no-change
  total store_read_bytes p50 5311718  full_source_hashes p50 2  skill_dir_reads p50 10
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189191..4189195 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099132..1099136 (3 distinct values)
  SPREAD   a session_save store_read_bytes 23379..23401 (8 distinct values)
  SPREAD   b attach_history store_read_bytes 4189193..4189195 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099135..1099136 (2 distinct values)
  SPREAD   b session_save store_read_bytes 23385..23401 (6 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    2.561/2.551    p95    2.569/2.586    delta  -0.010 ms  control-no-change
  skill_request_overlay p50   23.983/23.954   p95   24.153/24.112   delta  -0.029 ms  control-no-change
  mcp_config_load   p50    0.164/0.168    p95    0.166/0.175    delta  +0.004 ms  control-no-change
  attach_history    p50  321.342/320.357  p95  433.960/324.365  delta  -0.985 ms  control-no-change
  resume_ready      p50   51.035/51.012   p95   51.316/51.622   delta  -0.023 ms  control-no-change
  provider_dispatch p50    1.322/1.330    p95    1.353/1.344    delta  +0.008 ms  control-no-change
  session_save      p50    7.433/7.418    p95    7.626/13.380   delta  -0.014 ms  control-no-change
  total store_read_bytes p50 5311728  full_source_hashes p50 2  skill_dir_reads p50 10
  total skill_file_reads p50 2000  skill_bytes_read p50 243900  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 23385..23409 (6 distinct values)
  SPREAD   b attach_history store_read_bytes 4189189..4189195 (4 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099132..1099136 (4 distinct values)
  SPREAD   b session_save store_read_bytes 23386..23409 (6 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.018/0.017    p95    0.021/0.021    delta  -0.001 ms  control-no-change
  skill_request_overlay p50    0.114/0.113    p95    0.130/0.126    delta  -0.001 ms  control-no-change
  mcp_config_load   p50    0.156/0.156    p95    0.171/0.163    delta  +0.000 ms  control-no-change
  attach_history    p50 3935.511/3912.646 p95 3971.575/3997.828 delta -22.865 ms  control-no-change
  resume_ready      p50  563.027/563.293  p95  571.532/572.299  delta  +0.266 ms  control-no-change
  provider_dispatch p50   14.933/14.973   p95   18.103/15.597   delta  +0.040 ms  control-no-change
  session_save      p50    7.627/7.758    p95   20.397/7.870    delta  +0.131 ms  control-no-change
  total store_read_bytes p50 53417935  full_source_hashes p50 2  skill_dir_reads p50 10
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184289..42184293 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210244..11210246 (2 distinct values)
  SPREAD   a session_save store_read_bytes 23388..23412 (6 distinct values)
  SPREAD   b attach_history store_read_bytes 42184285..42184293 (4 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210238..11210246 (4 distinct values)
  SPREAD   b session_save store_read_bytes 23380..23404 (7 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    2.553/2.569    p95    2.579/2.592    delta  +0.016 ms  control-no-change
  skill_request_overlay p50   23.955/23.988   p95   24.136/24.185   delta  +0.033 ms  control-no-change
  mcp_config_load   p50    0.163/0.165    p95    0.170/0.174    delta  +0.002 ms  control-no-change
  attach_history    p50 3949.748/3930.464 p95 3974.875/3960.825 delta -19.284 ms  control-no-change
  resume_ready      p50  561.182/563.258  p95  566.053/567.642  delta  +2.077 ms  control-no-change
  provider_dispatch p50   14.586/14.942   p95   15.459/15.281   delta  +0.356 ms  control-no-change
  session_save      p50    7.677/7.751    p95    7.813/8.463    delta  +0.075 ms  control-no-change
  total store_read_bytes p50 53417935  full_source_hashes p50 2  skill_dir_reads p50 10
  total skill_file_reads p50 2000  skill_bytes_read p50 243900  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184289..42184293 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210242..11210246 (2 distinct values)
  SPREAD   a session_save store_read_bytes 23388..23408 (6 distinct values)
  SPREAD   b attach_history store_read_bytes 42184289..42184293 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210242..11210246 (3 distinct values)
  SPREAD   b session_save store_read_bytes 23388..23404 (5 distinct values)
report   bench-results/loading-cold.json
mode cold, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### 세션 로딩 (WARM)

```text
binary   /home/runner/work/agent/agent/target/release/examples/loading_probe (b66b1b992a91)
mode     warm
protocol WARM: one HOME per side, --warmup discarded runs per side, then measured runs alternate between the two sides on those same HOMEs.
fixtures /tmp/horon-loading-bench-yj7gz4gs
fixture events=0 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    0.016/0.017    p95    0.020/0.029    delta  +0.001 ms  control-no-change
  skill_request_overlay p50    0.113/0.112    p95    0.133/0.113    delta  -0.001 ms  control-no-change
  mcp_config_load   p50    0.150/0.150    p95    0.153/0.169    delta  -0.000 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.001/0.000    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  session_save      p50    7.380/7.330    p95    7.955/13.599   delta  -0.050 ms  control-no-change
  total store_read_bytes p50 24113  full_source_hashes p50 1  skill_dir_reads p50 10
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23384, 23388, 24111, 24115, 24814, 24826, 24830, 24842]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23384, 23388, 23392, 24103, 24115, 24822, 24826, 24830, 24834]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23384..24842 (8 distinct values)
  SPREAD   b session_save store_read_bytes 23384..24834 (9 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    2.562/2.576    p95    2.581/2.621    delta  +0.013 ms  control-no-change
  skill_request_overlay p50   24.253/24.234   p95   24.397/24.349   delta  -0.019 ms  control-no-change
  mcp_config_load   p50    0.164/0.167    p95    0.183/0.182    delta  +0.003 ms  control-no-change
  attach_history    p50    0.001/0.001    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  session_save      p50    8.277/8.341    p95   25.005/9.320    delta  +0.064 ms  control-no-change
  total store_read_bytes p50 24104  full_source_hashes p50 1  skill_dir_reads p50 10
  total skill_file_reads p50 2000  skill_bytes_read p50 243900  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23388, 23390, 23396, 23398, 24099, 24109, 24111, 24824, 24834, 24838]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23384, 23388, 23396, 24103, 24119, 24127, 24822, 24826, 24830, 24834]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23388..24838 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23384..24834 (10 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.016/0.016    p95    0.018/0.029    delta  +0.000 ms  control-no-change
  skill_request_overlay p50    0.112/0.113    p95    0.123/0.137    delta  +0.001 ms  control-no-change
  mcp_config_load   p50    0.153/0.156    p95    0.175/0.166    delta  +0.003 ms  control-no-change
  attach_history    p50   94.073/94.054   p95   94.582/94.643   delta  -0.019 ms  control-no-change
  resume_ready      p50   50.725/50.719   p95   51.244/51.102   delta  -0.006 ms  control-no-change
  provider_dispatch p50    1.329/1.325    p95    1.383/1.464    delta  -0.003 ms  control-no-change
  session_save      p50    7.692/7.775    p95   16.394/13.036   delta  +0.083 ms  control-no-change
  total store_read_bytes p50 2841678  full_source_hashes p50 1  skill_dir_reads p50 10
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [24112, 24126, 24833, 24839, 25542, 25550, 26257, 26269, 26972, 26990]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [24120, 24827, 24843, 25542, 25558, 26269, 26277, 26992, 26996]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 24112..26990 (10 distinct values)
  SPREAD   b session_save store_read_bytes 24120..26996 (9 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    2.530/2.534    p95    2.644/2.676    delta  +0.004 ms  control-no-change
  skill_request_overlay p50   23.915/23.753   p95   24.024/24.345   delta  -0.162 ms  control-no-change
  mcp_config_load   p50    0.162/0.163    p95    0.171/0.171    delta  +0.000 ms  control-no-change
  attach_history    p50   94.077/94.227   p95   94.785/94.689   delta  +0.150 ms  control-no-change
  resume_ready      p50   50.654/50.835   p95   51.058/50.935   delta  +0.181 ms  control-no-change
  provider_dispatch p50    1.330/1.339    p95    1.369/1.366    delta  +0.009 ms  control-no-change
  session_save      p50    8.500/8.586    p95   48.338/25.262   delta  +0.085 ms  control-no-change
  total store_read_bytes p50 2841687  full_source_hashes p50 1  skill_dir_reads p50 10
  total skill_file_reads p50 2000  skill_bytes_read p50 243900  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [24112, 24124, 24827, 24831, 25546, 25548, 26249, 26261, 26980, 26988]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [24106, 24114, 24813, 24833, 25534, 25542, 26257, 26269, 26976, 26988]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 24112..26988 (10 distinct values)
  SPREAD   b session_save store_read_bytes 24106..26988 (10 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.016/0.016    p95    0.018/0.018    delta  +0.000 ms  control-no-change
  skill_request_overlay p50    0.112/0.112    p95    0.132/0.124    delta  -0.000 ms  control-no-change
  mcp_config_load   p50    0.151/0.151    p95    0.162/0.155    delta  -0.000 ms  control-no-change
  attach_history    p50 1052.496/1051.516 p95 1063.349/1077.648 delta  -0.980 ms  control-no-change
  resume_ready      p50  560.086/565.409  p95  564.391/570.985  delta  +5.323 ms  control-no-change
  provider_dispatch p50   14.559/14.561   p95   15.451/15.109   delta  +0.002 ms  control-no-change
  session_save      p50    7.870/8.003    p95   76.232/8.798    delta  +0.132 ms  control-no-change
  total store_read_bytes p50 28640702  full_source_hashes p50 1  skill_dir_reads p50 10
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [24107, 24115, 24826, 24834, 25549, 25557, 26268, 26280, 26987, 26995]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [24115, 24832, 24834, 25531, 25561, 26264, 26276, 26999, 27003]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 24107..26995 (10 distinct values)
  SPREAD   b session_save store_read_bytes 24115..27003 (9 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    2.545/2.566    p95    2.574/2.653    delta  +0.022 ms  control-no-change
  skill_request_overlay p50   23.943/23.996   p95   24.066/24.138   delta  +0.053 ms  control-no-change
  mcp_config_load   p50    0.162/0.163    p95    0.169/0.175    delta  +0.001 ms  control-no-change
  attach_history    p50 1055.501/1052.034 p95 1058.503/1067.218 delta  -3.467 ms  control-no-change
  resume_ready      p50  566.010/566.054  p95  569.665/569.576  delta  +0.044 ms  control-no-change
  provider_dispatch p50   14.568/14.484   p95   14.837/14.828   delta  -0.084 ms  control-no-change
  session_save      p50    8.881/8.739    p95   54.268/10.124   delta  -0.142 ms  control-no-change
  total store_read_bytes p50 28640709  full_source_hashes p50 1  skill_dir_reads p50 10
  total skill_file_reads p50 2000  skill_bytes_read p50 243900  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [24123, 24127, 24834, 24836, 25549, 25555, 26266, 26270, 26997, 27001]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [24105, 24111, 24828, 24830, 25557, 25561, 26280, 26292, 26987, 26991]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 24123..27001 (10 distinct values)
  SPREAD   b session_save store_read_bytes 24105..26991 (10 distinct values)
report   bench-results/loading-warm.json
mode warm, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### Tool latency

| Tool case | median | p95 | min | max | samples |
|---|---|---|---|---|---|
| read (tiny file) | 0.3 ms | 0.4 ms | 0.3 ms | 0.4 ms | 5 |
| read (1000 lines) | 0.4 ms | 0.5 ms | 0.4 ms | 0.5 ms | 5 |
| read (10 lines @ offset) | 0.4 ms | 0.4 ms | 0.4 ms | 0.4 ms | 5 |
| read (repo Cargo.toml) | 0.4 ms | 0.4 ms | 0.3 ms | 0.4 ms | 5 |
| write (small) | 1.1 ms | 1.2 ms | 1.1 ms | 1.2 ms | 5 |
| write (10KB) | 1.3 ms | 1.3 ms | 1.2 ms | 1.3 ms | 5 |
| edit (simple replace) | 1.2 ms | 1.2 ms | 1.1 ms | 1.2 ms | 5 |
| ls (scripts dir) | 0.7 ms | 0.8 ms | 0.7 ms | 0.8 ms | 5 |
| agentgrep (grep in tree) | 1.6 ms | 1.6 ms | 1.6 ms | 1.6 ms | 5 |
| agentgrep (find **/*.rs) | 1.7 ms | 1.7 ms | 1.7 ms | 1.7 ms | 5 |
| bash (true) | 1.7 ms | 1.8 ms | 1.7 ms | 1.8 ms | 5 |
| bash (echo hello) | 1.7 ms | 1.8 ms | 1.7 ms | 1.8 ms | 5 |
| bash (ls fixture tree) | 2.9 ms | 2.9 ms | 2.8 ms | 2.9 ms | 5 |
| invalid tool (dispatch only) | 0.1 ms | 0.1 ms | 0.1 ms | 0.1 ms | 5 |

#### Reload storm

1 resume(s) per level; each level resumes sessions no other level has opened, from a freshly started daemon.

| Concurrent resumes | history median | p95 | max | storm wall | failures |
|---|---|---|---|---|---|
| 1 | 72.9 ms | 72.9 ms | 72.9 ms | 73.3 ms | 0 |
| 10 | 231.3 ms | 259.7 ms | 259.7 ms | 261.1 ms | 0 |
| 40 | 921.9 ms | 1.13 s | 1.14 s | 1.14 s | 0 |

#### Compile loop

| Checkpoint | Touched | median | min | max | samples |
|---|---|---|---|---|---|
| warm_noop | — | 222.3 ms | 222.3 ms | 222.3 ms | 1 |
| leaf_touch | src/main.rs | 354.4 ms | 354.4 ms | 354.4 ms | 1 |
| core_touch | crates/app-core/src/lib.rs | 16.90 s | 16.90 s | 16.90 s | 1 |
| build_meta_touch | crates/build-meta/src/lib.rs | 23.39 s | 23.39 s | 23.39 s | 1 |

### macOS

#### Coding agent comparison (headless, mock model)

Host: darwin/arm64, 3 CPUs, Python 3.12.10. Baseline: **horon**.

Skipped: syscalls (strace not available)

| Tool | Version | `--version` (median) | Install size |
|---|---|---|---|
| horon | horon-agent v0.98.1-dev (4a636d3) | 8.7 ms | 92.0 MB |
| jcode | jcode v0.99.1 (7206efd0c) | 16.6 ms | 136.3 MB |
| Claude Code | 2.1.317 (Claude Code) | 18.5 ms | 596.8 MB |
| Codex CLI | codex-cli 0.166.0 | 56.8 ms | 346.5 MB |
| OpenCode | 1.19.5 | 346.7 ms | 344.6 MB |
| pi | 0.73.1 | 520.3 ms | 145.6 MB |

**One-shot headless prompt** (includes startup, one `ls` tool call, final answer)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Tool round-trip | Note |
|---|---|---|---|---|---|---|---|
| horon | 182.7 ms | 171.0 ms–461.6 ms | baseline | 28.8 MB | baseline | yes | — |
| jcode | 577.5 ms | 530.0 ms–603.6 ms | 3.16× more | 33.9 MB | 1.18× more | yes | — |
| Claude Code | 345.3 ms | 334.5 ms–666.6 ms | 1.89× more | 189.0 MB | 6.57× more | yes | — |
| Codex CLI | 350.5 ms | 246.1 ms–1.17 s | 1.92× more | 143.4 MB | 4.99× more | yes | — |
| OpenCode | 2.14 s | 1.72 s–2.62 s | 11.69× more | 671.5 MB | 23.36× more | yes | — |
| pi | 726.0 ms | 621.0 ms–726.8 ms | 3.97× more | 173.9 MB | 6.05× more | yes | — |

**Session memory** (idle after 3 turn(s) per session; sizes are process-tree sums, RSS; PSS also recorded on Linux). "Each additional" is the least-squares slope across the measured session counts; 0 B means growth stayed below RSS sampling noise. Threads and descriptors come from the daemon's own `server:memory` after the turns.

| Tool | First session | Each additional | vs horon (additional) | Threads/session | FDs/session | First (post-turn) | Each additional (post-turn) |
|---|---|---|---|---|---|---|---|
| horon | 29.2 MB | 280.1 KB | baseline | 0.23 | 1.00 | 34.4 MB | 527.0 KB |
| jcode | 36.5 MB | 194.8 KB | 1.44× less | — | — | 40.1 MB | 341.4 KB |
| Claude Code | 156.2 MB | 155.6 MB | 568.90× more | — | — | 188.4 MB | 183.2 MB |
| Codex CLI | 123.7 MB | 80.5 MB | 294.29× more | — | — | 106.8 MB | 107.6 MB |
| OpenCode | 265.5 MB | 0.0 B | — | — | — | 568.0 MB | 16.0 MB |
| pi | 167.3 MB | 140.0 MB | 511.93× more | — | — | 192.1 MB | 168.5 MB |

Threads/descriptors unavailable: jcode (server:memory did not report thread_count, fd_count)

**Post-turn tree RSS per session count**

| Tool | 1 session(s) | 5 session(s) | 10 session(s) |
|---|---|---|---|
| horon | 34.4 MB | 36.8 MB | 39.0 MB |
| jcode | 40.1 MB | 41.4 MB | 43.1 MB |
| Claude Code | 188.4 MB | 928.9 MB | 1.8 GB |
| Codex CLI | 106.8 MB | 535.1 MB | 1.1 GB |
| OpenCode | 568.0 MB | 596.4 MB | 709.2 MB |
| pi | 192.1 MB | 960.5 MB | 1.7 GB |

#### 기동

```text
Benchmarking: /Users/runner/work/agent/agent/target/release/horon-agent
============================================================

--help (binary load):
  Min:    7.84 ms
  Max:    11.40 ms
  Mean:   9.01 ms
  Median: 8.67 ms
  Stdev:  1.05 ms

--version:
  Min:    7.43 ms
  Max:    12.90 ms
  Mean:   9.35 ms
  Median: 8.52 ms
  Stdev:  1.85 ms

Measuring isolated server startup (10 runs)...

Server ready (isolated socket connectable):
  Min:    35.05 ms
  Max:    186.92 ms
  Mean:   125.45 ms
  Median: 132.05 ms
  Stdev:  48.95 ms

Measuring harness bridge readiness (10 runs)...

Bridge stage: api socket connectable:
  Min:    32.91 ms
  Max:    184.04 ms
  Mean:   110.28 ms
  Median: 133.21 ms
  Stdev:  59.03 ms

Bridge stage: hello_ok:
  Min:    33.54 ms
  Max:    185.37 ms
  Mean:   112.33 ms
  Median: 134.45 ms
  Stdev:  58.59 ms

Bridge stage: no-op reply:
  Min:    33.95 ms
  Max:    186.99 ms
  Mean:   113.94 ms
  Median: 135.54 ms
  Stdev:  58.15 ms

Bridge total (socket -> no-op):
  Min:    33.95 ms
  Max:    186.99 ms
  Mean:   113.94 ms
  Median: 135.54 ms
  Stdev:  58.15 ms

============================================================
Summary:
  Binary load median:      ~8.7 ms
  Server ready median:     ~132.0 ms
  Bridge total median:     ~135.5 ms
```

#### 세션 로딩 (COLD)

```text
binary   /Users/runner/work/agent/agent/target/release/examples/loading_probe (f54c9f6a5a44)
mode     cold
protocol COLD: every measured run gets its own freshly copied HOME; no derived cache from an earlier run exists, so each sample is a first load of the fixture.
fixtures /var/folders/h5/t85nd63d71x27_8gl5gw3l_w0000gn/T/horon-loading-bench-0ah2o6qo
note     --warmup 3 is ignored in cold mode: every sample gets a fresh HOME
fixture events=0 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    0.037/0.035    p95    0.069/0.043    delta  -0.002 ms  control-no-change
  skill_request_overlay p50    0.191/0.192    p95    0.275/0.233    delta  +0.001 ms  control-no-change
  mcp_config_load   p50    0.278/0.283    p95    0.336/0.346    delta  +0.005 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.001    delta  +0.000 ms  control-no-change
  session_save      p50   13.368/13.025   p95   67.938/17.138   delta  -0.342 ms  control-no-change
  total store_read_bytes p50 23252  full_source_hashes p50 1  skill_dir_reads p50 10
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 23244..23260 (5 distinct values)
  SPREAD   b session_save store_read_bytes 23240..23260 (5 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    3.983/4.033    p95    4.739/4.271    delta  +0.050 ms  control-no-change
  skill_request_overlay p50   36.136/35.972   p95   39.152/38.201   delta  -0.164 ms  control-no-change
  mcp_config_load   p50    0.311/0.329    p95    0.347/0.351    delta  +0.018 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.001/0.001    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  session_save      p50   16.018/15.943   p95   18.472/17.567   delta  -0.076 ms  control-no-change
  total store_read_bytes p50 23254  full_source_hashes p50 1  skill_dir_reads p50 10
  total skill_file_reads p50 2000  skill_bytes_read p50 243900  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 23240..23260 (6 distinct values)
  SPREAD   b session_save store_read_bytes 23240..23260 (5 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.037/0.038    p95    0.050/0.048    delta  +0.000 ms  control-no-change
  skill_request_overlay p50    0.205/0.205    p95    0.323/0.312    delta  +0.000 ms  control-no-change
  mcp_config_load   p50    0.300/0.311    p95    0.322/0.361    delta  +0.011 ms  control-no-change
  attach_history    p50  339.362/339.671  p95  359.419/366.253  delta  +0.310 ms  control-no-change
  resume_ready      p50   63.869/66.451   p95   71.215/75.378   delta  +2.582 ms  control-no-change
  provider_dispatch p50    1.192/1.169    p95    1.295/1.291    delta  -0.023 ms  control-no-change
  session_save      p50   14.424/15.470   p95   16.542/21.142   delta  +1.047 ms  control-no-change
  total store_read_bytes p50 5311614  full_source_hashes p50 2  skill_dir_reads p50 10
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189203..4189207 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099141..1099145 (3 distinct values)
  SPREAD   a session_save store_read_bytes 23248..23274 (8 distinct values)
  SPREAD   b attach_history store_read_bytes 4189203..4189207 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099141..1099145 (3 distinct values)
  SPREAD   b session_save store_read_bytes 23254..23274 (8 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    3.892/3.819    p95    4.711/3.972    delta  -0.073 ms  control-no-change
  skill_request_overlay p50   36.919/34.996   p95   41.802/38.725   delta  -1.923 ms  control-no-change
  mcp_config_load   p50    0.324/0.299    p95    0.361/0.485    delta  -0.025 ms  control-no-change
  attach_history    p50  301.636/297.670  p95  325.857/321.000  delta  -3.966 ms  control-no-change
  resume_ready      p50   58.443/58.913   p95   62.383/70.603   delta  +0.470 ms  control-no-change
  provider_dispatch p50    1.084/1.119    p95    1.118/1.232    delta  +0.035 ms  control-no-change
  session_save      p50   13.077/12.973   p95   16.083/18.182   delta  -0.104 ms  control-no-change
  total store_read_bytes p50 5311614  full_source_hashes p50 2  skill_dir_reads p50 10
  total skill_file_reads p50 2000  skill_bytes_read p50 243900  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189203..4189207 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099141..1099145 (2 distinct values)
  SPREAD   a session_save store_read_bytes 23248..23270 (6 distinct values)
  SPREAD   b attach_history store_read_bytes 4189199..4189207 (4 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099137..1099145 (4 distinct values)
  SPREAD   b session_save store_read_bytes 23249..23269 (7 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.035/0.033    p95    0.044/0.054    delta  -0.002 ms  control-no-change
  skill_request_overlay p50    0.189/0.196    p95    0.237/0.342    delta  +0.007 ms  control-no-change
  mcp_config_load   p50    0.283/0.288    p95    0.319/0.361    delta  +0.006 ms  control-no-change
  attach_history    p50 3349.028/3348.984 p95 3453.094/3589.809 delta  -0.044 ms  control-no-change
  resume_ready      p50  593.492/623.834  p95  628.573/691.782  delta +30.341 ms  control-no-change
  provider_dispatch p50   10.456/10.574   p95   11.549/12.093   delta  +0.118 ms  control-no-change
  session_save      p50   15.505/16.462   p95   18.065/19.540   delta  +0.957 ms  control-no-change
  total store_read_bytes p50 53417825  full_source_hashes p50 2  skill_dir_reads p50 10
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184303..42184305 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210253..11210255 (3 distinct values)
  SPREAD   a session_save store_read_bytes 23251..23273 (6 distinct values)
  SPREAD   b attach_history store_read_bytes 42184299..42184305 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210250..11210255 (2 distinct values)
  SPREAD   b session_save store_read_bytes 23257..23273 (6 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    3.611/3.590    p95    4.523/3.699    delta  -0.022 ms  control-no-change
  skill_request_overlay p50   33.836/33.633   p95   41.754/33.983   delta  -0.204 ms  control-no-change
  mcp_config_load   p50    0.283/0.280    p95    0.361/0.292    delta  -0.004 ms  control-no-change
  attach_history    p50 3270.446/3251.020 p95 3510.623/3308.226 delta -19.426 ms  control-no-change
  resume_ready      p50  592.130/591.730  p95  658.848/619.312  delta  -0.401 ms  control-no-change
  provider_dispatch p50   10.501/10.435   p95   11.321/10.941   delta  -0.065 ms  control-no-change
  session_save      p50   13.403/14.965   p95   16.148/16.622   delta  +1.562 ms  control-no-change
  total store_read_bytes p50 53417825  full_source_hashes p50 2  skill_dir_reads p50 10
  total skill_file_reads p50 2000  skill_bytes_read p50 243900  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184297..42184305 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210247..11210255 (3 distinct values)
  SPREAD   a session_save store_read_bytes 23261..23273 (4 distinct values)
  SPREAD   b attach_history store_read_bytes 42184301..42184305 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210251..11210255 (2 distinct values)
  SPREAD   b session_save store_read_bytes 23253..23277 (8 distinct values)
report   bench-results/loading-cold.json
mode cold, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### 세션 로딩 (WARM)

```text
binary   /Users/runner/work/agent/agent/target/release/examples/loading_probe (f54c9f6a5a44)
mode     warm
protocol WARM: one HOME per side, --warmup discarded runs per side, then measured runs alternate between the two sides on those same HOMEs.
fixtures /var/folders/h5/t85nd63d71x27_8gl5gw3l_w0000gn/T/horon-loading-bench-1t50n6t0
fixture events=0 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    0.034/0.033    p95    0.044/0.050    delta  -0.001 ms  control-no-change
  skill_request_overlay p50    0.190/0.198    p95    0.324/0.311    delta  +0.009 ms  control-no-change
  mcp_config_load   p50    0.271/0.266    p95    0.297/0.345    delta  -0.004 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.001    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  session_save      p50   11.870/11.791   p95   13.023/25.614   delta  -0.079 ms  control-no-change
  total store_read_bytes p50 23964  full_source_hashes p50 1  skill_dir_reads p50 10
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23241, 23245, 23257, 23963, 23965, 23971, 24657, 24689, 24693]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23243, 23249, 23261, 23959, 23967, 23971, 24685, 24689]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23241..24693 (9 distinct values)
  SPREAD   b session_save store_read_bytes 23243..24689 (8 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    3.585/3.569    p95    4.160/3.811    delta  -0.015 ms  control-no-change
  skill_request_overlay p50   33.612/33.511   p95   37.253/34.110   delta  -0.101 ms  control-no-change
  mcp_config_load   p50    0.287/0.275    p95    0.310/0.304    delta  -0.012 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.001/0.001    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-drift
  session_save      p50   13.866/13.348   p95   14.395/13.854   delta  -0.518 ms  control-no-change
  total store_read_bytes p50 23953  full_source_hashes p50 1  skill_dir_reads p50 10
  total skill_file_reads p50 2000  skill_bytes_read p50 243900  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23247, 23249, 23253, 23263, 23949, 23957, 24683]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23241, 23249, 23253, 23967, 23971, 24681, 24689, 24693]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23247..24683 (7 distinct values)
  SPREAD   b session_save store_read_bytes 23241..24693 (8 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.030/0.032    p95    0.040/0.034    delta  +0.002 ms  control-no-change
  skill_request_overlay p50    0.181/0.181    p95    0.205/0.190    delta  +0.000 ms  control-no-change
  mcp_config_load   p50    0.266/0.266    p95    0.290/0.278    delta  -0.001 ms  control-no-change
  attach_history    p50   95.297/94.803   p95   98.870/95.766   delta  -0.494 ms  control-no-change
  resume_ready      p50   56.583/56.510   p95   63.830/58.285   delta  -0.073 ms  control-no-change
  provider_dispatch p50    1.095/1.088    p95    1.169/1.206    delta  -0.008 ms  control-no-change
  session_save      p50   12.122/11.961   p95   13.471/13.011   delta  -0.161 ms  control-no-change
  total store_read_bytes p50 2841556  full_source_hashes p50 1  skill_dir_reads p50 10
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23975, 24685, 25397, 25403, 26113, 26115, 26821, 26827]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23967, 23971, 24689, 24697, 25403, 26099, 26125, 26823, 26841]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23975..26827 (8 distinct values)
  SPREAD   b session_save store_read_bytes 23967..26841 (9 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    3.578/3.589    p95    4.104/4.444    delta  +0.011 ms  control-no-change
  skill_request_overlay p50   33.517/33.680   p95   38.200/39.944   delta  +0.163 ms  control-no-change
  mcp_config_load   p50    0.278/0.285    p95    0.313/0.364    delta  +0.007 ms  control-no-change
  attach_history    p50   94.828/95.605   p95  109.123/114.313  delta  +0.777 ms  control-no-change
  resume_ready      p50   57.465/56.492   p95   66.994/69.129   delta  -0.973 ms  control-no-change
  provider_dispatch p50    1.105/1.103    p95    1.166/1.215    delta  -0.003 ms  control-no-change
  session_save      p50   14.505/13.222   p95   16.961/15.713   delta  -1.283 ms  control-no-change
  total store_read_bytes p50 2841564  full_source_hashes p50 1  skill_dir_reads p50 10
  total skill_file_reads p50 2000  skill_bytes_read p50 243900  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23972, 23976, 24686, 24702, 25400, 25412, 26114, 26828, 26832]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23968, 23976, 24678, 24690, 25396, 25416, 26114, 26122, 26832, 26848]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23972..26832 (9 distinct values)
  SPREAD   b session_save store_read_bytes 23968..26848 (10 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.029/0.029    p95    0.030/0.036    delta  +0.000 ms  control-no-change
  skill_request_overlay p50    0.183/0.182    p95    0.207/0.223    delta  -0.001 ms  control-no-change
  mcp_config_load   p50    0.267/0.269    p95    0.277/0.324    delta  +0.002 ms  control-no-change
  attach_history    p50  976.724/984.812  p95 1064.309/1047.990 delta  +8.088 ms  control-no-change
  resume_ready      p50  589.421/595.531  p95  690.494/654.744  delta  +6.110 ms  control-no-change
  provider_dispatch p50   10.502/10.646   p95   12.453/11.600   delta  +0.144 ms  control-no-change
  session_save      p50   13.025/12.824   p95   15.512/14.852   delta  -0.200 ms  control-no-change
  total store_read_bytes p50 28640574  full_source_hashes p50 1  skill_dir_reads p50 10
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23969, 23987, 24689, 24701, 25395, 25403, 26107, 26121, 26829, 26839]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23983, 23995, 24693, 24699, 25407, 25415, 26115, 26125, 26837, 26847]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23969..26839 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23983..26847 (10 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    3.583/3.601    p95    4.132/4.006    delta  +0.018 ms  control-no-change
  skill_request_overlay p50   33.499/33.932   p95   37.071/37.188   delta  +0.433 ms  control-no-change
  mcp_config_load   p50    0.272/0.289    p95    0.306/0.342    delta  +0.018 ms  control-drift
  attach_history    p50  980.367/1023.664 p95 1080.951/1085.852 delta +43.297 ms  control-drift
  resume_ready      p50  594.715/592.103  p95  646.907/647.829  delta  -2.612 ms  control-no-change
  provider_dispatch p50   10.527/10.472   p95   13.139/11.464   delta  -0.055 ms  control-no-change
  session_save      p50   14.109/13.624   p95   15.893/15.120   delta  -0.485 ms  control-no-change
  total store_read_bytes p50 28640575  full_source_hashes p50 1  skill_dir_reads p50 10
  total skill_file_reads p50 2000  skill_bytes_read p50 243900  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23979, 24679, 24697, 25399, 25401, 26113, 26123, 26829, 26831]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23971, 23983, 24693, 24701, 25397, 25407, 26113, 26123, 26837, 26839]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23979..26831 (9 distinct values)
  SPREAD   b session_save store_read_bytes 23971..26839 (10 distinct values)
report   bench-results/loading-warm.json
mode warm, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### Tool latency

| Tool case | median | p95 | min | max | samples |
|---|---|---|---|---|---|
| read (tiny file) | 0.3 ms | 0.8 ms | 0.2 ms | 0.8 ms | 5 |
| read (1000 lines) | 0.6 ms | 0.6 ms | 0.6 ms | 0.6 ms | 5 |
| read (10 lines @ offset) | 0.5 ms | 0.6 ms | 0.5 ms | 0.6 ms | 5 |
| read (repo Cargo.toml) | 0.3 ms | 0.4 ms | 0.3 ms | 0.4 ms | 5 |
| write (small) | 1.3 ms | 1.7 ms | 1.2 ms | 1.7 ms | 5 |
| write (10KB) | 1.7 ms | 15.5 ms | 1.6 ms | 15.5 ms | 5 |
| edit (simple replace) | 1.5 ms | 2.0 ms | 1.3 ms | 2.0 ms | 5 |
| ls (scripts dir) | 1.0 ms | 1.2 ms | 0.9 ms | 1.2 ms | 5 |
| agentgrep (grep in tree) | 2.4 ms | 3.0 ms | 2.3 ms | 3.0 ms | 5 |
| agentgrep (find **/*.rs) | 1.7 ms | 1.8 ms | 1.7 ms | 1.8 ms | 5 |
| bash (true) | 3.4 ms | 4.1 ms | 3.0 ms | 4.1 ms | 5 |
| bash (echo hello) | 3.6 ms | 4.0 ms | 3.1 ms | 4.0 ms | 5 |
| bash (ls fixture tree) | 6.0 ms | 6.9 ms | 5.4 ms | 6.9 ms | 5 |
| invalid tool (dispatch only) | 0.1 ms | 0.1 ms | 0.1 ms | 0.1 ms | 5 |

#### Reload storm

1 resume(s) per level; each level resumes sessions no other level has opened, from a freshly started daemon.

| Concurrent resumes | history median | p95 | max | storm wall | failures |
|---|---|---|---|---|---|
| 1 | 84.7 ms | 84.7 ms | 84.7 ms | 84.8 ms | 0 |
| 10 | 310.5 ms | 311.2 ms | 311.2 ms | 312.0 ms | 0 |
| 40 | 935.2 ms | 937.9 ms | 938.6 ms | 939.0 ms | 0 |

#### Compile loop

| Checkpoint | Touched | median | min | max | samples |
|---|---|---|---|---|---|
| warm_noop | — | 794.5 ms | 794.5 ms | 794.5 ms | 1 |
| leaf_touch | src/main.rs | 421.4 ms | 421.4 ms | 421.4 ms | 1 |
| core_touch | crates/app-core/src/lib.rs | 14.81 s | 14.81 s | 14.81 s | 1 |
| build_meta_touch | crates/build-meta/src/lib.rs | 19.94 s | 19.94 s | 19.94 s | 1 |

### Windows

#### Coding agent comparison (headless, mock model)

Host: windows/AMD64, 4 CPUs, Python 3.12.10. Baseline: **horon**.

Skipped: syscalls (strace not available)

| Tool | Version | `--version` (median) | Install size |
|---|---|---|---|
| horon | horon-agent v0.98.1-dev (4a636d3) | 13.1 ms | 84.0 MB |
| jcode | jcode v0.99.1 (7206efd0c) | 13.9 ms | 128.4 MB |
| Claude Code | 2.1.317 (Claude Code) | 37.4 ms | 645.2 MB |
| Codex CLI | codex-cli 0.166.0 | 91.5 ms | 479.4 MB |
| OpenCode | 1.19.5 | 462.6 ms | 605.3 MB |
| pi | 0.73.1 | 868.5 ms | 142.7 MB |

**One-shot headless prompt** (includes startup, one `ls` tool call, final answer)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Tool round-trip | Note |
|---|---|---|---|---|---|---|---|
| horon | 281.1 ms | 269.9 ms–299.9 ms | baseline | 30.8 MB | baseline | yes | — |
| jcode | 122.5 ms | 119.6 ms–211.5 ms | 2.29× less | 32.5 MB | 1.06× more | yes | — |
| Claude Code | 1.25 s | 1.22 s–1.57 s | 4.46× more | 278.2 MB | 9.04× more | yes | — |
| Codex CLI | 827.1 ms | 640.5 ms–1.32 s | 2.94× more | 162.4 MB | 5.28× more | yes | — |
| OpenCode | 4.39 s | 4.23 s–5.81 s | 15.63× more | 861.4 MB | 27.99× more | yes | — |
| pi | 1.04 s | 1.02 s–1.06 s | 3.69× more | 156.5 MB | 5.09× more | yes | — |

**Session memory** (idle after 3 turn(s) per session; sizes are process-tree sums, RSS; PSS also recorded on Linux). "Each additional" is the least-squares slope across the measured session counts; 0 B means growth stayed below RSS sampling noise. Threads and descriptors come from the daemon's own `server:memory` after the turns.

| Tool | First session | Each additional | vs horon (additional) | Threads/session | FDs/session | First (post-turn) | Each additional (post-turn) |
|---|---|---|---|---|---|---|---|
| horon | 23.3 MB | 235.5 KB | baseline | — | — | 30.2 MB | 555.0 KB |
| jcode | 22.1 MB | 80.6 KB | 2.92× less | — | — | 27.2 MB | 319.6 KB |
| Claude Code | 155.3 MB | 161.5 MB | 702.02× more | — | — | 188.7 MB | 191.2 MB |
| Codex CLI | 124.7 MB | 97.7 MB | 424.64× more | — | — | 158.4 MB | 120.3 MB |
| OpenCode | 246.4 MB | 488.1 KB | 2.07× more | — | — | 435.2 MB | 22.6 MB |
| pi | 131.4 MB | 74.5 MB | 324.04× more | — | — | 136.4 MB | 138.8 MB |

Threads/descriptors unavailable: horon (server:memory did not report thread_count, fd_count), jcode (server:memory did not report thread_count, fd_count)

**Post-turn tree RSS per session count**

| Tool | 1 session(s) | 5 session(s) | 10 session(s) |
|---|---|---|---|
| horon | 30.2 MB | 33.5 MB | 35.2 MB |
| jcode | 27.2 MB | 29.1 MB | 30.0 MB |
| Claude Code | 188.7 MB | 955.2 MB | 1.9 GB |
| Codex CLI | 158.4 MB | 634.7 MB | 1.2 GB |
| OpenCode | 435.2 MB | 490.9 MB | 635.8 MB |
| pi | 136.4 MB | 682.6 MB | 1.4 GB |

#### 기동

```text
Benchmarking: D:/a/agent/agent/target/release/horon-agent.exe
============================================================

--help (binary load):
  Min:    11.28 ms
  Max:    13.95 ms
  Mean:   11.69 ms
  Median: 11.45 ms
  Stdev:  0.80 ms

--version:
  Min:    10.91 ms
  Max:    12.08 ms
  Mean:   11.23 ms
  Median: 11.16 ms
  Stdev:  0.34 ms

Measuring isolated server startup (10 runs)...

Server ready (isolated socket connectable):
  Min:    21.87 ms
  Max:    22.65 ms
  Mean:   22.13 ms
  Median: 22.06 ms
  Stdev:  0.25 ms

Measuring harness bridge readiness (10 runs)...
  skipped: api-bridge is Unix-only, so Windows has no harness bridge to time

Bridge stage: api socket connectable: No successful runs

Bridge stage: hello_ok: No successful runs

Bridge stage: no-op reply: No successful runs

Bridge total (socket -> no-op): No successful runs

============================================================
Summary:
  Binary load median:      ~11.5 ms
  Server ready median:     ~22.1 ms
  Bridge total median:     n/a
```

#### 세션 로딩 (COLD)

```text
binary   D:\a\agent\agent\target\release\examples\loading_probe.exe (7f5f9cee872b)
mode     cold
protocol COLD: every measured run gets its own freshly copied HOME; no derived cache from an earlier run exists, so each sample is a first load of the fixture.
fixtures C:\Users\RUNNER~1\AppData\Local\Temp\horon-loading-bench-3lgpnbzv
note     --warmup 3 is ignored in cold mode: every sample gets a fresh HOME
fixture events=0 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    0.142/0.145    p95    0.155/0.179    delta  +0.003 ms  control-no-change
  skill_request_overlay p50    0.873/0.863    p95    1.080/0.926    delta  -0.010 ms  control-no-change
  mcp_config_load   p50    1.164/1.183    p95    1.252/1.404    delta  +0.019 ms  control-no-change
  attach_history    p50    0.002/0.002    p95    0.003/0.003    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.002/0.002    p95    0.003/0.002    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  session_save      p50   47.735/48.389   p95   68.808/106.965  delta  +0.654 ms  control-no-change
  total store_read_bytes p50 23254  full_source_hashes p50 1  skill_dir_reads p50 10
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 23214..23276 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23223..23276 (9 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50   16.684/17.314   p95   22.885/19.422   delta  +0.630 ms  control-no-change
  skill_request_overlay p50  143.594/145.397  p95  148.787/155.335  delta  +1.803 ms  control-no-change
  mcp_config_load   p50    1.280/1.276    p95    1.470/1.362    delta  -0.004 ms  control-no-change
  attach_history    p50    0.002/0.002    p95    0.004/0.003    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.002/0.002    p95    0.002/0.002    delta  +0.000 ms  control-drift
  provider_dispatch p50    0.000/0.000    p95    0.000/0.001    delta  +0.000 ms  control-no-change
  session_save      p50   59.680/54.206   p95   86.798/187.132  delta  -5.475 ms  control-no-change
  total store_read_bytes p50 23252  full_source_hashes p50 1  skill_dir_reads p50 10
  total skill_file_reads p50 2000  skill_bytes_read p50 245100  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 23210..23278 (9 distinct values)
  SPREAD   b session_save store_read_bytes 23193..23272 (9 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.141/0.147    p95    0.163/0.195    delta  +0.006 ms  control-no-change
  skill_request_overlay p50    0.919/0.922    p95    1.056/0.995    delta  +0.002 ms  control-no-change
  mcp_config_load   p50    1.192/1.201    p95    1.285/1.313    delta  +0.009 ms  control-no-change
  attach_history    p50  492.836/493.525  p95  545.048/525.681  delta  +0.689 ms  control-no-change
  resume_ready      p50  161.305/161.937  p95  176.331/168.365  delta  +0.632 ms  control-no-change
  provider_dispatch p50    1.748/1.693    p95    2.211/1.794    delta  -0.055 ms  control-no-change
  session_save      p50   46.363/44.597   p95  214.635/169.706  delta  -1.766 ms  control-no-change
  total store_read_bytes p50 5311525  full_source_hashes p50 2  skill_dir_reads p50 10
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189151..4189155 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099102..1099106 (3 distinct values)
  SPREAD   a session_save store_read_bytes 23238..23284 (8 distinct values)
  SPREAD   b attach_history store_read_bytes 4189153..4189155 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099105..1099106 (2 distinct values)
  SPREAD   b session_save store_read_bytes 23248..23290 (8 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50   16.837/17.283   p95   17.101/18.561   delta  +0.446 ms  control-no-change
  skill_request_overlay p50  144.462/144.335  p95  149.889/151.745  delta  -0.127 ms  control-no-change
  mcp_config_load   p50    1.305/1.279    p95    1.575/1.419    delta  -0.026 ms  control-no-change
  attach_history    p50  502.420/518.255  p95  588.741/629.261  delta +15.836 ms  control-no-change
  resume_ready      p50  159.902/163.448  p95  172.092/166.081  delta  +3.546 ms  control-no-change
  provider_dispatch p50    1.651/1.669    p95    1.730/2.049    delta  +0.018 ms  control-no-change
  session_save      p50   50.258/59.420   p95  215.273/253.106  delta  +9.163 ms  control-no-change
  total store_read_bytes p50 5311524  full_source_hashes p50 2  skill_dir_reads p50 10
  total skill_file_reads p50 2000  skill_bytes_read p50 245100  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189151..4189153 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099104..1099105 (2 distinct values)
  SPREAD   a session_save store_read_bytes 23231..23290 (10 distinct values)
  SPREAD   b attach_history store_read_bytes 4189149..4189153 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099101..1099105 (3 distinct values)
  SPREAD   b session_save store_read_bytes 23231..23270 (7 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.146/0.141    p95    0.198/0.171    delta  -0.005 ms  control-no-change
  skill_request_overlay p50    0.958/0.953    p95    1.184/0.999    delta  -0.005 ms  control-no-change
  mcp_config_load   p50    1.185/1.152    p95    1.388/1.211    delta  -0.033 ms  control-no-change
  attach_history    p50 5533.762/5551.475 p95 5671.540/5765.901 delta +17.713 ms  control-no-change
  resume_ready      p50 1717.872/1723.641 p95 1741.554/1745.354 delta  +5.770 ms  control-no-change
  provider_dispatch p50   19.264/19.815   p95   22.114/22.014   delta  +0.551 ms  control-no-change
  session_save      p50   45.788/63.550   p95  104.174/177.942  delta +17.762 ms  control-drift
  total store_read_bytes p50 53417734  full_source_hashes p50 2  skill_dir_reads p50 10
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184249..42184253 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210212..11210216 (2 distinct values)
  SPREAD   a session_save store_read_bytes 23233..23289 (9 distinct values)
  SPREAD   b attach_history store_read_bytes 42184249..42184253 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210214..11210216 (3 distinct values)
  SPREAD   b session_save store_read_bytes 23241..23291 (9 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50   17.019/17.109   p95   19.902/17.689   delta  +0.090 ms  control-no-change
  skill_request_overlay p50  148.756/149.745  p95  155.028/152.710  delta  +0.989 ms  control-no-change
  mcp_config_load   p50    1.248/1.292    p95    1.454/1.376    delta  +0.044 ms  control-no-change
  attach_history    p50 5545.606/5572.472 p95 5685.537/5642.762 delta +26.866 ms  control-no-change
  resume_ready      p50 1702.469/1702.101 p95 1725.814/1734.200 delta  -0.368 ms  control-no-change
  provider_dispatch p50   19.873/19.636   p95   21.330/21.244   delta  -0.237 ms  control-no-change
  session_save      p50   50.257/51.471   p95  260.050/594.905  delta  +1.213 ms  control-no-change
  total store_read_bytes p50 53417727  full_source_hashes p50 2  skill_dir_reads p50 10
  total skill_file_reads p50 2000  skill_bytes_read p50 245100  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 23225..23295 (10 distinct values)
  SPREAD   b attach_history store_read_bytes 42184251..42184253 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210215..11210216 (2 distinct values)
  SPREAD   b session_save store_read_bytes 23227..23287 (9 distinct values)
report   bench-results\loading-cold.json
mode cold, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### 세션 로딩 (WARM)

```text
binary   D:\a\agent\agent\target\release\examples\loading_probe.exe (7f5f9cee872b)
mode     warm
protocol WARM: one HOME per side, --warmup discarded runs per side, then measured runs alternate between the two sides on those same HOMEs.
fixtures C:\Users\RUNNER~1\AppData\Local\Temp\horon-loading-bench-ln5agshp
fixture events=0 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    0.147/0.137    p95    0.185/0.166    delta  -0.009 ms  control-no-change
  skill_request_overlay p50    0.893/0.874    p95    1.099/0.922    delta  -0.020 ms  control-no-change
  mcp_config_load   p50    1.178/1.151    p95    1.266/1.223    delta  -0.027 ms  control-no-change
  attach_history    p50    0.002/0.002    p95    0.002/0.002    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.002/0.002    p95    0.002/0.002    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  session_save      p50   73.431/49.609   p95  259.842/254.270  delta -23.822 ms  control-no-change
  total store_read_bytes p50 23944  full_source_hashes p50 1  skill_dir_reads p50 10
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23246, 23257, 23265, 23269, 23937, 23952, 23972, 24663, 24668, 24671]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23251, 23263, 23265, 23269, 23948, 23959, 23970, 24651, 24653, 24663]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23246..24671 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23251..24663 (10 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50   16.080/16.680   p95   18.668/17.229   delta  +0.600 ms  control-no-change
  skill_request_overlay p50  142.551/141.425  p95  159.275/145.739  delta  -1.125 ms  control-no-change
  mcp_config_load   p50    1.276/1.246    p95    1.353/1.278    delta  -0.030 ms  control-no-change
  attach_history    p50    0.002/0.002    p95    0.003/0.003    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.002/0.002    p95    0.002/0.002    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  session_save      p50   52.452/48.334   p95  310.243/267.515  delta  -4.118 ms  control-no-change
  total store_read_bytes p50 23963  full_source_hashes p50 1  skill_dir_reads p50 10
  total skill_file_reads p50 2000  skill_bytes_read p50 245100  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23243, 23245, 23259, 23265, 23960, 23966, 23970, 24633, 24655]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23260, 23265, 23266, 23277, 23940, 23961, 23984, 24635, 24657, 24669]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23243..24655 (9 distinct values)
  SPREAD   b session_save store_read_bytes 23260..24669 (10 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.143/0.138    p95    0.180/0.196    delta  -0.004 ms  control-no-change
  skill_request_overlay p50    0.877/0.855    p95    0.959/0.921    delta  -0.021 ms  control-no-change
  mcp_config_load   p50    1.153/1.170    p95    1.261/1.252    delta  +0.017 ms  control-no-change
  attach_history    p50  219.065/217.160  p95  225.322/222.392  delta  -1.905 ms  control-no-change
  resume_ready      p50  162.554/161.541  p95  165.685/167.058  delta  -1.013 ms  control-no-change
  provider_dispatch p50    1.700/1.763    p95    1.790/1.947    delta  +0.063 ms  control-no-change
  session_save      p50   42.047/46.052   p95   76.105/163.406  delta  +4.005 ms  control-no-change
  total store_read_bytes p50 2841456  full_source_hashes p50 1  skill_dir_reads p50 10
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23935, 23983, 24649, 24656, 25373, 25380, 26071, 26080, 26771, 26778]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23955, 23967, 24668, 24670, 25353, 25361, 26016, 26064, 26749, 26753]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23935..26778 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23955..26753 (10 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50   17.050/17.282   p95   17.682/19.341   delta  +0.232 ms  control-no-change
  skill_request_overlay p50  150.272/150.189  p95  157.312/152.320  delta  -0.083 ms  control-no-change
  mcp_config_load   p50    1.290/1.315    p95    1.360/1.412    delta  +0.025 ms  control-no-change
  attach_history    p50  222.902/227.144  p95  228.558/229.431  delta  +4.243 ms  control-no-change
  resume_ready      p50  167.880/165.978  p95  173.770/172.575  delta  -1.902 ms  control-no-change
  provider_dispatch p50    1.765/1.775    p95    1.866/1.871    delta  +0.009 ms  control-no-change
  session_save      p50   68.519/45.791   p95  160.979/203.938  delta -22.727 ms  control-no-change
  total store_read_bytes p50 2841418  full_source_hashes p50 1  skill_dir_reads p50 10
  total skill_file_reads p50 2000  skill_bytes_read p50 245100  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23954, 23974, 24653, 24677, 25328, 25352, 26047, 26057, 26710, 26750]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23947, 23971, 24656, 24666, 25359, 25374, 26036, 26049, 26731, 26756]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23954..26750 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23947..26756 (10 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.141/0.142    p95    0.159/0.217    delta  +0.001 ms  control-no-change
  skill_request_overlay p50    0.904/0.938    p95    0.936/1.168    delta  +0.034 ms  control-no-change
  mcp_config_load   p50    1.146/1.210    p95    1.207/1.470    delta  +0.064 ms  control-no-change
  attach_history    p50 2396.298/2387.257 p95 2433.075/2404.091 delta  -9.041 ms  control-no-change
  resume_ready      p50 1737.041/1748.632 p95 1779.635/1768.095 delta +11.592 ms  control-no-change
  provider_dispatch p50   20.672/20.706   p95   22.356/22.174   delta  +0.033 ms  control-no-change
  session_save      p50   52.267/45.626   p95   92.617/111.911  delta  -6.641 ms  control-no-change
  total store_read_bytes p50 28640454  full_source_hashes p50 1  skill_dir_reads p50 10
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23949, 23961, 24662, 24692, 25340, 25379, 26041, 26071, 26770, 26780]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23923, 23973, 24581, 24660, 25340, 25375, 26046, 26058, 26757, 26761]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23949..26780 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23923..26761 (10 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50   17.230/17.159   p95   19.567/17.905   delta  -0.071 ms  control-no-change
  skill_request_overlay p50  149.311/150.168  p95  157.403/155.012  delta  +0.857 ms  control-no-change
  mcp_config_load   p50    1.216/1.243    p95    1.341/1.383    delta  +0.027 ms  control-no-change
  attach_history    p50 2357.981/2366.000 p95 2398.494/2386.525 delta  +8.019 ms  control-no-change
  resume_ready      p50 1730.558/1728.949 p95 1760.041/1750.437 delta  -1.609 ms  control-no-change
  provider_dispatch p50   20.550/21.091   p95   22.240/28.732   delta  +0.541 ms  control-no-change
  session_save      p50   56.380/47.711   p95  254.859/164.534  delta  -8.669 ms  control-no-change
  total store_read_bytes p50 28640465  full_source_hashes p50 1  skill_dir_reads p50 10
  total skill_file_reads p50 2000  skill_bytes_read p50 245100  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23969, 23989, 24648, 24680, 25367, 25373, 26018, 26080, 26767, 26783]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23899, 23954, 24659, 24665, 25350, 26071, 26073, 26756, 26768]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23969..26783 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23899..26768 (9 distinct values)
report   bench-results\loading-warm.json
mode warm, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### Tool latency

| Tool case | median | p95 | min | max | samples |
|---|---|---|---|---|---|
| read (tiny file) | 2.7 ms | 2.9 ms | 2.2 ms | 2.9 ms | 5 |
| read (1000 lines) | 2.7 ms | 2.7 ms | 2.2 ms | 2.7 ms | 5 |
| read (10 lines @ offset) | 2.8 ms | 2.8 ms | 2.3 ms | 2.8 ms | 5 |
| read (repo Cargo.toml) | 2.7 ms | 2.8 ms | 2.3 ms | 2.8 ms | 5 |
| write (small) | 50.8 ms | 87.1 ms | 7.4 ms | 87.1 ms | 5 |
| write (10KB) | 10.0 ms | 11.1 ms | 7.5 ms | 11.1 ms | 5 |
| edit (simple replace) | 10.4 ms | 20.8 ms | 7.4 ms | 20.8 ms | 5 |
| ls (scripts dir) | 2.7 ms | 2.8 ms | 2.2 ms | 2.8 ms | 5 |
| agentgrep (grep in tree) | 7.5 ms | 8.1 ms | 7.2 ms | 8.1 ms | 5 |
| agentgrep (find **/*.rs) | 5.3 ms | 5.4 ms | 4.8 ms | 5.4 ms | 5 |
| bash (true) | 12.1 ms | 12.7 ms | 10.5 ms | 12.7 ms | 5 |
| bash (echo hello) | 12.6 ms | 12.8 ms | 12.1 ms | 12.8 ms | 5 |
| bash (ls fixture tree) | 15.1 ms | 15.2 ms | 13.1 ms | 15.2 ms | 5 |
| invalid tool (dispatch only) | 2.7 ms | 2.7 ms | 2.7 ms | 2.7 ms | 5 |

#### Reload storm

1 resume(s) per level; each level resumes sessions no other level has opened, from a freshly started daemon.

| Concurrent resumes | history median | p95 | max | storm wall | failures |
|---|---|---|---|---|---|
| 1 | 224.4 ms | 224.4 ms | 224.4 ms | 224.6 ms | 0 |
| 10 | 519.9 ms | 524.9 ms | 524.9 ms | 525.3 ms | 0 |
| 40 | 1.44 s | 2.49 s | 2.75 s | 2.76 s | 0 |

#### Compile loop

| Checkpoint | Touched | median | min | max | samples |
|---|---|---|---|---|---|
| warm_noop | — | 599.2 ms | 599.2 ms | 599.2 ms | 1 |
| leaf_touch | src\main.rs | 814.1 ms | 814.1 ms | 814.1 ms | 1 |
| core_touch | crates\app-core\src\lib.rs | 25.89 s | 25.89 s | 25.89 s | 1 |
| build_meta_touch | crates\build-meta\src\lib.rs | 36.72 s | 36.72 s | 36.72 s | 1 |

<!-- bench-auto:end -->
