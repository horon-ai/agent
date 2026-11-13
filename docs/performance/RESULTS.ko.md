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

대화형 클라이언트는 별도 바이너리입니다. 이 차트는 `tui` 묶음만으로 그리며 위의 헤드리스 차트와 섞지 않습니다.

| TUI 항목 | 차트 |
| --- | --- |
| TUI 지연 | [tui-01-latency.ko.svg](charts/tui-01-latency.ko.svg) |
| TUI 메모리와 크기 | [tui-02-memory.ko.svg](charts/tui-02-memory.ko.svg) |

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

<!-- bench-auto:start commit=775c49115a6d550215a0ca7cc06fcd31f0838bcc -->

## 헤드리스 (`horon-agent`)

이 구간은 `.github/workflows/benchmark-docs.yml` 이 자동으로 다시 씁니다. 손으로 고치지 않습니다.

- 커밋: `775c49115a6d550215a0ca7cc06fcd31f0838bcc`
- 측정 시각: 2026-11-13 21:15 UTC
- 실행: <https://github.com/horon-ai/agent/actions/runs/43499875915> (원본 JSON 은 OS별 `benchmark-<os>-<run_id>` 아티팩트)

OS마다 다른 GitHub 호스티드 러너에서 따로 잽니다. 기계가 다르므로 OS끼리 절대값을 비교하지 않고, 같은 OS 안의 상대 비교(horon 대비 배수)와 커밋 사이의 추세만 읽습니다.

| OS | 러너 | 바이너리 | 상태 |
| --- | --- | --- | --- |
| Linux | `ubuntu-24.04` | `horon-agent v0.1.0-dev (775c491)` | 성공 |
| macOS | `macos-latest` | `horon-agent v0.1.0-dev (775c491)` | 성공 |
| Windows | `windows-latest` | `horon-agent v0.1.0-dev (775c491)` | 성공 |

### Linux

#### Coding agent comparison (headless, mock model)

Host: linux/x86_64, 4 CPUs, Python 3.12.16. Baseline: **horon**. Samples per one-shot, skill and MCP cell: 5.

| Tool | Version | `--version` (median) | Install size |
|---|---|---|---|
| horon | horon-agent v0.1.0-dev (775c491) | 5.7 ms | 115.5 MB |
| jcode | jcode v0.99.1 (7206efd0c) | 14.4 ms | 163.4 MB |
| Claude Code | 2.1.317 (Claude Code) | 12.2 ms | 632.2 MB |
| Codex CLI | codex-cli 0.166.0 | 53.0 ms | 435.0 MB |
| OpenCode | 1.19.5 | 494.8 ms | 613.4 MB |
| pi | 1.5.0 | 261.1 ms | 108.9 MB |
| oh-my-pi (omp) | omp/18.18.0 | 736.0 ms | 413.1 MB |
| oh-my-openagent (omo) | omo 5.1.64 (engine: senpi 2026.11.5-1) | 136.9 ms | 520.6 MB |

**One-shot headless prompt** (includes startup, one `ls` tool call, final answer)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Tool round-trip | fsync calls | Note |
|---|---|---|---|---|---|---|---|---|
| horon | 67.9 ms | 65.0 ms–1.07 s | baseline | 40.6 MB | baseline | yes | 10 | — |
| jcode | 157.8 ms | 156.3 ms–162.2 ms | 2.33× more | 38.8 MB | 1.05× less | yes | 5 | — |
| Claude Code | 556.3 ms | 533.1 ms–607.2 ms | 8.20× more | 206.0 MB | 5.07× more | yes | — | — |
| Codex CLI | 592.6 ms | 550.9 ms–625.0 ms | 8.73× more | 191.3 MB | 4.71× more | yes | 6 | — |
| OpenCode | 3.12 s | 2.97 s–3.26 s | 46.03× more | 572.0 MB | 14.09× more | yes | 4 | — |
| pi | 437.3 ms | 429.8 ms–463.3 ms | 6.44× more | 115.4 MB | 2.84× more | yes | — | — |
| oh-my-pi (omp) | 2.24 s | 2.18 s–2.43 s | 33.07× more | 394.5 MB | 9.72× more | yes | 71 | — |
| oh-my-openagent (omo) | 6.34 s | 6.26 s–6.97 s | 93.42× more | 653.5 MB | 16.10× more | yes | 36 | — |

**Skill invocation** (process start to final answer, including skill discovery and one load of the fixture skill)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | Note |
|---|---|---|---|---|---|---|---|---|
| horon | 61.0 ms | 60.4 ms–62.5 ms | baseline | 32.5 MB | baseline | skill-uri | yes | — |
| jcode | 73.2 ms | 71.1 ms–87.0 ms | 1.20× more | 42.4 MB | 1.30× more | skill-tool | yes | — |
| Claude Code | 523.5 ms | 517.7 ms–543.8 ms | 8.58× more | 206.3 MB | 6.35× more | skill-tool | yes | — |
| Codex CLI | 664.5 ms | 614.1 ms–716.0 ms | 10.89× more | 196.3 MB | 6.04× more | file-read | yes | — |
| OpenCode | 3.50 s | 3.47 s–3.52 s | 57.31× more | 614.7 MB | 18.92× more | skill-tool | yes | — |
| pi | 574.9 ms | 563.0 ms–580.5 ms | 9.42× more | 125.8 MB | 3.87× more | file-read | yes | — |
| oh-my-pi (omp) | 2.14 s | 1.90 s–2.25 s | 35.08× more | 381.8 MB | 11.75× more | file-read | yes | — |
| oh-my-openagent (omo) | 7.20 s | 6.97 s–7.39 s | 118.04× more | 662.5 MB | 20.39× more | file-read | yes | — |

**MCP tool call** (process start to final answer, including starting the fixture stdio MCP server and one `bench_echo` call)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | MCP ready | Note |
|---|---|---|---|---|---|---|---|---|---|
| horon | 94.3 ms | 92.8 ms–101.1 ms | baseline | 51.3 MB | baseline | mcp-call | yes | 71.6 ms | — |
| jcode | 194.4 ms | 194.2 ms–195.3 ms | 2.06× more | 55.1 MB | 1.07× more | mcp-action | yes | 155.9 ms | — |
| Claude Code | 649.2 ms | 630.0 ms–678.4 ms | 6.88× more | 227.2 MB | 4.43× more | mcp-tool | yes | 467.6 ms | — |
| Codex CLI | 585.5 ms | 560.8 ms–604.4 ms | 6.21× more | 195.2 MB | 3.80× more | mcp-tool | yes | 384.3 ms | — |
| OpenCode | 3.23 s | 3.16 s–3.27 s | 34.23× more | 560.6 MB | 10.92× more | mcp-tool | yes | 2.90 s | — |
| pi | 619.9 ms | 612.9 ms–644.5 ms | 6.57× more | 138.6 MB | 2.70× more | mcp-tool | yes | 550.3 ms | — |
| oh-my-pi (omp) | 2.23 s | 1.89 s–2.24 s | 23.64× more | 391.7 MB | 7.63× more | mcp-route | yes | 1.82 s | — |
| oh-my-openagent (omo) | — | — | — | — | — | mcp-tool | no | — | failed: the fixture sentinel never came back |

**Session memory** (idle after 3 turn(s) per session; sizes are process-tree sums, RSS; PSS also recorded on Linux). "Each additional" is the least-squares slope across the measured session counts; 0 B means growth stayed below RSS sampling noise. Threads and descriptors come from the daemon's own `server:memory` after the turns.

| Tool | First session | Each additional | vs horon (additional) | Threads/session | FDs/session | First (post-turn) | Each additional (post-turn) |
|---|---|---|---|---|---|---|---|
| horon | 37.2 MB | 106.3 KB | baseline | 0.34 | 1.00 | 44.5 MB | 438.6 KB |
| jcode | 42.5 MB | 90.2 KB | 1.18× less | 0.00 | — | 46.1 MB | 255.7 KB |
| Claude Code | 150.1 MB | 146.2 MB | 1,408× more | — | — | 185.3 MB | 180.5 MB |
| Codex CLI | 129.4 MB | 132.2 MB | 1,273× more | — | — | 162.3 MB | 163.8 MB |
| OpenCode | 242.0 MB | 1.4 MB | 13.17× more | — | — | 451.5 MB | 5.0 MB |
| pi | 107.8 MB | 108.7 MB | 1,047× more | — | — | 132.7 MB | 125.4 MB |
| oh-my-pi (omp) | 338.8 MB | 70.1 MB | 675.26× more | — | — | 365.0 MB | 357.2 MB |
| oh-my-openagent (omo) | 330.3 MB | 30.0 MB | 289.34× more | — | — | 617.7 MB | 574.3 MB |

Threads/descriptors unavailable: jcode (server:memory did not report fd_count)

**Post-turn tree RSS per session count**

| Tool | 1 session(s) | 5 session(s) | 10 session(s) |
|---|---|---|---|
| horon | 44.5 MB | 46.9 MB | 48.4 MB |
| jcode | 46.1 MB | 47.5 MB | 48.4 MB |
| Claude Code | 185.3 MB | 910.3 MB | 1.8 GB |
| Codex CLI | 162.3 MB | 809.6 MB | 1.6 GB |
| OpenCode | 451.5 MB | 521.9 MB | 499.8 MB |
| pi | 132.7 MB | 621.6 MB | 1.2 GB |
| oh-my-pi (omp) | 365.0 MB | 1.8 GB | 3.5 GB |
| oh-my-openagent (omo) | 617.7 MB | 3.0 GB | 5.7 GB |

#### 기동

