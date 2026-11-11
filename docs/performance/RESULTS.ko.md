# 성능: 측정 결과와 그 한계

언어: [English](RESULTS.md) | **한국어**

이 문서는 `agent-runtime-performance-2` 계획이 기준점 B0(계획 시작 직전 커밋) 위에서 남긴 측정값을 기록한다. 이 계획은 `dev` 에 성능 커밋 19개를 올렸고, 같은 범위에 이 계획과 무관한 `fix(cli)` 커밋 하나가 더 있지만 아래 수치와는 관계가 없다. 측정 대상은 세션 저장소의 해시와 이력 페이지, SSE 파서, 복제 내보내기, 결과 캐시의 임계값 판정, 그리고 attach/detach 소크다. 측정은 모두 이 기기(Apple M1 Max, 디버그 테스트 프로필)에서 같은 기계의 앞뒤를 번갈아 재는 방식으로 이뤄졌다.

원본 산출물은 전부 `.omo/evidence/agent-runtime-performance-2/` 아래에 있고 그 경로는 `.gitignore:26` 으로 무시된다. 계획은 주요 수치를 PR 본문에 옮겨 적으라고 했지만, 실제로는 대부분의 PR 본문이 한 줄이었다. 그래서 오늘 이 저장소를 클론한 사람은 이 작업의 측정값을 하나도 볼 수 없다. 이 페이지가 그 기록을 추적되는 자리로 옮긴 것이며, 모든 수치는 위 증거 파일에서 그대로 가져왔다.

## 차트: 최신 자동 측정

<!-- bench-charts:start -->

이 구간은 `.github/workflows/benchmark-docs.yml` 이 아래 자동 측정의 경쟁 도구 비교로 다시 그립니다. 손으로 고치지 않습니다. 모든 차트는 낮을수록 좋고 파란 막대가 horon 입니다.

![벤치마크 요약](charts/00-summary.ko.svg)

| 항목 | 차트 |
| --- | --- |
| `--version` 시간 | [01-version.ko.svg](charts/01-version.ko.svg) |
| 설치 크기 | [02-install-size.ko.svg](charts/02-install-size.ko.svg) |
| 원샷 프롬프트 시간 | [03-oneshot-time.ko.svg](charts/03-oneshot-time.ko.svg) |
| 원샷 프롬프트 메모리 | [04-oneshot-memory.ko.svg](charts/04-oneshot-memory.ko.svg) |
| 스킬 호출 시간 | [05-skill-time.ko.svg](charts/05-skill-time.ko.svg) |
| MCP 도구 호출 시간 | [06-mcp-time.ko.svg](charts/06-mcp-time.ko.svg) |
| 최대 세션 수에서의 메모리 | [07-sessions-memory.ko.svg](charts/07-sessions-memory.ko.svg) |
| 세션 하나당 늘어나는 메모리 | [08-per-session-memory.ko.svg](charts/08-per-session-memory.ko.svg) |
| 원샷 프롬프트당 fsync 호출 수 | [09-fsync-calls.ko.svg](charts/09-fsync-calls.ko.svg) |
| 첫 세션 메모리 | [10-first-session-memory.ko.svg](charts/10-first-session-memory.ko.svg) |

<!-- bench-charts:end -->

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

여섯 항목은 측정한 뒤 채택하지 않기로 결정했다. 각 항목에는 그 결정을 뒤집으려면 넘어야 할 숫자가 함께 있다.

