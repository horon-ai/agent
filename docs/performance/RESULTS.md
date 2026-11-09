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

세 변경을 넣었습니다. 명시 선택을 한 번도 하지 않은 세션은 `local-head-explicit.json`에 `false`를 쓰지 않고(A), 세션 writer는 이미 있는 `sessions/` 루트의 부모를 열 때마다 다시 flush하지 않습니다(B). 반대로 영속 런타임 저널은 새로 만든 폴더의 항목을 부모에 확정하도록 고쳤습니다(C). C 전에는 정전 뒤 이미 `Ok`를 돌려준 `runtime.jsonl`이 폴더째 사라질 수 있었습니다. 계약은 [`../protocol/SESSION_STORE.md`](../protocol/SESSION_STORE.md)에 있습니다. 측정은 2026-11-09 `benchmark.yml` 실행 42531992327이고, 기준 `dev`(`c3e3ceeb`)와 후보(`d9d2c836`)를 한 잡 안에서 같은 러너로 번갈아 쟀습니다. one-shot 표본은 5개입니다.

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

Windows 단발 실행·스킬 호출의 열세와 세션당 메모리 증가를 진단 기록으로 나눈 뒤, 데이터 안전을 깎지 않는 수정 하나만 넣었습니다. 실행 끝의 닫기는 상태만 바꾸는데도, 보조 기록(`AuxiliaryRecorded`)에 상태·pid·마지막 활동 시각(`runtime_history`)이 함께 들어 있어서 매번 이벤트 로그 커밋, 바인딩 확정, 파생 캐시 재작성을 모두 했습니다. 이 값은 읽는 곳이 없고 원본은 `local-runtime.json`이므로 보조 기록에서 뺐습니다([`../protocol/SESSION_STORE.md`](../protocol/SESSION_STORE.md)). 측정은 2026-11-09 `benchmark.yml` 실행 42572765871이고, 기준 `dev`(`fca59cd6`)와 후보(`1dd5cd2d`)를 한 잡 안에서 같은 러너로 번갈아 쟀습니다. 표본은 5개입니다.

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

`main` 에 올라간 커밋이 CI 와 구조 성능 게이트를 모두 통과하고 직전 측정 이후 에이전트 코드(`src`, `crates`, `Cargo.toml`, `Cargo.lock`, `.cargo`, `rust-toolchain.toml`, `scripts/perf`)가 바뀌었으면, `.github/workflows/benchmark-docs.yml` 이 Linux·macOS·Windows에서 전체 벤치마크 묶음을 OS마다 따로 돌리고 아래 구간을 `main` 과 `dev` 에서 함께 다시 씁니다. OS마다 절이 하나씩 있습니다. 측정 방법과 묶음별 뜻은 [`BENCHMARK.md`](BENCHMARK.md) 의 "자동 측정"에 있습니다.

<!-- bench-auto:start commit=7d0726ced806f3e3048e153ee1c33b70a337ff6b -->

이 구간은 `.github/workflows/benchmark-docs.yml` 이 자동으로 다시 씁니다. 손으로 고치지 않습니다.

- 커밋: `7d0726ced806f3e3048e153ee1c33b70a337ff6b`
- 측정 시각: 2026-11-09 09:21 UTC
- 실행: <https://github.com/horon-ai/agent/actions/runs/42586815039> (원본 JSON 은 OS별 `benchmark-<os>-<run_id>` 아티팩트)

OS마다 다른 GitHub 호스티드 러너에서 따로 잽니다. 기계가 다르므로 OS끼리 절대값을 비교하지 않고, 같은 OS 안의 상대 비교(horon 대비 배수)와 커밋 사이의 추세만 읽습니다.

| OS | 러너 | 바이너리 | 상태 |
| --- | --- | --- | --- |
| Linux | `ubuntu-24.04` | `horon-agent v0.1.0-dev (7d0726c)` | 성공 |
| macOS | `macos-latest` | `horon-agent v0.1.0-dev (7d0726c)` | 성공 |
| Windows | `windows-latest` | `horon-agent v0.1.0-dev (7d0726c)` | 성공 |

### Linux

#### Coding agent comparison (headless, mock model)

Host: linux/x86_64, 4 CPUs, Python 3.12.16. Baseline: **horon**.

| Tool | Version | `--version` (median) | Install size |
|---|---|---|---|
| horon | horon-agent v0.1.0-dev (7d0726c) | 3.4 ms | 110.5 MB |
| jcode | jcode v0.99.1 (7206efd0c) | 9.2 ms | 163.4 MB |
| Claude Code | 2.1.317 (Claude Code) | 8.2 ms | 632.2 MB |
| Codex CLI | codex-cli 0.166.0 | 37.1 ms | 435.0 MB |
| OpenCode | 1.19.5 | 458.8 ms | 613.4 MB |
| pi | 1.5.0 | 220.8 ms | 108.9 MB |
| oh-my-pi (omp) | omp/18.18.0 | 712.1 ms | 412.6 MB |
| oh-my-openagent (omo) | omo 5.1.64 (engine: senpi 2026.11.5-1) | 114.5 ms | 520.6 MB |

**One-shot headless prompt** (includes startup, one `ls` tool call, final answer)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Tool round-trip | fsync calls | Note |
|---|---|---|---|---|---|---|---|---|
| horon | 145.2 ms | 102.2 ms–808.8 ms | baseline | 38.6 MB | baseline | yes | 14 | — |
| jcode | 143.5 ms | 142.4 ms–151.9 ms | same | 41.6 MB | 1.08× more | yes | 5 | — |
| Claude Code | 442.7 ms | 417.8 ms–451.7 ms | 3.05× more | 204.6 MB | 5.31× more | yes | — | — |
| Codex CLI | 512.9 ms | 470.8 ms–530.6 ms | 3.53× more | 192.3 MB | 4.99× more | yes | 6 | — |
| OpenCode | 2.55 s | 2.52 s–2.82 s | 17.58× more | 567.1 MB | 14.70× more | yes | 4 | — |
| pi | 373.1 ms | 366.6 ms–377.3 ms | 2.57× more | 112.7 MB | 2.92× more | yes | — | — |
| oh-my-pi (omp) | 2.02 s | 1.92 s–2.32 s | 13.88× more | 394.6 MB | 10.23× more | yes | 71 | — |
| oh-my-openagent (omo) | 6.13 s | 6.09 s–6.49 s | 42.23× more | 670.6 MB | 17.39× more | yes | 36 | — |

**Skill invocation** (process start to final answer, including skill discovery and one load of the fixture skill)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | Note |
|---|---|---|---|---|---|---|---|---|
| horon | 51.8 ms | 51.1 ms–56.2 ms | baseline | 35.3 MB | baseline | skill-uri | yes | — |
| jcode | 56.4 ms | 53.7 ms–74.6 ms | 1.09× more | 41.6 MB | 1.18× more | skill-tool | yes | — |
| Claude Code | 426.2 ms | 422.4 ms–587.6 ms | 8.23× more | 207.4 MB | 5.87× more | skill-tool | yes | — |
| Codex CLI | 574.6 ms | 486.6 ms–748.3 ms | 11.10× more | 192.6 MB | 5.45× more | file-read | yes | — |
| OpenCode | 3.03 s | 3.03 s–3.31 s | 58.60× more | 692.1 MB | 19.59× more | skill-tool | yes | — |
| pi | 488.8 ms | 484.3 ms–505.0 ms | 9.44× more | 125.0 MB | 3.54× more | file-read | yes | — |
| oh-my-pi (omp) | 1.77 s | 1.76 s–1.79 s | 34.11× more | 341.4 MB | 9.66× more | file-read | yes | — |
| oh-my-openagent (omo) | 6.71 s | 6.44 s–6.88 s | 129.61× more | 666.9 MB | 18.87× more | file-read | yes | — |

**MCP tool call** (process start to final answer, including starting the fixture stdio MCP server and one `bench_echo` call)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | MCP ready | Note |
|---|---|---|---|---|---|---|---|---|---|
| horon | 72.6 ms | 71.7 ms–76.9 ms | baseline | 49.4 MB | baseline | mcp-call | yes | 50.3 ms | — |
| jcode | 172.7 ms | 171.3 ms–174.9 ms | 2.38× more | 53.9 MB | 1.09× more | mcp-action | yes | 142.7 ms | — |
| Claude Code | 529.3 ms | 508.9 ms–537.4 ms | 7.29× more | 226.1 MB | 4.58× more | mcp-tool | yes | 387.4 ms | — |
| Codex CLI | 601.8 ms | 494.9 ms–651.0 ms | 8.29× more | 190.7 MB | 3.86× more | mcp-tool | yes | 426.3 ms | — |
| OpenCode | 2.86 s | 2.81 s–2.99 s | 39.45× more | 567.5 MB | 11.48× more | mcp-tool | yes | 2.57 s | — |
| pi | 531.7 ms | 524.6 ms–538.9 ms | 7.32× more | 140.6 MB | 2.84× more | mcp-tool | yes | 477.7 ms | — |
| oh-my-pi (omp) | 2.05 s | 2.03 s–2.07 s | 28.21× more | 393.2 MB | 7.96× more | mcp-route | yes | 1.69 s | — |
| oh-my-openagent (omo) | 6.89 s | 6.78 s–6.90 s | 94.86× more | 661.4 MB | 13.38× more | mcp-tool | yes | 3.49 s | — |

**Session memory** (idle after 3 turn(s) per session; sizes are process-tree sums, RSS; PSS also recorded on Linux). "Each additional" is the least-squares slope across the measured session counts; 0 B means growth stayed below RSS sampling noise. Threads and descriptors come from the daemon's own `server:memory` after the turns.

