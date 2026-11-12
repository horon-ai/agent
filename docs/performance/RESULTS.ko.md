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

<!-- bench-auto:start commit=805b408e00f6d228089979559a00ca6f502803a3 -->

## 헤드리스 (`horon-agent`)

이 구간은 `.github/workflows/benchmark-docs.yml` 이 자동으로 다시 씁니다. 손으로 고치지 않습니다.

- 커밋: `805b408e00f6d228089979559a00ca6f502803a3`
- 측정 시각: 2026-11-12 18:57 UTC
- 실행: <https://github.com/horon-ai/agent/actions/runs/43273766126> (원본 JSON 은 OS별 `benchmark-<os>-<run_id>` 아티팩트)

OS마다 다른 GitHub 호스티드 러너에서 따로 잽니다. 기계가 다르므로 OS끼리 절대값을 비교하지 않고, 같은 OS 안의 상대 비교(horon 대비 배수)와 커밋 사이의 추세만 읽습니다.

| OS | 러너 | 바이너리 | 상태 |
| --- | --- | --- | --- |
| Linux | `ubuntu-24.04` | `horon-agent v0.1.0-dev (805b408)` | 성공 |
| macOS | `macos-latest` | `horon-agent v0.1.0-dev (805b408)` | 성공 |
| Windows | `windows-latest` | `horon-agent v0.1.0-dev (805b408)` | 성공 |

### Linux

#### Coding agent comparison (headless, mock model)

Host: linux/x86_64, 4 CPUs, Python 3.12.16. Baseline: **horon**. Samples per one-shot, skill and MCP cell: 5.

| Tool | Version | `--version` (median) | Install size |
|---|---|---|---|
| horon | horon-agent v0.1.0-dev (805b408) | 3.2 ms | 114.5 MB |
| jcode | jcode v0.99.1 (7206efd0c) | 9.2 ms | 163.4 MB |
| Claude Code | 2.1.317 (Claude Code) | 9.0 ms | 632.2 MB |
| Codex CLI | codex-cli 0.166.0 | 38.9 ms | 435.0 MB |
| OpenCode | 1.19.5 | 363.1 ms | 613.4 MB |
| pi | 1.5.0 | 192.7 ms | 108.9 MB |
| oh-my-pi (omp) | omp/18.18.0 | 561.8 ms | 413.1 MB |
| oh-my-openagent (omo) | omo 5.1.64 (engine: senpi 2026.11.5-1) | 96.9 ms | 520.6 MB |

**One-shot headless prompt** (includes startup, one `ls` tool call, final answer)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Tool round-trip | fsync calls | Note |
|---|---|---|---|---|---|---|---|---|
| horon | 55.9 ms | 46.2 ms–372.4 ms | baseline | 47.5 MB | baseline | yes | 10 | — |
| jcode | 140.8 ms | 139.9 ms–151.0 ms | 2.52× more | 42.2 MB | 1.13× less | yes | 5 | — |
| Claude Code | 371.1 ms | 354.4 ms–484.3 ms | 6.64× more | 204.2 MB | 4.30× more | yes | — | — |
| Codex CLI | 480.5 ms | 439.8 ms–538.9 ms | 8.59× more | 192.4 MB | 4.05× more | yes | 6 | — |
| OpenCode | 2.13 s | 2.11 s–2.32 s | 38.05× more | 577.1 MB | 12.15× more | yes | 4 | — |
| pi | 320.8 ms | 315.9 ms–336.9 ms | 5.74× more | 114.3 MB | 2.41× more | yes | — | — |
| oh-my-pi (omp) | 2.97 s | 2.25 s–3.77 s | 53.14× more | 428.0 MB | 9.01× more | yes | 71 | — |
| oh-my-openagent (omo) | 4.97 s | 4.76 s–5.33 s | 88.93× more | 664.7 MB | 13.99× more | yes | 36 | — |

**Skill invocation** (process start to final answer, including skill discovery and one load of the fixture skill)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | Note |
|---|---|---|---|---|---|---|---|---|
| horon | 53.3 ms | 46.5 ms–78.0 ms | baseline | 37.9 MB | baseline | skill-uri | yes | — |
| jcode | 121.5 ms | 49.8 ms–265.5 ms | 2.28× more | 41.1 MB | 1.08× more | skill-tool | yes | — |
| Claude Code | 374.1 ms | 360.1 ms–387.5 ms | 7.02× more | 206.0 MB | 5.44× more | skill-tool | yes | — |
| Codex CLI | 583.2 ms | 529.7 ms–701.9 ms | 10.94× more | 192.1 MB | 5.07× more | file-read | yes | — |
| OpenCode | 2.58 s | 2.56 s–2.83 s | 48.32× more | 618.6 MB | 16.32× more | skill-tool | yes | — |
| pi | 416.3 ms | 413.6 ms–435.0 ms | 7.81× more | 125.9 MB | 3.32× more | file-read | yes | — |
| oh-my-pi (omp) | 1.42 s | 1.37 s–1.78 s | 26.58× more | 373.2 MB | 9.85× more | file-read | yes | — |
| oh-my-openagent (omo) | 5.39 s | 5.17 s–5.61 s | 101.02× more | 666.3 MB | 17.58× more | file-read | yes | — |

**MCP tool call** (process start to final answer, including starting the fixture stdio MCP server and one `bench_echo` call)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | MCP ready | Note |
|---|---|---|---|---|---|---|---|---|---|
| horon | 66.4 ms | 61.8 ms–86.8 ms | baseline | 50.6 MB | baseline | mcp-call | yes | 51.0 ms | — |
| jcode | 313.5 ms | 166.8 ms–530.1 ms | 4.72× more | 56.4 MB | 1.11× more | mcp-action | yes | 283.6 ms | — |
| Claude Code | 469.2 ms | 461.6 ms–489.6 ms | 7.07× more | 226.5 MB | 4.47× more | mcp-tool | yes | 340.1 ms | — |
| Codex CLI | 540.4 ms | 472.8 ms–698.6 ms | 8.14× more | 195.3 MB | 3.86× more | mcp-tool | yes | 384.7 ms | — |
| OpenCode | 2.45 s | 2.37 s–2.54 s | 36.94× more | 578.6 MB | 11.43× more | mcp-tool | yes | 2.22 s | — |
| pi | 460.3 ms | 439.7 ms–492.9 ms | 6.94× more | 139.3 MB | 2.75× more | mcp-tool | yes | 400.7 ms | — |
| oh-my-pi (omp) | 2.03 s | 1.64 s–3.11 s | 30.60× more | 415.0 MB | 8.19× more | mcp-route | yes | 1.37 s | — |
| oh-my-openagent (omo) | — | — | — | — | — | mcp-tool | no | — | failed: the fixture sentinel never came back |

**Session memory** (idle after 3 turn(s) per session; sizes are process-tree sums, RSS; PSS also recorded on Linux). "Each additional" is the least-squares slope across the measured session counts; 0 B means growth stayed below RSS sampling noise. Threads and descriptors come from the daemon's own `server:memory` after the turns.

| Tool | First session | Each additional | vs horon (additional) | Threads/session | FDs/session | First (post-turn) | Each additional (post-turn) |
|---|---|---|---|---|---|---|---|
| horon | 37.0 MB | 137.8 KB | baseline | 0.45 | 1.00 | 44.7 MB | 375.8 KB |
| jcode | 42.7 MB | 129.8 KB | 1.06× less | 0.22 | — | 46.3 MB | 243.8 KB |
| Claude Code | 152.2 MB | 150.2 MB | 1,116× more | — | — | 187.2 MB | 180.9 MB |
| Codex CLI | 128.8 MB | 132.9 MB | 987.65× more | — | — | 162.4 MB | 164.6 MB |
| OpenCode | 242.4 MB | 0.0 B | — | — | — | 449.7 MB | 11.0 MB |
| pi | 107.2 MB | 108.8 MB | 808.13× more | — | — | 129.9 MB | 129.3 MB |
| oh-my-pi (omp) | 313.1 MB | 119.3 MB | 886.83× more | — | — | 365.4 MB | 356.5 MB |
| oh-my-openagent (omo) | 279.0 MB | 34.6 MB | 256.94× more | — | — | 584.4 MB | 589.0 MB |

Threads/descriptors unavailable: jcode (server:memory did not report fd_count)

**Post-turn tree RSS per session count**

| Tool | 1 session(s) | 5 session(s) | 10 session(s) |
|---|---|---|---|
| horon | 44.7 MB | 45.7 MB | 47.9 MB |
| jcode | 46.3 MB | 47.9 MB | 48.5 MB |
| Claude Code | 187.2 MB | 925.0 MB | 1.8 GB |
| Codex CLI | 162.4 MB | 812.7 MB | 1.6 GB |
| OpenCode | 449.7 MB | 662.7 MB | 561.0 MB |
| pi | 129.9 MB | 622.5 MB | 1.3 GB |
| oh-my-pi (omp) | 365.4 MB | 1.8 GB | 3.5 GB |
| oh-my-openagent (omo) | 584.4 MB | 2.9 GB | 5.7 GB |

#### 기동

```text
Benchmarking: /home/runner/work/agent/agent/bench-bin/horon-agent
============================================================

--help (binary load):
  Min:    4.09 ms
  Max:    4.83 ms
  Mean:   4.40 ms
  Median: 4.40 ms
  Stdev:  0.22 ms

--version:
  Min:    3.92 ms
  Max:    4.37 ms
  Mean:   4.17 ms
  Median: 4.14 ms
  Stdev:  0.15 ms

Measuring isolated server startup (10 runs)...

Server ready (isolated socket connectable):
  Min:    20.51 ms
  Max:    20.64 ms
  Mean:   20.59 ms
  Median: 20.58 ms
  Stdev:  0.04 ms

Measuring harness bridge readiness (10 runs)...

Bridge stage: api socket connectable:
  Min:    20.66 ms
  Max:    20.92 ms
  Mean:   20.73 ms
  Median: 20.71 ms
  Stdev:  0.08 ms

Bridge stage: hello_ok:
  Min:    21.35 ms
  Max:    21.70 ms
  Mean:   21.48 ms
  Median: 21.47 ms
  Stdev:  0.13 ms

Bridge stage: no-op reply:
  Min:    21.76 ms
  Max:    22.32 ms
  Mean:   21.98 ms
  Median: 21.93 ms
  Stdev:  0.19 ms

Bridge total (socket -> no-op):
  Min:    21.76 ms
  Max:    22.32 ms
  Mean:   21.98 ms
  Median: 21.93 ms
  Stdev:  0.19 ms

============================================================
Summary:
  Binary load median:      ~4.4 ms
  Server ready median:     ~20.6 ms
  Bridge total median:     ~21.9 ms
```

