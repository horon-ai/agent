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

<!-- bench-auto:start commit=a21f3c3db06ba55b7a3df62304a4c227f581a3b1 -->

이 구간은 `.github/workflows/benchmark-docs.yml` 이 자동으로 다시 씁니다. 손으로 고치지 않습니다.

- 커밋: `a21f3c3db06ba55b7a3df62304a4c227f581a3b1`
- 측정 시각: 2026-11-09 00:07 UTC
- 실행: <https://github.com/horon-ai/agent/actions/runs/42534644003> (원본 JSON 은 OS별 `benchmark-<os>-<run_id>` 아티팩트)

OS마다 다른 GitHub 호스티드 러너에서 따로 잽니다. 기계가 다르므로 OS끼리 절대값을 비교하지 않고, 같은 OS 안의 상대 비교(horon 대비 배수)와 커밋 사이의 추세만 읽습니다.

| OS | 러너 | 바이너리 | 상태 |
| --- | --- | --- | --- |
| Linux | `ubuntu-24.04` | `horon-agent v0.1.0-dev (a21f3c3)` | 성공 |
| macOS | `macos-latest` | `horon-agent v0.1.0-dev (a21f3c3)` | 성공 |
| Windows | `windows-latest` | `horon-agent v0.1.0-dev (a21f3c3)` | 성공 |

### Linux

#### Coding agent comparison (headless, mock model)

Host: linux/x86_64, 4 CPUs, Python 3.12.16. Baseline: **horon**.

| Tool | Version | `--version` (median) | Install size |
|---|---|---|---|
| horon | horon-agent v0.1.0-dev (a21f3c3) | 3.3 ms | 110.5 MB |
| jcode | jcode v0.99.1 (7206efd0c) | 9.6 ms | 163.4 MB |
| Claude Code | 2.1.317 (Claude Code) | 9.4 ms | 632.2 MB |
| Codex CLI | codex-cli 0.166.0 | 39.2 ms | 435.0 MB |
| OpenCode | 1.19.5 | 364.6 ms | 613.4 MB |
| pi | 1.5.0 | 187.5 ms | 108.9 MB |
| oh-my-pi (omp) | omp/18.18.0 | 529.8 ms | 412.6 MB |
| oh-my-openagent (omo) | omo 5.1.64 (engine: senpi 2026.11.5-1) | 101.2 ms | 520.6 MB |

**One-shot headless prompt** (includes startup, one `ls` tool call, final answer)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Tool round-trip | fsync calls | Note |
|---|---|---|---|---|---|---|---|---|
| horon | 65.8 ms | 54.5 ms–299.9 ms | baseline | 36.9 MB | baseline | yes | 16 | — |
| jcode | 144.6 ms | 141.0 ms–146.9 ms | 2.20× more | 38.4 MB | 1.04× more | yes | 5 | — |
| Claude Code | 363.0 ms | 361.2 ms–394.7 ms | 5.52× more | 201.8 MB | 5.46× more | yes | — | — |
| Codex CLI | 445.4 ms | 436.0 ms–476.2 ms | 6.77× more | 191.2 MB | 5.18× more | yes | 6 | — |
| OpenCode | 2.38 s | 2.36 s–2.43 s | 36.19× more | 563.6 MB | 15.26× more | yes | 4 | — |
| pi | 310.9 ms | 310.5 ms–325.5 ms | 4.72× more | 113.2 MB | 3.06× more | yes | — | — |
| oh-my-pi (omp) | 1.42 s | 1.41 s–1.72 s | 21.57× more | 388.1 MB | 10.51× more | yes | 66 | — |
| oh-my-openagent (omo) | 5.04 s | 4.93 s–5.18 s | 76.57× more | 650.3 MB | 17.61× more | yes | 36 | — |

**Skill invocation** (process start to final answer, including skill discovery and one load of the fixture skill)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | Note |
|---|---|---|---|---|---|---|---|---|
| horon | 50.8 ms | 50.2 ms–56.2 ms | baseline | 36.6 MB | baseline | skill-uri | yes | — |
| jcode | 49.3 ms | 48.7 ms–49.7 ms | 1.03× less | 40.5 MB | 1.11× more | skill-tool | yes | — |
| Claude Code | 359.6 ms | 351.2 ms–363.7 ms | 7.08× more | 206.1 MB | 5.63× more | skill-tool | yes | — |
| Codex CLI | 507.6 ms | 500.5 ms–558.1 ms | 9.99× more | 193.0 MB | 5.27× more | file-read | yes | — |
| OpenCode | 2.50 s | 2.49 s–2.50 s | 49.23× more | 613.1 MB | 16.74× more | skill-tool | yes | — |
| pi | 408.5 ms | 404.1 ms–410.5 ms | 8.04× more | 123.8 MB | 3.38× more | file-read | yes | — |
| oh-my-pi (omp) | 1.40 s | 1.39 s–1.44 s | 27.65× more | 340.8 MB | 9.30× more | file-read | yes | — |
| oh-my-openagent (omo) | 5.40 s | 5.11 s–5.41 s | 106.31× more | 660.4 MB | 18.02× more | file-read | yes | — |

**MCP tool call** (process start to final answer, including starting the fixture stdio MCP server and one `bench_echo` call)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | MCP ready | Note |
|---|---|---|---|---|---|---|---|---|---|
| horon | 72.3 ms | 71.2 ms–84.8 ms | baseline | 41.4 MB | baseline | mcp-call | yes | 52.0 ms | — |
| jcode | 165.2 ms | 165.1 ms–168.9 ms | 2.29× more | 55.7 MB | 1.34× more | mcp-action | yes | 139.7 ms | — |
| Claude Code | 453.9 ms | 446.5 ms–469.7 ms | 6.28× more | 227.4 MB | 5.49× more | mcp-tool | yes | 327.7 ms | — |
| Codex CLI | 501.6 ms | 472.3 ms–513.5 ms | 6.94× more | 180.2 MB | 4.35× more | mcp-tool | yes | 331.2 ms | — |
| OpenCode | 2.32 s | 2.31 s–2.34 s | 32.15× more | 561.7 MB | 13.55× more | mcp-tool | yes | 2.10 s | — |
| pi | 456.3 ms | 449.1 ms–462.1 ms | 6.31× more | 138.9 MB | 3.35× more | mcp-tool | yes | 400.7 ms | — |
| oh-my-pi (omp) | 1.41 s | 1.41 s–1.43 s | 19.48× more | 346.4 MB | 8.36× more | mcp-route | yes | 1.33 s | — |
| oh-my-openagent (omo) | — | — | — | — | — | mcp-tool | no | — | failed: the fixture sentinel never came back |

**Session memory** (idle after 3 turn(s) per session; sizes are process-tree sums, RSS; PSS also recorded on Linux). "Each additional" is the least-squares slope across the measured session counts; 0 B means growth stayed below RSS sampling noise. Threads and descriptors come from the daemon's own `server:memory` after the turns.

| Tool | First session | Each additional | vs horon (additional) | Threads/session | FDs/session | First (post-turn) | Each additional (post-turn) |
|---|---|---|---|---|---|---|---|
| horon | 36.4 MB | 130.7 KB | baseline | 0.11 | 1.00 | 43.5 MB | 401.1 KB |
| jcode | 43.2 MB | 51.7 KB | 2.53× less | 0.11 | — | 46.4 MB | 228.9 KB |
| Claude Code | 151.6 MB | 150.0 MB | 1,175× more | — | — | 187.5 MB | 180.8 MB |
| Codex CLI | 129.9 MB | 132.4 MB | 1,037× more | — | — | 162.8 MB | 164.1 MB |
| OpenCode | 236.7 MB | 1.4 MB | 11.19× more | — | — | 452.4 MB | 15.3 MB |
| pi | 109.0 MB | 107.6 MB | 843.11× more | — | — | 131.7 MB | 128.0 MB |
| oh-my-pi (omp) | 313.8 MB | 108.0 MB | 845.92× more | — | — | 362.9 MB | 356.7 MB |
| oh-my-openagent (omo) | 276.1 MB | 36.2 MB | 283.64× more | — | — | 584.2 MB | 580.1 MB |

Threads/descriptors unavailable: jcode (server:memory did not report fd_count)

**Post-turn tree RSS per session count**

| Tool | 1 session(s) | 5 session(s) | 10 session(s) |
|---|---|---|---|
| horon | 43.5 MB | 45.9 MB | 47.1 MB |
| jcode | 46.4 MB | 47.7 MB | 48.4 MB |
| Claude Code | 187.5 MB | 919.6 MB | 1.8 GB |
| Codex CLI | 162.8 MB | 814.7 MB | 1.6 GB |
| OpenCode | 452.4 MB | 521.7 MB | 590.3 MB |
| pi | 131.7 MB | 607.2 MB | 1.3 GB |
| oh-my-pi (omp) | 362.9 MB | 1.8 GB | 3.5 GB |
| oh-my-openagent (omo) | 584.2 MB | 3.0 GB | 5.7 GB |

#### 기동

