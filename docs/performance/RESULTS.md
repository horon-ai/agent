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

## 자동 측정: `main` 최신 측정 커밋

`main` 에 올라간 커밋이 CI 와 구조 성능 게이트를 모두 통과하고 직전 측정 이후 에이전트 코드(`src`, `crates`, `Cargo.toml`, `Cargo.lock`, `.cargo`, `rust-toolchain.toml`, `scripts/perf`)가 바뀌었으면, `.github/workflows/benchmark-docs.yml` 이 Linux·macOS·Windows에서 전체 벤치마크 묶음을 OS마다 따로 돌리고 아래 구간을 `main` 과 `dev` 에서 함께 다시 씁니다. OS마다 절이 하나씩 있습니다. 측정 방법과 묶음별 뜻은 [`BENCHMARK.md`](BENCHMARK.md) 의 "자동 측정"에 있습니다.

<!-- bench-auto:start commit=e76ed22dad283c130adc8b06581c4d64183dce5e -->

이 구간은 `.github/workflows/benchmark-docs.yml` 이 자동으로 다시 씁니다. 손으로 고치지 않습니다.

- 커밋: `e76ed22dad283c130adc8b06581c4d64183dce5e`
- 측정 시각: 2026-11-08 14:50 UTC
- 실행: <https://github.com/horon-ai/agent/actions/runs/42489076386> (원본 JSON 은 OS별 `benchmark-<os>-<run_id>` 아티팩트)

OS마다 다른 GitHub 호스티드 러너에서 따로 잽니다. 기계가 다르므로 OS끼리 절대값을 비교하지 않고, 같은 OS 안의 상대 비교(horon 대비 배수)와 커밋 사이의 추세만 읽습니다.

| OS | 러너 | 바이너리 | 상태 |
| --- | --- | --- | --- |
| Linux | `ubuntu-24.04` | `horon-agent v0.98.1-dev (e76ed22)` | 성공 |
| macOS | `macos-latest` | `horon-agent v0.98.1-dev (e76ed22)` | 성공 |
| Windows | `windows-latest` | `horon-agent v0.98.1-dev (e76ed22)` | 성공 |

### Linux

#### Coding agent comparison (headless, mock model)

Host: linux/x86_64, 4 CPUs, Python 3.12.16. Baseline: **horon**.

| Tool | Version | `--version` (median) | Install size |
|---|---|---|---|
| horon | horon-agent v0.98.1-dev (e76ed22) | 4.0 ms | 106.9 MB |
| jcode | jcode v0.99.1 (7206efd0c) | 9.2 ms | 163.4 MB |
| Claude Code | 2.1.317 (Claude Code) | 7.9 ms | 632.2 MB |
| Codex CLI | codex-cli 0.166.0 | 38.5 ms | 435.0 MB |
| OpenCode | 1.19.5 | 464.3 ms | 613.4 MB |
| pi | 0.73.1 | 680.6 ms | 142.7 MB |
| oh-my-pi (omp) | omp/18.18.0 | 708.5 ms | 412.6 MB |
| oh-my-openagent (omo) | omo 5.1.64 (engine: senpi 2026.11.5-1) | 110.5 ms | 520.6 MB |

**One-shot headless prompt** (includes startup, one `ls` tool call, final answer)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Tool round-trip | fsync calls | Note |
|---|---|---|---|---|---|---|---|---|
| horon | 417.7 ms | 413.4 ms–506.7 ms | baseline | 45.0 MB | baseline | yes | 22 | — |
| jcode | 321.2 ms | 316.2 ms–325.1 ms | 1.30× less | 48.5 MB | 1.08× more | yes | 5 | — |
| Claude Code | 755.4 ms | 749.5 ms–794.0 ms | 1.81× more | 209.5 MB | 4.65× more | yes | 9 | — |
| Codex CLI | 2.58 s | 971.1 ms–2.86 s | 6.19× more | 208.7 MB | 4.64× more | yes | 6 | — |
| OpenCode | 4.98 s | 4.57 s–5.11 s | 11.93× more | 635.3 MB | 14.11× more | yes | 4 | — |
| pi | 1.70 s | 1.67 s–1.74 s | 4.08× more | 164.0 MB | 3.64× more | yes | — | — |
| oh-my-pi (omp) | 3.36 s | 3.29 s–3.62 s | 8.04× more | 418.0 MB | 9.28× more | yes | 68 | — |
| oh-my-openagent (omo) | 16.14 s | 15.99 s–16.70 s | 38.65× more | 663.0 MB | 14.73× more | yes | 36 | — |

**Skill invocation** (process start to final answer, including skill discovery and one load of the fixture skill)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | Note |
|---|---|---|---|---|---|---|---|---|
| horon | 53.8 ms | 53.8 ms–56.2 ms | baseline | 36.8 MB | baseline | skill-uri | yes | — |
| jcode | 49.7 ms | 48.7 ms–50.4 ms | 1.08× less | 41.1 MB | 1.12× more | skill-tool | yes | — |
| Claude Code | 423.1 ms | 419.9 ms–427.4 ms | 7.86× more | 207.8 MB | 5.64× more | skill-tool | yes | — |
| Codex CLI | — | — | — | — | — | file-read | no | failed: the fixture sentinel never came back |
| OpenCode | 3.05 s | 3.03 s–3.06 s | 56.71× more | 604.9 MB | 16.42× more | skill-tool | yes | — |
| pi | 814.2 ms | 807.0 ms–837.3 ms | 15.13× more | 158.0 MB | 4.29× more | file-read | yes | — |
| oh-my-pi (omp) | 1.74 s | 1.72 s–1.77 s | 32.31× more | 339.5 MB | 9.22× more | file-read | yes | — |
| oh-my-openagent (omo) | 6.36 s | 6.23 s–6.48 s | 118.22× more | 663.5 MB | 18.01× more | file-read | yes | — |

**MCP tool call** (process start to final answer, including starting the fixture stdio MCP server and one `bench_echo` call)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | MCP ready | Note |
|---|---|---|---|---|---|---|---|---|---|
| horon | 157.7 ms | 156.2 ms–157.7 ms | baseline | 50.3 MB | baseline | mcp-call | yes | 132.8 ms | — |
| jcode | — | — | — | — | — | — | no | — | failed: no tool that could reach the fixture was offered (one request, no tool call) |
| Claude Code | — | — | — | — | — | — | — | — | skipped: Claude Code 2.1.317 -p did not expose a configured stdio MCP server in any of the |
| Codex CLI | — | — | — | — | — | — | — | — | skipped: codex 0.166.0 lists the server only as the Responses namespace tool mcp__bench; a |
| OpenCode | 2.77 s | 2.75 s–2.80 s | 17.54× more | 565.5 MB | 11.23× more | mcp-tool | yes | 2.55 s | — |
| pi | — | — | — | — | — | — | — | — | skipped: pi has no MCP client; its README points at a user extension |
| oh-my-pi (omp) | 1.97 s | 1.77 s–1.99 s | 12.51× more | 386.1 MB | 7.67× more | mcp-route | yes | 1.68 s | — |
| oh-my-openagent (omo) | 6.47 s | 6.39 s–6.50 s | 41.05× more | 657.6 MB | 13.06× more | mcp-tool | yes | 3.13 s | — |

**Session memory** (idle after 3 turn(s) per session; sizes are process-tree sums, RSS; PSS also recorded on Linux). "Each additional" is the least-squares slope across the measured session counts; 0 B means growth stayed below RSS sampling noise. Threads and descriptors come from the daemon's own `server:memory` after the turns.

| Tool | First session | Each additional | vs horon (additional) | Threads/session | FDs/session | First (post-turn) | Each additional (post-turn) |
|---|---|---|---|---|---|---|---|
| horon | 38.6 MB | 216.4 KB | baseline | 0.00 | 1.00 | 44.6 MB | 508.6 KB |
| jcode | 42.8 MB | 155.2 KB | 1.39× less | 0.22 | — | 46.5 MB | 237.7 KB |
| Claude Code | 150.0 MB | 148.5 MB | 702.50× more | — | — | 185.4 MB | 181.2 MB |
| Codex CLI | 164.8 MB | 130.9 MB | 619.41× more | — | — | 160.3 MB | 162.6 MB |
| OpenCode | 246.8 MB | 831.9 KB | 3.84× more | — | — | 452.6 MB | 10.2 MB |
| pi | 149.3 MB | 107.5 MB | 508.62× more | — | — | 167.9 MB | 167.0 MB |
| oh-my-pi (omp) | 324.7 MB | 56.5 MB | 267.19× more | — | — | 364.6 MB | 358.5 MB |
| oh-my-openagent (omo) | 274.4 MB | 38.6 MB | 182.77× more | — | — | 619.3 MB | 585.8 MB |

Threads/descriptors unavailable: jcode (server:memory did not report fd_count)

**Post-turn tree RSS per session count**

| Tool | 1 session(s) | 5 session(s) | 10 session(s) |
|---|---|---|---|
| horon | 44.6 MB | 47.3 MB | 49.1 MB |
| jcode | 46.5 MB | 47.2 MB | 48.5 MB |
| Claude Code | 185.4 MB | 917.0 MB | 1.8 GB |
| Codex CLI | 160.3 MB | 804.7 MB | 1.6 GB |
| OpenCode | 452.6 MB | 477.6 MB | 543.4 MB |
| pi | 167.9 MB | 825.3 MB | 1.6 GB |
| oh-my-pi (omp) | 364.6 MB | 1.7 GB | 3.5 GB |
| oh-my-openagent (omo) | 619.3 MB | 3.0 GB | 5.8 GB |

#### 기동