#### 세션 로딩 (COLD)

```text
binary   /home/runner/work/agent/agent/bench-bin/examples/loading_probe (e05ee8546af7)
mode     cold
protocol COLD: every measured run gets its own freshly copied HOME; no derived cache from an earlier run exists, so each sample is a first load of the fixture.
fixtures /tmp/horon-loading-bench-bdn00wed
note     --warmup 3 is ignored in cold mode: every sample gets a fresh HOME
fixture events=0 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    0.013/0.013    p95    0.013/0.014    delta  +0.000 ms  control-no-change
  skill_request_overlay p50    0.097/0.096    p95    0.098/0.117    delta  -0.001 ms  control-no-change
  mcp_config_load   p50    0.128/0.129    p95    0.140/0.140    delta  +0.000 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-drift
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  session_save      p50    7.168/6.995    p95  177.538/108.619  delta  -0.173 ms  control-no-change
  total store_read_bytes p50 23072  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 23052..23084 (7 distinct values)
  SPREAD   b session_save store_read_bytes 23068..23092 (5 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    2.280/2.276    p95    2.317/2.310    delta  -0.004 ms  control-no-change
  skill_request_overlay p50    3.902/3.896    p95    4.007/3.936    delta  -0.005 ms  control-no-change
  mcp_config_load   p50    0.132/0.132    p95    0.133/0.139    delta  -0.000 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  session_save      p50  109.228/91.242   p95  180.086/182.863  delta -17.986 ms  control-no-change
  total store_read_bytes p50 23076  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 23060..23088 (5 distinct values)
  SPREAD   b session_save store_read_bytes 23060..23088 (7 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.012/0.012    p95    0.013/0.014    delta  -0.000 ms  control-no-change
  skill_request_overlay p50    0.094/0.092    p95    0.122/0.117    delta  -0.002 ms  control-no-change
  mcp_config_load   p50    0.125/0.127    p95    0.129/0.144    delta  +0.002 ms  control-no-change
  attach_history    p50  244.533/246.994  p95  256.637/276.005  delta  +2.462 ms  control-no-change
  resume_ready      p50   43.942/44.245   p95   44.863/45.254   delta  +0.303 ms  control-no-change
  provider_dispatch p50    1.084/1.087    p95    1.131/1.107    delta  +0.003 ms  control-no-change
  session_save      p50    6.499/11.194   p95  151.074/199.253  delta  +4.696 ms  control-drift
  total store_read_bytes p50 5311407  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189191..4189195 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099132..1099136 (3 distinct values)
  SPREAD   a session_save store_read_bytes 23064..23089 (7 distinct values)
  SPREAD   b attach_history store_read_bytes 4189193..4189195 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099135..1099136 (2 distinct values)
  SPREAD   b session_save store_read_bytes 23070..23086 (6 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    2.264/2.279    p95    2.373/2.395    delta  +0.015 ms  control-no-change
  skill_request_overlay p50    3.907/3.893    p95    3.941/3.970    delta  -0.014 ms  control-no-change
  mcp_config_load   p50    0.134/0.132    p95    0.140/0.138    delta  -0.001 ms  control-no-change
  attach_history    p50  254.614/253.201  p95  264.492/368.866  delta  -1.413 ms  control-no-change
  resume_ready      p50   44.072/44.350   p95   44.605/44.959   delta  +0.278 ms  control-no-change
  provider_dispatch p50    1.092/1.081    p95    1.107/1.109    delta  -0.011 ms  control-no-change
  session_save      p50    6.277/6.453    p95   17.437/22.067   delta  +0.177 ms  control-no-change
  total store_read_bytes p50 5311411  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 23058..23094 (6 distinct values)
  SPREAD   b attach_history store_read_bytes 4189187..4189195 (4 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099128..1099136 (4 distinct values)
  SPREAD   b session_save store_read_bytes 23070..23098 (8 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.013/0.013    p95    0.015/0.015    delta  -0.000 ms  control-no-change
  skill_request_overlay p50    0.096/0.099    p95    0.114/0.120    delta  +0.003 ms  control-no-change
  mcp_config_load   p50    0.125/0.129    p95    0.130/0.137    delta  +0.003 ms  control-no-change
  attach_history    p50 3264.246/3257.788 p95 3310.492/3382.026 delta  -6.458 ms  control-no-change
  resume_ready      p50  505.573/500.376  p95  517.983/514.342  delta  -5.197 ms  control-no-change
  provider_dispatch p50   12.969/12.784   p95   13.541/14.071   delta  -0.185 ms  control-no-change
  session_save      p50    6.822/6.865    p95   69.439/40.233   delta  +0.043 ms  control-no-change
  total store_read_bytes p50 53417620  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184285..42184293 (4 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210238..11210246 (4 distinct values)
  SPREAD   a session_save store_read_bytes 23072..23097 (8 distinct values)
  SPREAD   b attach_history store_read_bytes 42184287..42184293 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210241..11210246 (2 distinct values)
  SPREAD   b session_save store_read_bytes 23067..23093 (6 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    2.320/2.407    p95    2.488/2.480    delta  +0.087 ms  control-drift
  skill_request_overlay p50    3.883/3.889    p95    4.006/3.996    delta  +0.005 ms  control-no-change
  mcp_config_load   p50    0.134/0.134    p95    0.146/0.140    delta  -0.000 ms  control-no-change
  attach_history    p50 3292.036/3311.115 p95 3338.041/3347.671 delta +19.079 ms  control-no-change
  resume_ready      p50  517.792/515.955  p95  543.367/535.445  delta  -1.837 ms  control-no-change
  provider_dispatch p50   14.173/13.729   p95   15.050/14.895   delta  -0.444 ms  control-no-change
  session_save      p50    6.913/6.818    p95   15.558/55.014   delta  -0.095 ms  control-no-change
  total store_read_bytes p50 53417612  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184287..42184291 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210241..11210245 (2 distinct values)
  SPREAD   a session_save store_read_bytes 23064..23096 (6 distinct values)
  SPREAD   b attach_history store_read_bytes 42184289..42184291 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210244..11210245 (2 distinct values)
  SPREAD   b session_save store_read_bytes 23068..23088 (7 distinct values)
report   bench-results/loading-cold.json
mode cold, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### 세션 로딩 (WARM)

```text
binary   /home/runner/work/agent/agent/bench-bin/examples/loading_probe (e05ee8546af7)
mode     warm
protocol WARM: one HOME per side, --warmup discarded runs per side, then measured runs alternate between the two sides on those same HOMEs.
fixtures /tmp/horon-loading-bench-5pivi16u
fixture events=0 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    0.007/0.007    p95    0.008/0.008    delta  -0.000 ms  control-no-change
  skill_request_overlay p50    0.040/0.041    p95    0.054/0.069    delta  +0.001 ms  control-no-change
  mcp_config_load   p50    0.057/0.059    p95    0.073/0.087    delta  +0.002 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  session_save      p50    7.741/7.609    p95   11.868/20.249   delta  -0.132 ms  control-no-change
  total store_read_bytes p50 23790  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23073, 23077, 23089, 23788, 23792, 24509, 24511, 24519]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23077, 23081, 23788, 23796, 23804, 24511, 24515]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23073..24519 (8 distinct values)
  SPREAD   b session_save store_read_bytes 23077..24515 (7 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    1.454/1.454    p95    1.556/1.539    delta  -0.000 ms  control-no-change
  skill_request_overlay p50    1.986/1.993    p95    2.023/2.192    delta  +0.008 ms  control-no-change
  mcp_config_load   p50    0.062/0.063    p95    0.076/0.074    delta  +0.000 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.001/0.000    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.001    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.001    delta  -0.000 ms  control-no-change
  session_save      p50   15.147/13.768   p95  282.307/300.454  delta  -1.380 ms  control-no-change
  total store_read_bytes p50 23791  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23075, 23077, 23079, 23081, 23784, 23798, 23804, 24515, 24519, 24525]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23067, 23081, 23089, 23794, 23796, 23800, 24511, 24521]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23075..24525 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23067..24521 (8 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.006/0.006    p95    0.007/0.007    delta  +0.000 ms  control-no-change
  skill_request_overlay p50    0.041/0.041    p95    0.052/0.058    delta  -0.000 ms  control-no-change
  mcp_config_load   p50    0.058/0.058    p95    0.067/0.060    delta  -0.001 ms  control-no-change
  attach_history    p50   63.280/62.878   p95   66.514/65.925   delta  -0.403 ms  control-no-change
  resume_ready      p50   29.781/29.864   p95   30.271/31.423   delta  +0.083 ms  control-no-change
  provider_dispatch p50    1.002/1.006    p95    1.016/1.020    delta  +0.004 ms  control-no-change
  session_save      p50    9.087/7.953    p95   19.492/12.393   delta  -1.135 ms  control-no-change
  total store_read_bytes p50 2841382  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23799, 23806, 24510, 24517, 25239, 25245, 25960, 25970, 26642, 26671]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23789, 23791, 24512, 24514, 25221, 25233, 25235, 25952, 25956, 26663]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23799..26671 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23789..26663 (10 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    1.494/1.503    p95    1.575/1.576    delta  +0.008 ms  control-no-change
  skill_request_overlay p50    2.001/2.013    p95    2.036/2.034    delta  +0.012 ms  control-no-change
  mcp_config_load   p50    0.063/0.063    p95    0.064/0.066    delta  -0.000 ms  control-no-change
  attach_history    p50   64.365/64.066   p95   66.331/66.507   delta  -0.299 ms  control-no-change
  resume_ready      p50   30.101/29.810   p95   31.944/30.424   delta  -0.291 ms  control-no-change
  provider_dispatch p50    0.991/0.989    p95    1.042/1.029    delta  -0.003 ms  control-no-change
  session_save      p50   10.818/10.861   p95   12.696/20.904   delta  +0.043 ms  control-no-change
  total store_read_bytes p50 2841361  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23793, 23797, 24520, 25227, 25231, 25950, 25954, 26665, 26689]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23799, 23803, 24506, 24514, 25223, 25233, 25952, 25954, 26661]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23793..26689 (9 distinct values)
  SPREAD   b session_save store_read_bytes 23799..26661 (9 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.006/0.006    p95    0.008/0.009    delta  +0.000 ms  control-no-change
  skill_request_overlay p50    0.041/0.041    p95    0.042/0.059    delta  +0.000 ms  control-no-change
  mcp_config_load   p50    0.059/0.058    p95    0.075/0.076    delta  -0.000 ms  control-no-change
  attach_history    p50  749.229/745.610  p95  766.524/758.157  delta  -3.619 ms  control-no-change
  resume_ready      p50  374.758/374.842  p95  379.667/382.063  delta  +0.084 ms  control-no-change
  provider_dispatch p50   12.034/12.205   p95   12.550/12.612   delta  +0.171 ms  control-no-change
  session_save      p50    8.400/8.359    p95  119.898/14.126   delta  -0.040 ms  control-no-change
  total store_read_bytes p50 28640394  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23796, 23804, 24505, 24519, 25244, 25246, 25961, 25963, 26678, 26680]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23804, 23808, 24515, 24523, 25226, 25248, 25953, 26656, 26680]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23796..26680 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23804..26680 (9 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    1.525/1.474    p95    1.728/1.659    delta  -0.052 ms  control-no-change
  skill_request_overlay p50    2.016/2.022    p95    2.089/2.137    delta  +0.006 ms  control-no-change
  mcp_config_load   p50    0.063/0.064    p95    0.064/0.071    delta  +0.000 ms  control-no-change
  attach_history    p50  754.718/756.876  p95  757.635/764.178  delta  +2.158 ms  control-no-change
  resume_ready      p50  388.554/387.829  p95  394.518/391.664  delta  -0.725 ms  control-no-change
  provider_dispatch p50   12.382/12.229   p95   12.782/12.547   delta  -0.152 ms  control-no-change
  session_save      p50   13.133/11.621   p95   35.643/20.138   delta  -1.511 ms  control-no-change
  total store_read_bytes p50 28640383  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23800, 23804, 24515, 24519, 25226, 25957, 26680, 26684]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23796, 24505, 24519, 25228, 25242, 25945, 25961, 26672, 26680]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23800..26684 (8 distinct values)
  SPREAD   b session_save store_read_bytes 23796..26680 (9 distinct values)
