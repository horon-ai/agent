# 성능: 측정 결과와 그 한계

언어: [English](RESULTS.md) | **한국어**

이 문서는 `agent-runtime-performance-2` 계획이 기준점 B0(계획 시작 직전 커밋) 위에서 남긴 측정값을 기록한다. 이 계획은 `dev` 에 성능 커밋 19개를 올렸고, 같은 범위에 이 계획과 무관한 `fix(cli)` 커밋 하나가 더 있지만 아래 수치와는 관계가 없다. 측정 대상은 세션 저장소의 해시와 이력 페이지, SSE 파서, 복제 내보내기, 결과 캐시의 임계값 판정, 그리고 attach/detach 소크다. 측정은 모두 이 기기(Apple M1 Max, 디버그 테스트 프로필)에서 같은 기계의 앞뒤를 번갈아 재는 방식으로 이뤄졌다.

원본 산출물은 전부 `.omo/evidence/agent-runtime-performance-2/` 아래에 있고 그 경로는 `.gitignore:19` 로 무시된다. 계획은 주요 수치를 PR 본문에 옮겨 적으라고 했지만, 실제로는 대부분의 PR 본문이 한 줄이었다. 그래서 오늘 이 저장소를 클론한 사람은 이 작업의 측정값을 하나도 볼 수 없다. 이 페이지가 그 기록을 추적되는 자리로 옮긴 것이며, 모든 수치는 위 증거 파일에서 그대로 가져왔다.

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
| attach/detach 소크, 200 사이클 뒤 자원 | `RESOURCES` | fd 14, 스레드 25, 태스크 2,141 | fd 14, 스레드 22, 태스크 2,141 |

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
| 비활성 세션 축출 정책 | 채택 안 함 | 세션당 fd 0.999개, 스레드 0.998개, RSS 0.41 MB. 1개에서 1,000개까지 이 기울기가 꺾이지 않는다. debug job 수(`server:memory`가 내보내는 `job_count`)는 다섯 인구 수 모두에서 0이고, 소크에서 세션 수는 0으로 돌아온다 |
| `globalEvents` 다중화 | 채택 안 함 (프로토콜 변경) | 감시 세션당 소비자 쪽 fd 7.4개와 스레드 3.0개. 데몬 자신의 몫은 세션당 연결 1.0개이고, 브리지는 연결 하나에 세션 하나를 붙인다 |
| 세션 맵 잠금 범위 축소 | 채택 안 함 | 무관한 세션 제거가 import 전체를 기다린다. 1 / 20 / 60 / 150 이벤트에서 861 / 5,561 / 11,255 / 24,499 ms, 비율 1.00 |
| 인터럽트 폴 25 ms 타이머를 통지로 교체 | 채택 안 함 | 최악 29.7 ms(평균 27.6 ms)를 아끼려고, 감시 그룹 라운드당 wakeup 1회라는 비용 구조를 바꾸는 것이다 |
| `run.stopped` 저널 쓰기와 닫기 저장 겹치기 (#144, #129) | 채택 안 함 (더 느림) | 같은 잡에서 `dev`와 비교한 Windows 마지막 요청→종료 중앙값: 5회 39.8 → 56.4 ms(run 42759006826), 20회 35.2 → 42.8 ms(run 42764256757, IQR 33.0–40.9 대 40.7–45.5). Linux와 macOS는 동률이었다. 이후 #146이 닫기 저장의 flush 자체를 없애 같은 구간이 32.5 ms가 됐다 |
| Windows 고정 셸 환경을 프로세스당 한 번만 계산 (#124, #122) | 채택 안 함 (동률) | Windows 도구 왕복 구간, 20회: 74.3 → 73.5 ms(run 42735692366), min–max 범위 안이다. 왕복의 대부분은 호출마다 만드는 env 표나 PATH 탐색이 아니라 `cmd.exe` 기동 비용이다 |

숫자로 기각한 것은 실패가 아니다. 이 계획은 "개선이 안 나오면 그 항목은 '채택 안 함'으로 근거와 함께 끝냅니다. 그것도 성공입니다"를 성공의 정의로 삼았다. 위 표의 마지막 열이 그 근거다.

## 이 숫자들이 말하지 않는 것

이 절이 이 페이지의 목적이다. 아래 문장 중 하나라도 빠지면 이 페이지는 읽는 사람을 잘못된 결론으로 데려간다.

- **소스 해시의 개선은 해시 단계에만 해당하며, 저장 경로는 검증도 흘려서 한다.** `crates/base/src/session/event_commit.rs:120-133` 는 잡아 둔 접두부를 64 KiB 버퍼 하나로 해시하고, `:148-166` 은 예약된 꼬리를 같은 크기 버퍼로 비교한다. 그래서 큰 로그를 저장해도 로그 사본을 통째로 할당하지 않는다. 계획 당시 기록한 7 KB 로그에서 156,518 B, 553 KB 로그에서 8,293,580 B는 최고 할당이 로그 크기를 따르던 저장에서 나온 값이며 지금 이 경로를 설명하지 않는다.
- **이력 페이지의 개선은 머리가 바뀌지 않은 동안의 반복 페이징에만 해당한다.** 색인은 로그의 신원 스탬프(장치, inode, 길이, mtime, ctime)에 묶여 있어 append 한 번, import 한 번, reset 한 번으로 무효가 된다. 그래서 계속 자라는 세션은 예전 비용을 한 번 더 치르고 색인을 다시 만든다. 그 재구축 비용은 측정되지 않았다. 새 머리에서 페이지를 한 번만 받는 경우는 빨라지지 않는다.
- **캐시의 개선은 할당이지 처리량이 아니다.** 벽시계는 MiB당 약 19 ms 로 양쪽이 같다. 이스케이프 비용이 어느 쪽이든 지배한다.
- **소크는 fd, 스레드, 큐, 대기 요청, 태스크를 단언한다. RSS 는 아니다.** RSS 는 68.4 MB 에서 79.5 MB 로 오른 뒤 평평하게 머물렀고(최고 83.7 MB), 관찰값으로만 남았다.
- **200 사이클 소크는 20 사이클당 자원 1개의 누수를 판별한다.** 계획이 요구한 1,000 사이클은 이제 예약 워크플로 `Attach/Detach Soak`(`.github/workflows/soak.yml`)가 매주 실행하고 첫 실행이 깨끗했으며, 풀 리퀘스트용 형식은 같은 테스트를 구조 게이트에서 20 사이클로 돌리는 것이다(`scripts/perf/check_structural.py:136-146`).
- **이 계획이 끝났을 때 구조 게이트는 프레임 상한을 단언하지 않았다.** 그때 게이트가 읽던 카운터에 프레임 계수가 없었고, 프레임 상한은 `crates/harness-api-server/src/framing_tests.rs` 만 지켰다. 이제 게이트에 `frame` 구획이 있다(`scripts/perf/check_structural.py`). 브리지 리더와 SDK 리더가 각각 `max_frame_bytes`, `accepted_frame_bytes`, `rejected_frame_bytes` 를 보고하고, 게이트는 상한이 `frame_max_bytes`(64 MiB)와 같고 거부된 가장 작은 프레임이 수락된 가장 큰 프레임보다 정확히 1바이트 큰지 확인한다. 프레임 개수는 여전히 세지 않는다.
- **이 숫자들이 덮던 `shared_context` 맵은 이제 트리에 없다.** 토픽 채널 저장소와 함께 제거됐으므로(#299) 소크도 이 맵을 더 이상 표집하지 않고, 위 표에도 이 항목의 값이 없다. 과거 측정값은 기록대로 남긴다. 모든 인구 수에서 0이었고, 그 0은 "누수가 없다"가 아니라 "아무도 쓰지 않았다"는 뜻이었다. `server:memory` 페이로드에 남은 것은 그 옆의 swarm 블록이다. `member_count`, `status_counts`, `swarm_count`, `swarm_membership_count`, `plan_count`, `plan_item_count` 와 각 바이트 추정치이고(`crates/app-core/src/server/debug_server_state.rs`), 앞으로 소크가 표집할 대상은 이 블록이다.
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

소크는 200 사이클 기본형과 `#[ignore]` 된 1,000 사이클 변형이 있다. 1,000 사이클형은 예약 워크플로 `Attach/Detach Soak`(`.github/workflows/soak.yml`)가 매주 실행한다. 이 워크플로는 `CI Required` 에 들어가지 않으므로 자원 잔류 실패가 풀 리퀘스트를 막지 않고, 추적 이슈 하나를 만들거나 갱신한다.

```bash
HORON_AGENT_WORKSPACE_TRUST=trust \
  cargo test --locked -p horon-agent --test e2e soak_attach_detach::soak_attach_detach_returns_every_counter_to_baseline -- --exact --nocapture
HORON_AGENT_WORKSPACE_TRUST=trust \
  cargo test -p horon-agent --test e2e soak_attach_detach_1000_cycles -- --ignored --exact --nocapture
```

예약 실행은 직접 재현할 수 있다: `gh workflow run soak.yml --ref dev -f cycles=1000`. 리포트는 `soak-attach-detach-report` 아티팩트이고, `python3 scripts/perf/check_soak.py <캡처한 로그>` 가 로컬 실행에서 같은 마크다운을 만든다.

### 호스트 러너에서 잰 1,000 사이클 기준선

첫 실행은 워크플로가 병합된 뒤 `dev` 푸시가 띄운 것이다([실행 43600180217](https://github.com/horon-ai/agent/actions/runs/43600180217), 커밋 `af41e0b7`). `ubuntu-24.04`, `HORON_AGENT_PERF_COUNTERS=1` 환경이다.

| 측정 | 값 |
| --- | --- |
| 사이클 | 1,000, 100개씩 10개 창 |
| 사이클 소요 | 7분 38초(13:34:30Z → 13:42:08Z), 사이클당 약 0.46초 |
| 잡 전체 | 콜드 빌드를 포함해 10분 56초 |
| 판정 | `STRUCTURAL_PERF section=soak cycles=1000 fd_retention=0 thread_retention=0 non_returning=0` |
| 리포트 | **Clean.** 모든 카운터가 기준선으로 돌아왔다 |

| 카운터 | 기준선 → 최종 | 100 사이클당 기울기 |
| --- | --- | --- |
| `fd_count` | 16 → 16 | +0.000 |
| `thread_count` | 16 → 16 | +0.000 |
| `queue_depth_high_water` | 0 → 1 | +0.100 |
| `pending_requests`, `interrupt_poll_wakeups`, `background_tasks`, `live_sessions`, `connected_clients` | 0 → 0 | +0.000 |

`queue_depth_high_water` 는 최종값이 아니라 최고 수위다. 어느 시점에든 관측된 가장 깊은 큐를 기록하므로 순간적인 1은 잔류가 아니다. 잡 타임아웃 180분은 측정된 실행보다 한 자릿수 이상 여유가 있다.

구조 게이트와 A/B 실행기의 사용법, 리포트 템플릿의 필수 키, 경계를 올리는 절차는 [`docs/performance/BENCHMARK.ko.md`](BENCHMARK.ko.md) 에 있다. 게이트 명령은 `python3 scripts/perf/check_structural.py` 이고, A/B 는 `scripts/perf/ab_perf.py` 다.

## 상태

계획의 항목은 17개이고, 14개가 완료, 3개가 부분 완료(`[~]`)다. 부분 완료의 뜻은 다음과 같다.

| 항목 | 상태 | 뜻 |
| --- | --- | --- |
| 7. 두 SDK 의 NDJSON 프레이밍 상한 | `[~]` | 코드와 테스트는 `dev` 에 있고 검증됐다. 이 항목이 지목한 증거 파일이 워크트리와 함께 사라졌고 재구성하지 않았다. 동작은 증명됐고 증거 궤적은 아니다 |
| 12. 보조·결과 캐시 바이트 회계 | `[~]` | 코드는 `dev` 에 있고, 결정적인 줄은 리드가 직접 재실행해 재현했다. 레인의 원본 증거 파일은 사라졌고, 지금 그 자리에 있는 파일은 스스로 `recovered` 라고 밝힌 재구성본이다 |
| 15. attach/detach 소크 | `[x]` | 계획 항목인 1,000 사이클이 이제 돈다. `.github/workflows/soak.yml` 이 매주·수동으로 실행하고, 첫 실행이 1,000 사이클을 7분 38초에 깨끗하게 마쳤다(`fd_retention=0`, `thread_retention=0`, `non_returning=0`, 실행 43600180217). 200 사이클 형은 풀 리퀘스트 게이트로 남되, 구조 게이트가 20 사이클로 돌린다(`scripts/perf/check_structural.py:136-146`) |
| 17. CI 구조 게이트와 리포트 템플릿 | `[~]` | 게이트는 실제로 돈다(경계 42개 `scripts/perf/structural_budget.json`, `EXIT=0`, 위반 시 `EXIT=1`, 측정 누락 시 `EXIT=2`, 계약 테스트 84개 `scripts/perf/test_check_structural.py`). 계획의 두 조건이 남았다. 프레임 계수 카운터가 없고, "B0 에서 통과"는 구조적으로 불가능하다(`STRUCTURAL_PERF` 줄을 인쇄하는 코드가 B0 뒤에 들어왔다). 이제 게이트는 `frame` 구획에서 두 리더의 프레임 상한 자체를 단언하고, 프레임 개수는 여전히 읽지 않는다 |

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

one-shot은 `run` 질문 한 건(기동, `ls` 도구 왕복 1회, 최종 응답)의 중앙값입니다. start→first request는 api-bridge 소켓 연결부터 첫 `list_sessions` 응답까지입니다. fsync는 one-shot 한 건이 부른 fsync·fdatasync 호출 수입니다. 메모리는 데몬 프로세스 트리의 idle RSS입니다.

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

## `perf-bench-harness-prompt` 계획의 측정

이 계획은 한 턴이 치르는 요청 바이트와 프롬프트 표면을 재고, 줄이고, 구조 게이트로 묶었습니다(#324). 동작도 하나 바뀝니다. 세션 도중 `.horon/prompt-overlay.md`, `preferred-tools.md`, `system-prompt.md`를 고치면 이제 다음 세션 경계(새 세션, `/clear`, 작업 폴더 변경, 복원)에서 반영됩니다. AGENTS.md가 이미 따르던 규칙과 같습니다.

### 측정 조건

- 3개 OS A/B: [run 43667470921](https://github.com/horon-ai/agent/actions/runs/43667470921), 후보 `f5993e9d`, 같은 잡에서 `dev` `5285d2a4`로 만든 기준선 `horon-base`. `suites=compare`, `oneshot_runs=5`, 중앙값과 min–max입니다. min–max가 겹치는 차이는 동률입니다.
- `compare,startup,tools`를 묶은 Linux A/B: [run 43670649131](https://github.com/horon-ai/agent/actions/runs/43670649131).
- macOS `compare` 재측정: [run 43672354272](https://github.com/horon-ai/agent/actions/runs/43672354272). 첫 macOS `--version` 표본이 겹치지 않아 다시 쟀습니다.
- 바이트와 카운터 값: 구조 게이트 [run 43666931419](https://github.com/horon-ai/agent/actions/runs/43666931419)와 ci.yml [run 43662099986](https://github.com/horon-ai/agent/actions/runs/43662099986)이며 ubuntu와 macOS가 같습니다. 전 값은 같은 브랜치 Wave 1(`308f6c5e`, 이 표면들에서 `dev`와 같음)에서 쟀습니다.

### 프롬프트 표면과 세션 컨텍스트

| 표면 | 전 | 후 |
| --- | --- | --- |
| 즉시 보내는 core 도구 카탈로그(스키마 8개) | 9,714 B | 9,151 B |
| core one-shot 첫 요청의 `tools`(모의 서버) | 9,818 B | 9,255 B |
| 정적 프롬프트(core, 프롬프트 파일 없음) | 1,367 B, 319 토큰 | 1,322 B, 305 토큰 |
| `# Additional tools` 색인 | 629 B, 문장 중간에서 잘린 행 5개 | 585 B, 1개(`todo`) |
| 바뀌지 않은 요청 9번의 프롬프트 원천 읽기 시도 | 54 | 0 |
| 세션 컨텍스트 `git` 프로세스, 저장소 밖 / 작업 트리 | 1 / 3 | 0 / 1 |
| PCI 표시 장치가 없을 때 GPU 탐지 프로세스 | 1 | 0 |

PR #332에서 도구 목록 상한을 9,151 B로 낮췄습니다. `1dfdd06a`에서 도구 8개에 공통으로 들어가는 `intent` 설명을 줄인 뒤 실측값인 8,975 B를 새 상한으로 정했습니다. 정적 프롬프트는 1,322 B였고 내용이 같은 요청 9회에서 프롬프트 원본을 읽은 횟수는 0회였습니다. 같은 Linux 러너에서 첫 요청의 `tools` 필드는 `83da1c39`의 9,255 B에서 9,079 B로 줄었습니다. 도구 목록은 목표인 9,000 B보다 25 B 작고 첫 요청은 최초 기준인 9,818 B보다 739 B 작습니다. [구조 측정 결과](https://github.com/horon-ai/agent/actions/runs/44190404248/job/134418534689) · [모의 서버가 받은 첫 요청](https://github.com/horon-ai/agent/actions/runs/44190400494/artifacts/14378517143)

### 시간과 메모리 (중앙값 ms, 괄호는 min–max)

| OS | 시나리오 | `horon-base` (dev) | 후보 |
| --- | --- | --- | --- |
| Linux | one-shot | 40.24 (39.92–179.35) | 29.35 (28.39–44.31) |
| Linux | core one-shot | 42.92 (41.29–49.03) | 31.31 (30.72–45.41) |
| Linux | skill 호출 | 37.43 (36.77–38.08) | 26.31 (25.22–26.45) |
| Linux | MCP 도구 호출 | 55.62 (50.58–57.65) | 40.16 (39.54–44.96) |
| macOS | one-shot | 54.93 (53.10–495.17) | 47.07 (44.89–265.97) |
| macOS | core one-shot | 60.63 (55.86–88.40) | 49.28 (48.89–127.21) |
| macOS | skill 호출 | 108.27 (85.84–128.53) | 62.82 (51.71–87.13) |
| macOS | MCP 도구 호출 | 82.51 (80.09–191.23) | 76.41 (73.10–206.67) |
| Windows | one-shot | 170.15 (158.82–229.05) | 188.26 (142.17–370.69) |
| Windows | core one-shot | 200.21 (171.71–436.16) | 179.55 (149.18–427.49) |
| Windows | skill 호출 | 176.06 (158.48–180.42) | 145.75 (139.41–231.53) |
| Windows | MCP 도구 호출 | 302.51 (201.68–494.52) | 268.93 (172.28–412.46) |

- one-shot 최대 RSS는 Linux 46.6 → 39.4 MB, macOS 31.6 → 24.0 MB, Windows 33.9 → 33.6 MB입니다. Linux one-shot fsync는 양쪽 모두 10회 그대로입니다.
- Linux run 43670649131도 같은 방향입니다. one-shot 48.69 → 34.59 ms, skill 호출 43.12 → 30.72 ms, MCP 도구 호출 78.07 → 49.12 ms, 최대 RSS 40.0 → 38.3 MB, fsync 10과 10입니다.
- min–max가 겹치지 않게 느려진 시나리오는 없습니다. Windows one-shot(170.15 → 188.26)과 run 43672354272의 macOS MCP 도구 호출(101.94 → 107.63)은 중앙값이 나빠졌지만 범위가 겹치므로 동률입니다.
- `--version`: 첫 macOS 표본은 7.36 (6.99–7.50) → 7.89 (7.79–8.18) ms로 겹치지 않았습니다. 재측정은 9.27 (8.96–10.48) → 8.29 (7.99–8.43) ms로 반대 방향으로 겹치지 않았고, Linux와 Windows는 두 실행 모두 겹칩니다. 이 브랜치는 `--version` 경로의 코드를 바꾸지 않았으므로 첫 표본은 러너 잡음입니다.
- startup·tools 묶음은 같은 잡 기준선이 없어 보고만 합니다. `serve` 소켓 준비 20.39 ms(dev 기준 run 43612646876에서 20.57 ms), 브리지 no-op 응답 21.59 ms(21.89 ms)입니다.

### 이 숫자가 말하지 않는 것

- 시간 단축의 원인은 나누지 않았습니다. 세션 컨텍스트의 `git` 프로세스 감소와 요청당 프롬프트 작업 감소가 모두 측정 경로에 있지만, 그 차이를 둘로 나눈 실행은 없습니다.
- `[features] foreign_rules`(기본값 꺼짐)는 여전히 요청마다 규칙 파일을 읽고, 프롬프트 원천 읽기 수에 잡히지 않습니다.
- Windows 벤치 빌드는 기준선과 후보 모두에서 같은 `linker stdout` 줄 두 개를 출력합니다. 이 변경이 아니라 벤치의 Windows release 링크에서 나오며, 이 브랜치의 ci.yml 실행에는 경고가 없습니다.

## 자동 측정: `main` 최신 측정 커밋

`main` 에 올라간 커밋이 CI 와 구조 성능 게이트를 모두 통과하고 직전 측정 이후 에이전트 코드(`src`, `crates`, `Cargo.toml`, `Cargo.lock`, `.cargo`, `rust-toolchain.toml`, `scripts/perf`)가 바뀌었으면, `.github/workflows/benchmark-docs.yml` 이 Linux·macOS·Windows에서 전체 벤치마크 묶음을 OS마다 따로 돌리고 아래 구간을 `main` 과 `dev` 에서 함께 다시 씁니다. OS마다 절이 하나씩 있고, 한 번의 실행이 이 파일과 `RESULTS.ko.md`를 함께 쓰므로 영어와 한국어 구간은 늘 같은 커밋을 가리킵니다. `suites=all`은 모든 정규 묶음을 뜻하고 `install`은 넣지 않으므로 아래 어느 줄도 설치 시간이 아닙니다. 설치 시간은 설치 묶음을 이름으로 지정한 실행에서 나오고, 그 묶음은 측정 중인 커밋이 아니라 고정한 배포 릴리스를 잽니다. 측정 방법과 묶음별 뜻은 [`BENCHMARK.ko.md`](BENCHMARK.ko.md) 의 "자동 측정"에 있습니다.

<!-- bench-auto:start commit=2f63e444a5d4330d6f651189b54fe6e64eb84866 -->

## 헤드리스 (`horon-agent`)

이 구간은 `.github/workflows/benchmark-docs.yml` 이 자동으로 다시 씁니다. 손으로 고치지 않습니다.

- 커밋: `2f63e444a5d4330d6f651189b54fe6e64eb84866`
- 측정 시각: 2026-11-19 14:51 UTC
- 실행: <https://github.com/horon-ai/agent/actions/runs/44537447832> (원본 JSON 은 OS별 `benchmark-<os>-<run_id>` 아티팩트)

OS마다 다른 GitHub 호스티드 러너에서 따로 잽니다. 기계가 다르므로 OS끼리 절대값을 비교하지 않고, 같은 OS 안의 상대 비교(horon 대비 배수)와 커밋 사이의 추세만 읽습니다.

| OS | 러너 | 바이너리 | 상태 |
| --- | --- | --- | --- |
| Linux | `ubuntu-24.04` | `horon-agent v0.1.0-dev (2f63e44)` | 성공 |
| macOS | `macos-latest` | `horon-agent v0.1.0-dev (2f63e44)` | 성공 |
| Windows | `windows-latest` | `horon-agent v0.1.0-dev (2f63e44)` | 성공 |

### Linux

#### Coding agent comparison (headless, mock model)

Host: linux/x86_64, 4 CPUs, Python 3.12.16. Baseline: **horon**. Samples per one-shot, skill and MCP cell: 5.

| Tool | Version | `--version` (median) | Install size |
|---|---|---|---|
| horon | horon-agent v0.1.0-dev (2f63e44) | 2.0 ms | 112.0 MB |
| jcode | jcode v0.99.1 (7206efd0c) | 6.8 ms | 163.4 MB |
| Claude Code | 2.1.317 (Claude Code) | 6.8 ms | 632.2 MB |
| Codex CLI | codex-cli 0.166.0 | 29.6 ms | 435.0 MB |
| OpenCode | 1.19.5 | 347.5 ms | 613.4 MB |
| pi | 1.5.0 | 169.2 ms | 109.0 MB |
| oh-my-pi (omp) | omp/18.18.0 | 526.7 ms | 413.9 MB |
| oh-my-openagent (omo) | omo 5.1.64 (engine: senpi 2026.11.5-1) | 89.6 ms | 520.7 MB |

**One-shot headless prompt** (includes startup, one `ls` tool call, final answer)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Tool round-trip | fsync calls | request bytes | Note |
|---|---|---|---|---|---|---|---|---|---|
| horon | 42.0 ms | 40.9 ms–219.1 ms | baseline | 46.5 MB | baseline | yes | 10 | 3,756 / 2,368 (1 tools) | — |
| jcode | 137.6 ms | 137.2 ms–138.7 ms | 3.27× more | 42.1 MB | 1.10× less | yes | 5 | 3,292 / 1,433 (1 tools) | — |
| Claude Code | 345.8 ms | 330.8 ms–349.6 ms | 8.22× more | 208.0 MB | 4.47× more | yes | — | 45,780 / 29,653 (12 tools) | — |
| Codex CLI | 354.8 ms | 348.9 ms–394.5 ms | 8.44× more | 203.1 MB | 4.37× more | yes | 3 | 39,432 / 17,271 (9 tools) | — |
| OpenCode | 2.06 s | 1.96 s–2.21 s | 48.96× more | 580.5 MB | 12.49× more | yes | 4 | 2,659 / 0 (0 tools) | — |
| pi | 281.4 ms | 280.2 ms–292.1 ms | 6.69× more | 114.4 MB | 2.46× more | yes | — | 5,502 / 2,841 (4 tools) | — |
| oh-my-pi (omp) | 1.32 s | 1.30 s–1.33 s | 31.30× more | 354.6 MB | 7.63× more | yes | 31 | 29,858 / 18,533 (11 tools) | — |
| oh-my-openagent (omo) | 4.33 s | 4.26 s–4.79 s | 103.03× more | 756.3 MB | 16.27× more | yes | 36 | 26,669 / 2,424 (2 tools) | — |

`request bytes` is the first model request of the first run as the mock server received it: JSON body bytes / tool catalog bytes (tools offered).

**Core-profile one-shot** (the one-shot turn without `--tools`, so the request carries the default core surface: the eager tools, `toolbox` and the listed-tool index)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | Note | request bytes |
|---|---|---|---|---|---|---|---|---|---|
| horon | 44.4 ms | 43.5 ms–44.9 ms | baseline | 43.1 MB | baseline | — | yes | — | 11,125 / 9,079 (8 tools) |
| jcode | — | — | — | — | — | — | — | skipped: jcode has no core tool profile | — |
| Claude Code | — | — | — | — | — | — | — | skipped: Claude Code has no core tool profile | — |
| Codex CLI | — | — | — | — | — | — | — | skipped: Codex CLI has no core tool profile | — |
| OpenCode | — | — | — | — | — | — | — | skipped: OpenCode has no core tool profile | — |
| pi | — | — | — | — | — | — | — | skipped: pi has no core tool profile | — |
| oh-my-pi (omp) | — | — | — | — | — | — | — | skipped: oh-my-pi (omp) has no core tool profile | — |
| oh-my-openagent (omo) | — | — | — | — | — | — | — | skipped: oh-my-openagent (omo) has no core tool profile | — |

**Skill invocation** (process start to final answer, including skill discovery and one load of the fixture skill)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | Note |
|---|---|---|---|---|---|---|---|---|
| horon | 38.0 ms | 37.6 ms–38.9 ms | baseline | 37.9 MB | baseline | skill-uri | yes | — |
| jcode | 45.8 ms | 45.1 ms–48.3 ms | 1.21× more | 41.2 MB | 1.09× more | skill-tool | yes | — |
| Claude Code | 332.6 ms | 324.4 ms–339.6 ms | 8.76× more | 202.2 MB | 5.34× more | skill-tool | yes | — |
| Codex CLI | 398.8 ms | 386.6 ms–415.7 ms | 10.51× more | 204.1 MB | 5.39× more | file-read | yes | — |
| OpenCode | 2.40 s | 2.36 s–2.43 s | 63.32× more | 615.4 MB | 16.25× more | skill-tool | yes | — |
| pi | 363.4 ms | 359.8 ms–365.4 ms | 9.58× more | 125.4 MB | 3.31× more | file-read | yes | — |
| oh-my-pi (omp) | 1.35 s | 1.34 s–1.37 s | 35.50× more | 344.0 MB | 9.08× more | file-read | yes | — |
| oh-my-openagent (omo) | 4.58 s | 4.54 s–4.61 s | 120.77× more | 692.5 MB | 18.28× more | file-read | yes | — |

**MCP tool call** (process start to final answer, including starting the fixture stdio MCP server and one `bench_echo` call)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | MCP ready | Note |
|---|---|---|---|---|---|---|---|---|---|
| horon | 57.3 ms | 56.4 ms–57.7 ms | baseline | 43.1 MB | baseline | mcp-call | yes | 47.2 ms | — |
| jcode | 160.3 ms | 159.2 ms–160.8 ms | 2.80× more | 52.0 MB | 1.21× more | mcp-action | yes | 139.1 ms | — |
| Claude Code | 419.3 ms | 409.4 ms–423.3 ms | 7.31× more | 224.1 MB | 5.20× more | mcp-tool | yes | 297.4 ms | — |
| Codex CLI | 343.2 ms | 342.9 ms–395.4 ms | 5.99× more | 197.0 MB | 4.58× more | mcp-tool | yes | 207.7 ms | — |
| OpenCode | 2.17 s | 2.12 s–2.20 s | 37.90× more | 566.3 MB | 13.15× more | mcp-tool | yes | 1.98 s | — |
| pi | 392.5 ms | 390.8 ms–404.2 ms | 6.85× more | 139.1 MB | 3.23× more | mcp-tool | yes | 354.2 ms | — |
| oh-my-pi (omp) | 1.34 s | 1.33 s–1.36 s | 23.43× more | 347.5 MB | 8.07× more | mcp-route | yes | 1.27 s | — |
| oh-my-openagent (omo) | — | — | — | — | — | mcp-tool | no | — | failed: the fixture sentinel never came back |

**Session memory** (idle after 3 turn(s) per session; sizes are process-tree sums, RSS; PSS also recorded on Linux). "Each additional" is the least-squares slope across the measured session counts; 0 B means growth stayed below RSS sampling noise. Threads and descriptors come from the daemon's own `server:memory` after the turns. Processes are counted in the same tree walk as the RSS sample (PSS shares it on Linux), so the count and the bytes describe one observation; a PID shared by two session roots counts once, a root that already exited is counted as neither bytes nor process, and a cell that could not be read is —, never 0.

| Tool | First session | Each additional | vs horon (additional) | Threads/session | FDs/session | Processes (idle) | Processes/session | First (post-turn) | Each additional (post-turn) |
|---|---|---|---|---|---|---|---|---|---|
| horon | 37.6 MB | 175.8 KB | baseline | 0.12 | 1.00 | 1 | 0.00 | 45.2 MB | 475.7 KB |
| jcode | 42.7 MB | 148.3 KB | 1.19× less | 0.00 | — | 1 | 0.00 | 46.4 MB | 272.3 KB |
| Claude Code | 152.6 MB | 149.6 MB | 871.23× more | — | — | 1 | 1.00 | 187.3 MB | 181.0 MB |
| Codex CLI | 128.9 MB | 132.4 MB | 771.06× more | — | — | 1 | 0.98 | 161.4 MB | 164.7 MB |
| OpenCode | 243.7 MB | 0.0 B | — | — | — | 1 | 0.00 | 453.9 MB | 10.7 MB |
| pi | 109.1 MB | 111.5 MB | 649.21× more | — | — | 1 | 1.00 | 131.4 MB | 130.7 MB |
| oh-my-pi (omp) | 377.2 MB | 127.8 MB | 743.99× more | — | — | 1 | 1.00 | 363.7 MB | 360.4 MB |
| oh-my-openagent (omo) | 326.1 MB | 60.0 MB | 349.31× more | — | — | 2 | 0.89 | 611.5 MB | 577.6 MB |

Threads/descriptors unavailable: jcode (server:memory did not report fd_count)

**Post-turn tree RSS per session count**

| Tool | 1 session(s) | 5 session(s) | 10 session(s) |
|---|---|---|---|
| horon | 45.2 MB | 47.0 MB | 49.4 MB |
| jcode | 46.4 MB | 47.4 MB | 48.8 MB |
| Claude Code | 187.3 MB | 920.3 MB | 1.8 GB |
| Codex CLI | 161.4 MB | 814.3 MB | 1.6 GB |
| OpenCode | 453.9 MB | 476.6 MB | 548.5 MB |
| pi | 131.4 MB | 647.6 MB | 1.3 GB |
| oh-my-pi (omp) | 363.7 MB | 1.8 GB | 3.5 GB |
| oh-my-openagent (omo) | 611.5 MB | 3.1 GB | 5.7 GB |

**Live processes per session count** (idle / post-turn)

| Tool | 1 session(s) | 5 session(s) | 10 session(s) |
|---|---|---|---|
| horon | 1 / 1 | 1 / 1 | 1 / 1 |
| jcode | 1 / 1 | 1 / 1 | 1 / 1 |
| Claude Code | 1 / 1 | 5 / 5 | 10 / 10 |
| Codex CLI | 1 / 1 | 8 / 5 | 10 / 10 |
| OpenCode | 1 / 1 | 1 / 1 | 1 / 1 |
| pi | 1 / 1 | 5 / 5 | 10 / 10 |
| oh-my-pi (omp) | 1 / 1 | 5 / 5 | 10 / 10 |
| oh-my-openagent (omo) | 2 / 4 | 5 / 20 | 10 / 41 |

#### 기동

```text
Benchmarking: /home/runner/work/agent/agent/bench-bin/horon-agent
============================================================

--help (binary load):
  Min:    1.86 ms
  Max:    2.48 ms
  Mean:   2.02 ms
  Median: 1.95 ms
  Stdev:  0.19 ms

--version:
  Min:    1.73 ms
  Max:    1.81 ms
  Mean:   1.77 ms
  Median: 1.77 ms
  Stdev:  0.03 ms

Measuring isolated server startup (10 runs)...

Server ready (isolated socket connectable):
  Min:    20.39 ms
  Max:    20.42 ms
  Mean:   20.40 ms
  Median: 20.40 ms
  Stdev:  0.01 ms

Server startup stages (one extra run with HORON_AGENT_STARTUP_PROFILE=1, not timed):
  stage             at ms  delta ms   share
  panic_hook          0.0       0.0    0.3%
  logging_init        0.1       0.1   14.9%
  nofile_limit        0.3       0.2   21.9%
  perm_harden         0.3       0.0    0.8%
  args_parse          0.3       0.0    0.1%
  config_loaded       0.4       0.1   12.6%
  server_new          0.7       0.3   42.1%
  socket_bound        0.7       0.1    7.4%

Measuring harness bridge readiness (10 runs)...

Bridge stage: api socket connectable:
  Min:    20.51 ms
  Max:    20.83 ms
  Mean:   20.71 ms
  Median: 20.73 ms
  Stdev:  0.10 ms

Bridge stage: hello_ok:
  Min:    20.92 ms
  Max:    21.30 ms
  Mean:   21.15 ms
  Median: 21.17 ms
  Stdev:  0.11 ms

Bridge stage: no-op reply:
  Min:    21.24 ms
  Max:    21.64 ms
  Mean:   21.47 ms
  Median: 21.50 ms
  Stdev:  0.13 ms

Bridge total (socket -> no-op):
  Min:    21.24 ms
  Max:    21.64 ms
  Mean:   21.47 ms
  Median: 21.50 ms
  Stdev:  0.13 ms

============================================================
Summary:
  Binary load median:      ~2.0 ms
  Server ready median:     ~20.4 ms
  Bridge total median:     ~21.5 ms
```

#### 세션 로딩 (COLD)

```text
binary   /home/runner/work/agent/agent/bench-bin/examples/loading_probe (9f742330ad52)
stages   /home/runner/work/agent/agent/bench-bin/examples/resume_stage_probe (59ecb999befb, 3 cold pass(es) per session fixture)
mode     cold
protocol COLD: every measured run gets its own freshly copied HOME; no derived cache from an earlier run exists, so each sample is a first load of the fixture.
fixtures /tmp/horon-loading-bench-f7uyiwju
note     --warmup 3 is ignored in cold mode: every sample gets a fresh HOME
fixture events=0 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    0.013/0.013    p95    0.014/0.015    delta  +0.000 ms  control-no-change
  skill_request_overlay p50    0.092/0.092    p95    0.096/0.097    delta  +0.001 ms  control-no-change
  mcp_config_load   p50    0.134/0.128    p95    0.158/0.154    delta  -0.006 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-drift
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  session_save      p50    6.913/6.910    p95  111.212/15.325   delta  -0.003 ms  control-no-change
  total store_read_bytes p50 23080  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition not_applicable: this fixture has no session log
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 23060..23088 (6 distinct values)
  SPREAD   b session_save store_read_bytes 23068..23088 (5 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    2.294/2.311    p95    2.338/2.380    delta  +0.017 ms  control-no-change
  skill_request_overlay p50    3.909/3.904    p95    3.950/3.986    delta  -0.005 ms  control-no-change
  mcp_config_load   p50    0.132/0.133    p95    0.138/0.134    delta  +0.001 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  session_save      p50   13.926/13.795   p95   62.680/68.180   delta  -0.131 ms  control-no-change
  total store_read_bytes p50 23076  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition not_applicable: this fixture has no session log
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 23072..23084 (4 distinct values)
  SPREAD   b session_save store_read_bytes 23064..23084 (4 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.013/0.013    p95    0.014/0.015    delta  -0.000 ms  control-no-change
  skill_request_overlay p50    0.094/0.095    p95    0.113/0.123    delta  +0.001 ms  control-no-change
  mcp_config_load   p50    0.127/0.128    p95    0.142/0.257    delta  +0.001 ms  control-no-change
  attach_history    p50  198.205/199.213  p95  214.594/209.145  delta  +1.007 ms  control-no-change
  resume_ready      p50   44.939/44.877   p95   46.135/45.785   delta  -0.062 ms  control-no-change
  provider_dispatch p50    1.120/1.129    p95    1.143/1.176    delta  +0.009 ms  control-no-change
  session_save      p50    6.579/6.581    p95  216.587/111.345  delta  +0.002 ms  control-no-change
  total store_read_bytes p50 5311401  full_source_hashes p50 2  skill_dir_reads p50 1
  decomposition 3/3 cold pass(es); anchor attach_history p50 198.205 ms (load 164.796 + history 33.248)
    selection          p50     0.028 ms  min     0.027  max     0.028  ratio 0.0% of anchor
    checkpoint_recent  p50   127.788 ms  min   127.687  max   128.063  ratio 64.5% of anchor
    log_read           p50     0.064 ms  min     0.060  max     0.070  ratio 0.0% of anchor
    full_scan          p50    16.927 ms  min    16.902  max    16.994  ratio 8.5% of anchor
    blob_read          p50     0.006 ms  min     0.003  max     0.034  ratio 0.0% of anchor
    graph_replay       p50    14.016 ms  min    13.251  max    14.185  ratio 7.1% of anchor
    metadata_json      p50     0.006 ms  min     0.005  max     0.007  ratio 0.0% of anchor
    messages_json      p50     0.800 ms  min     0.800  max     0.807  ratio 0.4% of anchor
    messages_decode    p50     0.752 ms  min     0.741  max     0.785  ratio 0.4% of anchor
    source_hash        p50     0.423 ms  min     0.417  max     0.424  ratio 0.2% of anchor
    history_rescan     p50    35.036 ms  min    34.800  max    35.154  ratio 17.7% of anchor
    page_index         p50    37.837 ms  min    37.787  max    39.184  ratio 19.1% of anchor
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189185..4189193 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099129..1099135 (2 distinct values)
  SPREAD   a session_save store_read_bytes 23063..23093 (7 distinct values)
  SPREAD   b attach_history store_read_bytes 4189189..4189193 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099131..1099135 (3 distinct values)
  SPREAD   b session_save store_read_bytes 23065..23093 (7 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    2.353/2.327    p95    2.494/2.433    delta  -0.026 ms  control-no-change
  skill_request_overlay p50    3.895/3.892    p95    3.920/4.059    delta  -0.003 ms  control-no-change
  mcp_config_load   p50    0.134/0.133    p95    0.142/0.138    delta  -0.001 ms  control-no-change
  attach_history    p50  202.504/201.068  p95  207.381/208.041  delta  -1.436 ms  control-no-change
  resume_ready      p50   45.233/45.228   p95   46.294/46.195   delta  -0.004 ms  control-no-change
  provider_dispatch p50    1.144/1.126    p95    1.178/1.183    delta  -0.018 ms  control-no-change
  session_save      p50    6.492/6.529    p95    9.171/11.276   delta  +0.037 ms  control-no-change
  total store_read_bytes p50 5311409  full_source_hashes p50 2  skill_dir_reads p50 1
  decomposition 3/3 cold pass(es); anchor attach_history p50 202.504 ms (load 168.007 + history 32.910)
    selection          p50     0.031 ms  min     0.030  max     0.031  ratio 0.0% of anchor
    checkpoint_recent  p50   131.628 ms  min   129.661  max   131.927  ratio 65.0% of anchor
    log_read           p50     0.070 ms  min     0.068  max     0.074  ratio 0.0% of anchor
    full_scan          p50    17.061 ms  min    16.970  max    17.131  ratio 8.4% of anchor
    blob_read          p50     0.004 ms  min     0.004  max     0.006  ratio 0.0% of anchor
    graph_replay       p50    13.237 ms  min    12.961  max    13.864  ratio 6.5% of anchor
    metadata_json      p50     0.004 ms  min     0.004  max     0.004  ratio 0.0% of anchor
    messages_json      p50     0.791 ms  min     0.790  max     0.803  ratio 0.4% of anchor
    messages_decode    p50     0.750 ms  min     0.747  max     0.751  ratio 0.4% of anchor
    source_hash        p50     0.423 ms  min     0.412  max     0.435  ratio 0.2% of anchor
    history_rescan     p50    34.532 ms  min    32.934  max    34.848  ratio 17.1% of anchor
    page_index         p50    38.779 ms  min    38.095  max    39.190  ratio 19.1% of anchor
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189191..4189195 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099132..1099136 (2 distinct values)
  SPREAD   a session_save store_read_bytes 23064..23090 (6 distinct values)
  SPREAD   b attach_history store_read_bytes 4189187..4189195 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099128..1099136 (2 distinct values)
  SPREAD   b session_save store_read_bytes 23058..23090 (7 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.013/0.013    p95    0.015/0.013    delta  +0.000 ms  control-no-change
  skill_request_overlay p50    0.096/0.096    p95    0.097/0.106    delta  +0.000 ms  control-no-change
  mcp_config_load   p50    0.130/0.129    p95    0.145/0.142    delta  -0.001 ms  control-no-change
  attach_history    p50 2604.853/2609.312 p95 2651.949/2649.356 delta  +4.458 ms  control-no-change
  resume_ready      p50  515.770/516.567  p95  557.375/538.656  delta  +0.797 ms  control-no-change
  provider_dispatch p50   13.669/13.750   p95   14.538/15.171   delta  +0.081 ms  control-no-change
  session_save      p50    7.159/7.083    p95   27.856/28.162   delta  -0.076 ms  control-no-change
  total store_read_bytes p50 53417618  full_source_hashes p50 2  skill_dir_reads p50 1
  decomposition 3/3 cold pass(es); anchor attach_history p50 2604.853 ms (load 2186.334 + history 422.007)
    selection          p50     0.043 ms  min     0.038  max     0.043  ratio 0.0% of anchor
    checkpoint_recent  p50  1720.952 ms  min  1714.412  max  1734.212  ratio 66.1% of anchor
    log_read           p50     0.601 ms  min     0.588  max     0.631  ratio 0.0% of anchor
    full_scan          p50   183.976 ms  min   183.973  max   188.310  ratio 7.1% of anchor
    blob_read          p50     0.067 ms  min     0.064  max     0.077  ratio 0.0% of anchor
    graph_replay       p50   193.208 ms  min   185.867  max   196.645  ratio 7.4% of anchor
    metadata_json      p50     0.006 ms  min     0.006  max     0.006  ratio 0.0% of anchor
    messages_json      p50     7.811 ms  min     7.722  max     8.170  ratio 0.3% of anchor
    messages_decode    p50    12.745 ms  min    12.478  max    13.512  ratio 0.5% of anchor
    source_hash        p50     3.905 ms  min     3.897  max     3.942  ratio 0.1% of anchor
    history_rescan     p50   427.252 ms  min   414.947  max   427.446  ratio 16.4% of anchor
    page_index         p50   445.084 ms  min   443.861  max   445.199  ratio 17.1% of anchor
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 23069..23093 (6 distinct values)
  SPREAD   b attach_history store_read_bytes 42184285..42184293 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210238..11210246 (3 distinct values)
  SPREAD   b session_save store_read_bytes 23065..23085 (5 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    2.381/2.423    p95    2.407/2.448    delta  +0.042 ms  control-no-change
  skill_request_overlay p50    3.879/3.877    p95    3.940/3.947    delta  -0.002 ms  control-no-change
  mcp_config_load   p50    0.135/0.136    p95    0.139/0.145    delta  +0.001 ms  control-no-change
  attach_history    p50 2641.810/2652.256 p95 2670.654/2735.872 delta +10.447 ms  control-no-change
  resume_ready      p50  537.080/542.155  p95  554.724/584.148  delta  +5.075 ms  control-no-change
  provider_dispatch p50   14.559/14.578   p95   15.421/15.044   delta  +0.019 ms  control-no-change
  session_save      p50    7.032/7.003    p95  116.212/32.110   delta  -0.029 ms  control-no-change
  total store_read_bytes p50 53417619  full_source_hashes p50 2  skill_dir_reads p50 1
  decomposition 3/3 cold pass(es); anchor attach_history p50 2641.810 ms (load 2205.688 + history 430.061)
    selection          p50     0.044 ms  min     0.040  max     0.044  ratio 0.0% of anchor
    checkpoint_recent  p50  1734.626 ms  min  1691.245  max  1759.992  ratio 65.7% of anchor
    log_read           p50     0.568 ms  min     0.567  max     0.583  ratio 0.0% of anchor
    full_scan          p50   184.911 ms  min   183.922  max   186.357  ratio 7.0% of anchor
    blob_read          p50     0.065 ms  min     0.065  max     0.067  ratio 0.0% of anchor
    graph_replay       p50   185.365 ms  min   179.195  max   188.129  ratio 7.0% of anchor
    metadata_json      p50     0.006 ms  min     0.006  max     0.008  ratio 0.0% of anchor
    messages_json      p50     7.912 ms  min     7.741  max     8.056  ratio 0.3% of anchor
    messages_decode    p50    12.340 ms  min    12.068  max    12.488  ratio 0.5% of anchor
    source_hash        p50     3.930 ms  min     3.930  max     3.932  ratio 0.1% of anchor
    history_rescan     p50   429.145 ms  min   421.180  max   459.345  ratio 16.2% of anchor
    page_index         p50   442.756 ms  min   428.137  max   448.310  ratio 16.8% of anchor
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184285..42184293 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210238..11210246 (3 distinct values)
  SPREAD   a session_save store_read_bytes 23065..23089 (7 distinct values)
  SPREAD   b attach_history store_read_bytes 42184289..42184293 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210242..11210246 (3 distinct values)
  SPREAD   b session_save store_read_bytes 23068..23089 (7 distinct values)
report   bench-results/loading-cold.json
mode cold, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### 세션 로딩 (WARM)

```text
binary   /home/runner/work/agent/agent/bench-bin/examples/loading_probe (9f742330ad52)
stages   /home/runner/work/agent/agent/bench-bin/examples/resume_stage_probe (59ecb999befb, 0 cold pass(es) per session fixture)
mode     warm
protocol WARM: one HOME per side, --warmup discarded runs per side, then measured runs alternate between the two sides on those same HOMEs.
fixtures /tmp/horon-loading-bench-s_gsf6i9
fixture events=0 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    0.011/0.011    p95    0.012/0.012    delta  -0.000 ms  control-no-change
  skill_request_overlay p50    0.085/0.085    p95    0.104/0.094    delta  +0.000 ms  control-no-change
  mcp_config_load   p50    0.113/0.113    p95    0.125/0.134    delta  +0.000 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  session_save      p50    5.866/6.014    p95   10.399/15.130   delta  +0.148 ms  control-no-change
  total store_read_bytes p50 23794  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition not_applicable: this fixture has no session log
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23073, 23081, 23784, 23804, 24503, 24507, 24515, 24523]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23069, 23077, 23081, 23792, 23796, 23800, 24515, 24519, 24523]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23073..24523 (8 distinct values)
  SPREAD   b session_save store_read_bytes 23069..24523 (9 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    2.015/2.036    p95    2.063/2.199    delta  +0.021 ms  control-no-change
  skill_request_overlay p50    3.408/3.470    p95    3.554/3.699    delta  +0.061 ms  control-no-change
  mcp_config_load   p50    0.112/0.115    p95    0.119/0.122    delta  +0.003 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.001/0.000    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  session_save      p50    7.515/6.812    p95   21.204/15.993   delta  -0.703 ms  control-no-change
  total store_read_bytes p50 23795  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition not_applicable: this fixture has no session log
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23063, 23073, 23081, 23792, 23794, 23796, 24493, 24495, 24515, 24531]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23073, 23075, 23077, 23780, 23792, 23806, 24511, 24513, 24515, 24517]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23063..24531 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23073..24517 (10 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.011/0.011    p95    0.012/0.012    delta  -0.000 ms  control-no-change
  skill_request_overlay p50    0.084/0.083    p95    0.094/0.101    delta  -0.001 ms  control-no-change
  mcp_config_load   p50    0.111/0.109    p95    0.123/0.122    delta  -0.001 ms  control-no-change
  attach_history    p50   60.552/60.601   p95   63.114/61.874   delta  +0.048 ms  control-no-change
  resume_ready      p50   34.069/34.099   p95   34.518/35.127   delta  +0.030 ms  control-no-change
  provider_dispatch p50    0.874/0.882    p95    0.905/0.891    delta  +0.007 ms  control-no-change
  session_save      p50    6.517/6.179    p95   29.400/22.153   delta  -0.338 ms  control-no-change
  total store_read_bytes p50 2841376  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition skipped: --stage-probe-samples is 0
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23797, 23805, 24500, 24528, 25235, 25237, 25960, 25966, 26661, 26671]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23797, 23801, 24508, 24528, 25228, 25243, 25950, 25952, 26673, 26675]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23797..26671 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23797..26675 (10 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    2.012/2.027    p95    2.068/2.142    delta  +0.015 ms  control-no-change
  skill_request_overlay p50    3.436/3.448    p95    3.550/3.794    delta  +0.013 ms  control-no-change
  mcp_config_load   p50    0.114/0.115    p95    0.119/0.134    delta  +0.000 ms  control-no-change
  attach_history    p50   61.448/63.092   p95   63.311/64.996   delta  +1.643 ms  control-no-change
  resume_ready      p50   34.695/34.651   p95   35.780/35.766   delta  -0.045 ms  control-no-change
  provider_dispatch p50    0.885/0.869    p95    0.897/0.923    delta  -0.015 ms  control-no-change
  session_save      p50    7.309/6.763    p95  179.266/22.215   delta  -0.546 ms  control-no-change
  total store_read_bytes p50 2841376  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition skipped: --stage-probe-samples is 0
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23792, 23804, 24503, 24515, 25232, 25244, 25951, 25955, 26666, 26682]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23796, 23798, 24505, 24519, 25230, 25234, 25236, 25939, 25943, 26662]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23792..26682 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23796..26662 (10 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.012/0.011    p95    0.014/0.012    delta  -0.001 ms  control-no-change
  skill_request_overlay p50    0.083/0.083    p95    0.097/0.086    delta  +0.000 ms  control-no-change
  mcp_config_load   p50    0.110/0.111    p95    0.120/0.118    delta  +0.000 ms  control-no-change
  attach_history    p50  729.539/741.517  p95  742.756/888.623  delta +11.978 ms  control-no-change
  resume_ready      p50  388.691/391.521  p95  401.724/416.744  delta  +2.830 ms  control-no-change
  provider_dispatch p50   10.398/10.234   p95   10.859/11.856   delta  -0.164 ms  control-no-change
  session_save      p50    6.239/6.104    p95   15.023/209.570  delta  -0.135 ms  control-no-change
  total store_read_bytes p50 28640390  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition skipped: --stage-probe-samples is 0
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23794, 23798, 24513, 24525, 25232, 25234, 25963, 25965, 26660, 26674]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23788, 23796, 24509, 24515, 25248, 25250, 25953, 25955, 26670, 26680]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23794..26674 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23788..26680 (10 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    2.062/1.989    p95    4.190/2.122    delta  -0.073 ms  control-no-change
  skill_request_overlay p50    3.490/3.422    p95    7.440/3.518    delta  -0.069 ms  control-no-change
  mcp_config_load   p50    0.117/0.116    p95    0.282/0.123    delta  -0.000 ms  control-no-change
  attach_history    p50  733.974/740.165  p95  785.286/767.367  delta  +6.190 ms  control-no-change
  resume_ready      p50  399.854/396.451  p95  450.727/428.241  delta  -3.403 ms  control-no-change
  provider_dispatch p50   10.249/10.232   p95   27.506/18.720   delta  -0.017 ms  control-no-change
  session_save      p50    7.126/6.949    p95    7.236/12.439   delta  -0.176 ms  control-no-change
  total store_read_bytes p50 28640397  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition skipped: --stage-probe-samples is 0
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23800, 23804, 24523, 24527, 25240, 25955, 25959, 26686]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23804, 23812, 24523, 24527, 25224, 25234, 25951, 25965, 26672, 26674]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23800..26686 (8 distinct values)
  SPREAD   b session_save store_read_bytes 23804..26674 (10 distinct values)
report   bench-results/loading-warm.json
mode warm, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### Tool latency

| Tool case | median | p95 | min | max | samples |
|---|---|---|---|---|---|
| read (tiny file) | 0.3 ms | 0.3 ms | 0.2 ms | 0.3 ms | 5 |
| read (1000 lines) | 0.4 ms | 0.5 ms | 0.4 ms | 0.5 ms | 5 |
| read (10 lines @ offset) | 0.4 ms | 0.5 ms | 0.3 ms | 0.5 ms | 5 |
| read (repo Cargo.toml) | 0.4 ms | 0.4 ms | 0.3 ms | 0.4 ms | 5 |
| write (small) | 1.8 ms | 2.0 ms | 1.7 ms | 2.0 ms | 5 |
| write (10KB) | 2.0 ms | 2.2 ms | 2.0 ms | 2.2 ms | 5 |
| edit (simple replace) | 1.8 ms | 1.9 ms | 1.8 ms | 1.9 ms | 5 |
| ls (scripts dir) | 1.0 ms | 1.2 ms | 0.9 ms | 1.2 ms | 5 |
| agentgrep (grep in tree) | 2.2 ms | 2.3 ms | 2.0 ms | 2.3 ms | 5 |
| agentgrep (find **/*.rs) | 1.8 ms | 1.9 ms | 1.4 ms | 1.9 ms | 5 |
| bash (true) | 1.9 ms | 2.0 ms | 1.9 ms | 2.0 ms | 5 |
| bash (echo hello) | 1.8 ms | 1.9 ms | 1.8 ms | 1.9 ms | 5 |
| bash (ls fixture tree) | 2.9 ms | 3.0 ms | 2.8 ms | 3.0 ms | 5 |
| invalid tool (dispatch only) | 0.1 ms | 0.1 ms | 0.1 ms | 0.1 ms | 5 |

#### Tool CPU per call

| Tool case | daemon cpu median | min | max | child cpu median | voluntary switches | involuntary switches | windows × calls | source |
|---|---|---|---|---|---|---|---|---|
| read (tiny file) | 0.242 ms | 0.241 ms | 0.252 ms | 0.000 ms | 5.64 | 0.00 | 5 × 50 | schedstat |
| read (1000 lines) | 0.267 ms | 0.252 ms | 0.334 ms | 0.000 ms | 5.50 | 0.00 | 5 × 50 | schedstat |
| read (10 lines @ offset) | 0.271 ms | 0.263 ms | 0.273 ms | 0.000 ms | 6.00 | 0.00 | 5 × 50 | schedstat |
| read (repo Cargo.toml) | 0.216 ms | 0.215 ms | 0.221 ms | 0.000 ms | 6.00 | 0.00 | 5 × 50 | schedstat |
| write (small) | 0.614 ms | 0.548 ms | 0.681 ms | 0.000 ms | 19.28 | 0.00 | 5 × 50 | schedstat |
| write (10KB) | 0.812 ms | 0.762 ms | 0.834 ms | 0.000 ms | 20.44 | 0.00 | 5 × 50 | schedstat |
| edit (simple replace) | 0.687 ms | 0.649 ms | 0.698 ms | 0.000 ms | 19.20 | 0.04 | 5 × 50 | schedstat |
| ls (scripts dir) | 0.646 ms | 0.627 ms | 0.682 ms | 0.000 ms | 5.78 | 0.00 | 5 × 50 | schedstat |
| agentgrep (grep in tree) | 1.049 ms | 1.041 ms | 1.062 ms | 0.000 ms | 10.34 | 0.18 | 5 × 50 | schedstat |
| agentgrep (find **/*.rs) | 1.217 ms | 1.161 ms | 1.235 ms | 0.000 ms | 6.00 | 0.00 | 5 × 50 | schedstat |
| bash (true) | 0.557 ms | 0.555 ms | 0.572 ms | 1.200 ms | 7.36 | 0.00 | 5 × 50 | schedstat |
| bash (echo hello) | 0.567 ms | 0.566 ms | 0.577 ms | 1.200 ms | 8.16 | 0.00 | 5 × 50 | schedstat |
| bash (ls fixture tree) | 0.640 ms | 0.639 ms | 0.646 ms | 2.400 ms | 8.66 | 0.00 | 5 × 50 | schedstat |
| invalid tool (dispatch only) | 0.043 ms | 0.042 ms | 0.043 ms | 0.000 ms | 2.42 | 0.00 | 5 × 50 | schedstat |

#### Reload storm

1 resume(s) per level; each level resumes sessions no other level has opened, from a freshly started daemon.

| Concurrent resumes | history median | p95 | max | storm wall | failures |
|---|---|---|---|---|---|
| 1 | 40.3 ms | 40.3 ms | 40.3 ms | 40.9 ms | 0 |
| 10 | 145.6 ms | 160.7 ms | 160.7 ms | 161.3 ms | 0 |
| 40 | 628.7 ms | 663.1 ms | 673.1 ms | 674.5 ms | 0 |

#### Compile loop

| Checkpoint | Touched | median | min | max | samples |
|---|---|---|---|---|---|
| warm_noop | — | 254.4 ms | 254.4 ms | 254.4 ms | 1 |
| leaf_touch | src/main.rs | 313.7 ms | 313.7 ms | 313.7 ms | 1 |
| core_touch | crates/app-core/src/lib.rs | 20.60 s | 20.60 s | 20.60 s | 1 |
| build_meta_touch | crates/build-meta/src/lib.rs | 28.97 s | 28.97 s | 28.97 s | 1 |

### macOS

#### Coding agent comparison (headless, mock model)

Host: darwin/arm64, 3 CPUs, Python 3.12.10. Baseline: **horon**. Samples per one-shot, skill and MCP cell: 5.

Skipped: syscalls (strace not available)

| Tool | Version | `--version` (median) | Install size |
|---|---|---|---|
| horon | horon-agent v0.1.0-dev (2f63e44) | 8.4 ms | 94.3 MB |
| jcode | jcode v0.99.1 (7206efd0c) | 10.0 ms | 136.3 MB |
| Claude Code | 2.1.317 (Claude Code) | 18.1 ms | 596.8 MB |
| Codex CLI | codex-cli 0.166.0 | 52.3 ms | 346.5 MB |
| OpenCode | 1.19.5 | 351.7 ms | 344.6 MB |
| pi | 1.5.0 | 195.1 ms | 109.0 MB |
| oh-my-pi (omp) | omp/18.18.0 | 508.6 ms | 274.3 MB |
| oh-my-openagent (omo) | omo 5.1.64 (engine: senpi 2026.11.5-1) | 128.5 ms | 502.3 MB |

**One-shot headless prompt** (includes startup, one `ls` tool call, final answer)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Tool round-trip | request bytes | Note |
|---|---|---|---|---|---|---|---|---|
| horon | 54.1 ms | 50.6 ms–416.9 ms | baseline | 27.2 MB | baseline | yes | 3,597 / 2,368 (1 tools) | — |
| jcode | 364.1 ms | 350.9 ms–587.6 ms | 6.72× more | 34.1 MB | 1.25× more | yes | 3,133 / 1,433 (1 tools) | — |
| Claude Code | 405.4 ms | 366.2 ms–719.1 ms | 7.49× more | 200.9 MB | 7.39× more | yes | 45,798 / 29,653 (12 tools) | — |
| Codex CLI | 318.1 ms | 279.3 ms–1.03 s | 5.88× more | 143.8 MB | 5.29× more | yes | 39,453 / 17,271 (9 tools) | — |
| OpenCode | 2.25 s | 1.73 s–3.84 s | 41.62× more | 786.7 MB | 28.94× more | yes | 2,659 / 0 (0 tools) | — |
| pi | 312.7 ms | 304.8 ms–350.6 ms | 5.78× more | 120.6 MB | 4.43× more | yes | 5,513 / 2,841 (4 tools) | — |
| oh-my-pi (omp) | 1.39 s | 1.22 s–1.58 s | 25.62× more | 405.3 MB | 14.91× more | yes | 29,853 / 18,533 (11 tools) | — |
| oh-my-openagent (omo) | 6.35 s | 5.86 s–8.42 s | 117.27× more | 903.9 MB | 33.25× more | yes | 27,493 / 2,424 (2 tools) | — |

`request bytes` is the first model request of the first run as the mock server received it: JSON body bytes / tool catalog bytes (tools offered).

**Core-profile one-shot** (the one-shot turn without `--tools`, so the request carries the default core surface: the eager tools, `toolbox` and the listed-tool index)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | Note | request bytes |
|---|---|---|---|---|---|---|---|---|---|
| horon | 55.1 ms | 52.1 ms–59.9 ms | baseline | 27.8 MB | baseline | — | yes | — | 10,966 / 9,079 (8 tools) |
| jcode | — | — | — | — | — | — | — | skipped: jcode has no core tool profile | — |
| Claude Code | — | — | — | — | — | — | — | skipped: Claude Code has no core tool profile | — |
| Codex CLI | — | — | — | — | — | — | — | skipped: Codex CLI has no core tool profile | — |
| OpenCode | — | — | — | — | — | — | — | skipped: OpenCode has no core tool profile | — |
| pi | — | — | — | — | — | — | — | skipped: pi has no core tool profile | — |
| oh-my-pi (omp) | — | — | — | — | — | — | — | skipped: oh-my-pi (omp) has no core tool profile | — |
| oh-my-openagent (omo) | — | — | — | — | — | — | — | skipped: oh-my-openagent (omo) has no core tool profile | — |

**Skill invocation** (process start to final answer, including skill discovery and one load of the fixture skill)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | Note |
|---|---|---|---|---|---|---|---|---|
| horon | 49.4 ms | 46.1 ms–56.7 ms | baseline | 23.2 MB | baseline | skill-uri | yes | — |
| jcode | 285.4 ms | 198.0 ms–385.7 ms | 5.78× more | 25.4 MB | 1.09× more | skill-tool | yes | — |
| Claude Code | 343.3 ms | 318.4 ms–352.9 ms | 6.95× more | 189.3 MB | 8.14× more | skill-tool | yes | — |
| Codex CLI | 338.0 ms | 316.5 ms–376.6 ms | 6.84× more | 143.1 MB | 6.15× more | file-read | yes | — |
| OpenCode | 3.58 s | 3.28 s–4.78 s | 72.49× more | 769.9 MB | 33.12× more | skill-tool | yes | — |
| pi | 411.0 ms | 403.0 ms–470.3 ms | 8.32× more | 132.3 MB | 5.69× more | file-read | yes | — |
| oh-my-pi (omp) | 1.49 s | 1.43 s–2.02 s | 30.17× more | 409.5 MB | 17.61× more | file-read | yes | — |
| oh-my-openagent (omo) | 7.29 s | 7.01 s–9.39 s | 147.59× more | 873.3 MB | 37.56× more | file-read | yes | — |

**MCP tool call** (process start to final answer, including starting the fixture stdio MCP server and one `bench_echo` call)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | MCP ready | Note |
|---|---|---|---|---|---|---|---|---|---|
| horon | 80.8 ms | 78.1 ms–100.0 ms | baseline | 19.5 MB | baseline | mcp-call | yes | 61.2 ms | — |
| jcode | 418.5 ms | 353.2 ms–491.9 ms | 5.18× more | 52.1 MB | 2.67× more | mcp-action | yes | 397.5 ms | — |
| Claude Code | 418.8 ms | 410.7 ms–511.8 ms | 5.18× more | 215.3 MB | 11.03× more | mcp-tool | yes | 309.9 ms | — |
| Codex CLI | 336.4 ms | 326.8 ms–343.2 ms | 4.16× more | 142.6 MB | 7.30× more | mcp-tool | yes | 223.2 ms | — |
| OpenCode | 3.61 s | 2.94 s–4.04 s | 44.63× more | 752.8 MB | 38.54× more | mcp-tool | yes | 3.03 s | — |
| pi | 457.4 ms | 433.6 ms–597.1 ms | 5.66× more | 153.9 MB | 7.88× more | mcp-tool | yes | 399.7 ms | — |
| oh-my-pi (omp) | 1.55 s | 1.36 s–1.56 s | 19.18× more | 406.2 MB | 20.80× more | mcp-route | yes | 1.34 s | — |
| oh-my-openagent (omo) | — | — | — | — | — | mcp-tool | no | — | failed: the fixture sentinel never came back |

**Session memory** (idle after 3 turn(s) per session; sizes are process-tree sums, RSS; PSS also recorded on Linux). "Each additional" is the least-squares slope across the measured session counts; 0 B means growth stayed below RSS sampling noise. Threads and descriptors come from the daemon's own `server:memory` after the turns. Processes are counted in the same tree walk as the RSS sample (PSS shares it on Linux), so the count and the bytes describe one observation; a PID shared by two session roots counts once, a root that already exited is counted as neither bytes nor process, and a cell that could not be read is —, never 0.

| Tool | First session | Each additional | vs horon (additional) | Threads/session | FDs/session | Processes (idle) | Processes/session | First (post-turn) | Each additional (post-turn) |
|---|---|---|---|---|---|---|---|---|---|
| horon | 28.9 MB | 135.6 KB | baseline | 0.32 | 1.00 | 1 | 0.00 | 34.9 MB | 396.6 KB |
| jcode | 36.4 MB | 218.5 KB | 1.61× more | — | — | 1 | 0.00 | 40.0 MB | 346.9 KB |
| Claude Code | 159.3 MB | 157.3 MB | 1,188× more | — | — | 1 | 1.00 | 189.2 MB | 185.4 MB |
| Codex CLI | 99.3 MB | 83.0 MB | 626.72× more | — | — | 2 | 1.21 | 106.8 MB | 107.4 MB |
| OpenCode | 242.9 MB | 2.5 MB | 18.79× more | — | — | 1 | 0.00 | 578.9 MB | 4.6 MB |
| pi | 118.5 MB | 115.2 MB | 869.63× more | — | — | 1 | 1.00 | 148.6 MB | 147.7 MB |
| oh-my-pi (omp) | 389.6 MB | 206.8 MB | 1,561× more | — | — | 1 | 1.00 | 465.5 MB | 320.9 MB |
| oh-my-openagent (omo) | 129.7 MB | 51.4 MB | 388.17× more | — | — | 1 | 1.00 | 830.9 MB | 335.2 MB |

Threads/descriptors unavailable: jcode (server:memory did not report thread_count, fd_count)

**Post-turn tree RSS per session count**

| Tool | 1 session(s) | 5 session(s) | 10 session(s) |
|---|---|---|---|
| horon | 34.9 MB | 36.7 MB | 38.4 MB |
| jcode | 40.0 MB | 41.3 MB | 43.1 MB |
| Claude Code | 189.2 MB | 933.4 MB | 1.8 GB |
| Codex CLI | 106.8 MB | 537.0 MB | 1.0 GB |
| OpenCode | 578.9 MB | 597.1 MB | 620.2 MB |
| pi | 148.6 MB | 738.4 MB | 1.4 GB |
| oh-my-pi (omp) | 465.5 MB | 1.8 GB | 3.3 GB |
| oh-my-openagent (omo) | 830.9 MB | 3.0 GB | 3.8 GB |

**Live processes per session count** (idle / post-turn)

| Tool | 1 session(s) | 5 session(s) | 10 session(s) |
|---|---|---|---|
| horon | 1 / 1 | 1 / 1 | 1 / 1 |
| jcode | 1 / 1 | 1 / 1 | 1 / 1 |
| Claude Code | 1 / 1 | 5 / 5 | 10 / 10 |
| Codex CLI | 2 / 1 | 8 / 5 | 13 / 10 |
| OpenCode | 1 / 1 | 1 / 1 | 1 / 1 |
| pi | 1 / 1 | 5 / 5 | 10 / 10 |
| oh-my-pi (omp) | 1 / 1 | 5 / 5 | 10 / 10 |
| oh-my-openagent (omo) | 1 / 4 | 5 / 20 | 10 / 40 |

#### 기동

```text
Benchmarking: /Users/runner/work/agent/agent/bench-bin/horon-agent
============================================================

--help (binary load):
  Min:    7.75 ms
  Max:    12.77 ms
  Mean:   9.83 ms
  Median: 9.27 ms
  Stdev:  1.58 ms

--version:
  Min:    14.16 ms
  Max:    20.50 ms
  Mean:   15.94 ms
  Median: 15.60 ms
  Stdev:  1.79 ms

Measuring isolated server startup (10 runs)...

Server ready (isolated socket connectable):
  Min:    33.08 ms
  Max:    115.87 ms
  Mean:   77.21 ms
  Median: 89.39 ms
  Stdev:  27.25 ms

Server startup stages (one extra run with HORON_AGENT_STARTUP_PROFILE=1, not timed):
  stage             at ms  delta ms   share
  panic_hook          0.0       0.0    0.3%
  logging_init        1.5       1.5   56.9%
  nofile_limit        1.8       0.3   10.8%
  perm_harden         1.8       0.1    2.1%
  args_parse          1.8       0.0    0.0%
  config_loaded       2.1       0.3   11.2%
  server_new          2.4       0.3   10.2%
  socket_bound        2.6       0.2    8.5%

Measuring harness bridge readiness (10 runs)...

Bridge stage: api socket connectable:
  Min:    26.37 ms
  Max:    101.93 ms
  Mean:   68.18 ms
  Median: 73.56 ms
  Stdev:  24.15 ms

Bridge stage: hello_ok:
  Min:    27.35 ms
  Max:    103.38 ms
  Mean:   69.24 ms
  Median: 74.54 ms
  Stdev:  24.17 ms

Bridge stage: no-op reply:
  Min:    27.86 ms
  Max:    104.35 ms
  Mean:   69.89 ms
  Median: 75.22 ms
  Stdev:  24.23 ms

Bridge total (socket -> no-op):
  Min:    27.86 ms
  Max:    104.35 ms
  Mean:   69.89 ms
  Median: 75.22 ms
  Stdev:  24.23 ms

============================================================
Summary:
  Binary load median:      ~9.3 ms
  Server ready median:     ~89.4 ms
  Bridge total median:     ~75.2 ms
```

#### 세션 로딩 (COLD)

```text
binary   /Users/runner/work/agent/agent/bench-bin/examples/loading_probe (f4e27dc87ca7)
stages   /Users/runner/work/agent/agent/bench-bin/examples/resume_stage_probe (19590f0cec4e, 3 cold pass(es) per session fixture)
mode     cold
protocol COLD: every measured run gets its own freshly copied HOME; no derived cache from an earlier run exists, so each sample is a first load of the fixture.
fixtures /var/folders/h5/t85nd63d71x27_8gl5gw3l_w0000gn/T/horon-loading-bench-tde6w2f2
note     --warmup 3 is ignored in cold mode: every sample gets a fresh HOME
fixture events=0 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    0.024/0.025    p95    0.038/0.036    delta  +0.000 ms  control-no-change
  skill_request_overlay p50    0.210/0.217    p95    0.369/0.283    delta  +0.007 ms  control-no-change
  mcp_config_load   p50    0.245/0.255    p95    0.357/0.312    delta  +0.010 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.001/0.000    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.001/0.000    delta  -0.000 ms  control-no-change
  session_save      p50   14.010/15.428   p95   23.590/95.649   delta  +1.418 ms  control-no-change
  total store_read_bytes p50 22940  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition not_applicable: this fixture has no session log
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 22934..22954 (5 distinct values)
  SPREAD   b session_save store_read_bytes 22938..22954 (4 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    4.225/3.819    p95    5.665/4.444    delta  -0.406 ms  control-no-change
  skill_request_overlay p50    5.259/4.891    p95    6.068/7.560    delta  -0.369 ms  control-no-change
  mcp_config_load   p50    0.267/0.273    p95    0.370/0.398    delta  +0.007 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-drift
  provider_dispatch p50    0.000/0.000    p95    0.005/0.001    delta  -0.000 ms  control-drift
  session_save      p50   16.307/15.746   p95   21.902/19.574   delta  -0.560 ms  control-no-change
  total store_read_bytes p50 22948  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition not_applicable: this fixture has no session log
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 22934..22954 (5 distinct values)
  SPREAD   b session_save store_read_bytes 22918..22950 (7 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.025/0.028    p95    0.034/0.046    delta  +0.003 ms  control-no-change
  skill_request_overlay p50    0.243/0.245    p95    0.282/0.295    delta  +0.002 ms  control-no-change
  mcp_config_load   p50    0.283/0.281    p95    0.299/0.304    delta  -0.002 ms  control-no-change
  attach_history    p50  233.627/260.016  p95  255.595/318.750  delta +26.389 ms  control-no-change
  resume_ready      p50   59.964/62.086   p95   77.004/83.571   delta  +2.122 ms  control-no-change
  provider_dispatch p50    1.040/1.180    p95    1.242/2.077    delta  +0.140 ms  control-no-change
  session_save      p50   14.472/14.845   p95   45.482/49.158   delta  +0.373 ms  control-no-change
  total store_read_bytes p50 5311305  full_source_hashes p50 2  skill_dir_reads p50 1
  decomposition 3/3 cold pass(es); anchor attach_history p50 233.627 ms (load 198.950 + history 35.033)
    selection          p50     0.070 ms  min     0.048  max     0.073  ratio 0.0% of anchor
    checkpoint_recent  p50   153.453 ms  min   151.497  max   165.236  ratio 65.7% of anchor
    log_read           p50     0.070 ms  min     0.067  max     0.097  ratio 0.0% of anchor
    full_scan          p50    22.213 ms  min    20.972  max    24.028  ratio 9.5% of anchor
    blob_read          p50     0.006 ms  min     0.005  max     0.006  ratio 0.0% of anchor
    graph_replay       p50    16.192 ms  min    13.733  max    16.813  ratio 6.9% of anchor
    metadata_json      p50     0.020 ms  min     0.015  max     0.036  ratio 0.0% of anchor
    messages_json      p50     0.866 ms  min     0.725  max     1.171  ratio 0.4% of anchor
    messages_decode    p50     0.545 ms  min     0.519  max     0.634  ratio 0.2% of anchor
    source_hash        p50     2.073 ms  min     1.997  max     2.922  ratio 0.9% of anchor
    history_rescan     p50    47.603 ms  min    42.093  max    50.659  ratio 20.4% of anchor
    page_index         p50    50.930 ms  min    50.608  max    58.555  ratio 21.8% of anchor
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189201..4189207 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099141..1099145 (3 distinct values)
  SPREAD   a session_save store_read_bytes 22944..22964 (8 distinct values)
  SPREAD   b attach_history store_read_bytes 4189203..4189207 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099143..1099145 (3 distinct values)
  SPREAD   b session_save store_read_bytes 22948..22968 (6 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    4.145/4.142    p95    6.102/5.182    delta  -0.003 ms  control-no-change
  skill_request_overlay p50    5.286/5.293    p95    7.050/6.981    delta  +0.007 ms  control-no-change
  mcp_config_load   p50    0.276/0.269    p95    0.669/0.442    delta  -0.007 ms  control-no-change
  attach_history    p50  263.091/260.494  p95  298.677/295.757  delta  -2.597 ms  control-no-change
  resume_ready      p50   59.874/61.813   p95   68.736/88.764   delta  +1.939 ms  control-no-change
  provider_dispatch p50    1.001/1.077    p95    1.449/1.834    delta  +0.076 ms  control-no-change
  session_save      p50   14.497/14.854   p95   51.119/32.729   delta  +0.357 ms  control-no-change
  total store_read_bytes p50 5311310  full_source_hashes p50 2  skill_dir_reads p50 1
  decomposition 3/3 cold pass(es); anchor attach_history p50 263.091 ms (load 226.565 + history 37.581)
    selection          p50     0.045 ms  min     0.040  max     0.068  ratio 0.0% of anchor
    checkpoint_recent  p50   164.651 ms  min   154.623  max   169.778  ratio 62.6% of anchor
    log_read           p50     0.081 ms  min     0.079  max     0.082  ratio 0.0% of anchor
    full_scan          p50    20.529 ms  min    20.104  max    20.813  ratio 7.8% of anchor
    blob_read          p50     0.005 ms  min     0.005  max     0.006  ratio 0.0% of anchor
    graph_replay       p50    14.785 ms  min    14.107  max    16.283  ratio 5.6% of anchor
    metadata_json      p50     0.014 ms  min     0.012  max     0.019  ratio 0.0% of anchor
    messages_json      p50     0.771 ms  min     0.731  max     2.260  ratio 0.3% of anchor
    messages_decode    p50     0.489 ms  min     0.485  max     0.489  ratio 0.2% of anchor
    source_hash        p50     2.056 ms  min     1.986  max     2.073  ratio 0.8% of anchor
    history_rescan     p50    39.265 ms  min    37.837  max    40.974  ratio 14.9% of anchor
    page_index         p50    44.343 ms  min    43.621  max    44.958  ratio 16.9% of anchor
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189203..4189207 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099141..1099145 (3 distinct values)
  SPREAD   a session_save store_read_bytes 22944..22967 (8 distinct values)
  SPREAD   b attach_history store_read_bytes 4189205..4189207 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099144..1099145 (2 distinct values)
  SPREAD   b session_save store_read_bytes 22948..22964 (5 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.033/0.030    p95    0.076/0.037    delta  -0.003 ms  control-no-change
  skill_request_overlay p50    0.221/0.218    p95    0.666/0.324    delta  -0.003 ms  control-no-change
  mcp_config_load   p50    0.264/0.257    p95    0.668/0.404    delta  -0.007 ms  control-no-change
  attach_history    p50 2965.518/2960.937 p95 3735.853/3322.781 delta  -4.581 ms  control-no-change
  resume_ready      p50  740.259/773.123  p95  919.936/939.662  delta +32.864 ms  control-no-change
  provider_dispatch p50   11.523/11.846   p95   15.865/18.206   delta  +0.323 ms  control-no-change
  session_save      p50   25.375/19.180   p95   35.875/45.499   delta  -6.195 ms  control-no-change
  total store_read_bytes p50 53417521  full_source_hashes p50 2  skill_dir_reads p50 1
  decomposition 3/3 cold pass(es); anchor attach_history p50 2965.518 ms (load 2491.246 + history 419.266)
    selection          p50     0.079 ms  min     0.078  max     0.813  ratio 0.0% of anchor
    checkpoint_recent  p50  2135.125 ms  min  2052.704  max  2315.665  ratio 72.0% of anchor
    log_read           p50     2.486 ms  min     1.332  max     2.630  ratio 0.1% of anchor
    full_scan          p50   256.517 ms  min   223.812  max   284.124  ratio 8.6% of anchor
    blob_read          p50     0.071 ms  min     0.068  max     0.075  ratio 0.0% of anchor
    graph_replay       p50   211.882 ms  min   203.481  max   266.415  ratio 7.1% of anchor
    metadata_json      p50     0.035 ms  min     0.021  max     0.042  ratio 0.0% of anchor
    messages_json      p50    14.627 ms  min     8.015  max    15.102  ratio 0.5% of anchor
    messages_decode    p50    12.694 ms  min    11.933  max    15.639  ratio 0.4% of anchor
    source_hash        p50    20.578 ms  min    19.037  max    21.541  ratio 0.7% of anchor
    history_rescan     p50   443.714 ms  min   443.102  max   487.495  ratio 15.0% of anchor
    page_index         p50   453.658 ms  min   412.323  max   530.375  ratio 15.3% of anchor
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184297..42184305 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210247..11210255 (3 distinct values)
  SPREAD   a session_save store_read_bytes 22949..22975 (9 distinct values)
  SPREAD   b attach_history store_read_bytes 42184301..42184305 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210251..11210255 (3 distinct values)
  SPREAD   b session_save store_read_bytes 22947..22975 (8 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    4.460/4.524    p95    5.558/6.336    delta  +0.064 ms  control-no-change
  skill_request_overlay p50    5.530/5.626    p95    8.943/10.180   delta  +0.096 ms  control-no-change
  mcp_config_load   p50    0.285/0.286    p95    0.541/0.490    delta  +0.000 ms  control-no-change
  attach_history    p50 2936.378/3033.122 p95 3518.760/3580.549 delta +96.744 ms  control-no-change
  resume_ready      p50  783.393/802.524  p95  976.143/1031.780 delta +19.130 ms  control-no-change
  provider_dispatch p50   12.101/14.531   p95   18.954/20.083   delta  +2.429 ms  control-no-change
  session_save      p50   22.953/25.785   p95   38.823/103.822  delta  +2.832 ms  control-no-change
  total store_read_bytes p50 53417518  full_source_hashes p50 2  skill_dir_reads p50 1
  decomposition 3/3 cold pass(es); anchor attach_history p50 2936.378 ms (load 2497.239 + history 471.603)
    selection          p50     0.115 ms  min     0.066  max     0.165  ratio 0.0% of anchor
    checkpoint_recent  p50  2127.532 ms  min  2019.338  max  2523.475  ratio 72.5% of anchor
    log_read           p50     1.462 ms  min     1.229  max     2.498  ratio 0.0% of anchor
    full_scan          p50   259.577 ms  min   259.568  max   349.707  ratio 8.8% of anchor
    blob_read          p50     0.114 ms  min     0.064  max     0.171  ratio 0.0% of anchor
    graph_replay       p50   250.551 ms  min   242.866  max   282.692  ratio 8.5% of anchor
    metadata_json      p50     0.032 ms  min     0.025  max     0.062  ratio 0.0% of anchor
    messages_json      p50    22.929 ms  min    12.076  max    24.907  ratio 0.8% of anchor
    messages_decode    p50    14.178 ms  min    11.963  max    17.128  ratio 0.5% of anchor
    source_hash        p50    26.917 ms  min    21.271  max    27.287  ratio 0.9% of anchor
    history_rescan     p50   533.280 ms  min   460.869  max   538.786  ratio 18.2% of anchor
    page_index         p50   526.720 ms  min   499.274  max   590.832  ratio 17.9% of anchor
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184301..42184305 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210251..11210255 (2 distinct values)
  SPREAD   a session_save store_read_bytes 22943..22967 (8 distinct values)
  SPREAD   b attach_history store_read_bytes 42184299..42184305 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210249..11210255 (3 distinct values)
  SPREAD   b session_save store_read_bytes 22947..22971 (6 distinct values)
report   bench-results/loading-cold.json
mode cold, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### 세션 로딩 (WARM)

```text
binary   /Users/runner/work/agent/agent/bench-bin/examples/loading_probe (f4e27dc87ca7)
stages   /Users/runner/work/agent/agent/bench-bin/examples/resume_stage_probe (19590f0cec4e, 0 cold pass(es) per session fixture)
mode     warm
protocol WARM: one HOME per side, --warmup discarded runs per side, then measured runs alternate between the two sides on those same HOMEs.
fixtures /var/folders/h5/t85nd63d71x27_8gl5gw3l_w0000gn/T/horon-loading-bench-xpgwkrn3
fixture events=0 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    0.030/0.029    p95    0.036/0.085    delta  -0.001 ms  control-no-change
  skill_request_overlay p50    0.244/0.251    p95    0.350/0.422    delta  +0.006 ms  control-no-change
  mcp_config_load   p50    0.281/0.280    p95    0.348/0.299    delta  -0.001 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.001    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.001    delta  +0.000 ms  control-no-change
  session_save      p50   13.539/14.341   p95   39.099/16.676   delta  +0.802 ms  control-no-change
  total store_read_bytes p50 23665  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition not_applicable: this fixture has no session log
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [22951, 23649, 23665, 24367, 24375, 24379, 24391]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [22947, 23653, 23665, 23669, 24367, 24371, 24375]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 22951..24391 (7 distinct values)
  SPREAD   b session_save store_read_bytes 22947..24375 (7 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    4.032/4.134    p95    5.840/4.864    delta  +0.102 ms  control-no-change
  skill_request_overlay p50    5.253/5.271    p95    6.670/6.384    delta  +0.018 ms  control-no-change
  mcp_config_load   p50    0.277/0.274    p95    0.301/0.307    delta  -0.003 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-drift
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  session_save      p50   14.316/13.571   p95   23.004/14.711   delta  -0.745 ms  control-no-change
  total store_read_bytes p50 23663  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition not_applicable: this fixture has no session log
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [22931, 22935, 22945, 23659, 23661, 23665, 24367, 24373, 24375, 24379]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [22935, 22949, 22957, 23663, 23667, 23673, 24365, 24369, 24373, 24377]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 22931..24379 (10 distinct values)
  SPREAD   b session_save store_read_bytes 22935..24377 (10 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.025/0.028    p95    0.077/0.040    delta  +0.004 ms  control-no-change
  skill_request_overlay p50    0.240/0.228    p95    0.806/0.257    delta  -0.012 ms  control-no-change
  mcp_config_load   p50    0.267/0.262    p95    0.421/0.302    delta  -0.006 ms  control-no-change
  attach_history    p50  105.317/106.466  p95  151.749/166.591  delta  +1.149 ms  control-no-change
  resume_ready      p50   65.516/70.225   p95   80.887/126.357  delta  +4.709 ms  control-no-change
  provider_dispatch p50    1.187/1.212    p95    1.309/1.340    delta  +0.024 ms  control-no-change
  session_save      p50   14.060/15.583   p95   20.443/25.328   delta  +1.523 ms  control-no-change
  total store_read_bytes p50 2841253  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition skipped: --stage-probe-samples is 0
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23670, 23678, 24386, 24388, 25100, 25106, 25802, 25808, 26528, 26534]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23670, 23678, 24380, 24388, 25094, 25812, 26522, 26534]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23670..26534 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23670..26534 (8 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    4.879/4.483    p95    9.696/12.839   delta  -0.396 ms  control-no-change
  skill_request_overlay p50    5.596/5.585    p95    6.828/13.143   delta  -0.011 ms  control-no-change
  mcp_config_load   p50    0.269/0.297    p95    0.294/0.859    delta  +0.028 ms  control-no-change
  attach_history    p50  110.187/121.376  p95  139.626/165.329  delta +11.189 ms  control-no-change
  resume_ready      p50   84.565/80.616   p95  106.521/127.912  delta  -3.949 ms  control-no-change
  provider_dispatch p50    1.418/1.407    p95    1.871/1.740    delta  -0.011 ms  control-no-change
  session_save      p50   22.306/17.703   p95   38.442/31.074   delta  -4.603 ms  control-no-change
  total store_read_bytes p50 2841251  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition skipped: --stage-probe-samples is 0
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23676, 23678, 24382, 24386, 25084, 25102, 25810, 25820, 26524, 26532]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23658, 23670, 24380, 24388, 25094, 25102, 25812, 26526, 26530]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23676..26532 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23658..26530 (9 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.037/0.031    p95    0.192/0.067    delta  -0.007 ms  control-no-change
  skill_request_overlay p50    0.244/0.268    p95    1.037/0.610    delta  +0.024 ms  control-no-change
  mcp_config_load   p50    0.277/0.294    p95    0.969/0.623    delta  +0.017 ms  control-no-change
  attach_history    p50 1682.940/1575.441 p95 2159.316/1821.048 delta -107.499 ms  control-no-change
  resume_ready      p50 1089.213/963.415  p95 1233.380/1302.587 delta -125.798 ms  control-no-change
  provider_dispatch p50   15.128/15.052   p95   26.326/25.294   delta  -0.076 ms  control-no-change
  session_save      p50   28.406/28.943   p95   51.609/45.717   delta  +0.536 ms  control-no-change
  total store_read_bytes p50 28640284  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition skipped: --stage-probe-samples is 0
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23665, 23673, 24375, 24383, 25105, 25113, 25807, 25815, 26521, 26541]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23661, 23677, 24381, 24397, 25097, 25099, 25805, 25827, 26525, 26531]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23665..26541 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23661..26531 (10 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    4.192/4.139    p95    5.824/9.472    delta  -0.054 ms  control-no-change
  skill_request_overlay p50    5.295/5.353    p95    6.824/7.915    delta  +0.058 ms  control-no-change
  mcp_config_load   p50    0.295/0.289    p95    0.604/0.456    delta  -0.006 ms  control-no-change
  attach_history    p50 1396.894/1353.383 p95 1689.842/1762.444 delta -43.512 ms  control-no-change
  resume_ready      p50  850.071/797.340  p95 1085.572/895.634  delta -52.731 ms  control-no-change
  provider_dispatch p50   13.438/14.289   p95   18.092/16.968   delta  +0.851 ms  control-no-change
  session_save      p50   26.799/22.278   p95   47.177/32.080   delta  -4.521 ms  control-no-change
  total store_read_bytes p50 28640269  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition skipped: --stage-probe-samples is 0
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23670, 23680, 24394, 24400, 25092, 25100, 25806, 25818, 26528]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23672, 23676, 24378, 24386, 25112, 25116, 25810, 26520, 26532]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23670..26528 (9 distinct values)
  SPREAD   b session_save store_read_bytes 23672..26532 (9 distinct values)