```text
Benchmarking: /home/runner/work/agent/agent/bench-bin/horon-agent
============================================================

--help (binary load):
  Min:    2.90 ms
  Max:    3.21 ms
  Mean:   3.01 ms
  Median: 2.99 ms
  Stdev:  0.08 ms

--version:
  Min:    2.81 ms
  Max:    3.44 ms
  Mean:   2.94 ms
  Median: 2.87 ms
  Stdev:  0.19 ms

Measuring isolated server startup (10 runs)...

Server ready (isolated socket connectable):
  Min:    20.40 ms
  Max:    20.58 ms
  Mean:   20.49 ms
  Median: 20.48 ms
  Stdev:  0.05 ms

Measuring harness bridge readiness (10 runs)...

Bridge stage: api socket connectable:
  Min:    20.54 ms
  Max:    20.65 ms
  Mean:   20.60 ms
  Median: 20.60 ms
  Stdev:  0.04 ms

Bridge stage: hello_ok:
  Min:    21.01 ms
  Max:    21.26 ms
  Mean:   21.15 ms
  Median: 21.16 ms
  Stdev:  0.07 ms

Bridge stage: no-op reply:
  Min:    21.29 ms
  Max:    21.56 ms
  Mean:   21.46 ms
  Median: 21.48 ms
  Stdev:  0.08 ms

Bridge total (socket -> no-op):
  Min:    21.29 ms
  Max:    21.56 ms
  Mean:   21.46 ms
  Median: 21.48 ms
  Stdev:  0.08 ms

============================================================
Summary:
  Binary load median:      ~3.0 ms
  Server ready median:     ~20.5 ms
  Bridge total median:     ~21.5 ms
```

#### 세션 로딩 (COLD)

```text
binary   /home/runner/work/agent/agent/bench-bin/examples/loading_probe (993cf2055188)
mode     cold
protocol COLD: every measured run gets its own freshly copied HOME; no derived cache from an earlier run exists, so each sample is a first load of the fixture.
fixtures /tmp/horon-loading-bench-3orrr93g
note     --warmup 3 is ignored in cold mode: every sample gets a fresh HOME
fixture events=0 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    0.006/0.006    p95    0.007/0.007    delta  +0.000 ms  control-no-change
  skill_request_overlay p50    0.038/0.038    p95    0.039/0.039    delta  -0.000 ms  control-no-change
  mcp_config_load   p50    0.052/0.053    p95    0.053/0.053    delta  +0.000 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  session_save      p50    4.991/4.765    p95    8.526/5.154    delta  -0.226 ms  control-no-change
  total store_read_bytes p50 23068  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 23056..23092 (6 distinct values)
  SPREAD   b session_save store_read_bytes 23064..23080 (5 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    1.196/1.181    p95    1.223/1.213    delta  -0.015 ms  control-no-change
  skill_request_overlay p50    1.820/1.821    p95    1.847/1.851    delta  +0.001 ms  control-no-change
  mcp_config_load   p50    0.058/0.058    p95    0.065/0.064    delta  -0.000 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.001/0.001    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  session_save      p50    6.784/7.108    p95    8.652/8.714    delta  +0.324 ms  control-no-change
  total store_read_bytes p50 23078  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 23064..23092 (7 distinct values)
  SPREAD   b session_save store_read_bytes 23060..23084 (6 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.005/0.005    p95    0.006/0.009    delta  +0.000 ms  control-no-change
  skill_request_overlay p50    0.038/0.038    p95    0.038/0.039    delta  -0.000 ms  control-no-change
  mcp_config_load   p50    0.053/0.053    p95    0.078/0.074    delta  -0.000 ms  control-no-change
  attach_history    p50  189.503/189.835  p95  192.513/194.001  delta  +0.332 ms  control-no-change
  resume_ready      p50   27.145/27.200   p95   27.288/27.371   delta  +0.055 ms  control-no-change
  provider_dispatch p50    0.931/0.928    p95    0.955/0.947    delta  -0.003 ms  control-no-change
  session_save      p50    5.008/4.988    p95    7.899/6.584    delta  -0.021 ms  control-no-change
  total store_read_bytes p50 5311413  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 23074..23090 (5 distinct values)
  SPREAD   b attach_history store_read_bytes 4189191..4189195 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099132..1099136 (3 distinct values)
  SPREAD   b session_save store_read_bytes 23064..23086 (5 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    1.172/1.179    p95    1.187/1.241    delta  +0.007 ms  control-no-change
  skill_request_overlay p50    1.837/1.829    p95    1.862/1.863    delta  -0.009 ms  control-no-change
  mcp_config_load   p50    0.058/0.058    p95    0.068/0.064    delta  -0.000 ms  control-no-change
  attach_history    p50  189.892/189.842  p95  193.014/191.941  delta  -0.051 ms  control-no-change
  resume_ready      p50   27.429/27.444   p95   27.807/27.598   delta  +0.015 ms  control-no-change
  provider_dispatch p50    0.921/0.943    p95    0.952/0.967    delta  +0.021 ms  control-no-change
  session_save      p50    4.489/4.430    p95    4.960/6.269    delta  -0.059 ms  control-no-change
  total store_read_bytes p50 5311407  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189191..4189195 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099132..1099136 (3 distinct values)
  SPREAD   a session_save store_read_bytes 23070..23090 (6 distinct values)
  SPREAD   b attach_history store_read_bytes 4189191..4189195 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099132..1099136 (2 distinct values)
  SPREAD   b session_save store_read_bytes 23058..23090 (8 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.005/0.005    p95    0.007/0.006    delta  +0.000 ms  control-no-change
  skill_request_overlay p50    0.038/0.038    p95    0.048/0.050    delta  -0.000 ms  control-no-change
  mcp_config_load   p50    0.054/0.054    p95    0.062/0.062    delta  -0.000 ms  control-no-change
  attach_history    p50 2396.794/2422.417 p95 2445.801/2513.228 delta +25.623 ms  control-no-change
  resume_ready      p50  321.755/323.261  p95  332.752/330.479  delta  +1.506 ms  control-no-change
  provider_dispatch p50   11.015/11.007   p95   11.301/11.299   delta  -0.009 ms  control-no-change
  session_save      p50    4.776/5.123    p95    8.085/7.097    delta  +0.347 ms  control-no-change
  total store_read_bytes p50 53417620  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184291..42184293 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210245..11210246 (2 distinct values)
  SPREAD   a session_save store_read_bytes 23073..23089 (6 distinct values)
  SPREAD   b attach_history store_read_bytes 42184289..42184293 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210242..11210246 (2 distinct values)
  SPREAD   b session_save store_read_bytes 23067..23085 (5 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    1.189/1.182    p95    1.228/1.213    delta  -0.007 ms  control-no-change
  skill_request_overlay p50    1.814/1.810    p95    1.836/1.827    delta  -0.004 ms  control-no-change
  mcp_config_load   p50    0.058/0.058    p95    0.062/0.066    delta  -0.000 ms  control-no-change
  attach_history    p50 2461.576/2449.519 p95 2481.889/2481.179 delta -12.058 ms  control-no-change
  resume_ready      p50  326.865/328.593  p95  331.207/332.479  delta  +1.728 ms  control-no-change
  provider_dispatch p50   11.103/11.187   p95   11.497/11.477   delta  +0.083 ms  control-no-change
  session_save      p50    4.946/5.051    p95    5.263/8.915    delta  +0.104 ms  control-no-change
  total store_read_bytes p50 53417618  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184289..42184293 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210242..11210246 (3 distinct values)
  SPREAD   a session_save store_read_bytes 23075..23087 (6 distinct values)
  SPREAD   b attach_history store_read_bytes 42184289..42184293 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210242..11210246 (3 distinct values)
  SPREAD   b session_save store_read_bytes 23073..23085 (6 distinct values)
report   bench-results/loading-cold.json
mode cold, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### 세션 로딩 (WARM)

```text
binary   /home/runner/work/agent/agent/bench-bin/examples/loading_probe (993cf2055188)
mode     warm
protocol WARM: one HOME per side, --warmup discarded runs per side, then measured runs alternate between the two sides on those same HOMEs.
fixtures /tmp/horon-loading-bench-xjnvs01_
fixture events=0 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    0.013/0.012    p95    0.028/0.013    delta  -0.001 ms  control-no-change
  skill_request_overlay p50    0.096/0.096    p95    0.115/0.096    delta  -0.000 ms  control-no-change
  mcp_config_load   p50    0.128/0.128    p95    0.159/0.129    delta  +0.000 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  session_save      p50    6.804/6.630    p95   75.105/54.692   delta  -0.174 ms  control-no-change
  total store_read_bytes p50 23798  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23065, 23077, 23093, 23796, 23800, 24511, 24515, 24523]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23069, 23073, 23077, 23788, 23804, 23812, 24511, 24515, 24519, 24527]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23065..24523 (8 distinct values)
  SPREAD   b session_save store_read_bytes 23069..24527 (10 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    2.394/2.423    p95    2.481/2.462    delta  +0.030 ms  control-no-change
  skill_request_overlay p50    3.875/3.897    p95    4.010/3.932    delta  +0.022 ms  control-no-change
  mcp_config_load   p50    0.134/0.134    p95    0.141/0.154    delta  -0.001 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  session_save      p50   17.278/11.673   p95  156.312/60.178   delta  -5.605 ms  control-no-change
  total store_read_bytes p50 23803  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23059, 23069, 23073, 23788, 23798, 23808, 24505, 24515, 24517, 24519]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23077, 23081, 23085, 23780, 23788, 23796, 24511, 24515, 24519]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23059..24519 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23077..24519 (9 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.013/0.013    p95    0.014/0.014    delta  -0.000 ms  control-no-change
  skill_request_overlay p50    0.092/0.092    p95    0.096/0.100    delta  +0.001 ms  control-no-change
  mcp_config_load   p50    0.124/0.124    p95    0.128/0.139    delta  +0.000 ms  control-no-change
  attach_history    p50   80.541/81.206   p95   82.166/81.703   delta  +0.666 ms  control-no-change
  resume_ready      p50   44.167/44.080   p95   44.514/44.357   delta  -0.087 ms  control-no-change
  provider_dispatch p50    1.055/1.058    p95    1.070/1.071    delta  +0.004 ms  control-no-change
  session_save      p50   40.968/7.165    p95  259.829/259.930  delta -33.803 ms  control-no-change
  total store_read_bytes p50 2841371  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23792, 23800, 24507, 24515, 25232, 25234, 25949, 25955, 26662, 26668]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23798, 23802, 24513, 24517, 25224, 25228, 25234, 25935, 25951, 26668]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23792..26668 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23798..26668 (10 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    2.429/2.425    p95    3.576/2.528    delta  -0.004 ms  control-no-change
  skill_request_overlay p50    3.901/3.891    p95    5.593/4.274    delta  -0.010 ms  control-no-change
  mcp_config_load   p50    0.136/0.134    p95    0.153/0.136    delta  -0.002 ms  control-no-change
  attach_history    p50   81.958/81.299   p95   94.453/103.070  delta  -0.659 ms  control-no-change
  resume_ready      p50   44.406/44.691   p95   45.090/50.858   delta  +0.285 ms  control-no-change
  provider_dispatch p50    1.052/1.046    p95    1.061/1.119    delta  -0.006 ms  control-no-change
  session_save      p50   15.118/8.769    p95   80.916/77.643   delta  -6.349 ms  control-no-change
  total store_read_bytes p50 2841370  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23785, 23809, 24502, 24516, 25235, 25241, 25940, 25958, 26669, 26675]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23783, 23801, 24516, 24522, 25235, 25239, 25950, 25956, 26673]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23785..26675 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23783..26673 (9 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.013/0.013    p95    0.014/0.021    delta  +0.000 ms  control-no-change
  skill_request_overlay p50    0.096/0.096    p95    0.106/0.115    delta  +0.000 ms  control-no-change
  mcp_config_load   p50    0.128/0.128    p95    0.143/0.142    delta  -0.000 ms  control-no-change
  attach_history    p50  947.886/948.528  p95  975.219/957.461  delta  +0.643 ms  control-no-change
  resume_ready      p50  532.063/533.207  p95  553.004/552.339  delta  +1.144 ms  control-no-change
  provider_dispatch p50   14.091/13.708   p95   15.671/15.279   delta  -0.383 ms  control-no-change
  session_save      p50    6.888/6.970    p95  218.624/26.814   delta  +0.081 ms  control-no-change
  total store_read_bytes p50 28640390  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23796, 23812, 24515, 24517, 25226, 25240, 25953, 25967, 26662, 26676]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23790, 23796, 24513, 24519, 25236, 25242, 25957, 25963, 26674, 26680]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23796..26676 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23790..26680 (10 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    2.451/2.445    p95    2.509/2.468    delta  -0.006 ms  control-no-change
  skill_request_overlay p50    3.936/3.927    p95    4.125/4.071    delta  -0.008 ms  control-no-change
  mcp_config_load   p50    0.138/0.134    p95    0.148/0.145    delta  -0.004 ms  control-no-change
  attach_history    p50  943.567/944.721  p95  956.009/995.993  delta  +1.154 ms  control-no-change
  resume_ready      p50  557.537/556.554  p95  576.338/571.906  delta  -0.983 ms  control-no-change
  provider_dispatch p50   14.026/14.474   p95   15.076/16.522   delta  +0.448 ms  control-no-change
  session_save      p50   12.031/12.084   p95  210.729/69.809   delta  +0.054 ms  control-no-change
  total store_read_bytes p50 28640400  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23794, 23804, 24507, 24509, 25238, 25248, 25953, 25967, 26662, 26672]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23780, 23800, 24515, 24527, 25240, 25242, 25949, 25959, 26660, 26678]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23794..26672 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23780..26678 (10 distinct values)