report   bench-results/loading-warm.json
mode warm, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### Tool latency

| Tool case | median | p95 | min | max | samples |
|---|---|---|---|---|---|
| read (tiny file) | 0.3 ms | 0.4 ms | 0.3 ms | 0.4 ms | 5 |
| read (1000 lines) | 0.5 ms | 0.5 ms | 0.4 ms | 0.5 ms | 5 |
| read (10 lines @ offset) | 0.5 ms | 0.6 ms | 0.4 ms | 0.6 ms | 5 |
| read (repo Cargo.toml) | 0.4 ms | 0.5 ms | 0.4 ms | 0.5 ms | 5 |
| write (small) | 1.3 ms | 1.4 ms | 1.2 ms | 1.4 ms | 5 |
| write (10KB) | 1.6 ms | 3.8 ms | 1.5 ms | 3.8 ms | 5 |
| edit (simple replace) | 1.3 ms | 1.5 ms | 1.2 ms | 1.5 ms | 5 |
| ls (scripts dir) | 0.8 ms | 0.8 ms | 0.7 ms | 0.8 ms | 5 |
| agentgrep (grep in tree) | 1.7 ms | 1.9 ms | 1.7 ms | 1.9 ms | 5 |
| agentgrep (find **/*.rs) | 1.6 ms | 1.6 ms | 1.5 ms | 1.6 ms | 5 |
| bash (true) | 2.1 ms | 2.2 ms | 2.1 ms | 2.2 ms | 5 |
| bash (echo hello) | 2.1 ms | 2.4 ms | 2.0 ms | 2.4 ms | 5 |
| bash (ls fixture tree) | 3.6 ms | 3.7 ms | 3.5 ms | 3.7 ms | 5 |
| invalid tool (dispatch only) | 0.1 ms | 0.2 ms | 0.1 ms | 0.2 ms | 5 |

#### Reload storm

1 resume(s) per level; each level resumes sessions no other level has opened, from a freshly started daemon.

| Concurrent resumes | history median | p95 | max | storm wall | failures |
|---|---|---|---|---|---|
| 1 | 51.4 ms | 51.4 ms | 51.4 ms | 51.6 ms | 0 |
| 10 | 163.9 ms | 183.6 ms | 183.6 ms | 183.8 ms | 0 |
| 40 | 736.8 ms | 742.6 ms | 743.4 ms | 743.8 ms | 0 |

#### Compile loop

| Checkpoint | Touched | median | min | max | samples |
|---|---|---|---|---|---|
| warm_noop | — | 232.7 ms | 232.7 ms | 232.7 ms | 1 |
| leaf_touch | src/main.rs | 366.7 ms | 366.7 ms | 366.7 ms | 1 |
| core_touch | crates/app-core/src/lib.rs | 20.71 s | 20.71 s | 20.71 s | 1 |
| build_meta_touch | crates/build-meta/src/lib.rs | 29.27 s | 29.27 s | 29.27 s | 1 |

### macOS

#### Coding agent comparison (headless, mock model)

Host: darwin/arm64, 3 CPUs, Python 3.12.10. Baseline: **horon**. Samples per one-shot, skill and MCP cell: 5.

Skipped: syscalls (strace not available)

| Tool | Version | `--version` (median) | Install size |
|---|---|---|---|
| horon | horon-agent v0.1.0-dev (805b408) | 14.3 ms | 96.3 MB |
| jcode | jcode v0.99.1 (7206efd0c) | 10.5 ms | 136.3 MB |
| Claude Code | 2.1.317 (Claude Code) | 24.2 ms | 596.8 MB |
| Codex CLI | codex-cli 0.166.0 | 82.2 ms | 346.5 MB |
| OpenCode | 1.19.5 | 585.8 ms | 344.6 MB |
| pi | 1.5.0 | 419.0 ms | 108.9 MB |
| oh-my-pi (omp) | omp/18.18.0 | 718.6 ms | 273.5 MB |
| oh-my-openagent (omo) | omo 5.1.64 (engine: senpi 2026.11.5-1) | 158.6 ms | 502.3 MB |

**One-shot headless prompt** (includes startup, one `ls` tool call, final answer)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Tool round-trip | Note |
|---|---|---|---|---|---|---|---|
| horon | 103.3 ms | 79.8 ms–527.4 ms | baseline | 30.7 MB | baseline | yes | — |
| jcode | 413.7 ms | 352.1 ms–588.0 ms | 4.00× more | 34.0 MB | 1.11× more | yes | — |
| Claude Code | 551.7 ms | 499.9 ms–1.02 s | 5.34× more | 199.9 MB | 6.50× more | yes | — |
| Codex CLI | 915.0 ms | 819.8 ms–2.31 s | 8.86× more | 143.6 MB | 4.67× more | yes | — |
| OpenCode | 3.60 s | 3.47 s–9.67 s | 34.81× more | 764.2 MB | 24.86× more | yes | — |
| pi | 585.9 ms | 433.5 ms–626.1 ms | 5.67× more | 125.1 MB | 4.07× more | yes | — |
| oh-my-pi (omp) | 2.30 s | 2.05 s–2.66 s | 22.28× more | 414.1 MB | 13.47× more | yes | — |
| oh-my-openagent (omo) | 10.10 s | 9.41 s–11.74 s | 97.79× more | 889.8 MB | 28.95× more | yes | — |

**Skill invocation** (process start to final answer, including skill discovery and one load of the fixture skill)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | Note |
|---|---|---|---|---|---|---|---|---|
| horon | 84.5 ms | 61.9 ms–108.9 ms | baseline | 30.2 MB | baseline | skill-uri | yes | — |
| jcode | 354.9 ms | 291.0 ms–366.2 ms | 4.20× more | 28.4 MB | 1.06× less | skill-tool | yes | — |
| Claude Code | 816.2 ms | 772.2 ms–1.07 s | 9.66× more | 186.0 MB | 6.15× more | skill-tool | yes | — |
| Codex CLI | 919.0 ms | 672.2 ms–1.65 s | 10.88× more | 136.2 MB | 4.51× more | file-read | yes | — |
| OpenCode | 5.14 s | 4.63 s–7.77 s | 60.90× more | 710.0 MB | 23.48× more | skill-tool | yes | — |
| pi | 601.2 ms | 568.2 ms–681.8 ms | 7.12× more | 132.0 MB | 4.36× more | file-read | yes | — |
| oh-my-pi (omp) | 2.09 s | 1.74 s–2.66 s | 24.69× more | 401.0 MB | 13.26× more | file-read | yes | — |
| oh-my-openagent (omo) | 10.53 s | 9.13 s–12.19 s | 124.61× more | 885.2 MB | 29.28× more | file-read | yes | — |

**MCP tool call** (process start to final answer, including starting the fixture stdio MCP server and one `bench_echo` call)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | MCP ready | Note |
|---|---|---|---|---|---|---|---|---|---|
| horon | 95.5 ms | 92.2 ms–111.9 ms | baseline | 19.1 MB | baseline | mcp-call | yes | 77.5 ms | — |
| jcode | 453.0 ms | 358.2 ms–551.5 ms | 4.75× more | 54.7 MB | 2.86× more | mcp-action | yes | 417.8 ms | — |
| Claude Code | 835.0 ms | 806.0 ms–1.25 s | 8.75× more | 227.2 MB | 11.88× more | mcp-tool | yes | 674.2 ms | — |
| Codex CLI | 1.14 s | 681.3 ms–1.47 s | 11.99× more | 143.3 MB | 7.49× more | mcp-tool | yes | 789.8 ms | — |
| OpenCode | 4.34 s | 3.91 s–5.07 s | 45.45× more | 693.5 MB | 36.26× more | mcp-tool | yes | 3.85 s | — |
| pi | 725.3 ms | 666.7 ms–819.1 ms | 7.60× more | 153.7 MB | 8.04× more | mcp-tool | yes | 628.6 ms | — |
| oh-my-pi (omp) | 2.46 s | 2.00 s–2.75 s | 25.78× more | 393.5 MB | 20.58× more | mcp-route | yes | 2.10 s | — |
| oh-my-openagent (omo) | — | — | — | — | — | mcp-tool | no | — | failed: the fixture sentinel never came back |

**Session memory** (idle after 3 turn(s) per session; sizes are process-tree sums, RSS; PSS also recorded on Linux). "Each additional" is the least-squares slope across the measured session counts; 0 B means growth stayed below RSS sampling noise. Threads and descriptors come from the daemon's own `server:memory` after the turns.

| Tool | First session | Each additional | vs horon (additional) | Threads/session | FDs/session | First (post-turn) | Each additional (post-turn) |
|---|---|---|---|---|---|---|---|
| horon | 28.6 MB | 166.4 KB | baseline | 0.20 | 1.00 | 34.9 MB | 382.8 KB |
| jcode | 36.5 MB | 197.1 KB | 1.18× more | — | — | 40.0 MB | 349.6 KB |
| Claude Code | 156.8 MB | 145.3 MB | 894.13× more | — | — | 189.4 MB | 181.9 MB |
| Codex CLI | 101.2 MB | 79.3 MB | 488.18× more | — | — | 106.8 MB | 107.7 MB |
| OpenCode | 265.1 MB | 18.2 MB | 111.87× more | — | — | 569.6 MB | 43.7 MB |
| pi | 116.0 MB | 115.1 MB | 708.00× more | — | — | 147.9 MB | 147.6 MB |
| oh-my-pi (omp) | 341.9 MB | 95.3 MB | 586.29× more | — | — | 447.1 MB | 316.7 MB |
| oh-my-openagent (omo) | 57.8 MB | 36.2 MB | 222.59× more | — | — | 809.5 MB | 340.4 MB |

Threads/descriptors unavailable: jcode (server:memory did not report thread_count, fd_count)

**Post-turn tree RSS per session count**

| Tool | 1 session(s) | 5 session(s) | 10 session(s) |
|---|---|---|---|
| horon | 34.9 MB | 36.5 MB | 38.3 MB |
| jcode | 40.0 MB | 41.5 MB | 43.1 MB |
| Claude Code | 189.4 MB | 929.4 MB | 1.8 GB |
| Codex CLI | 106.8 MB | 535.4 MB | 1.1 GB |
| OpenCode | 569.6 MB | 603.0 MB | 952.5 MB |
| pi | 147.9 MB | 738.2 MB | 1.4 GB |
| oh-my-pi (omp) | 447.1 MB | 1.8 GB | 3.2 GB |
| oh-my-openagent (omo) | 809.5 MB | 3.3 GB | 3.9 GB |

#### 기동

```text
Benchmarking: /Users/runner/work/agent/agent/bench-bin/horon-agent
============================================================

--help (binary load):
  Min:    11.03 ms
  Max:    28.79 ms
  Mean:   20.23 ms
  Median: 21.04 ms
  Stdev:  5.72 ms

--version:
  Min:    16.76 ms
  Max:    26.13 ms
  Mean:   20.58 ms
  Median: 19.30 ms
  Stdev:  3.22 ms

Measuring isolated server startup (10 runs)...

Server ready (isolated socket connectable):
  Min:    26.31 ms
  Max:    99.05 ms
  Mean:   65.55 ms
  Median: 67.14 ms
  Stdev:  27.84 ms

Measuring harness bridge readiness (10 runs)...

Bridge stage: api socket connectable:
  Min:    23.87 ms
  Max:    93.20 ms
  Mean:   57.66 ms
  Median: 57.00 ms
  Stdev:  23.54 ms

Bridge stage: hello_ok:
  Min:    24.45 ms
  Max:    94.88 ms
  Mean:   58.95 ms
  Median: 58.62 ms
  Stdev:  23.70 ms

Bridge stage: no-op reply:
  Min:    24.88 ms
  Max:    95.72 ms
  Mean:   59.61 ms
  Median: 59.34 ms
  Stdev:  23.72 ms

Bridge total (socket -> no-op):
  Min:    24.88 ms
  Max:    95.72 ms
  Mean:   59.61 ms
  Median: 59.34 ms
  Stdev:  23.72 ms

============================================================
Summary:
  Binary load median:      ~21.0 ms
  Server ready median:     ~67.1 ms
  Bridge total median:     ~59.3 ms
```

#### 세션 로딩 (COLD)

```text
binary   /Users/runner/work/agent/agent/bench-bin/examples/loading_probe (4e8292a1600d)
mode     cold
protocol COLD: every measured run gets its own freshly copied HOME; no derived cache from an earlier run exists, so each sample is a first load of the fixture.
fixtures /var/folders/h5/t85nd63d71x27_8gl5gw3l_w0000gn/T/horon-loading-bench-up3jn8s0
note     --warmup 3 is ignored in cold mode: every sample gets a fresh HOME
fixture events=0 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    0.031/0.024    p95    0.151/0.065    delta  -0.007 ms  control-no-change
  skill_request_overlay p50    0.221/0.216    p95    0.838/0.994    delta  -0.005 ms  control-no-change
  mcp_config_load   p50    0.262/0.260    p95    1.152/0.726    delta  -0.003 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.001/0.001    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.001/0.001    delta  +0.000 ms  control-drift
  provider_dispatch p50    0.000/0.000    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  session_save      p50   19.730/17.526   p95   41.201/34.927   delta  -2.204 ms  control-no-change
  total store_read_bytes p50 22950  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 22942..22954 (4 distinct values)
  SPREAD   b session_save store_read_bytes 22942..22958 (5 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    4.113/4.104    p95    4.713/5.798    delta  -0.008 ms  control-no-change
  skill_request_overlay p50    5.288/5.268    p95    6.930/10.609   delta  -0.020 ms  control-no-change
  mcp_config_load   p50    0.269/0.277    p95    0.358/0.373    delta  +0.007 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  session_save      p50   18.093/20.185   p95   29.027/96.929   delta  +2.092 ms  control-no-change
  total store_read_bytes p50 22946  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 22934..22950 (5 distinct values)
  SPREAD   b session_save store_read_bytes 22938..22954 (4 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.026/0.028    p95    0.093/0.346    delta  +0.002 ms  control-no-change
  skill_request_overlay p50    0.217/0.229    p95    0.466/0.419    delta  +0.013 ms  control-no-change
  mcp_config_load   p50    0.283/0.270    p95    0.525/0.490    delta  -0.013 ms  control-no-change
  attach_history    p50  359.598/355.779  p95  435.644/437.528  delta  -3.820 ms  control-no-change
  resume_ready      p50   69.177/72.366   p95   92.098/193.195  delta  +3.189 ms  control-no-change
  provider_dispatch p50    1.189/1.071    p95    2.071/2.102    delta  -0.118 ms  control-no-change
  session_save      p50   21.980/18.997   p95   75.975/149.671  delta  -2.983 ms  control-no-change
  total store_read_bytes p50 5311296  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189205..4189207 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099144..1099145 (2 distinct values)
  SPREAD   a session_save store_read_bytes 22944..22964 (5 distinct values)
  SPREAD   b attach_history store_read_bytes 4189203..4189207 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099141..1099145 (2 distinct values)
  SPREAD   b session_save store_read_bytes 22948..22972 (6 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    4.029/4.119    p95    5.174/4.988    delta  +0.090 ms  control-no-change
  skill_request_overlay p50    5.694/5.258    p95    7.188/6.068    delta  -0.435 ms  control-no-change
  mcp_config_load   p50    0.284/0.273    p95    0.355/0.516    delta  -0.011 ms  control-no-change
  attach_history    p50  313.061/303.770  p95  337.142/363.559  delta  -9.291 ms  control-no-change
  resume_ready      p50   63.776/58.438   p95   85.345/63.370   delta  -5.338 ms  control-no-change
  provider_dispatch p50    1.075/1.052    p95    1.826/1.203    delta  -0.024 ms  control-no-change
  session_save      p50   20.112/18.431   p95   28.453/23.819   delta  -1.681 ms  control-no-change
  total store_read_bytes p50 5311308  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189203..4189207 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099141..1099145 (2 distinct values)
  SPREAD   a session_save store_read_bytes 22944..22972 (6 distinct values)
  SPREAD   b attach_history store_read_bytes 4189199..4189207 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099137..1099145 (3 distinct values)
  SPREAD   b session_save store_read_bytes 22936..22972 (7 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.027/0.028    p95    0.060/0.038    delta  +0.001 ms  control-no-change
  skill_request_overlay p50    0.214/0.225    p95    0.530/0.326    delta  +0.011 ms  control-no-change
  mcp_config_load   p50    0.252/0.264    p95   10.177/0.420    delta  +0.012 ms  control-no-change
  attach_history    p50 3884.968/4180.926 p95 4135.646/4313.700 delta +295.957 ms  control-no-change
  resume_ready      p50  780.620/777.810  p95  856.050/869.334  delta  -2.810 ms  control-no-change
  provider_dispatch p50   10.096/11.193   p95   13.482/17.148   delta  +1.097 ms  control-drift
  session_save      p50   23.900/25.893   p95   33.753/134.738  delta  +1.993 ms  control-no-change
  total store_read_bytes p50 53417519  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184301..42184305 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210251..11210255 (2 distinct values)
  SPREAD   a session_save store_read_bytes 22935..22967 (6 distinct values)
  SPREAD   b attach_history store_read_bytes 42184301..42184305 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210251..11210255 (3 distinct values)
  SPREAD   b session_save store_read_bytes 22939..22975 (9 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    5.285/4.163    p95    8.539/5.178    delta  -1.122 ms  control-no-change
  skill_request_overlay p50    5.527/5.273    p95    8.302/8.916    delta  -0.254 ms  control-no-change
  mcp_config_load   p50    0.268/0.282    p95    0.422/0.372    delta  +0.014 ms  control-no-change
  attach_history    p50 3690.017/3752.381 p95 4552.464/4148.584 delta +62.365 ms  control-no-change
  resume_ready      p50  788.908/773.100  p95 1004.648/885.420  delta -15.808 ms  control-no-change
  provider_dispatch p50   11.393/11.268   p95   18.122/15.055   delta  -0.125 ms  control-no-change
  session_save      p50   30.000/27.425   p95   58.466/129.763  delta  -2.575 ms  control-no-change
  total store_read_bytes p50 53417516  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184297..42184305 (4 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210251..11210255 (3 distinct values)
  SPREAD   a session_save store_read_bytes 22954..22967 (6 distinct values)
  SPREAD   b attach_history store_read_bytes 42184303..42184305 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210254..11210255 (2 distinct values)
  SPREAD   b session_save store_read_bytes 22951..22979 (7 distinct values)
report   bench-results/loading-cold.json
mode cold, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### 세션 로딩 (WARM)

```text
binary   /Users/runner/work/agent/agent/bench-bin/examples/loading_probe (4e8292a1600d)
mode     warm
protocol WARM: one HOME per side, --warmup discarded runs per side, then measured runs alternate between the two sides on those same HOMEs.
fixtures /var/folders/h5/t85nd63d71x27_8gl5gw3l_w0000gn/T/horon-loading-bench-gsqng4s6
fixture events=0 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    0.025/0.024    p95    0.034/0.028    delta  -0.001 ms  control-no-change
  skill_request_overlay p50    0.212/0.216    p95    0.257/0.244    delta  +0.004 ms  control-no-change
  mcp_config_load   p50    0.253/0.251    p95    0.295/0.285    delta  -0.002 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  session_save      p50   11.287/11.476   p95   12.685/12.762   delta  +0.188 ms  control-no-change
  total store_read_bytes p50 23658  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [22939, 22947, 22951, 23657, 23659, 23665, 24375, 24379]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [22935, 22937, 22943, 22959, 23653, 23661, 23669, 24371, 24375, 24387]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 22939..24379 (8 distinct values)
  SPREAD   b session_save store_read_bytes 22935..24387 (10 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    4.115/3.763    p95    5.483/5.454    delta  -0.352 ms  control-no-change
  skill_request_overlay p50    5.401/4.859    p95    5.871/5.831    delta  -0.543 ms  control-no-change
  mcp_config_load   p50    0.283/0.253    p95    0.571/0.315    delta  -0.031 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-drift
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.001/0.000    delta  -0.000 ms  control-no-change
  session_save      p50   12.724/13.101   p95   18.943/14.996   delta  +0.377 ms  control-no-change
  total store_read_bytes p50 23670  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [22941, 22943, 22945, 23647, 23667, 23673, 24361, 24369, 24377, 24385]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [22931, 22947, 22959, 23653, 23657, 23669, 24363, 24371, 24383]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 22941..24385 (10 distinct values)
  SPREAD   b session_save store_read_bytes 22931..24383 (9 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.026/0.036    p95    0.043/0.130    delta  +0.010 ms  control-no-change
  skill_request_overlay p50    0.248/0.242    p95    0.286/0.505    delta  -0.005 ms  control-no-change
  mcp_config_load   p50    0.265/0.309    p95    0.446/0.543    delta  +0.044 ms  control-drift
  attach_history    p50  119.945/122.276  p95  140.438/168.354  delta  +2.331 ms  control-no-change
  resume_ready      p50   74.810/79.387   p95   95.116/113.012  delta  +4.578 ms  control-no-change
  provider_dispatch p50    1.128/1.281    p95    1.225/2.267    delta  +0.153 ms  control-no-change
  session_save      p50   15.786/15.832   p95   58.903/24.104   delta  +0.046 ms  control-no-change
  total store_read_bytes p50 2841243  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23666, 23668, 24366, 24368, 25084, 25102, 25802, 25812, 26506, 26530]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23662, 23666, 24382, 24388, 25100, 25110, 25804, 25806, 26516, 26530]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23666..26530 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23662..26530 (10 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    4.036/4.084    p95    4.607/6.540    delta  +0.048 ms  control-no-change
  skill_request_overlay p50    5.362/4.994    p95    5.898/8.391    delta  -0.368 ms  control-no-change
  mcp_config_load   p50    0.274/0.271    p95    0.310/0.607    delta  -0.003 ms  control-no-change
  attach_history    p50  106.499/103.155  p95  116.749/129.301  delta  -3.343 ms  control-no-change
  resume_ready      p50   63.666/64.353   p95  101.869/73.112   delta  +0.687 ms  control-no-change
  provider_dispatch p50    1.133/1.050    p95    1.480/1.195    delta  -0.083 ms  control-no-change
  session_save      p50   15.129/13.859   p95   21.149/18.521   delta  -1.270 ms  control-no-change
  total store_read_bytes p50 2841254  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23670, 23672, 24380, 24394, 25090, 25102, 25800, 25812, 26526, 26530]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23654, 23670, 24388, 24396, 25098, 25106, 25812, 25820, 26518, 26526]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23670..26530 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23654..26526 (10 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.030/0.029    p95    0.087/0.038    delta  -0.001 ms  control-no-change
  skill_request_overlay p50    0.247/0.241    p95    0.603/0.442    delta  -0.006 ms  control-no-change
  mcp_config_load   p50    0.276/0.277    p95    0.723/0.551    delta  +0.000 ms  control-no-change
  attach_history    p50 1581.656/1601.475 p95 1984.641/2047.052 delta +19.820 ms  control-no-change
  resume_ready      p50 1052.308/988.267  p95 1279.042/1278.184 delta -64.041 ms  control-no-change
  provider_dispatch p50   12.787/13.215   p95   19.437/19.272   delta  +0.429 ms  control-no-change
  session_save      p50   26.001/22.715   p95   49.553/65.601   delta  -3.286 ms  control-no-change
  total store_read_bytes p50 28640276  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23671, 23677, 24385, 24387, 25085, 25117, 25815, 26515, 26533]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23661, 23685, 24383, 24387, 25099, 25109, 25799, 25821, 26525, 26527]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23671..26533 (9 distinct values)
  SPREAD   b session_save store_read_bytes 23661..26527 (10 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    5.337/5.015    p95    8.842/15.644   delta  -0.322 ms  control-no-change
  skill_request_overlay p50    6.190/6.056    p95    7.240/18.632   delta  -0.135 ms  control-no-change
  mcp_config_load   p50    0.340/0.303    p95    0.874/0.841    delta  -0.037 ms  control-no-change
  attach_history    p50 1371.598/1295.943 p95 1560.019/1547.454 delta -75.655 ms  control-no-change
  resume_ready      p50  860.208/766.411  p95 1271.788/971.724  delta -93.797 ms  control-no-change
  provider_dispatch p50   12.780/11.164   p95   23.227/15.628   delta  -1.616 ms  control-no-change
  session_save      p50   27.722/21.600   p95   48.236/32.286   delta  -6.122 ms  control-no-change
  total store_read_bytes p50 28640272  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23669, 23681, 24379, 24383, 25105, 25815, 25831, 26529, 26537]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23677, 23685, 24387, 25093, 25109, 25823, 26529, 26533]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23669..26537 (9 distinct values)
  SPREAD   b session_save store_read_bytes 23677..26533 (8 distinct values)
report   bench-results/loading-warm.json
mode warm, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### Tool latency

| Tool case | median | p95 | min | max | samples |
|---|---|---|---|---|---|
| read (tiny file) | 0.5 ms | 0.6 ms | 0.3 ms | 0.6 ms | 5 |
| read (1000 lines) | 0.7 ms | 0.8 ms | 0.6 ms | 0.8 ms | 5 |
| read (10 lines @ offset) | 0.6 ms | 0.7 ms | 0.6 ms | 0.7 ms | 5 |
| read (repo Cargo.toml) | 0.4 ms | 0.8 ms | 0.4 ms | 0.8 ms | 5 |
| write (small) | 2.6 ms | 2.9 ms | 2.0 ms | 2.9 ms | 5 |
| write (10KB) | 3.0 ms | 3.5 ms | 2.7 ms | 3.5 ms | 5 |
| edit (simple replace) | 2.4 ms | 2.6 ms | 2.4 ms | 2.6 ms | 5 |
| ls (scripts dir) | 0.9 ms | 1.1 ms | 0.8 ms | 1.1 ms | 5 |
| agentgrep (grep in tree) | 4.0 ms | 4.1 ms | 3.5 ms | 4.1 ms | 5 |
| agentgrep (find **/*.rs) | 1.9 ms | 3.6 ms | 1.7 ms | 3.6 ms | 5 |
| bash (true) | 6.7 ms | 8.2 ms | 5.4 ms | 8.2 ms | 5 |
| bash (echo hello) | 6.4 ms | 8.4 ms | 5.5 ms | 8.4 ms | 5 |
| bash (ls fixture tree) | 8.1 ms | 9.4 ms | 7.9 ms | 9.4 ms | 5 |
| invalid tool (dispatch only) | 0.1 ms | 0.1 ms | 0.1 ms | 0.1 ms | 5 |

