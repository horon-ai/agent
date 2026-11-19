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

<!-- bench-auto:start commit=e249038d0170c302206c22930c598f852a868c0c -->

## 헤드리스 (`horon-agent`)

이 구간은 `.github/workflows/benchmark-docs.yml` 이 자동으로 다시 씁니다. 손으로 고치지 않습니다.

- 커밋: `e249038d0170c302206c22930c598f852a868c0c`
- 측정 시각: 2026-11-19 01:55 UTC
- 실행: <https://github.com/horon-ai/agent/actions/runs/44430342278> (원본 JSON 은 OS별 `benchmark-<os>-<run_id>` 아티팩트)

OS마다 다른 GitHub 호스티드 러너에서 따로 잽니다. 기계가 다르므로 OS끼리 절대값을 비교하지 않고, 같은 OS 안의 상대 비교(horon 대비 배수)와 커밋 사이의 추세만 읽습니다.

| OS | 러너 | 바이너리 | 상태 |
| --- | --- | --- | --- |
| Linux | `ubuntu-24.04` | `horon-agent v0.1.0-dev (e249038)` | 성공 |
| macOS | `macos-latest` | `horon-agent v0.1.0-dev (e249038)` | 성공 |
| Windows | `windows-latest` | `horon-agent v0.1.0-dev (e249038)` | 성공 |

### Linux

#### Coding agent comparison (headless, mock model)

Host: linux/x86_64, 4 CPUs, Python 3.12.16. Baseline: **horon**. Samples per one-shot, skill and MCP cell: 5.

| Tool | Version | `--version` (median) | Install size |
|---|---|---|---|
| horon | horon-agent v0.1.0-dev (e249038) | 3.5 ms | 112.0 MB |
| jcode | jcode v0.99.1 (7206efd0c) | 10.5 ms | 163.4 MB |
| Claude Code | 2.1.317 (Claude Code) | 10.2 ms | 632.2 MB |
| Codex CLI | codex-cli 0.166.0 | 43.4 ms | 435.0 MB |
| OpenCode | 1.19.5 | 490.7 ms | 613.4 MB |
| pi | 1.5.0 | 243.2 ms | 109.0 MB |
| oh-my-pi (omp) | omp/18.18.0 | 665.6 ms | 413.8 MB |
| oh-my-openagent (omo) | omo 5.1.64 (engine: senpi 2026.11.5-1) | 118.3 ms | 520.7 MB |

**One-shot headless prompt** (includes startup, one `ls` tool call, final answer)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Tool round-trip | fsync calls | request bytes | Note |
|---|---|---|---|---|---|---|---|---|---|
| horon | 41.9 ms | 41.0 ms–56.5 ms | baseline | 39.1 MB | baseline | yes | 10 | 3,709 / 2,368 (1 tools) | — |
| jcode | 146.1 ms | 143.1 ms–210.3 ms | 3.49× more | 41.9 MB | 1.07× more | yes | 5 | 3,245 / 1,433 (1 tools) | — |
| Claude Code | 432.0 ms | 423.5 ms–442.0 ms | 10.31× more | 206.4 MB | 5.28× more | yes | — | 45,782 / 29,653 (12 tools) | — |
| Codex CLI | 458.4 ms | 426.6 ms–746.2 ms | 10.94× more | 202.3 MB | 5.18× more | yes | 6 | 39,432 / 17,271 (9 tools) | — |
| OpenCode | 2.73 s | 2.70 s–3.01 s | 65.15× more | 587.7 MB | 15.04× more | yes | 4 | 2,659 / 0 (0 tools) | — |
| pi | 408.1 ms | 404.3 ms–416.1 ms | 9.74× more | 113.7 MB | 2.91× more | yes | — | 5,502 / 2,841 (4 tools) | — |
| oh-my-pi (omp) | 1.96 s | 1.78 s–2.06 s | 46.80× more | 395.8 MB | 10.13× more | yes | 68 | 29,860 / 18,533 (11 tools) | — |
| oh-my-openagent (omo) | 6.06 s | 5.90 s–6.41 s | 144.55× more | 655.9 MB | 16.78× more | yes | 36 | 26,678 / 2,424 (2 tools) | — |

`request bytes` is the first model request of the first run as the mock server received it: JSON body bytes / tool catalog bytes (tools offered).

**Core-profile one-shot** (the one-shot turn without `--tools`, so the request carries the default core surface: the eager tools, `toolbox` and the listed-tool index)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | Note | request bytes |
|---|---|---|---|---|---|---|---|---|---|
| horon | 45.1 ms | 43.9 ms–46.1 ms | baseline | 40.9 MB | baseline | — | yes | — | 11,078 / 9,079 (8 tools) |
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
| horon | 36.3 ms | 35.2 ms–36.6 ms | baseline | 17.7 MB | baseline | skill-uri | yes | — |
| jcode | 56.9 ms | 55.6 ms–57.8 ms | 1.57× more | 40.6 MB | 2.30× more | skill-tool | yes | — |
| Claude Code | 434.4 ms | 423.4 ms–446.7 ms | 11.97× more | 206.9 MB | 11.71× more | skill-tool | yes | — |
| Codex CLI | 547.7 ms | 520.2 ms–578.7 ms | 15.09× more | 198.8 MB | 11.25× more | file-read | yes | — |
| OpenCode | 3.20 s | 3.15 s–3.37 s | 88.29× more | 600.6 MB | 34.00× more | skill-tool | yes | — |
| pi | 531.2 ms | 523.1 ms–539.8 ms | 14.64× more | 125.1 MB | 7.08× more | file-read | yes | — |
| oh-my-pi (omp) | 1.75 s | 1.72 s–1.80 s | 48.23× more | 341.1 MB | 19.31× more | file-read | yes | — |
| oh-my-openagent (omo) | 6.61 s | 6.46 s–6.67 s | 182.05× more | 666.2 MB | 37.71× more | file-read | yes | — |

**MCP tool call** (process start to final answer, including starting the fixture stdio MCP server and one `bench_echo` call)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | MCP ready | Note |
|---|---|---|---|---|---|---|---|---|---|
| horon | 61.5 ms | 61.3 ms–62.2 ms | baseline | 42.7 MB | baseline | mcp-call | yes | 45.6 ms | — |
| jcode | 175.4 ms | 174.8 ms–177.5 ms | 2.85× more | 53.7 MB | 1.26× more | mcp-action | yes | 144.4 ms | — |
| Claude Code | 538.7 ms | 525.3 ms–551.3 ms | 8.76× more | 224.6 MB | 5.26× more | mcp-tool | yes | 396.9 ms | — |
| Codex CLI | 474.1 ms | 456.3 ms–476.1 ms | 7.71× more | 195.7 MB | 4.59× more | mcp-tool | yes | 298.6 ms | — |
| OpenCode | 3.04 s | 2.97 s–3.08 s | 49.35× more | 565.8 MB | 13.26× more | mcp-tool | yes | 2.76 s | — |
| pi | 594.7 ms | 585.1 ms–616.6 ms | 9.67× more | 139.1 MB | 3.26× more | mcp-tool | yes | 524.0 ms | — |
| oh-my-pi (omp) | 1.79 s | 1.77 s–1.82 s | 29.12× more | 363.0 MB | 8.51× more | mcp-route | yes | 1.65 s | — |
| oh-my-openagent (omo) | 6.91 s | 6.71 s–6.96 s | 112.40× more | 677.3 MB | 15.88× more | mcp-tool | yes | 3.37 s | — |

**Session memory** (idle after 3 turn(s) per session; sizes are process-tree sums, RSS; PSS also recorded on Linux). "Each additional" is the least-squares slope across the measured session counts; 0 B means growth stayed below RSS sampling noise. Threads and descriptors come from the daemon's own `server:memory` after the turns. Processes are counted in the same tree walk as the RSS sample (PSS shares it on Linux), so the count and the bytes describe one observation; a PID shared by two session roots counts once, a root that already exited is counted as neither bytes nor process, and a cell that could not be read is —, never 0.

| Tool | First session | Each additional | vs horon (additional) | Threads/session | FDs/session | Processes (idle) | Processes/session | First (post-turn) | Each additional (post-turn) |
|---|---|---|---|---|---|---|---|---|---|
| horon | 36.9 MB | 146.6 KB | baseline | 0.68 | 1.00 | 1 | 0.00 | 44.4 MB | 456.6 KB |
| jcode | 42.4 MB | 104.0 KB | 1.41× less | 0.23 | — | 1 | 0.00 | 45.9 MB | 289.5 KB |
| Claude Code | 150.6 MB | 146.5 MB | 1,023× more | — | — | 1 | 1.00 | 187.2 MB | 180.1 MB |
| Codex CLI | 157.9 MB | 129.6 MB | 905.11× more | — | — | 5 | 0.57 | 161.8 MB | 163.5 MB |
| OpenCode | 244.6 MB | 468.3 KB | 3.19× more | — | — | 1 | 0.00 | 452.4 MB | 5.3 MB |
| pi | 108.0 MB | 109.3 MB | 763.17× more | — | — | 1 | 1.00 | 117.9 MB | 127.6 MB |
| oh-my-pi (omp) | 290.7 MB | 66.4 MB | 463.65× more | — | — | 1 | 1.00 | 377.4 MB | 358.2 MB |
| oh-my-openagent (omo) | 269.7 MB | 36.6 MB | 255.48× more | — | — | 1 | 1.00 | 604.1 MB | 539.6 MB |

Threads/descriptors unavailable: jcode (server:memory did not report fd_count)

**Post-turn tree RSS per session count**

| Tool | 1 session(s) | 5 session(s) | 10 session(s) |
|---|---|---|---|
| horon | 44.4 MB | 47.0 MB | 48.5 MB |
| jcode | 45.9 MB | 47.5 MB | 48.5 MB |
| Claude Code | 187.2 MB | 923.1 MB | 1.8 GB |
| Codex CLI | 161.8 MB | 809.6 MB | 1.6 GB |
| OpenCode | 452.4 MB | 515.6 MB | 502.9 MB |
| pi | 117.9 MB | 643.7 MB | 1.2 GB |
| oh-my-pi (omp) | 377.4 MB | 1.8 GB | 3.5 GB |
| oh-my-openagent (omo) | 604.1 MB | 2.7 GB | 5.3 GB |

**Live processes per session count** (idle / post-turn)

| Tool | 1 session(s) | 5 session(s) | 10 session(s) |
|---|---|---|---|
| horon | 1 / 1 | 1 / 1 | 1 / 1 |
| jcode | 1 / 1 | 1 / 1 | 1 / 1 |
| Claude Code | 1 / 1 | 5 / 5 | 10 / 10 |
| Codex CLI | 5 / 1 | 5 / 5 | 10 / 10 |
| OpenCode | 1 / 1 | 1 / 1 | 1 / 1 |
| pi | 1 / 1 | 5 / 5 | 10 / 10 |
| oh-my-pi (omp) | 1 / 1 | 5 / 5 | 10 / 10 |
| oh-my-openagent (omo) | 1 / 4 | 5 / 20 | 10 / 40 |

#### 기동

```text
Benchmarking: /home/runner/work/agent/agent/bench-bin/horon-agent
============================================================

--help (binary load):
  Min:    2.49 ms
  Max:    3.19 ms
  Mean:   2.62 ms
  Median: 2.54 ms
  Stdev:  0.21 ms

--version:
  Min:    2.35 ms
  Max:    2.80 ms
  Mean:   2.61 ms
  Median: 2.58 ms
  Stdev:  0.15 ms

Measuring isolated server startup (10 runs)...

Server ready (isolated socket connectable):
  Min:    20.59 ms
  Max:    20.88 ms
  Mean:   20.70 ms
  Median: 20.67 ms
  Stdev:  0.10 ms

Server startup stages (one extra run with HORON_AGENT_STARTUP_PROFILE=1, not timed):
  stage             at ms  delta ms   share
  panic_hook          0.0       0.0    0.1%
  logging_init        0.2       0.2   15.6%
  nofile_limit        0.4       0.3   24.5%
  perm_harden         0.5       0.0    0.7%
  args_parse          0.5       0.0    0.1%
  config_loaded       0.6       0.1   12.2%
  server_new          1.0       0.4   38.9%
  socket_bound        1.1       0.1    7.8%

Measuring harness bridge readiness (10 runs)...

Bridge stage: api socket connectable:
  Min:    20.67 ms
  Max:    20.82 ms
  Mean:   20.72 ms
  Median: 20.71 ms
  Stdev:  0.04 ms

Bridge stage: hello_ok:
  Min:    21.35 ms
  Max:    21.59 ms
  Mean:   21.46 ms
  Median: 21.45 ms
  Stdev:  0.08 ms

Bridge stage: no-op reply:
  Min:    21.73 ms
  Max:    21.97 ms
  Mean:   21.87 ms
  Median: 21.90 ms
  Stdev:  0.09 ms

Bridge total (socket -> no-op):
  Min:    21.73 ms
  Max:    21.97 ms
  Mean:   21.87 ms
  Median: 21.90 ms
  Stdev:  0.09 ms

============================================================
Summary:
  Binary load median:      ~2.5 ms
  Server ready median:     ~20.7 ms
  Bridge total median:     ~21.9 ms
```

#### 세션 로딩 (COLD)

