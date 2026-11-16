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
| 17. CI 구조 게이트와 리포트 템플릿 | `[~]` | 게이트는 실제로 돈다(경계 38개 `scripts/perf/structural_budget.json`, `EXIT=0`, 위반 시 `EXIT=1`, 측정 누락 시 `EXIT=2`, 계약 테스트 77개 `scripts/perf/test_check_structural.py`). 계획의 두 조건이 남았다. 프레임 계수 카운터가 없고, "B0 에서 통과"는 구조적으로 불가능하다(`STRUCTURAL_PERF` 줄을 인쇄하는 코드가 B0 뒤에 들어왔다). 이제 게이트는 `frame` 구획에서 두 리더의 프레임 상한 자체를 단언하고, 프레임 개수는 여전히 읽지 않는다 |

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

구조 게이트는 이제 이 값들을 모두 후 값으로 묶습니다(`prefix_tools_bytes_max`는 9751에서 9151로 낮춤).

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
- 계획 목표는 카탈로그 9,000 B 이하와 core one-shot `tools` 700 B 이상 감소였습니다. 계약 시험이 요구해서 `intent` 설명에 "shown in the UI"를 남겼고, 다른 시험이 고정한 `bash`의 `rg`가 숨김·무시 파일을 건너뛴다는 안내도 남겨서 카탈로그는 9,151 B, 감소는 563 B입니다.
- `[features] foreign_rules`(기본값 꺼짐)는 여전히 요청마다 규칙 파일을 읽고, 프롬프트 원천 읽기 수에 잡히지 않습니다.
- Windows 벤치 빌드는 기준선과 후보 모두에서 같은 `linker stdout` 줄 두 개를 출력합니다. 이 변경이 아니라 벤치의 Windows release 링크에서 나오며, 이 브랜치의 ci.yml 실행에는 경고가 없습니다.

## 자동 측정: `main` 최신 측정 커밋

`main` 에 올라간 커밋이 CI 와 구조 성능 게이트를 모두 통과하고 직전 측정 이후 에이전트 코드(`src`, `crates`, `Cargo.toml`, `Cargo.lock`, `.cargo`, `rust-toolchain.toml`, `scripts/perf`)가 바뀌었으면, `.github/workflows/benchmark-docs.yml` 이 Linux·macOS·Windows에서 전체 벤치마크 묶음을 OS마다 따로 돌리고 아래 구간을 `main` 과 `dev` 에서 함께 다시 씁니다. OS마다 절이 하나씩 있습니다. 측정 방법과 묶음별 뜻은 [`BENCHMARK.ko.md`](BENCHMARK.ko.md) 의 "자동 측정"에 있습니다.

<!-- bench-auto:start commit=ef9339c47712ca7a4f1963e06f1b3e73e10dc850 -->

## 헤드리스 (`horon-agent`)

이 구간은 `.github/workflows/benchmark-docs.yml` 이 자동으로 다시 씁니다. 손으로 고치지 않습니다.

- 커밋: `ef9339c47712ca7a4f1963e06f1b3e73e10dc850`
- 측정 시각: 2026-11-16 12:13 UTC
- 실행: <https://github.com/horon-ai/agent/actions/runs/43856404791> (원본 JSON 은 OS별 `benchmark-<os>-<run_id>` 아티팩트)

OS마다 다른 GitHub 호스티드 러너에서 따로 잽니다. 기계가 다르므로 OS끼리 절대값을 비교하지 않고, 같은 OS 안의 상대 비교(horon 대비 배수)와 커밋 사이의 추세만 읽습니다.

| OS | 러너 | 바이너리 | 상태 |
| --- | --- | --- | --- |
| Linux | `ubuntu-24.04` | `horon-agent v0.1.0-dev (ef9339c)` | 성공 |
| macOS | `macos-latest` | `horon-agent v0.1.0-dev (ef9339c)` | 성공 |
| Windows | `windows-latest` | `horon-agent v0.1.0-dev (ef9339c)` | 성공 |

### Linux

#### Coding agent comparison (headless, mock model)

Host: linux/x86_64, 4 CPUs, Python 3.12.16. Baseline: **horon**. Samples per one-shot, skill and MCP cell: 5.

| Tool | Version | `--version` (median) | Install size |
|---|---|---|---|
| horon | horon-agent v0.1.0-dev (ef9339c) | 5.2 ms | 115.7 MB |
| jcode | jcode v0.99.1 (7206efd0c) | 12.6 ms | 163.4 MB |
| Claude Code | 2.1.317 (Claude Code) | 12.9 ms | 632.2 MB |
| Codex CLI | codex-cli 0.166.0 | 48.4 ms | 435.0 MB |
| OpenCode | 1.19.5 | 496.3 ms | 613.4 MB |
| pi | 1.5.0 | 256.1 ms | 108.9 MB |
| oh-my-pi (omp) | omp/18.18.0 | 704.3 ms | 413.1 MB |
| oh-my-openagent (omo) | omo 5.1.64 (engine: senpi 2026.11.5-1) | 129.4 ms | 520.6 MB |

**One-shot headless prompt** (includes startup, one `ls` tool call, final answer)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Tool round-trip | fsync calls | request bytes | Note |
|---|---|---|---|---|---|---|---|---|---|
| horon | 44.2 ms | 43.9 ms–91.2 ms | baseline | 39.9 MB | baseline | yes | 10 | 3,731 / 2,390 (1 tools) | — |
| jcode | 153.5 ms | 151.1 ms–206.3 ms | 3.47× more | 39.9 MB | same | yes | 5 | 3,245 / 1,433 (1 tools) | — |
| Claude Code | 584.4 ms | 575.8 ms–617.4 ms | 13.21× more | 206.0 MB | 5.17× more | yes | — | 45,782 / 29,653 (12 tools) | — |
| Codex CLI | 526.3 ms | 509.9 ms–572.9 ms | 11.89× more | 190.4 MB | 4.77× more | yes | 6 | 39,432 / 17,271 (9 tools) | — |
| OpenCode | 3.19 s | 2.91 s–3.35 s | 72.20× more | 579.4 MB | 14.53× more | yes | 4 | 2,659 / 0 (0 tools) | — |
| pi | 446.7 ms | 430.2 ms–459.4 ms | 10.10× more | 114.2 MB | 2.86× more | yes | — | 5,502 / 2,841 (4 tools) | — |
| oh-my-pi (omp) | 2.18 s | 1.88 s–2.23 s | 49.22× more | 391.6 MB | 9.82× more | yes | 71 | 29,860 / 18,533 (11 tools) | — |
| oh-my-openagent (omo) | 6.59 s | 6.41 s–7.02 s | 148.99× more | 665.2 MB | 16.68× more | yes | 36 | 27,452 / 2,424 (2 tools) | — |

`request bytes` is the first model request of the first run as the mock server received it: JSON body bytes / tool catalog bytes (tools offered).

**Core-profile one-shot** (the one-shot turn without `--tools`, so the request carries the default core surface: the eager tools, `toolbox` and the listed-tool index)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | Note | request bytes |
|---|---|---|---|---|---|---|---|---|---|
| horon | 49.0 ms | 48.3 ms–49.4 ms | baseline | 41.9 MB | baseline | — | yes | — | 11,254 / 9,255 (8 tools) |
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
| horon | 42.7 ms | 41.8 ms–43.1 ms | baseline | 16.7 MB | baseline | skill-uri | yes | — |
| jcode | 68.2 ms | 65.0 ms–71.3 ms | 1.60× more | 37.8 MB | 2.26× more | skill-tool | yes | — |
| Claude Code | 515.2 ms | 502.7 ms–535.9 ms | 12.06× more | 207.5 MB | 12.40× more | skill-tool | yes | — |
| Codex CLI | 608.4 ms | 500.9 ms–619.5 ms | 14.24× more | 191.1 MB | 11.42× more | file-read | yes | — |
| OpenCode | 3.48 s | 3.38 s–3.62 s | 81.36× more | 594.6 MB | 35.54× more | skill-tool | yes | — |
| pi | 552.5 ms | 545.0 ms–555.0 ms | 12.93× more | 125.0 MB | 7.47× more | file-read | yes | — |
| oh-my-pi (omp) | 2.20 s | 1.94 s–2.38 s | 51.49× more | 375.5 MB | 22.44× more | file-read | yes | — |
| oh-my-openagent (omo) | 7.29 s | 7.09 s–7.42 s | 170.61× more | 664.0 MB | 39.69× more | file-read | yes | — |

**MCP tool call** (process start to final answer, including starting the fixture stdio MCP server and one `bench_echo` call)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | MCP ready | Note |
|---|---|---|---|---|---|---|---|---|---|
| horon | 67.8 ms | 66.8 ms–71.2 ms | baseline | 43.9 MB | baseline | mcp-call | yes | 49.2 ms | — |
| jcode | 193.8 ms | 192.9 ms–196.9 ms | 2.86× more | 55.8 MB | 1.27× more | mcp-action | yes | 155.2 ms | — |
| Claude Code | 625.9 ms | 613.5 ms–642.3 ms | 9.22× more | 225.1 MB | 5.12× more | mcp-tool | yes | 464.9 ms | — |
| Codex CLI | 517.4 ms | 485.3 ms–572.7 ms | 7.62× more | 194.9 MB | 4.44× more | mcp-tool | yes | 360.6 ms | — |
| OpenCode | 3.23 s | 3.15 s–3.39 s | 47.67× more | 566.7 MB | 12.89× more | mcp-tool | yes | 2.93 s | — |
| pi | 616.3 ms | 595.2 ms–626.5 ms | 9.08× more | 138.2 MB | 3.14× more | mcp-tool | yes | 542.1 ms | — |
| oh-my-pi (omp) | 2.29 s | 2.28 s–2.60 s | 33.78× more | 408.3 MB | 9.29× more | mcp-route | yes | 1.88 s | — |
| oh-my-openagent (omo) | — | — | — | — | — | mcp-tool | no | — | failed: the fixture sentinel never came back |

**Session memory** (idle after 3 turn(s) per session; sizes are process-tree sums, RSS; PSS also recorded on Linux). "Each additional" is the least-squares slope across the measured session counts; 0 B means growth stayed below RSS sampling noise. Threads and descriptors come from the daemon's own `server:memory` after the turns.

| Tool | First session | Each additional | vs horon (additional) | Threads/session | FDs/session | First (post-turn) | Each additional (post-turn) |
|---|---|---|---|---|---|---|---|
| horon | 36.9 MB | 210.7 KB | baseline | 0.70 | 1.00 | 44.4 MB | 495.1 KB |
| jcode | 43.1 MB | 25.0 KB | 8.45× less | 0.11 | — | 46.6 MB | 191.4 KB |
| Claude Code | 149.6 MB | 147.3 MB | 715.62× more | — | — | 185.3 MB | 180.7 MB |
| Codex CLI | 129.0 MB | 132.5 MB | 643.74× more | — | — | 162.3 MB | 163.5 MB |
| OpenCode | 243.4 MB | 761.8 KB | 3.62× more | — | — | 461.6 MB | 8.9 MB |
| pi | 110.0 MB | 107.2 MB | 520.87× more | — | — | 132.3 MB | 128.1 MB |
| oh-my-pi (omp) | 338.2 MB | 60.6 MB | 294.64× more | — | — | 368.5 MB | 358.6 MB |
| oh-my-openagent (omo) | 377.7 MB | 25.0 MB | 121.50× more | — | — | 607.4 MB | 575.5 MB |