report   bench-results/loading-warm.json
mode warm, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### Tool latency

| Tool case | median | p95 | min | max | samples |
|---|---|---|---|---|---|
| read (tiny file) | 0.2 ms | 0.3 ms | 0.2 ms | 0.3 ms | 5 |
| read (1000 lines) | 0.8 ms | 1.3 ms | 0.6 ms | 1.3 ms | 5 |
| read (10 lines @ offset) | 0.5 ms | 0.6 ms | 0.4 ms | 0.6 ms | 5 |
| read (repo Cargo.toml) | 0.4 ms | 0.5 ms | 0.3 ms | 0.5 ms | 5 |
| write (small) | 2.1 ms | 2.1 ms | 1.9 ms | 2.1 ms | 5 |
| write (10KB) | 2.6 ms | 3.9 ms | 2.4 ms | 3.9 ms | 5 |
| edit (simple replace) | 2.3 ms | 4.4 ms | 2.0 ms | 4.4 ms | 5 |
| ls (scripts dir) | 0.8 ms | 3.3 ms | 0.7 ms | 3.3 ms | 5 |
| agentgrep (grep in tree) | 5.2 ms | 5.7 ms | 4.1 ms | 5.7 ms | 5 |
| agentgrep (find **/*.rs) | 1.6 ms | 1.6 ms | 1.4 ms | 1.6 ms | 5 |
| bash (true) | 5.0 ms | 6.1 ms | 4.7 ms | 6.1 ms | 5 |
| bash (echo hello) | 5.3 ms | 7.2 ms | 4.9 ms | 7.2 ms | 5 |
| bash (ls fixture tree) | 7.7 ms | 8.6 ms | 7.5 ms | 8.6 ms | 5 |
| invalid tool (dispatch only) | 0.1 ms | 0.2 ms | 0.1 ms | 0.2 ms | 5 |

#### Tool CPU per call

| Tool case | daemon cpu median | min | max | child cpu median | voluntary switches | involuntary switches | windows × calls | source |
|---|---|---|---|---|---|---|---|---|
| read (tiny file) | — | — | — | — | — | — | 0 | — |
| read (1000 lines) | — | — | — | — | — | — | 0 | — |
| read (10 lines @ offset) | — | — | — | — | — | — | 0 | — |
| read (repo Cargo.toml) | — | — | — | — | — | — | 0 | — |
| write (small) | — | — | — | — | — | — | 0 | — |
| write (10KB) | — | — | — | — | — | — | 0 | — |
| edit (simple replace) | — | — | — | — | — | — | 0 | — |
| ls (scripts dir) | — | — | — | — | — | — | 0 | — |
| agentgrep (grep in tree) | — | — | — | — | — | — | 0 | — |
| agentgrep (find **/*.rs) | — | — | — | — | — | — | 0 | — |
| bash (true) | — | — | — | — | — | — | 0 | — |
| bash (echo hello) | — | — | — | — | — | — | 0 | — |
| bash (ls fixture tree) | — | — | — | — | — | — | 0 | — |
| invalid tool (dispatch only) | — | — | — | — | — | — | 0 | — |