| 항목 | 결정 | 결정한 숫자 |
| --- | --- | --- |
| 비활성 세션 축출 정책 | 채택 안 함 | 세션당 fd 0.999개, 스레드 0.998개, RSS 0.41 MB. 1개에서 1,000개까지 이 기울기가 꺾이지 않는다. `shared_context` 항목과 debug job 은 다섯 인구 수 모두에서 0이고, 소크에서 세션 수는 0으로 돌아온다 |
| `globalEvents` 다중화 | 채택 안 함 (프로토콜 변경) | 감시 세션당 소비자 쪽 fd 7.4개와 스레드 3.0개. 데몬 자신의 몫은 세션당 연결 1.0개이고, 브리지는 연결 하나에 세션 하나를 붙인다 |
| 세션 맵 잠금 범위 축소 | 채택 안 함 | 무관한 세션 제거가 import 전체를 기다린다. 1 / 20 / 60 / 150 이벤트에서 861 / 5,561 / 11,255 / 24,499 ms, 비율 1.00 |
| 인터럽트 폴 25 ms 타이머를 통지로 교체 | 채택 안 함 | 최악 29.7 ms(평균 27.6 ms)를 아끼려고, 감시 그룹 라운드당 wakeup 1회라는 비용 구조를 바꾸는 것이다 |
| `run.stopped` 저널 쓰기와 닫기 저장 겹치기 (#144, #129) | 채택 안 함 (더 느림) | 같은 잡에서 `dev`와 비교한 Windows 마지막 요청→종료 중앙값: 5회 39.8 → 56.4 ms(run 42759006826), 20회 35.2 → 42.8 ms(run 42764256757, IQR 33.0–40.9 대 40.7–45.5). Linux와 macOS는 동률이었다. 이후 #146이 닫기 저장의 flush 자체를 없애 같은 구간이 32.5 ms가 됐다 |
| Windows 고정 셸 환경을 프로세스당 한 번만 계산 (#124, #122) | 채택 안 함 (동률) | Windows 도구 왕복 구간, 20회: 74.3 → 73.5 ms(run 42735692366), min–max 범위 안이다. 왕복의 대부분은 호출마다 만드는 env 표나 PATH 탐색이 아니라 `cmd.exe` 기동 비용이다 |

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

<!-- bench-auto:start commit=8724ad632e3798a4161d8158ff3728f2ad27307b -->

이 구간은 `.github/workflows/benchmark-docs.yml` 이 자동으로 다시 씁니다. 손으로 고치지 않습니다.

- 커밋: `8724ad632e3798a4161d8158ff3728f2ad27307b`
- 측정 시각: 2026-11-11 00:22 UTC
- 실행: <https://github.com/horon-ai/agent/actions/runs/42924574911> (원본 JSON 은 OS별 `benchmark-<os>-<run_id>` 아티팩트)

OS마다 다른 GitHub 호스티드 러너에서 따로 잽니다. 기계가 다르므로 OS끼리 절대값을 비교하지 않고, 같은 OS 안의 상대 비교(horon 대비 배수)와 커밋 사이의 추세만 읽습니다.

| OS | 러너 | 바이너리 | 상태 |
| --- | --- | --- | --- |
| Linux | `ubuntu-24.04` | `horon-agent v0.1.0-dev (8724ad6)` | 성공 |
| macOS | `macos-latest` | `horon-agent v0.1.0-dev (8724ad6)` | 성공 |
| Windows | `windows-latest` | `horon-agent v0.1.0-dev (8724ad6)` | 성공 |

### Linux

#### Coding agent comparison (headless, mock model)

Host: linux/x86_64, 4 CPUs, Python 3.12.16. Baseline: **horon**. Samples per one-shot, skill and MCP cell: 5.

| Tool | Version | `--version` (median) | Install size |
|---|---|---|---|
| horon | horon-agent v0.1.0-dev (8724ad6) | 3.6 ms | 112.4 MB |
| jcode | jcode v0.99.1 (7206efd0c) | 8.5 ms | 163.4 MB |
| Claude Code | 2.1.317 (Claude Code) | 8.6 ms | 632.2 MB |
| Codex CLI | codex-cli 0.166.0 | 39.9 ms | 435.0 MB |
| OpenCode | 1.19.5 | 463.2 ms | 613.4 MB |
| pi | 1.5.0 | 217.0 ms | 108.9 MB |
| oh-my-pi (omp) | omp/18.18.0 | 730.2 ms | 412.9 MB |
| oh-my-openagent (omo) | omo 5.1.64 (engine: senpi 2026.11.5-1) | 110.5 ms | 520.6 MB |

**One-shot headless prompt** (includes startup, one `ls` tool call, final answer)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Tool round-trip | fsync calls | Note |
|---|---|---|---|---|---|---|---|---|
| horon | 50.2 ms | 49.4 ms–247.6 ms | baseline | 47.0 MB | baseline | yes | 10 | — |
| jcode | 141.1 ms | 140.8 ms–149.2 ms | 2.81× more | 42.0 MB | 1.12× less | yes | 5 | — |
| Claude Code | 424.4 ms | 417.5 ms–451.4 ms | 8.45× more | 207.8 MB | 4.43× more | yes | — | — |
| Codex CLI | 622.5 ms | 540.9 ms–679.8 ms | 12.39× more | 199.3 MB | 4.25× more | yes | 6 | — |
| OpenCode | 2.78 s | 2.59 s–2.82 s | 55.35× more | 566.5 MB | 12.06× more | yes | 4 | — |
| pi | 355.9 ms | 354.8 ms–370.1 ms | 7.08× more | 114.3 MB | 2.43× more | yes | — | — |
| oh-my-pi (omp) | 1.95 s | 1.75 s–2.10 s | 38.90× more | 394.1 MB | 8.39× more | yes | 71 | — |
| oh-my-openagent (omo) | 6.26 s | 6.02 s–6.58 s | 124.71× more | 665.2 MB | 14.17× more | yes | 36 | — |

**Skill invocation** (process start to final answer, including skill discovery and one load of the fixture skill)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | Note |
|---|---|---|---|---|---|---|---|---|
| horon | 46.8 ms | 46.3 ms–49.2 ms | baseline | 35.8 MB | baseline | skill-uri | yes | — |
| jcode | 53.8 ms | 53.4 ms–55.7 ms | 1.15× more | 40.6 MB | 1.13× more | skill-tool | yes | — |
| Claude Code | 451.7 ms | 438.1 ms–455.1 ms | 9.65× more | 207.8 MB | 5.80× more | skill-tool | yes | — |
| Codex CLI | 795.8 ms | 637.5 ms–935.0 ms | 16.99× more | 200.5 MB | 5.60× more | file-read | yes | — |
| OpenCode | 3.06 s | 3.03 s–3.11 s | 65.26× more | 606.4 MB | 16.93× more | skill-tool | yes | — |
| pi | 490.9 ms | 489.2 ms–504.3 ms | 10.48× more | 125.6 MB | 3.51× more | file-read | yes | — |
| oh-my-pi (omp) | 1.76 s | 1.73 s–1.79 s | 37.53× more | 338.5 MB | 9.45× more | file-read | yes | — |
| oh-my-openagent (omo) | 6.79 s | 6.36 s–6.90 s | 145.03× more | 671.6 MB | 18.74× more | file-read | yes | — |

**MCP tool call** (process start to final answer, including starting the fixture stdio MCP server and one `bench_echo` call)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | MCP ready | Note |
|---|---|---|---|---|---|---|---|---|---|
| horon | 68.9 ms | 65.2 ms–69.0 ms | baseline | 45.1 MB | baseline | mcp-call | yes | 53.5 ms | — |
| jcode | 174.0 ms | 172.6 ms–175.1 ms | 2.53× more | 53.9 MB | 1.20× more | mcp-action | yes | 142.4 ms | — |
| Claude Code | 544.8 ms | 537.0 ms–566.6 ms | 7.91× more | 224.7 MB | 4.98× more | mcp-tool | yes | 401.8 ms | — |
| Codex CLI | 650.9 ms | 608.8 ms–703.2 ms | 9.45× more | 196.5 MB | 4.36× more | mcp-tool | yes | 341.3 ms | — |
| OpenCode | 2.76 s | 2.72 s–2.79 s | 40.10× more | 565.7 MB | 12.55× more | mcp-tool | yes | 2.54 s | — |
| pi | 548.7 ms | 538.5 ms–553.2 ms | 7.97× more | 140.3 MB | 3.11× more | mcp-tool | yes | 490.6 ms | — |
| oh-my-pi (omp) | 1.84 s | 1.79 s–2.09 s | 26.71× more | 385.0 MB | 8.54× more | mcp-route | yes | 1.71 s | — |
| oh-my-openagent (omo) | 6.64 s | 6.60 s–6.75 s | 96.46× more | 665.8 MB | 14.77× more | mcp-tool | yes | 3.41 s | — |

**Session memory** (idle after 3 turn(s) per session; sizes are process-tree sums, RSS; PSS also recorded on Linux). "Each additional" is the least-squares slope across the measured session counts; 0 B means growth stayed below RSS sampling noise. Threads and descriptors come from the daemon's own `server:memory` after the turns.

| Tool | First session | Each additional | vs horon (additional) | Threads/session | FDs/session | First (post-turn) | Each additional (post-turn) |
|---|---|---|---|---|---|---|---|
| horon | 36.8 MB | 133.3 KB | baseline | 0.66 | 1.00 | 44.0 MB | 501.3 KB |
| jcode | 42.6 MB | 163.2 KB | 1.22× more | 0.00 | — | 46.1 MB | 299.5 KB |
| Claude Code | 150.6 MB | 150.0 MB | 1,152× more | — | — | 184.9 MB | 181.2 MB |
| Codex CLI | 129.7 MB | 132.7 MB | 1,020× more | — | — | 162.8 MB | 164.4 MB |
| OpenCode | 241.1 MB | 300.3 KB | 2.25× more | — | — | 453.9 MB | 20.8 MB |
| pi | 109.3 MB | 109.2 MB | 839.14× more | — | — | 119.2 MB | 132.2 MB |
| oh-my-pi (omp) | 347.5 MB | 56.1 MB | 430.87× more | — | — | 365.8 MB | 358.2 MB |
| oh-my-openagent (omo) | 297.2 MB | 34.0 MB | 260.85× more | — | — | 594.1 MB | 594.0 MB |

Threads/descriptors unavailable: jcode (server:memory did not report fd_count)

**Post-turn tree RSS per session count**

| Tool | 1 session(s) | 5 session(s) | 10 session(s) |
|---|---|---|---|
| horon | 44.0 MB | 46.4 MB | 48.4 MB |
| jcode | 46.1 MB | 47.3 MB | 48.7 MB |
| Claude Code | 184.9 MB | 922.2 MB | 1.8 GB |
| Codex CLI | 162.8 MB | 814.3 MB | 1.6 GB |
| OpenCode | 453.9 MB | 490.3 MB | 637.5 MB |
| pi | 119.2 MB | 646.5 MB | 1.3 GB |
| oh-my-pi (omp) | 365.8 MB | 1.7 GB | 3.5 GB |
| oh-my-openagent (omo) | 594.1 MB | 3.0 GB | 5.8 GB |

#### 기동

```text
Benchmarking: /home/runner/work/agent/agent/bench-bin/horon-agent
============================================================

--help (binary load):
  Min:    3.40 ms
  Max:    3.82 ms
  Mean:   3.60 ms
  Median: 3.55 ms
  Stdev:  0.15 ms

--version:
  Min:    3.52 ms
  Max:    3.73 ms
  Mean:   3.61 ms
  Median: 3.62 ms
  Stdev:  0.06 ms

Measuring isolated server startup (10 runs)...

Server ready (isolated socket connectable):
  Min:    20.60 ms
  Max:    20.71 ms
  Mean:   20.64 ms
  Median: 20.63 ms
  Stdev:  0.03 ms

Measuring harness bridge readiness (10 runs)...

Bridge stage: api socket connectable:
  Min:    20.61 ms
  Max:    20.76 ms
  Mean:   20.70 ms
  Median: 20.70 ms
  Stdev:  0.05 ms

Bridge stage: hello_ok:
  Min:    21.29 ms
  Max:    21.61 ms
  Mean:   21.44 ms
  Median: 21.45 ms
  Stdev:  0.10 ms

Bridge stage: no-op reply:
  Min:    21.69 ms
  Max:    22.15 ms
  Mean:   21.86 ms
  Median: 21.85 ms
  Stdev:  0.13 ms

Bridge total (socket -> no-op):
  Min:    21.69 ms
  Max:    22.15 ms
  Mean:   21.86 ms
  Median: 21.85 ms
  Stdev:  0.13 ms

============================================================
Summary:
  Binary load median:      ~3.6 ms
  Server ready median:     ~20.6 ms
  Bridge total median:     ~21.8 ms
```

#### 세션 로딩 (COLD)

```text
binary   /home/runner/work/agent/agent/bench-bin/examples/loading_probe (e2dfdaafd454)
mode     cold
protocol COLD: every measured run gets its own freshly copied HOME; no derived cache from an earlier run exists, so each sample is a first load of the fixture.
fixtures /tmp/horon-loading-bench-jppxb6rr
note     --warmup 3 is ignored in cold mode: every sample gets a fresh HOME
fixture events=0 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    0.008/0.008    p95    0.011/0.010    delta  -0.000 ms  control-no-change
  skill_request_overlay p50    0.052/0.052    p95    0.079/0.053    delta  -0.000 ms  control-no-change
  mcp_config_load   p50    0.073/0.073    p95    0.092/0.087    delta  +0.000 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.001/0.000    delta  -0.000 ms  control-no-change
  session_save      p50    8.066/7.903    p95    8.833/8.501    delta  -0.163 ms  control-no-change
  total store_read_bytes p50 23072  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 23068..23080 (4 distinct values)
  SPREAD   b session_save store_read_bytes 23056..23080 (6 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    1.759/1.756    p95    2.250/1.853    delta  -0.002 ms  control-no-change
  skill_request_overlay p50    2.509/2.505    p95    2.882/2.579    delta  -0.004 ms  control-no-change
  mcp_config_load   p50    0.081/0.080    p95    0.099/0.083    delta  -0.001 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.001/0.001    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.001/0.001    delta  -0.000 ms  control-no-change
  session_save      p50    9.327/9.190    p95   13.693/13.711   delta  -0.136 ms  control-no-change
  total store_read_bytes p50 23070  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 23048..23080 (5 distinct values)
  SPREAD   b session_save store_read_bytes 23056..23080 (7 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.008/0.008    p95    0.011/0.010    delta  -0.000 ms  control-no-change
  skill_request_overlay p50    0.051/0.052    p95    0.052/0.072    delta  +0.001 ms  control-no-change
  mcp_config_load   p50    0.073/0.074    p95    0.092/0.105    delta  +0.000 ms  control-no-change
  attach_history    p50  258.518/257.634  p95  263.963/259.380  delta  -0.884 ms  control-no-change
  resume_ready      p50   38.507/38.359   p95   39.366/38.657   delta  -0.149 ms  control-no-change
  provider_dispatch p50    1.265/1.268    p95    1.311/1.318    delta  +0.003 ms  control-no-change
  session_save      p50    7.015/6.545    p95    7.945/7.304    delta  -0.471 ms  control-no-change
  total store_read_bytes p50 5311396  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189187..4189191 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099129..1099133 (2 distinct values)
  SPREAD   a session_save store_read_bytes 23062..23086 (6 distinct values)
  SPREAD   b attach_history store_read_bytes 4189189..4189191 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099132..1099133 (2 distinct values)
  SPREAD   b session_save store_read_bytes 23062..23082 (7 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    1.789/1.798    p95    1.838/1.860    delta  +0.009 ms  control-no-change
  skill_request_overlay p50    2.519/2.519    p95    2.587/2.589    delta  +0.000 ms  control-no-change
  mcp_config_load   p50    0.082/0.081    p95    0.088/0.082    delta  -0.001 ms  control-no-change
  attach_history    p50  261.380/260.308  p95  267.160/270.688  delta  -1.072 ms  control-no-change
  resume_ready      p50   38.578/38.644   p95   38.879/39.325   delta  +0.067 ms  control-no-change
  provider_dispatch p50    1.270/1.288    p95    1.334/1.306    delta  +0.018 ms  control-no-change
  session_save      p50    6.979/6.579    p95   11.316/7.237    delta  -0.400 ms  control-no-change
  total store_read_bytes p50 5311394  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189187..4189191 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099129..1099133 (3 distinct values)
  SPREAD   a session_save store_read_bytes 23058..23086 (7 distinct values)
  SPREAD   b attach_history store_read_bytes 4189187..4189191 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099129..1099133 (3 distinct values)
  SPREAD   b session_save store_read_bytes 23068..23082 (6 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.008/0.008    p95    0.011/0.010    delta  -0.000 ms  control-no-change
  skill_request_overlay p50    0.051/0.051    p95    0.056/0.052    delta  -0.000 ms  control-no-change
  mcp_config_load   p50    0.073/0.072    p95    0.082/0.073    delta  -0.001 ms  control-no-change
  attach_history    p50 3473.513/3460.726 p95 3505.842/3489.637 delta -12.787 ms  control-no-change
  resume_ready      p50  466.320/461.400  p95  482.636/491.117  delta  -4.920 ms  control-no-change
  provider_dispatch p50   17.397/17.180   p95   18.007/22.208   delta  -0.217 ms  control-no-change
  session_save      p50    6.605/6.840    p95    7.432/7.447    delta  +0.235 ms  control-no-change
  total store_read_bytes p50 53417609  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 23065..23085 (6 distinct values)
  SPREAD   b session_save store_read_bytes 23065..23089 (7 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    1.738/1.753    p95    1.801/1.774    delta  +0.015 ms  control-no-change
  skill_request_overlay p50    2.419/2.428    p95    2.472/2.624    delta  +0.009 ms  control-no-change
  mcp_config_load   p50    0.079/0.079    p95    0.081/0.086    delta  -0.000 ms  control-no-change
  attach_history    p50 3461.760/3470.606 p95 3490.570/3506.083 delta  +8.846 ms  control-no-change
  resume_ready      p50  451.753/450.040  p95  473.030/493.964  delta  -1.714 ms  control-no-change
  provider_dispatch p50   16.457/16.466   p95   18.117/17.510   delta  +0.010 ms  control-no-change
  session_save      p50    6.971/6.656    p95    7.629/7.045    delta  -0.315 ms  control-no-change
  total store_read_bytes p50 53417610  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184285..42184289 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210239..11210243 (3 distinct values)
  SPREAD   a session_save store_read_bytes 23069..23089 (5 distinct values)
  SPREAD   b attach_history store_read_bytes 42184285..42184289 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210239..11210243 (3 distinct values)
  SPREAD   b session_save store_read_bytes 23069..23089 (5 distinct values)
report   bench-results/loading-cold.json
mode cold, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### 세션 로딩 (WARM)

```text
binary   /home/runner/work/agent/agent/bench-bin/examples/loading_probe (e2dfdaafd454)
mode     warm
protocol WARM: one HOME per side, --warmup discarded runs per side, then measured runs alternate between the two sides on those same HOMEs.
fixtures /tmp/horon-loading-bench-kium_6ih
fixture events=0 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    0.015/0.015    p95    0.019/0.017    delta  -0.000 ms  control-no-change
  skill_request_overlay p50    0.105/0.105    p95    0.140/0.130    delta  +0.000 ms  control-no-change
  mcp_config_load   p50    0.144/0.144    p95    0.163/0.172    delta  -0.000 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.001    delta  -0.000 ms  control-no-change
  session_save      p50    8.837/8.493    p95   13.709/11.308   delta  -0.345 ms  control-no-change
  total store_read_bytes p50 23785  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23057, 23069, 23071, 23783, 23787, 23799, 24497, 24505, 24513]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23063, 23065, 23073, 23775, 23787, 24493, 24501, 24503, 24517]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23057..24513 (9 distinct values)
  SPREAD   b session_save store_read_bytes 23063..24517 (9 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    2.694/2.720    p95    2.778/2.801    delta  +0.027 ms  control-no-change
  skill_request_overlay p50    4.663/4.705    p95    4.728/4.748    delta  +0.042 ms  control-no-change
  mcp_config_load   p50    0.147/0.147    p95    0.163/0.157    delta  -0.000 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  session_save      p50    9.393/9.424    p95    9.817/10.689   delta  +0.031 ms  control-no-change
  total store_read_bytes p50 23799  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23061, 23065, 23069, 23791, 23795, 23803, 24485, 24501, 24509, 24525]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23065, 23073, 23077, 23779, 23787, 23791, 24497, 24503, 24505, 24525]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23061..24525 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23065..24525 (10 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.015/0.015    p95    0.020/0.017    delta  -0.000 ms  control-no-change
  skill_request_overlay p50    0.106/0.106    p95    0.117/0.106    delta  -0.000 ms  control-no-change
  mcp_config_load   p50    0.143/0.143    p95    0.162/0.148    delta  -0.000 ms  control-no-change
  attach_history    p50   98.213/98.442   p95  100.480/99.974   delta  +0.230 ms  control-no-change
  resume_ready      p50   53.085/53.148   p95   53.830/53.809   delta  +0.063 ms  control-no-change
  provider_dispatch p50    1.313/1.323    p95    1.354/1.352    delta  +0.010 ms  control-no-change
  session_save      p50    8.338/8.301    p95   13.279/8.916    delta  -0.037 ms  control-no-change
  total store_read_bytes p50 2841353  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23786, 23788, 24514, 24516, 25224, 25230, 25946, 25948, 26658, 26666]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23792, 23804, 24510, 25232, 25236, 25946, 25954, 26660, 26668]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23786..26666 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23792..26668 (9 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    2.787/2.766    p95    2.878/2.865    delta  -0.021 ms  control-no-change
  skill_request_overlay p50    4.698/4.681    p95    4.735/4.762    delta  -0.016 ms  control-no-change
  mcp_config_load   p50    0.149/0.148    p95    0.166/0.154    delta  -0.001 ms  control-no-change
  attach_history    p50   98.968/98.908   p95  100.734/99.710   delta  -0.060 ms  control-no-change
  resume_ready      p50   53.421/53.428   p95   54.548/54.223   delta  +0.007 ms  control-no-change
  provider_dispatch p50    1.332/1.330    p95    1.369/1.345    delta  -0.001 ms  control-no-change
  session_save      p50    9.567/9.470    p95   10.910/11.357   delta  -0.097 ms  control-no-change
  total store_read_bytes p50 2841361  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23778, 23788, 24504, 24510, 25218, 25236, 25948, 25954, 26660, 26662]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23792, 23794, 24502, 24516, 25222, 25226, 25944, 26650, 26658]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23778..26662 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23792..26658 (9 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.015/0.015    p95    0.017/0.017    delta  +0.000 ms  control-no-change
  skill_request_overlay p50    0.105/0.108    p95    0.119/0.127    delta  +0.003 ms  control-drift
  mcp_config_load   p50    0.144/0.149    p95    0.171/0.194    delta  +0.005 ms  control-drift
  attach_history    p50 1073.649/1080.340 p95 1089.247/1100.382 delta  +6.691 ms  control-no-change
  resume_ready      p50  576.587/575.282  p95  580.797/592.517  delta  -1.305 ms  control-no-change
  provider_dispatch p50   13.980/14.127   p95   14.379/14.621   delta  +0.147 ms  control-no-change
  session_save      p50    7.941/8.028    p95    8.233/8.329    delta  +0.087 ms  control-no-change
  total store_read_bytes p50 28640379  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23781, 23799, 24499, 24513, 25225, 25231, 25947, 25961, 26661, 26663]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23781, 23795, 24487, 24517, 25233, 25239, 25945, 25947, 26667, 26669]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23781..26663 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23781..26669 (10 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    2.661/2.648    p95    2.809/2.716    delta  -0.013 ms  control-no-change
  skill_request_overlay p50    4.647/4.676    p95    4.810/4.737    delta  +0.030 ms  control-no-change
  mcp_config_load   p50    0.147/0.147    p95    0.170/0.160    delta  -0.000 ms  control-no-change
  attach_history    p50 1072.957/1070.380 p95 1088.144/1086.777 delta  -2.577 ms  control-no-change
  resume_ready      p50  574.240/575.223  p95  581.553/579.169  delta  +0.982 ms  control-no-change
  provider_dispatch p50   14.018/13.840   p95   14.178/14.217   delta  -0.178 ms  control-no-change
  session_save      p50    9.539/9.169    p95   12.167/9.747    delta  -0.370 ms  control-no-change
  total store_read_bytes p50 28640376  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23791, 23803, 24505, 24509, 25227, 25239, 25929, 25961, 26659, 26667]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23779, 23785, 24497, 24503, 25219, 25945, 25949, 26671, 26675]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23791..26667 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23779..26675 (9 distinct values)