| Tool | First session | Each additional | vs horon (additional) | Threads/session | FDs/session | First (post-turn) | Each additional (post-turn) |
|---|---|---|---|---|---|---|---|
| horon | 36.2 MB | 169.4 KB | baseline | 0.92 | 1.00 | 43.1 MB | 481.0 KB |
| jcode | 43.2 MB | 26.9 KB | 6.29× less | 0.11 | — | 46.7 MB | 207.3 KB |
| Claude Code | 149.6 MB | 149.4 MB | 902.92× more | — | — | 184.6 MB | 180.9 MB |
| Codex CLI | 129.3 MB | 133.1 MB | 804.26× more | — | — | 161.7 MB | 164.7 MB |
| OpenCode | 243.8 MB | 1.0 MB | 6.05× more | — | — | 452.8 MB | 13.1 MB |
| pi | 107.6 MB | 108.4 MB | 655.37× more | — | — | 117.1 MB | 131.9 MB |
| oh-my-pi (omp) | 333.5 MB | 61.6 MB | 372.21× more | — | — | 363.3 MB | 357.1 MB |
| oh-my-openagent (omo) | 279.1 MB | 34.8 MB | 210.34× more | — | — | 616.1 MB | 588.1 MB |

Threads/descriptors unavailable: jcode (server:memory did not report fd_count)

**Post-turn tree RSS per session count**

| Tool | 1 session(s) | 5 session(s) | 10 session(s) |
|---|---|---|---|
| horon | 43.1 MB | 45.8 MB | 47.4 MB |
| jcode | 46.7 MB | 47.6 MB | 48.5 MB |
| Claude Code | 184.6 MB | 920.5 MB | 1.8 GB |
| Codex CLI | 161.7 MB | 813.8 MB | 1.6 GB |
| OpenCode | 452.8 MB | 495.4 MB | 570.2 MB |
| pi | 117.1 MB | 627.5 MB | 1.3 GB |
| oh-my-pi (omp) | 363.3 MB | 1.8 GB | 3.5 GB |
| oh-my-openagent (omo) | 616.1 MB | 2.9 GB | 5.8 GB |

#### 기동

```text
Benchmarking: /home/runner/work/agent/agent/bench-bin/horon-agent
============================================================

--help (binary load):
  Min:    4.61 ms
  Max:    4.98 ms
  Mean:   4.77 ms
  Median: 4.76 ms
  Stdev:  0.11 ms

--version:
  Min:    4.30 ms
  Max:    4.57 ms
  Mean:   4.47 ms
  Median: 4.50 ms
  Stdev:  0.10 ms

Measuring isolated server startup (10 runs)...

Server ready (isolated socket connectable):
  Min:    20.69 ms
  Max:    20.74 ms
  Mean:   20.71 ms
  Median: 20.71 ms
  Stdev:  0.02 ms

Measuring harness bridge readiness (10 runs)...

Bridge stage: api socket connectable:
  Min:    20.73 ms
  Max:    20.89 ms
  Mean:   20.77 ms
  Median: 20.76 ms
  Stdev:  0.05 ms

Bridge stage: hello_ok:
  Min:    21.35 ms
  Max:    21.65 ms
  Mean:   21.51 ms
  Median: 21.51 ms
  Stdev:  0.10 ms

Bridge stage: no-op reply:
  Min:    21.84 ms
  Max:    22.11 ms
  Mean:   21.99 ms
  Median: 22.00 ms
  Stdev:  0.09 ms

Bridge total (socket -> no-op):
  Min:    21.84 ms
  Max:    22.11 ms
  Mean:   21.99 ms
  Median: 22.00 ms
  Stdev:  0.09 ms

============================================================
Summary:
  Binary load median:      ~4.8 ms
  Server ready median:     ~20.7 ms
  Bridge total median:     ~22.0 ms
```

#### 세션 로딩 (COLD)

```text
binary   /home/runner/work/agent/agent/bench-bin/examples/loading_probe (38adf43b7e62)
mode     cold
protocol COLD: every measured run gets its own freshly copied HOME; no derived cache from an earlier run exists, so each sample is a first load of the fixture.
fixtures /tmp/horon-loading-bench-ou530cvo
note     --warmup 3 is ignored in cold mode: every sample gets a fresh HOME
fixture events=0 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    0.015/0.015    p95    0.017/0.016    delta  -0.000 ms  control-no-change
  skill_request_overlay p50    0.106/0.106    p95    0.107/0.141    delta  +0.000 ms  control-no-change
  mcp_config_load   p50    0.143/0.143    p95    0.176/0.160    delta  -0.000 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  session_save      p50    8.604/8.865    p95   11.319/10.491   delta  +0.261 ms  control-no-change
  total store_read_bytes p50 23076  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 23060..23084 (5 distinct values)
  SPREAD   b session_save store_read_bytes 23060..23080 (5 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    2.741/2.707    p95    2.823/2.782    delta  -0.034 ms  control-no-change
  skill_request_overlay p50    4.686/4.672    p95    5.023/4.712    delta  -0.015 ms  control-no-change
  mcp_config_load   p50    0.147/0.146    p95    0.161/0.154    delta  -0.001 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  session_save      p50   11.195/10.875   p95   12.449/18.238   delta  -0.319 ms  control-no-change
  total store_read_bytes p50 23072  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 23064..23084 (5 distinct values)
  SPREAD   b session_save store_read_bytes 23060..23076 (4 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.015/0.015    p95    0.016/0.015    delta  -0.000 ms  control-no-change
  skill_request_overlay p50    0.106/0.106    p95    0.106/0.106    delta  -0.000 ms  control-no-change
  mcp_config_load   p50    0.144/0.144    p95    0.186/0.185    delta  +0.000 ms  control-no-change
  attach_history    p50  295.130/293.480  p95  298.161/301.931  delta  -1.650 ms  control-no-change
  resume_ready      p50   53.160/53.252   p95   53.393/55.390   delta  +0.091 ms  control-no-change
  provider_dispatch p50    1.329/1.330    p95    1.343/1.669    delta  +0.000 ms  control-no-change
  session_save      p50    7.741/7.783    p95    8.157/9.121    delta  +0.041 ms  control-no-change
  total store_read_bytes p50 5311390  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189185..4189189 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099128..1099132 (2 distinct values)
  SPREAD   a session_save store_read_bytes 23065..23081 (5 distinct values)
  SPREAD   b attach_history store_read_bytes 4189183..4189189 (4 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099128..1099132 (4 distinct values)
  SPREAD   b session_save store_read_bytes 23061..23085 (10 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    2.722/2.702    p95    2.791/2.796    delta  -0.021 ms  control-no-change
  skill_request_overlay p50    4.709/4.735    p95    4.775/4.783    delta  +0.026 ms  control-no-change
  mcp_config_load   p50    0.147/0.147    p95    0.160/0.149    delta  -0.000 ms  control-no-change
  attach_history    p50  297.325/296.367  p95  303.250/300.368  delta  -0.958 ms  control-no-change
  resume_ready      p50   53.313/53.134   p95   54.189/53.747   delta  -0.179 ms  control-no-change
  provider_dispatch p50    1.344/1.340    p95    1.383/1.375    delta  -0.005 ms  control-no-change
  session_save      p50    7.722/7.707    p95    8.197/7.900    delta  -0.015 ms  control-no-change
  total store_read_bytes p50 5311392  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189187..4189191 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099129..1099133 (3 distinct values)
  SPREAD   a session_save store_read_bytes 23054..23082 (10 distinct values)
  SPREAD   b attach_history store_read_bytes 4189189..4189191 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099132..1099133 (2 distinct values)
  SPREAD   b session_save store_read_bytes 23066..23086 (5 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.016/0.015    p95    0.035/0.016    delta  -0.001 ms  control-no-change
  skill_request_overlay p50    0.106/0.105    p95    0.108/0.119    delta  -0.000 ms  control-no-change
  mcp_config_load   p50    0.143/0.143    p95    0.163/0.162    delta  -0.000 ms  control-no-change
  attach_history    p50 3761.642/3765.886 p95 3816.909/3840.970 delta  +4.244 ms  control-no-change
  resume_ready      p50  576.094/579.387  p95  586.687/581.664  delta  +3.293 ms  control-no-change
  provider_dispatch p50   14.552/14.830   p95   14.905/15.565   delta  +0.278 ms  control-no-change
  session_save      p50    7.999/8.028    p95    8.625/9.212    delta  +0.030 ms  control-no-change
  total store_read_bytes p50 53417609  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184281..42184289 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210235..11210243 (2 distinct values)
  SPREAD   a session_save store_read_bytes 23059..23085 (6 distinct values)
  SPREAD   b attach_history store_read_bytes 42184285..42184289 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210239..11210243 (3 distinct values)
  SPREAD   b session_save store_read_bytes 23060..23085 (6 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    2.707/2.705    p95    2.757/2.843    delta  -0.002 ms  control-no-change
  skill_request_overlay p50    4.672/4.668    p95    5.377/4.688    delta  -0.004 ms  control-no-change
  mcp_config_load   p50    0.146/0.147    p95    0.195/0.154    delta  +0.001 ms  control-no-change
  attach_history    p50 3748.727/3752.105 p95 3769.283/3774.423 delta  +3.378 ms  control-no-change
  resume_ready      p50  575.855/575.244  p95  581.434/579.066  delta  -0.611 ms  control-no-change
  provider_dispatch p50   14.705/14.892   p95   14.960/15.791   delta  +0.187 ms  control-no-change
  session_save      p50    7.991/8.047    p95    8.189/8.868    delta  +0.056 ms  control-no-change
  total store_read_bytes p50 53417611  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184285..42184289 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210239..11210243 (2 distinct values)
  SPREAD   a session_save store_read_bytes 23067..23089 (6 distinct values)
  SPREAD   b attach_history store_read_bytes 42184281..42184289 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210235..11210243 (3 distinct values)
  SPREAD   b session_save store_read_bytes 23063..23085 (6 distinct values)
report   bench-results/loading-cold.json
mode cold, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### 세션 로딩 (WARM)

```text
binary   /home/runner/work/agent/agent/bench-bin/examples/loading_probe (38adf43b7e62)
mode     warm
protocol WARM: one HOME per side, --warmup discarded runs per side, then measured runs alternate between the two sides on those same HOMEs.
fixtures /tmp/horon-loading-bench-jhr7ne20
fixture events=0 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    0.015/0.015    p95    0.034/0.017    delta  -0.000 ms  control-no-change
  skill_request_overlay p50    0.104/0.104    p95    0.105/0.105    delta  -0.000 ms  control-no-change
  mcp_config_load   p50    0.141/0.141    p95    0.158/0.173    delta  +0.000 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-drift
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  session_save      p50    7.704/7.699    p95    9.711/8.298    delta  -0.005 ms  control-no-change
  total store_read_bytes p50 23793  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23061, 23073, 23081, 23787, 23791, 23795, 24489, 24505, 24509]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23061, 23069, 23073, 23787, 23791, 23803, 24501, 24505, 24509, 24513]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23061..24509 (9 distinct values)
  SPREAD   b session_save store_read_bytes 23061..24513 (10 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    2.663/2.685    p95    2.704/2.765    delta  +0.021 ms  control-no-change
  skill_request_overlay p50    4.584/4.619    p95    4.636/4.680    delta  +0.036 ms  control-no-change
  mcp_config_load   p50    0.144/0.145    p95    0.151/0.156    delta  +0.001 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.001    delta  +0.000 ms  control-no-change
  session_save      p50    9.432/9.401    p95   10.883/9.747    delta  -0.030 ms  control-no-change
  total store_read_bytes p50 23792  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23069, 23073, 23075, 23077, 23789, 23795, 23797, 24503, 24505, 24513]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23057, 23065, 23081, 23781, 23783, 23795, 24497, 24499, 24505, 24513]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23069..24513 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23057..24513 (10 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.014/0.014    p95    0.016/0.017    delta  -0.000 ms  control-no-change
  skill_request_overlay p50    0.104/0.104    p95    0.148/0.105    delta  +0.000 ms  control-no-change
  mcp_config_load   p50    0.141/0.141    p95    0.172/0.152    delta  -0.001 ms  control-no-change
  attach_history    p50   96.333/96.112   p95   97.233/96.591   delta  -0.222 ms  control-no-change
  resume_ready      p50   52.208/52.159   p95   52.960/52.304   delta  -0.050 ms  control-no-change
  provider_dispatch p50    1.326/1.314    p95    1.343/1.334    delta  -0.012 ms  control-no-change
  session_save      p50    8.122/8.127    p95    9.338/11.459   delta  +0.005 ms  control-no-change
  total store_read_bytes p50 2841360  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23779, 23795, 24499, 24505, 25221, 25235, 25945, 25955, 26659, 26661]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23773, 23801, 24507, 24519, 25209, 25213, 25225, 25937, 25959, 26649]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23779..26661 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23773..26649 (10 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    2.735/2.709    p95    2.807/2.753    delta  -0.026 ms  control-no-change
  skill_request_overlay p50    4.650/4.621    p95    4.733/4.686    delta  -0.029 ms  control-no-change
  mcp_config_load   p50    0.145/0.145    p95    0.156/0.156    delta  -0.001 ms  control-no-change
  attach_history    p50   96.415/97.014   p95   98.342/100.464  delta  +0.599 ms  control-no-change
  resume_ready      p50   52.230/52.093   p95   52.658/52.521   delta  -0.137 ms  control-no-change
  provider_dispatch p50    1.321/1.318    p95    1.377/1.356    delta  -0.003 ms  control-no-change
  session_save      p50    9.010/9.150    p95    9.959/16.185   delta  +0.140 ms  control-no-change
  total store_read_bytes p50 2841364  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23782, 23788, 24498, 24504, 25230, 25928, 25940, 26666]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23769, 23795, 24503, 24509, 25221, 25229, 25951, 25955, 26649, 26661]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23782..26666 (8 distinct values)
  SPREAD   b session_save store_read_bytes 23769..26661 (10 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.015/0.015    p95    0.037/0.016    delta  -0.000 ms  control-no-change
  skill_request_overlay p50    0.104/0.104    p95    0.107/0.104    delta  -0.000 ms  control-no-change
  mcp_config_load   p50    0.140/0.140    p95    0.158/0.172    delta  -0.000 ms  control-no-change
  attach_history    p50 1059.256/1058.931 p95 1066.858/1066.415 delta  -0.326 ms  control-no-change
  resume_ready      p50  569.971/568.144  p95  578.455/577.230  delta  -1.827 ms  control-no-change
  provider_dispatch p50   14.346/14.460   p95   14.449/14.889   delta  +0.114 ms  control-no-change
  session_save      p50    7.767/7.825    p95    7.891/9.487    delta  +0.057 ms  control-no-change
  total store_read_bytes p50 28640385  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23786, 23798, 24514, 24516, 25234, 25238, 25952, 25956, 26666, 26678]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23772, 23794, 24508, 24514, 25214, 25226, 25942, 25952, 26668, 26678]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23786..26678 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23772..26678 (10 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    2.647/2.664    p95    2.764/2.717    delta  +0.017 ms  control-no-change
  skill_request_overlay p50    4.603/4.637    p95    4.799/4.709    delta  +0.034 ms  control-no-change
  mcp_config_load   p50    0.145/0.145    p95    0.151/0.156    delta  +0.001 ms  control-no-change
  attach_history    p50 1063.130/1065.083 p95 1081.297/1085.455 delta  +1.953 ms  control-no-change
  resume_ready      p50  571.914/574.140  p95  576.061/583.307  delta  +2.226 ms  control-no-change
  provider_dispatch p50   14.168/14.171   p95   14.551/14.680   delta  +0.003 ms  control-no-change
  session_save      p50    9.281/9.453    p95   10.255/10.225   delta  +0.172 ms  control-no-change
  total store_read_bytes p50 28640383  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23791, 24501, 24521, 25225, 25239, 25939, 25941, 26665, 26667]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23791, 23793, 24511, 24519, 25231, 25233, 25931, 25957, 26667, 26677]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23791..26667 (9 distinct values)
  SPREAD   b session_save store_read_bytes 23791..26677 (10 distinct values)