Threads/descriptors unavailable: jcode (server:memory did not report fd_count)

**Post-turn tree RSS per session count**

| Tool | 1 session(s) | 5 session(s) | 10 session(s) |
|---|---|---|---|
| horon | 44.4 MB | 47.1 MB | 48.8 MB |
| jcode | 46.6 MB | 47.6 MB | 48.3 MB |
| Claude Code | 185.3 MB | 911.3 MB | 1.8 GB |
| Codex CLI | 162.3 MB | 812.4 MB | 1.6 GB |
| OpenCode | 461.6 MB | 523.5 MB | 543.6 MB |
| pi | 132.3 MB | 623.9 MB | 1.3 GB |
| oh-my-pi (omp) | 368.5 MB | 1.8 GB | 3.5 GB |
| oh-my-openagent (omo) | 607.4 MB | 2.9 GB | 5.7 GB |

#### 기동

```text
Benchmarking: /home/runner/work/agent/agent/bench-bin/horon-agent
============================================================

--help (binary load):
  Min:    2.73 ms
  Max:    3.17 ms
  Mean:   2.87 ms
  Median: 2.84 ms
  Stdev:  0.13 ms

--version:
  Min:    2.56 ms
  Max:    2.86 ms
  Mean:   2.67 ms
  Median: 2.61 ms
  Stdev:  0.11 ms

Measuring isolated server startup (10 runs)...

Server ready (isolated socket connectable):
  Min:    20.46 ms
  Max:    20.69 ms
  Mean:   20.53 ms
  Median: 20.49 ms
  Stdev:  0.09 ms

Server startup stages (one extra run with HORON_AGENT_STARTUP_PROFILE=1, not timed):
  stage             at ms  delta ms   share
  panic_hook          0.0       0.0    0.2%
  logging_init        0.2       0.2   20.3%
  nofile_limit        0.4       0.2   24.9%
  perm_harden         0.5       0.0    0.8%
  args_parse          0.5       0.0    0.1%
  config_loaded       0.6       0.1   12.8%
  server_new          0.9       0.3   34.2%
  socket_bound        1.0       0.1    6.6%

Measuring harness bridge readiness (10 runs)...

Bridge stage: api socket connectable:
  Min:    20.55 ms
  Max:    20.79 ms
  Mean:   20.68 ms
  Median: 20.66 ms
  Stdev:  0.07 ms

Bridge stage: hello_ok:
  Min:    21.15 ms
  Max:    21.66 ms
  Mean:   21.32 ms
  Median: 21.25 ms
  Stdev:  0.17 ms

Bridge stage: no-op reply:
  Min:    21.49 ms
  Max:    22.08 ms
  Mean:   21.70 ms
  Median: 21.62 ms
  Stdev:  0.19 ms

Bridge total (socket -> no-op):
  Min:    21.49 ms
  Max:    22.08 ms
  Mean:   21.70 ms
  Median: 21.62 ms
  Stdev:  0.19 ms

============================================================
Summary:
  Binary load median:      ~2.8 ms
  Server ready median:     ~20.5 ms
  Bridge total median:     ~21.6 ms
```

#### 세션 로딩 (COLD)

```text
binary   /home/runner/work/agent/agent/bench-bin/examples/loading_probe (072398c7b9d4)
mode     cold
protocol COLD: every measured run gets its own freshly copied HOME; no derived cache from an earlier run exists, so each sample is a first load of the fixture.
fixtures /tmp/horon-loading-bench-867b6mbo
note     --warmup 3 is ignored in cold mode: every sample gets a fresh HOME
fixture events=0 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    0.006/0.006    p95    0.007/0.007    delta  -0.000 ms  control-no-change
  skill_request_overlay p50    0.038/0.039    p95    0.048/0.039    delta  +0.000 ms  control-no-change
  mcp_config_load   p50    0.053/0.053    p95    0.054/0.053    delta  +0.000 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  session_save      p50    4.545/4.550    p95    6.195/4.742    delta  +0.005 ms  control-no-change
  total store_read_bytes p50 23078  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 23064..23088 (6 distinct values)
  SPREAD   b session_save store_read_bytes 23060..23084 (6 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    1.200/1.186    p95    1.218/1.205    delta  -0.014 ms  control-no-change
  skill_request_overlay p50    1.829/1.832    p95    1.854/1.851    delta  +0.003 ms  control-no-change
  mcp_config_load   p50    0.058/0.058    p95    0.069/0.068    delta  +0.000 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  session_save      p50    5.537/5.589    p95    6.248/6.812    delta  +0.051 ms  control-no-change
  total store_read_bytes p50 23072  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 23056..23088 (6 distinct values)
  SPREAD   b session_save store_read_bytes 23052..23092 (6 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.005/0.005    p95    0.006/0.007    delta  -0.000 ms  control-no-change
  skill_request_overlay p50    0.038/0.038    p95    0.039/0.055    delta  +0.000 ms  control-no-change
  mcp_config_load   p50    0.053/0.054    p95    0.065/0.054    delta  +0.000 ms  control-no-change
  attach_history    p50  190.983/191.859  p95  194.346/193.067  delta  +0.876 ms  control-no-change
  resume_ready      p50   27.248/27.329   p95   27.378/27.622   delta  +0.081 ms  control-no-change
  provider_dispatch p50    0.914/0.919    p95    0.923/0.928    delta  +0.005 ms  control-no-change
  session_save      p50    4.126/4.173    p95    4.609/4.603    delta  +0.047 ms  control-no-change
  total store_read_bytes p50 5311409  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189191..4189195 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099132..1099136 (3 distinct values)
  SPREAD   a session_save store_read_bytes 23077..23086 (4 distinct values)
  SPREAD   b attach_history store_read_bytes 4189191..4189195 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099132..1099136 (2 distinct values)
  SPREAD   b session_save store_read_bytes 23062..23086 (6 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    1.189/1.186    p95    1.202/1.201    delta  -0.003 ms  control-no-change
  skill_request_overlay p50    1.841/1.835    p95    1.872/1.860    delta  -0.005 ms  control-no-change
  mcp_config_load   p50    0.058/0.057    p95    0.058/0.065    delta  -0.000 ms  control-no-change
  attach_history    p50  192.760/193.058  p95  194.397/193.848  delta  +0.298 ms  control-no-change
  resume_ready      p50   27.316/27.344   p95   27.488/27.399   delta  +0.027 ms  control-no-change
  provider_dispatch p50    0.913/0.915    p95    0.926/0.936    delta  +0.002 ms  control-no-change
  session_save      p50    4.246/4.236    p95    4.565/4.879    delta  -0.010 ms  control-no-change
  total store_read_bytes p50 5311406  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189191..4189195 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099132..1099136 (2 distinct values)
  SPREAD   a session_save store_read_bytes 23062..23084 (6 distinct values)
  SPREAD   b attach_history store_read_bytes 4189191..4189195 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099134..1099136 (3 distinct values)
  SPREAD   b session_save store_read_bytes 23070..23086 (6 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.005/0.006    p95    0.006/0.007    delta  +0.000 ms  control-no-change
  skill_request_overlay p50    0.038/0.038    p95    0.043/0.053    delta  +0.000 ms  control-no-change
  mcp_config_load   p50    0.054/0.054    p95    0.069/0.062    delta  +0.000 ms  control-no-change
  attach_history    p50 2355.594/2360.519 p95 2375.706/2433.601 delta  +4.925 ms  control-no-change
  resume_ready      p50  315.484/316.109  p95  321.533/319.420  delta  +0.624 ms  control-no-change
  provider_dispatch p50   10.594/10.606   p95   10.846/10.784   delta  +0.013 ms  control-no-change
  session_save      p50    4.765/4.747    p95    4.949/5.183    delta  -0.019 ms  control-no-change
  total store_read_bytes p50 53417616  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184289..42184293 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210242..11210246 (3 distinct values)
  SPREAD   a session_save store_read_bytes 23069..23089 (7 distinct values)
  SPREAD   b session_save store_read_bytes 23071..23089 (6 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    1.193/1.191    p95    1.212/1.213    delta  -0.002 ms  control-no-change
  skill_request_overlay p50    1.801/1.805    p95    1.835/1.828    delta  +0.004 ms  control-no-change
  mcp_config_load   p50    0.058/0.058    p95    0.063/0.061    delta  +0.000 ms  control-no-change
  attach_history    p50 2342.191/2341.531 p95 2386.219/2374.947 delta  -0.661 ms  control-no-change
  resume_ready      p50  312.115/314.809  p95  317.385/316.618  delta  +2.694 ms  control-no-change
  provider_dispatch p50   10.721/10.575   p95   10.901/10.867   delta  -0.146 ms  control-no-change
  session_save      p50    4.797/4.634    p95    4.950/4.804    delta  -0.162 ms  control-no-change
  total store_read_bytes p50 53417616  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184289..42184293 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210242..11210246 (4 distinct values)
  SPREAD   a session_save store_read_bytes 23071..23085 (7 distinct values)
  SPREAD   b attach_history store_read_bytes 42184291..42184293 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210245..11210246 (2 distinct values)
  SPREAD   b session_save store_read_bytes 23067..23089 (8 distinct values)
report   bench-results/loading-cold.json
mode cold, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### 세션 로딩 (WARM)

```text
binary   /home/runner/work/agent/agent/bench-bin/examples/loading_probe (072398c7b9d4)
mode     warm
protocol WARM: one HOME per side, --warmup discarded runs per side, then measured runs alternate between the two sides on those same HOMEs.
fixtures /tmp/horon-loading-bench-jrii4wd3
fixture events=0 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    0.013/0.013    p95    0.016/0.015    delta  +0.000 ms  control-no-change
  skill_request_overlay p50    0.093/0.094    p95    0.111/0.101    delta  +0.001 ms  control-no-change
  mcp_config_load   p50    0.126/0.128    p95    0.138/0.134    delta  +0.003 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.001/0.000    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  session_save      p50    6.941/7.005    p95    8.470/19.028   delta  +0.064 ms  control-no-change
  total store_read_bytes p50 23800  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23069, 23081, 23085, 23784, 23796, 23804, 24507, 24515, 24519]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23073, 23077, 23085, 23788, 23796, 23800, 24511, 24515, 24519, 24523]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23069..24519 (9 distinct values)
  SPREAD   b session_save store_read_bytes 23073..24523 (10 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    2.417/2.371    p95    2.520/2.493    delta  -0.046 ms  control-no-change
  skill_request_overlay p50    3.947/3.939    p95    4.039/4.262    delta  -0.008 ms  control-no-change
  mcp_config_load   p50    0.136/0.136    p95    0.143/0.151    delta  -0.000 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  session_save      p50   12.434/11.692   p95   15.829/14.881   delta  -0.742 ms  control-no-change
  total store_read_bytes p50 23795  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23069, 23071, 23077, 23079, 23792, 23798, 23804, 24507, 24509]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23077, 23081, 23085, 23792, 23800, 24507, 24513, 24515]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23069..24509 (9 distinct values)
  SPREAD   b session_save store_read_bytes 23077..24515 (8 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.013/0.013    p95    0.016/0.014    delta  -0.000 ms  control-no-change
  skill_request_overlay p50    0.093/0.094    p95    0.097/0.104    delta  +0.001 ms  control-no-change
  mcp_config_load   p50    0.124/0.126    p95    0.128/0.135    delta  +0.002 ms  control-no-change
  attach_history    p50   81.900/81.775   p95   82.454/83.287   delta  -0.125 ms  control-no-change
  resume_ready      p50   44.523/44.439   p95   45.096/45.152   delta  -0.084 ms  control-no-change
  provider_dispatch p50    1.057/1.051    p95    1.091/1.081    delta  -0.006 ms  control-no-change
  session_save      p50    7.079/7.097    p95   22.499/61.636   delta  +0.017 ms  control-no-change
  total store_read_bytes p50 2841370  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23799, 23801, 24510, 24512, 25229, 25231, 25950, 25960, 26661, 26667]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23787, 23801, 24516, 24522, 25235, 25239, 25241, 25940, 25948, 26663]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23799..26667 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23787..26663 (10 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    2.364/2.356    p95    2.470/2.437    delta  -0.008 ms  control-no-change
  skill_request_overlay p50    3.905/3.899    p95    3.969/4.138    delta  -0.006 ms  control-no-change
  mcp_config_load   p50    0.134/0.132    p95    0.143/0.138    delta  -0.002 ms  control-no-change
  attach_history    p50   80.934/81.062   p95   82.290/83.062   delta  +0.129 ms  control-no-change
  resume_ready      p50   44.531/44.356   p95   45.909/44.795   delta  -0.175 ms  control-no-change
  provider_dispatch p50    1.043/1.026    p95    1.090/1.055    delta  -0.016 ms  control-no-change
  session_save      p50    9.983/10.089   p95   71.395/60.583   delta  +0.106 ms  control-no-change
  total store_read_bytes p50 2841375  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23789, 23791, 24502, 24520, 25235, 25950, 25954, 26667, 26669]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23793, 24516, 24528, 25233, 25239, 25946, 25956, 26667, 26669]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23789..26669 (9 distinct values)
  SPREAD   b session_save store_read_bytes 23793..26669 (9 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.013/0.013    p95    0.016/0.016    delta  -0.000 ms  control-no-change
  skill_request_overlay p50    0.096/0.094    p95    0.123/0.100    delta  -0.002 ms  control-no-change
  mcp_config_load   p50    0.129/0.129    p95    0.157/0.154    delta  -0.000 ms  control-no-change
  attach_history    p50  954.994/952.427  p95  978.663/987.815  delta  -2.567 ms  control-no-change
  resume_ready      p50  507.897/518.368  p95  550.019/547.639  delta +10.470 ms  control-no-change
  provider_dispatch p50   12.937/12.827   p95   14.482/14.481   delta  -0.110 ms  control-no-change
  session_save      p50    6.922/6.953    p95  146.040/24.095   delta  +0.031 ms  control-no-change
  total store_read_bytes p50 28640394  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23794, 23797, 24511, 24514, 25237, 25238, 25944, 25957, 26651, 26671]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23796, 23802, 24509, 24511, 25238, 25246, 25941, 25953, 26678, 26680]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23794..26671 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23796..26680 (10 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    2.392/2.389    p95    2.482/2.515    delta  -0.002 ms  control-no-change
  skill_request_overlay p50    3.908/3.904    p95    3.976/4.115    delta  -0.003 ms  control-no-change
  mcp_config_load   p50    0.133/0.133    p95    0.143/0.143    delta  -0.001 ms  control-no-change
  attach_history    p50  936.624/941.852  p95  958.608/957.831  delta  +5.229 ms  control-no-change
  resume_ready      p50  513.764/511.729  p95  530.594/542.797  delta  -2.035 ms  control-no-change
  provider_dispatch p50   12.653/12.619   p95   13.871/13.533   delta  -0.035 ms  control-no-change
  session_save      p50   10.134/10.208   p95   10.969/21.516   delta  +0.074 ms  control-no-change
  total store_read_bytes p50 28640392  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23795, 23799, 24514, 24526, 25233, 25241, 25956, 25960, 26679]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23779, 23781, 24520, 24530, 25221, 25247, 25950, 25968, 26669, 26671]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23795..26679 (9 distinct values)
  SPREAD   b session_save store_read_bytes 23779..26671 (10 distinct values)