report   bench-results/loading-warm.json
mode warm, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### Tool latency

| Tool case | median | p95 | min | max | samples |
|---|---|---|---|---|---|
| read (tiny file) | 0.2 ms | 0.3 ms | 0.2 ms | 0.3 ms | 5 |
| read (1000 lines) | 0.3 ms | 0.3 ms | 0.2 ms | 0.3 ms | 5 |
| read (10 lines @ offset) | 0.2 ms | 0.3 ms | 0.2 ms | 0.3 ms | 5 |
| read (repo Cargo.toml) | 0.3 ms | 0.3 ms | 0.2 ms | 0.3 ms | 5 |
| write (small) | 39.8 ms | 110.1 ms | 0.9 ms | 110.1 ms | 5 |
| write (10KB) | 1.0 ms | 1.1 ms | 1.0 ms | 1.1 ms | 5 |
| edit (simple replace) | 1.0 ms | 1.0 ms | 1.0 ms | 1.0 ms | 5 |
| ls (scripts dir) | 0.4 ms | 0.5 ms | 0.4 ms | 0.5 ms | 5 |
| agentgrep (grep in tree) | 1.1 ms | 1.1 ms | 1.0 ms | 1.1 ms | 5 |
| agentgrep (find **/*.rs) | 0.9 ms | 1.0 ms | 0.9 ms | 1.0 ms | 5 |
| bash (true) | 1.5 ms | 1.6 ms | 1.5 ms | 1.6 ms | 5 |
| bash (echo hello) | 1.4 ms | 1.5 ms | 1.4 ms | 1.5 ms | 5 |
| bash (ls fixture tree) | 2.5 ms | 2.5 ms | 2.4 ms | 2.5 ms | 5 |
| invalid tool (dispatch only) | 0.0 ms | 0.1 ms | 0.0 ms | 0.1 ms | 5 |

#### Reload storm

1 resume(s) per level; each level resumes sessions no other level has opened, from a freshly started daemon.

| Concurrent resumes | history median | p95 | max | storm wall | failures |
|---|---|---|---|---|---|
| 1 | 79.3 ms | 79.3 ms | 79.3 ms | 79.5 ms | 0 |
| 10 | 197.2 ms | 300.9 ms | 300.9 ms | 302.0 ms | 0 |
| 40 | 928.5 ms | 1.08 s | 1.08 s | 1.08 s | 0 |

#### Compile loop

| Checkpoint | Touched | median | min | max | samples |
|---|---|---|---|---|---|
| warm_noop | — | 277.7 ms | 277.7 ms | 277.7 ms | 1 |
| leaf_touch | src/main.rs | 424.4 ms | 424.4 ms | 424.4 ms | 1 |
| core_touch | crates/app-core/src/lib.rs | 21.06 s | 21.06 s | 21.06 s | 1 |
| build_meta_touch | crates/build-meta/src/lib.rs | 29.38 s | 29.38 s | 29.38 s | 1 |

### macOS

#### Coding agent comparison (headless, mock model)

Host: darwin/arm64, 3 CPUs, Python 3.12.10. Baseline: **horon**. Samples per one-shot, skill and MCP cell: 5.

Skipped: syscalls (strace not available)

| Tool | Version | `--version` (median) | Install size |
|---|---|---|---|
| horon | horon-agent v0.1.0-dev (8724ad6) | 7.3 ms | 94.5 MB |
| jcode | jcode v0.99.1 (7206efd0c) | 9.6 ms | 136.3 MB |
| Claude Code | 2.1.317 (Claude Code) | 14.7 ms | 596.8 MB |
| Codex CLI | codex-cli 0.166.0 | 43.3 ms | 346.5 MB |
| OpenCode | 1.19.5 | 310.0 ms | 344.6 MB |
| pi | 1.5.0 | 209.0 ms | 108.9 MB |
| oh-my-pi (omp) | omp/18.18.0 | 530.2 ms | 273.3 MB |
| oh-my-openagent (omo) | omo 5.1.64 (engine: senpi 2026.11.5-1) | 143.6 ms | 502.2 MB |

**One-shot headless prompt** (includes startup, one `ls` tool call, final answer)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Tool round-trip | Note |
|---|---|---|---|---|---|---|---|
| horon | 61.3 ms | 60.0 ms–290.1 ms | baseline | 31.2 MB | baseline | yes | — |
| jcode | 301.2 ms | 292.2 ms–504.6 ms | 4.91× more | 34.0 MB | 1.09× more | yes | — |
| Claude Code | 315.4 ms | 290.0 ms–651.4 ms | 5.14× more | 195.1 MB | 6.24× more | yes | — |
| Codex CLI | 350.0 ms | 254.2 ms–2.12 s | 5.71× more | 142.5 MB | 4.56× more | yes | — |
| OpenCode | 2.11 s | 1.78 s–2.69 s | 34.40× more | 705.4 MB | 22.57× more | yes | — |
| pi | 677.8 ms | 491.0 ms–1.30 s | 11.05× more | 120.3 MB | 3.85× more | yes | — |
| oh-my-pi (omp) | 1.57 s | 1.42 s–1.72 s | 25.63× more | 406.9 MB | 13.02× more | yes | — |
| oh-my-openagent (omo) | 8.38 s | 8.20 s–8.66 s | 136.69× more | 856.6 MB | 27.41× more | yes | — |

**Skill invocation** (process start to final answer, including skill discovery and one load of the fixture skill)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | Note |
|---|---|---|---|---|---|---|---|---|
| horon | 56.2 ms | 52.4 ms–356.8 ms | baseline | 30.5 MB | baseline | skill-uri | yes | — |
| jcode | 202.5 ms | 188.6 ms–257.4 ms | 3.61× more | 31.3 MB | 1.03× more | skill-tool | yes | — |
| Claude Code | 295.6 ms | 265.2 ms–322.3 ms | 5.26× more | 191.5 MB | 6.29× more | skill-tool | yes | — |
| Codex CLI | 364.2 ms | 340.2 ms–425.2 ms | 6.48× more | 143.8 MB | 4.72× more | file-read | yes | — |
| OpenCode | 2.70 s | 2.49 s–2.98 s | 47.99× more | 691.7 MB | 22.71× more | skill-tool | yes | — |
| pi | 406.7 ms | 396.0 ms–454.6 ms | 7.24× more | 151.0 MB | 4.96× more | file-read | yes | — |
| oh-my-pi (omp) | 1.54 s | 1.31 s–1.76 s | 27.49× more | 403.9 MB | 13.26× more | file-read | yes | — |
| oh-my-openagent (omo) | 6.84 s | 6.77 s–11.74 s | 121.80× more | 879.1 MB | 28.87× more | file-read | yes | — |

**MCP tool call** (process start to final answer, including starting the fixture stdio MCP server and one `bench_echo` call)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | MCP ready | Note |
|---|---|---|---|---|---|---|---|---|---|
| horon | 88.1 ms | 84.5 ms–95.5 ms | baseline | 19.3 MB | baseline | mcp-call | yes | 71.6 ms | — |
| jcode | 313.9 ms | 305.8 ms–321.5 ms | 3.56× more | 51.9 MB | 2.69× more | mcp-action | yes | 295.8 ms | — |
| Claude Code | 416.9 ms | 357.9 ms–517.5 ms | 4.73× more | 220.0 MB | 11.39× more | mcp-tool | yes | 312.7 ms | — |
| Codex CLI | 345.4 ms | 284.4 ms–421.9 ms | 3.92× more | 120.5 MB | 6.24× more | mcp-tool | yes | 197.8 ms | — |
| OpenCode | 2.47 s | 2.29 s–2.55 s | 28.00× more | 723.9 MB | 37.48× more | mcp-tool | yes | 2.09 s | — |
| pi | 418.2 ms | 404.9 ms–440.4 ms | 4.75× more | 137.4 MB | 7.11× more | mcp-tool | yes | 364.2 ms | — |
| oh-my-pi (omp) | 1.50 s | 1.45 s–1.74 s | 17.08× more | 378.5 MB | 19.60× more | mcp-route | yes | 1.43 s | — |
| oh-my-openagent (omo) | — | — | — | — | — | mcp-tool | no | — | failed: the fixture sentinel never came back |

**Session memory** (idle after 3 turn(s) per session; sizes are process-tree sums, RSS; PSS also recorded on Linux). "Each additional" is the least-squares slope across the measured session counts; 0 B means growth stayed below RSS sampling noise. Threads and descriptors come from the daemon's own `server:memory` after the turns.

| Tool | First session | Each additional | vs horon (additional) | Threads/session | FDs/session | First (post-turn) | Each additional (post-turn) |
|---|---|---|---|---|---|---|---|
| horon | 28.7 MB | 157.2 KB | baseline | 0.78 | 1.00 | 34.7 MB | 383.7 KB |
| jcode | 36.3 MB | 229.5 KB | 1.46× more | — | — | 39.9 MB | 370.1 KB |
| Claude Code | 156.7 MB | 157.2 MB | 1,024× more | — | — | 188.5 MB | 185.1 MB |
| Codex CLI | 122.2 MB | 77.2 MB | 502.83× more | — | — | 106.7 MB | 107.4 MB |
| OpenCode | 263.1 MB | 126.4 KB | 1.24× less | — | — | 568.7 MB | 11.6 MB |
| pi | 117.6 MB | 115.9 MB | 754.71× more | — | — | 148.0 MB | 147.9 MB |
| oh-my-pi (omp) | 373.7 MB | 178.9 MB | 1,165× more | — | — | 442.8 MB | 310.8 MB |
| oh-my-openagent (omo) | 57.6 MB | 44.3 MB | 288.45× more | — | — | 816.1 MB | 326.7 MB |

Threads/descriptors unavailable: jcode (server:memory did not report thread_count, fd_count)

**Post-turn tree RSS per session count**

| Tool | 1 session(s) | 5 session(s) | 10 session(s) |
|---|---|---|---|
| horon | 34.7 MB | 36.4 MB | 38.1 MB |
| jcode | 39.9 MB | 41.3 MB | 43.2 MB |
| Claude Code | 188.5 MB | 932.8 MB | 1.8 GB |
| Codex CLI | 106.7 MB | 534.6 MB | 1.0 GB |
| OpenCode | 568.7 MB | 598.1 MB | 671.5 MB |
| pi | 148.0 MB | 739.4 MB | 1.4 GB |
| oh-my-pi (omp) | 442.8 MB | 1.8 GB | 3.2 GB |
| oh-my-openagent (omo) | 816.1 MB | 3.0 GB | 3.7 GB |

#### 기동

```text
Benchmarking: /Users/runner/work/agent/agent/bench-bin/horon-agent
============================================================

--help (binary load):
  Min:    10.14 ms
  Max:    20.53 ms
  Mean:   14.66 ms
  Median: 14.06 ms
  Stdev:  3.32 ms

--version:
  Min:    11.79 ms
  Max:    22.65 ms
  Mean:   16.50 ms
  Median: 16.34 ms
  Stdev:  3.69 ms

Measuring isolated server startup (10 runs)...

Server ready (isolated socket connectable):
  Min:    28.75 ms
  Max:    103.16 ms
  Mean:   65.73 ms
  Median: 57.87 ms
  Stdev:  30.09 ms

Measuring harness bridge readiness (10 runs)...

Bridge stage: api socket connectable:
  Min:    46.04 ms
  Max:    136.42 ms
  Mean:   89.60 ms
  Median: 91.45 ms
  Stdev:  31.98 ms

Bridge stage: hello_ok:
  Min:    48.48 ms
  Max:    137.71 ms
  Mean:   91.23 ms
  Median: 92.89 ms
  Stdev:  31.56 ms

Bridge stage: no-op reply:
  Min:    49.04 ms
  Max:    138.71 ms
  Mean:   92.09 ms
  Median: 93.73 ms
  Stdev:  31.50 ms

Bridge total (socket -> no-op):
  Min:    49.04 ms
  Max:    138.71 ms
  Mean:   92.09 ms
  Median: 93.73 ms
  Stdev:  31.50 ms

============================================================
Summary:
  Binary load median:      ~14.1 ms
  Server ready median:     ~57.9 ms
  Bridge total median:     ~93.7 ms
```

#### 세션 로딩 (COLD)

```text
binary   /Users/runner/work/agent/agent/bench-bin/examples/loading_probe (5f58be645d59)
mode     cold
protocol COLD: every measured run gets its own freshly copied HOME; no derived cache from an earlier run exists, so each sample is a first load of the fixture.
fixtures /var/folders/h5/t85nd63d71x27_8gl5gw3l_w0000gn/T/horon-loading-bench-cd5j6rkd
note     --warmup 3 is ignored in cold mode: every sample gets a fresh HOME
fixture events=0 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    0.026/0.023    p95    0.036/0.028    delta  -0.004 ms  control-no-change
  skill_request_overlay p50    0.247/0.221    p95    0.352/0.258    delta  -0.026 ms  control-no-change
  mcp_config_load   p50    0.283/0.262    p95    0.358/0.432    delta  -0.021 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.001/0.000    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.001/0.000    delta  -0.000 ms  control-no-change
  session_save      p50   17.038/16.319   p95   43.440/28.905   delta  -0.719 ms  control-no-change
  total store_read_bytes p50 22950  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 22942..22954 (4 distinct values)
  SPREAD   b session_save store_read_bytes 22930..22958 (7 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    4.419/4.230    p95    5.473/9.114    delta  -0.188 ms  control-no-change
  skill_request_overlay p50    5.731/5.261    p95    9.902/8.190    delta  -0.471 ms  control-no-change
  mcp_config_load   p50    0.274/0.270    p95    0.883/0.306    delta  -0.004 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.001/0.000    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.001/0.000    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.001/0.000    delta  +0.000 ms  control-no-change
  session_save      p50   20.786/19.244   p95   52.394/31.830   delta  -1.542 ms  control-no-change
  total store_read_bytes p50 22942  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 22930..22954 (6 distinct values)
  SPREAD   b session_save store_read_bytes 22934..22954 (6 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.029/0.025    p95    0.042/0.034    delta  -0.004 ms  control-no-change
  skill_request_overlay p50    0.233/0.242    p95    0.310/0.275    delta  +0.009 ms  control-no-change
  mcp_config_load   p50    0.258/0.267    p95    0.327/0.316    delta  +0.009 ms  control-no-change
  attach_history    p50  292.054/289.960  p95  356.914/341.367  delta  -2.094 ms  control-no-change
  resume_ready      p50   61.983/62.169   p95   73.554/84.288   delta  +0.187 ms  control-no-change
  provider_dispatch p50    1.075/1.105    p95    1.448/1.543    delta  +0.030 ms  control-no-change
  session_save      p50   13.992/17.097   p95   17.295/24.647   delta  +3.105 ms  control-no-change
  total store_read_bytes p50 5311300  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189203..4189207 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099141..1099145 (3 distinct values)
  SPREAD   a session_save store_read_bytes 22938..22964 (9 distinct values)
  SPREAD   b attach_history store_read_bytes 4189205..4189207 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099144..1099145 (2 distinct values)
  SPREAD   b session_save store_read_bytes 22932..22968 (7 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    4.055/4.457    p95    9.558/5.527    delta  +0.402 ms  control-no-change
  skill_request_overlay p50    5.258/5.684    p95    9.174/8.547    delta  +0.426 ms  control-no-change
  mcp_config_load   p50    0.272/0.284    p95    0.776/0.550    delta  +0.012 ms  control-no-change
  attach_history    p50  314.932/292.493  p95  414.941/424.108  delta -22.440 ms  control-no-change
  resume_ready      p50   67.200/66.436   p95   83.476/82.425   delta  -0.764 ms  control-no-change
  provider_dispatch p50    1.066/1.140    p95    1.573/2.169    delta  +0.074 ms  control-no-change
  session_save      p50   16.336/22.202   p95   26.719/39.106   delta  +5.866 ms  control-no-change
  total store_read_bytes p50 5311310  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189203..4189207 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099141..1099145 (3 distinct values)
  SPREAD   a session_save store_read_bytes 22936..22972 (9 distinct values)
  SPREAD   b attach_history store_read_bytes 4189203..4189207 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099141..1099145 (3 distinct values)
  SPREAD   b session_save store_read_bytes 22940..22968 (9 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.031/0.029    p95    0.038/0.061    delta  -0.002 ms  control-no-change
  skill_request_overlay p50    0.237/0.244    p95    0.520/0.621    delta  +0.008 ms  control-no-change
  mcp_config_load   p50    0.263/0.308    p95    0.650/0.451    delta  +0.045 ms  control-no-change
  attach_history    p50 4012.170/4223.070 p95 6245.852/5639.612 delta +210.900 ms  control-no-change
  resume_ready      p50  945.983/920.839  p95 1238.557/1206.673 delta -25.143 ms  control-no-change
  provider_dispatch p50   15.669/13.540   p95   21.579/37.960   delta  -2.129 ms  control-no-change
  session_save      p50   31.159/33.627   p95  164.394/90.924   delta  +2.468 ms  control-no-change
  total store_read_bytes p50 53417515  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184303..42184305 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210254..11210255 (2 distinct values)
  SPREAD   a session_save store_read_bytes 22946..22963 (6 distinct values)
  SPREAD   b attach_history store_read_bytes 42184301..42184305 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210251..11210255 (2 distinct values)
  SPREAD   b session_save store_read_bytes 22949..22971 (5 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    4.220/4.166    p95    5.270/5.074    delta  -0.054 ms  control-no-change
  skill_request_overlay p50    5.345/5.498    p95    7.663/9.194    delta  +0.153 ms  control-no-change
  mcp_config_load   p50    0.271/0.273    p95    0.527/0.547    delta  +0.002 ms  control-no-change
  attach_history    p50 3513.556/3242.818 p95 4541.435/3710.403 delta -270.738 ms  control-no-change
  resume_ready      p50  711.772/642.248  p95  986.116/715.815  delta -69.524 ms  control-no-change
  provider_dispatch p50   10.671/10.331   p95   16.505/16.918   delta  -0.340 ms  control-no-change
  session_save      p50   25.628/21.535   p95  103.860/60.054   delta  -4.093 ms  control-no-change
  total store_read_bytes p50 53417517  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184299..42184305 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210250..11210255 (3 distinct values)
  SPREAD   a session_save store_read_bytes 22945..22967 (8 distinct values)
  SPREAD   b attach_history store_read_bytes 42184295..42184305 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210246..11210255 (3 distinct values)
  SPREAD   b session_save store_read_bytes 22942..22967 (6 distinct values)
report   bench-results/loading-cold.json
mode cold, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### 세션 로딩 (WARM)

```text
binary   /Users/runner/work/agent/agent/bench-bin/examples/loading_probe (5f58be645d59)
mode     warm
protocol WARM: one HOME per side, --warmup discarded runs per side, then measured runs alternate between the two sides on those same HOMEs.
fixtures /var/folders/h5/t85nd63d71x27_8gl5gw3l_w0000gn/T/horon-loading-bench-ew_27mcr
fixture events=0 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    0.028/0.026    p95    0.111/0.103    delta  -0.002 ms  control-no-change
  skill_request_overlay p50    0.237/0.247    p95    0.636/0.699    delta  +0.010 ms  control-no-change
  mcp_config_load   p50    0.257/0.309    p95    0.727/0.741    delta  +0.051 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.001    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.001/0.001    delta  -0.000 ms  control-no-change
  session_save      p50   20.223/20.027   p95   24.050/44.871   delta  -0.197 ms  control-no-change
  total store_read_bytes p50 23673  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [22947, 22955, 22959, 23653, 23669, 23677, 24367, 24375, 24381, 24383]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [22943, 22951, 23661, 23665, 23673, 24367, 24375, 24379]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 22947..24383 (10 distinct values)
  SPREAD   b session_save store_read_bytes 22943..24379 (8 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    4.598/4.358    p95    9.648/8.400    delta  -0.240 ms  control-no-change
  skill_request_overlay p50    5.984/5.520    p95   11.187/11.434   delta  -0.464 ms  control-no-change
  mcp_config_load   p50    0.300/0.272    p95    0.630/0.444    delta  -0.029 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.001/0.000    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  session_save      p50   18.317/15.363   p95   23.887/27.984   delta  -2.954 ms  control-no-change
  total store_read_bytes p50 23659  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [22941, 22945, 22949, 22951, 23655, 23663, 23671, 24373, 24377, 24381]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [22939, 22963, 23665, 23669, 24367, 24375]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 22941..24381 (10 distinct values)
  SPREAD   b session_save store_read_bytes 22939..24375 (6 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.048/0.030    p95    0.074/0.081    delta  -0.017 ms  control-no-change
  skill_request_overlay p50    0.269/0.237    p95    0.682/0.701    delta  -0.032 ms  control-no-change
  mcp_config_load   p50    0.280/0.270    p95    1.077/0.659    delta  -0.010 ms  control-no-change
  attach_history    p50  176.182/152.049  p95  229.577/187.450  delta -24.133 ms  control-no-change
  resume_ready      p50   98.123/117.206  p95  157.125/155.287  delta +19.083 ms  control-no-change
  provider_dispatch p50    1.325/1.880    p95    2.311/2.783    delta  +0.556 ms  control-no-change
  session_save      p50   28.396/27.739   p95  102.846/37.484   delta  -0.657 ms  control-no-change
  total store_read_bytes p50 2841255  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23670, 23678, 24384, 24388, 25096, 25098, 25810, 25820, 26520, 26526]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23668, 23682, 24386, 24388, 25090, 25096, 25810, 25820, 26520, 26528]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23670..26526 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23668..26528 (10 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    5.157/5.094    p95   12.440/8.027    delta  -0.063 ms  control-no-change
  skill_request_overlay p50    7.459/7.124    p95   32.618/12.509   delta  -0.335 ms  control-no-change
  mcp_config_load   p50    0.311/0.290    p95    0.501/0.531    delta  -0.021 ms  control-no-change
  attach_history    p50  142.978/128.705  p95  166.048/179.937  delta -14.273 ms  control-no-change
  resume_ready      p50   80.734/82.624   p95   97.812/90.858   delta  +1.890 ms  control-no-change
  provider_dispatch p50    1.163/1.169    p95    2.231/1.728    delta  +0.006 ms  control-no-change
  session_save      p50   20.968/21.591   p95   32.917/30.179   delta  +0.624 ms  control-no-change
  total store_read_bytes p50 2841257  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23670, 24380, 24396, 25092, 25106, 25802, 25824, 26524, 26526]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23670, 23678, 24376, 24378, 25090, 25796, 26514, 26534]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23670..26526 (9 distinct values)
  SPREAD   b session_save store_read_bytes 23670..26534 (8 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.029/0.034    p95    0.061/0.086    delta  +0.005 ms  control-no-change
  skill_request_overlay p50    0.227/0.260    p95    0.805/0.630    delta  +0.033 ms  control-no-change
  mcp_config_load   p50    0.299/0.536    p95    0.899/0.981    delta  +0.237 ms  control-drift
  attach_history    p50 1551.646/1753.833 p95 1940.151/2151.198 delta +202.187 ms  control-drift
  resume_ready      p50  949.660/1049.862 p95 1232.501/1161.326 delta +100.202 ms  control-no-change
  provider_dispatch p50   14.152/19.763   p95   19.985/22.433   delta  +5.610 ms  control-no-change
  session_save      p50   26.354/35.015   p95   34.867/95.601   delta  +8.660 ms  control-drift
  total store_read_bytes p50 28640268  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23665, 23677, 24375, 24385, 25089, 25097, 25807, 25827, 26525]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23675, 23677, 24385, 24387, 25105, 25113, 25807, 25819, 26525, 26541]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23665..26525 (9 distinct values)
  SPREAD   b session_save store_read_bytes 23675..26541 (10 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    5.286/6.783    p95   13.700/13.105   delta  +1.497 ms  control-no-change
  skill_request_overlay p50    7.874/7.527    p95   17.508/12.687   delta  -0.347 ms  control-no-change
  mcp_config_load   p50    0.349/0.281    p95    0.843/0.861    delta  -0.068 ms  control-no-change
  attach_history    p50 2096.630/2034.740 p95 3067.823/2488.227 delta -61.891 ms  control-no-change
  resume_ready      p50 1077.765/1115.195 p95 1595.925/1506.036 delta +37.430 ms  control-no-change
  provider_dispatch p50   16.252/18.067   p95   33.558/25.858   delta  +1.815 ms  control-no-change
  session_save      p50   38.721/47.172   p95   95.950/125.433  delta  +8.451 ms  control-no-change
  total store_read_bytes p50 28640278  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23659, 23673, 24383, 24385, 25101, 25105, 25807, 25811, 26517, 26529]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23669, 23685, 24387, 24395, 25097, 25099, 25809, 25823, 26523, 26529]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23659..26529 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23669..26529 (10 distinct values)
report   bench-results/loading-warm.json
mode warm, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### Tool latency

| Tool case | median | p95 | min | max | samples |
|---|---|---|---|---|---|
| read (tiny file) | 0.5 ms | 0.6 ms | 0.4 ms | 0.6 ms | 5 |
| read (1000 lines) | 0.8 ms | 1.0 ms | 0.7 ms | 1.0 ms | 5 |
| read (10 lines @ offset) | 0.8 ms | 1.0 ms | 0.6 ms | 1.0 ms | 5 |
| read (repo Cargo.toml) | 0.6 ms | 0.7 ms | 0.4 ms | 0.7 ms | 5 |
| write (small) | 2.6 ms | 2.8 ms | 2.3 ms | 2.8 ms | 5 |
| write (10KB) | 3.1 ms | 3.2 ms | 2.6 ms | 3.2 ms | 5 |
| edit (simple replace) | 2.4 ms | 2.9 ms | 1.8 ms | 2.9 ms | 5 |
| ls (scripts dir) | 1.0 ms | 1.8 ms | 0.9 ms | 1.8 ms | 5 |
| agentgrep (grep in tree) | 4.3 ms | 5.3 ms | 4.0 ms | 5.3 ms | 5 |
| agentgrep (find **/*.rs) | 1.6 ms | 2.8 ms | 1.6 ms | 2.8 ms | 5 |
| bash (true) | 4.9 ms | 5.6 ms | 4.4 ms | 5.6 ms | 5 |
| bash (echo hello) | 6.5 ms | 7.9 ms | 5.7 ms | 7.9 ms | 5 |
| bash (ls fixture tree) | 8.8 ms | 10.9 ms | 8.8 ms | 10.9 ms | 5 |
| invalid tool (dispatch only) | 0.1 ms | 0.2 ms | 0.1 ms | 0.2 ms | 5 |