report   bench-results/loading-warm.json
mode warm, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### Tool latency

| Tool case | median | p95 | min | max | samples |
|---|---|---|---|---|---|
| read (tiny file) | 0.2 ms | 0.2 ms | 0.1 ms | 0.2 ms | 5 |
| read (1000 lines) | 0.3 ms | 0.4 ms | 0.3 ms | 0.4 ms | 5 |
| read (10 lines @ offset) | 0.3 ms | 0.3 ms | 0.2 ms | 0.3 ms | 5 |
| read (repo Cargo.toml) | 0.2 ms | 0.2 ms | 0.2 ms | 0.2 ms | 5 |
| write (small) | 1.0 ms | 1.1 ms | 0.9 ms | 1.1 ms | 5 |
| write (10KB) | 1.1 ms | 1.2 ms | 1.0 ms | 1.2 ms | 5 |
| edit (simple replace) | 1.0 ms | 1.0 ms | 1.0 ms | 1.0 ms | 5 |
| ls (scripts dir) | 0.5 ms | 0.7 ms | 0.4 ms | 0.7 ms | 5 |
| agentgrep (grep in tree) | 1.2 ms | 1.3 ms | 1.1 ms | 1.3 ms | 5 |
| agentgrep (find **/*.rs) | 1.0 ms | 1.0 ms | 0.9 ms | 1.0 ms | 5 |
| bash (true) | 1.5 ms | 1.5 ms | 1.5 ms | 1.5 ms | 5 |
| bash (echo hello) | 1.5 ms | 1.6 ms | 1.5 ms | 1.6 ms | 5 |
| bash (ls fixture tree) | 2.6 ms | 2.7 ms | 2.6 ms | 2.7 ms | 5 |
| invalid tool (dispatch only) | 0.1 ms | 0.1 ms | 0.1 ms | 0.1 ms | 5 |

#### Reload storm

1 resume(s) per level; each level resumes sessions no other level has opened, from a freshly started daemon.

| Concurrent resumes | history median | p95 | max | storm wall | failures |
|---|---|---|---|---|---|
| 1 | 41.2 ms | 41.2 ms | 41.2 ms | 41.3 ms | 0 |
| 10 | 131.1 ms | 192.8 ms | 192.8 ms | 193.1 ms | 0 |
| 40 | 619.7 ms | 663.0 ms | 663.9 ms | 664.3 ms | 0 |

#### Compile loop

| Checkpoint | Touched | median | min | max | samples |
|---|---|---|---|---|---|
| warm_noop | — | 228.8 ms | 228.8 ms | 228.8 ms | 1 |
| leaf_touch | src/main.rs | 353.5 ms | 353.5 ms | 353.5 ms | 1 |
| core_touch | crates/app-core/src/lib.rs | 17.11 s | 17.11 s | 17.11 s | 1 |
| build_meta_touch | crates/build-meta/src/lib.rs | 23.72 s | 23.72 s | 23.72 s | 1 |

### macOS

#### Coding agent comparison (headless, mock model)

Host: darwin/arm64, 3 CPUs, Python 3.12.10. Baseline: **horon**.

Skipped: syscalls (strace not available)

| Tool | Version | `--version` (median) | Install size |
|---|---|---|---|
| horon | horon-agent v0.1.0-dev (7d0726c) | 14.0 ms | 92.9 MB |
| jcode | jcode v0.99.1 (7206efd0c) | 14.0 ms | 136.3 MB |
| Claude Code | 2.1.317 (Claude Code) | 28.9 ms | 596.8 MB |
| Codex CLI | codex-cli 0.166.0 | 81.9 ms | 346.5 MB |
| OpenCode | 1.19.5 | 533.8 ms | 344.6 MB |
| pi | 1.5.0 | 333.7 ms | 108.9 MB |
| oh-my-pi (omp) | omp/18.18.0 | 778.8 ms | 273.0 MB |
| oh-my-openagent (omo) | omo 5.1.64 (engine: senpi 2026.11.5-1) | 221.8 ms | 502.2 MB |

**One-shot headless prompt** (includes startup, one `ls` tool call, final answer)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Tool round-trip | Note |
|---|---|---|---|---|---|---|---|
| horon | 124.7 ms | 105.7 ms–568.6 ms | baseline | 28.0 MB | baseline | yes | — |
| jcode | 461.9 ms | 385.7 ms–713.5 ms | 3.70× more | 34.6 MB | 1.23× more | yes | — |
| Claude Code | 779.2 ms | 677.2 ms–2.62 s | 6.25× more | 192.5 MB | 6.87× more | yes | — |
| Codex CLI | 500.4 ms | 455.6 ms–1.49 s | 4.01× more | 143.2 MB | 5.11× more | yes | — |
| OpenCode | 4.28 s | 3.25 s–6.84 s | 34.35× more | 781.6 MB | 27.88× more | yes | — |
| pi | 592.9 ms | 565.7 ms–625.5 ms | 4.76× more | 118.8 MB | 4.24× more | yes | — |
| oh-my-pi (omp) | 2.24 s | 2.22 s–2.69 s | 18.00× more | 407.6 MB | 14.54× more | yes | — |
| oh-my-openagent (omo) | 10.91 s | 10.54 s–11.31 s | 87.47× more | 863.5 MB | 30.81× more | yes | — |

**Skill invocation** (process start to final answer, including skill discovery and one load of the fixture skill)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | Note |
|---|---|---|---|---|---|---|---|---|
| horon | 110.9 ms | 93.3 ms–156.1 ms | baseline | 22.5 MB | baseline | skill-uri | yes | — |
| jcode | 385.8 ms | 332.3 ms–574.0 ms | 3.48× more | 27.5 MB | 1.22× more | skill-tool | yes | — |
| Claude Code | 513.4 ms | 498.4 ms–555.7 ms | 4.63× more | 194.2 MB | 8.65× more | skill-tool | yes | — |
| Codex CLI | 551.6 ms | 497.8 ms–606.4 ms | 4.97× more | 142.9 MB | 6.36× more | file-read | yes | — |
| OpenCode | 4.02 s | 3.67 s–4.24 s | 36.28× more | 678.5 MB | 30.20× more | skill-tool | yes | — |
| pi | 718.9 ms | 710.3 ms–766.4 ms | 6.48× more | 132.2 MB | 5.89× more | file-read | yes | — |
| oh-my-pi (omp) | 2.34 s | 2.14 s–3.39 s | 21.07× more | 413.2 MB | 18.39× more | file-read | yes | — |
| oh-my-openagent (omo) | 10.38 s | 10.23 s–14.52 s | 93.56× more | 896.9 MB | 39.92× more | file-read | yes | — |

**MCP tool call** (process start to final answer, including starting the fixture stdio MCP server and one `bench_echo` call)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | MCP ready | Note |
|---|---|---|---|---|---|---|---|---|---|
| horon | 166.3 ms | 148.5 ms–215.2 ms | baseline | 31.2 MB | baseline | mcp-call | yes | 128.6 ms | — |
| jcode | 479.3 ms | 465.5 ms–514.6 ms | 2.88× more | 43.5 MB | 1.39× more | mcp-action | yes | 442.4 ms | — |
| Claude Code | 707.0 ms | 649.0 ms–773.6 ms | 4.25× more | 221.2 MB | 7.09× more | mcp-tool | yes | 554.2 ms | — |
| Codex CLI | 474.2 ms | 456.6 ms–520.1 ms | 2.85× more | 142.9 MB | 4.58× more | mcp-tool | yes | 300.5 ms | — |
| OpenCode | 4.05 s | 3.66 s–4.30 s | 24.34× more | 709.6 MB | 22.73× more | mcp-tool | yes | 3.29 s | — |
| pi | 749.8 ms | 698.9 ms–864.0 ms | 4.51× more | 147.6 MB | 4.73× more | mcp-tool | yes | 608.6 ms | — |
| oh-my-pi (omp) | 2.52 s | 2.30 s–2.69 s | 15.15× more | 408.3 MB | 13.08× more | mcp-route | yes | 2.17 s | — |
| oh-my-openagent (omo) | — | — | — | — | — | — | no | — | failed: no tool that could reach the fixture was offered (one request, no tool call) |

**Session memory** (idle after 3 turn(s) per session; sizes are process-tree sums, RSS; PSS also recorded on Linux). "Each additional" is the least-squares slope across the measured session counts; 0 B means growth stayed below RSS sampling noise. Threads and descriptors come from the daemon's own `server:memory` after the turns.

| Tool | First session | Each additional | vs horon (additional) | Threads/session | FDs/session | First (post-turn) | Each additional (post-turn) |
|---|---|---|---|---|---|---|---|
| horon | 28.5 MB | 155.1 KB | baseline | 0.45 | 1.00 | 34.2 MB | 388.1 KB |
| jcode | 36.5 MB | 190.0 KB | 1.22× more | — | — | 40.1 MB | 324.9 KB |
| Claude Code | 157.5 MB | 151.8 MB | 1,002× more | — | — | 187.3 MB | 183.2 MB |
| Codex CLI | 113.4 MB | 82.8 MB | 546.24× more | — | — | 106.9 MB | 107.4 MB |
| OpenCode | 262.4 MB | 456.4 KB | 2.94× more | — | — | 575.6 MB | 11.6 MB |
| pi | 119.0 MB | 115.5 MB | 762.02× more | — | — | 148.5 MB | 148.0 MB |
| oh-my-pi (omp) | 369.9 MB | 93.9 MB | 619.62× more | — | — | 435.0 MB | 317.8 MB |
| oh-my-openagent (omo) | 57.6 MB | 41.6 MB | 274.49× more | — | — | 812.5 MB | 315.9 MB |

Threads/descriptors unavailable: jcode (server:memory did not report thread_count, fd_count)

**Post-turn tree RSS per session count**

| Tool | 1 session(s) | 5 session(s) | 10 session(s) |
|---|---|---|---|
| horon | 34.2 MB | 35.9 MB | 37.7 MB |
| jcode | 40.1 MB | 41.4 MB | 43.0 MB |
| Claude Code | 187.3 MB | 927.5 MB | 1.8 GB |
| Codex CLI | 106.9 MB | 534.2 MB | 1.0 GB |
| OpenCode | 575.6 MB | 599.3 MB | 678.1 MB |
| pi | 148.5 MB | 740.8 MB | 1.4 GB |
| oh-my-pi (omp) | 435.0 MB | 1.9 GB | 3.2 GB |
| oh-my-openagent (omo) | 812.5 MB | 3.1 GB | 3.6 GB |

#### 기동

```text
Benchmarking: /Users/runner/work/agent/agent/bench-bin/horon-agent
============================================================

--help (binary load):
  Min:    8.65 ms
  Max:    16.71 ms
  Mean:   11.31 ms
  Median: 10.44 ms
  Stdev:  2.59 ms

--version:
  Min:    7.99 ms
  Max:    12.16 ms
  Mean:   9.11 ms
  Median: 8.58 ms
  Stdev:  1.30 ms

Measuring isolated server startup (10 runs)...

Server ready (isolated socket connectable):
  Min:    24.97 ms
  Max:    100.15 ms
  Mean:   58.84 ms
  Median: 65.20 ms
  Stdev:  27.67 ms

Measuring harness bridge readiness (10 runs)...

Bridge stage: api socket connectable:
  Min:    34.98 ms
  Max:    84.15 ms
  Mean:   55.64 ms
  Median: 52.51 ms
  Stdev:  16.92 ms

Bridge stage: hello_ok:
  Min:    35.99 ms
  Max:    85.35 ms
  Mean:   56.81 ms
  Median: 53.63 ms
  Stdev:  17.18 ms

Bridge stage: no-op reply:
  Min:    36.69 ms
  Max:    85.90 ms
  Mean:   57.49 ms
  Median: 54.53 ms
  Stdev:  17.22 ms

Bridge total (socket -> no-op):
  Min:    36.69 ms
  Max:    85.90 ms
  Mean:   57.49 ms
  Median: 54.53 ms
  Stdev:  17.22 ms

============================================================
Summary:
  Binary load median:      ~10.4 ms
  Server ready median:     ~65.2 ms
  Bridge total median:     ~54.5 ms
```

#### 세션 로딩 (COLD)

```text
binary   /Users/runner/work/agent/agent/bench-bin/examples/loading_probe (21a2eee196a1)
mode     cold
protocol COLD: every measured run gets its own freshly copied HOME; no derived cache from an earlier run exists, so each sample is a first load of the fixture.
fixtures /var/folders/h5/t85nd63d71x27_8gl5gw3l_w0000gn/T/horon-loading-bench-m1mqsf1w
note     --warmup 3 is ignored in cold mode: every sample gets a fresh HOME
fixture events=0 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    0.026/0.025    p95    0.098/0.030    delta  -0.001 ms  control-no-change
  skill_request_overlay p50    0.248/0.230    p95    0.445/0.440    delta  -0.018 ms  control-no-change
  mcp_config_load   p50    0.273/0.261    p95    0.460/0.520    delta  -0.012 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.001    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.001    delta  +0.000 ms  control-drift
  provider_dispatch p50    0.000/0.000    p95    0.000/0.001    delta  +0.000 ms  control-no-change
  session_save      p50   20.905/20.853   p95   38.790/35.210   delta  -0.052 ms  control-no-change
  total store_read_bytes p50 22944  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 22934..22958 (7 distinct values)
  SPREAD   b session_save store_read_bytes 22938..22954 (5 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    4.567/4.384    p95    5.872/5.094    delta  -0.182 ms  control-no-change
  skill_request_overlay p50    5.611/5.620    p95    7.288/6.732    delta  +0.008 ms  control-no-change
  mcp_config_load   p50    0.273/0.291    p95    0.546/0.444    delta  +0.019 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.001/0.000    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.001/0.000    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.001/0.000    delta  -0.000 ms  control-no-change
  session_save      p50   20.786/20.785   p95  150.313/23.829   delta  -0.001 ms  control-no-change
  total store_read_bytes p50 22950  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 22938..22962 (6 distinct values)
  SPREAD   b session_save store_read_bytes 22930..22966 (5 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.031/0.031    p95    0.191/0.060    delta  -0.000 ms  control-no-change
  skill_request_overlay p50    0.249/0.280    p95    0.744/0.510    delta  +0.031 ms  control-no-change
  mcp_config_load   p50    0.276/0.384    p95    0.356/0.730    delta  +0.108 ms  control-drift
  attach_history    p50  414.603/395.407  p95  526.314/538.517  delta -19.196 ms  control-no-change
  resume_ready      p50   89.099/78.907   p95  144.675/128.004  delta -10.192 ms  control-no-change
  provider_dispatch p50    1.260/1.249    p95    3.395/2.894    delta  -0.012 ms  control-no-change
  session_save      p50   22.547/22.727   p95   80.807/36.025   delta  +0.180 ms  control-no-change
  total store_read_bytes p50 5311298  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189201..4189205 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099142..1099144 (3 distinct values)
  SPREAD   a session_save store_read_bytes 22939..22965 (10 distinct values)
  SPREAD   b attach_history store_read_bytes 4189201..4189205 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099140..1099144 (3 distinct values)
  SPREAD   b session_save store_read_bytes 22947..22970 (7 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    5.218/5.922    p95    8.679/10.125   delta  +0.705 ms  control-no-change
  skill_request_overlay p50    8.021/8.209    p95   13.065/13.036   delta  +0.188 ms  control-no-change
  mcp_config_load   p50    0.299/0.308    p95    0.706/0.731    delta  +0.009 ms  control-no-change
  attach_history    p50  421.469/415.380  p95  525.872/481.927  delta  -6.090 ms  control-no-change
  resume_ready      p50   85.487/91.337   p95  116.152/108.471  delta  +5.850 ms  control-no-change
  provider_dispatch p50    1.042/1.035    p95    2.663/1.965    delta  -0.007 ms  control-no-change
  session_save      p50   33.615/30.313   p95   60.270/40.760   delta  -3.302 ms  control-no-change
  total store_read_bytes p50 5311308  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189203..4189207 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099141..1099145 (3 distinct values)
  SPREAD   a session_save store_read_bytes 22952..22964 (5 distinct values)
  SPREAD   b attach_history store_read_bytes 4189202..4189207 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099140..1099145 (3 distinct values)
  SPREAD   b session_save store_read_bytes 22944..22968 (5 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.031/0.034    p95    0.075/0.085    delta  +0.003 ms  control-no-change
  skill_request_overlay p50    0.237/0.247    p95    0.579/0.562    delta  +0.010 ms  control-no-change
  mcp_config_load   p50    0.271/0.296    p95    0.673/1.233    delta  +0.025 ms  control-no-change
  attach_history    p50 4419.265/4029.742 p95 4896.619/4696.117 delta -389.524 ms  control-no-change
  resume_ready      p50  976.880/807.793  p95 1154.583/1083.100 delta -169.088 ms  control-no-change
  provider_dispatch p50   13.986/13.020   p95   20.720/19.375   delta  -0.966 ms  control-no-change
  session_save      p50   29.915/26.338   p95   48.788/89.898   delta  -3.577 ms  control-no-change
  total store_read_bytes p50 53417517  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184303..42184305 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210254..11210255 (2 distinct values)
  SPREAD   a session_save store_read_bytes 22947..22967 (6 distinct values)
  SPREAD   b attach_history store_read_bytes 42184301..42184305 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210251..11210255 (2 distinct values)
  SPREAD   b session_save store_read_bytes 22943..22963 (6 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    4.431/4.445    p95    9.833/5.730    delta  +0.014 ms  control-no-change
  skill_request_overlay p50    6.324/6.931    p95    9.009/10.016   delta  +0.607 ms  control-no-change
  mcp_config_load   p50    0.309/0.284    p95    0.349/0.711    delta  -0.025 ms  control-no-change
  attach_history    p50 4063.759/3829.454 p95 5229.736/5541.719 delta -234.306 ms  control-no-change
  resume_ready      p50  802.250/926.952  p95 1041.907/1139.913 delta +124.702 ms  control-no-change
  provider_dispatch p50   12.387/14.359   p95   17.812/21.631   delta  +1.973 ms  control-no-change
  session_save      p50   23.651/32.046   p95   54.690/56.621   delta  +8.395 ms  control-no-change
  total store_read_bytes p50 53417521  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184297..42184305 (4 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210247..11210255 (4 distinct values)
  SPREAD   a session_save store_read_bytes 22951..22975 (8 distinct values)
  SPREAD   b attach_history store_read_bytes 42184299..42184305 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210250..11210255 (3 distinct values)
  SPREAD   b session_save store_read_bytes 22951..22966 (5 distinct values)
report   bench-results/loading-cold.json
mode cold, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### 세션 로딩 (WARM)

```text
binary   /Users/runner/work/agent/agent/bench-bin/examples/loading_probe (21a2eee196a1)
mode     warm
protocol WARM: one HOME per side, --warmup discarded runs per side, then measured runs alternate between the two sides on those same HOMEs.
fixtures /var/folders/h5/t85nd63d71x27_8gl5gw3l_w0000gn/T/horon-loading-bench-120n472e
fixture events=0 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    0.030/0.031    p95    0.171/0.100    delta  +0.001 ms  control-no-change
  skill_request_overlay p50    0.252/0.263    p95    1.198/0.887    delta  +0.011 ms  control-no-change
  mcp_config_load   p50    0.267/0.267    p95    0.852/0.822    delta  +0.000 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.001/0.003    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  session_save      p50   26.595/27.208   p95   45.429/52.093   delta  +0.613 ms  control-no-change
  total store_read_bytes p50 23659  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [22929, 22939, 22959, 23643, 23657, 23661, 24367, 24373, 24375, 24379]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [22945, 22947, 22955, 23651, 23653, 23657, 24365, 24369, 24371, 24375]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 22929..24379 (10 distinct values)
  SPREAD   b session_save store_read_bytes 22945..24375 (10 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    6.144/5.394    p95   10.113/8.598    delta  -0.749 ms  control-no-change
  skill_request_overlay p50    7.810/7.286    p95   11.607/11.219   delta  -0.524 ms  control-no-change
  mcp_config_load   p50    0.374/0.341    p95    0.743/0.731    delta  -0.033 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.001    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.001    delta  +0.000 ms  control-no-change
  session_save      p50   22.529/23.864   p95   24.198/52.258   delta  +1.335 ms  control-no-change
  total store_read_bytes p50 23660  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [22941, 22951, 22955, 23659, 23661, 23667, 24371, 24373, 24375]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [22943, 22947, 23657, 23673, 24373, 24375, 24379, 24383]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 22941..24375 (9 distinct values)
  SPREAD   b session_save store_read_bytes 22943..24383 (8 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.030/0.032    p95    0.109/0.124    delta  +0.002 ms  control-no-change
  skill_request_overlay p50    0.311/0.245    p95    0.847/0.621    delta  -0.066 ms  control-no-change
  mcp_config_load   p50    0.309/0.423    p95    0.903/0.761    delta  +0.114 ms  control-no-change
  attach_history    p50  157.085/151.632  p95  193.768/169.966  delta  -5.453 ms  control-no-change
  resume_ready      p50   95.772/102.007  p95  129.133/109.834  delta  +6.235 ms  control-no-change
  provider_dispatch p50    1.624/1.187    p95    2.465/3.556    delta  -0.438 ms  control-no-change
  session_save      p50   21.382/23.814   p95   82.984/46.612   delta  +2.432 ms  control-no-change
  total store_read_bytes p50 2841252  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23668, 23670, 24374, 24388, 25092, 25096, 25794, 25806, 26524, 26536]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23648, 23682, 24382, 24388, 25102, 25110, 25808, 25812, 26522, 26526]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23668..26536 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23648..26526 (10 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    4.675/5.184    p95    5.571/6.316    delta  +0.509 ms  control-no-change
  skill_request_overlay p50    6.007/6.548    p95    8.490/8.053    delta  +0.541 ms  control-no-change
  mcp_config_load   p50    0.285/0.302    p95    0.325/0.435    delta  +0.017 ms  control-no-change
  attach_history    p50  116.520/122.846  p95  269.779/136.930  delta  +6.325 ms  control-no-change
  resume_ready      p50   73.821/65.162   p95   90.779/82.493   delta  -8.659 ms  control-no-change
  provider_dispatch p50    1.185/1.042    p95    2.083/1.531    delta  -0.144 ms  control-no-change
  session_save      p50   21.278/20.559   p95   28.706/27.974   delta  -0.719 ms  control-no-change
  total store_read_bytes p50 2841253  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23665, 24375, 25093, 25101, 25801, 25815, 26523, 26525]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23667, 23671, 24371, 24385, 25099, 25105, 25803, 25805, 26515, 26521]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23665..26525 (8 distinct values)
  SPREAD   b session_save store_read_bytes 23667..26521 (10 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.032/0.029    p95    0.078/0.048    delta  -0.002 ms  control-no-change
  skill_request_overlay p50    0.236/0.237    p95    0.485/0.483    delta  +0.002 ms  control-no-change
  mcp_config_load   p50    0.260/0.267    p95    0.571/0.630    delta  +0.007 ms  control-no-change
  attach_history    p50 1332.204/1497.659 p95 1893.158/1991.260 delta +165.455 ms  control-no-change
  resume_ready      p50  853.466/923.522  p95  995.550/1035.089 delta +70.057 ms  control-no-change
  provider_dispatch p50   15.561/12.294   p95   18.628/17.552   delta  -3.267 ms  control-no-change
  session_save      p50   24.425/29.658   p95   86.839/104.750  delta  +5.233 ms  control-no-change
  total store_read_bytes p50 28640279  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23673, 23681, 24375, 24389, 25095, 25113, 25789, 25817, 26519, 26527]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23677, 23681, 24385, 24395, 25099, 25103, 25809, 25821, 26515, 26527]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23673..26527 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23677..26527 (10 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    4.820/4.760    p95    7.928/8.378    delta  -0.060 ms  control-no-change
  skill_request_overlay p50    6.682/5.989    p95    9.995/8.883    delta  -0.693 ms  control-no-change
  mcp_config_load   p50    0.288/0.265    p95    0.486/0.439    delta  -0.022 ms  control-no-change
  attach_history    p50 1271.835/1243.592 p95 1429.288/1619.288 delta -28.242 ms  control-no-change
  resume_ready      p50  729.483/721.796  p95  827.481/850.027  delta  -7.688 ms  control-no-change
  provider_dispatch p50   10.538/11.176   p95   14.000/15.336   delta  +0.639 ms  control-no-change
  session_save      p50   21.156/21.492   p95   25.695/32.783   delta  +0.335 ms  control-no-change
  total store_read_bytes p50 28640269  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23669, 23673, 24385, 24387, 25087, 25101, 25811, 25829, 26529, 26531]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23665, 23673, 24391, 24395, 25099, 25101, 25803, 25809, 26519, 26521]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23669..26531 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23665..26521 (10 distinct values)
report   bench-results/loading-warm.json
mode warm, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### Tool latency

| Tool case | median | p95 | min | max | samples |
|---|---|---|---|---|---|
| read (tiny file) | 0.3 ms | 0.3 ms | 0.2 ms | 0.3 ms | 5 |
| read (1000 lines) | 0.5 ms | 0.6 ms | 0.5 ms | 0.6 ms | 5 |
| read (10 lines @ offset) | 0.5 ms | 0.5 ms | 0.5 ms | 0.5 ms | 5 |
| read (repo Cargo.toml) | 0.3 ms | 0.3 ms | 0.2 ms | 0.3 ms | 5 |
| write (small) | 1.7 ms | 1.9 ms | 1.5 ms | 1.9 ms | 5 |
| write (10KB) | 1.9 ms | 2.0 ms | 1.9 ms | 2.0 ms | 5 |
| edit (simple replace) | 1.5 ms | 1.6 ms | 1.5 ms | 1.6 ms | 5 |
| ls (scripts dir) | 0.7 ms | 0.7 ms | 0.7 ms | 0.7 ms | 5 |
| agentgrep (grep in tree) | 2.4 ms | 2.4 ms | 2.2 ms | 2.4 ms | 5 |
| agentgrep (find **/*.rs) | 1.3 ms | 1.3 ms | 1.3 ms | 1.3 ms | 5 |
| bash (true) | 3.9 ms | 4.2 ms | 3.7 ms | 4.2 ms | 5 |
| bash (echo hello) | 3.9 ms | 4.2 ms | 3.7 ms | 4.2 ms | 5 |
| bash (ls fixture tree) | 7.0 ms | 7.5 ms | 6.5 ms | 7.5 ms | 5 |
| invalid tool (dispatch only) | 0.1 ms | 0.1 ms | 0.1 ms | 0.1 ms | 5 |