```text
Benchmarking: /home/runner/work/agent/agent/bench-bin/horon-agent
============================================================

--help (binary load):
  Min:    3.04 ms
  Max:    3.76 ms
  Mean:   3.28 ms
  Median: 3.16 ms
  Stdev:  0.25 ms

--version:
  Min:    2.69 ms
  Max:    3.02 ms
  Mean:   2.86 ms
  Median: 2.86 ms
  Stdev:  0.11 ms

Measuring isolated server startup (10 runs)...

Server ready (isolated socket connectable):
  Min:    20.50 ms
  Max:    20.71 ms
  Mean:   20.63 ms
  Median: 20.62 ms
  Stdev:  0.06 ms

Measuring harness bridge readiness (10 runs)...

Bridge stage: api socket connectable:
  Min:    20.62 ms
  Max:    20.93 ms
  Mean:   20.80 ms
  Median: 20.84 ms
  Stdev:  0.11 ms

Bridge stage: hello_ok:
  Min:    21.30 ms
  Max:    21.72 ms
  Mean:   21.50 ms
  Median: 21.51 ms
  Stdev:  0.14 ms

Bridge stage: no-op reply:
  Min:    21.74 ms
  Max:    22.14 ms
  Mean:   21.94 ms
  Median: 21.96 ms
  Stdev:  0.15 ms

Bridge total (socket -> no-op):
  Min:    21.74 ms
  Max:    22.14 ms
  Mean:   21.94 ms
  Median: 21.96 ms
  Stdev:  0.15 ms

============================================================
Summary:
  Binary load median:      ~3.2 ms
  Server ready median:     ~20.6 ms
  Bridge total median:     ~22.0 ms
```

#### 세션 로딩 (COLD)