#### Reload storm

1 resume(s) per level; each level resumes sessions no other level has opened, from a freshly started daemon.

| Concurrent resumes | history median | p95 | max | storm wall | failures |
|---|---|---|---|---|---|
| 1 | 90.8 ms | 90.8 ms | 90.8 ms | 90.9 ms | 0 |
| 10 | 273.1 ms | 310.2 ms | 310.2 ms | 310.8 ms | 0 |
| 40 | 1.14 s | 1.15 s | 1.16 s | 1.16 s | 0 |

#### Compile loop

| Checkpoint | Touched | median | min | max | samples |
|---|---|---|---|---|---|
| warm_noop | — | 1.28 s | 1.28 s | 1.28 s | 1 |
| leaf_touch | src/main.rs | 568.9 ms | 568.9 ms | 568.9 ms | 1 |
| core_touch | crates/app-core/src/lib.rs | 27.23 s | 27.23 s | 27.23 s | 1 |
| build_meta_touch | crates/build-meta/src/lib.rs | 34.58 s | 34.58 s | 34.58 s | 1 |

### Windows

#### Coding agent comparison (headless, mock model)

Host: windows/AMD64, 4 CPUs, Python 3.12.10. Baseline: **horon**. Samples per one-shot, skill and MCP cell: 5.