```text
binary   /home/runner/work/agent/agent/bench-bin/examples/loading_probe (3a3ed1bbefef)
stages   /home/runner/work/agent/agent/bench-bin/examples/resume_stage_probe (0ef398c77010, 3 cold pass(es) per session fixture)
mode     cold
protocol COLD: every measured run gets its own freshly copied HOME; no derived cache from an earlier run exists, so each sample is a first load of the fixture.
fixtures /tmp/horon-loading-bench-u1gd5f05
note     --warmup 3 is ignored in cold mode: every sample gets a fresh HOME
fixture events=0 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    0.016/0.015    p95    0.019/0.018    delta  -0.000 ms  control-no-change
  skill_request_overlay p50    0.106/0.106    p95    0.139/0.127    delta  +0.000 ms  control-no-change
  mcp_config_load   p50    0.145/0.145    p95    0.178/0.183    delta  -0.000 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  session_save      p50    9.531/9.535    p95   15.414/11.414   delta  +0.004 ms  control-no-change
  total store_read_bytes p50 23080  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition not_applicable: this fixture has no session log
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 23064..23080 (4 distinct values)
  SPREAD   b session_save store_read_bytes 23060..23076 (5 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    2.749/2.766    p95    2.881/2.811    delta  +0.016 ms  control-no-change
  skill_request_overlay p50    4.844/4.892    p95    5.099/4.970    delta  +0.047 ms  control-no-change
  mcp_config_load   p50    0.148/0.149    p95    0.178/0.161    delta  +0.001 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.001/0.001    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  session_save      p50   12.488/12.423   p95   13.319/13.685   delta  -0.065 ms  control-no-change
  total store_read_bytes p50 23074  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition not_applicable: this fixture has no session log
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 23064..23080 (5 distinct values)
  SPREAD   b session_save store_read_bytes 23064..23084 (6 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.015/0.015    p95    0.030/0.016    delta  -0.000 ms  control-no-change
  skill_request_overlay p50    0.106/0.106    p95    0.135/0.140    delta  -0.000 ms  control-no-change
  mcp_config_load   p50    0.144/0.144    p95    0.145/0.168    delta  -0.000 ms  control-no-change
  attach_history    p50  248.263/247.264  p95  253.183/253.290  delta  -0.998 ms  control-no-change
  resume_ready      p50   54.033/54.008   p95   56.992/55.168   delta  -0.025 ms  control-no-change
  provider_dispatch p50    1.441/1.436    p95    1.463/1.855    delta  -0.005 ms  control-no-change
  session_save      p50    8.300/8.175    p95    8.684/12.207   delta  -0.126 ms  control-no-change
  total store_read_bytes p50 5311399  full_source_hashes p50 2  skill_dir_reads p50 1
  decomposition 3/3 cold pass(es); anchor attach_history p50 248.263 ms (load 204.512 + history 41.807)
    selection          p50     0.033 ms  min     0.032  max     0.033  ratio 0.0% of anchor
    checkpoint_recent  p50   164.839 ms  min   158.502  max   165.011  ratio 66.4% of anchor
    log_read           p50     0.087 ms  min     0.055  max     0.093  ratio 0.0% of anchor
    full_scan          p50    22.869 ms  min    22.548  max    23.862  ratio 9.2% of anchor
    blob_read          p50     0.006 ms  min     0.003  max     0.008  ratio 0.0% of anchor
    graph_replay       p50    17.676 ms  min    15.585  max    18.751  ratio 7.1% of anchor
    metadata_json      p50     0.007 ms  min     0.006  max     0.008  ratio 0.0% of anchor
    messages_json      p50     0.964 ms  min     0.955  max     1.050  ratio 0.4% of anchor
    messages_decode    p50     0.955 ms  min     0.929  max     1.061  ratio 0.4% of anchor
    source_hash        p50     0.465 ms  min     0.450  max     0.487  ratio 0.2% of anchor
    history_rescan     p50    45.181 ms  min    42.459  max    46.785  ratio 18.2% of anchor
    page_index         p50    50.899 ms  min    46.472  max    50.990  ratio 20.5% of anchor
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189187..4189191 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099129..1099133 (3 distinct values)
  SPREAD   a session_save store_read_bytes 23062..23082 (8 distinct values)
  SPREAD   b attach_history store_read_bytes 4189187..4189191 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099129..1099133 (3 distinct values)
  SPREAD   b session_save store_read_bytes 23066..23086 (6 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    2.775/2.715    p95    2.847/2.804    delta  -0.060 ms  control-no-change
  skill_request_overlay p50    4.878/4.824    p95    4.948/5.087    delta  -0.055 ms  control-no-change
  mcp_config_load   p50    0.149/0.148    p95    0.170/0.157    delta  -0.001 ms  control-no-change
  attach_history    p50  248.978/248.945  p95  255.057/253.611  delta  -0.033 ms  control-no-change
  resume_ready      p50   53.985/53.959   p95   54.883/54.935   delta  -0.025 ms  control-no-change
  provider_dispatch p50    1.427/1.424    p95    1.494/1.463    delta  -0.003 ms  control-no-change
  session_save      p50    8.252/8.042    p95    9.477/8.839    delta  -0.210 ms  control-no-change
  total store_read_bytes p50 5311394  full_source_hashes p50 2  skill_dir_reads p50 1
  decomposition 3/3 cold pass(es); anchor attach_history p50 248.978 ms (load 207.650 + history 41.167)
    selection          p50     0.035 ms  min     0.031  max     0.037  ratio 0.0% of anchor
    checkpoint_recent  p50   164.376 ms  min   162.987  max   169.719  ratio 66.0% of anchor
    log_read           p50     0.076 ms  min     0.070  max     0.081  ratio 0.0% of anchor
    full_scan          p50    22.991 ms  min    22.797  max    23.338  ratio 9.2% of anchor
    blob_read          p50     0.005 ms  min     0.004  max     0.006  ratio 0.0% of anchor
    graph_replay       p50    17.765 ms  min    16.314  max    18.600  ratio 7.1% of anchor
    metadata_json      p50     0.004 ms  min     0.004  max     0.008  ratio 0.0% of anchor
    messages_json      p50     1.003 ms  min     0.965  max     1.032  ratio 0.4% of anchor
    messages_decode    p50     0.985 ms  min     0.925  max     0.985  ratio 0.4% of anchor
    source_hash        p50     0.467 ms  min     0.446  max     0.489  ratio 0.2% of anchor
    history_rescan     p50    46.646 ms  min    42.396  max    48.153  ratio 18.7% of anchor
    page_index         p50    51.003 ms  min    46.487  max    52.900  ratio 20.5% of anchor
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189187..4189191 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099129..1099133 (3 distinct values)
  SPREAD   a session_save store_read_bytes 23066..23078 (6 distinct values)
  SPREAD   b attach_history store_read_bytes 4189189..4189191 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099132..1099133 (2 distinct values)
  SPREAD   b session_save store_read_bytes 23044..23082 (7 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.015/0.016    p95    0.017/0.026    delta  +0.001 ms  control-drift
  skill_request_overlay p50    0.108/0.106    p95    0.145/0.144    delta  -0.001 ms  control-no-change
  mcp_config_load   p50    0.146/0.146    p95    0.150/0.168    delta  -0.000 ms  control-no-change
  attach_history    p50 3144.049/3091.298 p95 3164.870/3197.200 delta -52.751 ms  control-no-change
  resume_ready      p50  593.887/593.726  p95  600.965/601.657  delta  -0.162 ms  control-no-change
  provider_dispatch p50   15.807/15.862   p95   16.180/16.107   delta  +0.054 ms  control-no-change
  session_save      p50    9.067/8.970    p95   12.619/9.748    delta  -0.097 ms  control-no-change
  total store_read_bytes p50 53417608  full_source_hashes p50 2  skill_dir_reads p50 1
  decomposition 3/3 cold pass(es); anchor attach_history p50 3144.049 ms (load 2636.715 + history 494.099)
    selection          p50     0.043 ms  min     0.043  max     0.047  ratio 0.0% of anchor
    checkpoint_recent  p50  2099.385 ms  min  2083.666  max  2109.971  ratio 66.8% of anchor
    log_read           p50     0.654 ms  min     0.571  max     0.821  ratio 0.0% of anchor
    full_scan          p50   244.841 ms  min   244.453  max   246.049  ratio 7.8% of anchor
    blob_read          p50     0.058 ms  min     0.055  max     0.059  ratio 0.0% of anchor
    graph_replay       p50   222.830 ms  min   219.056  max   231.784  ratio 7.1% of anchor
    metadata_json      p50     0.006 ms  min     0.006  max     0.006  ratio 0.0% of anchor
    messages_json      p50     8.880 ms  min     8.824  max     8.942  ratio 0.3% of anchor
    messages_decode    p50    12.947 ms  min    12.206  max    13.028  ratio 0.4% of anchor
    source_hash        p50     4.434 ms  min     4.404  max     4.440  ratio 0.1% of anchor
    history_rescan     p50   499.681 ms  min   498.807  max   520.433  ratio 15.9% of anchor
    page_index         p50   517.909 ms  min   515.285  max   530.147  ratio 16.5% of anchor
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184285..42184289 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210239..11210243 (2 distinct values)
  SPREAD   a session_save store_read_bytes 23059..23093 (7 distinct values)
  SPREAD   b attach_history store_read_bytes 42184285..42184289 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210239..11210243 (3 distinct values)
  SPREAD   b session_save store_read_bytes 23057..23088 (8 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    2.701/2.719    p95    2.789/2.819    delta  +0.018 ms  control-no-change
  skill_request_overlay p50    4.668/4.665    p95    4.744/4.704    delta  -0.003 ms  control-no-change
  mcp_config_load   p50    0.148/0.148    p95    0.155/0.159    delta  -0.000 ms  control-no-change
  attach_history    p50 2989.217/2977.297 p95 3082.346/3090.316 delta -11.920 ms  control-no-change
  resume_ready      p50  576.214/577.572  p95  590.262/588.816  delta  +1.358 ms  control-no-change
  provider_dispatch p50   15.536/15.485   p95   15.903/16.492   delta  -0.051 ms  control-no-change
  session_save      p50    8.368/8.230    p95    8.739/8.992    delta  -0.138 ms  control-no-change
  total store_read_bytes p50 53417607  full_source_hashes p50 2  skill_dir_reads p50 1
  decomposition 3/3 cold pass(es); anchor attach_history p50 2989.217 ms (load 2521.101 + history 464.603)
    selection          p50     0.037 ms  min     0.035  max     0.039  ratio 0.0% of anchor
    checkpoint_recent  p50  2063.039 ms  min  2026.064  max  2064.311  ratio 69.0% of anchor
    log_read           p50     0.529 ms  min     0.473  max     0.567  ratio 0.0% of anchor
    full_scan          p50   243.938 ms  min   243.477  max   243.945  ratio 8.2% of anchor
    blob_read          p50     0.046 ms  min     0.030  max     0.049  ratio 0.0% of anchor
    graph_replay       p50   199.660 ms  min   197.220  max   205.446  ratio 6.7% of anchor
    metadata_json      p50     0.007 ms  min     0.006  max     0.007  ratio 0.0% of anchor
    messages_json      p50     8.861 ms  min     8.726  max     8.864  ratio 0.3% of anchor
    messages_decode    p50    12.357 ms  min    11.707  max    12.498  ratio 0.4% of anchor
    source_hash        p50     4.413 ms  min     4.394  max     4.426  ratio 0.1% of anchor
    history_rescan     p50   486.790 ms  min   483.183  max   488.669  ratio 16.3% of anchor
    page_index         p50   507.505 ms  min   496.842  max   511.243  ratio 17.0% of anchor
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184285..42184289 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210239..11210243 (3 distinct values)
  SPREAD   a session_save store_read_bytes 23069..23089 (6 distinct values)
  SPREAD   b attach_history store_read_bytes 42184285..42184289 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210239..11210243 (2 distinct values)
  SPREAD   b session_save store_read_bytes 23061..23085 (7 distinct values)
report   bench-results/loading-cold.json
mode cold, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### 세션 로딩 (WARM)

```text
binary   /home/runner/work/agent/agent/bench-bin/examples/loading_probe (3a3ed1bbefef)
stages   /home/runner/work/agent/agent/bench-bin/examples/resume_stage_probe (0ef398c77010, 0 cold pass(es) per session fixture)
mode     warm
protocol WARM: one HOME per side, --warmup discarded runs per side, then measured runs alternate between the two sides on those same HOMEs.
fixtures /tmp/horon-loading-bench-gwmw45ap
fixture events=0 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    0.007/0.007    p95    0.008/0.008    delta  -0.000 ms  control-no-change
  skill_request_overlay p50    0.038/0.038    p95    0.051/0.038    delta  -0.000 ms  control-no-change
  mcp_config_load   p50    0.054/0.053    p95    0.072/0.085    delta  -0.000 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  session_save      p50    4.230/4.185    p95    4.317/4.383    delta  -0.046 ms  control-no-change
  total store_read_bytes p50 23788  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition not_applicable: this fixture has no session log
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23066, 23077, 23081, 23085, 23785, 23792, 23796, 24507, 24516, 24523]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23073, 23081, 23792, 23796, 23800, 24515, 24519, 24523]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23066..24523 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23073..24523 (8 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    1.304/1.326    p95    1.439/1.369    delta  +0.021 ms  control-no-change
  skill_request_overlay p50    1.845/1.849    p95    1.948/2.001    delta  +0.004 ms  control-no-change
  mcp_config_load   p50    0.058/0.058    p95    0.076/0.102    delta  -0.000 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  session_save      p50    5.239/5.170    p95    5.350/5.533    delta  -0.069 ms  control-no-change
  total store_read_bytes p50 23789  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition not_applicable: this fixture has no session log
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23069, 23081, 23085, 23784, 23794, 23800, 24515, 24527]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23073, 23077, 23085, 23792, 23800, 24495, 24513, 24519, 24523]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23069..24527 (8 distinct values)
  SPREAD   b session_save store_read_bytes 23073..24523 (9 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.006/0.006    p95    0.006/0.006    delta  +0.000 ms  control-no-change
  skill_request_overlay p50    0.038/0.038    p95    0.039/0.039    delta  -0.000 ms  control-no-change
  mcp_config_load   p50    0.053/0.053    p95    0.054/0.054    delta  -0.000 ms  control-no-change
  attach_history    p50   58.883/58.720   p95   59.777/59.254   delta  -0.163 ms  control-no-change
  resume_ready      p50   27.342/27.410   p95   27.622/30.713   delta  +0.068 ms  control-no-change
  provider_dispatch p50    0.989/0.995    p95    1.004/1.033    delta  +0.006 ms  control-no-change
  session_save      p50    4.927/4.726    p95    5.259/6.119    delta  -0.201 ms  control-no-change
  total store_read_bytes p50 2841375  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition skipped: --stage-probe-samples is 0
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23797, 24514, 25233, 25237, 25948, 25968, 26671, 26675]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23797, 23801, 24508, 24512, 25231, 25237, 25940, 25962, 26667, 26681]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23797..26675 (8 distinct values)
  SPREAD   b session_save store_read_bytes 23797..26681 (10 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    1.342/1.320    p95    1.522/1.497    delta  -0.022 ms  control-no-change
  skill_request_overlay p50    1.865/1.862    p95    1.944/2.047    delta  -0.003 ms  control-no-change
  mcp_config_load   p50    0.058/0.058    p95    0.059/0.068    delta  -0.000 ms  control-no-change
  attach_history    p50   57.921/58.136   p95   58.353/58.560   delta  +0.215 ms  control-no-change
  resume_ready      p50   27.160/27.176   p95   27.376/27.275   delta  +0.016 ms  control-no-change
  provider_dispatch p50    0.975/0.973    p95    1.014/0.995    delta  -0.002 ms  control-no-change
  session_save      p50    5.207/5.251    p95    5.563/5.330    delta  +0.044 ms  control-no-change
  total store_read_bytes p50 2841379  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition skipped: --stage-probe-samples is 0
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23781, 23793, 24516, 24524, 25235, 25243, 25958, 25962, 26665]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23793, 23797, 24500, 24510, 25229, 25241, 25960, 25964, 26667, 26675]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23781..26665 (9 distinct values)
  SPREAD   b session_save store_read_bytes 23793..26675 (10 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.006/0.006    p95    0.007/0.007    delta  -0.000 ms  control-no-change
  skill_request_overlay p50    0.038/0.038    p95    0.039/0.038    delta  -0.000 ms  control-no-change
  mcp_config_load   p50    0.053/0.053    p95    0.059/0.060    delta  +0.000 ms  control-no-change
  attach_history    p50  669.065/672.622  p95  679.374/681.146  delta  +3.557 ms  control-no-change
  resume_ready      p50  322.469/324.479  p95  329.262/326.833  delta  +2.010 ms  control-no-change
  provider_dispatch p50   11.404/11.430   p95   11.648/11.572   delta  +0.026 ms  control-no-change
  session_save      p50    4.839/4.962    p95    4.955/5.146    delta  +0.123 ms  control-no-change
  total store_read_bytes p50 28640398  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition skipped: --stage-probe-samples is 0
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23783, 23789, 24504, 24522, 25241, 25245, 25948, 26667, 26683]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23787, 23793, 24510, 24522, 25225, 25956, 26675, 26679]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23783..26683 (9 distinct values)
  SPREAD   b session_save store_read_bytes 23787..26679 (8 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    1.331/1.302    p95    1.463/1.321    delta  -0.029 ms  control-no-change
  skill_request_overlay p50    1.886/1.861    p95    2.031/2.041    delta  -0.025 ms  control-no-change
  mcp_config_load   p50    0.059/0.058    p95    0.067/0.089    delta  -0.000 ms  control-no-change
  attach_history    p50  674.123/675.665  p95  687.788/688.425  delta  +1.542 ms  control-no-change
  resume_ready      p50  331.630/334.303  p95  337.481/337.917  delta  +2.673 ms  control-no-change
  provider_dispatch p50   11.426/11.456   p95   11.618/11.701   delta  +0.029 ms  control-no-change
  session_save      p50    5.438/5.590    p95    5.892/6.102    delta  +0.152 ms  control-no-change
  total store_read_bytes p50 28640388  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition skipped: --stage-probe-samples is 0
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23802, 23816, 24517, 24523, 25218, 25244, 25945, 25955, 26666, 26678]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23796, 23806, 24517, 24535, 25230, 25240, 25953, 25955, 26670, 26674]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23802..26678 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23796..26674 (10 distinct values)
report   bench-results/loading-warm.json
mode warm, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### Tool latency