```text
Benchmarking: /home/runner/work/agent/agent/bench-bin/horon-agent
============================================================

--help (binary load):
  Min:    3.89 ms
  Max:    4.07 ms
  Mean:   3.96 ms
  Median: 3.96 ms
  Stdev:  0.06 ms

--version:
  Min:    3.64 ms
  Max:    3.94 ms
  Mean:   3.73 ms
  Median: 3.71 ms
  Stdev:  0.09 ms

Measuring isolated server startup (10 runs)...

Server ready (isolated socket connectable):
  Min:    20.55 ms
  Max:    20.73 ms
  Mean:   20.62 ms
  Median: 20.61 ms
  Stdev:  0.05 ms

Measuring harness bridge readiness (10 runs)...

Bridge stage: api socket connectable:
  Min:    20.66 ms
  Max:    20.77 ms
  Mean:   20.70 ms
  Median: 20.70 ms
  Stdev:  0.03 ms

Bridge stage: hello_ok:
  Min:    21.31 ms
  Max:    21.58 ms
  Mean:   21.40 ms
  Median: 21.38 ms
  Stdev:  0.08 ms

Bridge stage: no-op reply:
  Min:    21.73 ms
  Max:    22.07 ms
  Mean:   21.84 ms
  Median: 21.84 ms
  Stdev:  0.10 ms

Bridge total (socket -> no-op):
  Min:    21.73 ms
  Max:    22.07 ms
  Mean:   21.84 ms
  Median: 21.84 ms
  Stdev:  0.10 ms

============================================================
Summary:
  Binary load median:      ~4.0 ms
  Server ready median:     ~20.6 ms
  Bridge total median:     ~21.8 ms
```

#### 세션 로딩 (COLD)