report   bench-results/loading-warm.json
mode warm, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### Tool latency

| Tool case | median | p95 | min | max | samples |
|---|---|---|---|---|---|
| read (tiny file) | 0.2 ms | 0.3 ms | 0.2 ms | 0.3 ms | 5 |
| read (1000 lines) | 0.4 ms | 0.5 ms | 0.3 ms | 0.5 ms | 5 |
| read (10 lines @ offset) | 0.3 ms | 0.4 ms | 0.3 ms | 0.4 ms | 5 |
| read (repo Cargo.toml) | 0.3 ms | 0.4 ms | 0.3 ms | 0.4 ms | 5 |
| write (small) | 1.2 ms | 1.3 ms | 1.1 ms | 1.3 ms | 5 |
| write (10KB) | 1.4 ms | 2.9 ms | 1.3 ms | 2.9 ms | 5 |
| edit (simple replace) | 1.2 ms | 125.6 ms | 1.1 ms | 125.6 ms | 5 |
| ls (scripts dir) | 0.6 ms | 0.6 ms | 0.6 ms | 0.6 ms | 5 |
| agentgrep (grep in tree) | 1.4 ms | 1.4 ms | 1.4 ms | 1.4 ms | 5 |
| agentgrep (find **/*.rs) | 1.2 ms | 1.2 ms | 1.2 ms | 1.2 ms | 5 |
| bash (true) | 1.7 ms | 1.7 ms | 1.6 ms | 1.7 ms | 5 |
| bash (echo hello) | 1.7 ms | 1.7 ms | 1.6 ms | 1.7 ms | 5 |
| bash (ls fixture tree) | 2.9 ms | 3.0 ms | 2.8 ms | 3.0 ms | 5 |
| invalid tool (dispatch only) | 0.1 ms | 0.1 ms | 0.1 ms | 0.1 ms | 5 |

#### Reload storm

1 resume(s) per level; each level resumes sessions no other level has opened, from a freshly started daemon.

| Concurrent resumes | history median | p95 | max | storm wall | failures |
|---|---|---|---|---|---|
| 1 | 61.1 ms | 61.1 ms | 61.1 ms | 61.3 ms | 0 |
| 10 | 198.9 ms | 226.9 ms | 226.9 ms | 227.1 ms | 0 |
| 40 | 883.7 ms | 890.7 ms | 892.2 ms | 892.6 ms | 0 |

#### Compile loop

| Checkpoint | Touched | median | min | max | samples |
|---|---|---|---|---|---|
| warm_noop | — | 277.1 ms | 277.1 ms | 277.1 ms | 1 |
| leaf_touch | src/main.rs | 421.4 ms | 421.4 ms | 421.4 ms | 1 |
| core_touch | crates/app-core/src/lib.rs | 20.25 s | 20.25 s | 20.25 s | 1 |
| build_meta_touch | crates/build-meta/src/lib.rs | 28.72 s | 28.72 s | 28.72 s | 1 |

### macOS

#### Coding agent comparison (headless, mock model)

Host: darwin/arm64, 3 CPUs, Python 3.12.10. Baseline: **horon**. Samples per one-shot, skill and MCP cell: 5.

Skipped: syscalls (strace not available)

| Tool | Version | `--version` (median) | Install size |
|---|---|---|---|
| horon | horon-agent v0.1.0-dev (775c491) | 8.0 ms | 97.1 MB |
| jcode | jcode v0.99.1 (7206efd0c) | 10.6 ms | 136.3 MB |
| Claude Code | 2.1.317 (Claude Code) | 23.0 ms | 596.8 MB |
| Codex CLI | codex-cli 0.166.0 | 59.6 ms | 346.5 MB |
| OpenCode | 1.19.5 | 315.1 ms | 344.6 MB |
| pi | 1.5.0 | 247.1 ms | 108.9 MB |
| oh-my-pi (omp) | omp/18.18.0 | 519.2 ms | 273.5 MB |
| oh-my-openagent (omo) | omo 5.1.64 (engine: senpi 2026.11.5-1) | 220.7 ms | 502.3 MB |

**One-shot headless prompt** (includes startup, one `ls` tool call, final answer)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Tool round-trip | Note |
|---|---|---|---|---|---|---|---|
| horon | 70.3 ms | 63.1 ms–361.5 ms | baseline | 32.3 MB | baseline | yes | — |
| jcode | 396.0 ms | 329.7 ms–564.3 ms | 5.64× more | 34.1 MB | 1.06× more | yes | — |
| Claude Code | 422.2 ms | 387.3 ms–730.2 ms | 6.01× more | 199.1 MB | 6.17× more | yes | — |
| Codex CLI | 454.7 ms | 384.2 ms–1.19 s | 6.47× more | 136.9 MB | 4.24× more | yes | — |
| OpenCode | 2.95 s | 2.65 s–5.33 s | 41.98× more | 686.6 MB | 21.27× more | yes | — |
| pi | 413.1 ms | 348.4 ms–443.6 ms | 5.88× more | 122.8 MB | 3.80× more | yes | — |
| oh-my-pi (omp) | 1.66 s | 1.24 s–1.83 s | 23.57× more | 416.7 MB | 12.91× more | yes | — |
| oh-my-openagent (omo) | 9.23 s | 7.91 s–9.97 s | 131.32× more | 880.3 MB | 27.27× more | yes | — |

**Skill invocation** (process start to final answer, including skill discovery and one load of the fixture skill)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | Note |
|---|---|---|---|---|---|---|---|---|
| horon | 64.4 ms | 60.7 ms–110.7 ms | baseline | 23.5 MB | baseline | skill-uri | yes | — |
| jcode | 227.1 ms | 213.4 ms–371.1 ms | 3.52× more | 25.3 MB | 1.08× more | skill-tool | yes | — |
| Claude Code | 512.5 ms | 406.4 ms–536.5 ms | 7.95× more | 199.3 MB | 8.50× more | skill-tool | yes | — |
| Codex CLI | 478.6 ms | 383.1 ms–608.4 ms | 7.43× more | 144.4 MB | 6.16× more | file-read | yes | — |
| OpenCode | 3.57 s | 3.46 s–4.55 s | 55.45× more | 727.6 MB | 31.02× more | skill-tool | yes | — |
| pi | 452.4 ms | 441.6 ms–490.5 ms | 7.02× more | 135.1 MB | 5.76× more | file-read | yes | — |
| oh-my-pi (omp) | 2.19 s | 2.10 s–2.99 s | 34.00× more | 451.4 MB | 19.25× more | file-read | yes | — |
| oh-my-openagent (omo) | 9.22 s | 8.12 s–10.89 s | 143.09× more | 887.2 MB | 37.83× more | file-read | yes | — |

**MCP tool call** (process start to final answer, including starting the fixture stdio MCP server and one `bench_echo` call)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | MCP ready | Note |
|---|---|---|---|---|---|---|---|---|---|
| horon | 89.9 ms | 87.2 ms–114.0 ms | baseline | 19.5 MB | baseline | mcp-call | yes | 72.5 ms | — |
| jcode | 352.8 ms | 334.6 ms–395.6 ms | 3.93× more | 52.5 MB | 2.70× more | mcp-action | yes | 330.9 ms | — |
| Claude Code | 498.2 ms | 450.7 ms–634.8 ms | 5.54× more | 216.0 MB | 11.10× more | mcp-tool | yes | 375.3 ms | — |
| Codex CLI | 359.4 ms | 307.7 ms–399.1 ms | 4.00× more | 142.6 MB | 7.32× more | mcp-tool | yes | 223.3 ms | — |
| OpenCode | 3.28 s | 3.13 s–4.49 s | 36.50× more | 745.8 MB | 38.31× more | mcp-tool | yes | 2.87 s | — |
| pi | 516.2 ms | 499.0 ms–616.6 ms | 5.75× more | 132.7 MB | 6.81× more | mcp-tool | yes | 440.0 ms | — |
| oh-my-pi (omp) | 2.12 s | 1.77 s–2.68 s | 23.55× more | 408.2 MB | 20.97× more | mcp-route | yes | 1.88 s | — |
| oh-my-openagent (omo) | — | — | — | — | — | mcp-tool | no | — | failed: the fixture sentinel never came back |

**Session memory** (idle after 3 turn(s) per session; sizes are process-tree sums, RSS; PSS also recorded on Linux). "Each additional" is the least-squares slope across the measured session counts; 0 B means growth stayed below RSS sampling noise. Threads and descriptors come from the daemon's own `server:memory` after the turns.

| Tool | First session | Each additional | vs horon (additional) | Threads/session | FDs/session | First (post-turn) | Each additional (post-turn) |
|---|---|---|---|---|---|---|---|
| horon | 28.9 MB | 169.7 KB | baseline | 0.67 | 1.00 | 35.1 MB | 380.3 KB |
| jcode | 36.5 MB | 209.7 KB | 1.24× more | — | — | 40.1 MB | 357.8 KB |
| Claude Code | 159.1 MB | 152.8 MB | 921.81× more | — | — | 190.5 MB | 184.1 MB |
| Codex CLI | 114.6 MB | 81.8 MB | 493.32× more | — | — | 107.2 MB | 107.3 MB |
| OpenCode | 260.8 MB | 637.6 KB | 3.76× more | — | — | 559.3 MB | 6.9 MB |
| pi | 119.6 MB | 116.1 MB | 700.57× more | — | — | 148.7 MB | 148.0 MB |
| oh-my-pi (omp) | 414.0 MB | 117.4 MB | 708.62× more | — | — | 440.9 MB | 298.5 MB |
| oh-my-openagent (omo) | 57.8 MB | 46.8 MB | 282.45× more | — | — | 814.8 MB | 316.2 MB |

Threads/descriptors unavailable: jcode (server:memory did not report thread_count, fd_count)

**Post-turn tree RSS per session count**

| Tool | 1 session(s) | 5 session(s) | 10 session(s) |
|---|---|---|---|
| horon | 35.1 MB | 36.8 MB | 38.5 MB |
| jcode | 40.1 MB | 41.4 MB | 43.2 MB |
| Claude Code | 190.5 MB | 932.2 MB | 1.8 GB |
| Codex CLI | 107.2 MB | 536.0 MB | 1.0 GB |
| OpenCode | 559.3 MB | 625.2 MB | 624.0 MB |
| pi | 148.7 MB | 740.8 MB | 1.4 GB |
| oh-my-pi (omp) | 440.9 MB | 1.7 GB | 3.1 GB |
| oh-my-openagent (omo) | 814.8 MB | 2.9 GB | 3.6 GB |

#### 기동

```text
Benchmarking: /Users/runner/work/agent/agent/bench-bin/horon-agent
============================================================

--help (binary load):
  Min:    8.53 ms
  Max:    12.71 ms
  Mean:   9.82 ms
  Median: 9.75 ms
  Stdev:  1.26 ms

--version:
  Min:    8.10 ms
  Max:    16.79 ms
  Mean:   10.71 ms
  Median: 9.60 ms
  Stdev:  2.95 ms

Measuring isolated server startup (10 runs)...

Server ready (isolated socket connectable):
  Min:    24.66 ms
  Max:    97.66 ms
  Mean:   58.17 ms
  Median: 51.03 ms
  Stdev:  27.68 ms

Measuring harness bridge readiness (10 runs)...

Bridge stage: api socket connectable:
  Min:    24.75 ms
  Max:    109.36 ms
  Mean:   74.17 ms
  Median: 74.56 ms
  Stdev:  26.67 ms

Bridge stage: hello_ok:
  Min:    25.25 ms
  Max:    110.05 ms
  Mean:   74.97 ms
  Median: 75.60 ms
  Stdev:  26.81 ms

Bridge stage: no-op reply:
  Min:    25.65 ms
  Max:    110.49 ms
  Mean:   75.45 ms
  Median: 76.26 ms
  Stdev:  26.84 ms

Bridge total (socket -> no-op):
  Min:    25.65 ms
  Max:    110.49 ms
  Mean:   75.45 ms
  Median: 76.26 ms
  Stdev:  26.84 ms

============================================================
Summary:
  Binary load median:      ~9.8 ms
  Server ready median:     ~51.0 ms
  Bridge total median:     ~76.3 ms
```

#### 세션 로딩 (COLD)

```text
binary   /Users/runner/work/agent/agent/bench-bin/examples/loading_probe (3ba716d3e11a)
mode     cold
protocol COLD: every measured run gets its own freshly copied HOME; no derived cache from an earlier run exists, so each sample is a first load of the fixture.
fixtures /var/folders/h5/t85nd63d71x27_8gl5gw3l_w0000gn/T/horon-loading-bench-k70otyvc
note     --warmup 3 is ignored in cold mode: every sample gets a fresh HOME
fixture events=0 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    0.027/0.027    p95    0.030/0.072    delta  -0.000 ms  control-no-change
  skill_request_overlay p50    0.246/0.262    p95    0.267/0.755    delta  +0.016 ms  control-no-change
  mcp_config_load   p50    0.313/0.299    p95    0.612/0.739    delta  -0.014 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.001/0.001    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.002/0.002    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.004/0.001    delta  +0.000 ms  control-no-change
  session_save      p50   21.571/21.000   p95   26.985/25.692   delta  -0.572 ms  control-no-change
  total store_read_bytes p50 22942  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 22918..22950 (6 distinct values)
  SPREAD   b session_save store_read_bytes 22938..22954 (5 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    4.673/4.744    p95    6.893/5.516    delta  +0.071 ms  control-no-change
  skill_request_overlay p50    5.949/6.184    p95    6.861/7.066    delta  +0.235 ms  control-drift
  mcp_config_load   p50    0.317/0.313    p95    0.343/0.337    delta  -0.005 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.001    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.001    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  session_save      p50   17.352/17.332   p95   51.129/30.547   delta  -0.019 ms  control-no-change
  total store_read_bytes p50 22946  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 22934..22954 (5 distinct values)
  SPREAD   b session_save store_read_bytes 22934..22954 (6 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.027/0.027    p95    0.031/0.031    delta  -0.000 ms  control-no-change
  skill_request_overlay p50    0.266/0.264    p95    0.294/0.313    delta  -0.002 ms  control-no-change
  mcp_config_load   p50    0.309/0.310    p95    0.333/0.378    delta  +0.001 ms  control-no-change
  attach_history    p50  311.452/312.194  p95  334.663/335.352  delta  +0.742 ms  control-no-change
  resume_ready      p50   68.241/68.360   p95   73.772/74.059   delta  +0.118 ms  control-no-change
  provider_dispatch p50    1.128/1.175    p95    1.221/1.241    delta  +0.047 ms  control-no-change
  session_save      p50   15.624/14.789   p95   18.279/18.790   delta  -0.835 ms  control-no-change
  total store_read_bytes p50 5311310  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189203..4189207 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099141..1099145 (4 distinct values)
  SPREAD   a session_save store_read_bytes 22948..22972 (8 distinct values)
  SPREAD   b session_save store_read_bytes 22948..22972 (8 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    4.584/4.569    p95    4.737/4.763    delta  -0.015 ms  control-no-change
  skill_request_overlay p50    5.900/5.845    p95    6.641/6.338    delta  -0.055 ms  control-no-change
  mcp_config_load   p50    0.302/0.302    p95    0.320/0.336    delta  -0.001 ms  control-no-change
  attach_history    p50  302.555/304.194  p95  341.049/314.153  delta  +1.639 ms  control-no-change
  resume_ready      p50   68.086/67.608   p95   73.179/70.053   delta  -0.478 ms  control-no-change
  provider_dispatch p50    1.130/1.173    p95    1.195/1.325    delta  +0.044 ms  control-no-change
  session_save      p50   17.076/15.269   p95   25.093/17.343   delta  -1.808 ms  control-no-change
  total store_read_bytes p50 5311308  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189205..4189207 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099143..1099145 (3 distinct values)
  SPREAD   a session_save store_read_bytes 22947..22972 (8 distinct values)
  SPREAD   b session_save store_read_bytes 22944..22968 (7 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.029/0.031    p95    0.035/0.057    delta  +0.003 ms  control-no-change
  skill_request_overlay p50    0.262/0.270    p95    0.331/0.519    delta  +0.008 ms  control-no-change
  mcp_config_load   p50    0.293/0.308    p95    0.316/0.551    delta  +0.014 ms  control-no-change
  attach_history    p50 3565.067/3739.653 p95 4221.376/4177.904 delta +174.586 ms  control-no-change
  resume_ready      p50  741.205/749.260  p95  901.132/885.502  delta  +8.056 ms  control-no-change
  provider_dispatch p50   11.733/11.543   p95   14.050/13.951   delta  -0.190 ms  control-no-change
  session_save      p50   20.573/22.530   p95   34.353/25.461   delta  +1.957 ms  control-no-change
  total store_read_bytes p50 53417515  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184301..42184305 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210251..11210255 (3 distinct values)
  SPREAD   a session_save store_read_bytes 22943..22975 (7 distinct values)
  SPREAD   b attach_history store_read_bytes 42184295..42184305 (4 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210246..11210255 (4 distinct values)
  SPREAD   b session_save store_read_bytes 22953..22971 (8 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    4.107/3.901    p95    4.965/4.533    delta  -0.206 ms  control-no-change
  skill_request_overlay p50    5.417/4.796    p95    6.902/5.613    delta  -0.621 ms  control-no-change
  mcp_config_load   p50    0.265/0.257    p95    0.352/0.290    delta  -0.008 ms  control-no-change
  attach_history    p50 3193.887/3426.218 p95 3494.482/4200.907 delta +232.330 ms  control-no-change
  resume_ready      p50  649.569/639.183  p95  826.203/958.248  delta -10.386 ms  control-no-change
  provider_dispatch p50   10.761/9.656    p95   12.178/12.824   delta  -1.105 ms  control-no-change
  session_save      p50   23.277/22.558   p95   29.674/32.267   delta  -0.719 ms  control-no-change
  total store_read_bytes p50 53417523  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184299..42184305 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210250..11210255 (2 distinct values)
  SPREAD   a session_save store_read_bytes 22943..22971 (6 distinct values)
  SPREAD   b attach_history store_read_bytes 42184297..42184305 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210247..11210255 (3 distinct values)
  SPREAD   b session_save store_read_bytes 22946..22975 (6 distinct values)
report   bench-results/loading-cold.json
mode cold, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### 세션 로딩 (WARM)

```text
binary   /Users/runner/work/agent/agent/bench-bin/examples/loading_probe (3ba716d3e11a)
mode     warm
protocol WARM: one HOME per side, --warmup discarded runs per side, then measured runs alternate between the two sides on those same HOMEs.
fixtures /var/folders/h5/t85nd63d71x27_8gl5gw3l_w0000gn/T/horon-loading-bench-201p20ll
fixture events=0 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    0.028/0.032    p95    0.164/0.043    delta  +0.004 ms  control-no-change
  skill_request_overlay p50    0.256/0.239    p95    0.810/0.346    delta  -0.017 ms  control-no-change
  mcp_config_load   p50    0.267/0.282    p95    0.650/0.406    delta  +0.014 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.001/0.000    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.001/0.001    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.001/0.000    delta  -0.000 ms  control-no-change
  session_save      p50   18.996/18.079   p95   38.722/42.651   delta  -0.917 ms  control-no-change
  total store_read_bytes p50 23658  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [22937, 22943, 22955, 23649, 23655, 23661, 24351, 24371, 24377, 24379]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [22939, 22947, 22951, 23657, 23665, 23677, 24361, 24363, 24371, 24379]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 22937..24379 (10 distinct values)
  SPREAD   b session_save store_read_bytes 22939..24379 (10 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    4.561/6.888    p95    7.757/10.330   delta  +2.327 ms  control-drift
  skill_request_overlay p50    5.808/6.458    p95   11.321/11.023   delta  +0.650 ms  control-no-change
  mcp_config_load   p50    0.299/0.310    p95    1.044/0.689    delta  +0.011 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.001/0.001    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  session_save      p50   19.396/21.517   p95   28.661/28.560   delta  +2.121 ms  control-no-change
  total store_read_bytes p50 23655  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [22929, 22933, 22943, 22947, 23651, 23659, 23661, 24369, 24373, 24379]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [22927, 22947, 22959, 23661, 23665, 24367, 24371, 24375, 24383]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 22929..24379 (10 distinct values)
  SPREAD   b session_save store_read_bytes 22927..24383 (9 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.034/0.032    p95    0.084/0.083    delta  -0.001 ms  control-no-change
  skill_request_overlay p50    0.250/0.253    p95    0.793/0.739    delta  +0.003 ms  control-no-change
  mcp_config_load   p50    0.283/0.284    p95    0.848/0.757    delta  +0.001 ms  control-no-change
  attach_history    p50  139.151/124.672  p95  217.299/194.877  delta -14.479 ms  control-no-change
  resume_ready      p50   91.701/84.881   p95  131.543/169.222  delta  -6.820 ms  control-no-change
  provider_dispatch p50    1.253/1.345    p95    2.958/2.806    delta  +0.092 ms  control-no-change
  session_save      p50   17.037/27.797   p95   68.149/132.215  delta +10.760 ms  control-no-change
  total store_read_bytes p50 2841256  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23662, 23674, 24380, 24388, 25094, 25102, 25800, 25816, 26518, 26526]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23670, 24388, 24396, 25090, 25102, 25798, 25820, 26526, 26536]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23662..26526 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23670..26536 (9 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    4.266/4.231    p95    5.902/5.165    delta  -0.035 ms  control-no-change
  skill_request_overlay p50    5.746/5.716    p95    6.860/6.583    delta  -0.030 ms  control-no-change
  mcp_config_load   p50    0.280/0.280    p95    0.348/0.545    delta  -0.000 ms  control-no-change
  attach_history    p50  110.636/114.690  p95  143.933/133.494  delta  +4.054 ms  control-no-change
  resume_ready      p50   68.075/68.498   p95   89.890/84.146   delta  +0.423 ms  control-no-change
  provider_dispatch p50    1.079/1.096    p95    1.171/1.392    delta  +0.017 ms  control-no-change
  session_save      p50   15.511/17.489   p95   23.437/31.529   delta  +1.978 ms  control-no-change
  total store_read_bytes p50 2841247  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23660, 23670, 24372, 24386, 25096, 25098, 25818, 25828, 26524, 26530]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23662, 23670, 24384, 24392, 25082, 25102, 25804, 25816, 26522, 26534]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23660..26530 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23662..26534 (10 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.031/0.033    p95    0.128/0.143    delta  +0.002 ms  control-no-change
  skill_request_overlay p50    0.249/0.274    p95    0.817/1.055    delta  +0.026 ms  control-no-change
  mcp_config_load   p50    0.285/0.271    p95    0.850/1.015    delta  -0.014 ms  control-no-change
  attach_history    p50 1454.489/1462.522 p95 1875.237/1692.245 delta  +8.033 ms  control-no-change
  resume_ready      p50  871.235/792.116  p95 1200.534/1032.153 delta -79.119 ms  control-drift
  provider_dispatch p50   13.552/13.885   p95   18.400/19.808   delta  +0.332 ms  control-no-change
  session_save      p50   20.730/27.672   p95   29.221/86.070   delta  +6.943 ms  control-no-change
  total store_read_bytes p50 28640265  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23669, 23677, 24381, 24391, 25093, 25103, 25815, 25821, 26525, 26531]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23667, 23671, 24373, 24381, 25091, 25107, 25813, 25817, 26501, 26525]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23669..26531 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23667..26525 (10 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    4.488/4.554    p95    5.272/18.648   delta  +0.066 ms  control-no-change
  skill_request_overlay p50    5.957/5.490    p95    6.824/10.511   delta  -0.466 ms  control-no-change
  mcp_config_load   p50    0.290/0.272    p95    0.492/0.319    delta  -0.018 ms  control-no-change
  attach_history    p50 1140.377/1219.686 p95 1423.549/1362.330 delta +79.310 ms  control-no-change
  resume_ready      p50  783.237/735.207  p95  977.782/846.317  delta -48.031 ms  control-no-change
  provider_dispatch p50   11.067/11.674   p95   15.119/16.167   delta  +0.607 ms  control-no-change
  session_save      p50   23.225/21.192   p95   54.051/33.484   delta  -2.033 ms  control-no-change
  total store_read_bytes p50 28640267  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23669, 23677, 24383, 24387, 25099, 25101, 25801, 25819, 26523, 26529]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23673, 23677, 24387, 24391, 25091, 25105, 25809, 25823, 26525, 26527]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23669..26529 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23673..26527 (10 distinct values)
report   bench-results/loading-warm.json
mode warm, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### Tool latency

| Tool case | median | p95 | min | max | samples |
|---|---|---|---|---|---|
| read (tiny file) | 0.3 ms | 0.8 ms | 0.3 ms | 0.8 ms | 5 |
| read (1000 lines) | 0.8 ms | 1.4 ms | 0.7 ms | 1.4 ms | 5 |
| read (10 lines @ offset) | 0.9 ms | 1.4 ms | 0.6 ms | 1.4 ms | 5 |
| read (repo Cargo.toml) | 0.6 ms | 0.9 ms | 0.3 ms | 0.9 ms | 5 |
| write (small) | 3.4 ms | 7.2 ms | 2.6 ms | 7.2 ms | 5 |
| write (10KB) | 2.9 ms | 3.4 ms | 2.6 ms | 3.4 ms | 5 |
| edit (simple replace) | 2.6 ms | 5.3 ms | 2.4 ms | 5.3 ms | 5 |
| ls (scripts dir) | 1.1 ms | 1.8 ms | 0.9 ms | 1.8 ms | 5 |
| agentgrep (grep in tree) | 6.7 ms | 8.0 ms | 3.9 ms | 8.0 ms | 5 |
| agentgrep (find **/*.rs) | 1.8 ms | 2.3 ms | 1.6 ms | 2.3 ms | 5 |
| bash (true) | 6.4 ms | 9.0 ms | 5.5 ms | 9.0 ms | 5 |
| bash (echo hello) | 7.1 ms | 9.8 ms | 5.1 ms | 9.8 ms | 5 |
| bash (ls fixture tree) | 11.5 ms | 19.9 ms | 8.9 ms | 19.9 ms | 5 |
| invalid tool (dispatch only) | 0.1 ms | 0.2 ms | 0.1 ms | 0.2 ms | 5 |