#### Reload storm

1 resume(s) per level; each level resumes sessions no other level has opened, from a freshly started daemon.

| Concurrent resumes | history median | p95 | max | storm wall | failures |
|---|---|---|---|---|---|
| 1 | 70.7 ms | 70.7 ms | 70.7 ms | 70.9 ms | 0 |
| 10 | 330.2 ms | 335.5 ms | 335.5 ms | 336.1 ms | 0 |
| 40 | 1.33 s | 1.34 s | 1.34 s | 1.34 s | 0 |

#### Compile loop

| Checkpoint | Touched | median | min | max | samples |
|---|---|---|---|---|---|
| warm_noop | — | 3.68 s | 3.68 s | 3.68 s | 1 |
| leaf_touch | src/main.rs | 1.01 s | 1.01 s | 1.01 s | 1 |
| core_touch | crates/app-core/src/lib.rs | 31.10 s | 31.10 s | 31.10 s | 1 |
| build_meta_touch | crates/build-meta/src/lib.rs | 37.58 s | 37.58 s | 37.58 s | 1 |

### Windows

#### Coding agent comparison (headless, mock model)

Host: windows/AMD64, 4 CPUs, Python 3.12.10. Baseline: **horon**.

Skipped: syscalls (strace not available)

| Tool | Version | `--version` (median) | Install size |
|---|---|---|---|
| horon | horon-agent v0.1.0-dev (7d0726c) | 16.5 ms | 92.6 MB |
| jcode | jcode v0.99.1 (7206efd0c) | 18.8 ms | 128.4 MB |
| Claude Code | 2.1.317 (Claude Code) | 51.0 ms | 645.2 MB |
| Codex CLI | codex-cli 0.166.0 | 113.7 ms | 479.4 MB |
| OpenCode | 1.19.5 | 612.2 ms | 605.3 MB |
| pi | 1.5.0 | 327.4 ms | 108.9 MB |
| oh-my-pi (omp) | omp/18.18.0 | 792.9 ms | 291.2 MB |
| oh-my-openagent (omo) | omo 5.1.64 (engine: senpi 2026.11.5-1) | 218.4 ms | 514.8 MB |