Skipped: syscalls (strace not available)

| Tool | Version | `--version` (median) | Install size |
|---|---|---|---|
| horon | horon-agent v0.1.0-dev (8724ad6) | 12.1 ms | 94.2 MB |
| jcode | jcode v0.99.1 (7206efd0c) | 13.9 ms | 128.4 MB |
| Claude Code | 2.1.317 (Claude Code) | 36.7 ms | 645.2 MB |
| Codex CLI | codex-cli 0.166.0 | 88.4 ms | 479.4 MB |
| OpenCode | 1.19.5 | 518.0 ms | 605.3 MB |
| pi | 1.5.0 | 254.3 ms | 108.9 MB |
| oh-my-pi (omp) | omp/18.18.0 | 629.5 ms | 291.5 MB |
| oh-my-openagent (omo) | omo 5.1.64 (engine: senpi 2026.11.5-1) | 177.1 ms | 514.8 MB |

**One-shot headless prompt** (includes startup, one `ls` tool call, final answer)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Tool round-trip | Note |
|---|---|---|---|---|---|---|---|
| horon | 188.5 ms | 178.3 ms–267.0 ms | baseline | 35.3 MB | baseline | yes | — |
| jcode | 134.7 ms | 128.1 ms–327.4 ms | 1.40× less | 31.0 MB | 1.14× less | yes | — |
| Claude Code | 1.22 s | 1.15 s–2.02 s | 6.48× more | 273.2 MB | 7.75× more | yes | — |
| Codex CLI | 1.72 s | 1.54 s–2.16 s | 9.10× more | 258.1 MB | 7.32× more | yes | — |
| OpenCode | 4.53 s | 4.48 s–7.04 s | 24.05× more | 840.9 MB | 23.85× more | yes | — |
| pi | 454.0 ms | 427.6 ms–483.5 ms | 2.41× more | 125.2 MB | 3.55× more | yes | — |
| oh-my-pi (omp) | 3.04 s | 2.93 s–26.95 s | 16.14× more | 414.4 MB | 11.75× more | yes | — |
| oh-my-openagent (omo) | 22.07 s | 16.86 s–30.87 s | 117.07× more | 555.1 MB | 15.74× more | yes | — |