#### Reload storm

1 resume(s) per level; each level resumes sessions no other level has opened, from a freshly started daemon.

| Concurrent resumes | history median | p95 | max | storm wall | failures |
|---|---|---|---|---|---|
| 1 | 104.2 ms | 104.2 ms | 104.2 ms | 111.8 ms | 0 |
| 10 | 299.4 ms | 301.3 ms | 301.3 ms | 304.4 ms | 0 |
| 40 | 1.08 s | 1.19 s | 1.19 s | 1.19 s | 0 |

#### Compile loop

| Checkpoint | Touched | median | min | max | samples |
|---|---|---|---|---|---|
| warm_noop | — | 372.3 ms | 372.3 ms | 372.3 ms | 1 |
| leaf_touch | src/main.rs | 524.5 ms | 524.5 ms | 524.5 ms | 1 |
| core_touch | crates/app-core/src/lib.rs | 20.72 s | 20.72 s | 20.72 s | 1 |
| build_meta_touch | crates/build-meta/src/lib.rs | 24.70 s | 24.70 s | 24.70 s | 1 |

### Windows

#### Coding agent comparison (headless, mock model)

Host: windows/AMD64, 4 CPUs, Python 3.12.10. Baseline: **horon**. Samples per one-shot, skill and MCP cell: 5.