**One-shot headless prompt** (includes startup, one `ls` tool call, final answer)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Tool round-trip | Note |
|---|---|---|---|---|---|---|---|
| horon | 289.5 ms | 241.2 ms–481.0 ms | baseline | 35.8 MB | baseline | yes | — |
| jcode | 184.1 ms | 160.5 ms–228.7 ms | 1.57× less | 22.8 MB | 1.57× less | yes | — |
| Claude Code | 1.56 s | 1.53 s–2.25 s | 5.38× more | 269.6 MB | 7.52× more | yes | — |
| Codex CLI | 1.56 s | 1.54 s–2.06 s | 5.39× more | 267.2 MB | 7.46× more | yes | — |
| OpenCode | 5.99 s | 5.74 s–9.85 s | 20.69× more | 772.9 MB | 21.56× more | yes | — |
| pi | 596.9 ms | 572.2 ms–623.4 ms | 2.06× more | 120.1 MB | 3.35× more | yes | — |
| oh-my-pi (omp) | 3.30 s | 2.97 s–3.51 s | 11.41× more | 402.7 MB | 11.23× more | yes | — |
| oh-my-openagent (omo) | 24.60 s | 20.84 s–36.40 s | 84.97× more | 545.6 MB | 15.22× more | yes | — |

**Skill invocation** (process start to final answer, including skill discovery and one load of the fixture skill)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | Note |
|---|---|---|---|---|---|---|---|---|
| horon | 246.3 ms | 239.6 ms–254.3 ms | baseline | 24.8 MB | baseline | skill-uri | yes | — |
| jcode | 236.8 ms | 204.7 ms–247.5 ms | 1.04× less | 22.8 MB | 1.09× less | skill-tool | yes | — |
| Claude Code | 1.11 s | 1.09 s–1.12 s | 4.52× more | 238.1 MB | 9.59× more | skill-tool | yes | — |
| Codex CLI | 1.98 s | 1.88 s–1.98 s | 8.03× more | 265.5 MB | 10.69× more | file-read | yes | — |
| OpenCode | 9.29 s | 8.85 s–9.35 s | 37.73× more | 768.1 MB | 30.94× more | skill-tool | yes | — |
| pi | 747.7 ms | 732.5 ms–750.0 ms | 3.04× more | 97.8 MB | 3.94× more | file-read | yes | — |
| oh-my-pi (omp) | 4.01 s | 3.89 s–4.19 s | 16.28× more | 404.4 MB | 16.29× more | file-read | yes | — |
| oh-my-openagent (omo) | 21.44 s | 20.20 s–23.28 s | 87.05× more | 519.9 MB | 20.94× more | file-read | yes | — |