CPU unmeasured: no /proc CPU counters for the daemon on this system

#### Reload storm

1 resume(s) per level; each level resumes sessions no other level has opened, from a freshly started daemon.

| Concurrent resumes | history median | p95 | max | storm wall | failures |
|---|---|---|---|---|---|
| 1 | 58.6 ms | 58.6 ms | 58.6 ms | 58.7 ms | 0 |
| 10 | 220.2 ms | 226.1 ms | 226.1 ms | 226.3 ms | 0 |
| 40 | 637.6 ms | 707.8 ms | 707.8 ms | 708.4 ms | 0 |

#### Compile loop

| Checkpoint | Touched | median | min | max | samples |
|---|---|---|---|---|---|
| warm_noop | — | 1.55 s | 1.55 s | 1.55 s | 1 |
| leaf_touch | src/main.rs | 755.3 ms | 755.3 ms | 755.3 ms | 1 |
| core_touch | crates/app-core/src/lib.rs | 29.05 s | 29.05 s | 29.05 s | 1 |
| build_meta_touch | crates/build-meta/src/lib.rs | 68.58 s | 68.58 s | 68.58 s | 1 |

### Windows

#### Coding agent comparison (headless, mock model)

Host: windows/AMD64, 4 CPUs, Python 3.12.10. Baseline: **horon**. Samples per one-shot, skill and MCP cell: 5.