| Tool case | median | p95 | min | max | samples |
|---|---|---|---|---|---|
| read (tiny file) | 0.1 ms | 0.2 ms | 0.1 ms | 0.2 ms | 5 |
| read (1000 lines) | 0.2 ms | 0.3 ms | 0.2 ms | 0.3 ms | 5 |
| read (10 lines @ offset) | 0.2 ms | 0.2 ms | 0.2 ms | 0.2 ms | 5 |
| read (repo Cargo.toml) | 0.1 ms | 0.2 ms | 0.1 ms | 0.2 ms | 5 |
| write (small) | 0.6 ms | 0.7 ms | 0.5 ms | 0.7 ms | 5 |
| write (10KB) | 0.7 ms | 0.8 ms | 0.7 ms | 0.8 ms | 5 |
| edit (simple replace) | 0.6 ms | 0.7 ms | 0.6 ms | 0.7 ms | 5 |
| ls (scripts dir) | 0.5 ms | 0.5 ms | 0.4 ms | 0.5 ms | 5 |
| agentgrep (grep in tree) | 1.2 ms | 1.3 ms | 1.1 ms | 1.3 ms | 5 |
| agentgrep (find **/*.rs) | 0.9 ms | 1.0 ms | 0.9 ms | 1.0 ms | 5 |
| bash (true) | 1.3 ms | 1.6 ms | 1.2 ms | 1.6 ms | 5 |
| bash (echo hello) | 1.3 ms | 1.4 ms | 1.2 ms | 1.4 ms | 5 |
| bash (ls fixture tree) | 2.2 ms | 2.3 ms | 2.2 ms | 2.3 ms | 5 |
| invalid tool (dispatch only) | 0.0 ms | 0.1 ms | 0.0 ms | 0.1 ms | 5 |

#### Tool CPU per call

| Tool case | daemon cpu median | min | max | child cpu median | voluntary switches | involuntary switches | windows × calls | source |
|---|---|---|---|---|---|---|---|---|
| read (tiny file) | 0.120 ms | 0.113 ms | 0.123 ms | 0.000 ms | 5.22 | 0.00 | 5 × 50 | schedstat |
| read (1000 lines) | 0.209 ms | 0.199 ms | 0.213 ms | 0.000 ms | 5.34 | 0.00 | 5 × 50 | schedstat |
| read (10 lines @ offset) | 0.205 ms | 0.200 ms | 0.206 ms | 0.000 ms | 5.28 | 0.00 | 5 × 50 | schedstat |
| read (repo Cargo.toml) | 0.151 ms | 0.146 ms | 0.163 ms | 0.000 ms | 5.24 | 0.00 | 5 × 50 | schedstat |
| write (small) | 0.424 ms | 0.411 ms | 0.431 ms | 0.000 ms | 16.90 | 0.32 | 5 × 50 | schedstat |
| write (10KB) | 0.560 ms | 0.551 ms | 0.568 ms | 0.000 ms | 17.62 | 0.04 | 5 × 50 | schedstat |
| edit (simple replace) | 0.427 ms | 0.399 ms | 0.467 ms | 0.000 ms | 17.24 | 0.28 | 5 × 50 | schedstat |
| ls (scripts dir) | 0.436 ms | 0.425 ms | 0.460 ms | 0.000 ms | 5.24 | 0.00 | 5 × 50 | schedstat |
| agentgrep (grep in tree) | 0.832 ms | 0.824 ms | 0.846 ms | 0.000 ms | 10.08 | 0.10 | 5 × 50 | schedstat |
| agentgrep (find **/*.rs) | 0.839 ms | 0.821 ms | 0.864 ms | 0.000 ms | 5.24 | 0.00 | 5 × 50 | schedstat |
| bash (true) | 0.406 ms | 0.388 ms | 0.422 ms | 1.000 ms | 7.76 | 0.00 | 5 × 50 | schedstat |
| bash (echo hello) | 0.431 ms | 0.421 ms | 0.438 ms | 1.000 ms | 8.12 | 0.00 | 5 × 50 | schedstat |
| bash (ls fixture tree) | 0.510 ms | 0.503 ms | 0.513 ms | 1.800 ms | 8.78 | 0.00 | 5 × 50 | schedstat |
| invalid tool (dispatch only) | 0.023 ms | 0.022 ms | 0.024 ms | 0.000 ms | 2.96 | 0.00 | 5 × 50 | schedstat |

#### Reload storm

1 resume(s) per level; each level resumes sessions no other level has opened, from a freshly started daemon.

| Concurrent resumes | history median | p95 | max | storm wall | failures |
|---|---|---|---|---|---|
| 1 | 57.1 ms | 57.1 ms | 57.1 ms | 57.7 ms | 0 |
| 10 | 155.8 ms | 190.4 ms | 190.4 ms | 191.3 ms | 0 |
| 40 | 656.3 ms | 666.4 ms | 667.1 ms | 667.8 ms | 0 |

#### Compile loop

| Checkpoint | Touched | median | min | max | samples |
|---|---|---|---|---|---|
| warm_noop | — | 209.2 ms | 209.2 ms | 209.2 ms | 1 |
| leaf_touch | src/main.rs | 254.5 ms | 254.5 ms | 254.5 ms | 1 |
| core_touch | crates/app-core/src/lib.rs | 15.78 s | 15.78 s | 15.78 s | 1 |
| build_meta_touch | crates/build-meta/src/lib.rs | 22.40 s | 22.40 s | 22.40 s | 1 |

### macOS

#### Coding agent comparison (headless, mock model)

Host: darwin/arm64, 3 CPUs, Python 3.12.10. Baseline: **horon**. Samples per one-shot, skill and MCP cell: 5.

Skipped: syscalls (strace not available)

| Tool | Version | `--version` (median) | Install size |
|---|---|---|---|
| horon | horon-agent v0.1.0-dev (e249038) | 8.6 ms | 94.2 MB |
| jcode | jcode v0.99.1 (7206efd0c) | 11.0 ms | 136.3 MB |
| Claude Code | 2.1.317 (Claude Code) | 24.2 ms | 596.8 MB |
| Codex CLI | codex-cli 0.166.0 | 98.3 ms | 346.5 MB |
| OpenCode | 1.19.5 | 466.5 ms | 344.6 MB |
| pi | 1.5.0 | 387.4 ms | 109.0 MB |
| oh-my-pi (omp) | omp/18.18.0 | 998.5 ms | 274.2 MB |
| oh-my-openagent (omo) | omo 5.1.64 (engine: senpi 2026.11.5-1) | 203.4 ms | 502.3 MB |

**One-shot headless prompt** (includes startup, one `ls` tool call, final answer)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Tool round-trip | request bytes | Note |
|---|---|---|---|---|---|---|---|---|
| horon | 85.9 ms | 77.7 ms–508.4 ms | baseline | 29.7 MB | baseline | yes | 3,597 / 2,368 (1 tools) | — |
| jcode | 591.9 ms | 406.6 ms–672.6 ms | 6.89× more | 34.0 MB | 1.14× more | yes | 3,133 / 1,433 (1 tools) | — |
| Claude Code | 560.1 ms | 441.6 ms–752.6 ms | 6.52× more | 198.9 MB | 6.69× more | yes | 45,798 / 29,653 (12 tools) | — |
| Codex CLI | 816.5 ms | 446.8 ms–1.54 s | 9.51× more | 132.9 MB | 4.47× more | yes | 39,453 / 17,271 (9 tools) | — |
| OpenCode | 3.86 s | 3.73 s–5.54 s | 44.98× more | 784.3 MB | 26.38× more | yes | 2,659 / 0 (0 tools) | — |
| pi | 689.2 ms | 643.7 ms–792.8 ms | 8.03× more | 126.2 MB | 4.24× more | yes | 5,513 / 2,841 (4 tools) | — |
| oh-my-pi (omp) | 3.17 s | 2.50 s–3.46 s | 36.93× more | 458.4 MB | 15.42× more | yes | 29,853 / 18,533 (11 tools) | — |
| oh-my-openagent (omo) | 12.30 s | 11.57 s–12.52 s | 143.25× more | 874.9 MB | 29.42× more | yes | 27,493 / 2,424 (2 tools) | — |

`request bytes` is the first model request of the first run as the mock server received it: JSON body bytes / tool catalog bytes (tools offered).

**Core-profile one-shot** (the one-shot turn without `--tools`, so the request carries the default core surface: the eager tools, `toolbox` and the listed-tool index)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | Note | request bytes |
|---|---|---|---|---|---|---|---|---|---|
| horon | 101.9 ms | 69.8 ms–120.7 ms | baseline | 30.9 MB | baseline | — | yes | — | 10,966 / 9,079 (8 tools) |
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
| horon | 69.2 ms | 59.1 ms–134.5 ms | baseline | 24.1 MB | baseline | skill-uri | yes | — |
| jcode | 296.3 ms | 274.8 ms–325.5 ms | 4.28× more | 38.4 MB | 1.59× more | skill-tool | yes | — |
| Claude Code | 600.3 ms | 531.7 ms–690.6 ms | 8.67× more | 198.6 MB | 8.25× more | skill-tool | yes | — |
| Codex CLI | 561.8 ms | 507.9 ms–793.9 ms | 8.12× more | 130.5 MB | 5.42× more | file-read | yes | — |
| OpenCode | 5.30 s | 4.93 s–6.19 s | 76.55× more | 782.5 MB | 32.50× more | skill-tool | yes | — |
| pi | 789.5 ms | 734.8 ms–1.17 s | 11.41× more | 134.8 MB | 5.60× more | file-read | yes | — |
| oh-my-pi (omp) | 3.00 s | 2.27 s–3.74 s | 43.40× more | 459.9 MB | 19.10× more | file-read | yes | — |
| oh-my-openagent (omo) | 9.80 s | 9.72 s–13.91 s | 141.57× more | 897.1 MB | 37.26× more | file-read | yes | — |