report   bench-results/loading-warm.json
mode warm, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### Tool latency

| Tool case | median | p95 | min | max | samples |
|---|---|---|---|---|---|
| read (tiny file) | 0.2 ms | 0.2 ms | 0.2 ms | 0.2 ms | 5 |
| read (1000 lines) | 0.3 ms | 0.3 ms | 0.3 ms | 0.3 ms | 5 |
| read (10 lines @ offset) | 0.3 ms | 0.3 ms | 0.2 ms | 0.3 ms | 5 |
| read (repo Cargo.toml) | 0.2 ms | 0.3 ms | 0.2 ms | 0.3 ms | 5 |
| write (small) | 1.0 ms | 1.1 ms | 1.0 ms | 1.1 ms | 5 |
| write (10KB) | 1.1 ms | 1.3 ms | 1.1 ms | 1.3 ms | 5 |
| edit (simple replace) | 1.0 ms | 1.0 ms | 1.0 ms | 1.0 ms | 5 |
| ls (scripts dir) | 0.6 ms | 0.6 ms | 0.5 ms | 0.6 ms | 5 |
| agentgrep (grep in tree) | 1.3 ms | 1.3 ms | 1.2 ms | 1.3 ms | 5 |
| agentgrep (find **/*.rs) | 1.1 ms | 1.2 ms | 1.1 ms | 1.2 ms | 5 |
| bash (true) | 1.6 ms | 1.7 ms | 1.5 ms | 1.7 ms | 5 |
| bash (echo hello) | 1.5 ms | 1.6 ms | 1.5 ms | 1.6 ms | 5 |
| bash (ls fixture tree) | 2.7 ms | 2.8 ms | 2.7 ms | 2.8 ms | 5 |
| invalid tool (dispatch only) | 0.1 ms | 0.1 ms | 0.1 ms | 0.1 ms | 5 |

#### Tool CPU per call

| Tool case | daemon cpu median | min | max | child cpu median | voluntary switches | involuntary switches | windows × calls | source |
|---|---|---|---|---|---|---|---|---|
| read (tiny file) | 0.175 ms | 0.165 ms | 0.183 ms | 0.000 ms | 5.98 | 0.00 | 5 × 50 | schedstat |
| read (1000 lines) | 0.254 ms | 0.248 ms | 0.263 ms | 0.000 ms | 6.00 | 0.00 | 5 × 50 | schedstat |
| read (10 lines @ offset) | 0.243 ms | 0.242 ms | 0.267 ms | 0.000 ms | 5.94 | 0.00 | 5 × 50 | schedstat |
| read (repo Cargo.toml) | 0.198 ms | 0.188 ms | 0.203 ms | 0.000 ms | 6.00 | 0.00 | 5 × 50 | schedstat |
| write (small) | 0.528 ms | 0.506 ms | 0.624 ms | 0.000 ms | 19.04 | 0.00 | 5 × 50 | schedstat |
| write (10KB) | 0.656 ms | 0.652 ms | 0.704 ms | 0.000 ms | 20.12 | 0.00 | 5 × 50 | schedstat |
| edit (simple replace) | 0.582 ms | 0.532 ms | 0.626 ms | 0.000 ms | 19.14 | 0.00 | 5 × 50 | schedstat |
| ls (scripts dir) | 0.501 ms | 0.498 ms | 0.505 ms | 0.000 ms | 6.00 | 0.00 | 5 × 50 | schedstat |
| agentgrep (grep in tree) | 0.858 ms | 0.856 ms | 0.863 ms | 0.000 ms | 10.96 | 0.08 | 5 × 50 | schedstat |
| agentgrep (find **/*.rs) | 1.103 ms | 1.094 ms | 1.129 ms | 0.000 ms | 6.00 | 0.00 | 5 × 50 | schedstat |
| bash (true) | 0.422 ms | 0.418 ms | 0.436 ms | 1.200 ms | 7.22 | 0.00 | 5 × 50 | schedstat |
| bash (echo hello) | 0.436 ms | 0.433 ms | 0.438 ms | 1.200 ms | 8.34 | 0.00 | 5 × 50 | schedstat |
| bash (ls fixture tree) | 0.482 ms | 0.478 ms | 0.485 ms | 2.200 ms | 8.52 | 0.00 | 5 × 50 | schedstat |
| invalid tool (dispatch only) | 0.044 ms | 0.040 ms | 0.071 ms | 0.000 ms | 3.00 | 0.00 | 5 × 50 | schedstat |

#### Reload storm

1 resume(s) per level; each level resumes sessions no other level has opened, from a freshly started daemon.

| Concurrent resumes | history median | p95 | max | storm wall | failures |
|---|---|---|---|---|---|
| 1 | 55.1 ms | 55.1 ms | 55.1 ms | 55.3 ms | 0 |
| 10 | 182.8 ms | 211.7 ms | 211.7 ms | 212.5 ms | 0 |
| 40 | 937.6 ms | 975.1 ms | 977.2 ms | 979.3 ms | 0 |

#### Compile loop

| Checkpoint | Touched | median | min | max | samples |
|---|---|---|---|---|---|
| warm_noop | — | 294.5 ms | 294.5 ms | 294.5 ms | 1 |
| leaf_touch | src/main.rs | 449.9 ms | 449.9 ms | 449.9 ms | 1 |
| core_touch | crates/app-core/src/lib.rs | 21.25 s | 21.25 s | 21.25 s | 1 |
| build_meta_touch | crates/build-meta/src/lib.rs | 30.34 s | 30.34 s | 30.34 s | 1 |

### macOS

#### Coding agent comparison (headless, mock model)

Host: darwin/arm64, 3 CPUs, Python 3.12.10. Baseline: **horon**. Samples per one-shot, skill and MCP cell: 5.

Skipped: syscalls (strace not available)

| Tool | Version | `--version` (median) | Install size |
|---|---|---|---|
| horon | horon-agent v0.1.0-dev (ef9339c) | 10.6 ms | 97.3 MB |
| jcode | jcode v0.99.1 (7206efd0c) | 12.1 ms | 136.3 MB |
| Claude Code | 2.1.317 (Claude Code) | 26.8 ms | 596.8 MB |
| Codex CLI | codex-cli 0.166.0 | 67.7 ms | 346.5 MB |
| OpenCode | 1.19.5 | 492.3 ms | 344.6 MB |
| pi | 1.5.0 | 325.2 ms | 108.9 MB |
| oh-my-pi (omp) | omp/18.18.0 | 846.1 ms | 273.5 MB |
| oh-my-openagent (omo) | omo 5.1.64 (engine: senpi 2026.11.5-1) | 159.2 ms | 502.3 MB |