#### Reload storm

1 resume(s) per level; each level resumes sessions no other level has opened, from a freshly started daemon.

| Concurrent resumes | history median | p95 | max | storm wall | failures |
|---|---|---|---|---|---|
| 1 | 84.9 ms | 84.9 ms | 84.9 ms | 85.1 ms | 0 |
| 10 | 310.6 ms | 313.1 ms | 313.1 ms | 313.3 ms | 0 |
| 40 | 1.37 s | 1.38 s | 1.38 s | 1.38 s | 0 |

#### Compile loop

| Checkpoint | Touched | median | min | max | samples |
|---|---|---|---|---|---|
| warm_noop | — | 1.02 s | 1.02 s | 1.02 s | 1 |
| leaf_touch | src/main.rs | 512.5 ms | 512.5 ms | 512.5 ms | 1 |
| core_touch | crates/app-core/src/lib.rs | 19.70 s | 19.70 s | 19.70 s | 1 |
| build_meta_touch | crates/build-meta/src/lib.rs | 33.44 s | 33.44 s | 33.44 s | 1 |

### Windows

#### Coding agent comparison (headless, mock model)

Host: windows/AMD64, 4 CPUs, Python 3.12.10. Baseline: **horon**. Samples per one-shot, skill and MCP cell: 5.