**Skill invocation** (process start to final answer, including skill discovery and one load of the fixture skill)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | Note |
|---|---|---|---|---|---|---|---|---|
| horon | 352.2 ms | 249.3 ms–440.0 ms | baseline | 24.9 MB | baseline | skill-uri | yes | — |
| jcode | 209.2 ms | 205.4 ms–239.2 ms | 1.68× less | 24.1 MB | 1.03× less | skill-tool | yes | — |
| Claude Code | 835.2 ms | 807.9 ms–871.0 ms | 2.37× more | 237.6 MB | 9.53× more | skill-tool | yes | — |
| Codex CLI | 2.14 s | 1.97 s–2.93 s | 6.07× more | 264.4 MB | 10.60× more | file-read | yes | — |
| OpenCode | 7.10 s | 6.86 s–7.93 s | 20.16× more | 749.4 MB | 30.05× more | skill-tool | yes | — |
| pi | 686.1 ms | 607.6 ms–914.4 ms | 1.95× more | 97.8 MB | 3.92× more | file-read | yes | — |
| oh-my-pi (omp) | 3.65 s | 3.11 s–4.25 s | 10.38× more | 406.5 MB | 16.30× more | file-read | yes | — |
| oh-my-openagent (omo) | 21.20 s | 18.51 s–24.53 s | 60.18× more | 528.2 MB | 21.18× more | file-read | yes | — |

**MCP tool call** (process start to final answer, including starting the fixture stdio MCP server and one `bench_echo` call)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | MCP ready | Note |
|---|---|---|---|---|---|---|---|---|---|
| horon | 283.7 ms | 279.0 ms–1.02 s | baseline | 37.3 MB | baseline | mcp-call | yes | 197.2 ms | — |
| jcode | 345.1 ms | 342.6 ms–413.0 ms | 1.22× more | 37.1 MB | same | mcp-action | yes | 237.0 ms | — |
| Claude Code | 878.5 ms | 869.2 ms–1.22 s | 3.10× more | 270.4 MB | 7.25× more | mcp-tool | yes | 759.9 ms | — |
| Codex CLI | 2.22 s | 1.86 s–2.67 s | 7.83× more | 275.3 MB | 7.38× more | mcp-tool | yes | 892.0 ms | — |
| OpenCode | 6.96 s | 6.67 s–9.54 s | 24.52× more | 766.4 MB | 20.53× more | mcp-tool | yes | 5.32 s | — |
| pi | 691.4 ms | 682.5 ms–789.2 ms | 2.44× more | 118.4 MB | 3.17× more | mcp-tool | yes | 529.1 ms | — |
| oh-my-pi (omp) | 3.47 s | 3.40 s–3.85 s | 12.24× more | 417.5 MB | 11.18× more | mcp-route | yes | 2.74 s | — |
| oh-my-openagent (omo) | 22.11 s | 19.45 s–30.77 s | 77.92× more | 566.5 MB | 15.18× more | mcp-tool | yes | 18.82 s | — |