```text
binary   /home/runner/work/agent/agent/bench-bin/examples/loading_probe (613929db3225)
mode     cold
protocol COLD: every measured run gets its own freshly copied HOME; no derived cache from an earlier run exists, so each sample is a first load of the fixture.
fixtures /tmp/horon-loading-bench-676aasj0
note     --warmup 3 is ignored in cold mode: every sample gets a fresh HOME
fixture events=0 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    0.011/0.011    p95    0.013/0.012    delta  -0.000 ms  control-no-change
  skill_request_overlay p50    0.085/0.085    p95    0.088/0.090    delta  +0.000 ms  control-no-change
  mcp_config_load   p50    0.113/0.114    p95    0.117/0.150    delta  +0.001 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.001/0.000    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.001    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.001/0.001    delta  -0.000 ms  control-no-change
  session_save      p50    7.542/6.913    p95  233.867/257.037  delta  -0.628 ms  control-no-change
  total store_read_bytes p50 23390  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 23380..23400 (6 distinct values)
  SPREAD   b session_save store_read_bytes 23376..23400 (6 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    2.033/2.043    p95    2.082/2.083    delta  +0.010 ms  control-no-change
  skill_request_overlay p50    3.598/3.551    p95    3.643/3.598    delta  -0.047 ms  control-no-change
  mcp_config_load   p50    0.118/0.117    p95    0.121/0.120    delta  -0.001 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.001    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  session_save      p50   15.241/14.582   p95   37.177/33.024   delta  -0.658 ms  control-no-change
  total store_read_bytes p50 23390  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 23380..23396 (5 distinct values)
  SPREAD   b session_save store_read_bytes 23372..23400 (7 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.012/0.011    p95    0.014/0.012    delta  -0.001 ms  control-no-change
  skill_request_overlay p50    0.086/0.085    p95    0.112/0.090    delta  -0.001 ms  control-no-change
  mcp_config_load   p50    0.115/0.112    p95    0.118/0.115    delta  -0.003 ms  control-no-change
  attach_history    p50  190.654/187.113  p95  245.715/190.657  delta  -3.541 ms  control-no-change
  resume_ready      p50   35.230/35.601   p95   35.875/39.597   delta  +0.371 ms  control-no-change
  provider_dispatch p50    0.829/0.840    p95    0.872/0.858    delta  +0.011 ms  control-no-change
  session_save      p50    6.416/6.545    p95   16.477/148.930  delta  +0.129 ms  control-no-change
  total store_read_bytes p50 5311727  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 23374..23402 (5 distinct values)
  SPREAD   b attach_history store_read_bytes 4189191..4189195 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099132..1099136 (3 distinct values)
  SPREAD   b session_save store_read_bytes 23380..23406 (6 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    2.019/2.052    p95    2.765/2.121    delta  +0.033 ms  control-no-change
  skill_request_overlay p50    3.530/3.552    p95    4.811/3.755    delta  +0.023 ms  control-no-change
  mcp_config_load   p50    0.120/0.120    p95    0.193/0.124    delta  +0.000 ms  control-no-change
  attach_history    p50  191.279/186.211  p95  197.011/205.304  delta  -5.069 ms  control-no-change
  resume_ready      p50   35.133/35.415   p95   35.720/37.126   delta  +0.282 ms  control-no-change
  provider_dispatch p50    0.828/0.818    p95    0.856/0.852    delta  -0.010 ms  control-no-change
  session_save      p50    6.329/6.310    p95   11.744/8.643    delta  -0.019 ms  control-no-change
  total store_read_bytes p50 5311713  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189191..4189195 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099132..1099136 (3 distinct values)
  SPREAD   a session_save store_read_bytes 23362..23398 (7 distinct values)
  SPREAD   b attach_history store_read_bytes 4189191..4189195 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099132..1099136 (3 distinct values)
  SPREAD   b session_save store_read_bytes 23376..23394 (6 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.012/0.012    p95    0.014/0.013    delta  -0.000 ms  control-no-change
  skill_request_overlay p50    0.086/0.085    p95    0.088/0.104    delta  -0.001 ms  control-no-change
  mcp_config_load   p50    0.115/0.115    p95    0.125/0.129    delta  -0.000 ms  control-no-change
  attach_history    p50 2268.545/2274.791 p95 2322.477/2363.902 delta  +6.246 ms  control-no-change
  resume_ready      p50  383.302/381.430  p95  406.329/389.164  delta  -1.872 ms  control-no-change
  provider_dispatch p50    8.919/8.986    p95   11.147/10.234   delta  +0.067 ms  control-no-change
  session_save      p50    6.628/6.477    p95    7.014/10.428   delta  -0.151 ms  control-no-change
  total store_read_bytes p50 53417936  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184291..42184293 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210245..11210246 (2 distinct values)
  SPREAD   a session_save store_read_bytes 23381..23405 (8 distinct values)
  SPREAD   b attach_history store_read_bytes 42184289..42184293 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210242..11210246 (3 distinct values)
  SPREAD   b session_save store_read_bytes 23385..23405 (8 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    2.040/2.009    p95    2.102/2.034    delta  -0.031 ms  control-no-change
  skill_request_overlay p50    3.519/3.517    p95    3.643/3.575    delta  -0.002 ms  control-no-change
  mcp_config_load   p50    0.117/0.117    p95    0.123/0.121    delta  -0.000 ms  control-no-change
  attach_history    p50 2327.282/2302.023 p95 2401.391/2403.721 delta -25.259 ms  control-no-change
  resume_ready      p50  391.219/389.735  p95  407.670/413.408  delta  -1.483 ms  control-no-change
  provider_dispatch p50    9.774/9.803    p95   11.393/10.917   delta  +0.029 ms  control-no-change
  session_save      p50    6.768/6.800    p95   19.096/17.183   delta  +0.032 ms  control-no-change
  total store_read_bytes p50 53417931  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184289..42184293 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210242..11210246 (3 distinct values)
  SPREAD   a session_save store_read_bytes 23383..23405 (8 distinct values)
  SPREAD   b attach_history store_read_bytes 42184285..42184293 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210238..11210246 (3 distinct values)
  SPREAD   b session_save store_read_bytes 23389..23409 (5 distinct values)
report   bench-results/loading-cold.json
mode cold, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### 세션 로딩 (WARM)

```text
binary   /home/runner/work/agent/agent/bench-bin/examples/loading_probe (613929db3225)
mode     warm
protocol WARM: one HOME per side, --warmup discarded runs per side, then measured runs alternate between the two sides on those same HOMEs.
fixtures /tmp/horon-loading-bench-ihoft2oj
fixture events=0 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    0.009/0.008    p95    0.010/0.009    delta  -0.000 ms  control-no-change
  skill_request_overlay p50    0.050/0.050    p95    0.064/0.082    delta  +0.000 ms  control-no-change
  mcp_config_load   p50    0.070/0.070    p95    0.081/0.089    delta  +0.001 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  session_save      p50    8.789/8.531    p95   13.112/18.052   delta  -0.258 ms  control-no-change
  total store_read_bytes p50 24111  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23377, 23389, 23393, 24099, 24111, 24817, 24821, 24829]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23389, 23393, 23397, 24099, 24107, 24809, 24821, 24825]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23377..24829 (8 distinct values)
  SPREAD   b session_save store_read_bytes 23389..24825 (8 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    1.710/1.713    p95    1.781/1.938    delta  +0.003 ms  control-no-change
  skill_request_overlay p50    2.347/2.342    p95    2.397/2.389    delta  -0.004 ms  control-no-change
  mcp_config_load   p50    0.077/0.075    p95    0.083/0.078    delta  -0.001 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.001/0.001    delta  -0.000 ms  control-no-change
  session_save      p50  103.412/107.806  p95  195.522/225.298  delta  +4.395 ms  control-no-change
  total store_read_bytes p50 24103  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23389, 23391, 24097, 24103, 24807, 24821, 24827]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23371, 23375, 23397, 24093, 24101, 24107, 24805, 24813, 24815, 24819]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23389..24827 (7 distinct values)
  SPREAD   b session_save store_read_bytes 23371..24819 (10 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.008/0.008    p95    0.009/0.010    delta  +0.000 ms  control-no-change
  skill_request_overlay p50    0.050/0.050    p95    0.071/0.065    delta  +0.000 ms  control-no-change
  mcp_config_load   p50    0.071/0.071    p95    0.083/0.088    delta  -0.000 ms  control-no-change
  attach_history    p50   75.584/75.105   p95   76.446/76.085   delta  -0.479 ms  control-no-change
  resume_ready      p50   36.053/36.009   p95   36.436/36.389   delta  -0.044 ms  control-no-change
  provider_dispatch p50    1.179/1.162    p95    1.226/1.217    delta  -0.017 ms  control-no-change
  session_save      p50    8.675/8.694    p95   28.896/29.034   delta  +0.020 ms  control-no-change
  total store_read_bytes p50 2841678  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [24092, 24100, 24814, 24834, 25536, 25552, 26246, 26254, 26976, 26984]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [24100, 24106, 24812, 24818, 25532, 25546, 26258, 26262, 26974, 26980]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 24092..26984 (10 distinct values)
  SPREAD   b session_save store_read_bytes 24100..26980 (10 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    1.754/1.756    p95    1.801/1.800    delta  +0.001 ms  control-no-change
  skill_request_overlay p50    2.364/2.363    p95    2.496/2.465    delta  -0.000 ms  control-no-change
  mcp_config_load   p50    0.078/0.077    p95    0.085/0.080    delta  -0.001 ms  control-no-change
  attach_history    p50   75.089/75.700   p95   75.706/76.117   delta  +0.611 ms  control-no-change
  resume_ready      p50   36.054/36.159   p95   36.440/36.532   delta  +0.105 ms  control-no-change
  provider_dispatch p50    1.208/1.181    p95    1.258/1.219    delta  -0.027 ms  control-no-change
  session_save      p50   11.026/11.153   p95   18.090/28.355   delta  +0.127 ms  control-no-change
  total store_read_bytes p50 2841670  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [24085, 24105, 24803, 24819, 25525, 25551, 26251, 26265, 26965, 26973]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [24108, 24112, 24822, 24826, 25538, 25540, 26250, 26256, 26962, 26980]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 24085..26973 (10 distinct values)
  SPREAD   b session_save store_read_bytes 24108..26980 (10 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.008/0.008    p95    0.018/0.009    delta  +0.000 ms  control-no-change
  skill_request_overlay p50    0.051/0.050    p95    0.057/0.065    delta  -0.000 ms  control-no-change
  mcp_config_load   p50    0.071/0.072    p95    0.074/0.072    delta  +0.001 ms  control-no-change
  attach_history    p50  882.666/883.545  p95  888.829/891.638  delta  +0.879 ms  control-no-change
  resume_ready      p50  446.307/446.688  p95  452.526/451.791  delta  +0.380 ms  control-no-change
  provider_dispatch p50   15.934/15.837   p95   16.471/16.368   delta  -0.097 ms  control-no-change
  session_save      p50   34.350/9.155    p95  255.024/53.484   delta -25.196 ms  control-no-change
  total store_read_bytes p50 28640694  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [24103, 24119, 24825, 25539, 25547, 26253, 26257, 26975, 26987]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [24099, 24107, 24825, 24829, 25543, 26249, 26269, 26967, 26971]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 24103..26987 (9 distinct values)
  SPREAD   b session_save store_read_bytes 24099..26971 (9 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    1.806/1.767    p95    2.000/1.894    delta  -0.039 ms  control-no-change
  skill_request_overlay p50    2.398/2.369    p95    2.443/2.538    delta  -0.029 ms  control-no-change
  mcp_config_load   p50    0.078/0.078    p95    0.097/0.087    delta  -0.000 ms  control-no-change
  attach_history    p50  881.291/880.891  p95  886.085/889.505  delta  -0.401 ms  control-no-change
  resume_ready      p50  461.637/457.316  p95  466.442/463.447  delta  -4.321 ms  control-no-change
  provider_dispatch p50   15.816/15.722   p95   16.552/16.166   delta  -0.094 ms  control-no-change
  session_save      p50   12.834/13.789   p95   34.752/32.392   delta  +0.954 ms  control-no-change
  total store_read_bytes p50 28640676  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [24085, 24106, 24797, 24812, 25534, 25535, 25540, 26228, 26270, 26976]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [24095, 24103, 24829, 24833, 25543, 25551, 26249, 26265, 26979]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 24085..26976 (10 distinct values)
  SPREAD   b session_save store_read_bytes 24095..26979 (9 distinct values)
report   bench-results/loading-warm.json
mode warm, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### Tool latency

| Tool case | median | p95 | min | max | samples |
|---|---|---|---|---|---|
| read (tiny file) | 0.3 ms | 0.4 ms | 0.3 ms | 0.4 ms | 5 |
| read (1000 lines) | 0.4 ms | 0.5 ms | 0.4 ms | 0.5 ms | 5 |
| read (10 lines @ offset) | 0.4 ms | 0.4 ms | 0.3 ms | 0.4 ms | 5 |
| read (repo Cargo.toml) | 0.3 ms | 0.3 ms | 0.3 ms | 0.3 ms | 5 |
| write (small) | 1.3 ms | 1.3 ms | 1.1 ms | 1.3 ms | 5 |
| write (10KB) | 1.3 ms | 1.3 ms | 1.2 ms | 1.3 ms | 5 |
| edit (simple replace) | 1.3 ms | 1.5 ms | 1.2 ms | 1.5 ms | 5 |
| ls (scripts dir) | 0.9 ms | 0.9 ms | 0.8 ms | 0.9 ms | 5 |
| agentgrep (grep in tree) | 1.8 ms | 1.8 ms | 1.7 ms | 1.8 ms | 5 |
| agentgrep (find **/*.rs) | 1.8 ms | 2.1 ms | 1.7 ms | 2.1 ms | 5 |
| bash (true) | 1.7 ms | 1.8 ms | 1.7 ms | 1.8 ms | 5 |
| bash (echo hello) | 1.8 ms | 1.8 ms | 1.7 ms | 1.8 ms | 5 |
| bash (ls fixture tree) | 2.9 ms | 3.0 ms | 2.9 ms | 3.0 ms | 5 |
| invalid tool (dispatch only) | 0.1 ms | 0.1 ms | 0.1 ms | 0.1 ms | 5 |

#### Reload storm

1 resume(s) per level; each level resumes sessions no other level has opened, from a freshly started daemon.

| Concurrent resumes | history median | p95 | max | storm wall | failures |
|---|---|---|---|---|---|
| 1 | 69.4 ms | 69.4 ms | 69.4 ms | 69.5 ms | 0 |
| 10 | 227.0 ms | 251.7 ms | 251.7 ms | 252.3 ms | 0 |
| 40 | 970.8 ms | 1.15 s | 1.15 s | 1.15 s | 0 |

#### Compile loop

| Checkpoint | Touched | median | min | max | samples |
|---|---|---|---|---|---|
| warm_noop | — | 238.9 ms | 238.9 ms | 238.9 ms | 1 |
| leaf_touch | src/main.rs | 366.6 ms | 366.6 ms | 366.6 ms | 1 |
| core_touch | crates/app-core/src/lib.rs | 18.22 s | 18.22 s | 18.22 s | 1 |
| build_meta_touch | crates/build-meta/src/lib.rs | 25.91 s | 25.91 s | 25.91 s | 1 |

### macOS

#### Coding agent comparison (headless, mock model)

Host: darwin/arm64, 3 CPUs, Python 3.12.10. Baseline: **horon**.

Skipped: syscalls (strace not available)

| Tool | Version | `--version` (median) | Install size |
|---|---|---|---|
| horon | horon-agent v0.1.0-dev (a21f3c3) | 12.7 ms | 92.9 MB |
| jcode | jcode v0.99.1 (7206efd0c) | 9.8 ms | 136.3 MB |
| Claude Code | 2.1.317 (Claude Code) | 16.0 ms | 596.8 MB |
| Codex CLI | codex-cli 0.166.0 | 65.8 ms | 346.5 MB |
| OpenCode | 1.19.5 | 360.3 ms | 344.6 MB |
| pi | 1.5.0 | 175.4 ms | 108.9 MB |
| oh-my-pi (omp) | omp/18.18.0 | 452.6 ms | 273.0 MB |
| oh-my-openagent (omo) | omo 5.1.64 (engine: senpi 2026.11.5-1) | 127.7 ms | 502.2 MB |

**One-shot headless prompt** (includes startup, one `ls` tool call, final answer)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Tool round-trip | Note |
|---|---|---|---|---|---|---|---|
| horon | 120.8 ms | 99.3 ms–396.8 ms | baseline | 22.1 MB | baseline | yes | — |
| jcode | 372.4 ms | 364.8 ms–573.6 ms | 3.08× more | 34.1 MB | 1.54× more | yes | — |
| Claude Code | 397.4 ms | 371.0 ms–620.8 ms | 3.29× more | 199.7 MB | 9.04× more | yes | — |
| Codex CLI | 470.7 ms | 407.2 ms–1.48 s | 3.90× more | 141.3 MB | 6.40× more | yes | — |
| OpenCode | 2.83 s | 2.45 s–4.55 s | 23.44× more | 746.0 MB | 33.79× more | yes | — |
| pi | 355.4 ms | 331.1 ms–381.8 ms | 2.94× more | 123.6 MB | 5.60× more | yes | — |
| oh-my-pi (omp) | 1.24 s | 1.18 s–1.30 s | 10.27× more | 360.9 MB | 16.35× more | yes | — |
| oh-my-openagent (omo) | 6.98 s | 6.21 s–7.86 s | 57.80× more | 868.1 MB | 39.32× more | yes | — |

**Skill invocation** (process start to final answer, including skill discovery and one load of the fixture skill)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | Note |
|---|---|---|---|---|---|---|---|---|
| horon | 71.6 ms | 68.3 ms–99.6 ms | baseline | 21.2 MB | baseline | skill-uri | yes | — |
| jcode | 217.3 ms | 202.8 ms–224.0 ms | 3.04× more | 29.9 MB | 1.41× more | skill-tool | yes | — |
| Claude Code | 395.2 ms | 371.4 ms–409.8 ms | 5.52× more | 200.4 MB | 9.44× more | skill-tool | yes | — |
| Codex CLI | 541.0 ms | 503.5 ms–623.4 ms | 7.56× more | 144.7 MB | 6.81× more | file-read | yes | — |
| OpenCode | 3.38 s | 3.32 s–3.93 s | 47.20× more | 728.2 MB | 34.29× more | skill-tool | yes | — |
| pi | 385.8 ms | 384.3 ms–414.0 ms | 5.39× more | 134.6 MB | 6.34× more | file-read | yes | — |
| oh-my-pi (omp) | 1.27 s | 1.20 s–1.29 s | 17.70× more | 362.4 MB | 17.07× more | file-read | yes | — |
| oh-my-openagent (omo) | 7.23 s | 5.90 s–10.07 s | 101.06× more | 873.4 MB | 41.13× more | file-read | yes | — |

**MCP tool call** (process start to final answer, including starting the fixture stdio MCP server and one `bench_echo` call)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | MCP ready | Note |
|---|---|---|---|---|---|---|---|---|---|
| horon | 111.0 ms | 96.2 ms–208.4 ms | baseline | 31.0 MB | baseline | mcp-call | yes | 80.8 ms | — |
| jcode | 325.6 ms | 313.4 ms–351.0 ms | 2.93× more | 43.7 MB | 1.41× more | mcp-action | yes | 296.8 ms | — |
| Claude Code | 583.1 ms | 560.2 ms–620.4 ms | 5.25× more | 227.7 MB | 7.35× more | mcp-tool | yes | 430.1 ms | — |
| Codex CLI | 573.0 ms | 524.6 ms–875.5 ms | 5.16× more | 143.4 MB | 4.63× more | mcp-tool | yes | 226.7 ms | — |
| OpenCode | 2.37 s | 2.28 s–3.69 s | 21.31× more | 703.8 MB | 22.71× more | mcp-tool | yes | 2.09 s | — |
| pi | 439.1 ms | 423.6 ms–473.6 ms | 3.96× more | 150.8 MB | 4.87× more | mcp-tool | yes | 390.4 ms | — |
| oh-my-pi (omp) | 1.49 s | 1.36 s–1.96 s | 13.41× more | 366.8 MB | 11.84× more | mcp-route | yes | 1.42 s | — |
| oh-my-openagent (omo) | — | — | — | — | — | mcp-tool | no | — | failed: the fixture sentinel never came back |

**Session memory** (idle after 3 turn(s) per session; sizes are process-tree sums, RSS; PSS also recorded on Linux). "Each additional" is the least-squares slope across the measured session counts; 0 B means growth stayed below RSS sampling noise. Threads and descriptors come from the daemon's own `server:memory` after the turns.

| Tool | First session | Each additional | vs horon (additional) | Threads/session | FDs/session | First (post-turn) | Each additional (post-turn) |
|---|---|---|---|---|---|---|---|
| horon | 28.5 MB | 112.0 KB | baseline | 0.32 | 1.00 | 34.2 MB | 373.9 KB |
| jcode | 36.5 MB | 211.0 KB | 1.88× more | — | — | 40.1 MB | 343.6 KB |
| Claude Code | 157.3 MB | 149.3 MB | 1,365× more | — | — | 189.5 MB | 183.6 MB |
| Codex CLI | 98.7 MB | 83.2 MB | 760.89× more | — | — | 107.0 MB | 107.3 MB |
| OpenCode | 241.5 MB | 2.7 MB | 24.61× more | — | — | 587.6 MB | 2.4 MB |
| pi | 119.6 MB | 116.4 MB | 1,064× more | — | — | 148.8 MB | 148.1 MB |
| oh-my-pi (omp) | 362.7 MB | 201.8 MB | 1,845× more | — | — | 439.5 MB | 310.5 MB |
| oh-my-openagent (omo) | 112.4 MB | 44.5 MB | 407.21× more | — | — | 824.4 MB | 325.6 MB |

Threads/descriptors unavailable: jcode (server:memory did not report thread_count, fd_count)

**Post-turn tree RSS per session count**

| Tool | 1 session(s) | 5 session(s) | 10 session(s) |
|---|---|---|---|
| horon | 34.2 MB | 35.5 MB | 37.4 MB |
| jcode | 40.1 MB | 41.4 MB | 43.1 MB |
| Claude Code | 189.5 MB | 930.4 MB | 1.8 GB |
| Codex CLI | 107.0 MB | 536.1 MB | 1.0 GB |
| OpenCode | 587.6 MB | 593.7 MB | 609.2 MB |
| pi | 148.8 MB | 738.5 MB | 1.4 GB |
| oh-my-pi (omp) | 439.5 MB | 1.8 GB | 3.2 GB |
| oh-my-openagent (omo) | 824.4 MB | 2.9 GB | 3.7 GB |

#### 기동

```text
Benchmarking: /Users/runner/work/agent/agent/bench-bin/horon-agent
============================================================

--help (binary load):
  Min:    6.52 ms
  Max:    9.05 ms
  Mean:   7.18 ms
  Median: 6.99 ms
  Stdev:  0.70 ms

--version:
  Min:    6.28 ms
  Max:    7.59 ms
  Mean:   6.83 ms
  Median: 6.77 ms
  Stdev:  0.42 ms

Measuring isolated server startup (10 runs)...

Server ready (isolated socket connectable):
  Min:    34.77 ms
  Max:    162.30 ms
  Mean:   103.57 ms
  Median: 116.12 ms
  Stdev:  49.90 ms

Measuring harness bridge readiness (10 runs)...

Bridge stage: api socket connectable:
  Min:    23.83 ms
  Max:    159.68 ms
  Mean:   103.77 ms
  Median: 118.54 ms
  Stdev:  45.03 ms

Bridge stage: hello_ok:
  Min:    24.40 ms
  Max:    160.21 ms
  Mean:   104.67 ms
  Median: 119.11 ms
  Stdev:  44.90 ms

Bridge stage: no-op reply:
  Min:    25.09 ms
  Max:    160.62 ms
  Mean:   105.41 ms
  Median: 119.57 ms
  Stdev:  44.76 ms

Bridge total (socket -> no-op):
  Min:    25.09 ms
  Max:    160.62 ms
  Mean:   105.41 ms
  Median: 119.57 ms
  Stdev:  44.76 ms

============================================================
Summary:
  Binary load median:      ~7.0 ms
  Server ready median:     ~116.1 ms
  Bridge total median:     ~119.6 ms
```

#### 세션 로딩 (COLD)

```text
binary   /Users/runner/work/agent/agent/bench-bin/examples/loading_probe (dac201376798)
mode     cold
protocol COLD: every measured run gets its own freshly copied HOME; no derived cache from an earlier run exists, so each sample is a first load of the fixture.
fixtures /var/folders/h5/t85nd63d71x27_8gl5gw3l_w0000gn/T/horon-loading-bench-8b45if6q
note     --warmup 3 is ignored in cold mode: every sample gets a fresh HOME
fixture events=0 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    0.028/0.026    p95    0.067/0.064    delta  -0.003 ms  control-no-change
  skill_request_overlay p50    0.224/0.214    p95    0.576/0.433    delta  -0.010 ms  control-no-change
  mcp_config_load   p50    0.273/0.249    p95    0.551/0.510    delta  -0.024 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.001/0.001    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  session_save      p50   21.955/21.064   p95  164.064/35.678   delta  -0.890 ms  control-no-change
  total store_read_bytes p50 23249  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 23229..23261 (7 distinct values)
  SPREAD   b session_save store_read_bytes 23237..23257 (5 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    4.884/5.012    p95    7.011/27.213   delta  +0.129 ms  control-no-change
  skill_request_overlay p50    6.765/6.593    p95   12.423/16.789   delta  -0.172 ms  control-no-change
  mcp_config_load   p50    0.298/0.296    p95    0.611/0.687    delta  -0.002 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.011    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.001/0.001    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.001    delta  +0.000 ms  control-no-change
  session_save      p50   24.100/25.283   p95   31.830/34.080   delta  +1.183 ms  control-no-change
  total store_read_bytes p50 23249  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 23245..23261 (5 distinct values)
  SPREAD   b session_save store_read_bytes 23237..23261 (7 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.025/0.027    p95    0.031/0.029    delta  +0.002 ms  control-no-change
  skill_request_overlay p50    0.240/0.221    p95    0.301/0.249    delta  -0.019 ms  control-no-change
  mcp_config_load   p50    0.264/0.265    p95    0.319/0.387    delta  +0.001 ms  control-no-change
  attach_history    p50  303.878/287.759  p95  373.198/381.673  delta -16.119 ms  control-no-change
  resume_ready      p50   63.881/61.154   p95   74.535/84.572   delta  -2.728 ms  control-no-change
  provider_dispatch p50    1.068/1.044    p95    1.199/1.179    delta  -0.024 ms  control-no-change
  session_save      p50   20.208/17.238   p95   25.439/45.447   delta  -2.969 ms  control-no-change
  total store_read_bytes p50 5311613  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 23255..23267 (4 distinct values)
  SPREAD   b attach_history store_read_bytes 4189203..4189207 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099141..1099145 (3 distinct values)
  SPREAD   b session_save store_read_bytes 23243..23271 (9 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    4.389/4.552    p95    9.309/8.685    delta  +0.163 ms  control-no-change
  skill_request_overlay p50    5.834/5.826    p95    9.985/9.140    delta  -0.008 ms  control-no-change
  mcp_config_load   p50    0.282/0.288    p95    0.534/0.381    delta  +0.006 ms  control-no-change
  attach_history    p50  359.885/342.474  p95  455.813/421.271  delta -17.410 ms  control-no-change
  resume_ready      p50   77.937/67.182   p95  110.413/119.280  delta -10.755 ms  control-no-change
  provider_dispatch p50    1.154/1.090    p95    2.192/2.737    delta  -0.064 ms  control-no-change
  session_save      p50   29.864/24.965   p95  143.111/52.014   delta  -4.899 ms  control-no-change
  total store_read_bytes p50 5311608  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189201..4189205 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099142..1099144 (2 distinct values)
  SPREAD   a session_save store_read_bytes 23246..23266 (7 distinct values)
  SPREAD   b attach_history store_read_bytes 4189203..4189205 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099143..1099144 (2 distinct values)
  SPREAD   b session_save store_read_bytes 23246..23274 (8 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.029/0.030    p95    0.038/0.080    delta  +0.001 ms  control-no-change
  skill_request_overlay p50    0.218/0.231    p95    0.307/0.364    delta  +0.014 ms  control-no-change
  mcp_config_load   p50    0.263/0.266    p95    0.367/0.367    delta  +0.003 ms  control-no-change
  attach_history    p50 3401.532/3676.271 p95 5360.474/5516.446 delta +274.738 ms  control-no-change
  resume_ready      p50  801.348/773.378  p95 1191.609/1409.716 delta -27.971 ms  control-no-change
  provider_dispatch p50   10.893/10.903   p95   15.451/18.551   delta  +0.011 ms  control-no-change
  session_save      p50   25.094/24.605   p95   67.170/86.931   delta  -0.489 ms  control-no-change
  total store_read_bytes p50 53417816  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184297..42184305 (4 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210247..11210255 (4 distinct values)
  SPREAD   a session_save store_read_bytes 23250..23270 (6 distinct values)
  SPREAD   b attach_history store_read_bytes 42184303..42184305 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210254..11210255 (2 distinct values)
  SPREAD   b session_save store_read_bytes 23257..23278 (6 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    4.707/4.438    p95    6.256/8.009    delta  -0.269 ms  control-no-change
  skill_request_overlay p50    5.980/6.359    p95    8.427/11.117   delta  +0.379 ms  control-no-change
  mcp_config_load   p50    0.290/0.286    p95    0.647/0.566    delta  -0.004 ms  control-no-change
  attach_history    p50 3492.955/3627.234 p95 5892.434/4493.401 delta +134.279 ms  control-no-change
  resume_ready      p50  833.897/837.080  p95  994.346/927.835  delta  +3.184 ms  control-no-change
  provider_dispatch p50   11.805/12.189   p95   35.043/16.125   delta  +0.384 ms  control-no-change
  session_save      p50   30.107/25.356   p95  133.710/69.847   delta  -4.752 ms  control-no-change
  total store_read_bytes p50 53417826  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184303..42184305 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210254..11210255 (2 distinct values)
  SPREAD   a session_save store_read_bytes 23254..23274 (7 distinct values)
  SPREAD   b attach_history store_read_bytes 42184299..42184305 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210250..11210255 (3 distinct values)
  SPREAD   b session_save store_read_bytes 23254..23270 (7 distinct values)
report   bench-results/loading-cold.json
mode cold, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### 세션 로딩 (WARM)

```text
binary   /Users/runner/work/agent/agent/bench-bin/examples/loading_probe (dac201376798)
mode     warm
protocol WARM: one HOME per side, --warmup discarded runs per side, then measured runs alternate between the two sides on those same HOMEs.
fixtures /var/folders/h5/t85nd63d71x27_8gl5gw3l_w0000gn/T/horon-loading-bench-3ithjvn2
fixture events=0 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    0.024/0.025    p95    0.038/0.042    delta  +0.001 ms  control-no-change
  skill_request_overlay p50    0.218/0.223    p95    0.237/0.242    delta  +0.005 ms  control-no-change
  mcp_config_load   p50    0.257/0.254    p95    0.306/0.295    delta  -0.003 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.001/0.000    delta  -0.000 ms  control-no-change
  session_save      p50   14.042/14.097   p95   15.708/23.767   delta  +0.055 ms  control-no-change
  total store_read_bytes p50 23974  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23250, 23254, 23968, 23980, 24672, 24674, 24678, 24694]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23254, 23258, 23262, 23960, 23972, 23976, 24672, 24674, 24678, 24682]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23250..24694 (8 distinct values)
  SPREAD   b session_save store_read_bytes 23254..24682 (10 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    3.870/3.917    p95    8.528/4.224    delta  +0.047 ms  control-no-change
  skill_request_overlay p50    5.087/5.004    p95    5.497/5.114    delta  -0.083 ms  control-no-change
  mcp_config_load   p50    0.269/0.260    p95    0.291/0.281    delta  -0.009 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  session_save      p50   14.220/13.860   p95   26.237/203.656  delta  -0.360 ms  control-no-change
  total store_read_bytes p50 23966  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23242, 23250, 23262, 23956, 23964, 23968, 24674, 24678, 24682]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23252, 23258, 23266, 23948, 23964, 23970, 24672, 24674, 24676, 24686]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23242..24682 (9 distinct values)
  SPREAD   b session_save store_read_bytes 23252..24686 (10 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.022/0.023    p95    0.026/0.025    delta  +0.000 ms  control-no-change
  skill_request_overlay p50    0.201/0.195    p95    0.243/0.243    delta  -0.006 ms  control-no-change
  mcp_config_load   p50    0.234/0.226    p95    0.311/0.271    delta  -0.008 ms  control-no-change
  attach_history    p50   84.622/84.822   p95   93.899/91.057   delta  +0.200 ms  control-no-change
  resume_ready      p50   59.862/57.566   p95   66.561/62.574   delta  -2.295 ms  control-no-change
  provider_dispatch p50    1.032/1.035    p95    1.468/1.242    delta  +0.003 ms  control-no-change
  session_save      p50   15.744/13.633   p95   42.388/15.419   delta  -2.111 ms  control-no-change
  total store_read_bytes p50 2841562  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23975, 23977, 24683, 24685, 25403, 25405, 26105, 26111, 26821, 26831]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23951, 23981, 24691, 24693, 25393, 25401, 26111, 26823, 26825]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23975..26831 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23951..26825 (9 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    3.358/3.358    p95    3.940/3.768    delta  +0.001 ms  control-no-change
  skill_request_overlay p50    4.322/4.304    p95    5.042/5.007    delta  -0.019 ms  control-no-change
  mcp_config_load   p50    0.231/0.226    p95    0.269/0.246    delta  -0.006 ms  control-no-change
  attach_history    p50   83.136/83.129   p95   91.137/93.645   delta  -0.008 ms  control-no-change
  resume_ready      p50   50.827/51.252   p95   54.097/56.334   delta  +0.426 ms  control-no-change
  provider_dispatch p50    0.920/0.930    p95    0.986/1.010    delta  +0.009 ms  control-no-change
  session_save      p50   11.754/12.395   p95   13.198/13.821   delta  +0.641 ms  control-no-change
  total store_read_bytes p50 2841559  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23959, 23981, 24685, 24687, 25397, 25405, 26105, 26123, 26823, 26829]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23957, 23981, 24687, 25393, 25401, 26105, 26123, 26825, 26827]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23959..26829 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23957..26827 (9 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.023/0.023    p95    0.025/0.039    delta  +0.001 ms  control-no-change
  skill_request_overlay p50    0.201/0.200    p95    0.289/0.215    delta  -0.001 ms  control-no-change
  mcp_config_load   p50    0.237/0.233    p95    0.313/0.265    delta  -0.004 ms  control-no-change
  attach_history    p50  920.817/890.779  p95  993.640/994.331  delta -30.038 ms  control-no-change
  resume_ready      p50  549.099/549.209  p95  595.643/578.920  delta  +0.110 ms  control-no-change
  provider_dispatch p50    8.625/8.573    p95   12.133/8.920    delta  -0.052 ms  control-no-change
  session_save      p50   12.215/12.082   p95   16.500/14.969   delta  -0.134 ms  control-no-change
  total store_read_bytes p50 28640578  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23972, 23978, 24680, 24706, 25402, 25404, 26120, 26122, 26832, 26838]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23974, 23976, 24686, 24688, 25400, 25416, 26122, 26824, 26830]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23972..26838 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23974..26830 (9 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    3.550/3.501    p95    4.164/3.848    delta  -0.049 ms  control-no-change
  skill_request_overlay p50    4.546/4.576    p95    4.983/4.845    delta  +0.029 ms  control-no-change
  mcp_config_load   p50    0.237/0.238    p95    0.251/0.251    delta  +0.002 ms  control-no-change
  attach_history    p50  892.210/892.713  p95  921.876/945.539  delta  +0.503 ms  control-no-change
  resume_ready      p50  549.501/551.432  p95  592.694/567.032  delta  +1.931 ms  control-no-change
  provider_dispatch p50    8.646/8.616    p95    9.256/8.829    delta  -0.030 ms  control-no-change
  session_save      p50   12.158/12.068   p95   13.555/13.946   delta  -0.090 ms  control-no-change
  total store_read_bytes p50 28640575  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23975, 23979, 24681, 24689, 25395, 25409, 26111, 26117, 26827, 26833]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23980, 23988, 24678, 24698, 25400, 25408, 26112, 26126, 26834, 26836]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23975..26833 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23980..26836 (10 distinct values)
report   bench-results/loading-warm.json
mode warm, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### Tool latency

| Tool case | median | p95 | min | max | samples |
|---|---|---|---|---|---|
| read (tiny file) | 0.2 ms | 0.2 ms | 0.2 ms | 0.2 ms | 5 |
| read (1000 lines) | 0.4 ms | 0.5 ms | 0.4 ms | 0.5 ms | 5 |
| read (10 lines @ offset) | 0.5 ms | 0.5 ms | 0.4 ms | 0.5 ms | 5 |
| read (repo Cargo.toml) | 0.3 ms | 0.3 ms | 0.2 ms | 0.3 ms | 5 |
| write (small) | 1.5 ms | 2.8 ms | 1.4 ms | 2.8 ms | 5 |
| write (10KB) | 2.0 ms | 2.6 ms | 2.0 ms | 2.6 ms | 5 |
| edit (simple replace) | 1.6 ms | 1.9 ms | 1.5 ms | 1.9 ms | 5 |
| ls (scripts dir) | 0.8 ms | 0.8 ms | 0.7 ms | 0.8 ms | 5 |
| agentgrep (grep in tree) | 2.5 ms | 2.6 ms | 2.3 ms | 2.6 ms | 5 |
| agentgrep (find **/*.rs) | 1.4 ms | 1.5 ms | 1.4 ms | 1.5 ms | 5 |
| bash (true) | 3.5 ms | 3.6 ms | 3.3 ms | 3.6 ms | 5 |
| bash (echo hello) | 3.3 ms | 3.5 ms | 3.1 ms | 3.5 ms | 5 |
| bash (ls fixture tree) | 5.7 ms | 5.9 ms | 5.4 ms | 5.9 ms | 5 |
| invalid tool (dispatch only) | 0.1 ms | 0.1 ms | 0.0 ms | 0.1 ms | 5 |

#### Reload storm

1 resume(s) per level; each level resumes sessions no other level has opened, from a freshly started daemon.

| Concurrent resumes | history median | p95 | max | storm wall | failures |
|---|---|---|---|---|---|
| 1 | 78.6 ms | 78.6 ms | 78.6 ms | 78.7 ms | 0 |
| 10 | 207.2 ms | 213.2 ms | 213.2 ms | 213.4 ms | 0 |
| 40 | 1.06 s | 1.17 s | 1.17 s | 1.17 s | 0 |

#### Compile loop

| Checkpoint | Touched | median | min | max | samples |
|---|---|---|---|---|---|
| warm_noop | — | 2.08 s | 2.08 s | 2.08 s | 1 |
| leaf_touch | src/main.rs | 981.9 ms | 981.9 ms | 981.9 ms | 1 |
| core_touch | crates/app-core/src/lib.rs | 32.87 s | 32.87 s | 32.87 s | 1 |
| build_meta_touch | crates/build-meta/src/lib.rs | 66.69 s | 66.69 s | 66.69 s | 1 |

### Windows

#### Coding agent comparison (headless, mock model)

Host: windows/AMD64, 4 CPUs, Python 3.12.10. Baseline: **horon**.

Skipped: syscalls (strace not available)

| Tool | Version | `--version` (median) | Install size |
|---|---|---|---|
| horon | horon-agent v0.1.0-dev (a21f3c3) | 14.2 ms | 92.6 MB |
| jcode | jcode v0.99.1 (7206efd0c) | 16.0 ms | 128.4 MB |
| Claude Code | 2.1.317 (Claude Code) | 42.9 ms | 645.2 MB |
| Codex CLI | codex-cli 0.166.0 | 114.1 ms | 479.4 MB |
| OpenCode | 1.19.5 | 625.9 ms | 605.3 MB |
| pi | 1.5.0 | 308.9 ms | 108.9 MB |
| oh-my-pi (omp) | omp/18.18.0 | 804.1 ms | 291.2 MB |
| oh-my-openagent (omo) | omo 5.1.64 (engine: senpi 2026.11.5-1) | 214.2 ms | 514.8 MB |

**One-shot headless prompt** (includes startup, one `ls` tool call, final answer)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Tool round-trip | Note |
|---|---|---|---|---|---|---|---|
| horon | 840.7 ms | 653.2 ms–2.44 s | baseline | 36.1 MB | baseline | yes | — |
| jcode | 165.2 ms | 151.4 ms–271.4 ms | 5.09× less | 25.2 MB | 1.43× less | yes | — |
| Claude Code | 1.48 s | 1.44 s–2.23 s | 1.76× more | 273.2 MB | 7.57× more | yes | — |
| Codex CLI | 1.64 s | 1.46 s–2.11 s | 1.95× more | 266.0 MB | 7.37× more | yes | — |
| OpenCode | 5.41 s | 5.38 s–8.48 s | 6.43× more | 896.2 MB | 24.83× more | yes | — |
| pi | 548.7 ms | 538.4 ms–621.4 ms | 1.53× less | 121.3 MB | 3.36× more | yes | — |
| oh-my-pi (omp) | 3.95 s | 3.08 s–4.21 s | 4.70× more | 394.6 MB | 10.93× more | yes | — |
| oh-my-openagent (omo) | 21.33 s | 19.71 s–21.83 s | 25.37× more | 549.9 MB | 15.23× more | yes | — |

**Skill invocation** (process start to final answer, including skill discovery and one load of the fixture skill)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | Note |
|---|---|---|---|---|---|---|---|---|
| horon | 268.4 ms | 256.5 ms–273.9 ms | baseline | 24.7 MB | baseline | skill-uri | yes | — |
| jcode | 219.6 ms | 215.9 ms–220.8 ms | 1.22× less | 24.0 MB | 1.03× less | skill-tool | yes | — |
| Claude Code | 1.04 s | 996.6 ms–1.11 s | 3.88× more | 236.4 MB | 9.56× more | skill-tool | yes | — |
| Codex CLI | 1.89 s | 1.86 s–1.92 s | 7.05× more | 264.8 MB | 10.71× more | file-read | yes | — |
| OpenCode | 8.48 s | 8.22 s–9.09 s | 31.60× more | 765.1 MB | 30.95× more | skill-tool | yes | — |
| pi | 792.3 ms | 714.5 ms–909.4 ms | 2.95× more | 97.7 MB | 3.95× more | file-read | yes | — |
| oh-my-pi (omp) | 3.52 s | 3.49 s–5.13 s | 13.13× more | 388.2 MB | 15.70× more | file-read | yes | — |
| oh-my-openagent (omo) | 21.68 s | 21.65 s–24.42 s | 80.78× more | 529.8 MB | 21.43× more | file-read | yes | — |

**MCP tool call** (process start to final answer, including starting the fixture stdio MCP server and one `bench_echo` call)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | MCP ready | Note |
|---|---|---|---|---|---|---|---|---|---|
| horon | 300.3 ms | 296.9 ms–315.5 ms | baseline | 37.3 MB | baseline | mcp-call | yes | 168.6 ms | — |
| jcode | 346.3 ms | 345.2 ms–355.5 ms | 1.15× more | 36.7 MB | same | mcp-action | yes | 240.1 ms | — |
| Claude Code | 1.25 s | 1.21 s–1.47 s | 4.17× more | 275.5 MB | 7.38× more | mcp-tool | yes | 1.01 s | — |
| Codex CLI | 1.80 s | 876.3 ms–1.83 s | 6.01× more | 164.7 MB | 4.41× more | mcp-tool | yes | 758.9 ms | — |
| OpenCode | 8.33 s | 8.13 s–8.97 s | 27.75× more | 3.1 GB | 85.93× more | mcp-tool | yes | 6.82 s | — |
| pi | 876.2 ms | 848.3 ms–901.8 ms | 2.92× more | 117.9 MB | 3.16× more | mcp-tool | yes | 707.8 ms | — |
| oh-my-pi (omp) | 3.50 s | 3.49 s–3.54 s | 11.67× more | 377.4 MB | 10.11× more | mcp-route | yes | 2.91 s | — |
| oh-my-openagent (omo) | 23.98 s | 22.46 s–25.40 s | 79.84× more | 567.5 MB | 15.20× more | mcp-tool | yes | 20.87 s | — |

**Session memory** (idle after 3 turn(s) per session; sizes are process-tree sums, RSS; PSS also recorded on Linux). "Each additional" is the least-squares slope across the measured session counts; 0 B means growth stayed below RSS sampling noise. Threads and descriptors come from the daemon's own `server:memory` after the turns.

| Tool | First session | Each additional | vs horon (additional) | Threads/session | FDs/session | First (post-turn) | Each additional (post-turn) |
|---|---|---|---|---|---|---|---|
| horon | 22.6 MB | 125.6 KB | baseline | — | — | 29.6 MB | 460.5 KB |
| jcode | 21.9 MB | 71.0 KB | 1.77× less | — | — | 27.0 MB | 332.7 KB |
| Claude Code | 153.6 MB | 155.0 MB | 1,264× more | — | — | 190.8 MB | 189.5 MB |
| Codex CLI | 126.7 MB | 118.0 MB | 962.17× more | — | — | 166.2 MB | 117.1 MB |
| OpenCode | 243.1 MB | 17.4 MB | 141.49× more | — | — | 434.6 MB | 34.4 MB |
| pi | 91.1 MB | 91.0 MB | 741.76× more | — | — | 101.3 MB | 101.1 MB |
| oh-my-pi (omp) | 321.9 MB | 127.9 MB | 1,043× more | — | — | 344.9 MB | 328.6 MB |
| oh-my-openagent (omo) | 141.6 MB | 125.2 MB | 1,021× more | — | — | 411.4 MB | 397.8 MB |

Threads/descriptors unavailable: horon (server:memory did not report thread_count, fd_count), jcode (server:memory did not report thread_count, fd_count)

**Post-turn tree RSS per session count**

| Tool | 1 session(s) | 5 session(s) | 10 session(s) |
|---|---|---|---|
| horon | 29.6 MB | 32.4 MB | 33.7 MB |
| jcode | 27.0 MB | 28.4 MB | 29.9 MB |
| Claude Code | 190.8 MB | 943.6 MB | 1.9 GB |
| Codex CLI | 166.2 MB | 656.6 MB | 1.2 GB |
| OpenCode | 434.6 MB | 485.0 MB | 737.9 MB |
| pi | 101.3 MB | 504.2 MB | 1011.2 MB |
| oh-my-pi (omp) | 344.9 MB | 1.6 GB | 3.2 GB |
| oh-my-openagent (omo) | 411.4 MB | 2.2 GB | 3.9 GB |

#### 기동

```text
Benchmarking: D:/a/agent/agent/bench-bin/horon-agent.exe
============================================================

--help (binary load):
  Min:    14.17 ms
  Max:    16.10 ms
  Mean:   14.91 ms
  Median: 14.87 ms
  Stdev:  0.55 ms

--version:
  Min:    14.09 ms
  Max:    17.29 ms
  Mean:   14.83 ms
  Median: 14.44 ms
  Stdev:  0.96 ms

Measuring isolated server startup (10 runs)...

Server ready (isolated socket connectable):
  Min:    22.43 ms
  Max:    28.37 ms
  Mean:   23.37 ms
  Median: 22.92 ms
  Stdev:  1.77 ms

Measuring harness bridge readiness (10 runs)...
  skipped: api-bridge is Unix-only, so Windows has no harness bridge to time

Bridge stage: api socket connectable: No successful runs

Bridge stage: hello_ok: No successful runs

Bridge stage: no-op reply: No successful runs

Bridge total (socket -> no-op): No successful runs

============================================================
Summary:
  Binary load median:      ~14.9 ms
  Server ready median:     ~22.9 ms
  Bridge total median:     n/a
```

#### 세션 로딩 (COLD)

```text
binary   D:\a\agent\agent\bench-bin\examples\loading_probe.exe (4eb852ac6b21)
mode     cold
protocol COLD: every measured run gets its own freshly copied HOME; no derived cache from an earlier run exists, so each sample is a first load of the fixture.
fixtures C:\Users\RUNNER~1\AppData\Local\Temp\horon-loading-bench-4ik6wmip
note     --warmup 3 is ignored in cold mode: every sample gets a fresh HOME
fixture events=0 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    0.158/0.164    p95    0.194/0.201    delta  +0.005 ms  control-no-change
  skill_request_overlay p50    1.212/1.222    p95    1.253/1.287    delta  +0.010 ms  control-no-change
  mcp_config_load   p50    1.412/1.423    p95    1.466/1.505    delta  +0.011 ms  control-no-change
  attach_history    p50    0.001/0.001    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.001/0.000    delta  +0.000 ms  control-no-change
  session_save      p50   67.920/69.086   p95   76.899/72.255   delta  +1.166 ms  control-no-change
  total store_read_bytes p50 23256  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 23228..23276 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23219..23276 (8 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50   24.185/24.416   p95   26.340/34.397   delta  +0.231 ms  control-no-change
  skill_request_overlay p50   55.295/55.057   p95   62.878/57.601   delta  -0.238 ms  control-no-change
  mcp_config_load   p50    1.469/1.480    p95    1.887/1.552    delta  +0.011 ms  control-no-change
  attach_history    p50    0.001/0.001    p95    0.003/0.008    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.001/0.000    p95    0.001/0.001    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  session_save      p50   77.649/71.081   p95   85.835/87.863   delta  -6.568 ms  control-no-change
  total store_read_bytes p50 23266  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 23207..23276 (8 distinct values)
  SPREAD   b session_save store_read_bytes 23234..23276 (8 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.155/0.153    p95    0.200/0.189    delta  -0.002 ms  control-no-change
  skill_request_overlay p50    1.236/1.222    p95    1.284/1.268    delta  -0.014 ms  control-no-change
  mcp_config_load   p50    1.422/1.435    p95    1.504/1.518    delta  +0.013 ms  control-no-change
  attach_history    p50  570.728/564.014  p95  601.390/572.917  delta  -6.713 ms  control-no-change
  resume_ready      p50  229.304/227.969  p95  238.309/234.160  delta  -1.335 ms  control-no-change
  provider_dispatch p50    1.866/1.850    p95    1.904/2.379    delta  -0.017 ms  control-no-change
  session_save      p50   64.935/64.737   p95   70.242/83.807   delta  -0.198 ms  control-no-change
  total store_read_bytes p50 5311525  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189151..4189153 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099104..1099105 (2 distinct values)
  SPREAD   a session_save store_read_bytes 23251..23283 (8 distinct values)
  SPREAD   b attach_history store_read_bytes 4189149..4189153 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099101..1099105 (3 distinct values)
  SPREAD   b session_save store_read_bytes 23236..23291 (9 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50   24.841/23.132   p95   29.146/24.006   delta  -1.709 ms  control-no-change
  skill_request_overlay p50   56.175/56.231   p95   59.111/58.443   delta  +0.056 ms  control-no-change
  mcp_config_load   p50    1.500/1.531    p95    1.564/1.598    delta  +0.030 ms  control-no-change
  attach_history    p50  576.894/568.600  p95  598.305/689.096  delta  -8.294 ms  control-no-change
  resume_ready      p50  228.746/235.955  p95  234.601/242.066  delta  +7.208 ms  control-drift
  provider_dispatch p50    1.749/1.787    p95    1.842/1.865    delta  +0.039 ms  control-no-change
  session_save      p50   65.960/65.893   p95   74.171/127.105  delta  -0.068 ms  control-no-change
  total store_read_bytes p50 5311521  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189151..4189155 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099102..1099106 (3 distinct values)
  SPREAD   a session_save store_read_bytes 23248..23284 (7 distinct values)
  SPREAD   b attach_history store_read_bytes 4189153..4189155 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099105..1099106 (2 distinct values)
  SPREAD   b session_save store_read_bytes 23251..23292 (8 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.161/0.163    p95    0.255/0.205    delta  +0.002 ms  control-no-change
  skill_request_overlay p50    1.251/1.238    p95    1.774/1.664    delta  -0.014 ms  control-no-change
  mcp_config_load   p50    1.479/1.451    p95    1.549/2.233    delta  -0.028 ms  control-no-change
  attach_history    p50 6430.024/6435.586 p95 7367.224/7288.418 delta  +5.561 ms  control-no-change
  resume_ready      p50 2411.033/2392.342 p95 2673.015/2715.510 delta -18.691 ms  control-no-change
  provider_dispatch p50   19.985/19.948   p95   22.837/22.565   delta  -0.038 ms  control-no-change
  session_save      p50   64.324/61.843   p95  149.011/261.215  delta  -2.481 ms  control-no-change
  total store_read_bytes p50 53417730  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184250..42184253 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210214..11210216 (3 distinct values)
  SPREAD   a session_save store_read_bytes 23236..23287 (10 distinct values)
  SPREAD   b attach_history store_read_bytes 42184249..42184253 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210212..11210216 (2 distinct values)
  SPREAD   b session_save store_read_bytes 23181..23295 (8 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50   23.720/23.655   p95   24.475/26.896   delta  -0.065 ms  control-no-change
  skill_request_overlay p50   55.376/55.856   p95   59.963/65.906   delta  +0.481 ms  control-no-change
  mcp_config_load   p50    1.523/1.497    p95    1.776/1.650    delta  -0.026 ms  control-no-change
  attach_history    p50 6393.968/6395.444 p95 6683.140/6485.365 delta  +1.476 ms  control-no-change
  resume_ready      p50 2378.952/2366.827 p95 2645.317/2389.831 delta -12.126 ms  control-no-change
  provider_dispatch p50   19.333/19.169   p95   22.218/20.583   delta  -0.164 ms  control-no-change
  session_save      p50   63.498/63.529   p95  130.767/69.626   delta  +0.031 ms  control-no-change
  total store_read_bytes p50 53417738  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184249..42184253 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210212..11210216 (3 distinct values)
  SPREAD   a session_save store_read_bytes 23219..23292 (10 distinct values)
  SPREAD   b attach_history store_read_bytes 42184245..42184253 (4 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210208..11210216 (4 distinct values)
  SPREAD   b session_save store_read_bytes 23243..23291 (8 distinct values)
report   bench-results\loading-cold.json
mode cold, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### 세션 로딩 (WARM)

```text
binary   D:\a\agent\agent\bench-bin\examples\loading_probe.exe (4eb852ac6b21)
mode     warm
protocol WARM: one HOME per side, --warmup discarded runs per side, then measured runs alternate between the two sides on those same HOMEs.
fixtures C:\Users\RUNNER~1\AppData\Local\Temp\horon-loading-bench-5uxh3fiv
fixture events=0 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    0.153/0.154    p95    0.192/0.189    delta  +0.001 ms  control-no-change
  skill_request_overlay p50    1.199/1.216    p95    1.468/1.260    delta  +0.017 ms  control-no-change
  mcp_config_load   p50    1.382/1.384    p95    1.798/1.605    delta  +0.001 ms  control-no-change
  attach_history    p50    0.001/0.001    p95    0.004/0.001    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.001/0.001    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  session_save      p50   50.518/53.191   p95   61.367/57.482   delta  +2.673 ms  control-no-change
  total store_read_bytes p50 23962  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23257, 23263, 23271, 23924, 23942, 23982, 24617, 24643, 24657, 24671]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23241, 23255, 23257, 23922, 23952, 23972, 24641, 24668, 24675]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23257..24671 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23241..24675 (9 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50   23.923/24.465   p95   24.735/25.256   delta  +0.542 ms  control-no-change
  skill_request_overlay p50   53.476/53.234   p95   64.460/59.866   delta  -0.242 ms  control-no-change
  mcp_config_load   p50    1.405/1.488    p95    1.511/2.907    delta  +0.083 ms  control-no-change
  attach_history    p50    0.001/0.001    p95    0.008/0.001    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  session_save      p50   54.066/56.660   p95   67.138/59.564   delta  +2.595 ms  control-no-change
  total store_read_bytes p50 23934  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23241, 23244, 23247, 23253, 23932, 23936, 23964, 24642, 24651, 24675]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23243, 23262, 23273, 23278, 23915, 23922, 23941, 23969, 24656, 24667]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23241..24675 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23243..24667 (10 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.150/0.148    p95    0.155/0.180    delta  -0.003 ms  control-no-change
  skill_request_overlay p50    1.236/1.203    p95    1.425/1.320    delta  -0.033 ms  control-no-change
  mcp_config_load   p50    1.420/1.381    p95    1.446/1.433    delta  -0.039 ms  control-no-change
  attach_history    p50  295.545/290.187  p95  300.045/299.921  delta  -5.358 ms  control-no-change
  resume_ready      p50  232.676/232.077  p95  238.289/236.924  delta  -0.599 ms  control-no-change
  provider_dispatch p50    1.787/1.772    p95    1.879/1.906    delta  -0.014 ms  control-no-change
  session_save      p50   50.039/50.616   p95   51.917/52.786   delta  +0.577 ms  control-no-change
  total store_read_bytes p50 2841441  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23947, 23981, 24632, 24676, 25361, 26044, 26060, 26745, 26753]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23947, 23969, 24676, 24678, 25343, 25357, 26000, 26074, 26721, 26777]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23947..26753 (9 distinct values)
  SPREAD   b session_save store_read_bytes 23947..26777 (10 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50   24.784/24.605   p95   26.360/26.368   delta  -0.179 ms  control-no-change
  skill_request_overlay p50   53.854/54.019   p95   56.750/59.908   delta  +0.166 ms  control-no-change
  mcp_config_load   p50    1.424/1.439    p95    1.475/1.819    delta  +0.015 ms  control-no-change
  attach_history    p50  290.129/293.654  p95  298.336/295.943  delta  +3.524 ms  control-no-change
  resume_ready      p50  231.739/231.196  p95  239.069/238.633  delta  -0.543 ms  control-no-change
  provider_dispatch p50    1.788/1.824    p95    3.159/1.910    delta  +0.036 ms  control-no-change
  session_save      p50   55.545/50.363   p95   69.665/53.894   delta  -5.181 ms  control-no-change
  total store_read_bytes p50 2841436  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23911, 23975, 24654, 24660, 25354, 25357, 26056, 26077, 26747, 26760]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23947, 23983, 24646, 24656, 25323, 25361, 26018, 26068, 26775, 26781]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23911..26760 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23947..26781 (10 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.150/0.154    p95    0.167/0.231    delta  +0.004 ms  control-no-change
  skill_request_overlay p50    1.216/1.167    p95    1.578/1.254    delta  -0.048 ms  control-no-change
  mcp_config_load   p50    1.394/1.388    p95    1.739/1.506    delta  -0.005 ms  control-no-change
  attach_history    p50 3060.388/3044.174 p95 3091.414/3082.499 delta -16.214 ms  control-no-change
  resume_ready      p50 2407.143/2414.261 p95 2470.850/2443.398 delta  +7.118 ms  control-no-change
  provider_dispatch p50   19.233/19.272   p95   20.052/19.855   delta  +0.039 ms  control-no-change
  session_save      p50   51.692/51.633   p95   60.521/54.368   delta  -0.060 ms  control-no-change
  total store_read_bytes p50 28640423  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23956, 23964, 24680, 24683, 25326, 25334, 26057, 26067, 26742, 26772]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23966, 23980, 24653, 24685, 25352, 25386, 26069, 26071, 26772, 26776]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23956..26772 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23966..26776 (10 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50   24.754/24.892   p95   26.423/26.332   delta  +0.138 ms  control-no-change
  skill_request_overlay p50   54.254/54.339   p95   59.359/57.757   delta  +0.085 ms  control-no-change
  mcp_config_load   p50    1.443/1.465    p95    1.652/1.507    delta  +0.022 ms  control-no-change
  attach_history    p50 3039.300/3031.853 p95 3058.208/3052.629 delta  -7.447 ms  control-no-change
  resume_ready      p50 2406.568/2398.711 p95 2430.368/2432.733 delta  -7.856 ms  control-no-change
  provider_dispatch p50   18.873/18.898   p95   19.937/19.545   delta  +0.025 ms  control-no-change
  session_save      p50   52.024/51.475   p95   57.987/56.704   delta  -0.549 ms  control-no-change
  total store_read_bytes p50 28640458  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23962, 23986, 24673, 24677, 25350, 25372, 26057, 26074, 26765, 26772]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23932, 23986, 24637, 24666, 25349, 25382, 26050, 26068, 26715, 26772]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23962..26772 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23932..26772 (10 distinct values)