Skipped: syscalls (strace not available)

| Tool | Version | `--version` (median) | Install size |
|---|---|---|---|
| horon | horon-agent v0.1.0-dev (2f63e44) | 13.2 ms | 95.5 MB |
| jcode | jcode v0.99.1 (7206efd0c) | 15.1 ms | 128.4 MB |
| Claude Code | 2.1.317 (Claude Code) | 42.1 ms | 645.2 MB |
| Codex CLI | codex-cli 0.166.0 | 94.0 ms | 479.4 MB |
| OpenCode | 1.19.5 | 500.9 ms | 605.3 MB |
| pi | 1.5.0 | 258.4 ms | 109.0 MB |
| oh-my-pi (omp) | omp/18.18.0 | 657.1 ms | 292.5 MB |
| oh-my-openagent (omo) | omo 5.1.64 (engine: senpi 2026.11.5-1) | 194.5 ms | 514.9 MB |

**One-shot headless prompt** (includes startup, one `ls` tool call, final answer)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Tool round-trip | request bytes | Note |
|---|---|---|---|---|---|---|---|---|
| horon | 164.2 ms | 151.8 ms–236.8 ms | baseline | 129.1 MB | baseline | yes | 3,664 / 2,368 (1 tools) | — |
| jcode | 131.5 ms | 123.5 ms–215.3 ms | 1.25× less | 32.0 MB | 4.03× less | yes | 3,105 / 1,312 (1 tools) | — |
| Claude Code | 1.27 s | 1.25 s–2.06 s | 7.72× more | 273.4 MB | 2.12× more | yes | 50,067 / 33,795 (14 tools) | — |
| Codex CLI | 1.90 s | 1.67 s–2.10 s | 11.58× more | 327.8 MB | 2.54× more | yes | 41,028 / 18,342 (9 tools) | — |
| OpenCode | 4.78 s | 4.63 s–7.51 s | 29.14× more | 842.1 MB | 6.52× more | yes | 2,659 / 0 (0 tools) | — |
| pi | 450.7 ms | 438.2 ms–483.8 ms | 2.75× more | 124.0 MB | 1.04× less | yes | 5,522 / 2,841 (4 tools) | — |
| oh-my-pi (omp) | 2.87 s | 2.74 s–3.33 s | 17.50× more | 431.9 MB | 3.35× more | yes | 29,889 / 18,533 (11 tools) | — |
| oh-my-openagent (omo) | 26.80 s | 17.99 s–39.31 s | 163.27× more | 563.9 MB | 4.37× more | yes | 29,467 / 2,424 (2 tools) | — |