**MCP tool call** (process start to final answer, including starting the fixture stdio MCP server and one `bench_echo` call)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | MCP ready | Note |
|---|---|---|---|---|---|---|---|---|---|
| horon | 278.4 ms | 277.6 ms–287.5 ms | baseline | 37.0 MB | baseline | mcp-call | yes | 165.1 ms | — |
| jcode | 357.4 ms | 317.0 ms–373.8 ms | 1.28× more | 37.1 MB | same | mcp-action | yes | 236.4 ms | — |
| Claude Code | 1.17 s | 1.17 s–1.19 s | 4.20× more | 277.4 MB | 7.51× more | mcp-tool | yes | 992.5 ms | — |
| Codex CLI | 972.0 ms | 871.9 ms–1.94 s | 3.49× more | 179.9 MB | 4.87× more | mcp-tool | yes | 701.7 ms | — |
| OpenCode | 8.88 s | 8.84 s–9.55 s | 31.91× more | 755.3 MB | 20.44× more | mcp-tool | yes | 7.11 s | — |
| pi | 828.7 ms | 811.7 ms–829.5 ms | 2.98× more | 118.5 MB | 3.21× more | mcp-tool | yes | 680.6 ms | — |
| oh-my-pi (omp) | 4.07 s | 3.81 s–4.24 s | 14.62× more | 404.1 MB | 10.93× more | mcp-route | yes | 3.67 s | — |
| oh-my-openagent (omo) | — | — | — | — | — | mcp-tool | no | — | failed: the fixture sentinel never came back |