**MCP tool call** (process start to final answer, including starting the fixture stdio MCP server and one `bench_echo` call)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | MCP ready | Note |
|---|---|---|---|---|---|---|---|---|---|
| horon | 106.6 ms | 101.2 ms–123.9 ms | baseline | 38.3 MB | baseline | mcp-call | yes | 81.2 ms | — |
| jcode | 404.8 ms | 396.2 ms–427.8 ms | 3.80× more | 48.1 MB | 1.26× more | mcp-action | yes | 373.9 ms | — |
| Claude Code | 645.1 ms | 566.6 ms–956.8 ms | 6.05× more | 224.7 MB | 5.87× more | mcp-tool | yes | 520.1 ms | — |
| Codex CLI | 514.6 ms | 460.6 ms–620.4 ms | 4.83× more | 137.6 MB | 3.59× more | mcp-tool | yes | 316.3 ms | — |
| OpenCode | 5.23 s | 4.85 s–5.45 s | 49.03× more | 701.0 MB | 18.31× more | mcp-tool | yes | 4.53 s | — |
| pi | 1.05 s | 869.6 ms–1.12 s | 9.85× more | 146.7 MB | 3.83× more | mcp-tool | yes | 892.8 ms | — |
| oh-my-pi (omp) | 2.68 s | 2.50 s–3.13 s | 25.16× more | 433.5 MB | 11.32× more | mcp-route | yes | 2.28 s | — |
| oh-my-openagent (omo) | — | — | — | — | — | mcp-tool | no | — | failed: the fixture sentinel never came back |

**Session memory** (idle after 3 turn(s) per session; sizes are process-tree sums, RSS; PSS also recorded on Linux). "Each additional" is the least-squares slope across the measured session counts; 0 B means growth stayed below RSS sampling noise. Threads and descriptors come from the daemon's own `server:memory` after the turns. Processes are counted in the same tree walk as the RSS sample (PSS shares it on Linux), so the count and the bytes describe one observation; a PID shared by two session roots counts once, a root that already exited is counted as neither bytes nor process, and a cell that could not be read is —, never 0.

| Tool | First session | Each additional | vs horon (additional) | Threads/session | FDs/session | Processes (idle) | Processes/session | First (post-turn) | Each additional (post-turn) |
|---|---|---|---|---|---|---|---|---|---|
| horon | 28.8 MB | 128.5 KB | baseline | 0.43 | 1.00 | 1 | 0.00 | 35.1 MB | 369.7 KB |
| jcode | 36.4 MB | 207.2 KB | 1.61× more | — | — | 1 | 0.00 | 40.0 MB | 365.5 KB |
| Claude Code | 157.9 MB | 145.5 MB | 1,160× more | — | — | 1 | 1.00 | 190.5 MB | 182.2 MB |
| Codex CLI | 114.3 MB | 78.4 MB | 624.76× more | — | — | 2 | 0.89 | 106.9 MB | 107.8 MB |
| OpenCode | 258.4 MB | 978.6 KB | 7.61× more | — | — | 1 | 0.00 | 564.5 MB | 24.9 MB |
| pi | 114.5 MB | 115.8 MB | 922.66× more | — | — | 1 | 1.00 | 147.8 MB | 147.9 MB |
| oh-my-pi (omp) | 347.5 MB | 166.2 MB | 1,324× more | — | — | 1 | 1.00 | 438.9 MB | 327.0 MB |
| oh-my-openagent (omo) | 57.1 MB | 43.1 MB | 343.73× more | — | — | 1 | 1.00 | 845.5 MB | 326.5 MB |

Threads/descriptors unavailable: jcode (server:memory did not report thread_count, fd_count)

**Post-turn tree RSS per session count**

| Tool | 1 session(s) | 5 session(s) | 10 session(s) |
|---|---|---|---|
| horon | 35.1 MB | 36.3 MB | 38.3 MB |
| jcode | 40.0 MB | 41.5 MB | 43.2 MB |
| Claude Code | 190.5 MB | 931.7 MB | 1.8 GB |
| Codex CLI | 106.9 MB | 536.4 MB | 1.1 GB |
| OpenCode | 564.5 MB | 581.9 MB | 782.3 MB |
| pi | 147.8 MB | 738.1 MB | 1.4 GB |
| oh-my-pi (omp) | 438.9 MB | 2.0 GB | 3.3 GB |
| oh-my-openagent (omo) | 845.5 MB | 2.9 GB | 3.8 GB |

**Live processes per session count** (idle / post-turn)

| Tool | 1 session(s) | 5 session(s) | 10 session(s) |
|---|---|---|---|
| horon | 1 / 1 | 1 / 1 | 1 / 1 |
| jcode | 1 / 1 | 1 / 1 | 1 / 1 |
| Claude Code | 1 / 1 | 5 / 5 | 10 / 10 |
| Codex CLI | 2 / 1 | 5 / 5 | 10 / 10 |
| OpenCode | 1 / 1 | 1 / 1 | 1 / 1 |
| pi | 1 / 1 | 5 / 5 | 10 / 10 |
| oh-my-pi (omp) | 1 / 1 | 5 / 5 | 10 / 10 |
| oh-my-openagent (omo) | 1 / 4 | 5 / 20 | 10 / 40 |

#### 기동

```text
Benchmarking: /Users/runner/work/agent/agent/bench-bin/horon-agent
============================================================

--help (binary load):
  Min:    9.54 ms
  Max:    16.84 ms
  Mean:   12.59 ms
  Median: 12.38 ms
  Stdev:  2.85 ms

--version:
  Min:    9.09 ms
  Max:    17.60 ms
  Mean:   11.92 ms
  Median: 11.64 ms
  Stdev:  2.46 ms

Measuring isolated server startup (10 runs)...

Server ready (isolated socket connectable):
  Min:    28.37 ms
  Max:    103.10 ms
  Mean:   78.17 ms
  Median: 96.41 ms
  Stdev:  31.29 ms

Server startup stages (one extra run with HORON_AGENT_STARTUP_PROFILE=1, not timed):
  stage             at ms  delta ms   share
  panic_hook          0.0       0.0    0.0%
  logging_init        0.9       0.9   10.1%
  nofile_limit        1.2       0.3    2.9%
  perm_harden         1.2       0.1    0.7%
  args_parse          1.2       0.0    0.0%
  config_loaded       8.4       7.2   79.8%
  server_new          8.7       0.3    3.1%
  socket_bound        9.0       0.3    3.3%

Measuring harness bridge readiness (10 runs)...

Bridge stage: api socket connectable:
  Min:    55.53 ms
  Max:    123.67 ms
  Mean:   84.89 ms
  Median: 85.59 ms
  Stdev:  24.87 ms

Bridge stage: hello_ok:
  Min:    56.95 ms
  Max:    125.85 ms
  Mean:   86.45 ms
  Median: 87.89 ms
  Stdev:  25.16 ms

Bridge stage: no-op reply:
  Min:    57.89 ms
  Max:    126.98 ms
  Mean:   87.40 ms
  Median: 89.33 ms
  Stdev:  25.25 ms

Bridge total (socket -> no-op):
  Min:    57.89 ms
  Max:    126.98 ms
  Mean:   87.40 ms
  Median: 89.33 ms
  Stdev:  25.25 ms

============================================================
Summary:
  Binary load median:      ~12.4 ms
  Server ready median:     ~96.4 ms
  Bridge total median:     ~89.3 ms
```

#### 세션 로딩 (COLD)