`request bytes` is the first model request of the first run as the mock server received it: JSON body bytes / tool catalog bytes (tools offered).

**Core-profile one-shot** (the one-shot turn without `--tools`, so the request carries the default core surface: the eager tools, `toolbox` and the listed-tool index)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | Note | request bytes |
|---|---|---|---|---|---|---|---|---|---|
| horon | 206.3 ms | 178.6 ms–232.7 ms | baseline | 36.1 MB | baseline | — | yes | — | 11,033 / 9,079 (8 tools) |
| jcode | — | — | — | — | — | — | — | skipped: jcode has no core tool profile | — |
| Claude Code | — | — | — | — | — | — | — | skipped: Claude Code has no core tool profile | — |
| Codex CLI | — | — | — | — | — | — | — | skipped: Codex CLI has no core tool profile | — |
| OpenCode | — | — | — | — | — | — | — | skipped: OpenCode has no core tool profile | — |
| pi | — | — | — | — | — | — | — | skipped: pi has no core tool profile | — |
| oh-my-pi (omp) | — | — | — | — | — | — | — | skipped: oh-my-pi (omp) has no core tool profile | — |
| oh-my-openagent (omo) | — | — | — | — | — | — | — | skipped: oh-my-openagent (omo) has no core tool profile | — |

**Skill invocation** (process start to final answer, including skill discovery and one load of the fixture skill)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | Note |
|---|---|---|---|---|---|---|---|---|
| horon | 243.4 ms | 200.0 ms–303.6 ms | baseline | 24.5 MB | baseline | skill-uri | yes | — |
| jcode | 195.4 ms | 178.0 ms–214.8 ms | 1.25× less | 23.5 MB | 1.04× less | skill-tool | yes | — |
| Claude Code | 901.1 ms | 872.8 ms–1.05 s | 3.70× more | 238.9 MB | 9.75× more | skill-tool | yes | — |
| Codex CLI | 2.04 s | 1.69 s–2.11 s | 8.38× more | 262.0 MB | 10.69× more | file-read | yes | — |
| OpenCode | 6.45 s | 6.42 s–6.79 s | 26.48× more | 742.0 MB | 30.28× more | skill-tool | yes | — |
| pi | 611.6 ms | 583.8 ms–708.0 ms | 2.51× more | 97.8 MB | 3.99× more | file-read | yes | — |
| oh-my-pi (omp) | 3.17 s | 2.72 s–3.66 s | 13.03× more | 422.4 MB | 17.24× more | file-read | yes | — |
| oh-my-openagent (omo) | 20.38 s | 18.24 s–29.40 s | 83.72× more | 542.2 MB | 22.13× more | file-read | yes | — |