**One-shot headless prompt** (includes startup, one `ls` tool call, final answer)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Tool round-trip | request bytes | Note |
|---|---|---|---|---|---|---|---|---|
| horon | 93.5 ms | 72.3 ms–399.0 ms | baseline | 30.9 MB | baseline | yes | 3,619 / 2,390 (1 tools) | — |
| jcode | 339.4 ms | 333.9 ms–696.2 ms | 3.63× more | 34.0 MB | 1.10× more | yes | 3,133 / 1,433 (1 tools) | — |
| Claude Code | 451.7 ms | 415.8 ms–871.5 ms | 4.83× more | 194.2 MB | 6.29× more | yes | 45,798 / 29,653 (12 tools) | — |
| Codex CLI | 407.7 ms | 372.4 ms–1.55 s | 4.36× more | 143.3 MB | 4.64× more | yes | 39,453 / 17,271 (9 tools) | — |
| OpenCode | 2.87 s | 2.25 s–4.51 s | 30.67× more | 753.3 MB | 24.38× more | yes | 2,659 / 0 (0 tools) | — |
| pi | 583.0 ms | 460.7 ms–631.0 ms | 6.23× more | 126.1 MB | 4.08× more | yes | 5,513 / 2,841 (4 tools) | — |
| oh-my-pi (omp) | 2.43 s | 2.19 s–4.06 s | 25.94× more | 452.7 MB | 14.65× more | yes | 29,853 / 18,533 (11 tools) | — |
| oh-my-openagent (omo) | 8.74 s | 8.57 s–11.32 s | 93.44× more | 867.3 MB | 28.08× more | yes | 27,493 / 2,424 (2 tools) | — |

`request bytes` is the first model request of the first run as the mock server received it: JSON body bytes / tool catalog bytes (tools offered).

**Core-profile one-shot** (the one-shot turn without `--tools`, so the request carries the default core surface: the eager tools, `toolbox` and the listed-tool index)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | Note | request bytes |
|---|---|---|---|---|---|---|---|---|---|
| horon | 76.9 ms | 72.7 ms–81.9 ms | baseline | 27.6 MB | baseline | — | yes | — | 11,142 / 9,255 (8 tools) |
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
| horon | 84.4 ms | 75.0 ms–121.2 ms | baseline | 30.6 MB | baseline | skill-uri | yes | — |
| jcode | 278.5 ms | 264.8 ms–343.6 ms | 3.30× more | 36.1 MB | 1.18× more | skill-tool | yes | — |
| Claude Code | 619.3 ms | 555.0 ms–860.0 ms | 7.34× more | 203.0 MB | 6.64× more | skill-tool | yes | — |
| Codex CLI | 516.8 ms | 435.0 ms–1.12 s | 6.13× more | 144.1 MB | 4.72× more | file-read | yes | — |
| OpenCode | 7.89 s | 6.30 s–8.48 s | 93.49× more | 774.4 MB | 25.34× more | skill-tool | yes | — |
| pi | 791.0 ms | 656.2 ms–863.9 ms | 9.37× more | 150.8 MB | 4.93× more | file-read | yes | — |
| oh-my-pi (omp) | 2.12 s | 2.02 s–2.67 s | 25.19× more | 409.3 MB | 13.39× more | file-read | yes | — |
| oh-my-openagent (omo) | 9.06 s | 8.30 s–11.48 s | 107.38× more | 898.9 MB | 29.41× more | file-read | yes | — |

**MCP tool call** (process start to final answer, including starting the fixture stdio MCP server and one `bench_echo` call)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | MCP ready | Note |
|---|---|---|---|---|---|---|---|---|---|
| horon | 100.1 ms | 90.4 ms–143.9 ms | baseline | 39.3 MB | baseline | mcp-call | yes | 75.2 ms | — |
| jcode | 408.9 ms | 362.1 ms–457.4 ms | 4.09× more | 51.5 MB | 1.31× more | mcp-action | yes | 370.8 ms | — |
| Claude Code | 864.0 ms | 711.2 ms–971.5 ms | 8.63× more | 223.3 MB | 5.68× more | mcp-tool | yes | 696.2 ms | — |
| Codex CLI | 534.0 ms | 405.5 ms–637.2 ms | 5.34× more | 142.1 MB | 3.61× more | mcp-tool | yes | 362.0 ms | — |
| OpenCode | 5.99 s | 5.21 s–7.19 s | 59.88× more | 777.1 MB | 19.77× more | mcp-tool | yes | 4.87 s | — |
| pi | 667.2 ms | 606.8 ms–693.7 ms | 6.67× more | 149.7 MB | 3.81× more | mcp-tool | yes | 558.4 ms | — |
| oh-my-pi (omp) | 2.03 s | 1.83 s–2.49 s | 20.25× more | 422.0 MB | 10.73× more | mcp-route | yes | 1.71 s | — |
| oh-my-openagent (omo) | — | — | — | — | — | mcp-tool | no | — | failed: the fixture sentinel never came back |

**Session memory** (idle after 3 turn(s) per session; sizes are process-tree sums, RSS; PSS also recorded on Linux). "Each additional" is the least-squares slope across the measured session counts; 0 B means growth stayed below RSS sampling noise. Threads and descriptors come from the daemon's own `server:memory` after the turns.

| Tool | First session | Each additional | vs horon (additional) | Threads/session | FDs/session | First (post-turn) | Each additional (post-turn) |
|---|---|---|---|---|---|---|---|
| horon | 28.8 MB | 128.7 KB | baseline | 0.57 | 1.00 | 34.9 MB | 384.9 KB |
| jcode | 36.6 MB | 182.0 KB | 1.41× more | — | — | 40.2 MB | 297.8 KB |
| Claude Code | 157.4 MB | 152.3 MB | 1,212× more | — | — | 189.7 MB | 182.7 MB |
| Codex CLI | 110.9 MB | 80.3 MB | 638.78× more | — | — | 137.1 MB | 104.1 MB |
| OpenCode | 264.1 MB | 0.0 B | — | — | — | 563.7 MB | 8.3 MB |
| pi | 119.5 MB | 115.3 MB | 918.04× more | — | — | 148.9 MB | 147.9 MB |
| oh-my-pi (omp) | 385.7 MB | 106.8 MB | 850.22× more | — | — | 468.5 MB | 316.2 MB |
| oh-my-openagent (omo) | 57.5 MB | 44.5 MB | 353.80× more | — | — | 824.9 MB | 324.1 MB |

Threads/descriptors unavailable: jcode (server:memory did not report thread_count, fd_count)

**Post-turn tree RSS per session count**

| Tool | 1 session(s) | 5 session(s) | 10 session(s) |
|---|---|---|---|
| horon | 34.9 MB | 36.3 MB | 38.3 MB |
| jcode | 40.2 MB | 41.5 MB | 42.8 MB |
| Claude Code | 189.7 MB | 926.6 MB | 1.8 GB |
| Codex CLI | 137.1 MB | 542.5 MB | 1.0 GB |
| OpenCode | 563.7 MB | 608.0 MB | 639.3 MB |
| pi | 148.9 MB | 741.9 MB | 1.4 GB |
| oh-my-pi (omp) | 468.5 MB | 1.8 GB | 3.2 GB |
| oh-my-openagent (omo) | 824.9 MB | 2.8 GB | 3.7 GB |

#### 기동

```text
Benchmarking: /Users/runner/work/agent/agent/bench-bin/horon-agent
============================================================

--help (binary load):
  Min:    10.73 ms
  Max:    19.96 ms
  Mean:   14.60 ms
  Median: 14.40 ms
  Stdev:  3.34 ms

--version:
  Min:    14.23 ms
  Max:    22.96 ms
  Mean:   18.27 ms
  Median: 18.00 ms
  Stdev:  3.16 ms

Measuring isolated server startup (10 runs)...

Server ready (isolated socket connectable):
  Min:    27.50 ms
  Max:    98.81 ms
  Mean:   78.59 ms
  Median: 92.87 ms
  Stdev:  28.18 ms

Server startup stages (one extra run with HORON_AGENT_STARTUP_PROFILE=1, not timed):
  stage             at ms  delta ms   share
  panic_hook          0.0       0.0    0.0%
  logging_init        0.3       0.3    2.1%
  nofile_limit        0.5       0.2    1.7%
  perm_harden         0.6       0.1    0.5%
  args_parse          0.6       0.0    0.0%
  config_loaded      12.6      12.0   89.4%
  server_new         13.2       0.6    4.1%
  socket_bound       13.4       0.3    2.0%

Measuring harness bridge readiness (10 runs)...

Bridge stage: api socket connectable:
  Min:    54.07 ms
  Max:    107.22 ms
  Mean:   82.78 ms
  Median: 82.94 ms
  Stdev:  18.00 ms

Bridge stage: hello_ok:
  Min:    55.01 ms
  Max:    109.22 ms
  Mean:   83.82 ms
  Median: 83.88 ms
  Stdev:  18.14 ms

Bridge stage: no-op reply:
  Min:    55.42 ms
  Max:    110.43 ms
  Mean:   84.57 ms
  Median: 84.63 ms
  Stdev:  18.28 ms

Bridge total (socket -> no-op):
  Min:    55.42 ms
  Max:    110.43 ms
  Mean:   84.57 ms
  Median: 84.63 ms
  Stdev:  18.28 ms

============================================================
Summary:
  Binary load median:      ~14.4 ms
  Server ready median:     ~92.9 ms
  Bridge total median:     ~84.6 ms
```

#### 세션 로딩 (COLD)