Skipped: syscalls (strace not available)

| Tool | Version | `--version` (median) | Install size |
|---|---|---|---|
| horon | horon-agent v0.1.0-dev (805b408) | 14.1 ms | 95.8 MB |
| jcode | jcode v0.99.1 (7206efd0c) | 16.1 ms | 128.4 MB |
| Claude Code | 2.1.317 (Claude Code) | 43.2 ms | 645.2 MB |
| Codex CLI | codex-cli 0.166.0 | 106.7 ms | 479.4 MB |
| OpenCode | 1.19.5 | 597.2 ms | 605.3 MB |
| pi | 1.5.0 | 326.2 ms | 108.9 MB |
| oh-my-pi (omp) | omp/18.18.0 | 782.4 ms | 291.7 MB |
| oh-my-openagent (omo) | omo 5.1.64 (engine: senpi 2026.11.5-1) | 226.3 ms | 514.8 MB |

**One-shot headless prompt** (includes startup, one `ls` tool call, final answer)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Tool round-trip | Note |
|---|---|---|---|---|---|---|---|
| horon | 208.5 ms | 207.4 ms–293.3 ms | baseline | 30.5 MB | baseline | yes | — |
| jcode | 153.3 ms | 142.3 ms–237.3 ms | 1.36× less | 30.2 MB | same | yes | — |
| Claude Code | 1.62 s | 1.42 s–2.18 s | 7.75× more | 3.0 GB | 102.44× more | yes | — |
| Codex CLI | 1.98 s | 1.80 s–2.59 s | 9.48× more | 256.8 MB | 8.43× more | yes | — |
| OpenCode | 5.72 s | 5.57 s–9.04 s | 27.44× more | 910.9 MB | 29.90× more | yes | — |
| pi | 572.1 ms | 564.6 ms–644.7 ms | 2.74× more | 120.2 MB | 3.95× more | yes | — |
| oh-my-pi (omp) | 3.40 s | 3.24 s–3.61 s | 16.33× more | 409.9 MB | 13.45× more | yes | — |
| oh-my-openagent (omo) | 21.21 s | 18.60 s–22.94 s | 101.75× more | 543.1 MB | 17.83× more | yes | — |