```text
binary   /Users/runner/work/agent/agent/bench-bin/examples/loading_probe (f9cbe8ee6468)
stages   /Users/runner/work/agent/agent/bench-bin/examples/resume_stage_probe (02a293eb6b3a, 3 cold pass(es) per session fixture)
mode     cold
protocol COLD: every measured run gets its own freshly copied HOME; no derived cache from an earlier run exists, so each sample is a first load of the fixture.
fixtures /var/folders/h5/t85nd63d71x27_8gl5gw3l_w0000gn/T/horon-loading-bench-prwm5zsm
note     --warmup 3 is ignored in cold mode: every sample gets a fresh HOME
fixture events=0 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    0.023/0.021    p95    0.033/0.029    delta  -0.002 ms  control-no-change
  skill_request_overlay p50    0.217/0.207    p95    0.237/0.244    delta  -0.010 ms  control-no-change
  mcp_config_load   p50    0.253/0.239    p95    0.308/0.285    delta  -0.014 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.006    delta  +0.000 ms  control-no-change
  session_save      p50   11.894/11.400   p95   27.078/12.021   delta  -0.494 ms  control-no-change
  total store_read_bytes p50 22944  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition not_applicable: this fixture has no session log
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 22934..22958 (6 distinct values)
  SPREAD   b session_save store_read_bytes 22926..22958 (7 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    3.836/3.659    p95    9.817/4.245    delta  -0.178 ms  control-no-change
  skill_request_overlay p50    4.640/4.568    p95    7.891/5.726    delta  -0.071 ms  control-no-change
  mcp_config_load   p50    0.246/0.231    p95    0.340/0.280    delta  -0.016 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  session_save      p50   13.268/13.730   p95   32.700/30.599   delta  +0.461 ms  control-no-change
  total store_read_bytes p50 22948  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition not_applicable: this fixture has no session log
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 22934..22958 (6 distinct values)
  SPREAD   b session_save store_read_bytes 22906..22954 (7 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.024/0.024    p95    0.035/0.040    delta  +0.000 ms  control-no-change
  skill_request_overlay p50    0.229/0.240    p95    0.307/0.369    delta  +0.011 ms  control-no-change
  mcp_config_load   p50    0.270/0.295    p95    0.331/0.469    delta  +0.026 ms  control-no-change
  attach_history    p50  246.991/256.570  p95  560.073/326.425  delta  +9.579 ms  control-no-change
  resume_ready      p50   61.192/64.241   p95   81.137/76.160   delta  +3.049 ms  control-no-change
  provider_dispatch p50    1.113/1.198    p95    1.502/1.401    delta  +0.084 ms  control-no-change
  session_save      p50   12.973/17.056   p95   74.764/99.364   delta  +4.084 ms  control-no-change
  total store_read_bytes p50 5311304  full_source_hashes p50 2  skill_dir_reads p50 1
  decomposition 3/3 cold pass(es); anchor attach_history p50 246.991 ms (load 209.983 + history 36.176)
    selection          p50     0.057 ms  min     0.046  max     0.080  ratio 0.0% of anchor
    checkpoint_recent  p50   187.116 ms  min   172.291  max   197.368  ratio 75.8% of anchor
    log_read           p50     0.072 ms  min     0.054  max     0.089  ratio 0.0% of anchor
    full_scan          p50    24.513 ms  min    23.670  max    25.036  ratio 9.9% of anchor
    blob_read          p50     0.005 ms  min     0.005  max     0.005  ratio 0.0% of anchor
    graph_replay       p50    15.898 ms  min    15.644  max    19.221  ratio 6.4% of anchor
    metadata_json      p50     0.004 ms  min     0.004  max     0.004  ratio 0.0% of anchor
    messages_json      p50     0.851 ms  min     0.816  max     1.522  ratio 0.3% of anchor
    messages_decode    p50     0.598 ms  min     0.579  max     0.651  ratio 0.2% of anchor
    source_hash        p50     2.414 ms  min     2.376  max     2.444  ratio 1.0% of anchor
    history_rescan     p50    43.156 ms  min    41.740  max    45.331  ratio 17.5% of anchor
    page_index         p50    50.583 ms  min    45.265  max    73.273  ratio 20.5% of anchor
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 22946..22968 (6 distinct values)
  SPREAD   b attach_history store_read_bytes 4189203..4189207 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099141..1099145 (3 distinct values)
  SPREAD   b session_save store_read_bytes 22944..22968 (8 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    4.279/4.691    p95    4.679/5.220    delta  +0.412 ms  control-no-change
  skill_request_overlay p50    5.412/5.593    p95    5.843/6.510    delta  +0.180 ms  control-no-change
  mcp_config_load   p50    0.290/0.312    p95    0.320/0.368    delta  +0.021 ms  control-no-change
  attach_history    p50  266.706/269.273  p95  279.112/295.513  delta  +2.567 ms  control-no-change
  resume_ready      p50   64.202/66.901   p95   70.253/68.244   delta  +2.700 ms  control-no-change
  provider_dispatch p50    1.225/1.279    p95    1.393/1.776    delta  +0.053 ms  control-no-change
  session_save      p50   14.954/23.156   p95   37.633/42.330   delta  +8.202 ms  control-drift
  total store_read_bytes p50 5311306  full_source_hashes p50 2  skill_dir_reads p50 1
  decomposition 3/3 cold pass(es); anchor attach_history p50 266.706 ms (load 230.007 + history 38.965)
    selection          p50     0.040 ms  min     0.039  max     0.049  ratio 0.0% of anchor
    checkpoint_recent  p50   168.358 ms  min   168.184  max   192.250  ratio 63.1% of anchor
    log_read           p50     0.074 ms  min     0.061  max     0.077  ratio 0.0% of anchor
    full_scan          p50    22.125 ms  min    22.026  max    22.509  ratio 8.3% of anchor
    blob_read          p50     0.004 ms  min     0.003  max     0.005  ratio 0.0% of anchor
    graph_replay       p50    14.796 ms  min    14.323  max    15.716  ratio 5.5% of anchor
    metadata_json      p50     0.004 ms  min     0.004  max     0.007  ratio 0.0% of anchor
    messages_json      p50     0.792 ms  min     0.783  max     0.819  ratio 0.3% of anchor
    messages_decode    p50     0.589 ms  min     0.558  max     0.674  ratio 0.2% of anchor
    source_hash        p50     2.264 ms  min     2.221  max     2.733  ratio 0.8% of anchor
    history_rescan     p50    40.946 ms  min    36.622  max    41.281  ratio 15.4% of anchor
    page_index         p50    44.516 ms  min    44.293  max    48.374  ratio 16.7% of anchor
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189203..4189207 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099143..1099145 (3 distinct values)
  SPREAD   a session_save store_read_bytes 22936..22960 (8 distinct values)
  SPREAD   b attach_history store_read_bytes 4189203..4189207 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099141..1099145 (3 distinct values)
  SPREAD   b session_save store_read_bytes 22946..22964 (7 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.025/0.026    p95    0.078/0.035    delta  +0.001 ms  control-no-change
  skill_request_overlay p50    0.236/0.226    p95    0.297/0.542    delta  -0.010 ms  control-no-change
  mcp_config_load   p50    0.264/0.274    p95    0.302/0.463    delta  +0.010 ms  control-no-change
  attach_history    p50 2780.621/2714.804 p95 3136.797/2969.156 delta -65.817 ms  control-no-change
  resume_ready      p50  735.569/681.876  p95  800.318/965.665  delta -53.693 ms  control-no-change
  provider_dispatch p50   12.464/12.461   p95   15.653/14.797   delta  -0.003 ms  control-no-change
  session_save      p50   16.702/17.051   p95   50.364/73.930   delta  +0.348 ms  control-no-change
  total store_read_bytes p50 53417519  full_source_hashes p50 2  skill_dir_reads p50 1
  decomposition 3/3 cold pass(es); anchor attach_history p50 2780.621 ms (load 2353.483 + history 417.989)
    selection          p50     0.068 ms  min     0.057  max     0.604  ratio 0.0% of anchor
    checkpoint_recent  p50  1837.568 ms  min  1762.211  max  2057.928  ratio 66.1% of anchor
    log_read           p50     1.224 ms  min     1.120  max     1.402  ratio 0.0% of anchor
    full_scan          p50   220.996 ms  min   201.972  max   231.274  ratio 7.9% of anchor
    blob_read          p50     0.070 ms  min     0.068  max     0.071  ratio 0.0% of anchor
    graph_replay       p50   172.848 ms  min   141.870  max   210.479  ratio 6.2% of anchor
    metadata_json      p50     0.005 ms  min     0.005  max     0.006  ratio 0.0% of anchor
    messages_json      p50     7.682 ms  min     6.531  max    10.948  ratio 0.3% of anchor
    messages_decode    p50     8.393 ms  min     8.301  max    15.072  ratio 0.3% of anchor
    source_hash        p50    19.451 ms  min    18.392  max    21.469  ratio 0.7% of anchor
    history_rescan     p50   370.865 ms  min   360.742  max   413.858  ratio 13.3% of anchor
    page_index         p50   368.554 ms  min   358.233  max   370.132  ratio 13.3% of anchor
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184301..42184305 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210251..11210255 (3 distinct values)
  SPREAD   a session_save store_read_bytes 22935..22971 (8 distinct values)
  SPREAD   b attach_history store_read_bytes 42184301..42184305 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210251..11210255 (3 distinct values)
  SPREAD   b session_save store_read_bytes 22951..22971 (7 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    4.168/4.114    p95    4.899/4.595    delta  -0.054 ms  control-no-change
  skill_request_overlay p50    5.333/5.278    p95    5.781/5.872    delta  -0.055 ms  control-no-change
  mcp_config_load   p50    0.277/0.280    p95    0.299/0.341    delta  +0.003 ms  control-no-change
  attach_history    p50 2771.764/2755.535 p95 3069.861/2900.750 delta -16.229 ms  control-no-change
  resume_ready      p50  685.341/665.497  p95  724.815/735.011  delta -19.845 ms  control-no-change
  provider_dispatch p50   12.181/12.476   p95   13.571/13.628   delta  +0.295 ms  control-no-change
  session_save      p50   19.266/19.457   p95   29.966/21.759   delta  +0.190 ms  control-no-change
  total store_read_bytes p50 53417515  full_source_hashes p50 2  skill_dir_reads p50 1
  decomposition 3/3 cold pass(es); anchor attach_history p50 2771.764 ms (load 2353.429 + history 398.962)
    selection          p50     0.063 ms  min     0.058  max     0.318  ratio 0.0% of anchor
    checkpoint_recent  p50  1920.201 ms  min  1866.570  max  1925.210  ratio 69.3% of anchor
    log_read           p50     1.328 ms  min     1.187  max     1.367  ratio 0.0% of anchor
    full_scan          p50   249.766 ms  min   228.557  max   255.186  ratio 9.0% of anchor
    blob_read          p50     0.074 ms  min     0.070  max     0.078  ratio 0.0% of anchor
    graph_replay       p50   192.387 ms  min   180.757  max   192.767  ratio 6.9% of anchor
    metadata_json      p50     0.008 ms  min     0.006  max     0.009  ratio 0.0% of anchor
    messages_json      p50     8.093 ms  min     7.717  max     9.253  ratio 0.3% of anchor
    messages_decode    p50    10.779 ms  min    10.571  max    11.744  ratio 0.4% of anchor
    source_hash        p50    23.219 ms  min    20.767  max    24.004  ratio 0.8% of anchor
    history_rescan     p50   426.977 ms  min   415.804  max   447.507  ratio 15.4% of anchor
    page_index         p50   417.171 ms  min   408.766  max   460.844  ratio 15.1% of anchor
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184301..42184305 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210253..11210255 (3 distinct values)
  SPREAD   a session_save store_read_bytes 22943..22963 (7 distinct values)
  SPREAD   b attach_history store_read_bytes 42184301..42184305 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210251..11210255 (2 distinct values)
  SPREAD   b session_save store_read_bytes 22947..22971 (6 distinct values)
report   bench-results/loading-cold.json
mode cold, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### 세션 로딩 (WARM)

```text
binary   /Users/runner/work/agent/agent/bench-bin/examples/loading_probe (f9cbe8ee6468)
stages   /Users/runner/work/agent/agent/bench-bin/examples/resume_stage_probe (02a293eb6b3a, 0 cold pass(es) per session fixture)
mode     warm
protocol WARM: one HOME per side, --warmup discarded runs per side, then measured runs alternate between the two sides on those same HOMEs.
fixtures /var/folders/h5/t85nd63d71x27_8gl5gw3l_w0000gn/T/horon-loading-bench-74z7cbmf
fixture events=0 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    0.028/0.063    p95    0.044/0.112    delta  +0.035 ms  control-drift
  skill_request_overlay p50    0.262/0.412    p95    0.348/0.719    delta  +0.150 ms  control-drift
  mcp_config_load   p50    0.308/0.495    p95    1.209/0.765    delta  +0.187 ms  control-drift
  attach_history    p50    0.000/0.001    p95    0.002/0.008    delta  +0.000 ms  control-drift
  resume_ready      p50    0.000/0.000    p95    0.000/0.001    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.001    delta  +0.000 ms  control-no-change
  session_save      p50   30.934/30.857   p95   47.037/65.916   delta  -0.078 ms  control-no-change
  total store_read_bytes p50 23660  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition not_applicable: this fixture has no session log
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [22941, 22943, 22951, 23659, 23661, 23665, 24371, 24373]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [22939, 22943, 22945, 22947, 23655, 23661, 23677, 24355, 24385, 24391]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 22941..24373 (8 distinct values)
  SPREAD   b session_save store_read_bytes 22939..24391 (10 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    8.869/5.791    p95   11.857/13.118   delta  -3.077 ms  control-no-change
  skill_request_overlay p50   10.229/7.749    p95   12.665/15.919   delta  -2.480 ms  control-no-change
  mcp_config_load   p50    0.299/0.283    p95    0.773/1.196    delta  -0.016 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.001/0.000    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.001/0.000    delta  -0.000 ms  control-no-change
  session_save      p50   24.896/25.647   p95   40.242/45.636   delta  +0.751 ms  control-no-change
  total store_read_bytes p50 23652  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition not_applicable: this fixture has no session log
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [22935, 22949, 22951, 22959, 23647, 23657, 23679, 24373, 24383, 24387]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [22939, 22945, 22951, 23651, 23657, 23665, 24361, 24379, 24383]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 22935..24387 (10 distinct values)
  SPREAD   b session_save store_read_bytes 22939..24383 (9 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.035/0.047    p95    0.069/0.090    delta  +0.012 ms  control-no-change
  skill_request_overlay p50    0.232/0.309    p95    1.001/0.615    delta  +0.077 ms  control-drift
  mcp_config_load   p50    0.269/0.355    p95    0.540/0.874    delta  +0.086 ms  control-no-change
  attach_history    p50  147.051/143.851  p95  176.024/222.151  delta  -3.199 ms  control-no-change
  resume_ready      p50   86.480/93.645   p95  102.742/103.216  delta  +7.164 ms  control-no-change
  provider_dispatch p50    1.193/1.190    p95    2.794/2.881    delta  -0.003 ms  control-no-change
  session_save      p50   24.201/33.569   p95   34.532/62.012   delta  +9.368 ms  control-no-change
  total store_read_bytes p50 2841244  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition skipped: --stage-probe-samples is 0
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23666, 24380, 24392, 25090, 25098, 25804, 25816, 26518, 26530]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23672, 23676, 24382, 24384, 25098, 25808, 25816, 26512, 26516]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23666..26530 (9 distinct values)
  SPREAD   b session_save store_read_bytes 23672..26516 (9 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    4.364/5.769    p95    8.288/9.119    delta  +1.405 ms  control-no-change
  skill_request_overlay p50    7.811/6.598    p95   12.279/13.590   delta  -1.213 ms  control-no-change
  mcp_config_load   p50    0.397/0.289    p95    0.971/0.720    delta  -0.108 ms  control-no-change
  attach_history    p50  131.333/119.635  p95  159.318/131.489  delta -11.698 ms  control-no-change
  resume_ready      p50   74.929/75.769   p95   88.348/92.572   delta  +0.840 ms  control-no-change
  provider_dispatch p50    1.120/1.120    p95    1.690/1.800    delta  +0.000 ms  control-no-change
  session_save      p50   24.230/24.613   p95   28.384/55.211   delta  +0.383 ms  control-no-change
  total store_read_bytes p50 2841261  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition skipped: --stage-probe-samples is 0
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23668, 23678, 24374, 24388, 25094, 25112, 25820, 25822, 26520, 26532]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23656, 23670, 24376, 24378, 25083, 25104, 25808, 25811, 26501, 26522]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23668..26532 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23656..26522 (10 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.031/0.026    p95    0.071/0.075    delta  -0.005 ms  control-no-change
  skill_request_overlay p50    0.256/0.242    p95    0.581/0.658    delta  -0.014 ms  control-no-change
  mcp_config_load   p50    0.273/0.279    p95    0.515/0.670    delta  +0.006 ms  control-no-change
  attach_history    p50 1577.880/1519.299 p95 1706.761/1734.149 delta -58.581 ms  control-no-change
  resume_ready      p50  917.271/961.900  p95 1068.966/1106.812 delta +44.630 ms  control-no-change
  provider_dispatch p50   15.652/15.432   p95   38.967/21.310   delta  -0.220 ms  control-no-change
  session_save      p50   25.285/26.223   p95   32.795/86.996   delta  +0.938 ms  control-no-change
  total store_read_bytes p50 28640266  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition skipped: --stage-probe-samples is 0
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23677, 23679, 24379, 24385, 25079, 25103, 25805, 25817, 26529, 26531]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23663, 23665, 24377, 24395, 25087, 25101, 25813, 25819, 26527, 26535]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23677..26531 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23663..26535 (10 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    4.266/6.256    p95    6.104/10.255   delta  +1.990 ms  control-drift
  skill_request_overlay p50    6.687/8.027    p95   12.288/55.329   delta  +1.340 ms  control-no-change
  mcp_config_load   p50    0.289/0.299    p95    0.640/0.574    delta  +0.010 ms  control-no-change
  attach_history    p50 1503.410/1417.444 p95 1793.951/1696.413 delta -85.967 ms  control-no-change
  resume_ready      p50  909.297/929.255  p95 1482.742/1178.097 delta +19.958 ms  control-no-change
  provider_dispatch p50   13.389/14.887   p95   17.775/22.266   delta  +1.498 ms  control-no-change
  session_save      p50   30.508/31.748   p95  149.449/141.035  delta  +1.240 ms  control-no-change
  total store_read_bytes p50 28640276  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition skipped: --stage-probe-samples is 0
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23661, 23675, 24383, 24389, 25097, 25105, 25811, 25823, 26527, 26529]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23654, 23664, 24382, 24384, 25088, 25104, 25798, 25806, 26520, 26524]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23661..26529 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23654..26524 (10 distinct values)
report   bench-results/loading-warm.json
mode warm, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### Tool latency