```text
binary   /Users/runner/work/agent/agent/bench-bin/examples/loading_probe (c86073d131a4)
mode     cold
protocol COLD: every measured run gets its own freshly copied HOME; no derived cache from an earlier run exists, so each sample is a first load of the fixture.
fixtures /var/folders/h5/t85nd63d71x27_8gl5gw3l_w0000gn/T/horon-loading-bench-aggkzx8m
note     --warmup 3 is ignored in cold mode: every sample gets a fresh HOME
fixture events=0 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    0.040/0.039    p95    0.077/0.093    delta  -0.002 ms  control-no-change
  skill_request_overlay p50    0.295/0.270    p95    1.194/0.600    delta  -0.025 ms  control-no-change
  mcp_config_load   p50    0.344/0.290    p95    0.695/0.629    delta  -0.054 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.001/0.000    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.001/0.001    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.001/0.000    delta  -0.000 ms  control-no-change
  session_save      p50   25.070/23.960   p95   48.656/38.254   delta  -1.110 ms  control-no-change
  total store_read_bytes p50 22942  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 22938..22950 (3 distinct values)
  SPREAD   b session_save store_read_bytes 22930..22958 (7 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    7.152/5.905    p95    9.784/16.070   delta  -1.247 ms  control-no-change
  skill_request_overlay p50   10.384/8.938    p95   16.599/16.020   delta  -1.447 ms  control-no-change
  mcp_config_load   p50    0.410/0.403    p95    1.126/1.592    delta  -0.006 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.001/0.001    delta  -0.000 ms  control-no-change
  session_save      p50   34.138/32.874   p95  165.900/61.855   delta  -1.264 ms  control-no-change
  total store_read_bytes p50 22946  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 22930..22958 (4 distinct values)
  SPREAD   b session_save store_read_bytes 22938..22954 (5 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.037/0.032    p95    0.143/0.060    delta  -0.005 ms  control-no-change
  skill_request_overlay p50    0.247/0.255    p95    0.591/0.641    delta  +0.008 ms  control-no-change
  mcp_config_load   p50    0.293/0.276    p95    0.668/0.707    delta  -0.017 ms  control-no-change
  attach_history    p50  414.254/468.758  p95  558.050/608.940  delta +54.504 ms  control-no-change
  resume_ready      p50   95.015/112.214  p95  126.519/127.438  delta +17.199 ms  control-no-change
  provider_dispatch p50    1.304/1.743    p95    2.590/2.439    delta  +0.439 ms  control-no-change
  session_save      p50   26.340/25.590   p95   54.734/42.099   delta  -0.750 ms  control-no-change
  total store_read_bytes p50 5311305  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189203..4189207 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099141..1099145 (3 distinct values)
  SPREAD   a session_save store_read_bytes 22944..22970 (9 distinct values)
  SPREAD   b attach_history store_read_bytes 4189205..4189207 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099144..1099145 (2 distinct values)
  SPREAD   b session_save store_read_bytes 22940..22964 (8 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    4.365/4.591    p95    9.544/6.992    delta  +0.226 ms  control-no-change
  skill_request_overlay p50    6.120/6.071    p95    8.248/8.287    delta  -0.049 ms  control-no-change
  mcp_config_load   p50    0.293/0.271    p95    0.525/0.356    delta  -0.022 ms  control-no-change
  attach_history    p50  312.973/316.955  p95  438.292/399.444  delta  +3.982 ms  control-no-change
  resume_ready      p50   73.062/61.141   p95   99.386/116.349  delta -11.920 ms  control-no-change
  provider_dispatch p50    1.048/1.009    p95    1.327/1.386    delta  -0.039 ms  control-no-change
  session_save      p50   17.694/16.427   p95   26.144/23.418   delta  -1.267 ms  control-no-change
  total store_read_bytes p50 5311310  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189205..4189207 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099144..1099145 (2 distinct values)
  SPREAD   a session_save store_read_bytes 22948..22968 (6 distinct values)
  SPREAD   b session_save store_read_bytes 22944..22964 (6 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.027/0.028    p95    0.031/0.068    delta  +0.001 ms  control-no-change
  skill_request_overlay p50    0.225/0.231    p95    0.526/0.592    delta  +0.006 ms  control-no-change
  mcp_config_load   p50    0.251/0.265    p95    0.300/0.778    delta  +0.014 ms  control-no-change
  attach_history    p50 4279.165/3979.964 p95 4530.956/5533.180 delta -299.201 ms  control-no-change
  resume_ready      p50  848.191/849.646  p95 1069.023/1098.774 delta  +1.455 ms  control-no-change
  provider_dispatch p50   10.453/10.575   p95   19.658/15.567   delta  +0.122 ms  control-no-change
  session_save      p50   22.548/22.842   p95  138.636/63.483   delta  +0.294 ms  control-no-change
  total store_read_bytes p50 53417520  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184303..42184305 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210254..11210255 (2 distinct values)
  SPREAD   a session_save store_read_bytes 22949..22969 (7 distinct values)
  SPREAD   b attach_history store_read_bytes 42184299..42184305 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210250..11210255 (3 distinct values)
  SPREAD   b session_save store_read_bytes 22943..22971 (6 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    4.839/4.206    p95    8.319/13.429   delta  -0.633 ms  control-no-change
  skill_request_overlay p50    7.104/5.566    p95   10.950/11.101   delta  -1.539 ms  control-no-change
  mcp_config_load   p50    0.279/0.264    p95    0.524/0.676    delta  -0.015 ms  control-no-change
  attach_history    p50 4129.955/4212.060 p95 5843.188/5749.831 delta +82.105 ms  control-no-change
  resume_ready      p50  887.077/932.206  p95 1003.115/1032.908 delta +45.130 ms  control-no-change
  provider_dispatch p50   11.367/11.582   p95   21.217/20.851   delta  +0.215 ms  control-no-change
  session_save      p50   33.034/30.131   p95   90.494/86.149   delta  -2.903 ms  control-no-change
  total store_read_bytes p50 53417517  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184301..42184305 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210251..11210255 (3 distinct values)
  SPREAD   a session_save store_read_bytes 22951..22966 (7 distinct values)
  SPREAD   b attach_history store_read_bytes 42184301..42184305 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210251..11210255 (2 distinct values)
  SPREAD   b session_save store_read_bytes 22959..22971 (5 distinct values)
report   bench-results/loading-cold.json
mode cold, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### 세션 로딩 (WARM)

```text
binary   /Users/runner/work/agent/agent/bench-bin/examples/loading_probe (c86073d131a4)
mode     warm
protocol WARM: one HOME per side, --warmup discarded runs per side, then measured runs alternate between the two sides on those same HOMEs.
fixtures /var/folders/h5/t85nd63d71x27_8gl5gw3l_w0000gn/T/horon-loading-bench-3sma7g0k
fixture events=0 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    0.032/0.033    p95    0.080/0.066    delta  +0.001 ms  control-no-change
  skill_request_overlay p50    0.279/0.245    p95    0.603/0.668    delta  -0.034 ms  control-no-change
  mcp_config_load   p50    0.306/0.465    p95    0.688/0.982    delta  +0.160 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.001/0.001    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  session_save      p50   19.247/17.525   p95   43.782/40.268   delta  -1.722 ms  control-no-change
  total store_read_bytes p50 23654  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [22931, 22935, 22943, 22951, 23651, 23657, 23661, 24371, 24387]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [22943, 22947, 22959, 23657, 23661, 24363, 24365, 24375, 24383]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 22931..24387 (9 distinct values)
  SPREAD   b session_save store_read_bytes 22943..24383 (9 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    4.830/6.476    p95    7.218/11.557   delta  +1.646 ms  control-no-change
  skill_request_overlay p50    6.288/6.602    p95   10.902/13.638   delta  +0.315 ms  control-no-change
  mcp_config_load   p50    0.292/0.294    p95    0.366/0.769    delta  +0.002 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.001    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.001    delta  -0.000 ms  control-no-change
  session_save      p50   19.625/18.969   p95   31.915/65.035   delta  -0.656 ms  control-no-change
  total store_read_bytes p50 23660  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [22943, 22947, 22949, 22951, 23657, 23663, 24357, 24379, 24387]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [22943, 22947, 22951, 23653, 23657, 24367, 24375, 24383]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 22943..24387 (9 distinct values)
  SPREAD   b session_save store_read_bytes 22943..24383 (8 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.031/0.030    p95    0.088/0.115    delta  -0.001 ms  control-no-change
  skill_request_overlay p50    0.291/0.254    p95    0.682/0.747    delta  -0.037 ms  control-no-change
  mcp_config_load   p50    0.362/0.283    p95    0.797/0.624    delta  -0.079 ms  control-no-change
  attach_history    p50  169.733/156.289  p95  226.404/192.219  delta -13.444 ms  control-no-change
  resume_ready      p50   98.561/94.663   p95  147.370/121.230  delta  -3.897 ms  control-no-change
  provider_dispatch p50    1.136/1.114    p95    2.603/3.161    delta  -0.022 ms  control-no-change
  session_save      p50   29.099/24.700   p95   60.437/47.137   delta  -4.400 ms  control-no-change
  total store_read_bytes p50 2841253  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23670, 23682, 24382, 24386, 25092, 25098, 25808, 25810, 26524, 26534]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23662, 23678, 24388, 24392, 25080, 25102, 25804, 25810, 26520, 26538]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23670..26534 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23662..26538 (10 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    5.004/4.814    p95    8.425/7.701    delta  -0.190 ms  control-no-change
  skill_request_overlay p50    6.854/7.435    p95   13.130/11.325   delta  +0.581 ms  control-no-change
  mcp_config_load   p50    0.306/0.318    p95    0.780/0.926    delta  +0.012 ms  control-no-change
  attach_history    p50  119.068/140.017  p95  151.697/161.707  delta +20.949 ms  control-no-change
  resume_ready      p50   81.400/78.557   p95  109.209/98.079   delta  -2.843 ms  control-no-change
  provider_dispatch p50    1.072/1.098    p95    1.805/2.132    delta  +0.026 ms  control-no-change
  session_save      p50   21.544/22.624   p95   26.137/29.505   delta  +1.080 ms  control-no-change
  total store_read_bytes p50 2841258  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23670, 23682, 24388, 24392, 25094, 25106, 25812, 25816, 26526, 26534]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23664, 23668, 24374, 24386, 25100, 25102, 25800, 25818, 26508, 26518]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23670..26534 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23664..26518 (10 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.035/0.030    p95    0.083/0.065    delta  -0.005 ms  control-no-change
  skill_request_overlay p50    0.335/0.251    p95    0.902/0.780    delta  -0.083 ms  control-no-change
  mcp_config_load   p50    0.348/0.314    p95    0.995/0.872    delta  -0.033 ms  control-no-change
  attach_history    p50 1632.331/1660.022 p95 1842.503/1787.893 delta +27.691 ms  control-no-change
  resume_ready      p50 1084.480/1011.157 p95 1282.456/1187.024 delta -73.323 ms  control-no-change
  provider_dispatch p50   13.040/12.246   p95   17.730/20.594   delta  -0.794 ms  control-no-change
  session_save      p50   25.922/28.228   p95   33.213/45.676   delta  +2.306 ms  control-no-change
  total store_read_bytes p50 28640284  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23677, 23685, 24375, 24395, 25105, 25113, 25799, 25803, 26517, 26525]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23673, 23685, 24375, 24399, 25101, 25105, 25811, 25815, 26521, 26529]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23677..26525 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23673..26529 (10 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    4.864/5.177    p95   36.774/9.733    delta  +0.313 ms  control-no-change
  skill_request_overlay p50    7.585/7.705    p95   54.288/11.229   delta  +0.120 ms  control-no-change
  mcp_config_load   p50    0.326/0.311    p95    0.642/0.756    delta  -0.015 ms  control-no-change
  attach_history    p50 1494.826/1500.153 p95 1767.452/1744.991 delta  +5.327 ms  control-no-change
  resume_ready      p50  988.164/982.877  p95 1561.017/1174.780 delta  -5.286 ms  control-no-change
  provider_dispatch p50   14.916/12.887   p95   26.066/25.485   delta  -2.028 ms  control-no-change
  session_save      p50   34.937/31.678   p95  143.266/38.498   delta  -3.259 ms  control-no-change
  total store_read_bytes p50 28640273  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23677, 23681, 24391, 24403, 25087, 25109, 25807, 25817, 26527, 26529]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23672, 23676, 24378, 24392, 25074, 25102, 25808, 25812, 26522, 26530]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23677..26529 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23672..26530 (10 distinct values)