report   bench-results\loading-warm.json
mode warm, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### Tool latency

| Tool case | median | p95 | min | max | samples |
|---|---|---|---|---|---|
| read (tiny file) | 2.2 ms | 2.7 ms | 2.2 ms | 2.7 ms | 5 |
| read (1000 lines) | 2.7 ms | 2.8 ms | 2.4 ms | 2.8 ms | 5 |
| read (10 lines @ offset) | 2.7 ms | 2.7 ms | 2.2 ms | 2.7 ms | 5 |
| read (repo Cargo.toml) | 2.7 ms | 2.7 ms | 2.2 ms | 2.7 ms | 5 |
| write (small) | 7.9 ms | 9.9 ms | 7.4 ms | 9.9 ms | 5 |
| write (10KB) | 7.5 ms | 8.0 ms | 7.4 ms | 8.0 ms | 5 |
| edit (simple replace) | 7.4 ms | 10.0 ms | 7.4 ms | 10.0 ms | 5 |
| ls (scripts dir) | 2.7 ms | 2.7 ms | 2.2 ms | 2.7 ms | 5 |
| agentgrep (grep in tree) | 7.4 ms | 8.0 ms | 6.9 ms | 8.0 ms | 5 |
| agentgrep (find **/*.rs) | 6.9 ms | 7.4 ms | 5.3 ms | 7.4 ms | 5 |
| bash (true) | 10.4 ms | 10.5 ms | 10.0 ms | 10.5 ms | 5 |
| bash (echo hello) | 10.5 ms | 10.5 ms | 10.0 ms | 10.5 ms | 5 |
| bash (ls fixture tree) | 13.0 ms | 15.2 ms | 12.2 ms | 15.2 ms | 5 |
| invalid tool (dispatch only) | 2.7 ms | 2.7 ms | 2.2 ms | 2.7 ms | 5 |

#### Reload storm

1 resume(s) per level; each level resumes sessions no other level has opened, from a freshly started daemon.

| Concurrent resumes | history median | p95 | max | storm wall | failures |
|---|---|---|---|---|---|
| 1 | 100.6 ms | 100.6 ms | 100.6 ms | 101.4 ms | 0 |
| 10 | 346.4 ms | 348.4 ms | 348.4 ms | 348.8 ms | 0 |
| 40 | 821.3 ms | 1.38 s | 1.38 s | 1.38 s | 0 |

#### Compile loop

| Checkpoint | Touched | median | min | max | samples |
|---|---|---|---|---|---|
| warm_noop | — | 667.6 ms | 667.6 ms | 667.6 ms | 1 |
| leaf_touch | src\main.rs | 916.6 ms | 916.6 ms | 916.6 ms | 1 |
| core_touch | crates\app-core\src\lib.rs | 26.26 s | 26.26 s | 26.26 s | 1 |
| build_meta_touch | crates\build-meta\src\lib.rs | 38.23 s | 38.23 s | 38.23 s | 1 |

<!-- bench-auto:end -->