```text
binary   /home/runner/work/agent/agent/bench-bin/examples/loading_probe (6b3cf6e16297)
mode     cold
protocol COLD: every measured run gets its own freshly copied HOME; no derived cache from an earlier run exists, so each sample is a first load of the fixture.
fixtures /tmp/horon-loading-bench-3y2mn2ka
note     --warmup 3 is ignored in cold mode: every sample gets a fresh HOME
fixture events=0 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    0.011/0.011    p95    0.016/0.025    delta  -0.000 ms  control-no-change
  skill_request_overlay p50    0.083/0.083    p95    0.093/0.085    delta  -0.000 ms  control-no-change
  mcp_config_load   p50    0.111/0.112    p95    0.123/0.133    delta  +0.001 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.001/0.000    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  session_save      p50    6.815/6.928    p95  243.440/67.495   delta  +0.113 ms  control-no-change
  total store_read_bytes p50 23388  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 23380..23396 (5 distinct values)
  SPREAD   b session_save store_read_bytes 23372..23392 (5 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    1.880/1.890    p95    1.952/2.018    delta  +0.010 ms  control-no-change
  skill_request_overlay p50    3.349/3.338    p95    3.471/3.521    delta  -0.011 ms  control-no-change
  mcp_config_load   p50    0.112/0.114    p95    0.115/0.121    delta  +0.002 ms  control-no-change
  attach_history    p50    0.001/0.001    p95    0.001/0.001    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.001/0.001    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  session_save      p50   13.946/15.731   p95   35.379/41.551   delta  +1.785 ms  control-no-change
  total store_read_bytes p50 23390  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 23372..23396 (6 distinct values)
  SPREAD   b session_save store_read_bytes 23380..23396 (5 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.014/0.012    p95    0.033/0.016    delta  -0.002 ms  control-no-change
  skill_request_overlay p50    0.086/0.087    p95    0.088/0.089    delta  +0.001 ms  control-no-change
  mcp_config_load   p50    0.115/0.117    p95    0.118/0.130    delta  +0.002 ms  control-no-change
  attach_history    p50  236.634/237.550  p95  244.434/241.661  delta  +0.916 ms  control-no-change
  resume_ready      p50   39.795/39.842   p95   40.539/40.967   delta  +0.046 ms  control-no-change
  provider_dispatch p50    0.995/0.996    p95    1.019/1.035    delta  +0.002 ms  control-no-change
  session_save      p50    6.633/6.643    p95   73.349/15.245   delta  +0.010 ms  control-no-change
  total store_read_bytes p50 5311722  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189191..4189195 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099134..1099136 (2 distinct values)
  SPREAD   a session_save store_read_bytes 23386..23402 (7 distinct values)
  SPREAD   b attach_history store_read_bytes 4189191..4189195 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099132..1099136 (3 distinct values)
  SPREAD   b session_save store_read_bytes 23385..23394 (4 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    1.954/1.922    p95    2.003/3.700    delta  -0.031 ms  control-no-change
  skill_request_overlay p50    3.461/3.450    p95    3.598/5.611    delta  -0.012 ms  control-no-change
  mcp_config_load   p50    0.117/0.114    p95    0.124/0.189    delta  -0.003 ms  control-no-change
  attach_history    p50  230.028/231.616  p95  239.188/320.242  delta  +1.588 ms  control-no-change
  resume_ready      p50   38.624/38.549   p95   41.321/39.362   delta  -0.075 ms  control-no-change
  provider_dispatch p50    0.951/0.953    p95    1.003/0.992    delta  +0.001 ms  control-no-change
  session_save      p50    6.177/6.422    p95   19.457/19.482   delta  +0.245 ms  control-no-change
  total store_read_bytes p50 5311711  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189189..4189193 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099131..1099135 (3 distinct values)
  SPREAD   a session_save store_read_bytes 23377..23393 (5 distinct values)
  SPREAD   b attach_history store_read_bytes 4189189..4189193 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099131..1099135 (4 distinct values)
  SPREAD   b session_save store_read_bytes 23371..23401 (8 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.015/0.013    p95    0.017/0.017    delta  -0.002 ms  control-no-change
  skill_request_overlay p50    0.085/0.083    p95    0.087/0.086    delta  -0.001 ms  control-no-change
  mcp_config_load   p50    0.114/0.112    p95    0.129/0.119    delta  -0.002 ms  control-no-change
  attach_history    p50 2784.469/2809.445 p95 2831.021/2870.211 delta +24.977 ms  control-no-change
  resume_ready      p50  422.319/431.358  p95  456.900/437.191  delta  +9.039 ms  control-no-change
  provider_dispatch p50   10.584/11.101   p95   11.078/11.573   delta  +0.517 ms  control-no-change
  session_save      p50    6.654/6.844    p95   14.858/7.073    delta  +0.190 ms  control-no-change
  total store_read_bytes p50 53417934  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184291..42184293 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210245..11210246 (2 distinct values)
  SPREAD   a session_save store_read_bytes 23377..23405 (7 distinct values)
  SPREAD   b attach_history store_read_bytes 42184289..42184293 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210242..11210246 (3 distinct values)
  SPREAD   b session_save store_read_bytes 23379..23405 (7 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    1.922/1.905    p95    1.964/2.076    delta  -0.017 ms  control-no-change
  skill_request_overlay p50    3.353/3.384    p95    3.462/3.648    delta  +0.031 ms  control-no-change
  mcp_config_load   p50    0.115/0.113    p95    0.123/0.122    delta  -0.002 ms  control-no-change
  attach_history    p50 2799.541/2792.083 p95 2837.839/2847.882 delta  -7.457 ms  control-no-change
  resume_ready      p50  423.088/423.546  p95  445.876/437.232  delta  +0.458 ms  control-no-change
  provider_dispatch p50   10.593/10.733   p95   11.178/11.625   delta  +0.140 ms  control-no-change
  session_save      p50    6.814/6.758    p95    7.421/6.899    delta  -0.056 ms  control-no-change
  total store_read_bytes p50 53417936  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 23381..23401 (5 distinct values)
  SPREAD   b attach_history store_read_bytes 42184289..42184293 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210242..11210246 (2 distinct values)
  SPREAD   b session_save store_read_bytes 23389..23405 (5 distinct values)
report   bench-results/loading-cold.json
mode cold, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### 세션 로딩 (WARM)

```text
binary   /home/runner/work/agent/agent/bench-bin/examples/loading_probe (6b3cf6e16297)
mode     warm
protocol WARM: one HOME per side, --warmup discarded runs per side, then measured runs alternate between the two sides on those same HOMEs.
fixtures /tmp/horon-loading-bench-whpj7j7k
fixture events=0 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    0.015/0.012    p95    0.017/0.016    delta  -0.003 ms  control-no-change
  skill_request_overlay p50    0.088/0.087    p95    0.102/0.098    delta  -0.001 ms  control-no-change
  mcp_config_load   p50    0.118/0.117    p95    0.128/0.137    delta  -0.001 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.001/0.000    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  session_save      p50    6.954/7.097    p95    7.216/19.487   delta  +0.143 ms  control-no-change
  total store_read_bytes p50 24120  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23381, 23385, 24112, 24120, 24823, 24831]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23393, 23397, 24100, 24112, 24827, 24831, 24835]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23381..24831 (6 distinct values)
  SPREAD   b session_save store_read_bytes 23393..24835 (7 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    2.019/2.035    p95    2.481/2.091    delta  +0.015 ms  control-no-change
  skill_request_overlay p50    3.593/3.615    p95    4.405/3.692    delta  +0.021 ms  control-no-change
  mcp_config_load   p50    0.119/0.120    p95    0.146/0.141    delta  +0.000 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  session_save      p50    7.868/7.736    p95   32.092/11.727   delta  -0.133 ms  control-no-change
  total store_read_bytes p50 24106  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23387, 23393, 23395, 23397, 24100, 24112, 24114, 24817, 24831, 24839]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23379, 23385, 23389, 24102, 24112, 24116, 24827, 24833, 24835]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23387..24839 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23379..24835 (9 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.014/0.012    p95    0.017/0.015    delta  -0.003 ms  control-no-change
  skill_request_overlay p50    0.089/0.088    p95    0.103/0.104    delta  -0.001 ms  control-no-change
  mcp_config_load   p50    0.118/0.117    p95    0.123/0.129    delta  -0.000 ms  control-no-change
  attach_history    p50   71.055/71.152   p95   72.453/72.260   delta  +0.097 ms  control-no-change
  resume_ready      p50   39.285/39.364   p95   40.023/39.796   delta  +0.078 ms  control-no-change
  provider_dispatch p50    0.962/0.969    p95    0.988/0.996    delta  +0.007 ms  control-no-change
  session_save      p50    7.155/7.454    p95    7.586/12.126   delta  +0.299 ms  control-no-change
  total store_read_bytes p50 2841690  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [24113, 24125, 24828, 24836, 25547, 25553, 26252, 26270, 26989, 26995]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [24095, 24101, 24822, 24838, 25545, 25551, 26262, 26268, 26983, 26989]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 24113..26995 (10 distinct values)
  SPREAD   b session_save store_read_bytes 24095..26989 (10 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    2.086/2.095    p95    2.142/2.177    delta  +0.009 ms  control-no-change
  skill_request_overlay p50    3.578/3.601    p95    3.642/3.733    delta  +0.023 ms  control-no-change
  mcp_config_load   p50    0.120/0.119    p95    0.129/0.122    delta  -0.001 ms  control-no-change
  attach_history    p50   73.184/72.405   p95   95.585/74.237   delta  -0.779 ms  control-no-change
  resume_ready      p50   40.280/39.770   p95   42.559/40.456   delta  -0.510 ms  control-no-change
  provider_dispatch p50    0.997/0.991    p95    1.053/1.017    delta  -0.005 ms  control-no-change
  session_save      p50    7.827/7.728    p95   10.889/9.792    delta  -0.099 ms  control-no-change
  total store_read_bytes p50 2841679  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [24105, 24117, 24824, 24828, 25543, 25551, 26258, 26270, 26981, 26985]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [24109, 24111, 24818, 24826, 25545, 25549, 26264, 26969, 26987]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 24105..26985 (10 distinct values)
  SPREAD   b session_save store_read_bytes 24109..26987 (9 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.015/0.016    p95    0.019/0.018    delta  +0.001 ms  control-no-change
  skill_request_overlay p50    0.088/0.088    p95    0.090/0.095    delta  +0.000 ms  control-no-change
  mcp_config_load   p50    0.117/0.117    p95    0.140/0.118    delta  -0.000 ms  control-no-change
  attach_history    p50  821.819/824.276  p95  904.356/866.861  delta  +2.457 ms  control-no-change
  resume_ready      p50  441.420/440.002  p95  476.729/477.451  delta  -1.418 ms  control-no-change
  provider_dispatch p50   10.982/10.939   p95   12.391/12.548   delta  -0.043 ms  control-no-change
  session_save      p50    7.422/7.220    p95   18.709/7.387    delta  -0.202 ms  control-no-change
  total store_read_bytes p50 28640711  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [24104, 24120, 24831, 25550, 25558, 26249, 26273, 26996]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [24108, 24110, 24825, 24839, 25546, 25558, 26271, 26273, 26984, 26990]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 24104..26996 (8 distinct values)
  SPREAD   b session_save store_read_bytes 24108..26990 (10 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    2.109/2.093    p95    2.305/2.150    delta  -0.015 ms  control-no-change
  skill_request_overlay p50    3.620/3.605    p95    3.779/3.692    delta  -0.015 ms  control-no-change
  mcp_config_load   p50    0.121/0.122    p95    0.135/0.124    delta  +0.001 ms  control-no-change
  attach_history    p50  868.404/868.181  p95  913.016/885.575  delta  -0.223 ms  control-no-change
  resume_ready      p50  489.228/500.462  p95  505.114/525.480  delta +11.234 ms  control-no-change
  provider_dispatch p50   12.679/12.527   p95   13.298/13.383   delta  -0.152 ms  control-no-change
  session_save      p50    8.248/8.358    p95   13.419/11.754   delta  +0.111 ms  control-no-change
  total store_read_bytes p50 28640703  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [24107, 24113, 24816, 24818, 25541, 25555, 26264, 26266, 26989]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [24111, 24119, 24822, 24834, 25543, 25555, 26258, 26262, 26981, 26989]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 24107..26989 (9 distinct values)
  SPREAD   b session_save store_read_bytes 24111..26989 (10 distinct values)
report   bench-results/loading-warm.json
mode warm, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### Tool latency

| Tool case | median | p95 | min | max | samples |
|---|---|---|---|---|---|
| read (tiny file) | 0.2 ms | 0.3 ms | 0.2 ms | 0.3 ms | 5 |
| read (1000 lines) | 0.3 ms | 0.4 ms | 0.3 ms | 0.4 ms | 5 |
| read (10 lines @ offset) | 0.3 ms | 0.3 ms | 0.2 ms | 0.3 ms | 5 |
| read (repo Cargo.toml) | 0.2 ms | 0.2 ms | 0.2 ms | 0.2 ms | 5 |
| write (small) | 0.9 ms | 1.0 ms | 0.9 ms | 1.0 ms | 5 |
| write (10KB) | 1.0 ms | 1.1 ms | 1.0 ms | 1.1 ms | 5 |
| edit (simple replace) | 0.9 ms | 1.0 ms | 0.9 ms | 1.0 ms | 5 |
| ls (scripts dir) | 0.5 ms | 0.5 ms | 0.5 ms | 0.5 ms | 5 |
| agentgrep (grep in tree) | 1.3 ms | 1.3 ms | 1.2 ms | 1.3 ms | 5 |
| agentgrep (find **/*.rs) | 1.3 ms | 1.4 ms | 1.3 ms | 1.4 ms | 5 |
| bash (true) | 1.5 ms | 1.5 ms | 1.4 ms | 1.5 ms | 5 |
| bash (echo hello) | 1.5 ms | 1.5 ms | 1.5 ms | 1.5 ms | 5 |
| bash (ls fixture tree) | 2.5 ms | 2.5 ms | 2.5 ms | 2.5 ms | 5 |
| invalid tool (dispatch only) | 0.1 ms | 0.1 ms | 0.1 ms | 0.1 ms | 5 |

#### Reload storm

1 resume(s) per level; each level resumes sessions no other level has opened, from a freshly started daemon.

| Concurrent resumes | history median | p95 | max | storm wall | failures |
|---|---|---|---|---|---|
| 1 | 102.7 ms | 102.7 ms | 102.7 ms | 102.9 ms | 0 |
| 10 | 324.1 ms | 384.6 ms | 384.6 ms | 384.9 ms | 0 |
| 40 | 1.38 s | 1.55 s | 1.55 s | 1.55 s | 0 |

#### Compile loop

| Checkpoint | Touched | median | min | max | samples |
|---|---|---|---|---|---|
| warm_noop | — | 240.3 ms | 240.3 ms | 240.3 ms | 1 |
| leaf_touch | src/main.rs | 374.9 ms | 374.9 ms | 374.9 ms | 1 |
| core_touch | crates/app-core/src/lib.rs | 20.44 s | 20.44 s | 20.44 s | 1 |
| build_meta_touch | crates/build-meta/src/lib.rs | 28.79 s | 28.79 s | 28.79 s | 1 |

### macOS

#### Coding agent comparison (headless, mock model)

Host: darwin/arm64, 3 CPUs, Python 3.12.10. Baseline: **horon**.

Skipped: syscalls (strace not available)

| Tool | Version | `--version` (median) | Install size |
|---|---|---|---|
| horon | horon-agent v0.98.1-dev (e76ed22) | 10.4 ms | 92.2 MB |
| jcode | jcode v0.99.1 (7206efd0c) | 16.9 ms | 136.3 MB |
| Claude Code | 2.1.317 (Claude Code) | 24.8 ms | 596.8 MB |
| Codex CLI | codex-cli 0.166.0 | 117.6 ms | 346.5 MB |
| OpenCode | 1.19.5 | 491.9 ms | 344.6 MB |
| pi | 0.73.1 | 708.8 ms | 145.6 MB |
| oh-my-pi (omp) | omp/18.18.0 | 609.8 ms | 273.0 MB |
| oh-my-openagent (omo) | omo 5.1.64 (engine: senpi 2026.11.5-1) | 224.0 ms | 502.2 MB |

**One-shot headless prompt** (includes startup, one `ls` tool call, final answer)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Tool round-trip | Note |
|---|---|---|---|---|---|---|---|
| horon | 197.8 ms | 181.1 ms–545.4 ms | baseline | 29.9 MB | baseline | yes | — |
| jcode | 485.0 ms | 460.4 ms–855.7 ms | 2.45× more | 34.1 MB | 1.14× more | yes | — |
| Claude Code | 611.3 ms | 573.9 ms–891.4 ms | 3.09× more | 199.2 MB | 6.66× more | yes | — |
| Codex CLI | 1.20 s | 843.5 ms–2.19 s | 6.08× more | 137.2 MB | 4.59× more | yes | — |
| OpenCode | 3.52 s | 3.33 s–7.30 s | 17.82× more | 759.2 MB | 25.39× more | yes | — |
| pi | 918.2 ms | 826.6 ms–1.25 s | 4.64× more | 167.6 MB | 5.60× more | yes | — |
| oh-my-pi (omp) | 2.15 s | 1.83 s–3.03 s | 10.87× more | 432.2 MB | 14.45× more | yes | — |
| oh-my-openagent (omo) | 14.10 s | 12.12 s–17.40 s | 71.28× more | 859.5 MB | 28.74× more | yes | — |

**Skill invocation** (process start to final answer, including skill discovery and one load of the fixture skill)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | Note |
|---|---|---|---|---|---|---|---|---|
| horon | 222.9 ms | 160.2 ms–234.4 ms | baseline | 29.2 MB | baseline | skill-uri | yes | — |
| jcode | 321.1 ms | 303.4 ms–351.6 ms | 1.44× more | 36.2 MB | 1.24× more | skill-tool | yes | — |
| Claude Code | 866.4 ms | 639.1 ms–982.5 ms | 3.89× more | 199.4 MB | 6.83× more | skill-tool | yes | — |
| Codex CLI | 422.1 ms | 407.4 ms–489.6 ms | 1.89× more | 144.4 MB | 4.95× more | file-read | yes | — |
| OpenCode | 4.49 s | 4.04 s–4.55 s | 20.17× more | 759.9 MB | 26.02× more | skill-tool | yes | — |
| pi | 957.9 ms | 881.0 ms–1.08 s | 4.30× more | 169.3 MB | 5.80× more | file-read | yes | — |
| oh-my-pi (omp) | 2.32 s | 2.10 s–2.48 s | 10.41× more | 397.4 MB | 13.61× more | file-read | yes | — |
| oh-my-openagent (omo) | 9.40 s | 9.18 s–12.38 s | 42.18× more | 875.8 MB | 29.99× more | file-read | yes | — |

**MCP tool call** (process start to final answer, including starting the fixture stdio MCP server and one `bench_echo` call)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | MCP ready | Note |
|---|---|---|---|---|---|---|---|---|---|
| horon | 249.4 ms | 227.6 ms–347.5 ms | baseline | 20.9 MB | baseline | mcp-call | yes | 193.7 ms | — |
| jcode | — | — | — | — | — | — | no | — | failed: no tool that could reach the fixture was offered (one request, no tool call) |
| Claude Code | — | — | — | — | — | — | — | — | skipped: Claude Code 2.1.317 -p did not expose a configured stdio MCP server in any of the |
| Codex CLI | — | — | — | — | — | — | — | — | skipped: codex 0.166.0 lists the server only as the Responses namespace tool mcp__bench; a |
| OpenCode | 3.81 s | 3.63 s–4.25 s | 15.25× more | 703.0 MB | 33.68× more | mcp-tool | yes | 3.41 s | — |
| pi | — | — | — | — | — | — | — | — | skipped: pi has no MCP client; its README points at a user extension |
| oh-my-pi (omp) | 3.04 s | 2.83 s–3.81 s | 12.20× more | 439.0 MB | 21.03× more | mcp-route | yes | 2.42 s | — |
| oh-my-openagent (omo) | — | — | — | — | — | — | no | — | failed: no tool that could reach the fixture was offered (one request, no tool call) |

**Session memory** (idle after 3 turn(s) per session; sizes are process-tree sums, RSS; PSS also recorded on Linux). "Each additional" is the least-squares slope across the measured session counts; 0 B means growth stayed below RSS sampling noise. Threads and descriptors come from the daemon's own `server:memory` after the turns.

| Tool | First session | Each additional | vs horon (additional) | Threads/session | FDs/session | First (post-turn) | Each additional (post-turn) |
|---|---|---|---|---|---|---|---|
| horon | 29.2 MB | 283.8 KB | baseline | 0.22 | 1.00 | 34.7 MB | 516.2 KB |
| jcode | 36.6 MB | 181.6 KB | 1.56× less | — | — | 40.2 MB | 319.3 KB |
| Claude Code | 159.2 MB | 145.2 MB | 523.85× more | — | — | 188.2 MB | 180.3 MB |
| Codex CLI | 106.4 MB | 79.5 MB | 286.69× more | — | — | 107.4 MB | 107.4 MB |
| OpenCode | 267.2 MB | 94.6 KB | 3.00× less | — | — | 560.5 MB | 7.1 MB |
| pi | 173.0 MB | 118.6 MB | 427.77× more | — | — | 193.6 MB | 188.1 MB |
| oh-my-pi (omp) | 400.9 MB | 134.7 MB | 486.07× more | — | — | 429.0 MB | 323.3 MB |
| oh-my-openagent (omo) | 58.0 MB | 43.9 MB | 158.38× more | — | — | 840.0 MB | 322.8 MB |

Threads/descriptors unavailable: jcode (server:memory did not report thread_count, fd_count)

**Post-turn tree RSS per session count**

| Tool | 1 session(s) | 5 session(s) | 10 session(s) |
|---|---|---|---|
| horon | 34.7 MB | 36.7 MB | 39.3 MB |
| jcode | 40.2 MB | 41.3 MB | 43.0 MB |
| Claude Code | 188.2 MB | 946.7 MB | 1.8 GB |
| Codex CLI | 107.4 MB | 536.5 MB | 1.0 GB |
| OpenCode | 560.5 MB | 592.0 MB | 625.0 MB |
| pi | 193.6 MB | 960.9 MB | 1.8 GB |
| oh-my-pi (omp) | 429.0 MB | 2.1 GB | 3.3 GB |
| oh-my-openagent (omo) | 840.0 MB | 3.1 GB | 3.7 GB |

#### 기동

```text
Benchmarking: /Users/runner/work/agent/agent/bench-bin/horon-agent
============================================================

--help (binary load):
  Min:    14.08 ms
  Max:    29.19 ms
  Mean:   20.41 ms
  Median: 21.21 ms
  Stdev:  4.62 ms

--version:
  Min:    14.61 ms
  Max:    28.96 ms
  Mean:   22.81 ms
  Median: 23.30 ms
  Stdev:  4.76 ms

Measuring isolated server startup (10 runs)...

Server ready (isolated socket connectable):
  Min:    52.44 ms
  Max:    170.49 ms
  Mean:   130.12 ms
  Median: 131.36 ms
  Stdev:  38.28 ms

Measuring harness bridge readiness (10 runs)...

Bridge stage: api socket connectable:
  Min:    43.50 ms
  Max:    177.22 ms
  Mean:   121.91 ms
  Median: 140.04 ms
  Stdev:  54.38 ms

Bridge stage: hello_ok:
  Min:    45.23 ms
  Max:    178.59 ms
  Mean:   123.77 ms
  Median: 141.79 ms
  Stdev:  54.66 ms

Bridge stage: no-op reply:
  Min:    46.04 ms
  Max:    179.54 ms
  Mean:   124.85 ms
  Median: 142.65 ms
  Stdev:  54.63 ms

Bridge total (socket -> no-op):
  Min:    46.04 ms
  Max:    179.54 ms
  Mean:   124.85 ms
  Median: 142.65 ms
  Stdev:  54.63 ms

============================================================
Summary:
  Binary load median:      ~21.2 ms
  Server ready median:     ~131.4 ms
  Bridge total median:     ~142.6 ms
```

#### 세션 로딩 (COLD)

```text
binary   /Users/runner/work/agent/agent/bench-bin/examples/loading_probe (d38959afe085)
mode     cold
protocol COLD: every measured run gets its own freshly copied HOME; no derived cache from an earlier run exists, so each sample is a first load of the fixture.
fixtures /var/folders/h5/t85nd63d71x27_8gl5gw3l_w0000gn/T/horon-loading-bench-q8b3fgb_
note     --warmup 3 is ignored in cold mode: every sample gets a fresh HOME
fixture events=0 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    0.033/0.031    p95    0.071/0.054    delta  -0.003 ms  control-no-change
  skill_request_overlay p50    0.232/0.231    p95    0.631/0.464    delta  -0.001 ms  control-no-change
  mcp_config_load   p50    0.269/0.269    p95    0.689/0.546    delta  +0.000 ms  control-no-change
  attach_history    p50    0.001/0.000    p95    0.001/0.001    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.001/0.001    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.001/0.001    delta  -0.000 ms  control-no-change
  session_save      p50   20.944/23.238   p95   37.211/40.105   delta  +2.294 ms  control-no-change
  total store_read_bytes p50 23253  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 23233..23261 (6 distinct values)
  SPREAD   b session_save store_read_bytes 23237..23261 (6 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    4.401/4.218    p95    4.691/4.431    delta  -0.183 ms  control-no-change
  skill_request_overlay p50    5.278/5.589    p95    5.950/6.148    delta  +0.311 ms  control-no-change
  mcp_config_load   p50    0.287/0.274    p95    0.353/0.308    delta  -0.013 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.001/0.002    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.001/0.000    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.001/0.000    delta  +0.000 ms  control-no-change
  session_save      p50   20.470/21.542   p95   24.256/37.784   delta  +1.072 ms  control-no-change
  total store_read_bytes p50 23251  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 23241..23257 (5 distinct values)
  SPREAD   b session_save store_read_bytes 23225..23261 (6 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.029/0.031    p95    0.031/0.068    delta  +0.002 ms  control-no-change
  skill_request_overlay p50    0.230/0.242    p95    0.253/0.370    delta  +0.013 ms  control-no-change
  mcp_config_load   p50    0.268/0.268    p95    0.295/0.372    delta  -0.001 ms  control-no-change
  attach_history    p50  348.130/349.491  p95  382.491/386.880  delta  +1.360 ms  control-no-change
  resume_ready      p50   69.395/67.113   p95   79.727/83.828   delta  -2.282 ms  control-no-change
  provider_dispatch p50    1.310/1.247    p95    1.723/2.137    delta  -0.064 ms  control-no-change
  session_save      p50   16.055/15.752   p95   19.230/27.998   delta  -0.303 ms  control-no-change
  total store_read_bytes p50 5311609  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189203..4189207 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099141..1099145 (3 distinct values)
  SPREAD   a session_save store_read_bytes 23239..23270 (7 distinct values)
  SPREAD   b attach_history store_read_bytes 4189203..4189207 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099141..1099145 (2 distinct values)
  SPREAD   b session_save store_read_bytes 23255..23267 (5 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    4.225/4.035    p95    4.549/5.278    delta  -0.190 ms  control-no-change
  skill_request_overlay p50    5.409/4.813    p95    5.940/5.911    delta  -0.596 ms  control-no-change
  mcp_config_load   p50    0.281/0.256    p95    0.472/0.328    delta  -0.024 ms  control-no-change
  attach_history    p50  358.234/355.290  p95  388.326/381.088  delta  -2.944 ms  control-no-change
  resume_ready      p50   66.197/68.249   p95   77.963/83.505   delta  +2.052 ms  control-no-change
  provider_dispatch p50    1.298/1.315    p95    1.859/2.850    delta  +0.017 ms  control-no-change
  session_save      p50   15.391/17.807   p95   21.036/34.742   delta  +2.416 ms  control-no-change
  total store_read_bytes p50 5311609  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189203..4189207 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099141..1099145 (2 distinct values)
  SPREAD   a session_save store_read_bytes 23247..23271 (6 distinct values)
  SPREAD   b attach_history store_read_bytes 4189205..4189207 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099144..1099145 (2 distinct values)
  SPREAD   b session_save store_read_bytes 23251..23267 (8 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.032/0.034    p95    0.066/0.115    delta  +0.003 ms  control-no-change
  skill_request_overlay p50    0.226/0.249    p95    0.428/0.464    delta  +0.023 ms  control-no-change
  mcp_config_load   p50    0.251/0.270    p95    0.606/0.516    delta  +0.019 ms  control-no-change
  attach_history    p50 4423.038/4447.513 p95 6595.303/6240.417 delta +24.475 ms  control-no-change
  resume_ready      p50  767.506/804.077  p95 1097.336/1222.346 delta +36.570 ms  control-no-change
  provider_dispatch p50   12.659/12.911   p95   17.049/16.846   delta  +0.251 ms  control-no-change
  session_save      p50   25.554/26.005   p95   38.828/49.067   delta  +0.452 ms  control-no-change
  total store_read_bytes p50 53417824  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184301..42184305 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210251..11210255 (2 distinct values)
  SPREAD   a session_save store_read_bytes 23250..23270 (6 distinct values)
  SPREAD   b attach_history store_read_bytes 42184297..42184305 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210247..11210255 (3 distinct values)
  SPREAD   b session_save store_read_bytes 23253..23278 (7 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    4.393/4.366    p95    5.836/5.076    delta  -0.027 ms  control-no-change
  skill_request_overlay p50    5.832/5.611    p95    7.890/6.523    delta  -0.220 ms  control-no-change
  mcp_config_load   p50    0.300/0.318    p95    0.321/0.360    delta  +0.019 ms  control-no-change
  attach_history    p50 4095.519/4050.928 p95 5554.449/5081.356 delta -44.591 ms  control-no-change
  resume_ready      p50  748.951/745.804  p95 1514.922/917.264  delta  -3.147 ms  control-no-change
  provider_dispatch p50   12.383/12.369   p95   17.365/17.808   delta  -0.014 ms  control-no-change
  session_save      p50   22.239/24.538   p95  102.934/41.257   delta  +2.299 ms  control-no-change
  total store_read_bytes p50 53417824  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184299..42184305 (4 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210250..11210255 (4 distinct values)
  SPREAD   a session_save store_read_bytes 23243..23274 (7 distinct values)
  SPREAD   b attach_history store_read_bytes 42184293..42184305 (4 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210243..11210255 (4 distinct values)
  SPREAD   b session_save store_read_bytes 23254..23274 (6 distinct values)
report   bench-results/loading-cold.json
mode cold, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### 세션 로딩 (WARM)

```text
binary   /Users/runner/work/agent/agent/bench-bin/examples/loading_probe (d38959afe085)
mode     warm
protocol WARM: one HOME per side, --warmup discarded runs per side, then measured runs alternate between the two sides on those same HOMEs.
fixtures /var/folders/h5/t85nd63d71x27_8gl5gw3l_w0000gn/T/horon-loading-bench-wn155voh
fixture events=0 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    0.028/0.030    p95    0.061/0.161    delta  +0.002 ms  control-no-change
  skill_request_overlay p50    0.231/0.237    p95    0.361/0.723    delta  +0.006 ms  control-no-change
  mcp_config_load   p50    0.263/0.280    p95    0.349/0.693    delta  +0.017 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.002/0.001    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.001    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.001/0.000    delta  +0.000 ms  control-no-change
  session_save      p50   17.536/18.333   p95   24.713/24.830   delta  +0.797 ms  control-no-change
  total store_read_bytes p50 23953  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23244, 23246, 23250, 23252, 23952, 23954, 23956, 24676, 24682, 24686]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23236, 23254, 23954, 23956, 23964, 24664, 24670, 24682, 24690]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23244..24686 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23236..24690 (9 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    4.247/4.071    p95    4.459/4.697    delta  -0.176 ms  control-no-change
  skill_request_overlay p50    5.242/5.266    p95    8.027/5.504    delta  +0.025 ms  control-no-change
  mcp_config_load   p50    0.310/0.269    p95    0.637/0.291    delta  -0.041 ms  control-no-change
  attach_history    p50    0.001/0.000    p95    0.001/0.001    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.001/0.000    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.001/0.001    delta  -0.000 ms  control-no-change
  session_save      p50   16.280/16.188   p95   22.498/18.213   delta  -0.092 ms  control-no-change
  total store_read_bytes p50 23960  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23248, 23250, 23256, 23952, 23954, 23966, 24658, 24668, 24676, 24678]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23246, 23254, 23960, 23968, 23972, 24670, 24674, 24678, 24682]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23248..24678 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23246..24682 (9 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.041/0.032    p95    0.088/0.077    delta  -0.008 ms  control-no-change
  skill_request_overlay p50    0.296/0.232    p95    1.085/0.545    delta  -0.064 ms  control-no-change
  mcp_config_load   p50    0.318/0.276    p95    0.806/0.708    delta  -0.042 ms  control-no-change
  attach_history    p50  141.959/133.909  p95  191.784/173.216  delta  -8.050 ms  control-no-change
  resume_ready      p50   77.060/86.538   p95  107.917/120.121  delta  +9.478 ms  control-no-change
  provider_dispatch p50    1.233/1.353    p95    3.234/1.601    delta  +0.121 ms  control-no-change
  session_save      p50   24.651/20.384   p95   37.583/32.104   delta  -4.267 ms  control-no-change
  total store_read_bytes p50 2841562  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23963, 23971, 24685, 24689, 25401, 25407, 26101, 26119, 26827, 26829]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23961, 23977, 24691, 24695, 25401, 25409, 26111, 26127, 26829, 26837]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23963..26829 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23961..26837 (10 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    4.101/4.151    p95    4.309/5.341    delta  +0.051 ms  control-no-change
  skill_request_overlay p50    5.462/5.270    p95    6.135/7.857    delta  -0.192 ms  control-no-change
  mcp_config_load   p50    0.279/0.277    p95    0.325/0.333    delta  -0.002 ms  control-no-change
  attach_history    p50  109.991/110.057  p95  119.984/126.298  delta  +0.067 ms  control-no-change
  resume_ready      p50   64.733/66.794   p95   71.274/71.066   delta  +2.061 ms  control-no-change
  provider_dispatch p50    1.246/1.240    p95    1.913/1.397    delta  -0.006 ms  control-no-change
  session_save      p50   17.284/18.240   p95   23.362/20.843   delta  +0.957 ms  control-no-change
  total store_read_bytes p50 2841556  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23969, 23981, 24687, 24695, 25395, 25401, 26115, 26117, 26827, 26837]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23963, 23969, 24673, 24677, 25399, 25409, 26107, 26113, 26815, 26821]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23969..26837 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23963..26821 (10 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.040/0.042    p95    0.054/0.146    delta  +0.002 ms  control-no-change
  skill_request_overlay p50    0.241/0.252    p95    0.297/0.599    delta  +0.011 ms  control-no-change
  mcp_config_load   p50    0.276/0.294    p95    0.606/0.702    delta  +0.018 ms  control-no-change
  attach_history    p50 1535.498/1553.817 p95 2103.852/2076.040 delta +18.319 ms  control-no-change
  resume_ready      p50  981.716/835.314  p95 1304.111/1148.186 delta -146.402 ms  control-drift
  provider_dispatch p50   13.775/14.952   p95   18.638/18.306   delta  +1.177 ms  control-no-change
  session_save      p50   39.141/29.604   p95  132.719/44.475   delta  -9.537 ms  control-no-change
  total store_read_bytes p50 28640579  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23976, 24682, 24694, 25396, 25412, 26110, 26114, 26826, 26840]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23972, 23976, 24694, 25396, 25412, 26114, 26126, 26832, 26840]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23976..26840 (9 distinct values)
  SPREAD   b session_save store_read_bytes 23972..26840 (9 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    3.951/4.632    p95    4.754/5.955    delta  +0.681 ms  control-drift
  skill_request_overlay p50    5.118/5.471    p95    6.985/6.437    delta  +0.353 ms  control-no-change
  mcp_config_load   p50    0.270/0.268    p95    0.334/0.402    delta  -0.001 ms  control-no-change
  attach_history    p50 1182.203/1205.862 p95 1227.496/1353.449 delta +23.660 ms  control-no-change
  resume_ready      p50  688.268/716.076  p95  760.998/864.311  delta +27.808 ms  control-no-change
  provider_dispatch p50   11.771/11.944   p95   15.673/15.263   delta  +0.173 ms  control-no-change
  session_save      p50   21.659/24.576   p95   78.717/41.506   delta  +2.917 ms  control-no-change
  total store_read_bytes p50 28640575  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23976, 23988, 24684, 24686, 25400, 26110, 26126, 26832, 26844]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23973, 23975, 24689, 24695, 25401, 25403, 26119, 26125, 26833, 26843]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23976..26844 (9 distinct values)
  SPREAD   b session_save store_read_bytes 23973..26843 (10 distinct values)
report   bench-results/loading-warm.json
mode warm, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### Tool latency

| Tool case | median | p95 | min | max | samples |
|---|---|---|---|---|---|
| read (tiny file) | 0.3 ms | 0.5 ms | 0.2 ms | 0.5 ms | 5 |
| read (1000 lines) | 0.6 ms | 0.7 ms | 0.5 ms | 0.7 ms | 5 |
| read (10 lines @ offset) | 0.5 ms | 0.6 ms | 0.5 ms | 0.6 ms | 5 |
| read (repo Cargo.toml) | 0.3 ms | 0.4 ms | 0.3 ms | 0.4 ms | 5 |
| write (small) | 1.9 ms | 2.2 ms | 1.6 ms | 2.2 ms | 5 |
| write (10KB) | 2.3 ms | 2.8 ms | 2.3 ms | 2.8 ms | 5 |
| edit (simple replace) | 2.1 ms | 3.1 ms | 1.9 ms | 3.1 ms | 5 |
| ls (scripts dir) | 1.3 ms | 4.1 ms | 1.1 ms | 4.1 ms | 5 |
| agentgrep (grep in tree) | 6.8 ms | 8.7 ms | 4.1 ms | 8.7 ms | 5 |
| agentgrep (find **/*.rs) | 2.3 ms | 2.5 ms | 2.3 ms | 2.5 ms | 5 |
| bash (true) | 7.3 ms | 10.9 ms | 4.7 ms | 10.9 ms | 5 |
| bash (echo hello) | 8.3 ms | 10.7 ms | 6.8 ms | 10.7 ms | 5 |
| bash (ls fixture tree) | 7.4 ms | 8.6 ms | 7.1 ms | 8.6 ms | 5 |
| invalid tool (dispatch only) | 0.1 ms | 0.1 ms | 0.1 ms | 0.1 ms | 5 |

#### Reload storm

1 resume(s) per level; each level resumes sessions no other level has opened, from a freshly started daemon.

| Concurrent resumes | history median | p95 | max | storm wall | failures |
|---|---|---|---|---|---|
| 1 | 90.0 ms | 90.0 ms | 90.0 ms | 90.1 ms | 0 |
| 10 | 216.7 ms | 218.1 ms | 218.1 ms | 218.4 ms | 0 |
| 40 | 1.48 s | 1.50 s | 1.50 s | 1.50 s | 0 |

#### Compile loop

| Checkpoint | Touched | median | min | max | samples |
|---|---|---|---|---|---|
| warm_noop | — | 1.68 s | 1.68 s | 1.68 s | 1 |
| leaf_touch | src/main.rs | 783.7 ms | 783.7 ms | 783.7 ms | 1 |
| core_touch | crates/app-core/src/lib.rs | 34.25 s | 34.25 s | 34.25 s | 1 |
| build_meta_touch | crates/build-meta/src/lib.rs | 62.15 s | 62.15 s | 62.15 s | 1 |

### Windows

#### Coding agent comparison (headless, mock model)

Host: windows/AMD64, 4 CPUs, Python 3.12.10. Baseline: **horon**.

Skipped: syscalls (strace not available)

| Tool | Version | `--version` (median) | Install size |
|---|---|---|---|
| horon | horon-agent v0.98.1-dev (e76ed22) | 14.5 ms | 84.2 MB |
| jcode | jcode v0.99.1 (7206efd0c) | 16.0 ms | 128.4 MB |
| Claude Code | 2.1.317 (Claude Code) | 43.7 ms | 645.2 MB |
| Codex CLI | codex-cli 0.166.0 | 165.9 ms | 479.4 MB |
| OpenCode | 1.19.5 | 607.5 ms | 605.3 MB |
| pi | 0.73.1 | 1.13 s | 142.7 MB |
| oh-my-pi (omp) | omp/18.18.0 | 724.7 ms | 291.2 MB |
| oh-my-openagent (omo) | omo 5.1.64 (engine: senpi 2026.11.5-1) | 216.3 ms | 514.8 MB |

**One-shot headless prompt** (includes startup, one `ls` tool call, final answer)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Tool round-trip | Note |
|---|---|---|---|---|---|---|---|
| horon | 360.6 ms | 322.7 ms–423.3 ms | baseline | 35.8 MB | baseline | yes | — |
| jcode | 173.3 ms | 153.5 ms–310.6 ms | 2.08× less | 26.5 MB | 1.35× less | yes | — |
| Claude Code | 1.45 s | 1.42 s–2.34 s | 4.03× more | 272.5 MB | 7.62× more | yes | — |
| Codex CLI | 1.15 s | 473.8 ms–4.73 s | 3.18× more | 171.8 MB | 4.80× more | yes | — |
| OpenCode | 5.47 s | 5.37 s–9.25 s | 15.16× more | 835.7 MB | 23.37× more | yes | — |
| pi | 1.34 s | 1.33 s–1.37 s | 3.72× more | 155.6 MB | 4.35× more | yes | — |
| oh-my-pi (omp) | 3.61 s | 3.48 s–3.62 s | 10.00× more | 393.2 MB | 11.00× more | yes | — |
| oh-my-openagent (omo) | 26.29 s | 24.08 s–58.24 s | 72.91× more | 572.1 MB | 16.00× more | yes | — |

**Skill invocation** (process start to final answer, including skill discovery and one load of the fixture skill)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | Note |
|---|---|---|---|---|---|---|---|---|
| horon | 368.2 ms | 335.1 ms–378.2 ms | baseline | 25.2 MB | baseline | skill-uri | yes | — |
| jcode | 278.0 ms | 246.8 ms–299.1 ms | 1.32× less | 23.5 MB | 1.07× less | skill-tool | yes | — |
| Claude Code | 1.06 s | 1.06 s–1.10 s | 2.88× more | 237.4 MB | 9.41× more | skill-tool | yes | — |
| Codex CLI | — | — | — | — | — | file-read | no | failed: the fixture sentinel never came back |
| OpenCode | 8.10 s | 8.02 s–8.38 s | 21.99× more | 750.2 MB | 29.73× more | skill-tool | yes | — |
| pi | 1.31 s | 1.30 s–1.31 s | 3.55× more | 136.8 MB | 5.42× more | file-read | yes | — |
| oh-my-pi (omp) | 3.98 s | 3.93 s–4.05 s | 10.80× more | 408.7 MB | 16.20× more | file-read | yes | — |
| oh-my-openagent (omo) | 25.26 s | 24.78 s–27.73 s | 68.62× more | 524.9 MB | 20.81× more | file-read | yes | — |

**MCP tool call** (process start to final answer, including starting the fixture stdio MCP server and one `bench_echo` call)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | MCP ready | Note |
|---|---|---|---|---|---|---|---|---|---|
| horon | 459.2 ms | 440.8 ms–470.8 ms | baseline | 37.4 MB | baseline | mcp-call | yes | 257.9 ms | — |
| jcode | — | — | — | — | — | — | no | — | failed: no tool that could reach the fixture was offered (one request, no tool call) |
| Claude Code | — | — | — | — | — | — | — | — | skipped: Claude Code 2.1.317 -p did not expose a configured stdio MCP server in any of the |
| Codex CLI | — | — | — | — | — | — | — | — | skipped: codex 0.166.0 lists the server only as the Responses namespace tool mcp__bench; a |
| OpenCode | 7.82 s | 7.72 s–7.85 s | 17.02× more | 759.1 MB | 20.27× more | mcp-tool | yes | 6.24 s | — |
| pi | — | — | — | — | — | — | — | — | skipped: pi has no MCP client; its README points at a user extension |
| oh-my-pi (omp) | 3.65 s | 3.15 s–3.74 s | 7.95× more | 378.9 MB | 10.12× more | mcp-route | yes | 2.96 s | — |
| oh-my-openagent (omo) | — | — | — | — | — | mcp-tool | no | — | failed: the fixture sentinel never came back |

**Session memory** (idle after 3 turn(s) per session; sizes are process-tree sums, RSS; PSS also recorded on Linux). "Each additional" is the least-squares slope across the measured session counts; 0 B means growth stayed below RSS sampling noise. Threads and descriptors come from the daemon's own `server:memory` after the turns.

| Tool | First session | Each additional | vs horon (additional) | Threads/session | FDs/session | First (post-turn) | Each additional (post-turn) |
|---|---|---|---|---|---|---|---|
| horon | 23.6 MB | 185.8 KB | baseline | — | — | 30.4 MB | 589.8 KB |
| jcode | 21.9 MB | 78.6 KB | 2.37× less | — | — | 26.6 MB | 397.3 KB |
| Claude Code | 154.0 MB | 157.8 MB | 869.47× more | — | — | 190.7 MB | 189.1 MB |
| Codex CLI | 127.5 MB | 96.5 MB | 531.70× more | — | — | 147.0 MB | 120.2 MB |
| OpenCode | 242.0 MB | 17.4 MB | 95.73× more | — | — | 432.9 MB | 16.9 MB |
| pi | 131.7 MB | 59.8 MB | 329.29× more | — | — | 136.5 MB | 137.9 MB |
| oh-my-pi (omp) | 298.2 MB | 100.2 MB | 552.12× more | — | — | 340.7 MB | 335.9 MB |
| oh-my-openagent (omo) | 144.9 MB | 132.3 MB | 729.16× more | — | — | 410.7 MB | 438.7 MB |

Threads/descriptors unavailable: horon (server:memory did not report thread_count, fd_count), jcode (server:memory did not report thread_count, fd_count)

**Post-turn tree RSS per session count**

| Tool | 1 session(s) | 5 session(s) | 10 session(s) |
|---|---|---|---|
| horon | 30.4 MB | 33.2 MB | 35.6 MB |
| jcode | 26.6 MB | 28.3 MB | 30.1 MB |
| Claude Code | 190.7 MB | 950.0 MB | 1.8 GB |
| Codex CLI | 147.0 MB | 625.6 MB | 1.2 GB |
| OpenCode | 432.9 MB | 478.5 MB | 583.8 MB |
| pi | 136.5 MB | 680.8 MB | 1.3 GB |
| oh-my-pi (omp) | 340.7 MB | 1.7 GB | 3.3 GB |
| oh-my-openagent (omo) | 410.7 MB | 2.1 GB | 3.9 GB |

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
- oh-my-openagent (omo) @ 10 sessions: turn 2 session 2: Agent is already processing. Specify streamingBehavior ('steer' or 'followUp') to queue the message.
- oh-my-openagent (omo) @ 10 sessions: turn 2 session 3: Agent is already processing. Specify streamingBehavior ('steer' or 'followUp') to queue the message.
- oh-my-openagent (omo) @ 10 sessions: turn 2 session 6: Agent is already processing. Specify streamingBehavior ('steer' or 'followUp') to queue the message.
- oh-my-openagent (omo) @ 10 sessions: turn 2 session 9: Agent is already processing. Specify streamingBehavior ('steer' or 'followUp') to queue the message.
- oh-my-openagent (omo) @ 10 sessions: turn 3 session 2: Agent is already processing. Specify streamingBehavior ('steer' or 'followUp') to queue the message.
- oh-my-openagent (omo) @ 10 sessions: turn 3 session 3: Agent is already processing. Specify streamingBehavior ('steer' or 'followUp') to queue the message.
- oh-my-openagent (omo) @ 10 sessions: turn 3 session 6: Agent is already processing. Specify streamingBehavior ('steer' or 'followUp') to queue the message.
- oh-my-openagent (omo) @ 10 sessions: turn 3 session 9: Agent is already processing. Specify streamingBehavior ('steer' or 'followUp') to queue the message.

#### 기동

```text
Benchmarking: D:/a/agent/agent/bench-bin/horon-agent.exe
============================================================

--help (binary load):
  Min:    11.10 ms
  Max:    14.63 ms
  Mean:   12.62 ms
  Median: 12.54 ms
  Stdev:  1.13 ms

--version:
  Min:    10.87 ms
  Max:    11.83 ms
  Mean:   11.40 ms
  Median: 11.39 ms
  Stdev:  0.27 ms

Measuring isolated server startup (10 runs)...

Server ready (isolated socket connectable):
  Min:    21.87 ms
  Max:    26.42 ms
  Mean:   22.51 ms
  Median: 22.05 ms
  Stdev:  1.39 ms

Measuring harness bridge readiness (10 runs)...
  skipped: api-bridge is Unix-only, so Windows has no harness bridge to time

Bridge stage: api socket connectable: No successful runs

Bridge stage: hello_ok: No successful runs

Bridge stage: no-op reply: No successful runs

Bridge total (socket -> no-op): No successful runs

============================================================
Summary:
  Binary load median:      ~12.5 ms
  Server ready median:     ~22.0 ms
  Bridge total median:     n/a
```

#### 세션 로딩 (COLD)

```text
binary   D:\a\agent\agent\bench-bin\examples\loading_probe.exe (f58e84fc02c5)
mode     cold
protocol COLD: every measured run gets its own freshly copied HOME; no derived cache from an earlier run exists, so each sample is a first load of the fixture.
fixtures C:\Users\RUNNER~1\AppData\Local\Temp\horon-loading-bench-hezn2pe9
note     --warmup 3 is ignored in cold mode: every sample gets a fresh HOME
fixture events=0 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    0.157/0.157    p95    0.266/0.165    delta  -0.000 ms  control-no-change
  skill_request_overlay p50    1.225/1.209    p95    1.347/1.369    delta  -0.016 ms  control-no-change
  mcp_config_load   p50    1.412/1.442    p95    1.438/1.519    delta  +0.030 ms  control-no-change
  attach_history    p50    0.002/0.002    p95    0.003/0.002    delta  -0.000 ms  control-drift
  resume_ready      p50    0.001/0.001    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.001/0.000    delta  +0.000 ms  control-no-change
  session_save      p50   67.343/65.853   p95   78.604/82.418   delta  -1.489 ms  control-no-change
  total store_read_bytes p50 23244  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 23189..23268 (9 distinct values)
  SPREAD   b session_save store_read_bytes 23224..23264 (7 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50   23.938/24.163   p95   33.363/25.429   delta  +0.225 ms  control-no-change
  skill_request_overlay p50   52.645/52.613   p95   68.722/55.081   delta  -0.031 ms  control-no-change
  mcp_config_load   p50    1.436/1.478    p95    1.525/1.806    delta  +0.042 ms  control-no-change
  attach_history    p50    0.002/0.002    p95    0.006/0.006    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.001/0.001    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.001    p95    0.001/0.001    delta  +0.000 ms  control-drift
  session_save      p50   75.903/70.236   p95   81.931/92.230   delta  -5.667 ms  control-no-change
  total store_read_bytes p50 23262  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 23215..23280 (8 distinct values)
  SPREAD   b session_save store_read_bytes 23228..23284 (9 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.150/0.152    p95    0.197/0.194    delta  +0.002 ms  control-drift
  skill_request_overlay p50    1.213/1.221    p95    1.278/1.448    delta  +0.008 ms  control-no-change
  mcp_config_load   p50    1.419/1.413    p95    1.462/1.724    delta  -0.006 ms  control-no-change
  attach_history    p50  634.556/641.681  p95  652.265/678.450  delta  +7.125 ms  control-no-change
  resume_ready      p50  238.042/234.183  p95  244.554/245.096  delta  -3.859 ms  control-no-change
  provider_dispatch p50    1.926/1.953    p95    1.982/2.013    delta  +0.028 ms  control-no-change
  session_save      p50   58.471/60.673   p95   63.633/70.138   delta  +2.202 ms  control-no-change
  total store_read_bytes p50 5311514  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189149..4189153 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099101..1099105 (3 distinct values)
  SPREAD   a session_save store_read_bytes 23223..23278 (10 distinct values)
  SPREAD   b attach_history store_read_bytes 4189149..4189153 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099101..1099105 (3 distinct values)
  SPREAD   b session_save store_read_bytes 23241..23288 (9 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50   23.123/23.385   p95   23.704/24.390   delta  +0.263 ms  control-no-change
  skill_request_overlay p50   52.665/52.863   p95   56.853/60.104   delta  +0.198 ms  control-no-change
  mcp_config_load   p50    1.482/1.472    p95    1.539/1.542    delta  -0.009 ms  control-no-change
  attach_history    p50  648.106/650.250  p95  671.900/668.658  delta  +2.144 ms  control-no-change
  resume_ready      p50  237.305/235.735  p95  248.011/247.872  delta  -1.570 ms  control-no-change
  provider_dispatch p50    1.903/1.914    p95    2.012/1.968    delta  +0.011 ms  control-no-change
  session_save      p50   63.317/61.974   p95   79.399/64.087   delta  -1.343 ms  control-no-change
  total store_read_bytes p50 5311513  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189149..4189153 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099101..1099105 (3 distinct values)
  SPREAD   a session_save store_read_bytes 23215..23279 (9 distinct values)
  SPREAD   b attach_history store_read_bytes 4189149..4189153 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099101..1099105 (2 distinct values)
  SPREAD   b session_save store_read_bytes 23217..23287 (9 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.155/0.156    p95    0.204/0.190    delta  +0.001 ms  control-no-change
  skill_request_overlay p50    1.228/1.243    p95    1.538/1.300    delta  +0.015 ms  control-no-change
  mcp_config_load   p50    1.444/1.440    p95    1.894/1.550    delta  -0.004 ms  control-no-change
  attach_history    p50 7120.087/7111.845 p95 7433.713/7342.930 delta  -8.242 ms  control-no-change
  resume_ready      p50 2473.483/2454.593 p95 2585.518/2592.576 delta -18.890 ms  control-no-change
  provider_dispatch p50   20.445/20.983   p95   22.231/27.356   delta  +0.538 ms  control-no-change
  session_save      p50   58.886/62.382   p95   68.928/94.595   delta  +3.496 ms  control-no-change
  total store_read_bytes p50 53417740  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184249..42184253 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210212..11210216 (2 distinct values)
  SPREAD   a session_save store_read_bytes 23239..23291 (9 distinct values)
  SPREAD   b attach_history store_read_bytes 42184249..42184253 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210212..11210216 (3 distinct values)
  SPREAD   b session_save store_read_bytes 23263..23291 (7 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50   23.369/23.676   p95   31.542/27.754   delta  +0.308 ms  control-no-change
  skill_request_overlay p50   52.891/52.235   p95   57.733/54.100   delta  -0.656 ms  control-no-change
  mcp_config_load   p50    1.457/1.469    p95    1.542/1.615    delta  +0.012 ms  control-no-change
  attach_history    p50 7152.967/7124.148 p95 7688.268/8016.371 delta -28.820 ms  control-no-change
  resume_ready      p50 2452.863/2470.003 p95 2630.421/2674.246 delta +17.140 ms  control-no-change
  provider_dispatch p50   20.404/20.465   p95   21.400/24.860   delta  +0.062 ms  control-no-change
  session_save      p50   63.170/62.969   p95   84.353/947.884  delta  -0.201 ms  control-no-change
  total store_read_bytes p50 53417738  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184249..42184253 (4 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210212..11210216 (3 distinct values)
  SPREAD   a session_save store_read_bytes 23237..23291 (10 distinct values)
  SPREAD   b attach_history store_read_bytes 42184249..42184253 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210212..11210216 (2 distinct values)
  SPREAD   b session_save store_read_bytes 23233..23287 (7 distinct values)
report   bench-results\loading-cold.json
mode cold, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### 세션 로딩 (WARM)

```text
binary   D:\a\agent\agent\bench-bin\examples\loading_probe.exe (f58e84fc02c5)
mode     warm
protocol WARM: one HOME per side, --warmup discarded runs per side, then measured runs alternate between the two sides on those same HOMEs.
fixtures C:\Users\RUNNER~1\AppData\Local\Temp\horon-loading-bench-8tbl8os6
fixture events=0 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    0.102/0.107    p95    0.115/0.218    delta  +0.005 ms  control-no-change
  skill_request_overlay p50    0.767/0.788    p95    0.809/1.116    delta  +0.021 ms  control-no-change
  mcp_config_load   p50    0.920/0.915    p95    0.951/1.235    delta  -0.005 ms  control-no-change
  attach_history    p50    0.002/0.007    p95    0.007/0.008    delta  +0.005 ms  control-drift
  resume_ready      p50    0.001/0.001    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.001    delta  +0.000 ms  control-no-change
  session_save      p50   54.818/143.263  p95  206.778/432.940  delta +88.445 ms  control-drift
  total store_read_bytes p50 23967  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23250, 23257, 23263, 23944, 23962, 23972, 24645, 24651, 24669]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23257, 23273, 23948, 23952, 23960, 24635, 24651, 24658, 24659]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23250..24669 (9 distinct values)
  SPREAD   b session_save store_read_bytes 23257..24659 (9 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50   14.856/14.800   p95   15.378/18.128   delta  -0.056 ms  control-no-change
  skill_request_overlay p50   37.342/37.974   p95   39.492/45.948   delta  +0.632 ms  control-no-change
  mcp_config_load   p50    0.957/0.986    p95    1.065/1.289    delta  +0.029 ms  control-no-change
  attach_history    p50    0.002/0.002    p95    0.002/0.003    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.001/0.001    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.001    p95    0.001/0.001    delta  +0.000 ms  control-drift
  session_save      p50  132.033/155.209  p95  228.213/303.987  delta +23.176 ms  control-no-change
  total store_read_bytes p50 23942  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23199, 23252, 23257, 23265, 23930, 23954, 23964, 24645, 24659, 24675]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23240, 23265, 23277, 23944, 23959, 23968, 24601, 24648, 24658, 24671]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23199..24675 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23240..24671 (10 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.100/0.105    p95    0.109/0.123    delta  +0.005 ms  control-no-change
  skill_request_overlay p50    0.788/0.779    p95    0.830/0.850    delta  -0.009 ms  control-no-change
  mcp_config_load   p50    0.940/0.933    p95    0.986/1.002    delta  -0.007 ms  control-no-change
  attach_history    p50  215.478/213.894  p95  224.416/223.568  delta  -1.584 ms  control-no-change
  resume_ready      p50  160.434/159.542  p95  180.204/161.660  delta  -0.892 ms  control-no-change
  provider_dispatch p50    1.727/1.702    p95    1.782/1.839    delta  -0.025 ms  control-no-change
  session_save      p50   59.025/52.305   p95  269.624/154.573  delta  -6.720 ms  control-no-change
  total store_read_bytes p50 2841420  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23931, 23941, 24659, 24671, 25318, 25362, 26035, 26069, 26720, 26758]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23979, 23983, 24678, 24682, 25351, 25369, 26041, 26078, 26753, 26765]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23931..26758 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23979..26765 (10 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50   15.398/15.150   p95   15.791/15.767   delta  -0.248 ms  control-no-change
  skill_request_overlay p50   38.108/37.581   p95   39.287/39.202   delta  -0.527 ms  control-no-change
  mcp_config_load   p50    0.964/0.998    p95    1.076/1.181    delta  +0.033 ms  control-no-change
  attach_history    p50  213.696/216.937  p95  223.042/224.013  delta  +3.241 ms  control-no-change
  resume_ready      p50  160.402/161.486  p95  165.821/165.651  delta  +1.084 ms  control-no-change
  provider_dispatch p50    1.773/1.736    p95    1.908/1.800    delta  -0.036 ms  control-no-change
  session_save      p50   61.115/54.327   p95  274.288/79.971   delta  -6.788 ms  control-no-change
  total store_read_bytes p50 2841448  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23971, 23980, 24650, 24683, 25365, 25370, 26037, 26088, 26753, 26762]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23936, 23949, 24651, 24687, 25360, 25369, 26047, 26068, 26695, 26750]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23971..26762 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23936..26750 (10 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.101/0.102    p95    0.153/0.136    delta  +0.001 ms  control-no-change
  skill_request_overlay p50    0.752/0.779    p95    1.106/0.845    delta  +0.027 ms  control-no-change
  mcp_config_load   p50    0.930/0.950    p95    1.315/1.015    delta  +0.020 ms  control-no-change
  attach_history    p50 2281.241/2290.999 p95 2491.738/2434.084 delta  +9.758 ms  control-no-change
  resume_ready      p50 1675.209/1671.560 p95 1936.078/1776.050 delta  -3.649 ms  control-no-change
  provider_dispatch p50   19.985/20.023   p95   22.714/20.506   delta  +0.038 ms  control-no-change
  session_save      p50   54.983/82.788   p95  215.187/253.337  delta +27.805 ms  control-no-change
  total store_read_bytes p50 28640452  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23974, 24630, 24653, 25347, 25363, 26062, 26066, 26733, 26761]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23986, 23994, 24665, 24673, 25312, 25376, 26052, 26075, 26755, 26766]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23974..26761 (9 distinct values)
  SPREAD   b session_save store_read_bytes 23986..26766 (10 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50   15.024/15.244   p95   15.537/16.483   delta  +0.220 ms  control-no-change
  skill_request_overlay p50   37.488/37.349   p95   41.867/43.534   delta  -0.139 ms  control-no-change
  mcp_config_load   p50    1.018/0.997    p95    1.060/1.115    delta  -0.020 ms  control-no-change
  attach_history    p50 2252.424/2245.680 p95 2268.086/2259.287 delta  -6.744 ms  control-no-change
  resume_ready      p50 1650.594/1640.764 p95 1675.008/1668.753 delta  -9.831 ms  control-no-change
  provider_dispatch p50   19.086/18.940   p95   19.571/23.880   delta  -0.146 ms  control-no-change
  session_save      p50   59.534/116.387  p95  143.223/279.682  delta +56.853 ms  control-drift
  total store_read_bytes p50 28640454  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23948, 23982, 24670, 24677, 25341, 25372, 26056, 26060, 26771, 26782]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23950, 23970, 24619, 24681, 25356, 25380, 26043, 26079, 26728, 26766]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23948..26782 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23950..26766 (10 distinct values)