report   bench-results/loading-warm.json
mode warm, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### Tool latency

| Tool case | median | p95 | min | max | samples |
|---|---|---|---|---|---|
| read (tiny file) | 0.3 ms | 0.5 ms | 0.2 ms | 0.5 ms | 5 |
| read (1000 lines) | 1.0 ms | 2.1 ms | 0.8 ms | 2.1 ms | 5 |
| read (10 lines @ offset) | 0.7 ms | 1.0 ms | 0.5 ms | 1.0 ms | 5 |
| read (repo Cargo.toml) | 1.4 ms | 1.8 ms | 1.0 ms | 1.8 ms | 5 |
| write (small) | 7.9 ms | 14.9 ms | 4.5 ms | 14.9 ms | 5 |
| write (10KB) | 3.2 ms | 7.7 ms | 2.4 ms | 7.7 ms | 5 |
| edit (simple replace) | 2.7 ms | 3.7 ms | 2.3 ms | 3.7 ms | 5 |
| ls (scripts dir) | 2.2 ms | 2.7 ms | 1.1 ms | 2.7 ms | 5 |
| agentgrep (grep in tree) | 5.7 ms | 8.4 ms | 5.0 ms | 8.4 ms | 5 |
| agentgrep (find **/*.rs) | 3.5 ms | 5.7 ms | 2.5 ms | 5.7 ms | 5 |
| bash (true) | 9.4 ms | 14.2 ms | 8.9 ms | 14.2 ms | 5 |
| bash (echo hello) | 8.7 ms | 14.7 ms | 6.4 ms | 14.7 ms | 5 |
| bash (ls fixture tree) | 14.3 ms | 21.8 ms | 13.8 ms | 21.8 ms | 5 |
| invalid tool (dispatch only) | 0.2 ms | 0.7 ms | 0.1 ms | 0.7 ms | 5 |

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
| 1 | 56.6 ms | 56.6 ms | 56.6 ms | 56.7 ms | 0 |
| 10 | 192.6 ms | 193.6 ms | 193.6 ms | 193.9 ms | 0 |
| 40 | 1.23 s | 1.23 s | 1.23 s | 1.23 s | 0 |

#### Compile loop

| Checkpoint | Touched | median | min | max | samples |
|---|---|---|---|---|---|
| warm_noop | — | 1.52 s | 1.52 s | 1.52 s | 1 |
| leaf_touch | src/main.rs | 717.8 ms | 717.8 ms | 717.8 ms | 1 |
| core_touch | crates/app-core/src/lib.rs | 40.27 s | 40.27 s | 40.27 s | 1 |
| build_meta_touch | crates/build-meta/src/lib.rs | 50.15 s | 50.15 s | 50.15 s | 1 |

### Windows

#### Coding agent comparison (headless, mock model)

Host: windows/AMD64, 4 CPUs, Python 3.12.10. Baseline: **horon**. Samples per one-shot, skill and MCP cell: 5.

Skipped: syscalls (strace not available)

| Tool | Version | `--version` (median) | Install size |
|---|---|---|---|
| horon | horon-agent v0.1.0-dev (ef9339c) | 12.2 ms | 97.0 MB |
| jcode | jcode v0.99.1 (7206efd0c) | 13.5 ms | 128.4 MB |
| Claude Code | 2.1.317 (Claude Code) | 37.4 ms | 645.2 MB |
| Codex CLI | codex-cli 0.166.0 | 95.0 ms | 479.4 MB |
| OpenCode | 1.19.5 | 459.6 ms | 605.3 MB |
| pi | 1.5.0 | 254.5 ms | 108.9 MB |
| oh-my-pi (omp) | omp/18.18.0 | 626.2 ms | 291.7 MB |
| oh-my-openagent (omo) | omo 5.1.64 (engine: senpi 2026.11.5-1) | 176.0 ms | 514.8 MB |

**One-shot headless prompt** (includes startup, one `ls` tool call, final answer)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Tool round-trip | request bytes | Note |
|---|---|---|---|---|---|---|---|---|
| horon | 182.7 ms | 159.2 ms–590.7 ms | baseline | 31.2 MB | baseline | yes | 3,686 / 2,390 (1 tools) | — |
| jcode | 128.6 ms | 118.5 ms–258.3 ms | 1.42× less | 32.9 MB | 1.06× more | yes | 3,105 / 1,312 (1 tools) | — |
| Claude Code | 1.19 s | 1.15 s–3.80 s | 6.51× more | 272.6 MB | 8.75× more | yes | 50,067 / 33,795 (14 tools) | — |
| Codex CLI | 1.69 s | 1.34 s–1.78 s | 9.25× more | 266.0 MB | 8.54× more | yes | 41,028 / 18,342 (9 tools) | — |
| OpenCode | 4.88 s | 4.46 s–7.68 s | 26.70× more | 3.3 GB | 107.89× more | yes | 2,659 / 0 (0 tools) | — |
| pi | 439.8 ms | 438.4 ms–483.9 ms | 2.41× more | 124.2 MB | 3.99× more | yes | 5,522 / 2,841 (4 tools) | — |
| oh-my-pi (omp) | 3.17 s | 2.15 s–3.60 s | 17.36× more | 409.5 MB | 13.14× more | yes | 29,889 / 18,533 (11 tools) | — |
| oh-my-openagent (omo) | 19.19 s | 14.10 s–59.73 s | 105.05× more | 547.5 MB | 17.57× more | yes | 29,467 / 2,424 (2 tools) | — |

`request bytes` is the first model request of the first run as the mock server received it: JSON body bytes / tool catalog bytes (tools offered).

**Core-profile one-shot** (the one-shot turn without `--tools`, so the request carries the default core surface: the eager tools, `toolbox` and the listed-tool index)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | Note | request bytes |
|---|---|---|---|---|---|---|---|---|---|
| horon | 151.3 ms | 147.9 ms–177.8 ms | baseline | 32.8 MB | baseline | — | yes | — | 11,209 / 9,255 (8 tools) |
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
| horon | 153.6 ms | 141.7 ms–193.5 ms | baseline | 26.0 MB | baseline | skill-uri | yes | — |
| jcode | 271.6 ms | 194.2 ms–680.0 ms | 1.77× more | 24.0 MB | 1.08× less | skill-tool | yes | — |
| Claude Code | 875.4 ms | 803.1 ms–959.2 ms | 5.70× more | 239.3 MB | 9.21× more | skill-tool | yes | — |
| Codex CLI | 2.13 s | 1.77 s–2.78 s | 13.86× more | 263.9 MB | 10.15× more | file-read | yes | — |
| OpenCode | 7.46 s | 6.88 s–7.87 s | 48.58× more | 3.2 GB | 125.44× more | skill-tool | yes | — |
| pi | 569.9 ms | 561.7 ms–587.8 ms | 3.71× more | 97.9 MB | 3.77× more | file-read | yes | — |
| oh-my-pi (omp) | 3.02 s | 2.59 s–3.47 s | 19.66× more | 361.8 MB | 13.92× more | file-read | yes | — |
| oh-my-openagent (omo) | 19.15 s | 15.56 s–21.16 s | 124.67× more | 545.7 MB | 20.99× more | file-read | yes | — |

**MCP tool call** (process start to final answer, including starting the fixture stdio MCP server and one `bench_echo` call)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | MCP ready | Note |
|---|---|---|---|---|---|---|---|---|---|
| horon | 218.3 ms | 168.2 ms–366.4 ms | baseline | 37.5 MB | baseline | mcp-call | yes | 127.2 ms | — |
| jcode | 426.5 ms | 286.0 ms–647.2 ms | 1.95× more | 36.1 MB | 1.04× less | mcp-action | yes | 279.8 ms | — |
| Claude Code | 1.01 s | 879.8 ms–1.19 s | 4.60× more | 273.4 MB | 7.30× more | mcp-tool | yes | 771.0 ms | — |
| Codex CLI | 831.0 ms | 708.5 ms–1.85 s | 3.81× more | 177.7 MB | 4.74× more | mcp-tool | yes | 641.5 ms | — |
| OpenCode | 7.11 s | 6.81 s–7.64 s | 32.57× more | 854.4 MB | 22.81× more | mcp-tool | yes | 5.84 s | — |
| pi | 671.8 ms | 659.7 ms–1.14 s | 3.08× more | 118.7 MB | 3.17× more | mcp-tool | yes | 517.1 ms | — |
| oh-my-pi (omp) | 2.99 s | 2.95 s–3.02 s | 13.71× more | 379.1 MB | 10.12× more | mcp-route | yes | 2.44 s | — |
| oh-my-openagent (omo) | — | — | — | — | — | mcp-tool | no | — | failed: the fixture sentinel never came back |

**Session memory** (idle after 3 turn(s) per session; sizes are process-tree sums, RSS; PSS also recorded on Linux). "Each additional" is the least-squares slope across the measured session counts; 0 B means growth stayed below RSS sampling noise. Threads and descriptors come from the daemon's own `server:memory` after the turns.

| Tool | First session | Each additional | vs horon (additional) | Threads/session | FDs/session | First (post-turn) | Each additional (post-turn) |
|---|---|---|---|---|---|---|---|
| horon | 22.4 MB | 140.7 KB | baseline | — | — | 30.0 MB | 455.9 KB |
| jcode | 21.9 MB | 71.9 KB | 1.96× less | — | — | 26.9 MB | 365.6 KB |
| Claude Code | 154.0 MB | 170.8 MB | 1,243× more | — | — | 185.7 MB | 190.4 MB |
| Codex CLI | 123.6 MB | 119.5 MB | 869.76× more | — | — | 163.4 MB | 119.1 MB |
| OpenCode | 246.2 MB | 17.9 MB | 130.04× more | — | — | 436.2 MB | 24.1 MB |
| pi | 90.6 MB | 90.7 MB | 660.08× more | — | — | 101.0 MB | 100.9 MB |
| oh-my-pi (omp) | 330.3 MB | 110.9 MB | 806.69× more | — | — | 349.2 MB | 338.3 MB |
| oh-my-openagent (omo) | 149.3 MB | 130.0 MB | 945.93× more | — | — | 415.4 MB | 393.8 MB |

Threads/descriptors unavailable: horon (server:memory did not report thread_count, fd_count), jcode (server:memory did not report thread_count, fd_count)

**Post-turn tree RSS per session count**

| Tool | 1 session(s) | 5 session(s) | 10 session(s) |
|---|---|---|---|
| horon | 30.0 MB | 32.2 MB | 34.1 MB |
| jcode | 26.9 MB | 29.1 MB | 30.2 MB |
| Claude Code | 185.7 MB | 945.3 MB | 1.9 GB |
| Codex CLI | 163.4 MB | 645.7 MB | 1.2 GB |
| OpenCode | 436.2 MB | 462.2 MB | 647.8 MB |
| pi | 101.0 MB | 505.3 MB | 1008.9 MB |
| oh-my-pi (omp) | 349.2 MB | 1.7 GB | 3.3 GB |
| oh-my-openagent (omo) | 415.4 MB | 2.2 GB | 3.9 GB |

#### 기동

```text
Benchmarking: D:/a/agent/agent/bench-bin/horon-agent.exe
============================================================

--help (binary load):
  Min:    10.22 ms
  Max:    11.45 ms
  Mean:   10.72 ms
  Median: 10.61 ms
  Stdev:  0.39 ms

--version:
  Min:    10.29 ms
  Max:    11.27 ms
  Mean:   10.64 ms
  Median: 10.54 ms
  Stdev:  0.35 ms

Measuring isolated server startup (10 runs)...

Server ready (isolated socket connectable):
  Min:    16.45 ms
  Max:    23.32 ms
  Mean:   21.88 ms
  Median: 22.32 ms
  Stdev:  1.96 ms

Server startup stages (one extra run with HORON_AGENT_STARTUP_PROFILE=1, not timed):
  stage             at ms  delta ms   share
  panic_hook          0.0       0.0    0.1%
  logging_init        1.5       1.5   22.7%
  nofile_limit        1.9       0.5    7.2%
  perm_harden         4.4       2.5   38.1%
  args_parse          4.4       0.0    0.0%
  config_loaded       5.3       0.9   13.9%
  server_new          6.1       0.8   12.2%
  socket_bound        6.5       0.4    5.8%

Measuring harness bridge readiness (10 runs)...
  skipped: api-bridge is Unix-only, so Windows has no harness bridge to time

Bridge stage: api socket connectable: No successful runs

Bridge stage: hello_ok: No successful runs

Bridge stage: no-op reply: No successful runs

Bridge total (socket -> no-op): No successful runs

============================================================
Summary:
  Binary load median:      ~10.6 ms
  Server ready median:     ~22.3 ms
  Bridge total median:     n/a
```

#### 세션 로딩 (COLD)

```text
binary   D:\a\agent\agent\bench-bin\examples\loading_probe.exe (fb8dc412fbe6)
mode     cold
protocol COLD: every measured run gets its own freshly copied HOME; no derived cache from an earlier run exists, so each sample is a first load of the fixture.
fixtures C:\Users\RUNNER~1\AppData\Local\Temp\horon-loading-bench-lll365np
note     --warmup 3 is ignored in cold mode: every sample gets a fresh HOME
fixture events=0 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    0.101/0.101    p95    0.119/0.120    delta  -0.000 ms  control-no-change
  skill_request_overlay p50    0.787/0.780    p95    0.853/0.862    delta  -0.007 ms  control-no-change
  mcp_config_load   p50    0.974/1.005    p95    1.067/1.086    delta  +0.031 ms  control-no-change
  attach_history    p50    0.001/0.001    p95    0.002/0.002    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.001/0.001    p95    0.002/0.002    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.002/0.001    p95    0.002/0.002    delta  -0.000 ms  control-no-change
  session_save      p50   70.589/61.310   p95   84.387/105.412  delta  -9.280 ms  control-no-change
  total store_read_bytes p50 22934  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 22916..22968 (7 distinct values)
  SPREAD   b session_save store_read_bytes 22900..22964 (8 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50   13.827/14.149   p95   14.773/14.517   delta  +0.322 ms  control-no-change
  skill_request_overlay p50   36.667/36.797   p95   39.480/37.608   delta  +0.130 ms  control-no-change
  mcp_config_load   p50    0.941/0.949    p95    1.010/1.054    delta  +0.008 ms  control-no-change
  attach_history    p50    0.002/0.002    p95    0.005/0.002    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.001/0.001    p95    0.002/0.002    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.002/0.002    p95    0.002/0.002    delta  -0.000 ms  control-no-change
  session_save      p50   72.852/80.418   p95  116.520/144.092  delta  +7.567 ms  control-no-change
  total store_read_bytes p50 22940  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 22918..22956 (9 distinct values)
  SPREAD   b session_save store_read_bytes 22916..22964 (8 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.094/0.098    p95    0.098/0.138    delta  +0.003 ms  control-no-change
  skill_request_overlay p50    0.775/0.774    p95    0.848/0.963    delta  -0.001 ms  control-no-change
  mcp_config_load   p50    0.925/0.944    p95    0.978/1.141    delta  +0.019 ms  control-no-change
  attach_history    p50  422.463/413.279  p95  439.401/427.349  delta  -9.184 ms  control-no-change
  resume_ready      p50  147.736/146.577  p95  167.283/157.795  delta  -1.159 ms  control-no-change
  provider_dispatch p50    1.497/1.486    p95    1.535/1.549    delta  -0.010 ms  control-no-change
  session_save      p50   77.877/52.780   p95  129.678/95.429   delta -25.097 ms  control-no-change
  total store_read_bytes p50 5311213  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189151..4189155 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099102..1099106 (3 distinct values)
  SPREAD   a session_save store_read_bytes 22936..22975 (9 distinct values)
  SPREAD   b attach_history store_read_bytes 4189151..4189155 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099102..1099106 (2 distinct values)
  SPREAD   b session_save store_read_bytes 22930..22976 (7 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50   14.976/14.755   p95   15.439/15.121   delta  -0.221 ms  control-no-change
  skill_request_overlay p50   37.281/36.890   p95   39.389/41.657   delta  -0.391 ms  control-no-change
  mcp_config_load   p50    0.976/1.003    p95    1.026/1.033    delta  +0.027 ms  control-no-change
  attach_history    p50  419.450/416.852  p95  423.319/443.932  delta  -2.598 ms  control-no-change
  resume_ready      p50  146.281/146.520  p95  166.872/168.022  delta  +0.239 ms  control-no-change
  provider_dispatch p50    1.453/1.436    p95    1.526/1.544    delta  -0.017 ms  control-no-change
  session_save      p50   69.335/81.554   p95   84.555/121.155  delta +12.218 ms  control-no-change
  total store_read_bytes p50 5311213  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189145..4189155 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099096..1099106 (3 distinct values)
  SPREAD   a session_save store_read_bytes 22888..22983 (8 distinct values)
  SPREAD   b attach_history store_read_bytes 4189147..4189155 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099098..1099106 (3 distinct values)
  SPREAD   b session_save store_read_bytes 22935..22968 (8 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.103/0.099    p95    0.145/0.149    delta  -0.004 ms  control-no-change
  skill_request_overlay p50    0.780/0.789    p95    1.040/0.802    delta  +0.009 ms  control-no-change
  mcp_config_load   p50    0.925/0.919    p95    1.046/0.996    delta  -0.006 ms  control-no-change
  attach_history    p50 4812.511/4810.719 p95 4890.158/4874.619 delta  -1.792 ms  control-no-change
  resume_ready      p50 1540.991/1543.595 p95 1557.503/1554.152 delta  +2.604 ms  control-no-change
  provider_dispatch p50   16.666/16.130   p95   17.572/17.790   delta  -0.536 ms  control-no-change
  session_save      p50   68.857/52.511   p95  105.337/151.425  delta -16.347 ms  control-no-change
  total store_read_bytes p50 53417439  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184252..42184253 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210215..11210216 (2 distinct values)
  SPREAD   a session_save store_read_bytes 22961..22979 (5 distinct values)
  SPREAD   b attach_history store_read_bytes 42184251..42184253 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210215..11210216 (2 distinct values)
  SPREAD   b session_save store_read_bytes 22943..22978 (9 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50   14.364/14.730   p95   15.063/15.712   delta  +0.366 ms  control-no-change
  skill_request_overlay p50   37.227/37.095   p95   39.965/40.066   delta  -0.132 ms  control-no-change
  mcp_config_load   p50    0.994/1.012    p95    1.108/1.388    delta  +0.018 ms  control-no-change
  attach_history    p50 4804.463/4820.813 p95 4904.130/4866.154 delta +16.349 ms  control-no-change
  resume_ready      p50 1527.567/1540.279 p95 1550.013/1562.739 delta +12.712 ms  control-no-change
  provider_dispatch p50   15.869/16.806   p95   18.229/17.563   delta  +0.936 ms  control-no-change
  session_save      p50   45.749/47.038   p95   92.112/98.338   delta  +1.290 ms  control-no-change
  total store_read_bytes p50 53417410  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184249..42184253 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210212..11210216 (3 distinct values)
  SPREAD   a session_save store_read_bytes 22911..22975 (9 distinct values)
  SPREAD   b attach_history store_read_bytes 42184249..42184253 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210212..11210216 (3 distinct values)
  SPREAD   b session_save store_read_bytes 22942..22979 (8 distinct values)
report   bench-results\loading-cold.json
mode cold, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### 세션 로딩 (WARM)

```text
binary   D:\a\agent\agent\bench-bin\examples\loading_probe.exe (fb8dc412fbe6)
mode     warm
protocol WARM: one HOME per side, --warmup discarded runs per side, then measured runs alternate between the two sides on those same HOMEs.
fixtures C:\Users\RUNNER~1\AppData\Local\Temp\horon-loading-bench-x3as8qv_
fixture events=0 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    0.154/0.172    p95    0.235/0.222    delta  +0.017 ms  control-drift
  skill_request_overlay p50    1.240/1.248    p95    1.318/1.594    delta  +0.008 ms  control-no-change
  mcp_config_load   p50    1.449/1.446    p95    1.586/1.920    delta  -0.003 ms  control-no-change
  attach_history    p50    0.002/0.002    p95    0.002/0.002    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.002/0.002    p95    0.002/0.002    delta  +0.000 ms  control-drift
  provider_dispatch p50    0.002/0.002    p95    0.002/0.002    delta  +0.000 ms  control-no-change
  session_save      p50   51.645/51.790   p95   55.350/58.566   delta  +0.145 ms  control-no-change
  total store_read_bytes p50 23632  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [22933, 22937, 22941, 22949, 23620, 23644, 23664, 24343, 24351, 24363]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [22892, 22901, 22922, 22945, 23619, 23652, 23656, 24335, 24343]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 22933..24363 (10 distinct values)
  SPREAD   b session_save store_read_bytes 22892..24343 (9 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50   24.199/24.483   p95   26.006/28.480   delta  +0.285 ms  control-no-change
  skill_request_overlay p50   55.383/54.927   p95   66.121/55.717   delta  -0.456 ms  control-no-change
  mcp_config_load   p50    1.486/1.444    p95    1.547/1.533    delta  -0.042 ms  control-no-change
  attach_history    p50    0.002/0.005    p95    0.008/0.007    delta  +0.003 ms  control-drift
  resume_ready      p50    0.002/0.002    p95    0.002/0.002    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.002/0.002    p95    0.002/0.002    delta  +0.000 ms  control-drift
  session_save      p50   56.684/54.318   p95   59.138/56.748   delta  -2.366 ms  control-no-change
  total store_read_bytes p50 23656  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [22949, 22953, 23628, 23648, 23664, 24311, 24343, 24355]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [22922, 22925, 22941, 22965, 23598, 23636, 23642, 24325, 24351, 24359]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 22949..24355 (8 distinct values)
  SPREAD   b session_save store_read_bytes 22922..24359 (10 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.156/0.158    p95    0.162/0.172    delta  +0.002 ms  control-no-change
  skill_request_overlay p50    1.255/1.243    p95    1.280/1.305    delta  -0.012 ms  control-no-change
  mcp_config_load   p50    1.443/1.482    p95    1.494/1.618    delta  +0.039 ms  control-no-change
  attach_history    p50  300.536/296.484  p95  310.021/302.917  delta  -4.053 ms  control-no-change
  resume_ready      p50  237.246/236.246  p95  242.656/244.168  delta  -1.001 ms  control-no-change
  provider_dispatch p50    1.815/1.809    p95    1.886/1.929    delta  -0.006 ms  control-no-change
  session_save      p50   50.306/50.300   p95   52.413/55.254   delta  -0.006 ms  control-no-change
  total store_read_bytes p50 2841098  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23657, 23675, 24306, 24350, 25011, 25025, 25728, 25758, 26423, 26441]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23619, 23649, 24320, 24360, 25039, 25063, 25758, 25766, 26437, 26449]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23657..26441 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23619..26449 (10 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50   25.147/24.391   p95   27.017/25.479   delta  -0.756 ms  control-no-change
  skill_request_overlay p50   57.182/56.731   p95   61.800/62.108   delta  -0.451 ms  control-no-change
  mcp_config_load   p50    1.523/1.503    p95    1.659/1.860    delta  -0.020 ms  control-no-change
  attach_history    p50  300.108/300.531  p95  307.973/306.021  delta  +0.422 ms  control-no-change
  resume_ready      p50  236.704/236.437  p95  249.990/239.918  delta  -0.267 ms  control-no-change
  provider_dispatch p50    1.794/1.819    p95    2.156/2.394    delta  +0.025 ms  control-no-change
  session_save      p50   54.924/51.330   p95   73.091/64.987   delta  -3.594 ms  control-no-change
  total store_read_bytes p50 2841142  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23635, 23655, 24354, 24366, 25059, 25065, 25764, 25766, 26443, 26453]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23657, 23663, 24343, 24350, 25041, 25061, 25724, 25748, 26387, 26441]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23635..26453 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23657..26441 (10 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.160/0.155    p95    0.247/0.197    delta  -0.005 ms  control-no-change
  skill_request_overlay p50    1.278/1.231    p95    1.653/1.267    delta  -0.048 ms  control-no-change
  mcp_config_load   p50    1.480/1.415    p95    1.787/1.535    delta  -0.065 ms  control-no-change
  attach_history    p50 3078.012/3085.252 p95 3099.224/3123.189 delta  +7.239 ms  control-no-change
  resume_ready      p50 2441.105/2467.634 p95 2475.137/2484.414 delta +26.528 ms  control-no-change
  provider_dispatch p50   18.496/19.934   p95   20.043/21.151   delta  +1.438 ms  control-no-change
  session_save      p50   50.780/51.939   p95   60.132/56.111   delta  +1.159 ms  control-no-change
  total store_read_bytes p50 28640136  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23654, 23662, 24341, 24365, 25035, 25044, 25750, 25757, 26429, 26464]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23642, 23644, 24323, 24357, 25039, 25064, 25738, 25763, 26409, 26430]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23654..26464 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23642..26430 (10 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50   23.534/23.852   p95   24.845/24.973   delta  +0.318 ms  control-no-change
  skill_request_overlay p50   54.491/54.007   p95   56.546/59.436   delta  -0.484 ms  control-no-change
  mcp_config_load   p50    1.538/1.502    p95    1.580/1.560    delta  -0.036 ms  control-no-change
  attach_history    p50 3054.648/3071.271 p95 3101.236/3114.167 delta +16.623 ms  control-no-change
  resume_ready      p50 2429.708/2427.986 p95 2467.493/2466.425 delta  -1.723 ms  control-no-change
  provider_dispatch p50   18.580/18.414   p95   20.114/20.507   delta  -0.166 ms  control-no-change
  session_save      p50   49.033/49.553   p95   56.092/56.372   delta  +0.520 ms  control-no-change
  total store_read_bytes p50 28640160  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23654, 23657, 24324, 24366, 25057, 25068, 25732, 25747, 26455, 26460]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23647, 23655, 24319, 24358, 25061, 25065, 25751, 25772, 26456, 26461]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23654..26460 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23647..26461 (10 distinct values)
report   bench-results\loading-warm.json
mode warm, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### Tool latency

| Tool case | median | p95 | min | max | samples |
|---|---|---|---|---|---|
| read (tiny file) | 2.7 ms | 2.8 ms | 2.2 ms | 2.8 ms | 5 |
| read (1000 lines) | 2.7 ms | 2.8 ms | 2.2 ms | 2.8 ms | 5 |
| read (10 lines @ offset) | 2.7 ms | 2.8 ms | 2.7 ms | 2.8 ms | 5 |
| read (repo Cargo.toml) | 2.7 ms | 2.8 ms | 2.3 ms | 2.8 ms | 5 |
| write (small) | 13.1 ms | 23.4 ms | 10.5 ms | 23.4 ms | 5 |
| write (10KB) | 14.6 ms | 27.5 ms | 10.0 ms | 27.5 ms | 5 |
| edit (simple replace) | 12.5 ms | 40.8 ms | 9.9 ms | 40.8 ms | 5 |
| ls (scripts dir) | 2.7 ms | 2.8 ms | 2.2 ms | 2.8 ms | 5 |
| agentgrep (grep in tree) | 7.9 ms | 8.0 ms | 7.4 ms | 8.0 ms | 5 |
| agentgrep (find **/*.rs) | 5.3 ms | 7.3 ms | 5.3 ms | 7.3 ms | 5 |
| bash (true) | 10.5 ms | 10.5 ms | 9.9 ms | 10.5 ms | 5 |
| bash (echo hello) | 10.5 ms | 10.6 ms | 10.0 ms | 10.6 ms | 5 |
| bash (ls fixture tree) | 12.6 ms | 13.1 ms | 12.6 ms | 13.1 ms | 5 |
| invalid tool (dispatch only) | 2.7 ms | 2.7 ms | 2.2 ms | 2.7 ms | 5 |

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
| 1 | 234.6 ms | 234.6 ms | 234.6 ms | 234.8 ms | 0 |
| 10 | 863.3 ms | 867.8 ms | 867.8 ms | 867.9 ms | 0 |
| 40 | 2.51 s | 3.19 s | 3.22 s | 3.22 s | 0 |