**MCP tool call** (process start to final answer, including starting the fixture stdio MCP server and one `bench_echo` call)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | MCP ready | Note |
|---|---|---|---|---|---|---|---|---|---|
| horon | 265.1 ms | 220.6 ms–369.6 ms | baseline | 37.3 MB | baseline | mcp-call | yes | 139.0 ms | — |
| jcode | 345.9 ms | 311.8 ms–449.3 ms | 1.30× more | 36.9 MB | same | mcp-action | yes | 217.2 ms | — |
| Claude Code | 970.1 ms | 910.0 ms–1.05 s | 3.66× more | 274.7 MB | 7.37× more | mcp-tool | yes | 818.5 ms | — |
| Codex CLI | 956.9 ms | 811.0 ms–2.73 s | 3.61× more | 177.9 MB | 4.77× more | mcp-tool | yes | 777.8 ms | — |
| OpenCode | 7.26 s | 6.64 s–10.00 s | 27.38× more | 3.3 GB | 91.57× more | mcp-tool | yes | 5.78 s | — |
| pi | 680.9 ms | 674.8 ms–788.6 ms | 2.57× more | 118.4 MB | 3.18× more | mcp-tool | yes | 545.8 ms | — |
| oh-my-pi (omp) | 3.52 s | 2.83 s–4.06 s | 13.29× more | 440.0 MB | 11.80× more | mcp-route | yes | 2.60 s | — |
| oh-my-openagent (omo) | 21.73 s | 19.58 s–26.73 s | 81.96× more | 562.5 MB | 15.09× more | mcp-tool | yes | 18.15 s | — |

**Session memory** (idle after 3 turn(s) per session; sizes are process-tree sums, RSS; PSS also recorded on Linux). "Each additional" is the least-squares slope across the measured session counts; 0 B means growth stayed below RSS sampling noise. Threads and descriptors come from the daemon's own `server:memory` after the turns. Processes are counted in the same tree walk as the RSS sample (PSS shares it on Linux), so the count and the bytes describe one observation; a PID shared by two session roots counts once, a root that already exited is counted as neither bytes nor process, and a cell that could not be read is —, never 0.

| Tool | First session | Each additional | vs horon (additional) | Threads/session | FDs/session | Processes (idle) | Processes/session | First (post-turn) | Each additional (post-turn) |
|---|---|---|---|---|---|---|---|---|---|
| horon | 22.2 MB | 110.0 KB | baseline | — | — | 1 | 0.00 | 29.8 MB | 344.6 KB |
| jcode | 22.0 MB | 69.8 KB | 1.58× less | — | — | 1 | 0.00 | 27.1 MB | 406.3 KB |
| Claude Code | 153.9 MB | 166.1 MB | 1,546× more | — | — | 2 | 2.23 | 185.4 MB | 189.6 MB |
| Codex CLI | 125.8 MB | 96.1 MB | 893.95× more | — | — | 6 | 3.00 | 165.1 MB | 116.0 MB |
| OpenCode | 246.2 MB | 0.0 B | — | — | — | 2 | 0.00 | 435.8 MB | 6.9 MB |
| pi | 91.2 MB | 90.7 MB | 844.29× more | — | — | 2 | 2.00 | 101.6 MB | 101.0 MB |
| oh-my-pi (omp) | 330.3 MB | 130.4 MB | 1,214× more | — | — | 2 | 2.00 | 342.0 MB | 340.4 MB |
| oh-my-openagent (omo) | 148.9 MB | 133.2 MB | 1,240× more | — | — | 4 | 4.00 | 416.4 MB | 443.1 MB |

Threads/descriptors unavailable: horon (server:memory did not report thread_count, fd_count), jcode (server:memory did not report thread_count, fd_count)

**Post-turn tree RSS per session count**