Skipped: syscalls (strace not available)

| Tool | Version | `--version` (median) | Install size |
|---|---|---|---|
| horon | horon-agent v0.1.0-dev (775c491) | 14.9 ms | 96.8 MB |
| jcode | jcode v0.99.1 (7206efd0c) | 16.6 ms | 128.4 MB |
| Claude Code | 2.1.317 (Claude Code) | 44.6 ms | 645.2 MB |
| Codex CLI | codex-cli 0.166.0 | 109.6 ms | 479.4 MB |
| OpenCode | 1.19.5 | 589.9 ms | 605.3 MB |
| pi | 1.5.0 | 318.1 ms | 108.9 MB |
| oh-my-pi (omp) | omp/18.18.0 | 796.4 ms | 291.7 MB |
| oh-my-openagent (omo) | omo 5.1.64 (engine: senpi 2026.11.5-1) | 247.3 ms | 514.8 MB |

**One-shot headless prompt** (includes startup, one `ls` tool call, final answer)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Tool round-trip | Note |
|---|---|---|---|---|---|---|---|
| horon | 217.3 ms | 207.9 ms–289.6 ms | baseline | 29.0 MB | baseline | yes | — |
| jcode | 166.9 ms | 153.5 ms–229.3 ms | 1.30× less | 30.9 MB | 1.07× more | yes | — |
| Claude Code | 1.63 s | 1.61 s–2.38 s | 7.52× more | 276.2 MB | 9.54× more | yes | — |
| Codex CLI | 1.92 s | 1.57 s–2.43 s | 8.84× more | 256.2 MB | 8.84× more | yes | — |
| OpenCode | 5.75 s | 5.54 s–8.12 s | 26.45× more | 886.9 MB | 30.62× more | yes | — |
| pi | 560.0 ms | 553.6 ms–592.5 ms | 2.58× more | 120.3 MB | 4.15× more | yes | — |
| oh-my-pi (omp) | 3.16 s | 2.80 s–3.38 s | 14.52× more | 418.1 MB | 14.43× more | yes | — |
| oh-my-openagent (omo) | 20.98 s | 19.02 s–25.91 s | 96.53× more | 548.0 MB | 18.92× more | yes | — |