**Skill invocation** (process start to final answer, including skill discovery and one load of the fixture skill)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | Note |
|---|---|---|---|---|---|---|---|---|
| horon | 220.5 ms | 213.8 ms–222.3 ms | baseline | 25.2 MB | baseline | skill-uri | yes | — |
| jcode | 200.9 ms | 192.0 ms–203.4 ms | 1.10× less | 23.0 MB | 1.10× less | skill-tool | yes | — |
| Claude Code | 1.01 s | 992.6 ms–1.03 s | 4.59× more | 240.4 MB | 9.52× more | skill-tool | yes | — |
| Codex CLI | 2.16 s | 2.12 s–2.79 s | 9.78× more | 254.7 MB | 10.09× more | file-read | yes | — |
| OpenCode | 8.08 s | 7.95 s–9.51 s | 36.66× more | 845.9 MB | 33.50× more | skill-tool | yes | — |
| pi | 745.0 ms | 728.5 ms–762.3 ms | 3.38× more | 97.8 MB | 3.87× more | file-read | yes | — |
| oh-my-pi (omp) | 3.99 s | 3.93 s–12.46 s | 18.08× more | 412.5 MB | 16.34× more | file-read | yes | — |
| oh-my-openagent (omo) | 21.20 s | 19.73 s–25.15 s | 96.13× more | 538.9 MB | 21.34× more | file-read | yes | — |

**MCP tool call** (process start to final answer, including starting the fixture stdio MCP server and one `bench_echo` call)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | MCP ready | Note |
|---|---|---|---|---|---|---|---|---|---|
| horon | 269.4 ms | 241.3 ms–278.3 ms | baseline | 37.4 MB | baseline | mcp-call | yes | 183.6 ms | — |
| jcode | 318.5 ms | 316.5 ms–329.7 ms | 1.18× more | 36.3 MB | 1.03× less | mcp-action | yes | 227.7 ms | — |
| Claude Code | 1.13 s | 1.07 s–1.36 s | 4.19× more | 273.0 MB | 7.30× more | mcp-tool | yes | 956.9 ms | — |
| Codex CLI | 862.3 ms | 832.7 ms–3.07 s | 3.20× more | 179.1 MB | 4.79× more | mcp-tool | yes | 648.4 ms | — |
| OpenCode | 8.53 s | 7.92 s–11.33 s | 31.66× more | 749.0 MB | 20.03× more | mcp-tool | yes | 6.37 s | — |
| pi | 812.6 ms | 803.0 ms–817.9 ms | 3.02× more | 118.0 MB | 3.16× more | mcp-tool | yes | 680.4 ms | — |
| oh-my-pi (omp) | 6.86 s | 4.69 s–10.30 s | 25.47× more | 415.5 MB | 11.11× more | mcp-route | yes | 6.68 s | — |
| oh-my-openagent (omo) | 23.31 s | 19.82 s–25.44 s | 86.51× more | 563.8 MB | 15.07× more | mcp-tool | yes | 20.03 s | — |

**Session memory** (idle after 3 turn(s) per session; sizes are process-tree sums, RSS; PSS also recorded on Linux). "Each additional" is the least-squares slope across the measured session counts; 0 B means growth stayed below RSS sampling noise. Threads and descriptors come from the daemon's own `server:memory` after the turns.

| Tool | First session | Each additional | vs horon (additional) | Threads/session | FDs/session | First (post-turn) | Each additional (post-turn) |
|---|---|---|---|---|---|---|---|
| horon | 22.6 MB | 142.4 KB | baseline | — | — | 29.9 MB | 431.1 KB |
| jcode | 22.1 MB | 72.3 KB | 1.97× less | — | — | 26.8 MB | 340.2 KB |
| Claude Code | 153.8 MB | 155.1 MB | 1,115× more | — | — | 188.0 MB | 190.6 MB |
| Codex CLI | 124.7 MB | 97.3 MB | 699.27× more | — | — | 166.6 MB | 123.4 MB |
| OpenCode | 245.1 MB | 203.3 KB | 1.43× more | — | — | 430.0 MB | 6.6 MB |
| pi | 91.3 MB | 90.6 MB | 651.36× more | — | — | 101.7 MB | 100.8 MB |
| oh-my-pi (omp) | 300.4 MB | 75.5 MB | 542.84× more | — | — | 342.2 MB | 335.9 MB |
| oh-my-openagent (omo) | 145.1 MB | 128.4 MB | 923.23× more | — | — | 416.3 MB | 403.0 MB |

Threads/descriptors unavailable: horon (server:memory did not report thread_count, fd_count), jcode (server:memory did not report thread_count, fd_count)

**Post-turn tree RSS per session count**

| Tool | 1 session(s) | 5 session(s) | 10 session(s) |
|---|---|---|---|
| horon | 29.9 MB | 32.0 MB | 33.7 MB |
| jcode | 26.8 MB | 28.4 MB | 29.8 MB |
| Claude Code | 188.0 MB | 944.6 MB | 1.9 GB |
| Codex CLI | 166.6 MB | 657.9 MB | 1.2 GB |
| OpenCode | 430.0 MB | 546.2 MB | 495.6 MB |
| pi | 101.7 MB | 504.4 MB | 1009.1 MB |
| oh-my-pi (omp) | 342.2 MB | 1.7 GB | 3.3 GB |
| oh-my-openagent (omo) | 416.3 MB | 2.1 GB | 4.0 GB |

#### 기동

```text
Benchmarking: D:/a/agent/agent/bench-bin/horon-agent.exe
============================================================

--help (binary load):
  Min:    11.02 ms
  Max:    11.93 ms
  Mean:   11.40 ms
  Median: 11.41 ms
  Stdev:  0.31 ms

--version:
  Min:    10.83 ms
  Max:    17.51 ms
  Mean:   13.26 ms
  Median: 12.33 ms
  Stdev:  2.38 ms

Measuring isolated server startup (10 runs)...

Server ready (isolated socket connectable):
  Min:    21.96 ms
  Max:    27.32 ms
  Mean:   23.07 ms
  Median: 22.48 ms
  Stdev:  1.58 ms

Measuring harness bridge readiness (10 runs)...
  skipped: api-bridge is Unix-only, so Windows has no harness bridge to time

Bridge stage: api socket connectable: No successful runs

Bridge stage: hello_ok: No successful runs

Bridge stage: no-op reply: No successful runs

Bridge total (socket -> no-op): No successful runs

============================================================
Summary:
  Binary load median:      ~11.4 ms
  Server ready median:     ~22.5 ms
  Bridge total median:     n/a
```

#### 세션 로딩 (COLD)