**Session memory** (idle after 3 turn(s) per session; sizes are process-tree sums, RSS; PSS also recorded on Linux). "Each additional" is the least-squares slope across the measured session counts; 0 B means growth stayed below RSS sampling noise. Threads and descriptors come from the daemon's own `server:memory` after the turns.

| Tool | First session | Each additional | vs horon (additional) | Threads/session | FDs/session | First (post-turn) | Each additional (post-turn) |
|---|---|---|---|---|---|---|---|
| horon | 22.5 MB | 159.3 KB | baseline | — | — | 29.3 MB | 388.9 KB |
| jcode | 22.0 MB | 90.7 KB | 1.76× less | — | — | 26.4 MB | 412.9 KB |
| Claude Code | 154.7 MB | 151.9 MB | 976.19× more | — | — | 188.0 MB | 190.8 MB |
| Codex CLI | 124.6 MB | 96.9 MB | 623.04× more | — | — | 165.2 MB | 116.7 MB |
| OpenCode | 245.0 MB | 17.6 MB | 112.87× more | — | — | 428.5 MB | 25.6 MB |
| pi | 90.6 MB | 90.8 MB | 583.59× more | — | — | 100.8 MB | 100.9 MB |
| oh-my-pi (omp) | 303.7 MB | 107.3 MB | 689.62× more | — | — | 337.4 MB | 335.9 MB |
| oh-my-openagent (omo) | 142.2 MB | 131.9 MB | 847.89× more | — | — | 410.4 MB | 458.4 MB |

Threads/descriptors unavailable: horon (server:memory did not report thread_count, fd_count), jcode (server:memory did not report thread_count, fd_count)

**Post-turn tree RSS per session count**

| Tool | 1 session(s) | 5 session(s) | 10 session(s) |
|---|---|---|---|
| horon | 29.3 MB | 32.2 MB | 32.8 MB |
| jcode | 26.4 MB | 28.7 MB | 30.1 MB |
| Claude Code | 188.0 MB | 939.7 MB | 1.9 GB |
| Codex CLI | 165.2 MB | 609.7 MB | 1.2 GB |
| OpenCode | 428.5 MB | 449.0 MB | 652.7 MB |
| pi | 100.8 MB | 505.3 MB | 1009.2 MB |
| oh-my-pi (omp) | 337.4 MB | 1.6 GB | 3.3 GB |
| oh-my-openagent (omo) | 410.4 MB | 2.2 GB | 3.9 GB |

**Errors recorded during measurement**

- oh-my-openagent (omo) @ 10 sessions: turn 3 session 6: Connection error.
- oh-my-openagent (omo) @ 10 sessions: turn 3 session 8: Connection error.

#### 기동

```text
Benchmarking: D:/a/agent/agent/bench-bin/horon-agent.exe
============================================================

--help (binary load):
  Min:    14.46 ms
  Max:    15.31 ms
  Mean:   14.81 ms
  Median: 14.71 ms
  Stdev:  0.28 ms

--version:
  Min:    13.94 ms
  Max:    14.76 ms
  Mean:   14.27 ms
  Median: 14.20 ms
  Stdev:  0.25 ms

Measuring isolated server startup (10 runs)...

Server ready (isolated socket connectable):
  Min:    22.71 ms
  Max:    45.40 ms
  Mean:   27.32 ms
  Median: 23.09 ms
  Stdev:  7.68 ms

Measuring harness bridge readiness (10 runs)...
  skipped: api-bridge is Unix-only, so Windows has no harness bridge to time

Bridge stage: api socket connectable: No successful runs

Bridge stage: hello_ok: No successful runs

Bridge stage: no-op reply: No successful runs

Bridge total (socket -> no-op): No successful runs

============================================================
Summary:
  Binary load median:      ~14.7 ms
  Server ready median:     ~23.1 ms
  Bridge total median:     n/a
```

#### 세션 로딩 (COLD)