**Session memory** (idle after 3 turn(s) per session; sizes are process-tree sums, RSS; PSS also recorded on Linux). "Each additional" is the least-squares slope across the measured session counts; 0 B means growth stayed below RSS sampling noise. Threads and descriptors come from the daemon's own `server:memory` after the turns.

| Tool | First session | Each additional | vs horon (additional) | Threads/session | FDs/session | First (post-turn) | Each additional (post-turn) |
|---|---|---|---|---|---|---|---|
| horon | 22.6 MB | 145.2 KB | baseline | — | — | 29.7 MB | 400.4 KB |
| jcode | 22.0 MB | 77.5 KB | 1.87× less | — | — | 27.0 MB | 315.3 KB |
| Claude Code | 154.9 MB | 172.9 MB | 1,219× more | — | — | 187.4 MB | 188.5 MB |
| Codex CLI | 126.2 MB | 117.1 MB | 825.67× more | — | — | 165.8 MB | 117.4 MB |
| OpenCode | 245.8 MB | 13.1 MB | 92.69× more | — | — | 434.8 MB | 26.1 MB |
| pi | 90.8 MB | 90.8 MB | 639.96× more | — | — | 101.3 MB | 100.8 MB |
| oh-my-pi (omp) | 205.7 MB | 150.5 MB | 1,062× more | — | — | 338.7 MB | 338.6 MB |
| oh-my-openagent (omo) | 148.0 MB | 131.0 MB | 923.49× more | — | — | 411.2 MB | 459.9 MB |

Threads/descriptors unavailable: horon (server:memory did not report thread_count, fd_count), jcode (server:memory did not report thread_count, fd_count)

**Post-turn tree RSS per session count**

| Tool | 1 session(s) | 5 session(s) | 10 session(s) |
|---|---|---|---|
| horon | 29.7 MB | 32.2 MB | 33.3 MB |
| jcode | 27.0 MB | 28.4 MB | 29.8 MB |
| Claude Code | 187.4 MB | 950.5 MB | 1.8 GB |
| Codex CLI | 165.8 MB | 653.4 MB | 1.2 GB |
| OpenCode | 434.8 MB | 461.6 MB | 663.8 MB |
| pi | 101.3 MB | 505.8 MB | 1008.9 MB |
| oh-my-pi (omp) | 338.7 MB | 1.6 GB | 3.3 GB |
| oh-my-openagent (omo) | 411.2 MB | 2.2 GB | 3.9 GB |

**Errors recorded during measurement**

- oh-my-openagent (omo) @ 10 sessions: turn 2 session 8: Connection error.
- oh-my-openagent (omo) @ 10 sessions: turn 3 session 0: Connection error.
- oh-my-openagent (omo) @ 10 sessions: turn 3 session 8: Agent is already processing. Specify streamingBehavior ('steer' or 'followUp') to queue the message.

#### 기동

```text
Benchmarking: D:/a/agent/agent/bench-bin/horon-agent.exe
============================================================

--help (binary load):
  Min:    15.84 ms
  Max:    16.32 ms
  Mean:   16.10 ms
  Median: 16.09 ms
  Stdev:  0.16 ms

--version:
  Min:    15.50 ms
  Max:    16.89 ms
  Mean:   15.97 ms
  Median: 15.84 ms
  Stdev:  0.49 ms

Measuring isolated server startup (10 runs)...

Server ready (isolated socket connectable):
  Min:    22.58 ms
  Max:    27.65 ms
  Mean:   23.32 ms
  Median: 22.66 ms
  Stdev:  1.55 ms

Measuring harness bridge readiness (10 runs)...
  skipped: api-bridge is Unix-only, so Windows has no harness bridge to time

Bridge stage: api socket connectable: No successful runs

Bridge stage: hello_ok: No successful runs

Bridge stage: no-op reply: No successful runs

Bridge total (socket -> no-op): No successful runs

============================================================
Summary:
  Binary load median:      ~16.1 ms
  Server ready median:     ~22.7 ms
  Bridge total median:     n/a
```

#### 세션 로딩 (COLD)