**Skill invocation** (process start to final answer, including skill discovery and one load of the fixture skill)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | Note |
|---|---|---|---|---|---|---|---|---|
| horon | 222.9 ms | 218.5 ms–235.9 ms | baseline | 25.2 MB | baseline | skill-uri | yes | — |
| jcode | 222.6 ms | 218.7 ms–240.6 ms | same | 24.0 MB | 1.05× less | skill-tool | yes | — |
| Claude Code | 1.03 s | 1.02 s–1.04 s | 4.63× more | 236.9 MB | 9.40× more | skill-tool | yes | — |
| Codex CLI | 2.40 s | 2.22 s–2.81 s | 10.78× more | 253.6 MB | 10.06× more | file-read | yes | — |
| OpenCode | 7.60 s | 7.38 s–8.00 s | 34.11× more | 756.4 MB | 30.02× more | skill-tool | yes | — |
| pi | 717.0 ms | 712.4 ms–742.1 ms | 3.22× more | 97.8 MB | 3.88× more | file-read | yes | — |
| oh-my-pi (omp) | 3.98 s | 3.79 s–4.53 s | 17.86× more | 408.2 MB | 16.20× more | file-read | yes | — |
| oh-my-openagent (omo) | 22.51 s | 19.66 s–23.18 s | 100.98× more | 542.0 MB | 21.51× more | file-read | yes | — |

**MCP tool call** (process start to final answer, including starting the fixture stdio MCP server and one `bench_echo` call)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | MCP ready | Note |
|---|---|---|---|---|---|---|---|---|---|
| horon | 267.0 ms | 252.9 ms–270.1 ms | baseline | 37.5 MB | baseline | mcp-call | yes | 186.1 ms | — |
| jcode | 339.5 ms | 327.1 ms–346.4 ms | 1.27× more | 36.5 MB | 1.03× less | mcp-action | yes | 242.6 ms | — |
| Claude Code | 1.15 s | 1.08 s–1.38 s | 4.29× more | 277.2 MB | 7.38× more | mcp-tool | yes | 964.5 ms | — |
| Codex CLI | 872.6 ms | 826.4 ms–900.4 ms | 3.27× more | 180.0 MB | 4.79× more | mcp-tool | yes | 671.6 ms | — |
| OpenCode | 7.38 s | 7.28 s–8.08 s | 27.63× more | 761.2 MB | 20.28× more | mcp-tool | yes | 5.81 s | — |
| pi | 802.8 ms | 788.2 ms–814.5 ms | 3.01× more | 117.8 MB | 3.14× more | mcp-tool | yes | 658.0 ms | — |
| oh-my-pi (omp) | 7.41 s | 3.99 s–14.59 s | 27.74× more | 422.6 MB | 11.26× more | mcp-route | yes | 7.22 s | — |
| oh-my-openagent (omo) | 23.33 s | 21.34 s–27.35 s | 87.37× more | 563.1 MB | 15.00× more | mcp-tool | yes | 20.44 s | — |

**Session memory** (idle after 3 turn(s) per session; sizes are process-tree sums, RSS; PSS also recorded on Linux). "Each additional" is the least-squares slope across the measured session counts; 0 B means growth stayed below RSS sampling noise. Threads and descriptors come from the daemon's own `server:memory` after the turns.

| Tool | First session | Each additional | vs horon (additional) | Threads/session | FDs/session | First (post-turn) | Each additional (post-turn) |
|---|---|---|---|---|---|---|---|
| horon | 22.8 MB | 110.8 KB | baseline | — | — | 30.4 MB | 310.5 KB |
| jcode | 21.8 MB | 87.6 KB | 1.27× less | — | — | 26.6 MB | 350.2 KB |
| Claude Code | 154.5 MB | 154.8 MB | 1,431× more | — | — | 188.6 MB | 191.2 MB |
| Codex CLI | 123.9 MB | 99.2 MB | 916.84× more | — | — | 165.4 MB | 118.5 MB |
| OpenCode | 234.7 MB | 1.5 MB | 13.40× more | — | — | 430.0 MB | 5.8 MB |
| pi | 90.6 MB | 90.9 MB | 839.86× more | — | — | 101.1 MB | 100.8 MB |
| oh-my-pi (omp) | 306.0 MB | 86.7 MB | 800.79× more | — | — | 338.4 MB | 336.3 MB |
| oh-my-openagent (omo) | 149.2 MB | 128.6 MB | 1,188× more | — | — | 411.8 MB | 399.3 MB |

Threads/descriptors unavailable: horon (server:memory did not report thread_count, fd_count), jcode (server:memory did not report thread_count, fd_count)

**Post-turn tree RSS per session count**

| Tool | 1 session(s) | 5 session(s) | 10 session(s) |
|---|---|---|---|
| horon | 30.4 MB | 32.1 MB | 33.2 MB |
| jcode | 26.6 MB | 28.4 MB | 29.7 MB |
| Claude Code | 188.6 MB | 946.9 MB | 1.9 GB |
| Codex CLI | 165.4 MB | 657.1 MB | 1.2 GB |
| OpenCode | 430.0 MB | 579.3 MB | 491.2 MB |
| pi | 101.1 MB | 505.2 MB | 1008.2 MB |
| oh-my-pi (omp) | 338.4 MB | 1.7 GB | 3.3 GB |
| oh-my-openagent (omo) | 411.8 MB | 2.2 GB | 3.9 GB |

#### 기동

```text
Benchmarking: D:/a/agent/agent/bench-bin/horon-agent.exe
============================================================

--help (binary load):
  Min:    10.99 ms
  Max:    12.07 ms
  Mean:   11.42 ms
  Median: 11.37 ms
  Stdev:  0.28 ms

--version:
  Min:    10.78 ms
  Max:    11.11 ms
  Mean:   10.92 ms
  Median: 10.89 ms
  Stdev:  0.12 ms

Measuring isolated server startup (10 runs)...

Server ready (isolated socket connectable):
  Min:    21.77 ms
  Max:    25.80 ms
  Mean:   22.37 ms
  Median: 22.02 ms
  Stdev:  1.22 ms

Measuring harness bridge readiness (10 runs)...
  skipped: api-bridge is Unix-only, so Windows has no harness bridge to time

Bridge stage: api socket connectable: No successful runs

Bridge stage: hello_ok: No successful runs

Bridge stage: no-op reply: No successful runs

Bridge total (socket -> no-op): No successful runs

============================================================
Summary:
  Binary load median:      ~11.4 ms
  Server ready median:     ~22.0 ms
  Bridge total median:     n/a
```

#### 세션 로딩 (COLD)