#### Compile loop

| Checkpoint | Touched | median | min | max | samples |
|---|---|---|---|---|---|
| warm_noop | — | 455.1 ms | 455.1 ms | 455.1 ms | 1 |
| leaf_touch | src\main.rs | 619.4 ms | 619.4 ms | 619.4 ms | 1 |
| core_touch | crates\app-core\src\lib.rs | 19.40 s | 19.40 s | 19.40 s | 1 |
| build_meta_touch | crates\build-meta\src\lib.rs | 27.77 s | 27.77 s | 27.77 s | 1 |

## TUI (`horon-tui`)

대화형 클라이언트는 별도 바이너리이고 따로 잽니다. 이 수치는 `tui` 묶음에서 나오며 위의 헤드리스 표와 비교하지 않습니다. `first_frame_ms` 와 `direct_first_frame_ms` 는 서로 다른 두 프로그램을 각각 띄운 값이고, 핸드오프는 그 둘의 차이입니다.

![TUI (`horon-tui`)](charts/tui-01-latency.ko.svg)

![TUI (`horon-tui`)](charts/tui-02-memory.ko.svg)

### Linux

#### TUI (`horon-tui`)

Measured version: the interactive client `horon-tui`. The headless `horon-agent` numbers are in the suites above and are never compared with these: the two binaries are timed against themselves.