```text
binary   D:\a\agent\agent\bench-bin\examples\loading_probe.exe (1dec058e6ab5)
mode     cold
protocol COLD: every measured run gets its own freshly copied HOME; no derived cache from an earlier run exists, so each sample is a first load of the fixture.
fixtures C:\Users\RUNNER~1\AppData\Local\Temp\horon-loading-bench-_m835xem
note     --warmup 3 is ignored in cold mode: every sample gets a fresh HOME
fixture events=0 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    0.076/0.071    p95    0.086/0.120    delta  -0.005 ms  control-no-change
  skill_request_overlay p50    0.557/0.552    p95    0.604/0.974    delta  -0.005 ms  control-no-change
  mcp_config_load   p50    0.696/0.673    p95    0.766/1.137    delta  -0.023 ms  control-no-change
  attach_history    p50    0.002/0.002    p95    0.002/0.002    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.001/0.001    p95    0.009/0.002    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.001/0.001    p95    0.002/0.002    delta  +0.000 ms  control-no-change
  session_save      p50   39.369/39.518   p95   48.990/52.151   delta  +0.149 ms  control-no-change
  total store_read_bytes p50 22948  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 22904..22964 (8 distinct values)
  SPREAD   b session_save store_read_bytes 22878..22964 (9 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50   10.661/10.674   p95   10.872/12.360   delta  +0.012 ms  control-no-change
  skill_request_overlay p50   29.745/29.774   p95   32.076/35.205   delta  +0.028 ms  control-no-change
  mcp_config_load   p50    0.719/0.709    p95    0.868/0.793    delta  -0.010 ms  control-no-change
  attach_history    p50    0.002/0.002    p95    0.002/0.004    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.001/0.001    p95    0.002/0.001    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.002/0.002    p95    0.014/0.002    delta  +0.000 ms  control-no-change
  session_save      p50   54.963/48.670   p95   62.029/57.922   delta  -6.294 ms  control-no-change
  total store_read_bytes p50 22946  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 22894..22960 (7 distinct values)
  SPREAD   b session_save store_read_bytes 22916..22956 (7 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.070/0.070    p95    0.090/0.072    delta  +0.000 ms  control-no-change
  skill_request_overlay p50    0.553/0.545    p95    0.581/0.608    delta  -0.008 ms  control-no-change
  mcp_config_load   p50    0.666/0.698    p95    0.740/0.767    delta  +0.032 ms  control-no-change
  attach_history    p50  318.236/313.070  p95  330.835/330.552  delta  -5.167 ms  control-no-change
  resume_ready      p50  101.453/99.255   p95  106.598/103.824  delta  -2.197 ms  control-no-change
  provider_dispatch p50    1.194/1.200    p95    1.732/1.250    delta  +0.006 ms  control-no-change
  session_save      p50   35.138/35.398   p95   37.810/41.076   delta  +0.260 ms  control-no-change
  total store_read_bytes p50 5311215  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189151..4189155 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099102..1099106 (2 distinct values)
  SPREAD   a session_save store_read_bytes 22940..22974 (8 distinct values)
  SPREAD   b attach_history store_read_bytes 4189151..4189155 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099102..1099106 (4 distinct values)
  SPREAD   b session_save store_read_bytes 22923..22976 (8 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50   11.000/10.812   p95   11.743/12.801   delta  -0.188 ms  control-no-change
  skill_request_overlay p50   29.564/29.931   p95   32.685/35.739   delta  +0.367 ms  control-no-change
  mcp_config_load   p50    0.733/0.726    p95    0.860/1.432    delta  -0.007 ms  control-no-change
  attach_history    p50  307.690/311.737  p95  324.830/321.862  delta  +4.047 ms  control-no-change
  resume_ready      p50   99.850/101.815  p95  103.708/107.870  delta  +1.964 ms  control-no-change
  provider_dispatch p50    1.181/1.144    p95    1.246/1.270    delta  -0.038 ms  control-no-change
  session_save      p50   37.991/38.569   p95   61.852/56.673   delta  +0.579 ms  control-no-change
  total store_read_bytes p50 5311213  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189151..4189155 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099104..1099106 (3 distinct values)
  SPREAD   a session_save store_read_bytes 22931..22976 (9 distinct values)
  SPREAD   b attach_history store_read_bytes 4189151..4189155 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099102..1099106 (3 distinct values)
  SPREAD   b session_save store_read_bytes 22930..22968 (7 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.067/0.068    p95    0.083/0.091    delta  +0.001 ms  control-no-change
  skill_request_overlay p50    0.524/0.540    p95    0.580/0.580    delta  +0.016 ms  control-no-change
  mcp_config_load   p50    0.683/0.678    p95    0.715/0.735    delta  -0.005 ms  control-no-change
  attach_history    p50 3504.310/3345.711 p95 3816.235/3703.461 delta -158.599 ms  control-no-change
  resume_ready      p50 1055.542/1022.610 p95 1092.154/1117.242 delta -32.932 ms  control-no-change
  provider_dispatch p50   12.876/12.905   p95   15.085/15.255   delta  +0.029 ms  control-no-change
  session_save      p50   33.115/33.970   p95  127.937/291.088  delta  +0.855 ms  control-no-change
  total store_read_bytes p50 53417430  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184251..42184253 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210215..11210216 (2 distinct values)
  SPREAD   a session_save store_read_bytes 22935..22987 (9 distinct values)
  SPREAD   b attach_history store_read_bytes 42184249..42184253 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210212..11210216 (3 distinct values)
  SPREAD   b session_save store_read_bytes 22930..22979 (9 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    9.547/9.704    p95   10.698/10.576   delta  +0.157 ms  control-no-change
  skill_request_overlay p50   26.796/26.482   p95   30.348/29.370   delta  -0.314 ms  control-no-change
  mcp_config_load   p50    0.650/0.656    p95    0.732/0.708    delta  +0.006 ms  control-no-change
  attach_history    p50 3327.271/3294.592 p95 3528.120/3540.102 delta -32.679 ms  control-no-change
  resume_ready      p50 1005.018/1005.208 p95 1049.803/1043.810 delta  +0.189 ms  control-no-change
  provider_dispatch p50   12.537/12.349   p95   15.119/14.186   delta  -0.189 ms  control-no-change
  session_save      p50   32.948/32.543   p95   38.367/50.003   delta  -0.404 ms  control-no-change
  total store_read_bytes p50 53417438  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184251..42184253 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210215..11210216 (2 distinct values)
  SPREAD   a session_save store_read_bytes 22959..22979 (5 distinct values)
  SPREAD   b attach_history store_read_bytes 42184249..42184253 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210214..11210216 (3 distinct values)
  SPREAD   b session_save store_read_bytes 22935..22979 (9 distinct values)
report   bench-results\loading-cold.json
mode cold, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### 세션 로딩 (WARM)

```text
binary   D:\a\agent\agent\bench-bin\examples\loading_probe.exe (1dec058e6ab5)
mode     warm
protocol WARM: one HOME per side, --warmup discarded runs per side, then measured runs alternate between the two sides on those same HOMEs.
fixtures C:\Users\RUNNER~1\AppData\Local\Temp\horon-loading-bench-am2hln65
fixture events=0 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    0.119/0.122    p95    0.149/0.148    delta  +0.003 ms  control-no-change
  skill_request_overlay p50    0.940/0.904    p95    1.001/0.961    delta  -0.036 ms  control-no-change
  mcp_config_load   p50    1.087/1.128    p95    1.144/1.270    delta  +0.041 ms  control-drift
  attach_history    p50    0.002/0.002    p95    0.002/0.002    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.002/0.002    p95    0.002/0.002    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.002/0.002    p95    0.002/0.002    delta  +0.000 ms  control-no-change
  session_save      p50   38.041/38.131   p95   41.423/39.397   delta  +0.089 ms  control-no-change
  total store_read_bytes p50 23632  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [22899, 22930, 22951, 23630, 23634, 23636, 23647, 24319, 24345]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [22908, 22925, 22937, 23618, 23641, 23644, 24328, 24339, 24341, 24351]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 22899..24345 (9 distinct values)
  SPREAD   b session_save store_read_bytes 22908..24351 (10 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50   18.297/18.115   p95   20.303/19.471   delta  -0.181 ms  control-no-change
  skill_request_overlay p50   49.047/48.832   p95   58.425/58.300   delta  -0.215 ms  control-no-change
  mcp_config_load   p50    1.171/1.177    p95    1.802/1.233    delta  +0.006 ms  control-no-change
  attach_history    p50    0.002/0.002    p95    0.007/0.003    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.002/0.002    p95    0.002/0.002    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.002/0.002    p95    0.002/0.002    delta  +0.000 ms  control-no-change
  session_save      p50   41.541/41.497   p95   84.345/44.777   delta  -0.044 ms  control-no-change
  total store_read_bytes p50 23624  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [22941, 22952, 22953, 22955, 23620, 23628, 23642, 23653, 24335, 24352]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [22942, 22945, 22947, 22950, 23608, 23617, 23627, 23634, 24337, 24339]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 22941..24352 (10 distinct values)
  SPREAD   b session_save store_read_bytes 22942..24339 (10 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.117/0.117    p95    0.159/0.143    delta  +0.000 ms  control-no-change
  skill_request_overlay p50    0.935/0.905    p95    0.951/1.021    delta  -0.029 ms  control-no-change
  mcp_config_load   p50    1.109/1.114    p95    1.191/1.199    delta  +0.005 ms  control-no-change
  attach_history    p50  252.310/250.506  p95  268.361/257.956  delta  -1.804 ms  control-no-change
  resume_ready      p50  186.938/191.066  p95  190.110/205.489  delta  +4.128 ms  control-no-change
  provider_dispatch p50    1.684/1.714    p95    1.762/1.793    delta  +0.030 ms  control-no-change
  session_save      p50   37.361/37.420   p95   38.425/41.932   delta  +0.060 ms  control-no-change
  total store_read_bytes p50 2841110  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23623, 23655, 24346, 24352, 25028, 25061, 25747, 25764, 26438, 26467]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23632, 23667, 24343, 24362, 25035, 25050, 25761, 25770, 26412, 26448]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23623..26467 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23632..26448 (10 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50   18.224/18.435   p95   24.341/19.116   delta  +0.211 ms  control-no-change
  skill_request_overlay p50   48.243/48.465   p95   51.008/55.036   delta  +0.221 ms  control-no-change
  mcp_config_load   p50    1.147/1.193    p95    1.483/1.507    delta  +0.046 ms  control-no-change
  attach_history    p50  249.051/245.683  p95  261.179/250.331  delta  -3.368 ms  control-no-change
  resume_ready      p50  187.457/191.208  p95  190.651/199.516  delta  +3.750 ms  control-no-change
  provider_dispatch p50    1.678/1.687    p95    1.749/1.948    delta  +0.009 ms  control-no-change
  session_save      p50   39.342/38.219   p95   42.505/57.633   delta  -1.123 ms  control-no-change
  total store_read_bytes p50 2841112  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23662, 23665, 24352, 24353, 25024, 25041, 25741, 25764, 26406, 26430]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23669, 23679, 24306, 24364, 25044, 25055, 25725, 25744, 26439, 26440]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23662..26430 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23669..26440 (10 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.118/0.121    p95    0.179/0.180    delta  +0.003 ms  control-no-change
  skill_request_overlay p50    0.955/0.960    p95    1.330/1.036    delta  +0.005 ms  control-no-change
  mcp_config_load   p50    1.133/1.107    p95    1.592/1.147    delta  -0.025 ms  control-no-change
  attach_history    p50 2608.144/2598.797 p95 2851.005/2717.734 delta  -9.347 ms  control-no-change
  resume_ready      p50 1977.009/1981.329 p95 2640.800/2060.218 delta  +4.320 ms  control-no-change
  provider_dispatch p50   17.800/18.118   p95   19.561/22.160   delta  +0.318 ms  control-no-change
  session_save      p50   38.441/37.771   p95   86.490/215.086  delta  -0.669 ms  control-no-change
  total store_read_bytes p50 28640167  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23630, 23663, 24358, 24369, 25064, 25076, 25755, 25763, 26429, 26470]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23654, 23664, 24321, 24360, 25052, 25065, 25757, 25760, 26444, 26461]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23630..26470 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23654..26461 (10 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50   18.077/18.325   p95   19.748/18.682   delta  +0.249 ms  control-no-change
  skill_request_overlay p50   48.910/48.454   p95   51.095/52.324   delta  -0.456 ms  control-no-change
  mcp_config_load   p50    1.194/1.223    p95    1.242/1.509    delta  +0.029 ms  control-no-change
  attach_history    p50 2610.457/2617.941 p95 2631.786/2629.029 delta  +7.484 ms  control-no-change
  resume_ready      p50 1981.307/1978.245 p95 1998.206/1990.525 delta  -3.062 ms  control-no-change
  provider_dispatch p50   17.887/17.910   p95   18.548/18.933   delta  +0.023 ms  control-no-change
  session_save      p50   36.887/36.656   p95   45.332/40.567   delta  -0.231 ms  control-no-change
  total store_read_bytes p50 28640153  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23621, 23664, 24359, 24362, 25047, 25065, 25744, 25756, 26455, 26475]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23652, 23678, 24371, 24377, 25051, 25061, 25752, 25764, 26431, 26451]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23621..26475 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23652..26451 (10 distinct values)
report   bench-results\loading-warm.json
mode warm, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### Tool latency

| Tool case | median | p95 | min | max | samples |
|---|---|---|---|---|---|
| read (tiny file) | 2.7 ms | 2.7 ms | 2.7 ms | 2.7 ms | 5 |
| read (1000 lines) | 2.7 ms | 2.8 ms | 2.7 ms | 2.8 ms | 5 |
| read (10 lines @ offset) | 2.7 ms | 2.7 ms | 2.7 ms | 2.7 ms | 5 |
| read (repo Cargo.toml) | 2.7 ms | 2.7 ms | 2.2 ms | 2.7 ms | 5 |
| write (small) | 8.0 ms | 10.5 ms | 7.9 ms | 10.5 ms | 5 |
| write (10KB) | 10.0 ms | 12.2 ms | 10.0 ms | 12.2 ms | 5 |
| edit (simple replace) | 10.0 ms | 10.0 ms | 7.4 ms | 10.0 ms | 5 |
| ls (scripts dir) | 2.7 ms | 2.7 ms | 2.7 ms | 2.7 ms | 5 |
| agentgrep (grep in tree) | 7.6 ms | 8.0 ms | 7.4 ms | 8.0 ms | 5 |
| agentgrep (find **/*.rs) | 7.3 ms | 7.9 ms | 5.3 ms | 7.9 ms | 5 |
| bash (true) | 10.4 ms | 10.5 ms | 10.0 ms | 10.5 ms | 5 |
| bash (echo hello) | 10.5 ms | 10.6 ms | 9.9 ms | 10.6 ms | 5 |
| bash (ls fixture tree) | 13.0 ms | 14.6 ms | 13.0 ms | 14.6 ms | 5 |
| invalid tool (dispatch only) | 2.7 ms | 2.8 ms | 2.7 ms | 2.8 ms | 5 |

#### Reload storm

1 resume(s) per level; each level resumes sessions no other level has opened, from a freshly started daemon.

| Concurrent resumes | history median | p95 | max | storm wall | failures |
|---|---|---|---|---|---|
| 1 | 154.0 ms | 154.0 ms | 154.0 ms | 154.1 ms | 0 |
| 10 | 473.2 ms | 476.2 ms | 476.2 ms | 477.0 ms | 0 |
| 40 | 1.40 s | 1.81 s | 1.81 s | 1.82 s | 0 |

#### Compile loop

| Checkpoint | Touched | median | min | max | samples |
|---|---|---|---|---|---|
| warm_noop | — | 474.5 ms | 474.5 ms | 474.5 ms | 1 |
| leaf_touch | src\main.rs | 671.7 ms | 671.7 ms | 671.7 ms | 1 |
| core_touch | crates\app-core\src\lib.rs | 21.57 s | 21.57 s | 21.57 s | 1 |
| build_meta_touch | crates\build-meta\src\lib.rs | 29.24 s | 29.24 s | 29.24 s | 1 |

<!-- bench-auto:end -->