report   bench-results\loading-warm.json
mode warm, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### Tool latency

| Tool case | median | p95 | min | max | samples |
|---|---|---|---|---|---|
| read (tiny file) | 2.7 ms | 2.7 ms | 2.6 ms | 2.7 ms | 5 |
| read (1000 lines) | 2.6 ms | 2.7 ms | 2.2 ms | 2.7 ms | 5 |
| read (10 lines @ offset) | 2.7 ms | 2.7 ms | 2.6 ms | 2.7 ms | 5 |
| read (repo Cargo.toml) | 2.6 ms | 2.7 ms | 2.2 ms | 2.7 ms | 5 |
| write (small) | 10.4 ms | 10.4 ms | 9.6 ms | 10.4 ms | 5 |
| write (10KB) | 8.1 ms | 12.5 ms | 7.9 ms | 12.5 ms | 5 |
| edit (simple replace) | 12.5 ms | 12.7 ms | 9.4 ms | 12.7 ms | 5 |
| ls (scripts dir) | 2.7 ms | 2.7 ms | 2.2 ms | 2.7 ms | 5 |
| agentgrep (grep in tree) | 5.3 ms | 7.3 ms | 4.8 ms | 7.3 ms | 5 |
| agentgrep (find **/*.rs) | 4.7 ms | 5.3 ms | 4.2 ms | 5.3 ms | 5 |
| bash (true) | 9.9 ms | 10.4 ms | 9.9 ms | 10.4 ms | 5 |
| bash (echo hello) | 12.6 ms | 14.8 ms | 10.4 ms | 14.8 ms | 5 |
| bash (ls fixture tree) | 13.0 ms | 13.0 ms | 12.0 ms | 13.0 ms | 5 |
| invalid tool (dispatch only) | 2.6 ms | 2.7 ms | 2.6 ms | 2.7 ms | 5 |

#### Reload storm

1 resume(s) per level; each level resumes sessions no other level has opened, from a freshly started daemon.

| Concurrent resumes | history median | p95 | max | storm wall | failures |
|---|---|---|---|---|---|
| 1 | 318.9 ms | 318.9 ms | 318.9 ms | 319.2 ms | 0 |
| 10 | 830.4 ms | 831.7 ms | 831.7 ms | 834.4 ms | 0 |
| 40 | 2.21 s | 3.04 s | 3.04 s | 3.04 s | 0 |

#### Compile loop

| Checkpoint | Touched | median | min | max | samples |
|---|---|---|---|---|---|
| warm_noop | — | 685.3 ms | 685.3 ms | 685.3 ms | 1 |
| leaf_touch | src\main.rs | 914.7 ms | 914.7 ms | 914.7 ms | 1 |
| core_touch | crates\app-core\src\lib.rs | 28.72 s | 28.72 s | 28.72 s | 1 |
| build_meta_touch | crates\build-meta\src\lib.rs | 39.49 s | 39.49 s | 39.49 s | 1 |

<!-- bench-auto:end -->