| Metric | Median | Min–max | Samples |
| --- | --- | --- | --- |
| First frame (bare `horon-agent`) | 36.6 ms | 35.1 ms–132.9 ms | 5 |
| First frame (direct `horon-tui`) | 39.8 ms | 39.0 ms–40.7 ms | 5 |
| TTY handoff cost | -3.1 ms | -5.6 ms–92.8 ms | 5 |
| Key press to echo (p95) | 0.1 ms | 0.1 ms–0.3 ms | 50 |
| Idle RSS after 5 s | 5.5 MB | 5.4 MB–5.6 MB | 5 |
| RSS after one turn | 5.7 MB | 5.7 MB–5.8 MB | 5 |
| Frame draw time (p50) | 1.3 ms | 1.3 ms–1.3 ms | 5 |
| Frame draw time (p95) | 1.4 ms | 1.4 ms–1.4 ms | 5 |
| Install size (`horon-tui`) | 6.0 MB | — | 1 |

Handoff: median first-frame time of a bare `horon-agent` launch (no arguments, on a PTY) minus the median first-frame time of a direct `horon-tui` launch against an already-running daemon, both on the same machine and build. The bare path also starts the daemon, so the number covers that startup, the sibling lookup and the `exec`; it is the extra cost of the handoff over launching the client itself.

Render bench: 200 frames on a 120×40 terminal.

### macOS

#### TUI (`horon-tui`)

Measured version: the interactive client `horon-tui`. The headless `horon-agent` numbers are in the suites above and are never compared with these: the two binaries are timed against themselves.

| Metric | Median | Min–max | Samples |
| --- | --- | --- | --- |
| First frame (bare `horon-agent`) | 53.8 ms | 49.5 ms–296.5 ms | 5 |
| First frame (direct `horon-tui`) | 47.0 ms | 38.1 ms–56.6 ms | 5 |
| TTY handoff cost | 4.5 ms | -7.1 ms–253.2 ms | 5 |
| Key press to echo (p95) | 0.2 ms | 0.1 ms–0.5 ms | 50 |
| Idle RSS after 5 s | 8.8 MB | 8.8 MB–8.9 MB | 5 |
| RSS after one turn | 9.0 MB | 9.0 MB–9.1 MB | 5 |
| Frame draw time (p50) | 1.1 ms | 1.1 ms–1.2 ms | 5 |
| Frame draw time (p95) | 1.4 ms | 1.3 ms–2.2 ms | 5 |
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
| Frame draw time (p50) | 3.3 ms | 3.3 ms–3.3 ms | 5 |
| Frame draw time (p95) | 3.5 ms | 3.4 ms–4.7 ms | 5 |
| Install size (`horon-tui`) | 5.0 MB | — | 1 |

Handoff: median first-frame time of a bare `horon-agent` launch (no arguments, on a PTY) minus the median first-frame time of a direct `horon-tui` launch against an already-running daemon, both on the same machine and build. The bare path also starts the daemon, so the number covers that startup, the sibling lookup and the `exec`; it is the extra cost of the handoff over launching the client itself.

Render bench: 200 frames on a 120×40 terminal.

Skipped: pty unavailable on Windows

<!-- bench-auto:end -->