```text
binary   D:\a\agent\agent\bench-bin\examples\loading_probe.exe (827bcf138ec2)
mode     cold
protocol COLD: every measured run gets its own freshly copied HOME; no derived cache from an earlier run exists, so each sample is a first load of the fixture.
fixtures C:\Users\RUNNER~1\AppData\Local\Temp\horon-loading-bench-1hmerzmu
note     --warmup 3 is ignored in cold mode: every sample gets a fresh HOME
fixture events=0 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    0.104/0.102    p95    0.159/0.121    delta  -0.002 ms  control-no-change
  skill_request_overlay p50    0.754/0.772    p95    0.803/0.876    delta  +0.018 ms  control-no-change
  mcp_config_load   p50    0.930/0.917    p95    1.010/0.989    delta  -0.014 ms  control-no-change
  attach_history    p50    0.002/0.002    p95    0.003/0.002    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.001/0.002    p95    0.002/0.002    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.001/0.001    p95    0.002/0.001    delta  +0.000 ms  control-drift
  session_save      p50   51.236/50.946   p95   98.876/141.963  delta  -0.290 ms  control-no-change
  total store_read_bytes p50 22946  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 22924..22960 (7 distinct values)
  SPREAD   b session_save store_read_bytes 22912..22964 (8 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50   15.254/14.873   p95   16.363/15.723   delta  -0.381 ms  control-no-change
  skill_request_overlay p50   40.643/41.786   p95   44.650/46.016   delta  +1.143 ms  control-no-change
  mcp_config_load   p50    1.027/1.091    p95    1.147/1.219    delta  +0.064 ms  control-no-change
  attach_history    p50    0.002/0.002    p95    0.004/0.005    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.002/0.002    p95    0.002/0.002    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.002/0.002    p95    0.002/0.002    delta  +0.000 ms  control-no-change
  session_save      p50   59.321/67.318   p95  118.517/185.040  delta  +7.997 ms  control-no-change
  total store_read_bytes p50 22934  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 22910..22964 (7 distinct values)
  SPREAD   b session_save store_read_bytes 22902..22960 (7 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.104/0.100    p95    0.140/0.136    delta  -0.004 ms  control-no-change
  skill_request_overlay p50    0.773/0.817    p95    0.797/0.873    delta  +0.044 ms  control-no-change
  mcp_config_load   p50    0.967/0.956    p95    1.008/1.007    delta  -0.011 ms  control-no-change
  attach_history    p50  453.241/437.956  p95  527.248/588.913  delta -15.285 ms  control-no-change
  resume_ready      p50  152.479/152.291  p95  162.966/206.899  delta  -0.188 ms  control-no-change
  provider_dispatch p50    1.477/1.541    p95    1.552/1.654    delta  +0.064 ms  control-no-change
  session_save      p50   66.156/47.397   p95  204.522/369.586  delta -18.759 ms  control-no-change
  total store_read_bytes p50 5311213  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 22931..22975 (7 distinct values)
  SPREAD   b attach_history store_read_bytes 4189149..4189153 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099101..1099105 (3 distinct values)
  SPREAD   b session_save store_read_bytes 22924..22975 (9 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50   15.692/15.874   p95   16.396/17.761   delta  +0.182 ms  control-no-change
  skill_request_overlay p50   39.359/39.558   p95   40.698/44.390   delta  +0.199 ms  control-no-change
  mcp_config_load   p50    1.062/1.079    p95    1.187/1.154    delta  +0.017 ms  control-no-change
  attach_history    p50  447.852/445.973  p95  529.773/459.235  delta  -1.879 ms  control-no-change
  resume_ready      p50  153.400/152.909  p95  185.069/168.067  delta  -0.491 ms  control-no-change
  provider_dispatch p50    1.468/1.496    p95    1.521/1.960    delta  +0.027 ms  control-no-change
  session_save      p50   53.556/53.292   p95  169.546/164.440  delta  -0.264 ms  control-no-change
  total store_read_bytes p50 5311208  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189151..4189155 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099102..1099106 (3 distinct values)
  SPREAD   a session_save store_read_bytes 22924..22968 (9 distinct values)
  SPREAD   b attach_history store_read_bytes 4189153..4189155 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099105..1099106 (2 distinct values)
  SPREAD   b session_save store_read_bytes 22897..22968 (9 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.102/0.101    p95    0.134/0.134    delta  -0.001 ms  control-no-change
  skill_request_overlay p50    0.784/0.808    p95    0.920/0.910    delta  +0.024 ms  control-no-change
  mcp_config_load   p50    0.977/0.963    p95    1.041/1.031    delta  -0.014 ms  control-no-change
  attach_history    p50 5041.514/5025.402 p95 5312.070/5074.502 delta -16.113 ms  control-no-change
  resume_ready      p50 1615.401/1609.245 p95 1638.766/1652.737 delta  -6.156 ms  control-no-change
  provider_dispatch p50   18.888/18.306   p95   24.594/19.543   delta  -0.583 ms  control-no-change
  session_save      p50   47.633/49.026   p95  111.106/146.654  delta  +1.393 ms  control-no-change
  total store_read_bytes p50 53417423  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184249..42184251 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210214..11210215 (2 distinct values)
  SPREAD   a session_save store_read_bytes 22917..22978 (9 distinct values)
  SPREAD   b attach_history store_read_bytes 42184247..42184251 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210211..11210215 (3 distinct values)
  SPREAD   b session_save store_read_bytes 22942..22970 (7 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50   15.255/15.315   p95   19.842/16.049   delta  +0.060 ms  control-no-change
  skill_request_overlay p50   39.175/39.900   p95   43.243/43.424   delta  +0.725 ms  control-no-change
  mcp_config_load   p50    1.081/1.051    p95    1.251/1.152    delta  -0.030 ms  control-no-change
  attach_history    p50 4942.467/4939.104 p95 5519.570/5031.975 delta  -3.364 ms  control-no-change
  resume_ready      p50 1578.372/1579.360 p95 1760.529/1619.367 delta  +0.987 ms  control-no-change
  provider_dispatch p50   18.014/18.393   p95   20.392/19.478   delta  +0.379 ms  control-no-change
  session_save      p50   48.911/53.030   p95  140.878/324.404  delta  +4.119 ms  control-no-change
  total store_read_bytes p50 53417428  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184249..42184253 (4 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210212..11210216 (3 distinct values)
  SPREAD   a session_save store_read_bytes 22919..22983 (8 distinct values)
  SPREAD   b attach_history store_read_bytes 42184249..42184253 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210212..11210216 (3 distinct values)
  SPREAD   b session_save store_read_bytes 22938..22979 (9 distinct values)
report   bench-results\loading-cold.json
mode cold, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### 세션 로딩 (WARM)

```text
binary   D:\a\agent\agent\bench-bin\examples\loading_probe.exe (827bcf138ec2)
mode     warm
protocol WARM: one HOME per side, --warmup discarded runs per side, then measured runs alternate between the two sides on those same HOMEs.
fixtures C:\Users\RUNNER~1\AppData\Local\Temp\horon-loading-bench-f02gizk1
fixture events=0 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    0.092/0.093    p95    0.105/0.099    delta  +0.001 ms  control-no-change
  skill_request_overlay p50    0.693/0.750    p95    0.748/0.955    delta  +0.057 ms  control-drift
  mcp_config_load   p50    0.907/0.932    p95    0.977/1.038    delta  +0.025 ms  control-no-change
  attach_history    p50    0.002/0.002    p95    0.002/0.002    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.002/0.002    p95    0.002/0.002    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.002/0.002    p95    0.002/0.002    delta  +0.000 ms  control-no-change
  session_save      p50   49.878/48.319   p95   63.239/50.253   delta  -1.559 ms  control-no-change
  total store_read_bytes p50 23630  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [22910, 22918, 22934, 22957, 23610, 23651, 23657, 24326, 24338, 24339]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [22913, 22943, 22949, 22957, 23636, 23650, 23654, 24337, 24341, 24355]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 22910..24339 (10 distinct values)
  SPREAD   b session_save store_read_bytes 22913..24355 (10 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50   14.309/14.397   p95   15.623/16.991   delta  +0.088 ms  control-no-change
  skill_request_overlay p50   38.975/38.086   p95   45.551/45.814   delta  -0.889 ms  control-no-change
  mcp_config_load   p50    0.946/0.990    p95    1.031/1.125    delta  +0.044 ms  control-no-change
  attach_history    p50    0.002/0.003    p95    0.006/0.006    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.002/0.002    p95    0.002/0.002    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.002/0.002    p95    0.002/0.002    delta  +0.000 ms  control-no-change
  session_save      p50   63.569/63.426   p95  367.648/169.003  delta  -0.143 ms  control-no-change
  total store_read_bytes p50 23636  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [22896, 22933, 22954, 22958, 23625, 23648, 23653, 24330, 24348, 24356]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [22942, 22951, 22956, 22957, 23603, 23619, 23645, 23656, 24326, 24344]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 22896..24356 (10 distinct values)
  SPREAD   b session_save store_read_bytes 22942..24344 (10 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.093/0.103    p95    0.097/0.161    delta  +0.010 ms  control-no-change
  skill_request_overlay p50    0.705/0.752    p95    1.055/0.801    delta  +0.047 ms  control-no-change
  mcp_config_load   p50    0.918/0.919    p95    1.069/1.169    delta  +0.002 ms  control-no-change
  attach_history    p50  207.111/206.069  p95  222.231/213.914  delta  -1.042 ms  control-no-change
  resume_ready      p50  152.962/151.390  p95  160.300/155.432  delta  -1.572 ms  control-no-change
  provider_dispatch p50    1.773/1.709    p95    2.224/1.737    delta  -0.065 ms  control-no-change
  session_save      p50   49.867/50.591   p95   71.553/51.891   delta  +0.724 ms  control-no-change
  total store_read_bytes p50 2841133  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23637, 23663, 24352, 24366, 25045, 25061, 25740, 25768, 26463]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23627, 23663, 24336, 24354, 25002, 25049, 25740, 25753, 26408, 26447]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23637..26463 (9 distinct values)
  SPREAD   b session_save store_read_bytes 23627..26447 (10 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50   14.673/14.508   p95   16.279/16.874   delta  -0.164 ms  control-no-change
  skill_request_overlay p50   38.442/39.028   p95   41.874/45.670   delta  +0.586 ms  control-no-change
  mcp_config_load   p50    0.994/0.994    p95    1.034/1.125    delta  +0.000 ms  control-no-change
  attach_history    p50  208.359/207.404  p95  230.213/220.248  delta  -0.955 ms  control-no-change
  resume_ready      p50  149.262/155.367  p95  160.246/162.586  delta  +6.105 ms  control-no-change
  provider_dispatch p50    1.705/1.661    p95    2.131/1.811    delta  -0.044 ms  control-no-change
  session_save      p50   54.887/54.988   p95   61.014/63.594   delta  +0.101 ms  control-no-change
  total store_read_bytes p50 2841127  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23599, 23667, 24356, 24358, 25017, 25077, 25752, 25772, 26450, 26451]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23651, 23667, 24338, 24358, 25020, 25058, 25761, 26448, 26464]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23599..26451 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23651..26464 (9 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.092/0.092    p95    0.199/0.131    delta  -0.001 ms  control-no-change
  skill_request_overlay p50    0.751/0.738    p95    0.813/0.792    delta  -0.013 ms  control-no-change
  mcp_config_load   p50    0.865/0.939    p95    1.141/1.030    delta  +0.074 ms  control-no-change
  attach_history    p50 2181.302/2169.503 p95 2600.420/2294.437 delta -11.798 ms  control-no-change
  resume_ready      p50 1599.370/1573.023 p95 1697.621/1709.557 delta -26.347 ms  control-no-change
  provider_dispatch p50   17.811/17.905   p95   23.589/20.202   delta  +0.094 ms  control-no-change
  session_save      p50   49.161/51.651   p95  214.742/229.047  delta  +2.490 ms  control-no-change
  total store_read_bytes p50 28640144  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23589, 23641, 24342, 24364, 25037, 25058, 25721, 25762, 26429, 26438]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23656, 23674, 24345, 24346, 25038, 25060, 25745, 25767, 26408, 26410]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23589..26438 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23656..26410 (10 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50   14.130/14.361   p95   16.414/18.365   delta  +0.231 ms  control-no-change
  skill_request_overlay p50   38.170/38.391   p95   41.345/41.754   delta  +0.221 ms  control-no-change
  mcp_config_load   p50    1.008/1.007    p95    1.081/1.091    delta  -0.001 ms  control-no-change
  attach_history    p50 2169.130/2161.407 p95 2210.605/2176.481 delta  -7.723 ms  control-no-change
  resume_ready      p50 1566.955/1564.135 p95 1598.768/1587.800 delta  -2.819 ms  control-no-change
  provider_dispatch p50   17.417/17.517   p95   18.630/24.040   delta  +0.100 ms  control-no-change
  session_save      p50   48.679/49.755   p95   51.789/68.989   delta  +1.076 ms  control-no-change
  total store_read_bytes p50 28640144  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23629, 23637, 24339, 24372, 25046, 25051, 25729, 25764, 26463, 26468]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23642, 23686, 24361, 24373, 25035, 25060, 25723, 25739, 26410, 26454]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23629..26468 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23642..26454 (10 distinct values)