| Tool | 1 session(s) | 5 session(s) | 10 session(s) |
|---|---|---|---|
| horon | 29.8 MB | 31.7 MB | 32.9 MB |
| jcode | 27.1 MB | 28.6 MB | 30.6 MB |
| Claude Code | 185.4 MB | 948.6 MB | 1.8 GB |
| Codex CLI | 165.1 MB | 648.1 MB | 1.2 GB |
| OpenCode | 435.8 MB | 492.0 MB | 499.7 MB |
| pi | 101.6 MB | 505.2 MB | 1010.6 MB |
| oh-my-pi (omp) | 342.0 MB | 1.7 GB | 3.3 GB |
| oh-my-openagent (omo) | 416.4 MB | 2.1 GB | 4.0 GB |

**Live processes per session count** (idle / post-turn)

| Tool | 1 session(s) | 5 session(s) | 10 session(s) |
|---|---|---|---|
| horon | 1 / 1 | 1 / 1 | 1 / 1 |
| jcode | 1 / 1 | 1 / 1 | 1 / 1 |
| Claude Code | 2 / 2 | 10 / 10 | 22 / 20 |
| Codex CLI | 6 / 6 | 18 / 18 | 33 / 30 |
| OpenCode | 2 / 2 | 2 / 2 | 2 / 2 |
| pi | 2 / 2 | 10 / 10 | 20 / 20 |
| oh-my-pi (omp) | 2 / 2 | 10 / 10 | 20 / 20 |
| oh-my-openagent (omo) | 4 / 6 | 20 / 50 | 40 / 64 |

**Errors recorded during measurement**

- oh-my-openagent (omo) @ 10 sessions: turn 2 session 6: Connection error.
- oh-my-openagent (omo) @ 10 sessions: turn 3 session 6: Agent is already processing. Specify streamingBehavior ('steer' or 'followUp') to queue the message.

#### 기동

```text
Benchmarking: D:/a/agent/agent/bench-bin/horon-agent.exe
============================================================

--help (binary load):
  Min:    14.34 ms
  Max:    15.61 ms
  Mean:   14.61 ms
  Median: 14.51 ms
  Stdev:  0.37 ms

--version:
  Min:    13.76 ms
  Max:    15.78 ms
  Mean:   14.38 ms
  Median: 14.09 ms
  Stdev:  0.64 ms

Measuring isolated server startup (10 runs)...

Server ready (isolated socket connectable):
  Min:    22.32 ms
  Max:    28.17 ms
  Mean:   23.19 ms
  Median: 22.73 ms
  Stdev:  1.77 ms

Server startup stages (one extra run with HORON_AGENT_STARTUP_PROFILE=1, not timed):
  stage             at ms  delta ms   share
  panic_hook          0.0       0.0    0.1%
  logging_init        1.3       1.3   19.1%
  nofile_limit        1.8       0.5    8.1%
  perm_harden         4.4       2.6   38.9%
  args_parse          4.4       0.0    0.0%
  config_loaded       5.4       0.9   13.7%
  server_new          6.4       1.0   14.9%
  socket_bound        6.7       0.3    5.2%

Measuring harness bridge readiness (10 runs)...
  skipped: api-bridge is Unix-only, so Windows has no harness bridge to time

Bridge stage: api socket connectable: No successful runs

Bridge stage: hello_ok: No successful runs

Bridge stage: no-op reply: No successful runs

Bridge total (socket -> no-op): No successful runs

============================================================
Summary:
  Binary load median:      ~14.5 ms
  Server ready median:     ~22.7 ms
  Bridge total median:     n/a
```

#### 세션 로딩 (COLD)

```text
binary   D:\a\agent\agent\bench-bin\examples\loading_probe.exe (6cb0c264e226)
stages   D:\a\agent\agent\bench-bin\examples\resume_stage_probe.exe (aa178149eb6c, 3 cold pass(es) per session fixture)
mode     cold
protocol COLD: every measured run gets its own freshly copied HOME; no derived cache from an earlier run exists, so each sample is a first load of the fixture.
fixtures C:\Users\RUNNER~1\AppData\Local\Temp\horon-loading-bench-l2rabmgl
note     --warmup 3 is ignored in cold mode: every sample gets a fresh HOME
fixture events=0 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    0.101/0.101    p95    0.118/0.103    delta  +0.000 ms  control-no-change
  skill_request_overlay p50    0.740/0.741    p95    1.430/0.779    delta  +0.002 ms  control-no-change
  mcp_config_load   p50    0.927/0.912    p95    1.009/0.951    delta  -0.015 ms  control-no-change
  attach_history    p50    0.001/0.001    p95    0.002/0.001    delta  -0.000 ms  control-drift
  resume_ready      p50    0.000/0.000    p95    0.000/0.001    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.001/0.001    delta  -0.000 ms  control-no-change
  session_save      p50   49.280/50.452   p95   63.726/51.507   delta  +1.172 ms  control-no-change
  total store_read_bytes p50 22950  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition not_applicable: this fixture has no session log
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 22898..22960 (8 distinct values)
  SPREAD   b session_save store_read_bytes 22894..22956 (6 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50   14.060/14.031   p95   16.640/15.193   delta  -0.030 ms  control-no-change
  skill_request_overlay p50   37.314/37.276   p95   43.149/37.854   delta  -0.039 ms  control-no-change
  mcp_config_load   p50    0.996/0.969    p95    1.086/1.098    delta  -0.027 ms  control-no-change
  attach_history    p50    0.002/0.002    p95    0.002/0.005    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.001    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.001/0.000    delta  +0.000 ms  control-no-change
  session_save      p50   57.346/61.789   p95   77.509/103.402  delta  +4.443 ms  control-no-change
  total store_read_bytes p50 22956  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition not_applicable: this fixture has no session log
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 22898..22968 (8 distinct values)
  SPREAD   b session_save store_read_bytes 22900..22960 (7 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.100/0.100    p95    0.127/0.123    delta  +0.000 ms  control-no-change
  skill_request_overlay p50    0.743/0.780    p95    0.970/0.818    delta  +0.037 ms  control-no-change
  mcp_config_load   p50    0.948/0.938    p95    1.097/0.992    delta  -0.010 ms  control-no-change
  attach_history    p50  362.206/364.279  p95  367.391/387.287  delta  +2.073 ms  control-no-change
  resume_ready      p50  144.810/145.456  p95  156.344/150.543  delta  +0.646 ms  control-no-change
  provider_dispatch p50    1.587/1.580    p95    1.621/1.663    delta  -0.007 ms  control-no-change
  session_save      p50   45.737/47.092   p95   69.525/55.629   delta  +1.355 ms  control-no-change
  total store_read_bytes p50 5311214  full_source_hashes p50 2  skill_dir_reads p50 1
  decomposition 3/3 cold pass(es); anchor attach_history p50 362.206 ms (load 315.842 + history 46.548)
    selection          p50     0.123 ms  min     0.116  max     0.149  ratio 0.0% of anchor
    checkpoint_recent  p50   261.262 ms  min   260.500  max   266.811  ratio 72.1% of anchor
    log_read           p50     0.171 ms  min     0.159  max     0.197  ratio 0.0% of anchor
    full_scan          p50    25.536 ms  min    25.459  max    25.652  ratio 7.1% of anchor
    blob_read          p50     0.004 ms  min     0.003  max     0.005  ratio 0.0% of anchor
    graph_replay       p50    18.955 ms  min    18.811  max    19.590  ratio 5.2% of anchor
    metadata_json      p50     0.009 ms  min     0.009  max     0.017  ratio 0.0% of anchor
    messages_json      p50     1.445 ms  min     1.415  max     1.445  ratio 0.4% of anchor
    messages_decode    p50     0.945 ms  min     0.905  max     0.949  ratio 0.3% of anchor
    source_hash        p50     0.505 ms  min     0.481  max     0.511  ratio 0.1% of anchor
    history_rescan     p50    45.311 ms  min    45.264  max    46.128  ratio 12.5% of anchor
    page_index         p50    69.309 ms  min    67.550  max    71.439  ratio 19.1% of anchor
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189151..4189155 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099102..1099106 (2 distinct values)
  SPREAD   a session_save store_read_bytes 22939..22968 (10 distinct values)
  SPREAD   b attach_history store_read_bytes 4189153..4189155 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099105..1099106 (2 distinct values)
  SPREAD   b session_save store_read_bytes 22923..22972 (7 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50   14.984/15.162   p95   15.535/16.418   delta  +0.178 ms  control-no-change
  skill_request_overlay p50   37.118/37.180   p95   37.817/40.510   delta  +0.061 ms  control-no-change
  mcp_config_load   p50    0.972/0.962    p95    1.038/1.016    delta  -0.010 ms  control-no-change
  attach_history    p50  364.640/360.173  p95  393.184/367.597  delta  -4.468 ms  control-no-change
  resume_ready      p50  145.677/145.067  p95  151.526/158.421  delta  -0.610 ms  control-no-change
  provider_dispatch p50    1.518/1.519    p95    1.584/1.570    delta  +0.001 ms  control-no-change
  session_save      p50   48.411/50.092   p95   54.576/61.856   delta  +1.681 ms  control-no-change
  total store_read_bytes p50 5311217  full_source_hashes p50 2  skill_dir_reads p50 1
  decomposition 3/3 cold pass(es); anchor attach_history p50 364.640 ms (load 317.040 + history 46.690)
    selection          p50     0.148 ms  min     0.146  max     0.148  ratio 0.0% of anchor
    checkpoint_recent  p50   271.155 ms  min   263.524  max   275.445  ratio 74.4% of anchor
    log_read           p50     0.169 ms  min     0.149  max     0.181  ratio 0.0% of anchor
    full_scan          p50    25.812 ms  min    25.704  max    26.077  ratio 7.1% of anchor
    blob_read          p50     0.005 ms  min     0.003  max     0.005  ratio 0.0% of anchor
    graph_replay       p50    19.309 ms  min    19.282  max    20.283  ratio 5.3% of anchor
    metadata_json      p50     0.009 ms  min     0.009  max     0.009  ratio 0.0% of anchor
    messages_json      p50     1.438 ms  min     1.422  max     1.520  ratio 0.4% of anchor
    messages_decode    p50     0.972 ms  min     0.926  max     0.980  ratio 0.3% of anchor
    source_hash        p50     0.505 ms  min     0.491  max     0.514  ratio 0.1% of anchor
    history_rescan     p50    45.817 ms  min    45.296  max    46.514  ratio 12.6% of anchor
    page_index         p50    67.981 ms  min    66.201  max    69.851  ratio 18.6% of anchor
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189153..4189155 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099105..1099106 (2 distinct values)
  SPREAD   a session_save store_read_bytes 22936..22976 (6 distinct values)
  SPREAD   b attach_history store_read_bytes 4189151..4189155 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099102..1099106 (3 distinct values)
  SPREAD   b session_save store_read_bytes 22943..22972 (8 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.101/0.101    p95    0.135/0.131    delta  +0.001 ms  control-no-change
  skill_request_overlay p50    0.766/0.753    p95    0.834/0.820    delta  -0.013 ms  control-no-change
  mcp_config_load   p50    0.919/0.906    p95    1.026/0.959    delta  -0.013 ms  control-no-change
  attach_history    p50 4077.018/4077.666 p95 4102.168/4103.049 delta  +0.648 ms  control-no-change
  resume_ready      p50 1524.580/1528.909 p95 1537.710/1536.894 delta  +4.330 ms  control-no-change
  provider_dispatch p50   16.943/16.571   p95   18.048/17.785   delta  -0.372 ms  control-no-change
  session_save      p50   45.276/44.277   p95   52.495/48.612   delta  -0.999 ms  control-no-change
  total store_read_bytes p50 53417428  full_source_hashes p50 2  skill_dir_reads p50 1
  decomposition 3/3 cold pass(es); anchor attach_history p50 4077.018 ms (load 3537.741 + history 540.073)
    selection          p50     0.143 ms  min     0.142  max     0.149  ratio 0.0% of anchor
    checkpoint_recent  p50  3023.910 ms  min  3012.509  max  3025.102  ratio 74.2% of anchor
    log_read           p50     1.673 ms  min     1.653  max     1.833  ratio 0.0% of anchor
    full_scan          p50   262.749 ms  min   260.728  max   263.440  ratio 6.4% of anchor
    blob_read          p50     0.065 ms  min     0.062  max     0.067  ratio 0.0% of anchor
    graph_replay       p50   245.092 ms  min   242.827  max   264.607  ratio 6.0% of anchor
    metadata_json      p50     0.013 ms  min     0.012  max     0.013  ratio 0.0% of anchor
    messages_json      p50    20.058 ms  min    19.904  max    20.421  ratio 0.5% of anchor
    messages_decode    p50    15.107 ms  min    13.841  max    15.614  ratio 0.4% of anchor
    source_hash        p50     4.662 ms  min     4.522  max     4.729  ratio 0.1% of anchor
    history_rescan     p50   531.071 ms  min   530.553  max   557.219  ratio 13.0% of anchor
    page_index         p50   593.496 ms  min   586.020  max   622.276  ratio 14.6% of anchor
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184249..42184253 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210212..11210216 (2 distinct values)
  SPREAD   a session_save store_read_bytes 22931..22973 (8 distinct values)
  SPREAD   b attach_history store_read_bytes 42184249..42184253 (4 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210212..11210216 (3 distinct values)
  SPREAD   b session_save store_read_bytes 22947..22975 (9 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50   14.572/14.707   p95   15.888/15.596   delta  +0.135 ms  control-no-change
  skill_request_overlay p50   37.711/37.805   p95   47.551/46.807   delta  +0.094 ms  control-no-change
  mcp_config_load   p50    0.994/0.989    p95    1.421/1.346    delta  -0.005 ms  control-no-change
  attach_history    p50 4087.228/4109.465 p95 4227.287/4773.957 delta +22.237 ms  control-no-change
  resume_ready      p50 1523.848/1530.143 p95 1549.087/1630.703 delta  +6.296 ms  control-no-change
  provider_dispatch p50   17.983/17.195   p95   22.671/19.764   delta  -0.788 ms  control-no-change
  session_save      p50   45.428/46.081   p95  397.726/77.774   delta  +0.654 ms  control-no-change
  total store_read_bytes p50 53417428  full_source_hashes p50 2  skill_dir_reads p50 1
  decomposition 3/3 cold pass(es); anchor attach_history p50 4087.228 ms (load 3549.249 + history 541.386)
    selection          p50     0.153 ms  min     0.134  max     0.211  ratio 0.0% of anchor
    checkpoint_recent  p50  2996.752 ms  min  2984.384  max  3010.270  ratio 73.3% of anchor
    log_read           p50     1.655 ms  min     1.588  max     1.693  ratio 0.0% of anchor
    full_scan          p50   260.883 ms  min   259.596  max   266.187  ratio 6.4% of anchor
    blob_read          p50     0.065 ms  min     0.064  max     0.068  ratio 0.0% of anchor
    graph_replay       p50   245.313 ms  min   243.528  max   254.399  ratio 6.0% of anchor
    metadata_json      p50     0.013 ms  min     0.012  max     0.013  ratio 0.0% of anchor
    messages_json      p50    19.928 ms  min    19.878  max    20.011  ratio 0.5% of anchor
    messages_decode    p50    16.007 ms  min    15.997  max    16.112  ratio 0.4% of anchor
    source_hash        p50     4.574 ms  min     4.503  max     4.643  ratio 0.1% of anchor
    history_rescan     p50   536.416 ms  min   524.816  max   537.513  ratio 13.1% of anchor
    page_index         p50   578.396 ms  min   577.827  max   599.834  ratio 14.2% of anchor
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184249..42184253 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210212..11210216 (2 distinct values)
  SPREAD   a session_save store_read_bytes 22927..22975 (7 distinct values)
  SPREAD   b attach_history store_read_bytes 42184251..42184253 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210215..11210216 (2 distinct values)
  SPREAD   b session_save store_read_bytes 22946..22979 (8 distinct values)
report   bench-results\loading-cold.json
mode cold, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### 세션 로딩 (WARM)

```text
binary   D:\a\agent\agent\bench-bin\examples\loading_probe.exe (6cb0c264e226)
stages   D:\a\agent\agent\bench-bin\examples\resume_stage_probe.exe (aa178149eb6c, 0 cold pass(es) per session fixture)
mode     warm
protocol WARM: one HOME per side, --warmup discarded runs per side, then measured runs alternate between the two sides on those same HOMEs.
fixtures C:\Users\RUNNER~1\AppData\Local\Temp\horon-loading-bench-9hltqr7g
fixture events=0 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    0.101/0.099    p95    0.114/0.107    delta  -0.001 ms  control-no-change
  skill_request_overlay p50    0.770/0.750    p95    0.857/0.934    delta  -0.021 ms  control-no-change
  mcp_config_load   p50    0.937/0.921    p95    1.023/0.947    delta  -0.016 ms  control-no-change
  attach_history    p50    0.001/0.002    p95    0.002/0.002    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.001    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.001    delta  +0.000 ms  control-no-change
  session_save      p50   35.250/35.481   p95   78.387/45.242   delta  +0.231 ms  control-no-change
  total store_read_bytes p50 23638  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition not_applicable: this fixture has no session log
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [22901, 22941, 22947, 22957, 23637, 23640, 23642, 24337, 24351, 24359]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [22899, 22949, 22958, 22961, 23603, 23635, 23636, 24343, 24348, 24355]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 22901..24359 (10 distinct values)
  SPREAD   b session_save store_read_bytes 22899..24355 (10 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50   14.987/14.834   p95   20.135/17.201   delta  -0.153 ms  control-no-change
  skill_request_overlay p50   38.277/38.849   p95   39.911/42.545   delta  +0.572 ms  control-no-change
  mcp_config_load   p50    0.955/0.977    p95    1.095/1.057    delta  +0.022 ms  control-no-change
  attach_history    p50    0.002/0.002    p95    0.009/0.007    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  session_save      p50  112.103/114.032  p95  203.740/249.008  delta  +1.929 ms  control-no-change
  total store_read_bytes p50 23268  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition not_applicable: this fixture has no session log
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [22907, 22950, 22952, 22953, 23584, 23612, 23624, 23643, 24334]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [22947, 22951, 22955, 22961, 23616, 23634, 23648, 24343, 24353, 24356]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 22907..24334 (9 distinct values)
  SPREAD   b session_save store_read_bytes 22947..24356 (10 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.100/0.105    p95    0.123/0.139    delta  +0.005 ms  control-no-change
  skill_request_overlay p50    0.795/0.745    p95    1.060/0.821    delta  -0.050 ms  control-no-change
  mcp_config_load   p50    0.893/0.914    p95    1.305/1.043    delta  +0.021 ms  control-no-change
  attach_history    p50  199.988/198.835  p95  215.488/205.023  delta  -1.154 ms  control-no-change
  resume_ready      p50  153.178/150.387  p95  163.393/155.892  delta  -2.792 ms  control-no-change
  provider_dispatch p50    1.621/1.606    p95    2.353/2.683    delta  -0.015 ms  control-no-change
  session_save      p50   35.763/72.877   p95  237.795/272.882  delta +37.114 ms  control-drift
  total store_read_bytes p50 2841122  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition skipped: --stage-probe-samples is 0
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23651, 23656, 24338, 24367, 25041, 25044, 25753, 25772, 26431, 26463]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23626, 23636, 24349, 24359, 25044, 25045, 25743, 25746, 26441, 26466]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23651..26463 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23626..26466 (10 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50   15.273/15.300   p95   15.531/15.575   delta  +0.027 ms  control-no-change
  skill_request_overlay p50   38.387/38.613   p95   40.479/41.143   delta  +0.226 ms  control-no-change
  mcp_config_load   p50    0.974/0.975    p95    1.046/1.053    delta  +0.001 ms  control-no-change
  attach_history    p50  201.367/200.596  p95  205.920/203.608  delta  -0.771 ms  control-no-change
  resume_ready      p50  153.939/153.681  p95  159.249/158.078  delta  -0.258 ms  control-no-change
  provider_dispatch p50    1.583/1.550    p95    1.724/1.670    delta  -0.032 ms  control-no-change
  session_save      p50   37.306/38.895   p95   80.196/65.659   delta  +1.589 ms  control-no-change
  total store_read_bytes p50 2841124  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition skipped: --stage-probe-samples is 0
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23659, 23671, 24342, 24346, 25043, 25045, 25736, 25754, 26453, 26471]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23666, 23667, 24337, 24346, 25050, 25057, 25713, 25754, 26421, 26434]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23659..26471 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23666..26434 (10 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.110/0.097    p95    0.146/0.147    delta  -0.013 ms  control-no-change
  skill_request_overlay p50    0.769/0.750    p95    0.858/0.878    delta  -0.019 ms  control-no-change
  mcp_config_load   p50    0.945/0.923    p95    1.005/1.016    delta  -0.022 ms  control-no-change
  attach_history    p50 2095.247/2099.515 p95 2115.283/2134.536 delta  +4.268 ms  control-no-change
  resume_ready      p50 1574.441/1568.713 p95 1596.460/1594.178 delta  -5.728 ms  control-no-change
  provider_dispatch p50   17.124/18.783   p95   18.888/21.260   delta  +1.659 ms  control-no-change
  session_save      p50   34.874/35.903   p95  191.836/147.915  delta  +1.029 ms  control-no-change
  total store_read_bytes p50 28640162  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition skipped: --stage-probe-samples is 0
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23618, 23674, 24369, 24377, 25061, 25070, 25725, 25735, 26460, 26468]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23629, 23658, 24358, 24377, 25051, 25068, 25743, 25758, 26445, 26474]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23618..26468 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23629..26474 (10 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50   14.518/14.195   p95   15.997/16.317   delta  -0.323 ms  control-no-change
  skill_request_overlay p50   37.412/36.999   p95   41.733/38.738   delta  -0.413 ms  control-no-change
  mcp_config_load   p50    1.006/0.975    p95    1.182/1.008    delta  -0.031 ms  control-no-change
  attach_history    p50 2097.239/2092.690 p95 2130.809/2125.258 delta  -4.548 ms  control-no-change
  resume_ready      p50 1560.724/1556.405 p95 1581.445/1575.623 delta  -4.319 ms  control-no-change
  provider_dispatch p50   17.107/16.785   p95   18.657/18.447   delta  -0.321 ms  control-no-change
  session_save      p50   34.670/33.104   p95   49.411/50.483   delta  -1.566 ms  control-no-change
  total store_read_bytes p50 28640140  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition skipped: --stage-probe-samples is 0
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23658, 23663, 24344, 24358, 25041, 25045, 25752, 25764, 26442, 26451]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23622, 23644, 24341, 24385, 25036, 25051, 25719, 25738, 26412, 26420]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23658..26451 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23622..26420 (10 distinct values)
report   bench-results\loading-warm.json
mode warm, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### Tool latency