```text
binary   D:\a\agent\agent\bench-bin\examples\loading_probe.exe (934eebd7c4a3)
mode     cold
protocol COLD: every measured run gets its own freshly copied HOME; no derived cache from an earlier run exists, so each sample is a first load of the fixture.
fixtures C:\Users\RUNNER~1\AppData\Local\Temp\horon-loading-bench-510pkfze
note     --warmup 3 is ignored in cold mode: every sample gets a fresh HOME
fixture events=0 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    0.079/0.079    p95    0.092/0.090    delta  +0.000 ms  control-no-change
  skill_request_overlay p50    0.585/0.586    p95    0.632/0.679    delta  +0.000 ms  control-no-change
  mcp_config_load   p50    0.779/0.753    p95    1.005/0.832    delta  -0.025 ms  control-no-change
  attach_history    p50    0.001/0.001    p95    0.001/0.001    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.001/0.001    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  session_save      p50   50.408/47.936   p95   66.467/66.429   delta  -2.472 ms  control-no-change
  total store_read_bytes p50 22940  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 22906..22956 (8 distinct values)
  SPREAD   b session_save store_read_bytes 22912..22964 (8 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50   12.654/12.701   p95   14.076/15.432   delta  +0.047 ms  control-no-change
  skill_request_overlay p50   32.956/32.233   p95   38.304/32.536   delta  -0.723 ms  control-no-change
  mcp_config_load   p50    0.835/0.828    p95    0.891/0.872    delta  -0.007 ms  control-no-change
  attach_history    p50    0.001/0.001    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.001/0.000    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  session_save      p50   72.638/58.340   p95  324.833/200.922  delta -14.298 ms  control-no-change
  total store_read_bytes p50 22932  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 22898..22952 (9 distinct values)
  SPREAD   b session_save store_read_bytes 22912..22964 (9 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.079/0.079    p95    0.088/0.086    delta  +0.001 ms  control-no-change
  skill_request_overlay p50    0.582/0.627    p95    0.638/0.696    delta  +0.046 ms  control-drift
  mcp_config_load   p50    0.783/0.768    p95    0.809/0.833    delta  -0.014 ms  control-no-change
  attach_history    p50  396.045/401.237  p95  407.501/408.785  delta  +5.192 ms  control-no-change
  resume_ready      p50  122.459/123.212  p95  145.076/126.153  delta  +0.753 ms  control-no-change
  provider_dispatch p50    1.387/1.438    p95    1.469/1.473    delta  +0.051 ms  control-no-change
  session_save      p50   47.496/46.066   p95  103.584/61.913   delta  -1.430 ms  control-no-change
  total store_read_bytes p50 5311205  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189151..4189155 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099102..1099106 (3 distinct values)
  SPREAD   a session_save store_read_bytes 22904..22964 (9 distinct values)
  SPREAD   b attach_history store_read_bytes 4189153..4189155 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099105..1099106 (2 distinct values)
  SPREAD   b session_save store_read_bytes 22931..22968 (10 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50   12.147/11.566   p95   12.387/12.226   delta  -0.580 ms  control-drift
  skill_request_overlay p50   31.608/32.039   p95   36.877/36.051   delta  +0.431 ms  control-no-change
  mcp_config_load   p50    0.804/0.828    p95    0.841/0.879    delta  +0.024 ms  control-no-change
  attach_history    p50  397.043/397.387  p95  492.980/450.931  delta  +0.344 ms  control-no-change
  resume_ready      p50  127.676/121.642  p95  136.180/123.610  delta  -6.034 ms  control-no-change
  provider_dispatch p50    1.359/1.323    p95    1.424/1.753    delta  -0.036 ms  control-no-change
  session_save      p50   51.959/56.933   p95  103.042/76.933   delta  +4.974 ms  control-no-change
  total store_read_bytes p50 5311214  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189153..4189155 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099105..1099106 (2 distinct values)
  SPREAD   a session_save store_read_bytes 22924..22976 (9 distinct values)
  SPREAD   b session_save store_read_bytes 22939..22984 (7 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.079/0.080    p95    0.092/0.111    delta  +0.001 ms  control-no-change
  skill_request_overlay p50    0.650/0.649    p95    0.736/1.154    delta  -0.002 ms  control-no-change
  mcp_config_load   p50    0.765/0.764    p95    0.890/1.331    delta  -0.000 ms  control-no-change
  attach_history    p50 4659.331/4674.706 p95 5210.793/4794.073 delta +15.375 ms  control-no-change
  resume_ready      p50 1310.327/1312.874 p95 1354.155/1334.292 delta  +2.548 ms  control-no-change
  provider_dispatch p50   15.001/14.988   p95   16.274/15.237   delta  -0.014 ms  control-no-change
  session_save      p50   48.040/47.150   p95   76.752/433.371  delta  -0.890 ms  control-no-change
  total store_read_bytes p50 53417435  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184249..42184253 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210212..11210216 (2 distinct values)
  SPREAD   a session_save store_read_bytes 22920..22987 (8 distinct values)
  SPREAD   b attach_history store_read_bytes 42184251..42184253 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210215..11210216 (2 distinct values)
  SPREAD   b session_save store_read_bytes 22938..22975 (7 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50   12.064/12.069   p95   12.626/15.236   delta  +0.005 ms  control-no-change
  skill_request_overlay p50   32.047/32.273   p95   33.310/36.356   delta  +0.226 ms  control-no-change
  mcp_config_load   p50    0.833/0.840    p95    0.927/1.041    delta  +0.007 ms  control-no-change
  attach_history    p50 4678.186/4674.549 p95 4719.880/4716.489 delta  -3.637 ms  control-no-change
  resume_ready      p50 1299.225/1292.846 p95 1323.220/1322.051 delta  -6.379 ms  control-no-change
  provider_dispatch p50   14.828/14.946   p95   15.209/17.105   delta  +0.118 ms  control-no-change
  session_save      p50   45.153/47.857   p95   61.944/56.583   delta  +2.704 ms  control-no-change
  total store_read_bytes p50 53417424  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184249..42184253 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210212..11210216 (3 distinct values)
  SPREAD   a session_save store_read_bytes 22903..22987 (9 distinct values)
  SPREAD   b attach_history store_read_bytes 42184251..42184253 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210215..11210216 (2 distinct values)
  SPREAD   b session_save store_read_bytes 22938..22967 (8 distinct values)
report   bench-results\loading-cold.json
mode cold, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### 세션 로딩 (WARM)

```text
binary   D:\a\agent\agent\bench-bin\examples\loading_probe.exe (934eebd7c4a3)
mode     warm
protocol WARM: one HOME per side, --warmup discarded runs per side, then measured runs alternate between the two sides on those same HOMEs.
fixtures C:\Users\RUNNER~1\AppData\Local\Temp\horon-loading-bench-fnaikcyb
fixture events=0 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    0.107/0.102    p95    0.136/0.126    delta  -0.005 ms  control-no-change
  skill_request_overlay p50    0.756/0.733    p95    0.814/0.832    delta  -0.023 ms  control-no-change
  mcp_config_load   p50    0.880/0.902    p95    0.915/0.953    delta  +0.022 ms  control-no-change
  attach_history    p50    0.000/0.003    p95    0.003/0.008    delta  +0.003 ms  control-drift
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.001/0.001    delta  -0.000 ms  control-no-change
  session_save      p50   38.898/39.156   p95  181.536/49.585   delta  +0.258 ms  control-no-change
  total store_read_bytes p50 23646  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [22921, 22931, 22945, 23621, 23639, 23652, 24313, 24318, 24342, 24351]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [22947, 22949, 22957, 23654, 23656, 23660, 24329, 24331, 24339]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 22921..24351 (10 distinct values)
  SPREAD   b session_save store_read_bytes 22947..24339 (9 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50   14.316/14.174   p95   14.767/17.158   delta  -0.142 ms  control-no-change
  skill_request_overlay p50   37.020/37.145   p95   46.263/40.141   delta  +0.125 ms  control-no-change
  mcp_config_load   p50    0.928/0.954    p95    1.282/1.113    delta  +0.026 ms  control-no-change
  attach_history    p50    0.001/0.001    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.001    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.001/0.000    delta  +0.000 ms  control-no-change
  session_save      p50   49.797/48.192   p95  109.444/159.467  delta  -1.604 ms  control-no-change
  total store_read_bytes p50 23650  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [22925, 22953, 22961, 23648, 23652, 24351, 24355, 24359]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [22953, 22963, 22977, 23634, 23640, 23656, 24323, 24333, 24339, 24351]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 22925..24359 (8 distinct values)
  SPREAD   b session_save store_read_bytes 22953..24351 (10 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.101/0.097    p95    0.162/0.154    delta  -0.003 ms  control-no-change
  skill_request_overlay p50    0.763/0.775    p95    1.073/1.146    delta  +0.011 ms  control-no-change
  mcp_config_load   p50    0.892/0.929    p95    0.964/1.329    delta  +0.038 ms  control-no-change
  attach_history    p50  196.026/197.667  p95  220.204/212.984  delta  +1.641 ms  control-no-change
  resume_ready      p50  152.684/150.372  p95  159.355/154.234  delta  -2.312 ms  control-no-change
  provider_dispatch p50    1.484/1.486    p95    1.634/1.558    delta  +0.002 ms  control-no-change
  session_save      p50   37.897/40.768   p95  333.350/220.663  delta  +2.870 ms  control-no-change
  total store_read_bytes p50 2841097  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23643, 23659, 24328, 24343, 25004, 25046, 25747, 25753, 26452, 26466]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23634, 23667, 24350, 24354, 25057, 25071, 25756, 25772, 26447, 26459]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23643..26466 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23634..26459 (10 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50   14.883/14.870   p95   15.334/15.266   delta  -0.013 ms  control-no-change
  skill_request_overlay p50   36.858/36.798   p95   38.158/37.430   delta  -0.060 ms  control-no-change
  mcp_config_load   p50    0.993/0.917    p95    1.069/0.974    delta  -0.077 ms  control-no-change
  attach_history    p50  195.850/196.627  p95  204.100/204.975  delta  +0.777 ms  control-no-change
  resume_ready      p50  149.585/148.859  p95  157.091/157.755  delta  -0.726 ms  control-no-change
  provider_dispatch p50    1.485/1.466    p95    1.528/1.494    delta  -0.019 ms  control-no-change
  session_save      p50   44.814/54.898   p95  196.447/112.017  delta +10.084 ms  control-no-change
  total store_read_bytes p50 2841130  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23667, 23675, 24321, 24329, 25047, 25054, 25745, 25750, 26433, 26444]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23655, 23665, 24336, 24344, 25029, 25053, 25691, 25754, 26448, 26457]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23667..26444 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23655..26457 (10 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.099/0.097    p95    0.136/0.161    delta  -0.002 ms  control-no-change
  skill_request_overlay p50    0.752/0.753    p95    0.831/0.820    delta  +0.002 ms  control-no-change
  mcp_config_load   p50    0.898/0.933    p95    0.944/0.984    delta  +0.035 ms  control-no-change
  attach_history    p50 2103.316/2099.309 p95 2123.937/2122.366 delta  -4.007 ms  control-no-change
  resume_ready      p50 1567.250/1569.259 p95 1575.554/1585.703 delta  +2.009 ms  control-no-change
  provider_dispatch p50   15.933/15.832   p95   16.876/17.751   delta  -0.101 ms  control-no-change
  session_save      p50   39.080/39.184   p95   51.763/146.892  delta  +0.104 ms  control-no-change
  total store_read_bytes p50 28640162  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23650, 23666, 24333, 24351, 25054, 25076, 25735, 25759, 26446, 26447]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23630, 23670, 24353, 24373, 25051, 25076, 25728, 25731, 26435, 26454]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23650..26447 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23630..26454 (10 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50   14.766/14.562   p95   15.316/15.625   delta  -0.205 ms  control-no-change
  skill_request_overlay p50   37.043/37.013   p95   37.624/38.209   delta  -0.030 ms  control-no-change
  mcp_config_load   p50    0.926/0.976    p95    1.107/1.019    delta  +0.050 ms  control-no-change
  attach_history    p50 2083.063/2077.153 p95 2099.146/2097.535 delta  -5.910 ms  control-no-change
  resume_ready      p50 1552.093/1548.812 p95 1575.696/1572.338 delta  -3.280 ms  control-no-change
  provider_dispatch p50   15.646/15.556   p95   16.554/16.283   delta  -0.090 ms  control-no-change
  session_save      p50   38.269/38.625   p95   48.799/115.349  delta  +0.356 ms  control-no-change
  total store_read_bytes p50 28640152  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23654, 23665, 24317, 24352, 25050, 25059, 25734, 25750, 26432, 26457]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23647, 23650, 24342, 24344, 25034, 25073, 25756, 25763, 26434, 26451]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23654..26457 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23647..26451 (10 distinct values)
report   bench-results\loading-warm.json
mode warm, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### Tool latency

| Tool case | median | p95 | min | max | samples |
|---|---|---|---|---|---|
| read (tiny file) | 2.3 ms | 2.8 ms | 2.3 ms | 2.8 ms | 5 |
| read (1000 lines) | 2.8 ms | 2.8 ms | 2.3 ms | 2.8 ms | 5 |
| read (10 lines @ offset) | 2.8 ms | 2.8 ms | 2.7 ms | 2.8 ms | 5 |
| read (repo Cargo.toml) | 2.7 ms | 2.8 ms | 2.2 ms | 2.8 ms | 5 |
| write (small) | 24.6 ms | 127.0 ms | 15.2 ms | 127.0 ms | 5 |
| write (10KB) | 12.8 ms | 51.2 ms | 10.1 ms | 51.2 ms | 5 |
| edit (simple replace) | 10.5 ms | 18.0 ms | 10.1 ms | 18.0 ms | 5 |
| ls (scripts dir) | 2.8 ms | 5.6 ms | 2.7 ms | 5.6 ms | 5 |
| agentgrep (grep in tree) | 8.5 ms | 16.6 ms | 8.0 ms | 16.6 ms | 5 |
| agentgrep (find **/*.rs) | 7.6 ms | 10.2 ms | 7.0 ms | 10.2 ms | 5 |
| bash (true) | 12.8 ms | 13.3 ms | 11.4 ms | 13.3 ms | 5 |
| bash (echo hello) | 10.1 ms | 13.1 ms | 10.0 ms | 13.1 ms | 5 |
| bash (ls fixture tree) | 15.3 ms | 19.3 ms | 13.2 ms | 19.3 ms | 5 |
| invalid tool (dispatch only) | 2.8 ms | 3.0 ms | 2.2 ms | 3.0 ms | 5 |

#### Reload storm

1 resume(s) per level; each level resumes sessions no other level has opened, from a freshly started daemon.

| Concurrent resumes | history median | p95 | max | storm wall | failures |
|---|---|---|---|---|---|
| 1 | 235.8 ms | 235.8 ms | 235.8 ms | 236.0 ms | 0 |
| 10 | 659.3 ms | 706.4 ms | 706.4 ms | 708.6 ms | 0 |
| 40 | 2.31 s | 3.26 s | 3.45 s | 3.47 s | 0 |

#### Compile loop

| Checkpoint | Touched | median | min | max | samples |
|---|---|---|---|---|---|
| warm_noop | — | 663.9 ms | 663.9 ms | 663.9 ms | 1 |
| leaf_touch | src\main.rs | 902.9 ms | 902.9 ms | 902.9 ms | 1 |
| core_touch | crates\app-core\src\lib.rs | 25.21 s | 25.21 s | 25.21 s | 1 |
| build_meta_touch | crates\build-meta\src\lib.rs | 35.77 s | 35.77 s | 35.77 s | 1 |

<!-- bench-auto:end -->