```text
binary   D:\a\agent\agent\bench-bin\examples\loading_probe.exe (6a64eefa67ac)
mode     cold
protocol COLD: every measured run gets its own freshly copied HOME; no derived cache from an earlier run exists, so each sample is a first load of the fixture.
fixtures C:\Users\RUNNER~1\AppData\Local\Temp\horon-loading-bench-pr584u0_
note     --warmup 3 is ignored in cold mode: every sample gets a fresh HOME
fixture events=0 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    0.112/0.108    p95    0.212/0.170    delta  -0.004 ms  control-no-change
  skill_request_overlay p50    0.847/0.808    p95    1.182/1.146    delta  -0.039 ms  control-no-change
  mcp_config_load   p50    1.053/1.072    p95    1.436/1.656    delta  +0.019 ms  control-no-change
  attach_history    p50    0.002/0.002    p95    0.004/0.003    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.002/0.002    p95    0.003/0.011    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.002/0.002    p95    0.004/0.003    delta  +0.000 ms  control-no-change
  session_save      p50   49.255/50.980   p95   72.922/62.343   delta  +1.725 ms  control-no-change
  total store_read_bytes p50 22946  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 22898..22964 (7 distinct values)
  SPREAD   b session_save store_read_bytes 22922..22972 (7 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50   15.348/14.007   p95   16.393/15.943   delta  -1.341 ms  control-no-change
  skill_request_overlay p50   41.936/40.499   p95   50.467/43.847   delta  -1.437 ms  control-no-change
  mcp_config_load   p50    1.098/1.065    p95    1.211/1.148    delta  -0.033 ms  control-no-change
  attach_history    p50    0.002/0.002    p95    0.005/0.010    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.002/0.002    p95    0.002/0.002    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.002/0.002    p95    0.002/0.004    delta  -0.000 ms  control-drift
  session_save      p50   66.022/68.884   p95   79.306/146.574  delta  +2.862 ms  control-no-change
  total store_read_bytes p50 22937  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 22918..22956 (7 distinct values)
  SPREAD   b session_save store_read_bytes 22902..22964 (8 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.103/0.091    p95    0.164/0.137    delta  -0.012 ms  control-drift
  skill_request_overlay p50    0.843/0.783    p95    1.198/0.818    delta  -0.060 ms  control-no-change
  mcp_config_load   p50    1.001/0.920    p95    1.132/1.027    delta  -0.081 ms  control-no-change
  attach_history    p50  494.920/474.775  p95  553.687/493.852  delta -20.144 ms  control-no-change
  resume_ready      p50  154.821/157.747  p95  185.127/162.424  delta  +2.925 ms  control-no-change
  provider_dispatch p50    1.718/1.707    p95    8.036/1.879    delta  -0.011 ms  control-no-change
  session_save      p50   39.523/40.505   p95   83.336/55.062   delta  +0.982 ms  control-no-change
  total store_read_bytes p50 5311217  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 22915..22972 (7 distinct values)
  SPREAD   b attach_history store_read_bytes 4189148..4189155 (4 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099099..1099106 (4 distinct values)
  SPREAD   b session_save store_read_bytes 22931..22976 (9 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50   14.793/15.581   p95   17.845/16.568   delta  +0.788 ms  control-no-change
  skill_request_overlay p50   38.882/39.577   p95   41.150/43.124   delta  +0.694 ms  control-no-change
  mcp_config_load   p50    1.065/1.020    p95    1.182/1.147    delta  -0.045 ms  control-no-change
  attach_history    p50  497.455/501.250  p95  549.196/522.555  delta  +3.795 ms  control-no-change
  resume_ready      p50  154.566/156.667  p95  166.797/175.819  delta  +2.101 ms  control-no-change
  provider_dispatch p50    1.657/1.677    p95    1.827/1.838    delta  +0.020 ms  control-no-change
  session_save      p50   40.008/39.332   p95   43.230/42.075   delta  -0.676 ms  control-no-change
  total store_read_bytes p50 5311221  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189151..4189155 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099102..1099106 (2 distinct values)
  SPREAD   a session_save store_read_bytes 22920..22972 (7 distinct values)
  SPREAD   b attach_history store_read_bytes 4189151..4189155 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099104..1099106 (2 distinct values)
  SPREAD   b session_save store_read_bytes 22900..22984 (10 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.095/0.108    p95    0.139/0.137    delta  +0.013 ms  control-no-change
  skill_request_overlay p50    0.765/0.845    p95    1.372/0.940    delta  +0.080 ms  control-no-change
  mcp_config_load   p50    0.959/1.008    p95    1.001/1.140    delta  +0.049 ms  control-no-change
  attach_history    p50 5586.857/5608.310 p95 6162.441/6112.798 delta +21.453 ms  control-no-change
  resume_ready      p50 1622.532/1627.550 p95 1882.785/1838.396 delta  +5.018 ms  control-no-change
  provider_dispatch p50   18.248/17.975   p95   21.326/21.508   delta  -0.273 ms  control-no-change
  session_save      p50   40.466/38.957   p95  148.818/174.288  delta  -1.509 ms  control-no-change
  total store_read_bytes p50 53417428  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184251..42184253 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210215..11210216 (2 distinct values)
  SPREAD   a session_save store_read_bytes 22923..22971 (8 distinct values)
  SPREAD   b session_save store_read_bytes 22943..22979 (7 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50   14.221/14.617   p95   15.329/15.912   delta  +0.396 ms  control-no-change
  skill_request_overlay p50   38.918/40.007   p95   42.296/43.082   delta  +1.089 ms  control-no-change
  mcp_config_load   p50    1.031/1.081    p95    1.253/1.181    delta  +0.050 ms  control-no-change
  attach_history    p50 5626.078/5587.476 p95 5694.994/5613.679 delta -38.602 ms  control-no-change
  resume_ready      p50 1630.133/1622.310 p95 1644.343/1646.351 delta  -7.822 ms  control-no-change
  provider_dispatch p50   17.679/17.988   p95   19.334/19.549   delta  +0.309 ms  control-no-change
  session_save      p50   39.553/39.508   p95   41.461/44.642   delta  -0.045 ms  control-no-change
  total store_read_bytes p50 53417432  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184251..42184253 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210215..11210216 (2 distinct values)
  SPREAD   a session_save store_read_bytes 22941..22983 (8 distinct values)
  SPREAD   b attach_history store_read_bytes 42184247..42184253 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210210..11210216 (3 distinct values)
  SPREAD   b session_save store_read_bytes 22943..22979 (8 distinct values)
report   bench-results\loading-cold.json
mode cold, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### 세션 로딩 (WARM)

```text
binary   D:\a\agent\agent\bench-bin\examples\loading_probe.exe (6a64eefa67ac)
mode     warm
protocol WARM: one HOME per side, --warmup discarded runs per side, then measured runs alternate between the two sides on those same HOMEs.
fixtures C:\Users\RUNNER~1\AppData\Local\Temp\horon-loading-bench-svgifs0a
fixture events=0 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    0.158/0.155    p95    0.201/0.207    delta  -0.003 ms  control-no-change
  skill_request_overlay p50    1.283/1.257    p95    1.436/1.631    delta  -0.027 ms  control-no-change
  mcp_config_load   p50    1.435/1.466    p95    1.533/1.826    delta  +0.032 ms  control-no-change
  attach_history    p50    0.002/0.002    p95    0.002/0.002    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.002/0.002    p95    0.002/0.002    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.002/0.002    p95    0.002/0.002    delta  +0.000 ms  control-no-change
  session_save      p50   47.287/46.866   p95   57.871/48.835   delta  -0.421 ms  control-no-change
  total store_read_bytes p50 23624  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [22949, 22950, 22953, 23619, 23630, 23652, 23664, 24331, 24338]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [22940, 22943, 22953, 22955, 23622, 23636, 23643, 24300, 24310, 24325]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 22949..24338 (9 distinct values)
  SPREAD   b session_save store_read_bytes 22940..24325 (10 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50   24.980/24.868   p95   27.924/30.236   delta  -0.112 ms  control-no-change
  skill_request_overlay p50   57.601/56.887   p95   67.368/60.386   delta  -0.715 ms  control-no-change
  mcp_config_load   p50    1.473/1.513    p95    2.067/1.871    delta  +0.040 ms  control-no-change
  attach_history    p50    0.002/0.002    p95    0.009/0.007    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.002/0.002    p95    0.002/0.002    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.002/0.002    p95    0.002/0.002    delta  +0.000 ms  control-no-change
  session_save      p50   51.688/53.722   p95   71.757/104.976  delta  +2.034 ms  control-no-change
  total store_read_bytes p50 23633  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [22935, 22939, 22945, 22953, 23632, 23634, 23652, 24329, 24371]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [22925, 22940, 22943, 22945, 23622, 23624, 23631, 24278, 24343, 24346]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 22935..24371 (9 distinct values)
  SPREAD   b session_save store_read_bytes 22925..24346 (10 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.158/0.157    p95    0.197/0.190    delta  -0.001 ms  control-no-change
  skill_request_overlay p50    1.245/1.224    p95    1.368/1.323    delta  -0.021 ms  control-no-change
  mcp_config_load   p50    1.474/1.475    p95    1.685/1.778    delta  +0.001 ms  control-no-change
  attach_history    p50  301.705/305.690  p95  313.647/310.950  delta  +3.985 ms  control-no-change
  resume_ready      p50  242.049/241.778  p95  247.893/244.460  delta  -0.271 ms  control-no-change
  provider_dispatch p50    1.895/1.938    p95    2.469/2.194    delta  +0.043 ms  control-no-change
  session_save      p50   45.953/44.224   p95   52.155/53.867   delta  -1.730 ms  control-no-change
  total store_read_bytes p50 2841122  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23639, 24330, 24362, 25027, 25057, 25758, 25768, 26423, 26457]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23655, 23659, 24352, 24362, 25065, 25067, 25744, 25748, 26467, 26471]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23639..26457 (9 distinct values)
  SPREAD   b session_save store_read_bytes 23655..26471 (10 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50   24.676/24.609   p95   27.269/28.193   delta  -0.067 ms  control-no-change
  skill_request_overlay p50   54.236/53.597   p95   63.178/65.213   delta  -0.639 ms  control-no-change
  mcp_config_load   p50    1.432/1.515    p95    1.568/1.757    delta  +0.083 ms  control-drift
  attach_history    p50  300.612/297.883  p95  320.915/307.083  delta  -2.729 ms  control-no-change
  resume_ready      p50  239.691/234.497  p95  254.647/246.621  delta  -5.194 ms  control-no-change
  provider_dispatch p50    1.853/1.870    p95    2.032/2.161    delta  +0.017 ms  control-no-change
  session_save      p50   48.487/44.737   p95   53.851/58.487   delta  -3.750 ms  control-no-change
  total store_read_bytes p50 2841132  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23651, 23659, 24336, 24350, 25051, 25053, 25727, 25760, 26423, 26451]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23631, 23663, 24350, 24374, 25013, 25029, 25738, 25760, 26411, 26459]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23651..26451 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23631..26459 (10 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.160/0.156    p95    0.198/0.209    delta  -0.004 ms  control-no-change
  skill_request_overlay p50    1.263/1.266    p95    1.590/1.572    delta  +0.003 ms  control-no-change
  mcp_config_load   p50    1.476/1.416    p95    2.098/1.685    delta  -0.060 ms  control-no-change
  attach_history    p50 3159.223/3171.970 p95 3405.498/3396.427 delta +12.746 ms  control-no-change
  resume_ready      p50 2493.825/2489.944 p95 3275.382/2573.485 delta  -3.881 ms  control-no-change
  provider_dispatch p50   21.227/20.438   p95   24.582/24.148   delta  -0.789 ms  control-no-change
  session_save      p50   45.788/51.264   p95  151.232/139.333  delta  +5.476 ms  control-no-change
  total store_read_bytes p50 28640152  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23655, 23682, 24333, 24342, 25061, 25064, 25741, 25763, 26433, 26434]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23670, 23675, 24341, 24346, 25051, 25067, 25710, 25746, 26424, 26461]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23655..26434 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23670..26461 (10 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50   24.142/24.583   p95   25.682/27.174   delta  +0.441 ms  control-no-change
  skill_request_overlay p50   55.961/53.983   p95   60.782/58.007   delta  -1.978 ms  control-no-change
  mcp_config_load   p50    1.514/1.480    p95    1.918/1.744    delta  -0.034 ms  control-no-change
  attach_history    p50 3101.744/3142.632 p95 3212.901/3171.944 delta +40.888 ms  control-no-change
  resume_ready      p50 2450.901/2491.203 p95 2514.016/2606.656 delta +40.302 ms  control-no-change
  provider_dispatch p50   21.223/20.789   p95   28.120/21.986   delta  -0.434 ms  control-no-change
  session_save      p50   46.556/46.187   p95   73.127/53.966   delta  -0.369 ms  control-no-change
  total store_read_bytes p50 28640118  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23602, 23650, 24339, 24351, 25025, 25034, 25707, 25765, 26433, 26440]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23654, 23678, 24352, 24353, 25037, 25053, 25724, 25766, 26445, 26449]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23602..26440 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23654..26449 (10 distinct values)