| Tool case | median | p95 | min | max | samples |
|---|---|---|---|---|---|
| read (tiny file) | 0.5 ms | 0.6 ms | 0.2 ms | 0.6 ms | 5 |
| read (1000 lines) | 1.1 ms | 3.3 ms | 0.7 ms | 3.3 ms | 5 |
| read (10 lines @ offset) | 0.7 ms | 1.2 ms | 0.5 ms | 1.2 ms | 5 |
| read (repo Cargo.toml) | 0.4 ms | 0.5 ms | 0.3 ms | 0.5 ms | 5 |
| write (small) | 2.4 ms | 3.6 ms | 2.2 ms | 3.6 ms | 5 |
| write (10KB) | 2.8 ms | 3.4 ms | 2.6 ms | 3.4 ms | 5 |
| edit (simple replace) | 2.6 ms | 3.0 ms | 2.2 ms | 3.0 ms | 5 |
| ls (scripts dir) | 1.1 ms | 1.7 ms | 1.0 ms | 1.7 ms | 5 |
| agentgrep (grep in tree) | 5.5 ms | 9.2 ms | 4.6 ms | 9.2 ms | 5 |
| agentgrep (find **/*.rs) | 2.3 ms | 3.4 ms | 1.9 ms | 3.4 ms | 5 |
| bash (true) | 8.5 ms | 11.9 ms | 7.3 ms | 11.9 ms | 5 |
| bash (echo hello) | 11.2 ms | 11.8 ms | 7.7 ms | 11.8 ms | 5 |
| bash (ls fixture tree) | 12.9 ms | 14.9 ms | 8.6 ms | 14.9 ms | 5 |
| invalid tool (dispatch only) | 0.1 ms | 0.1 ms | 0.1 ms | 0.1 ms | 5 |

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
| 1 | 84.8 ms | 84.8 ms | 84.8 ms | 85.0 ms | 0 |
| 10 | 181.9 ms | 189.8 ms | 189.8 ms | 190.0 ms | 0 |
| 40 | 750.1 ms | 801.4 ms | 802.1 ms | 803.1 ms | 0 |

#### Compile loop

| Checkpoint | Touched | median | min | max | samples |
|---|---|---|---|---|---|
| warm_noop | — | 488.5 ms | 488.5 ms | 488.5 ms | 1 |
| leaf_touch | src/main.rs | 442.6 ms | 442.6 ms | 442.6 ms | 1 |
| core_touch | crates/app-core/src/lib.rs | 18.43 s | 18.43 s | 18.43 s | 1 |
| build_meta_touch | crates/build-meta/src/lib.rs | 24.24 s | 24.24 s | 24.24 s | 1 |

### Windows

#### Coding agent comparison (headless, mock model)

Host: windows/AMD64, 4 CPUs, Python 3.12.10. Baseline: **horon**. Samples per one-shot, skill and MCP cell: 5.

Skipped: syscalls (strace not available)

| Tool | Version | `--version` (median) | Install size |
|---|---|---|---|
| horon | horon-agent v0.1.0-dev (e249038) | 12.7 ms | 92.8 MB |
| jcode | jcode v0.99.1 (7206efd0c) | 18.6 ms | 128.4 MB |
| Claude Code | 2.1.317 (Claude Code) | 47.7 ms | 645.2 MB |
| Codex CLI | codex-cli 0.166.0 | 103.0 ms | 479.4 MB |
| OpenCode | 1.19.5 | 578.4 ms | 605.3 MB |
| pi | 1.5.0 | 321.0 ms | 109.0 MB |
| oh-my-pi (omp) | omp/18.18.0 | 776.4 ms | 292.4 MB |
| oh-my-openagent (omo) | omo 5.1.64 (engine: senpi 2026.11.5-1) | 215.4 ms | 514.9 MB |

**One-shot headless prompt** (includes startup, one `ls` tool call, final answer)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Tool round-trip | request bytes | Note |
|---|---|---|---|---|---|---|---|---|
| horon | 175.9 ms | 165.0 ms–258.3 ms | baseline | 32.8 MB | baseline | yes | 3,664 / 2,368 (1 tools) | — |
| jcode | 157.8 ms | 149.1 ms–218.7 ms | 1.11× less | 32.3 MB | same | yes | 3,105 / 1,312 (1 tools) | — |
| Claude Code | 1.53 s | 1.40 s–3.26 s | 8.71× more | 272.1 MB | 8.30× more | yes | 50,067 / 33,795 (14 tools) | — |
| Codex CLI | 1.77 s | 1.54 s–2.09 s | 10.07× more | 266.7 MB | 8.13× more | yes | 41,028 / 18,342 (9 tools) | — |
| OpenCode | 5.36 s | 5.30 s–9.09 s | 30.45× more | 787.1 MB | 24.00× more | yes | 2,659 / 0 (0 tools) | — |
| pi | 576.0 ms | 561.3 ms–629.5 ms | 3.28× more | 120.5 MB | 3.67× more | yes | 5,522 / 2,841 (4 tools) | — |
| oh-my-pi (omp) | 3.22 s | 3.10 s–3.41 s | 18.33× more | 413.8 MB | 12.62× more | yes | 29,889 / 18,533 (11 tools) | — |
| oh-my-openagent (omo) | 21.72 s | 18.37 s–22.48 s | 123.52× more | 551.1 MB | 16.80× more | yes | 29,467 / 2,424 (2 tools) | — |

`request bytes` is the first model request of the first run as the mock server received it: JSON body bytes / tool catalog bytes (tools offered).

**Core-profile one-shot** (the one-shot turn without `--tools`, so the request carries the default core surface: the eager tools, `toolbox` and the listed-tool index)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | Note | request bytes |
|---|---|---|---|---|---|---|---|---|---|
| horon | 187.3 ms | 175.4 ms–216.8 ms | baseline | 33.8 MB | baseline | — | yes | — | 11,033 / 9,079 (8 tools) |
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
| horon | 172.1 ms | 164.7 ms–190.1 ms | baseline | 24.6 MB | baseline | skill-uri | yes | — |
| jcode | 190.2 ms | 182.3 ms–202.3 ms | 1.11× more | 24.1 MB | 1.02× less | skill-tool | yes | — |
| Claude Code | 977.0 ms | 967.9 ms–994.3 ms | 5.68× more | 236.9 MB | 9.64× more | skill-tool | yes | — |
| Codex CLI | 2.18 s | 1.96 s–8.45 s | 12.69× more | 263.9 MB | 10.74× more | file-read | yes | — |
| OpenCode | 8.23 s | 8.11 s–8.37 s | 47.81× more | 3.1 GB | 127.33× more | skill-tool | yes | — |
| pi | 721.9 ms | 715.3 ms–749.2 ms | 4.20× more | 98.0 MB | 3.99× more | file-read | yes | — |
| oh-my-pi (omp) | 7.75 s | 4.36 s–11.59 s | 45.07× more | 408.7 MB | 16.64× more | file-read | yes | — |
| oh-my-openagent (omo) | 22.46 s | 20.77 s–23.07 s | 130.52× more | 534.7 MB | 21.77× more | file-read | yes | — |

**MCP tool call** (process start to final answer, including starting the fixture stdio MCP server and one `bench_echo` call)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | MCP ready | Note |
|---|---|---|---|---|---|---|---|---|---|
| horon | 211.3 ms | 199.1 ms–246.9 ms | baseline | 37.2 MB | baseline | mcp-call | yes | 134.6 ms | — |
| jcode | 322.9 ms | 305.9 ms–1.01 s | 1.53× more | 36.7 MB | same | mcp-action | yes | 238.6 ms | — |
| Claude Code | 1.02 s | 1.02 s–1.03 s | 4.85× more | 280.8 MB | 7.54× more | mcp-tool | yes | 890.1 ms | — |
| Codex CLI | 2.18 s | 2.06 s–2.98 s | 10.30× more | 180.2 MB | 4.84× more | mcp-tool | yes | 677.2 ms | — |
| OpenCode | 8.21 s | 8.01 s–8.57 s | 38.85× more | 2.8 GB | 77.05× more | mcp-tool | yes | 6.47 s | — |
| pi | 813.3 ms | 807.4 ms–838.0 ms | 3.85× more | 117.7 MB | 3.16× more | mcp-tool | yes | 681.9 ms | — |
| oh-my-pi (omp) | 3.91 s | 3.85 s–4.16 s | 18.52× more | 442.0 MB | 11.87× more | mcp-route | yes | 2.89 s | — |
| oh-my-openagent (omo) | — | — | — | — | — | mcp-tool | no | — | failed: the fixture sentinel never came back |

**Session memory** (idle after 3 turn(s) per session; sizes are process-tree sums, RSS; PSS also recorded on Linux). "Each additional" is the least-squares slope across the measured session counts; 0 B means growth stayed below RSS sampling noise. Threads and descriptors come from the daemon's own `server:memory` after the turns. Processes are counted in the same tree walk as the RSS sample (PSS shares it on Linux), so the count and the bytes describe one observation; a PID shared by two session roots counts once, a root that already exited is counted as neither bytes nor process, and a cell that could not be read is —, never 0.

| Tool | First session | Each additional | vs horon (additional) | Threads/session | FDs/session | Processes (idle) | Processes/session | First (post-turn) | Each additional (post-turn) |
|---|---|---|---|---|---|---|---|---|---|
| horon | 22.2 MB | 93.8 KB | baseline | — | — | 1 | 0.00 | 29.6 MB | 348.3 KB |
| jcode | 22.0 MB | 76.1 KB | 1.23× less | — | — | 1 | 0.00 | 26.8 MB | 317.2 KB |
| Claude Code | 154.5 MB | 152.4 MB | 1,665× more | — | — | 2 | 3.61 | 187.3 MB | 190.8 MB |
| Codex CLI | 125.8 MB | 97.2 MB | 1,061× more | — | — | 6 | 3.00 | 165.9 MB | 116.2 MB |
| OpenCode | 245.1 MB | 956.6 KB | 10.20× more | — | — | 2 | 0.00 | 435.7 MB | 4.4 MB |
| pi | 91.2 MB | 90.6 MB | 989.31× more | — | — | 2 | 2.00 | 101.2 MB | 100.9 MB |
| oh-my-pi (omp) | 302.1 MB | 88.2 MB | 963.55× more | — | — | 2 | 2.00 | 349.5 MB | 338.8 MB |
| oh-my-openagent (omo) | 145.9 MB | 108.7 MB | 1,187× more | — | — | 4 | 3.89 | 412.6 MB | 404.0 MB |

Threads/descriptors unavailable: horon (server:memory did not report thread_count, fd_count), jcode (server:memory did not report thread_count, fd_count)

**Post-turn tree RSS per session count**

| Tool | 1 session(s) | 5 session(s) | 10 session(s) |
|---|---|---|---|
| horon | 29.6 MB | 32.2 MB | 32.7 MB |
| jcode | 26.8 MB | 28.6 MB | 29.6 MB |
| Claude Code | 187.3 MB | 958.3 MB | 1.9 GB |
| Codex CLI | 165.9 MB | 656.8 MB | 1.2 GB |
| OpenCode | 435.7 MB | 641.3 MB | 488.5 MB |
| pi | 101.2 MB | 505.0 MB | 1009.3 MB |
| oh-my-pi (omp) | 349.5 MB | 1.7 GB | 3.3 GB |
| oh-my-openagent (omo) | 412.6 MB | 2.2 GB | 4.0 GB |

**Live processes per session count** (idle / post-turn)

| Tool | 1 session(s) | 5 session(s) | 10 session(s) |
|---|---|---|---|
| horon | 1 / 1 | 1 / 1 | 1 / 1 |
| jcode | 1 / 1 | 1 / 1 | 1 / 1 |
| Claude Code | 2 / 2 | 10 / 10 | 34 / 20 |
| Codex CLI | 6 / 6 | 18 / 18 | 33 / 30 |
| OpenCode | 2 / 2 | 2 / 2 | 2 / 2 |
| pi | 2 / 2 | 10 / 10 | 20 / 20 |
| oh-my-pi (omp) | 2 / 2 | 10 / 10 | 20 / 20 |
| oh-my-openagent (omo) | 4 / 6 | 20 / 58 | 39 / 76 |

#### 기동

```text
Benchmarking: D:/a/agent/agent/bench-bin/horon-agent.exe
============================================================

--help (binary load):
  Min:    13.86 ms
  Max:    15.66 ms
  Mean:   14.52 ms
  Median: 14.55 ms
  Stdev:  0.52 ms

--version:
  Min:    13.52 ms
  Max:    14.76 ms
  Mean:   14.29 ms
  Median: 14.42 ms
  Stdev:  0.38 ms

Measuring isolated server startup (10 runs)...

Server ready (isolated socket connectable):
  Min:    22.44 ms
  Max:    28.00 ms
  Mean:   23.40 ms
  Median: 22.76 ms
  Stdev:  1.68 ms

Server startup stages (one extra run with HORON_AGENT_STARTUP_PROFILE=1, not timed):
  stage             at ms  delta ms   share
  panic_hook          0.0       0.0    0.1%
  logging_init        1.5       1.5   17.8%
  nofile_limit        2.1       0.6    7.4%
  perm_harden         5.3       3.2   37.7%
  args_parse          5.3       0.0    0.0%
  config_loaded       6.7       1.4   16.3%
  server_new          7.9       1.2   14.2%
  socket_bound        8.5       0.6    6.5%

Measuring harness bridge readiness (10 runs)...
  skipped: api-bridge is Unix-only, so Windows has no harness bridge to time

Bridge stage: api socket connectable: No successful runs

Bridge stage: hello_ok: No successful runs

Bridge stage: no-op reply: No successful runs

Bridge total (socket -> no-op): No successful runs

============================================================
Summary:
  Binary load median:      ~14.5 ms
  Server ready median:     ~22.8 ms
  Bridge total median:     n/a
```

#### 세션 로딩 (COLD)

```text
binary   D:\a\agent\agent\bench-bin\examples\loading_probe.exe (df92a5eef817)
stages   D:\a\agent\agent\bench-bin\examples\resume_stage_probe.exe (fbd5ecd482d1, 3 cold pass(es) per session fixture)
mode     cold
protocol COLD: every measured run gets its own freshly copied HOME; no derived cache from an earlier run exists, so each sample is a first load of the fixture.
fixtures C:\Users\RUNNER~1\AppData\Local\Temp\horon-loading-bench-ny5po7yz
note     --warmup 3 is ignored in cold mode: every sample gets a fresh HOME
fixture events=0 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    0.161/0.160    p95    0.165/0.180    delta  -0.001 ms  control-no-change
  skill_request_overlay p50    1.256/1.283    p95    1.278/1.402    delta  +0.027 ms  control-no-change
  mcp_config_load   p50    1.456/1.449    p95    1.496/1.486    delta  -0.007 ms  control-no-change
  attach_history    p50    0.002/0.002    p95    0.003/0.002    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  session_save      p50   57.882/55.428   p95   67.034/60.495   delta  -2.455 ms  control-no-change
  total store_read_bytes p50 22948  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition not_applicable: this fixture has no session log
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 22924..22968 (8 distinct values)
  SPREAD   b session_save store_read_bytes 22914..22968 (7 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50   23.249/24.031   p95   23.687/24.996   delta  +0.782 ms  control-no-change
  skill_request_overlay p50   55.424/53.804   p95   70.361/60.718   delta  -1.619 ms  control-no-change
  mcp_config_load   p50    1.565/1.529    p95    1.909/1.622    delta  -0.037 ms  control-no-change
  attach_history    p50    0.002/0.002    p95    0.005/0.002    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  session_save      p50   64.216/60.688   p95   69.159/83.098   delta  -3.528 ms  control-no-change
  total store_read_bytes p50 22946  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition not_applicable: this fixture has no session log
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 22910..22968 (9 distinct values)
  SPREAD   b session_save store_read_bytes 22912..22960 (7 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.156/0.158    p95    0.210/0.186    delta  +0.002 ms  control-no-change
  skill_request_overlay p50    1.251/1.210    p95    1.342/1.265    delta  -0.041 ms  control-no-change
  mcp_config_load   p50    1.471/1.441    p95    1.586/1.470    delta  -0.030 ms  control-no-change
  attach_history    p50  507.193/516.417  p95  533.985/534.697  delta  +9.224 ms  control-no-change
  resume_ready      p50  231.963/235.272  p95  245.082/245.729  delta  +3.309 ms  control-no-change
  provider_dispatch p50    2.013/1.974    p95    2.122/2.241    delta  -0.039 ms  control-no-change
  session_save      p50   50.610/51.195   p95   52.929/56.650   delta  +0.585 ms  control-no-change
  total store_read_bytes p50 5311224  full_source_hashes p50 2  skill_dir_reads p50 1
  decomposition 3/3 cold pass(es); anchor attach_history p50 507.193 ms (load 447.633 + history 59.769)
    selection          p50     0.190 ms  min     0.188  max     0.221  ratio 0.0% of anchor
    checkpoint_recent  p50   384.659 ms  min   380.213  max   385.101  ratio 75.8% of anchor
    log_read           p50     0.218 ms  min     0.199  max     0.248  ratio 0.0% of anchor
    full_scan          p50    32.651 ms  min    32.574  max    32.972  ratio 6.4% of anchor
    blob_read          p50     0.008 ms  min     0.006  max     0.008  ratio 0.0% of anchor
    graph_replay       p50    25.567 ms  min    23.888  max    26.409  ratio 5.0% of anchor
    metadata_json      p50     0.013 ms  min     0.013  max     0.023  ratio 0.0% of anchor
    messages_json      p50     1.668 ms  min     1.637  max     1.937  ratio 0.3% of anchor
    messages_decode    p50     1.331 ms  min     1.135  max     1.333  ratio 0.3% of anchor
    source_hash        p50     0.593 ms  min     0.578  max     0.734  ratio 0.1% of anchor
    history_rescan     p50    60.337 ms  min    59.381  max    62.073  ratio 11.9% of anchor
    page_index         p50    88.420 ms  min    87.717  max    95.566  ratio 17.4% of anchor
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189151..4189155 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099102..1099106 (3 distinct values)
  SPREAD   a session_save store_read_bytes 22932..22980 (9 distinct values)
  SPREAD   b attach_history store_read_bytes 4189151..4189155 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099102..1099106 (3 distinct values)
  SPREAD   b session_save store_read_bytes 22911..22960 (7 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50   24.813/25.822   p95   28.081/36.639   delta  +1.008 ms  control-no-change
  skill_request_overlay p50   56.084/56.415   p95   63.938/86.315   delta  +0.332 ms  control-no-change
  mcp_config_load   p50    1.529/1.546    p95    2.310/2.029    delta  +0.017 ms  control-no-change
  attach_history    p50  510.280/514.817  p95  869.719/628.494  delta  +4.537 ms  control-no-change
  resume_ready      p50  234.043/235.687  p95  296.722/252.885  delta  +1.644 ms  control-no-change
  provider_dispatch p50    1.934/1.902    p95    3.085/2.009    delta  -0.032 ms  control-no-change
  session_save      p50   53.524/49.867   p95  514.077/103.074  delta  -3.657 ms  control-no-change
  total store_read_bytes p50 5311221  full_source_hashes p50 2  skill_dir_reads p50 1
  decomposition 3/3 cold pass(es); anchor attach_history p50 510.280 ms (load 451.232 + history 59.503)
    selection          p50     0.188 ms  min     0.176  max     0.214  ratio 0.0% of anchor
    checkpoint_recent  p50   411.634 ms  min   383.376  max   420.401  ratio 80.7% of anchor
    log_read           p50     0.191 ms  min     0.189  max     0.213  ratio 0.0% of anchor
    full_scan          p50    39.240 ms  min    32.566  max    44.540  ratio 7.7% of anchor
    blob_read          p50     0.007 ms  min     0.006  max     0.008  ratio 0.0% of anchor
    graph_replay       p50    28.442 ms  min    24.619  max    32.710  ratio 5.6% of anchor
    metadata_json      p50     0.012 ms  min     0.011  max     0.013  ratio 0.0% of anchor
    messages_json      p50     1.612 ms  min     1.605  max     2.017  ratio 0.3% of anchor
    messages_decode    p50     1.266 ms  min     1.077  max     1.679  ratio 0.2% of anchor
    source_hash        p50     0.573 ms  min     0.560  max     0.628  ratio 0.1% of anchor
    history_rescan     p50    60.469 ms  min    59.908  max    73.018  ratio 11.9% of anchor
    page_index         p50    91.023 ms  min    86.788  max   155.185  ratio 17.8% of anchor
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189150..4189155 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099101..1099106 (3 distinct values)
  SPREAD   a session_save store_read_bytes 22926..22972 (7 distinct values)
  SPREAD   b attach_history store_read_bytes 4189151..4189155 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099104..1099106 (3 distinct values)
  SPREAD   b session_save store_read_bytes 22912..22972 (10 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.158/0.159    p95    0.195/0.205    delta  +0.001 ms  control-no-change
  skill_request_overlay p50    1.247/1.227    p95    1.304/1.369    delta  -0.020 ms  control-no-change
  mcp_config_load   p50    1.446/1.468    p95    1.513/1.562    delta  +0.022 ms  control-no-change
  attach_history    p50 5571.474/5568.400 p95 5668.776/5613.086 delta  -3.074 ms  control-no-change
  resume_ready      p50 2413.501/2405.820 p95 2454.509/2446.347 delta  -7.680 ms  control-no-change
  provider_dispatch p50   21.190/20.983   p95   21.541/21.902   delta  -0.207 ms  control-no-change
  session_save      p50   48.247/47.260   p95   51.884/51.478   delta  -0.988 ms  control-no-change
  total store_read_bytes p50 53417420  full_source_hashes p50 2  skill_dir_reads p50 1
  decomposition 3/3 cold pass(es); anchor attach_history p50 5571.474 ms (load 4917.795 + history 660.003)
    selection          p50     0.238 ms  min     0.194  max     0.243  ratio 0.0% of anchor
    checkpoint_recent  p50  4270.946 ms  min  4246.248  max  4285.788  ratio 76.7% of anchor
    log_read           p50     1.933 ms  min     1.924  max     1.944  ratio 0.0% of anchor
    full_scan          p50   332.374 ms  min   324.346  max   335.425  ratio 6.0% of anchor
    blob_read          p50     0.050 ms  min     0.049  max     0.051  ratio 0.0% of anchor
    graph_replay       p50   299.008 ms  min   287.887  max   303.128  ratio 5.4% of anchor
    metadata_json      p50     0.015 ms  min     0.015  max     0.015  ratio 0.0% of anchor
    messages_json      p50    21.340 ms  min    21.323  max    21.517  ratio 0.4% of anchor
    messages_decode    p50    17.822 ms  min    17.740  max    17.991  ratio 0.3% of anchor
    source_hash        p50     5.134 ms  min     5.124  max     5.186  ratio 0.1% of anchor
    history_rescan     p50   655.293 ms  min   654.894  max   679.214  ratio 11.8% of anchor
    page_index         p50   710.076 ms  min   694.059  max   715.926  ratio 12.7% of anchor
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184249..42184253 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210214..11210216 (2 distinct values)
  SPREAD   a session_save store_read_bytes 22927..22975 (10 distinct values)
  SPREAD   b session_save store_read_bytes 22943..22979 (10 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50   23.598/24.171   p95   24.168/30.763   delta  +0.573 ms  control-no-change
  skill_request_overlay p50   54.508/55.135   p95   54.913/64.759   delta  +0.627 ms  control-no-change
  mcp_config_load   p50    1.511/1.546    p95    1.608/1.751    delta  +0.035 ms  control-no-change
  attach_history    p50 5569.933/5564.143 p95 5643.630/5683.045 delta  -5.790 ms  control-no-change
  resume_ready      p50 2382.174/2382.422 p95 2427.959/2404.737 delta  +0.248 ms  control-no-change
  provider_dispatch p50   20.881/20.779   p95   22.159/21.175   delta  -0.101 ms  control-no-change
  session_save      p50   44.812/46.174   p95   48.472/51.761   delta  +1.363 ms  control-no-change
  total store_read_bytes p50 53417432  full_source_hashes p50 2  skill_dir_reads p50 1
  decomposition 3/3 cold pass(es); anchor attach_history p50 5569.933 ms (load 4912.281 + history 651.068)
    selection          p50     0.201 ms  min     0.193  max     0.247  ratio 0.0% of anchor
    checkpoint_recent  p50  4231.726 ms  min  4221.550  max  4235.118  ratio 76.0% of anchor
    log_read           p50     1.940 ms  min     1.876  max     2.009  ratio 0.0% of anchor
    full_scan          p50   325.514 ms  min   325.054  max   332.298  ratio 5.8% of anchor
    blob_read          p50     0.052 ms  min     0.051  max     0.052  ratio 0.0% of anchor
    graph_replay       p50   297.162 ms  min   290.678  max   298.396  ratio 5.3% of anchor
    metadata_json      p50     0.014 ms  min     0.013  max     0.016  ratio 0.0% of anchor
    messages_json      p50    21.418 ms  min    21.216  max    21.419  ratio 0.4% of anchor
    messages_decode    p50    17.003 ms  min    16.535  max    17.284  ratio 0.3% of anchor
    source_hash        p50     5.154 ms  min     5.078  max     5.205  ratio 0.1% of anchor
    history_rescan     p50   651.316 ms  min   643.645  max   653.297  ratio 11.7% of anchor
    page_index         p50   705.759 ms  min   704.094  max   730.814  ratio 12.7% of anchor
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184249..42184253 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210212..11210216 (3 distinct values)
  SPREAD   a session_save store_read_bytes 22947..22974 (7 distinct values)
  SPREAD   b attach_history store_read_bytes 42184244..42184253 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210207..11210216 (3 distinct values)
  SPREAD   b session_save store_read_bytes 22939..22979 (9 distinct values)
report   bench-results\loading-cold.json
mode cold, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### 세션 로딩 (WARM)

```text
binary   D:\a\agent\agent\bench-bin\examples\loading_probe.exe (df92a5eef817)
stages   D:\a\agent\agent\bench-bin\examples\resume_stage_probe.exe (fbd5ecd482d1, 0 cold pass(es) per session fixture)
mode     warm
protocol WARM: one HOME per side, --warmup discarded runs per side, then measured runs alternate between the two sides on those same HOMEs.
fixtures C:\Users\RUNNER~1\AppData\Local\Temp\horon-loading-bench-utuwbxfd
fixture events=0 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    0.076/0.079    p95    0.120/0.120    delta  +0.003 ms  control-no-change
  skill_request_overlay p50    0.631/0.629    p95    0.725/0.954    delta  -0.001 ms  control-no-change
  mcp_config_load   p50    0.794/0.778    p95    0.962/0.940    delta  -0.015 ms  control-no-change
  attach_history    p50    0.002/0.002    p95    0.002/0.002    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-drift
  session_save      p50   34.637/34.536   p95   36.767/36.692   delta  -0.101 ms  control-no-change
  total store_read_bytes p50 23622  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition not_applicable: this fixture has no session log
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [22927, 22933, 22934, 22955, 23616, 23627, 23643, 24308, 24327, 24346]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [22909, 22917, 22949, 23634, 23648, 23652, 24347, 24351, 24357, 24359]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 22927..24346 (10 distinct values)
  SPREAD   b session_save store_read_bytes 22909..24359 (10 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50   12.361/12.169   p95   16.596/13.118   delta  -0.192 ms  control-no-change
  skill_request_overlay p50   31.453/31.225   p95   33.683/46.306   delta  -0.228 ms  control-no-change
  mcp_config_load   p50    0.813/0.841    p95    0.872/1.043    delta  +0.028 ms  control-no-change
  attach_history    p50    0.002/0.002    p95    0.004/0.005    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.001    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.001/0.001    delta  -0.000 ms  control-drift
  session_save      p50   40.202/39.766   p95   42.130/44.225   delta  -0.435 ms  control-no-change
  total store_read_bytes p50 23656  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition not_applicable: this fixture has no session log
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [22929, 22930, 22941, 23620, 23653, 23658, 24338, 24352, 24355, 24357]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [22925, 22941, 23618, 23644, 23652, 24333, 24335, 24345, 24359]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 22929..24357 (10 distinct values)
  SPREAD   b session_save store_read_bytes 22925..24359 (9 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.074/0.074    p95    0.092/0.084    delta  +0.000 ms  control-no-change
  skill_request_overlay p50    0.581/0.561    p95    0.837/0.663    delta  -0.020 ms  control-no-change
  mcp_config_load   p50    0.742/0.753    p95    0.856/0.863    delta  +0.011 ms  control-no-change
  attach_history    p50  171.497/168.361  p95  191.754/172.853  delta  -3.136 ms  control-no-change
  resume_ready      p50  120.605/121.524  p95  124.704/131.275  delta  +0.919 ms  control-no-change
  provider_dispatch p50    1.584/1.570    p95    1.755/1.677    delta  -0.014 ms  control-no-change
  session_save      p50   32.120/32.842   p95   35.630/35.770   delta  +0.722 ms  control-no-change
  total store_read_bytes p50 2841127  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition skipped: --stage-probe-samples is 0
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23641, 23657, 24346, 24360, 25036, 25066, 25739, 25757, 26430, 26448]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23658, 23659, 24346, 24353, 25033, 25035, 25726, 25756, 26406, 26433]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23641..26448 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23658..26433 (10 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50   12.100/12.295   p95   18.886/12.887   delta  +0.195 ms  control-no-change
  skill_request_overlay p50   31.045/30.905   p95   32.944/36.753   delta  -0.139 ms  control-no-change
  mcp_config_load   p50    0.807/0.842    p95    0.903/0.944    delta  +0.034 ms  control-no-change
  attach_history    p50  168.150/166.268  p95  187.227/176.463  delta  -1.881 ms  control-no-change
  resume_ready      p50  119.493/117.958  p95  126.381/134.495  delta  -1.535 ms  control-no-change
  provider_dispatch p50    1.556/1.551    p95    1.764/1.711    delta  -0.004 ms  control-no-change
  session_save      p50   39.222/39.191   p95   42.408/40.719   delta  -0.032 ms  control-no-change
  total store_read_bytes p50 2841129  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition skipped: --stage-probe-samples is 0
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23642, 23651, 24310, 24374, 25021, 25077, 25708, 25726, 26443, 26461]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23659, 23679, 24346, 24358, 25049, 25061, 25724, 25760, 26435, 26455]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23642..26461 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23659..26455 (10 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.074/0.079    p95    0.106/0.117    delta  +0.005 ms  control-no-change
  skill_request_overlay p50    0.609/0.621    p95    0.684/0.794    delta  +0.012 ms  control-no-change
  mcp_config_load   p50    0.759/0.773    p95    0.882/0.839    delta  +0.014 ms  control-no-change
  attach_history    p50 1824.726/1822.797 p95 1850.600/1851.459 delta  -1.930 ms  control-no-change
  resume_ready      p50 1284.232/1295.390 p95 1329.096/1328.144 delta +11.158 ms  control-no-change
  provider_dispatch p50   16.817/16.798   p95   17.622/19.657   delta  -0.019 ms  control-no-change
  session_save      p50   33.379/33.490   p95   36.504/69.927   delta  +0.111 ms  control-no-change
  total store_read_bytes p50 28640164  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition skipped: --stage-probe-samples is 0
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23637, 23650, 24328, 24332, 25062, 25073, 25733, 25760, 26386, 26428]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23652, 23668, 24327, 24359, 25045, 25070, 25752, 25773, 26427, 26431]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23637..26428 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23652..26431 (10 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50   12.145/12.334   p95   12.817/13.322   delta  +0.189 ms  control-no-change
  skill_request_overlay p50   32.810/31.692   p95   34.683/33.017   delta  -1.118 ms  control-no-change
  mcp_config_load   p50    0.863/0.866    p95    1.103/0.982    delta  +0.003 ms  control-no-change
  attach_history    p50 1796.592/1820.685 p95 1830.106/1861.950 delta +24.093 ms  control-no-change
  resume_ready      p50 1277.042/1273.733 p95 1311.805/1340.287 delta  -3.309 ms  control-no-change
  provider_dispatch p50   16.840/16.891   p95   18.854/18.131   delta  +0.051 ms  control-no-change
  session_save      p50   33.627/34.995   p95   35.942/47.621   delta  +1.367 ms  control-no-change
  total store_read_bytes p50 28640148  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition skipped: --stage-probe-samples is 0
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23650, 23668, 24345, 24357, 25031, 25072, 25738, 25759, 26447, 26450]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23614, 23639, 24363, 24366, 25028, 25050, 25741, 25757, 26423, 26452]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23650..26450 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23614..26452 (10 distinct values)
report   bench-results\loading-warm.json
mode warm, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### Tool latency

| Tool case | median | p95 | min | max | samples |
|---|---|---|---|---|---|
| read (tiny file) | 2.7 ms | 2.7 ms | 2.3 ms | 2.7 ms | 5 |
| read (1000 lines) | 2.7 ms | 2.7 ms | 2.7 ms | 2.7 ms | 5 |
| read (10 lines @ offset) | 2.7 ms | 2.7 ms | 2.2 ms | 2.7 ms | 5 |
| read (repo Cargo.toml) | 2.7 ms | 2.7 ms | 2.2 ms | 2.7 ms | 5 |
| write (small) | 7.8 ms | 10.4 ms | 7.4 ms | 10.4 ms | 5 |
| write (10KB) | 7.4 ms | 10.0 ms | 7.4 ms | 10.0 ms | 5 |
| edit (simple replace) | 7.9 ms | 9.9 ms | 7.4 ms | 9.9 ms | 5 |
| ls (scripts dir) | 2.7 ms | 2.7 ms | 2.2 ms | 2.7 ms | 5 |
| agentgrep (grep in tree) | 7.8 ms | 7.9 ms | 7.4 ms | 7.9 ms | 5 |
| agentgrep (find **/*.rs) | 5.3 ms | 5.3 ms | 4.8 ms | 5.3 ms | 5 |
| bash (true) | 10.3 ms | 10.6 ms | 9.7 ms | 10.6 ms | 5 |
| bash (echo hello) | 12.8 ms | 15.3 ms | 10.5 ms | 15.3 ms | 5 |
| bash (ls fixture tree) | 13.0 ms | 15.1 ms | 12.5 ms | 15.1 ms | 5 |
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
| 1 | 219.7 ms | 219.7 ms | 219.7 ms | 219.9 ms | 0 |
| 10 | 667.6 ms | 669.9 ms | 669.9 ms | 671.6 ms | 0 |
| 40 | 2.04 s | 2.51 s | 2.51 s | 2.52 s | 0 |