| Tool case | median | p95 | min | max | samples |
|---|---|---|---|---|---|
| read (tiny file) | 2.7 ms | 2.7 ms | 2.2 ms | 2.7 ms | 5 |
| read (1000 lines) | 2.7 ms | 2.7 ms | 2.2 ms | 2.7 ms | 5 |
| read (10 lines @ offset) | 2.7 ms | 2.7 ms | 2.2 ms | 2.7 ms | 5 |
| read (repo Cargo.toml) | 2.7 ms | 2.7 ms | 2.2 ms | 2.7 ms | 5 |
| write (small) | 13.0 ms | 22.8 ms | 10.4 ms | 22.8 ms | 5 |
| write (10KB) | 12.5 ms | 12.6 ms | 10.5 ms | 12.6 ms | 5 |
| edit (simple replace) | 12.5 ms | 17.7 ms | 9.9 ms | 17.7 ms | 5 |
| ls (scripts dir) | 2.7 ms | 2.7 ms | 2.2 ms | 2.7 ms | 5 |
| agentgrep (grep in tree) | 7.4 ms | 8.0 ms | 5.3 ms | 8.0 ms | 5 |
| agentgrep (find **/*.rs) | 5.3 ms | 5.3 ms | 4.3 ms | 5.3 ms | 5 |
| bash (true) | 10.5 ms | 10.5 ms | 9.9 ms | 10.5 ms | 5 |
| bash (echo hello) | 10.5 ms | 10.5 ms | 9.7 ms | 10.5 ms | 5 |
| bash (ls fixture tree) | 12.5 ms | 13.0 ms | 12.0 ms | 13.0 ms | 5 |
| invalid tool (dispatch only) | 2.7 ms | 2.7 ms | 2.7 ms | 2.7 ms | 5 |

#### Tool CPU per call

| Tool case | daemon cpu median | min | max | child cpu median | voluntary switches | involuntary switches | windows × calls | source |
|---|---|---|---|---|---|---|---|---|
| read (tiny file) | — | — | — | — | — | — | 0 | — |
| read (1000 lines) | — | — | — | — | — | — | 0 | — |
| read (10 lines @ offset) | — | — | — | — | — | — | 0 | — |
| read (repo Cargo.toml) | — | — | — | — | — | — | 0 | — |
| write (small) | — | — | — | — | — | — | 0 | — |
| write (10KB) | — | — | — | — | — | — | 0 | — |
| edit (simple replace) | — | — | — | — | — | — | 0 | — |
| ls (scripts dir) | — | — | — | — | — | — | 0 | — |
| agentgrep (grep in tree) | — | — | — | — | — | — | 0 | — |
| agentgrep (find **/*.rs) | — | — | — | — | — | — | 0 | — |
| bash (true) | — | — | — | — | — | — | 0 | — |
| bash (echo hello) | — | — | — | — | — | — | 0 | — |
| bash (ls fixture tree) | — | — | — | — | — | — | 0 | — |
| invalid tool (dispatch only) | — | — | — | — | — | — | 0 | — |

CPU unmeasured: no /proc CPU counters for the daemon on this system

#### Reload storm

1 resume(s) per level; each level resumes sessions no other level has opened, from a freshly started daemon.

| Concurrent resumes | history median | p95 | max | storm wall | failures |
|---|---|---|---|---|---|
| 1 | 152.9 ms | 152.9 ms | 152.9 ms | 153.0 ms | 0 |
| 10 | 504.0 ms | 507.9 ms | 507.9 ms | 508.3 ms | 0 |
| 40 | 1.42 s | 1.73 s | 1.73 s | 1.73 s | 0 |

#### Compile loop

| Checkpoint | Touched | median | min | max | samples |
|---|---|---|---|---|---|
| warm_noop | — | 607.8 ms | 607.8 ms | 607.8 ms | 1 |
| leaf_touch | src\main.rs | 747.3 ms | 747.3 ms | 747.3 ms | 1 |
| core_touch | crates\app-core\src\lib.rs | 23.57 s | 23.57 s | 23.57 s | 1 |
| build_meta_touch | crates\build-meta\src\lib.rs | 34.13 s | 34.13 s | 34.13 s | 1 |

## TUI (`horon-tui`)

대화형 클라이언트는 별도 바이너리이고 따로 잽니다. 이 수치는 `tui` 묶음에서 나오며 위의 헤드리스 표와 비교하지 않습니다. `first_frame_ms` 와 `direct_first_frame_ms` 는 서로 다른 두 프로그램을 각각 띄운 값이고, 핸드오프는 그 둘의 차이입니다.

![TUI (`horon-tui`)](charts/tui-01-latency.ko.svg)

![TUI (`horon-tui`)](charts/tui-02-memory.ko.svg)

### Linux

#### TUI (`horon-tui`)

Measured version: the interactive client `horon-tui`. The headless `horon-agent` numbers are in the suites above and are never compared with these: the two binaries are timed against themselves.

| Metric | Median | Min–max | Samples |
| --- | --- | --- | --- |
| First frame (bare `horon-agent`) | 27.7 ms | 26.4 ms–43.3 ms | 5 |
| First frame (direct `horon-tui`) | 27.4 ms | 26.9 ms–28.9 ms | 5 |
| TTY handoff cost | 0.5 ms | -1.9 ms–16.4 ms | 5 |
| Key press to echo (p95) | 0.2 ms | 0.1 ms–0.5 ms | 50 |
| Idle RSS after 5 s | 5.9 MB | 5.8 MB–6.0 MB | 5 |
| RSS after one turn | 6.0 MB | 6.0 MB–6.1 MB | 5 |
| Frame draw time (p50) | 0.8 ms | 0.8 ms–0.8 ms | 5 |
| Frame draw time (p95) | 0.8 ms | 0.8 ms–1.2 ms | 5 |
| Install size (`horon-tui`) | 6.4 MB | — | 1 |

Handoff: median first-frame time of a bare `horon-agent` launch (no arguments, on a PTY) minus the median first-frame time of a direct `horon-tui` launch against an already-running daemon, both on the same machine and build. The bare path also starts the daemon, so the number covers that startup, the sibling lookup and the `exec`; it is the extra cost of the handoff over launching the client itself.

Render bench: 200 frames on a 120×40 terminal.

Scroll bench (a separate measurement, not the frame cells above): 120 one-row scroll steps and 20 no-input redraws over a 2,081-row transcript (72 items, 148.4 KB of tool output, expanded) on a 120×40 terminal. Every scroll step moved the expanded output.

| Scroll metric | Median | Min–max | Samples |
| --- | --- | --- | --- |
| Held frame draw (p50) | 0.4 ms | 0.4 ms–0.4 ms | 5 |
| Held frame draw (p95) | 0.4 ms | 0.4 ms–0.4 ms | 5 |
| Scroll key handling (p50) | 0.2 µs | 0.1 µs–0.2 µs | 5 |
| Scroll key handling (p95) | 0.3 µs | 0.3 µs–0.4 µs | 5 |
| First held frame after scrolling | 0.5 ms | 0.5 ms–0.5 ms | 5 |
| Key dispatch through completed draw (p50) | 0.4 ms | 0.4 ms–0.4 ms | 5 |
| Key dispatch through completed draw (p95) | 0.4 ms | 0.4 ms–0.4 ms | 5 |
| First key dispatch through completed draw | 0.5 ms | 0.5 ms–0.5 ms | 5 |
| Held frame redraw without input (p50) | 0.4 ms | 0.4 ms–0.4 ms | 5 |
| Held frame redraw without input (p95) | 0.4 ms | 0.4 ms–0.4 ms | 5 |

### macOS

#### TUI (`horon-tui`)

Measured version: the interactive client `horon-tui`. The headless `horon-agent` numbers are in the suites above and are never compared with these: the two binaries are timed against themselves.

| Metric | Median | Min–max | Samples |
| --- | --- | --- | --- |
| First frame (bare `horon-agent`) | 60.8 ms | 45.1 ms–262.5 ms | 5 |
| First frame (direct `horon-tui`) | 54.0 ms | 39.5 ms–77.4 ms | 5 |
| TTY handoff cost | 6.8 ms | -32.3 ms–211.5 ms | 5 |
| Key press to echo (p95) | 0.2 ms | 0.2 ms–0.8 ms | 50 |
| Idle RSS after 5 s | 9.5 MB | 9.4 MB–9.6 MB | 5 |
| RSS after one turn | 9.7 MB | 9.7 MB–9.8 MB | 5 |
| Frame draw time (p50) | 0.8 ms | 0.8 ms–0.8 ms | 5 |
| Frame draw time (p95) | 1.0 ms | 0.9 ms–2.0 ms | 5 |
| Install size (`horon-tui`) | 5.4 MB | — | 1 |

Handoff: median first-frame time of a bare `horon-agent` launch (no arguments, on a PTY) minus the median first-frame time of a direct `horon-tui` launch against an already-running daemon, both on the same machine and build. The bare path also starts the daemon, so the number covers that startup, the sibling lookup and the `exec`; it is the extra cost of the handoff over launching the client itself.

Render bench: 200 frames on a 120×40 terminal.

Scroll bench (a separate measurement, not the frame cells above): 120 one-row scroll steps and 20 no-input redraws over a 2,081-row transcript (72 items, 148.4 KB of tool output, expanded) on a 120×40 terminal. Every scroll step moved the expanded output.

| Scroll metric | Median | Min–max | Samples |
| --- | --- | --- | --- |
| Held frame draw (p50) | 0.4 ms | 0.4 ms–0.4 ms | 5 |
| Held frame draw (p95) | 0.4 ms | 0.4 ms–0.6 ms | 5 |
| Scroll key handling (p50) | 0.2 µs | 0.2 µs–0.2 µs | 5 |
| Scroll key handling (p95) | 0.4 µs | 0.4 µs–1.5 µs | 5 |
| First held frame after scrolling | 0.7 ms | 0.6 ms–1.1 ms | 5 |
| Key dispatch through completed draw (p50) | 0.4 ms | 0.4 ms–0.4 ms | 5 |
| Key dispatch through completed draw (p95) | 0.4 ms | 0.4 ms–0.6 ms | 5 |
| First key dispatch through completed draw | 0.7 ms | 0.6 ms–1.1 ms | 5 |
| Held frame redraw without input (p50) | 0.4 ms | 0.4 ms–0.4 ms | 5 |
| Held frame redraw without input (p95) | 0.4 ms | 0.4 ms–0.4 ms | 5 |

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
| Frame draw time (p50) | 1.5 ms | 1.5 ms–1.5 ms | 5 |
| Frame draw time (p95) | 2.3 ms | 1.5 ms–2.3 ms | 5 |
| Install size (`horon-tui`) | 5.3 MB | — | 1 |

Handoff: median first-frame time of a bare `horon-agent` launch (no arguments, on a PTY) minus the median first-frame time of a direct `horon-tui` launch against an already-running daemon, both on the same machine and build. The bare path also starts the daemon, so the number covers that startup, the sibling lookup and the `exec`; it is the extra cost of the handoff over launching the client itself.

Render bench: 200 frames on a 120×40 terminal.

Scroll bench (a separate measurement, not the frame cells above): 120 one-row scroll steps and 20 no-input redraws over a 2,081-row transcript (72 items, 148.4 KB of tool output, expanded) on a 120×40 terminal. Every scroll step moved the expanded output.

| Scroll metric | Median | Min–max | Samples |
| --- | --- | --- | --- |
| Held frame draw (p50) | 0.4 ms | 0.4 ms–0.4 ms | 5 |
| Held frame draw (p95) | 0.4 ms | 0.4 ms–0.5 ms | 5 |
| Scroll key handling (p50) | 0.2 µs | 0.1 µs–0.2 µs | 5 |
| Scroll key handling (p95) | 0.3 µs | 0.3 µs–0.4 µs | 5 |
| First held frame after scrolling | 0.7 ms | 0.6 ms–0.7 ms | 5 |
| Key dispatch through completed draw (p50) | 0.4 ms | 0.4 ms–0.4 ms | 5 |
| Key dispatch through completed draw (p95) | 0.4 ms | 0.4 ms–0.5 ms | 5 |
| First key dispatch through completed draw | 0.7 ms | 0.6 ms–0.7 ms | 5 |
| Held frame redraw without input (p50) | 0.4 ms | 0.4 ms–0.4 ms | 5 |
| Held frame redraw without input (p95) | 0.4 ms | 0.4 ms–0.7 ms | 5 |

Skipped: pty unavailable on Windows

<!-- bench-auto:end -->