report   bench-results\loading-warm.json
mode warm, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### Tool latency

| Tool case | median | p95 | min | max | samples |
|---|---|---|---|---|---|
| read (tiny file) | 2.8 ms | 3.6 ms | 2.3 ms | 3.6 ms | 5 |
| read (1000 lines) | 2.7 ms | 2.8 ms | 2.3 ms | 2.8 ms | 5 |
| read (10 lines @ offset) | 2.7 ms | 2.9 ms | 2.2 ms | 2.9 ms | 5 |
| read (repo Cargo.toml) | 2.7 ms | 2.7 ms | 2.2 ms | 2.7 ms | 5 |
| write (small) | 8.0 ms | 10.7 ms | 7.4 ms | 10.7 ms | 5 |
| write (10KB) | 8.9 ms | 16.7 ms | 7.3 ms | 16.7 ms | 5 |
| edit (simple replace) | 7.9 ms | 10.4 ms | 7.4 ms | 10.4 ms | 5 |
| ls (scripts dir) | 2.7 ms | 2.8 ms | 2.2 ms | 2.8 ms | 5 |
| agentgrep (grep in tree) | 7.5 ms | 7.5 ms | 7.4 ms | 7.5 ms | 5 |
| agentgrep (find **/*.rs) | 5.3 ms | 5.3 ms | 4.8 ms | 5.3 ms | 5 |
| bash (true) | 13.1 ms | 15.6 ms | 10.0 ms | 15.6 ms | 5 |
| bash (echo hello) | 10.9 ms | 12.9 ms | 10.5 ms | 12.9 ms | 5 |
| bash (ls fixture tree) | 16.6 ms | 20.0 ms | 15.7 ms | 20.0 ms | 5 |
| invalid tool (dispatch only) | 2.7 ms | 2.8 ms | 2.6 ms | 2.8 ms | 5 |

#### Reload storm

1 resume(s) per level; each level resumes sessions no other level has opened, from a freshly started daemon.

| Concurrent resumes | history median | p95 | max | storm wall | failures |
|---|---|---|---|---|---|
| 1 | 194.9 ms | 194.9 ms | 194.9 ms | 195.1 ms | 0 |
| 10 | 676.4 ms | 683.6 ms | 683.6 ms | 683.8 ms | 0 |
| 40 | 1.78 s | 2.16 s | 2.20 s | 2.20 s | 0 |

#### Compile loop

| Checkpoint | Touched | median | min | max | samples |
|---|---|---|---|---|---|
| warm_noop | — | 574.1 ms | 574.1 ms | 574.1 ms | 1 |
| leaf_touch | src\main.rs | 779.6 ms | 779.6 ms | 779.6 ms | 1 |
| core_touch | crates\app-core\src\lib.rs | 23.84 s | 23.84 s | 23.84 s | 1 |
| build_meta_touch | crates\build-meta\src\lib.rs | 33.95 s | 33.95 s | 33.95 s | 1 |

## TUI (`horon-tui`)

대화형 클라이언트는 별도 바이너리이고 따로 잽니다. 이 수치는 `tui` 묶음에서 나오며 위의 헤드리스 표와 비교하지 않습니다. `first_frame_ms` 와 `direct_first_frame_ms` 는 서로 다른 두 프로그램을 각각 띄운 값이고, 핸드오프는 그 둘의 차이입니다.

![TUI (`horon-tui`)](charts/tui-01-latency.ko.svg)

![TUI (`horon-tui`)](charts/tui-02-memory.ko.svg)

### Linux

#### TUI (`horon-tui`)

Measured version: the interactive client `horon-tui`. The headless `horon-agent` numbers are in the suites above and are never compared with these: the two binaries are timed against themselves.

| Metric | Median | Min–max | Samples |
| --- | --- | --- | --- |
| First frame (bare `horon-agent`) | 42.2 ms | 40.0 ms–53.7 ms | 5 |
| First frame (direct `horon-tui`) | 40.3 ms | 39.5 ms–47.4 ms | 5 |
| TTY handoff cost | 1.0 ms | -4.6 ms–12.7 ms | 5 |
| Key press to echo (p95) | 0.2 ms | 0.1 ms–1.3 ms | 50 |
| Idle RSS after 5 s | 5.5 MB | 5.3 MB–5.6 MB | 5 |
| RSS after one turn | 5.6 MB | 5.5 MB–5.7 MB | 5 |
| Frame draw time (p50) | 1.5 ms | 1.5 ms–1.5 ms | 5 |
| Frame draw time (p95) | 1.6 ms | 1.5 ms–2.4 ms | 5 |
| Install size (`horon-tui`) | 5.9 MB | — | 1 |

Handoff: median first-frame time of a bare `horon-agent` launch (no arguments, on a PTY) minus the median first-frame time of a direct `horon-tui` launch against an already-running daemon, both on the same machine and build. The bare path also starts the daemon, so the number covers that startup, the sibling lookup and the `exec`; it is the extra cost of the handoff over launching the client itself.

Render bench: 200 frames on a 120×40 terminal.

### macOS

#### TUI (`horon-tui`)

Measured version: the interactive client `horon-tui`. The headless `horon-agent` numbers are in the suites above and are never compared with these: the two binaries are timed against themselves.

| Metric | Median | Min–max | Samples |
| --- | --- | --- | --- |
| First frame (bare `horon-agent`) | 129.8 ms | 63.2 ms–413.4 ms | 5 |
| First frame (direct `horon-tui`) | 84.0 ms | 71.2 ms–108.4 ms | 5 |
| TTY handoff cost | 45.8 ms | -39.3 ms–340.1 ms | 5 |
| Key press to echo (p95) | 0.2 ms | 0.1 ms–0.5 ms | 50 |
| Idle RSS after 5 s | 8.9 MB | 8.8 MB–8.9 MB | 5 |
| RSS after one turn | 9.1 MB | 9.0 MB–9.1 MB | 5 |
| Frame draw time (p50) | 1.3 ms | 1.1 ms–1.8 ms | 5 |
| Frame draw time (p95) | 3.2 ms | 2.3 ms–3.5 ms | 5 |
| Install size (`horon-tui`) | 5.0 MB | — | 1 |

Handoff: median first-frame time of a bare `horon-agent` launch (no arguments, on a PTY) minus the median first-frame time of a direct `horon-tui` launch against an already-running daemon, both on the same machine and build. The bare path also starts the daemon, so the number covers that startup, the sibling lookup and the `exec`; it is the extra cost of the handoff over launching the client itself.

Render bench: 200 frames on a 120×40 terminal.

### Windows

#### TUI (`horon-tui`)

Measured version: the interactive client `horon-tui`. The headless `horon-agent` numbers are in the suites above and are never compared with these: the two binaries are timed against themselves.

| Metric | Median | Min–max | Samples |
| --- | --- | --- | --- |
| First frame (bare `horon-agent`) | — | — | — |
| First frame (direct `horon-tui`) | — | — | — |
| TTY handoff cost | — | — | — |
| Key press to echo (p95) | — | — | — |
| Idle RSS after 5 s | — | — | — |
| RSS after one turn | — | — | — |
| Frame draw time (p50) | 2.5 ms | 2.4 ms–2.6 ms | 5 |
| Frame draw time (p95) | 2.6 ms | 2.5 ms–2.7 ms | 5 |
| Install size (`horon-tui`) | 4.9 MB | — | 1 |

Handoff: median first-frame time of a bare `horon-agent` launch (no arguments, on a PTY) minus the median first-frame time of a direct `horon-tui` launch against an already-running daemon, both on the same machine and build. The bare path also starts the daemon, so the number covers that startup, the sibling lookup and the `exec`; it is the extra cost of the handoff over launching the client itself.

Render bench: 200 frames on a 120×40 terminal.

Skipped: pty unavailable on Windows

<!-- bench-auto:end -->