#### Compile loop

| Checkpoint | Touched | median | min | max | samples |
|---|---|---|---|---|---|
| warm_noop | — | 763.7 ms | 763.7 ms | 763.7 ms | 1 |
| leaf_touch | src\main.rs | 917.9 ms | 917.9 ms | 917.9 ms | 1 |
| core_touch | crates\app-core\src\lib.rs | 39.39 s | 39.39 s | 39.39 s | 1 |
| build_meta_touch | crates\build-meta\src\lib.rs | 47.71 s | 47.71 s | 47.71 s | 1 |

## TUI (`horon-tui`)

대화형 클라이언트는 별도 바이너리이고 따로 잽니다. 이 수치는 `tui` 묶음에서 나오며 위의 헤드리스 표와 비교하지 않습니다. `first_frame_ms` 와 `direct_first_frame_ms` 는 서로 다른 두 프로그램을 각각 띄운 값이고, 핸드오프는 그 둘의 차이입니다.

![TUI (`horon-tui`)](charts/tui-01-latency.ko.svg)

![TUI (`horon-tui`)](charts/tui-02-memory.ko.svg)

### Linux

#### TUI (`horon-tui`)

Measured version: the interactive client `horon-tui`. The headless `horon-agent` numbers are in the suites above and are never compared with these: the two binaries are timed against themselves.