report   bench-results\loading-warm.json
mode warm, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### Tool latency

| Tool case | median | p95 | min | max | samples |
|---|---|---|---|---|---|
| read (tiny file) | 2.7 ms | 2.8 ms | 2.7 ms | 2.8 ms | 5 |
| read (1000 lines) | 2.7 ms | 2.8 ms | 2.4 ms | 2.8 ms | 5 |
| read (10 lines @ offset) | 2.7 ms | 2.7 ms | 2.2 ms | 2.7 ms | 5 |
| read (repo Cargo.toml) | 2.7 ms | 2.8 ms | 2.7 ms | 2.8 ms | 5 |
| write (small) | 13.0 ms | 33.1 ms | 10.1 ms | 33.1 ms | 5 |
| write (10KB) | 12.5 ms | 20.2 ms | 10.4 ms | 20.2 ms | 5 |
| edit (simple replace) | 10.4 ms | 15.0 ms | 9.9 ms | 15.0 ms | 5 |
| ls (scripts dir) | 2.7 ms | 2.7 ms | 2.6 ms | 2.7 ms | 5 |
| agentgrep (grep in tree) | 5.2 ms | 5.3 ms | 4.7 ms | 5.3 ms | 5 |
| agentgrep (find **/*.rs) | 5.2 ms | 5.2 ms | 4.7 ms | 5.2 ms | 5 |
| bash (true) | 7.8 ms | 7.8 ms | 7.3 ms | 7.8 ms | 5 |
| bash (echo hello) | 7.8 ms | 7.8 ms | 7.8 ms | 7.8 ms | 5 |
| bash (ls fixture tree) | 12.6 ms | 13.0 ms | 10.4 ms | 13.0 ms | 5 |
| invalid tool (dispatch only) | 2.6 ms | 2.7 ms | 2.2 ms | 2.7 ms | 5 |

#### Reload storm

1 resume(s) per level; each level resumes sessions no other level has opened, from a freshly started daemon.

| Concurrent resumes | history median | p95 | max | storm wall | failures |
|---|---|---|---|---|---|
| 1 | 274.6 ms | 274.6 ms | 274.6 ms | 274.8 ms | 0 |
| 10 | 1.11 s | 1.12 s | 1.12 s | 1.12 s | 0 |
| 40 | 2.00 s | 2.99 s | 3.24 s | 3.25 s | 0 |

#### Compile loop

| Checkpoint | Touched | median | min | max | samples |
|---|---|---|---|---|---|
| warm_noop | — | 717.1 ms | 717.1 ms | 717.1 ms | 1 |
| leaf_touch | src\main.rs | 969.2 ms | 969.2 ms | 969.2 ms | 1 |
| core_touch | crates\app-core\src\lib.rs | 30.64 s | 30.64 s | 30.64 s | 1 |
| build_meta_touch | crates\build-meta\src\lib.rs | 43.27 s | 43.27 s | 43.27 s | 1 |

## TUI (`horon-tui`)

대화형 클라이언트는 별도 바이너리이고 따로 잽니다. 이 수치는 `tui` 묶음에서 나오며 위의 헤드리스 표와 비교하지 않습니다. `first_frame_ms` 와 `direct_first_frame_ms` 는 서로 다른 두 프로그램을 각각 띄운 값이고, 핸드오프는 그 둘의 차이입니다.

![TUI (`horon-tui`)](charts/tui-01-latency.ko.svg)

![TUI (`horon-tui`)](charts/tui-02-memory.ko.svg)

### Linux

#### TUI (`horon-tui`)

Measured version: the interactive client `horon-tui`. The headless `horon-agent` numbers are in the suites above and are never compared with these: the two binaries are timed against themselves.

| Metric | Median | Min–max | Samples |
| --- | --- | --- | --- |
| First frame (bare `horon-agent`) | 48.7 ms | 46.3 ms–117.1 ms | 5 |
| First frame (direct `horon-tui`) | 42.8 ms | 42.3 ms–44.4 ms | 5 |
| TTY handoff cost | 5.9 ms | 1.9 ms–74.4 ms | 5 |
| Key press to echo (p95) | 0.2 ms | 0.2 ms–1.0 ms | 50 |
| Idle RSS after 5 s | 5.4 MB | 5.2 MB–5.4 MB | 5 |
| RSS after one turn | 5.5 MB | 5.4 MB–5.6 MB | 5 |
| Frame draw time (p50) | 1.9 ms | 1.9 ms–1.9 ms | 5 |
| Frame draw time (p95) | 2.0 ms | 1.9 ms–2.0 ms | 5 |
| Install size (`horon-tui`) | 5.6 MB | — | 1 |

Handoff: median first-frame time of a bare `horon-agent` launch (no arguments, on a PTY) minus the median first-frame time of a direct `horon-tui` launch against an already-running daemon, both on the same machine and build. The bare path also starts the daemon, so the number covers that startup, the sibling lookup and the `exec`; it is the extra cost of the handoff over launching the client itself.

Render bench: 200 frames on a 120×40 terminal.

### macOS

#### TUI (`horon-tui`)

Measured version: the interactive client `horon-tui`. The headless `horon-agent` numbers are in the suites above and are never compared with these: the two binaries are timed against themselves.

| Metric | Median | Min–max | Samples |
| --- | --- | --- | --- |
| First frame (bare `horon-agent`) | 80.1 ms | 58.2 ms–383.1 ms | 5 |
| First frame (direct `horon-tui`) | 72.3 ms | 53.8 ms–107.7 ms | 5 |
| TTY handoff cost | 11.3 ms | -35.0 ms–315.9 ms | 5 |
| Key press to echo (p95) | 0.2 ms | 0.2 ms–0.9 ms | 50 |
| Idle RSS after 5 s | 8.9 MB | 8.8 MB–8.9 MB | 5 |
| RSS after one turn | 9.0 MB | 9.0 MB–9.1 MB | 5 |
| Frame draw time (p50) | 1.4 ms | 1.2 ms–1.5 ms | 5 |
| Frame draw time (p95) | 3.0 ms | 2.7 ms–3.1 ms | 5 |
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
| Frame draw time (p50) | 3.1 ms | 2.9 ms–3.1 ms | 5 |
| Frame draw time (p95) | 3.3 ms | 3.2 ms–4.2 ms | 5 |
| Install size (`horon-tui`) | 4.9 MB | — | 1 |

Handoff: median first-frame time of a bare `horon-agent` launch (no arguments, on a PTY) minus the median first-frame time of a direct `horon-tui` launch against an already-running daemon, both on the same machine and build. The bare path also starts the daemon, so the number covers that startup, the sibling lookup and the `exec`; it is the extra cost of the handoff over launching the client itself.

Render bench: 200 frames on a 120×40 terminal.

Skipped: pty unavailable on Windows

<!-- bench-auto:end -->