| Metric | Median | Min–max | Samples |
| --- | --- | --- | --- |
| First frame (bare `horon-agent`) | 38.4 ms | 38.1 ms–169.8 ms | 5 |
| First frame (direct `horon-tui`) | 41.1 ms | 40.2 ms–41.6 ms | 5 |
| TTY handoff cost | -2.1 ms | -2.8 ms–128.2 ms | 5 |
| Key press to echo (p95) | 0.1 ms | 0.1 ms–0.3 ms | 50 |
| Idle RSS after 5 s | 5.9 MB | 5.8 MB–6.0 MB | 5 |
| RSS after one turn | 6.1 MB | 5.9 MB–6.2 MB | 5 |
| Frame draw time (p50) | 0.7 ms | 0.7 ms–0.7 ms | 5 |
| Frame draw time (p95) | 0.8 ms | 0.8 ms–0.8 ms | 5 |
| Install size (`horon-tui`) | 6.3 MB | — | 1 |

Handoff: median first-frame time of a bare `horon-agent` launch (no arguments, on a PTY) minus the median first-frame time of a direct `horon-tui` launch against an already-running daemon, both on the same machine and build. The bare path also starts the daemon, so the number covers that startup, the sibling lookup and the `exec`; it is the extra cost of the handoff over launching the client itself.

Render bench: 200 frames on a 120×40 terminal.

Scroll bench (a separate measurement, not the frame cells above): 120 one-row scroll steps and 20 no-input redraws over a 2,081-row transcript (72 items, 148.4 KB of tool output, expanded) on a 120×40 terminal. Every scroll step moved the expanded output.

| Scroll metric | Median | Min–max | Samples |
| --- | --- | --- | --- |
| Held frame draw (p50) | 0.3 ms | 0.3 ms–0.3 ms | 5 |
| Held frame draw (p95) | 0.4 ms | 0.3 ms–0.4 ms | 5 |
| Scroll key handling (p50) | 0.2 µs | 0.1 µs–0.2 µs | 5 |
| Scroll key handling (p95) | 0.2 µs | 0.2 µs–0.3 µs | 5 |
| First held frame after scrolling | 0.5 ms | 0.5 ms–0.5 ms | 5 |
| Key dispatch through completed draw (p50) | 0.3 ms | 0.3 ms–0.3 ms | 5 |
| Key dispatch through completed draw (p95) | 0.4 ms | 0.3 ms–0.4 ms | 5 |
| First key dispatch through completed draw | 0.5 ms | 0.5 ms–0.5 ms | 5 |
| Held frame redraw without input (p50) | 0.3 ms | 0.3 ms–0.3 ms | 5 |
| Held frame redraw without input (p95) | 0.3 ms | 0.3 ms–0.4 ms | 5 |

### macOS

#### TUI (`horon-tui`)

Measured version: the interactive client `horon-tui`. The headless `horon-agent` numbers are in the suites above and are never compared with these: the two binaries are timed against themselves.

| Metric | Median | Min–max | Samples |
| --- | --- | --- | --- |
| First frame (bare `horon-agent`) | 55.0 ms | 49.6 ms–85.8 ms | 5 |
| First frame (direct `horon-tui`) | 46.9 ms | 35.9 ms–55.8 ms | 5 |
| TTY handoff cost | 13.7 ms | -0.2 ms–30.0 ms | 5 |
| Key press to echo (p95) | 0.2 ms | 0.1 ms–0.4 ms | 50 |
| Idle RSS after 5 s | 9.5 MB | 9.4 MB–9.5 MB | 5 |
| RSS after one turn | 9.7 MB | 9.6 MB–9.7 MB | 5 |
| Frame draw time (p50) | 0.7 ms | 0.6 ms–0.7 ms | 5 |
| Frame draw time (p95) | 0.8 ms | 0.7 ms–0.9 ms | 5 |
| Install size (`horon-tui`) | 5.3 MB | — | 1 |

Handoff: median first-frame time of a bare `horon-agent` launch (no arguments, on a PTY) minus the median first-frame time of a direct `horon-tui` launch against an already-running daemon, both on the same machine and build. The bare path also starts the daemon, so the number covers that startup, the sibling lookup and the `exec`; it is the extra cost of the handoff over launching the client itself.

Render bench: 200 frames on a 120×40 terminal.

Scroll bench (a separate measurement, not the frame cells above): 120 one-row scroll steps and 20 no-input redraws over a 2,081-row transcript (72 items, 148.4 KB of tool output, expanded) on a 120×40 terminal. Every scroll step moved the expanded output.

| Scroll metric | Median | Min–max | Samples |
| --- | --- | --- | --- |
| Held frame draw (p50) | 0.4 ms | 0.3 ms–0.4 ms | 5 |
| Held frame draw (p95) | 0.4 ms | 0.3 ms–0.5 ms | 5 |
| Scroll key handling (p50) | 0.2 µs | 0.1 µs–0.2 µs | 5 |
| Scroll key handling (p95) | 0.3 µs | 0.2 µs–0.4 µs | 5 |
| First held frame after scrolling | 0.6 ms | 0.5 ms–0.7 ms | 5 |
| Key dispatch through completed draw (p50) | 0.4 ms | 0.3 ms–0.4 ms | 5 |
| Key dispatch through completed draw (p95) | 0.4 ms | 0.3 ms–0.5 ms | 5 |
| First key dispatch through completed draw | 0.6 ms | 0.5 ms–0.7 ms | 5 |
| Held frame redraw without input (p50) | 0.3 ms | 0.3 ms–0.4 ms | 5 |
| Held frame redraw without input (p95) | 0.4 ms | 0.3 ms–0.4 ms | 5 |

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
| Frame draw time (p50) | 1.9 ms | 1.8 ms–1.9 ms | 5 |
| Frame draw time (p95) | 1.9 ms | 1.9 ms–2.1 ms | 5 |
| Install size (`horon-tui`) | 5.4 MB | — | 1 |

Handoff: median first-frame time of a bare `horon-agent` launch (no arguments, on a PTY) minus the median first-frame time of a direct `horon-tui` launch against an already-running daemon, both on the same machine and build. The bare path also starts the daemon, so the number covers that startup, the sibling lookup and the `exec`; it is the extra cost of the handoff over launching the client itself.

Render bench: 200 frames on a 120×40 terminal.

Scroll bench (a separate measurement, not the frame cells above): 120 one-row scroll steps and 20 no-input redraws over a 2,081-row transcript (72 items, 148.4 KB of tool output, expanded) on a 120×40 terminal. Every scroll step moved the expanded output.

| Scroll metric | Median | Min–max | Samples |
| --- | --- | --- | --- |
| Held frame draw (p50) | 0.5 ms | 0.5 ms–0.5 ms | 5 |
| Held frame draw (p95) | 0.6 ms | 0.6 ms–1.0 ms | 5 |
| Scroll key handling (p50) | 0.1 µs | 0.1 µs–0.1 µs | 5 |
| Scroll key handling (p95) | 0.2 µs | 0.2 µs–0.5 µs | 5 |
| First held frame after scrolling | 0.9 ms | 0.9 ms–0.9 ms | 5 |
| Key dispatch through completed draw (p50) | 0.5 ms | 0.5 ms–0.5 ms | 5 |
| Key dispatch through completed draw (p95) | 0.6 ms | 0.6 ms–1.0 ms | 5 |
| First key dispatch through completed draw | 0.9 ms | 0.9 ms–0.9 ms | 5 |
| Held frame redraw without input (p50) | 0.5 ms | 0.5 ms–0.5 ms | 5 |
| Held frame redraw without input (p95) | 0.6 ms | 0.6 ms–0.6 ms | 5 |

Skipped: pty unavailable on Windows

<!-- bench-auto:end -->
