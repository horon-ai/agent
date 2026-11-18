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

`main` 에 올라간 커밋이 CI 와 구조 성능 게이트를 모두 통과하고 직전 측정 이후 에이전트 코드(`src`, `crates`, `Cargo.toml`, `Cargo.lock`, `.cargo`, `rust-toolchain.toml`, `scripts/perf`)가 바뀌었으면, `.github/workflows/benchmark-docs.yml` 이 Linux·macOS·Windows에서 전체 벤치마크 묶음을 OS마다 따로 돌리고 아래 구간을 `main` 과 `dev` 에서 함께 다시 씁니다. OS마다 절이 하나씩 있습니다. 측정 방법과 묶음별 뜻은 [`BENCHMARK.ko.md`](BENCHMARK.ko.md) 의 "자동 측정"에 있습니다.

<!-- bench-auto:start commit=729624b0fb7cecb86af64245930ff067825eb964 -->

## 헤드리스 (`horon-agent`)

이 구간은 `.github/workflows/benchmark-docs.yml` 이 자동으로 다시 씁니다. 손으로 고치지 않습니다.

- 커밋: `729624b0fb7cecb86af64245930ff067825eb964`
- 측정 시각: 2026-11-18 19:16 UTC
- 실행: <https://github.com/horon-ai/agent/actions/runs/44366651032> (원본 JSON 은 OS별 `benchmark-<os>-<run_id>` 아티팩트)

OS마다 다른 GitHub 호스티드 러너에서 따로 잽니다. 기계가 다르므로 OS끼리 절대값을 비교하지 않고, 같은 OS 안의 상대 비교(horon 대비 배수)와 커밋 사이의 추세만 읽습니다.

| OS | 러너 | 바이너리 | 상태 |
| --- | --- | --- | --- |
| Linux | `ubuntu-24.04` | `horon-agent v0.1.0-dev (729624b)` | 성공 |
| macOS | `macos-latest` | `horon-agent v0.1.0-dev (729624b)` | 성공 |
| Windows | `windows-latest` | `horon-agent v0.1.0-dev (729624b)` | 성공 |

### Linux

#### Coding agent comparison (headless, mock model)

Host: linux/x86_64, 4 CPUs, Python 3.12.16. Baseline: **horon**. Samples per one-shot, skill and MCP cell: 5.

| Tool | Version | `--version` (median) | Install size |
|---|---|---|---|
| horon | horon-agent v0.1.0-dev (729624b) | 2.0 ms | 110.6 MB |
| jcode | jcode v0.99.1 (7206efd0c) | 7.4 ms | 163.4 MB |
| Claude Code | 2.1.317 (Claude Code) | 7.2 ms | 632.2 MB |
| Codex CLI | codex-cli 0.166.0 | 32.5 ms | 435.0 MB |
| OpenCode | 1.19.5 | 348.9 ms | 613.4 MB |
| pi | 1.5.0 | 173.6 ms | 109.0 MB |
| oh-my-pi (omp) | omp/18.18.0 | 532.1 ms | 413.8 MB |
| oh-my-openagent (omo) | omo 5.1.64 (engine: senpi 2026.11.5-1) | 92.3 ms | 520.7 MB |

**One-shot headless prompt** (includes startup, one `ls` tool call, final answer)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Tool round-trip | fsync calls | request bytes | Note |
|---|---|---|---|---|---|---|---|---|---|
| horon | 42.5 ms | 40.8 ms–133.2 ms | baseline | 39.4 MB | baseline | yes | 10 | 3,756 / 2,368 (1 tools) | — |
| jcode | 138.0 ms | 137.7 ms–140.7 ms | 3.24× more | 42.2 MB | 1.07× more | yes | 5 | 3,292 / 1,433 (1 tools) | — |
| Claude Code | 362.7 ms | 360.0 ms–384.2 ms | 8.53× more | 204.5 MB | 5.19× more | yes | — | 45,780 / 29,653 (12 tools) | — |
| Codex CLI | 365.7 ms | 336.9 ms–388.3 ms | 8.60× more | 192.1 MB | 4.88× more | yes | 6 | 39,432 / 17,271 (9 tools) | — |
| OpenCode | 1.98 s | 1.94 s–2.12 s | 46.58× more | 571.6 MB | 14.51× more | yes | 4 | 2,659 / 0 (0 tools) | — |
| pi | 293.1 ms | 286.7 ms–297.6 ms | 6.89× more | 113.2 MB | 2.87× more | yes | — | 5,502 / 2,841 (4 tools) | — |
| oh-my-pi (omp) | 1.39 s | 1.32 s–1.40 s | 32.78× more | 354.4 MB | 9.00× more | yes | 67 | 29,858 / 18,533 (11 tools) | — |
| oh-my-openagent (omo) | 4.35 s | 4.25 s–4.93 s | 102.30× more | 651.0 MB | 16.53× more | yes | 36 | 27,443 / 2,424 (2 tools) | — |

`request bytes` is the first model request of the first run as the mock server received it: JSON body bytes / tool catalog bytes (tools offered).

**Core-profile one-shot** (the one-shot turn without `--tools`, so the request carries the default core surface: the eager tools, `toolbox` and the listed-tool index)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | Note | request bytes |
|---|---|---|---|---|---|---|---|---|---|
| horon | 111.9 ms | 91.9 ms–273.3 ms | baseline | 41.8 MB | baseline | — | yes | — | 11,125 / 9,079 (8 tools) |
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
| horon | 38.0 ms | 37.2 ms–39.5 ms | baseline | 38.1 MB | baseline | skill-uri | yes | — |
| jcode | 53.1 ms | 48.4 ms–61.6 ms | 1.40× more | 40.6 MB | 1.07× more | skill-tool | yes | — |
| Claude Code | 350.5 ms | 340.0 ms–354.7 ms | 9.22× more | 207.7 MB | 5.45× more | skill-tool | yes | — |
| Codex CLI | 405.2 ms | 374.2 ms–518.1 ms | 10.66× more | 197.9 MB | 5.20× more | file-read | yes | — |
| OpenCode | 2.26 s | 2.20 s–2.31 s | 59.46× more | 615.2 MB | 16.15× more | skill-tool | yes | — |
| pi | 411.3 ms | 371.3 ms–432.9 ms | 10.82× more | 126.0 MB | 3.31× more | file-read | yes | — |
| oh-my-pi (omp) | 1.42 s | 1.39 s–1.43 s | 37.25× more | 340.9 MB | 8.95× more | file-read | yes | — |
| oh-my-openagent (omo) | 4.72 s | 4.71 s–4.83 s | 124.26× more | 671.0 MB | 17.62× more | file-read | yes | — |

**MCP tool call** (process start to final answer, including starting the fixture stdio MCP server and one `bench_echo` call)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | MCP ready | Note |
|---|---|---|---|---|---|---|---|---|---|
| horon | 58.8 ms | 57.2 ms–59.5 ms | baseline | 43.1 MB | baseline | mcp-call | yes | 48.0 ms | — |
| jcode | 162.9 ms | 161.3 ms–164.1 ms | 2.77× more | 53.1 MB | 1.23× more | mcp-action | yes | 140.0 ms | — |
| Claude Code | 426.7 ms | 422.7 ms–444.4 ms | 7.26× more | 225.4 MB | 5.23× more | mcp-tool | yes | 300.5 ms | — |
| Codex CLI | 335.5 ms | 321.3 ms–442.2 ms | 5.71× more | 196.2 MB | 4.55× more | mcp-tool | yes | 218.0 ms | — |
| OpenCode | 2.18 s | 2.15 s–2.20 s | 37.18× more | 568.5 MB | 13.19× more | mcp-tool | yes | 2.00 s | — |
| pi | 428.0 ms | 408.1 ms–445.3 ms | 7.28× more | 139.4 MB | 3.24× more | mcp-tool | yes | 387.1 ms | — |
| oh-my-pi (omp) | 1.41 s | 1.39 s–1.57 s | 23.98× more | 397.5 MB | 9.22× more | mcp-route | yes | 1.33 s | — |
| oh-my-openagent (omo) | — | — | — | — | — | mcp-tool | no | — | failed: the fixture sentinel never came back |

**Session memory** (idle after 3 turn(s) per session; sizes are process-tree sums, RSS; PSS also recorded on Linux). "Each additional" is the least-squares slope across the measured session counts; 0 B means growth stayed below RSS sampling noise. Threads and descriptors come from the daemon's own `server:memory` after the turns. Processes are counted in the same tree walk as the RSS sample (PSS shares it on Linux), so the count and the bytes describe one observation; a PID shared by two session roots counts once, a root that already exited is counted as neither bytes nor process, and a cell that could not be read is —, never 0.

| Tool | First session | Each additional | vs horon (additional) | Threads/session | FDs/session | Processes (idle) | Processes/session | First (post-turn) | Each additional (post-turn) |
|---|---|---|---|---|---|---|---|---|---|
| horon | 37.0 MB | 151.0 KB | baseline | 0.68 | 1.00 | 1 | 0.00 | 44.8 MB | 500.4 KB |
| jcode | 42.8 MB | 98.1 KB | 1.54× less | 0.22 | — | 1 | 0.00 | 46.3 MB | 242.9 KB |
| Claude Code | 151.6 MB | 149.8 MB | 1,016× more | — | — | 1 | 1.00 | 187.2 MB | 180.7 MB |
| Codex CLI | 129.1 MB | 132.8 MB | 900.77× more | — | — | 1 | 1.00 | 161.4 MB | 164.6 MB |
| OpenCode | 244.9 MB | 290.3 KB | 1.92× more | — | — | 1 | 0.00 | 456.5 MB | 6.9 MB |
| pi | 109.2 MB | 109.8 MB | 744.73× more | — | — | 1 | 1.00 | 131.5 MB | 128.0 MB |
| oh-my-pi (omp) | 321.0 MB | 121.5 MB | 824.07× more | — | — | 1 | 1.00 | 366.3 MB | 354.3 MB |
| oh-my-openagent (omo) | 308.8 MB | 41.8 MB | 283.88× more | — | — | 2 | 0.89 | 584.5 MB | 566.0 MB |

Threads/descriptors unavailable: jcode (server:memory did not report fd_count)

**Post-turn tree RSS per session count**

| Tool | 1 session(s) | 5 session(s) | 10 session(s) |
|---|---|---|---|
| horon | 44.8 MB | 47.1 MB | 49.2 MB |
| jcode | 46.3 MB | 47.5 MB | 48.5 MB |
| Claude Code | 187.2 MB | 916.4 MB | 1.8 GB |
| Codex CLI | 161.4 MB | 811.1 MB | 1.6 GB |
| OpenCode | 456.5 MB | 526.6 MB | 521.9 MB |
| pi | 131.5 MB | 646.0 MB | 1.3 GB |
| oh-my-pi (omp) | 366.3 MB | 1.8 GB | 3.5 GB |
| oh-my-openagent (omo) | 584.5 MB | 3.1 GB | 5.6 GB |

**Live processes per session count** (idle / post-turn)

| Tool | 1 session(s) | 5 session(s) | 10 session(s) |
|---|---|---|---|
| horon | 1 / 1 | 1 / 1 | 1 / 1 |
| jcode | 1 / 1 | 1 / 1 | 1 / 1 |
| Claude Code | 1 / 1 | 5 / 5 | 10 / 10 |
| Codex CLI | 1 / 1 | 5 / 5 | 10 / 10 |
| OpenCode | 1 / 1 | 1 / 1 | 1 / 1 |
| pi | 1 / 1 | 5 / 5 | 10 / 10 |
| oh-my-pi (omp) | 1 / 1 | 5 / 5 | 10 / 10 |
| oh-my-openagent (omo) | 2 / 4 | 5 / 20 | 10 / 40 |

#### 기동

```text
Benchmarking: /home/runner/work/agent/agent/bench-bin/horon-agent
============================================================

--help (binary load):
  Min:    1.93 ms
  Max:    2.47 ms
  Mean:   2.06 ms
  Median: 1.98 ms
  Stdev:  0.17 ms

--version:
  Min:    1.78 ms
  Max:    2.30 ms
  Mean:   1.92 ms
  Median: 1.85 ms
  Stdev:  0.17 ms

Measuring isolated server startup (10 runs)...

Server ready (isolated socket connectable):
  Min:    20.48 ms
  Max:    20.55 ms
  Mean:   20.52 ms
  Median: 20.53 ms
  Stdev:  0.02 ms

Server startup stages (one extra run with HORON_AGENT_STARTUP_PROFILE=1, not timed):
  stage             at ms  delta ms   share
  panic_hook          0.0       0.0    0.5%
  logging_init        0.1       0.1   16.0%
  nofile_limit        0.4       0.2   24.5%
  perm_harden         0.4       0.0    0.9%
  args_parse          0.4       0.0    0.1%
  config_loaded       0.5       0.1   12.6%
  server_new          0.8       0.3   38.4%
  socket_bound        0.9       0.1    7.0%

Measuring harness bridge readiness (10 runs)...

Bridge stage: api socket connectable:
  Min:    20.61 ms
  Max:    20.91 ms
  Mean:   20.73 ms
  Median: 20.69 ms
  Stdev:  0.11 ms

Bridge stage: hello_ok:
  Min:    21.16 ms
  Max:    21.55 ms
  Mean:   21.29 ms
  Median: 21.23 ms
  Stdev:  0.13 ms

Bridge stage: no-op reply:
  Min:    21.47 ms
  Max:    21.87 ms
  Mean:   21.60 ms
  Median: 21.56 ms
  Stdev:  0.14 ms

Bridge total (socket -> no-op):
  Min:    21.47 ms
  Max:    21.87 ms
  Mean:   21.60 ms
  Median: 21.56 ms
  Stdev:  0.14 ms

============================================================
Summary:
  Binary load median:      ~2.0 ms
  Server ready median:     ~20.5 ms
  Bridge total median:     ~21.6 ms
```

#### 세션 로딩 (COLD)

```text
binary   /home/runner/work/agent/agent/bench-bin/examples/loading_probe (8d6d13968d3e)
stages   /home/runner/work/agent/agent/bench-bin/examples/resume_stage_probe (e4ba7ae54616, 3 cold pass(es) per session fixture)
mode     cold
protocol COLD: every measured run gets its own freshly copied HOME; no derived cache from an earlier run exists, so each sample is a first load of the fixture.
fixtures /tmp/horon-loading-bench-7t4i93nw
note     --warmup 3 is ignored in cold mode: every sample gets a fresh HOME
fixture events=0 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    0.016/0.016    p95    0.017/0.019    delta  +0.000 ms  control-no-change
  skill_request_overlay p50    0.120/0.119    p95    0.124/0.142    delta  -0.001 ms  control-no-change
  mcp_config_load   p50    0.159/0.160    p95    0.199/0.189    delta  +0.001 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  session_save      p50    8.256/8.181    p95    9.704/8.485    delta  -0.075 ms  control-no-change
  total store_read_bytes p50 23072  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition not_applicable: this fixture has no session log
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 23064..23084 (6 distinct values)
  SPREAD   b session_save store_read_bytes 23064..23088 (6 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    2.961/2.941    p95    3.031/2.971    delta  -0.020 ms  control-no-change
  skill_request_overlay p50    5.174/5.179    p95    5.214/5.235    delta  +0.005 ms  control-no-change
  mcp_config_load   p50    0.170/0.169    p95    0.175/0.181    delta  -0.001 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.001    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  session_save      p50   10.934/11.098   p95   11.523/17.803   delta  +0.164 ms  control-no-change
  total store_read_bytes p50 23072  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition not_applicable: this fixture has no session log
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 23044..23080 (6 distinct values)
  SPREAD   b session_save store_read_bytes 23064..23084 (6 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.016/0.016    p95    0.018/0.031    delta  -0.000 ms  control-no-change
  skill_request_overlay p50    0.123/0.119    p95    0.146/0.124    delta  -0.004 ms  control-no-change
  mcp_config_load   p50    0.165/0.159    p95    0.194/0.164    delta  -0.006 ms  control-no-change
  attach_history    p50  244.607/244.929  p95  248.073/247.476  delta  +0.322 ms  control-no-change
  resume_ready      p50   56.581/56.510   p95   57.384/57.207   delta  -0.071 ms  control-no-change
  provider_dispatch p50    1.345/1.352    p95    1.380/1.377    delta  +0.007 ms  control-no-change
  session_save      p50    7.728/7.635    p95    9.522/7.796    delta  -0.092 ms  control-no-change
  total store_read_bytes p50 5311394  full_source_hashes p50 2  skill_dir_reads p50 1
  decomposition 3/3 cold pass(es); anchor attach_history p50 244.607 ms (load 204.242 + history 40.037)
    selection          p50     0.036 ms  min     0.035  max     0.037  ratio 0.0% of anchor
    checkpoint_recent  p50   164.473 ms  min   163.171  max   164.560  ratio 67.2% of anchor
    log_read           p50     0.067 ms  min     0.062  max     0.070  ratio 0.0% of anchor
    full_scan          p50    22.327 ms  min    22.136  max    27.560  ratio 9.1% of anchor
    blob_read          p50     0.005 ms  min     0.004  max     0.005  ratio 0.0% of anchor
    graph_replay       p50    16.419 ms  min    15.694  max    19.925  ratio 6.7% of anchor
    metadata_json      p50     0.006 ms  min     0.006  max     0.007  ratio 0.0% of anchor
    messages_json      p50     0.929 ms  min     0.928  max     0.930  ratio 0.4% of anchor
    messages_decode    p50     0.988 ms  min     0.986  max     0.997  ratio 0.4% of anchor
    source_hash        p50     0.515 ms  min     0.511  max     0.528  ratio 0.2% of anchor
    history_rescan     p50    42.764 ms  min    42.645  max    42.889  ratio 17.5% of anchor
    page_index         p50    48.137 ms  min    47.918  max    49.759  ratio 19.7% of anchor
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189187..4189191 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099129..1099133 (3 distinct values)
  SPREAD   a session_save store_read_bytes 23060..23078 (6 distinct values)
  SPREAD   b attach_history store_read_bytes 4189187..4189191 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099129..1099133 (3 distinct values)
  SPREAD   b session_save store_read_bytes 23068..23085 (6 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    2.942/2.936    p95    3.039/2.986    delta  -0.006 ms  control-no-change
  skill_request_overlay p50    5.168/5.170    p95    5.242/5.198    delta  +0.002 ms  control-no-change
  mcp_config_load   p50    0.170/0.170    p95    0.180/0.172    delta  +0.000 ms  control-no-change
  attach_history    p50  249.820/250.538  p95  253.367/253.008  delta  +0.718 ms  control-no-change
  resume_ready      p50   56.762/56.890   p95   57.227/57.549   delta  +0.128 ms  control-no-change
  provider_dispatch p50    1.355/1.351    p95    1.377/1.403    delta  -0.005 ms  control-no-change
  session_save      p50    7.743/7.867    p95    8.119/7.981    delta  +0.125 ms  control-no-change
  total store_read_bytes p50 5311396  full_source_hashes p50 2  skill_dir_reads p50 1
  decomposition 3/3 cold pass(es); anchor attach_history p50 249.820 ms (load 209.203 + history 40.614)
    selection          p50     0.036 ms  min     0.035  max     0.037  ratio 0.0% of anchor
    checkpoint_recent  p50   166.516 ms  min   164.387  max   167.176  ratio 66.7% of anchor
    log_read           p50     0.054 ms  min     0.053  max     0.059  ratio 0.0% of anchor
    full_scan          p50    22.070 ms  min    21.944  max    22.386  ratio 8.8% of anchor
    blob_read          p50     0.003 ms  min     0.003  max     0.005  ratio 0.0% of anchor
    graph_replay       p50    16.100 ms  min    16.028  max    17.436  ratio 6.4% of anchor
    metadata_json      p50     0.007 ms  min     0.007  max     0.007  ratio 0.0% of anchor
    messages_json      p50     0.951 ms  min     0.950  max     0.955  ratio 0.4% of anchor
    messages_decode    p50     1.020 ms  min     1.019  max     1.090  ratio 0.4% of anchor
    source_hash        p50     0.518 ms  min     0.513  max     0.522  ratio 0.2% of anchor
    history_rescan     p50    42.940 ms  min    42.486  max    44.204  ratio 17.2% of anchor
    page_index         p50    47.055 ms  min    46.754  max    50.116  ratio 18.8% of anchor
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189187..4189191 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099129..1099133 (3 distinct values)
  SPREAD   a session_save store_read_bytes 23062..23078 (6 distinct values)
  SPREAD   b attach_history store_read_bytes 4189189..4189191 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099132..1099133 (2 distinct values)
  SPREAD   b session_save store_read_bytes 23058..23082 (7 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.016/0.016    p95    0.018/0.018    delta  -0.000 ms  control-no-change
  skill_request_overlay p50    0.122/0.121    p95    0.125/0.125    delta  -0.000 ms  control-no-change
  mcp_config_load   p50    0.165/0.173    p95    0.174/0.189    delta  +0.009 ms  control-no-change
  attach_history    p50 3122.106/3132.593 p95 3162.357/3163.345 delta +10.488 ms  control-no-change
  resume_ready      p50  625.564/626.914  p95  629.336/637.694  delta  +1.350 ms  control-no-change
  provider_dispatch p50   15.123/15.028   p95   15.871/15.629   delta  -0.095 ms  control-no-change
  session_save      p50    8.106/8.019    p95    8.405/8.673    delta  -0.087 ms  control-no-change
  total store_read_bytes p50 53417604  full_source_hashes p50 2  skill_dir_reads p50 1
  decomposition 3/3 cold pass(es); anchor attach_history p50 3122.106 ms (load 2630.686 + history 489.319)
    selection          p50     0.040 ms  min     0.038  max     0.047  ratio 0.0% of anchor
    checkpoint_recent  p50  2155.634 ms  min  2133.037  max  2168.011  ratio 69.0% of anchor
    log_read           p50     0.595 ms  min     0.577  max     0.602  ratio 0.0% of anchor
    full_scan          p50   239.264 ms  min   238.700  max   239.371  ratio 7.7% of anchor
    blob_read          p50     0.066 ms  min     0.065  max     0.066  ratio 0.0% of anchor
    graph_replay       p50   213.222 ms  min   210.484  max   219.906  ratio 6.8% of anchor
    metadata_json      p50     0.005 ms  min     0.005  max     0.005  ratio 0.0% of anchor
    messages_json      p50     8.538 ms  min     8.501  max     8.591  ratio 0.3% of anchor
    messages_decode    p50    14.472 ms  min    14.234  max    14.539  ratio 0.5% of anchor
    source_hash        p50     4.984 ms  min     4.976  max     4.996  ratio 0.2% of anchor
    history_rescan     p50   504.921 ms  min   500.063  max   513.551  ratio 16.2% of anchor
    page_index         p50   518.304 ms  min   509.492  max   522.737  ratio 16.6% of anchor
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184285..42184289 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210239..11210243 (3 distinct values)
  SPREAD   a session_save store_read_bytes 23059..23089 (9 distinct values)
  SPREAD   b attach_history store_read_bytes 42184285..42184289 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210239..11210243 (3 distinct values)
  SPREAD   b session_save store_read_bytes 23059..23081 (5 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    2.924/2.939    p95    2.974/3.003    delta  +0.015 ms  control-no-change
  skill_request_overlay p50    5.159/5.172    p95    5.221/5.362    delta  +0.013 ms  control-no-change
  mcp_config_load   p50    0.170/0.171    p95    0.181/0.184    delta  +0.001 ms  control-no-change
  attach_history    p50 3126.578/3131.026 p95 3177.690/3137.221 delta  +4.448 ms  control-no-change
  resume_ready      p50  626.952/623.922  p95  633.827/633.999  delta  -3.031 ms  control-no-change
  provider_dispatch p50   15.607/15.457   p95   15.631/17.968   delta  -0.150 ms  control-no-change
  session_save      p50    8.205/8.117    p95    8.677/8.396    delta  -0.088 ms  control-no-change
  total store_read_bytes p50 53417607  full_source_hashes p50 2  skill_dir_reads p50 1
  decomposition 3/3 cold pass(es); anchor attach_history p50 3126.578 ms (load 2636.065 + history 492.923)
    selection          p50     0.042 ms  min     0.039  max     0.044  ratio 0.0% of anchor
    checkpoint_recent  p50  2141.092 ms  min  2131.222  max  2151.906  ratio 68.5% of anchor
    log_read           p50     0.457 ms  min     0.454  max     0.485  ratio 0.0% of anchor
    full_scan          p50   239.739 ms  min   239.714  max   239.800  ratio 7.7% of anchor
    blob_read          p50     0.063 ms  min     0.063  max     0.066  ratio 0.0% of anchor
    graph_replay       p50   212.375 ms  min   211.854  max   213.929  ratio 6.8% of anchor
    metadata_json      p50     0.006 ms  min     0.005  max     0.006  ratio 0.0% of anchor
    messages_json      p50     8.586 ms  min     8.330  max     8.718  ratio 0.3% of anchor
    messages_decode    p50    14.627 ms  min    13.871  max    14.679  ratio 0.5% of anchor
    source_hash        p50     4.945 ms  min     4.938  max     4.946  ratio 0.2% of anchor
    history_rescan     p50   507.784 ms  min   502.572  max   508.162  ratio 16.2% of anchor
    page_index         p50   519.466 ms  min   515.167  max   519.701  ratio 16.6% of anchor
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184282..42184289 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210236..11210243 (2 distinct values)
  SPREAD   a session_save store_read_bytes 23061..23081 (5 distinct values)
  SPREAD   b attach_history store_read_bytes 42184285..42184289 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210239..11210243 (3 distinct values)
  SPREAD   b session_save store_read_bytes 23068..23085 (7 distinct values)
report   bench-results/loading-cold.json
mode cold, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### 세션 로딩 (WARM)

```text
binary   /home/runner/work/agent/agent/bench-bin/examples/loading_probe (8d6d13968d3e)
stages   /home/runner/work/agent/agent/bench-bin/examples/resume_stage_probe (e4ba7ae54616, 0 cold pass(es) per session fixture)
mode     warm
protocol WARM: one HOME per side, --warmup discarded runs per side, then measured runs alternate between the two sides on those same HOMEs.
fixtures /tmp/horon-loading-bench-fpjjov0p
fixture events=0 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    0.007/0.007    p95    0.008/0.007    delta  +0.000 ms  control-no-change
  skill_request_overlay p50    0.038/0.039    p95    0.040/0.039    delta  +0.001 ms  control-no-change
  mcp_config_load   p50    0.054/0.054    p95    0.056/0.076    delta  +0.001 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  session_save      p50    4.255/4.254    p95    4.324/4.335    delta  -0.001 ms  control-no-change
  total store_read_bytes p50 23798  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition not_applicable: this fixture has no session log
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23077, 23085, 23792, 23796, 23800, 24507, 24523]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23077, 23085, 23792, 23800, 23804, 24499, 24515, 24519]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23077..24523 (7 distinct values)
  SPREAD   b session_save store_read_bytes 23077..24519 (8 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    1.186/1.172    p95    1.329/1.211    delta  -0.014 ms  control-no-change
  skill_request_overlay p50    1.845/1.845    p95    1.943/1.910    delta  -0.000 ms  control-no-change
  mcp_config_load   p50    0.058/0.059    p95    0.069/0.077    delta  +0.001 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  session_save      p50    4.778/4.782    p95    4.959/4.939    delta  +0.004 ms  control-no-change
  total store_read_bytes p50 23798  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition not_applicable: this fixture has no session log
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23057, 23077, 23778, 23796, 23800, 24513, 24515, 24523, 24531]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23069, 23071, 23081, 23085, 23796, 23804, 24503, 24511]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23057..24531 (9 distinct values)
  SPREAD   b session_save store_read_bytes 23069..24511 (8 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.006/0.006    p95    0.007/0.007    delta  -0.000 ms  control-no-change
  skill_request_overlay p50    0.038/0.038    p95    0.038/0.039    delta  +0.000 ms  control-no-change
  mcp_config_load   p50    0.053/0.054    p95    0.075/0.064    delta  +0.000 ms  control-no-change
  attach_history    p50   57.671/57.578   p95   58.344/59.691   delta  -0.093 ms  control-no-change
  resume_ready      p50   27.256/27.321   p95   27.581/27.615   delta  +0.066 ms  control-no-change
  provider_dispatch p50    0.921/0.917    p95    0.931/0.927    delta  -0.004 ms  control-no-change
  session_save      p50    4.493/4.428    p95    4.923/6.178    delta  -0.065 ms  control-no-change
  total store_read_bytes p50 2841369  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition skipped: --stage-probe-samples is 0
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23793, 23797, 24506, 24518, 25225, 25233, 25952, 25956, 26671, 26679]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23793, 23801, 24508, 24516, 25227, 25231, 25946, 25962, 26657, 26673]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23793..26679 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23793..26673 (10 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    1.329/1.297    p95    1.495/1.408    delta  -0.032 ms  control-no-change
  skill_request_overlay p50    1.847/1.854    p95    1.961/2.032    delta  +0.007 ms  control-no-change
  mcp_config_load   p50    0.058/0.059    p95    0.075/0.074    delta  +0.000 ms  control-no-change
  attach_history    p50   57.522/57.971   p95   57.958/58.662   delta  +0.449 ms  control-no-change
  resume_ready      p50   27.311/27.372   p95   27.447/27.684   delta  +0.061 ms  control-no-change
  provider_dispatch p50    0.921/0.909    p95    0.943/0.957    delta  -0.012 ms  control-no-change
  session_save      p50    4.884/4.777    p95    5.006/5.193    delta  -0.106 ms  control-no-change
  total store_read_bytes p50 2841369  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition skipped: --stage-probe-samples is 0
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23801, 23805, 24512, 24520, 25227, 25231, 25938, 25950, 26673, 26681]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23797, 23805, 24510, 24518, 25233, 25245, 25956, 25960, 26675, 26687]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23801..26681 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23797..26687 (10 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.005/0.006    p95    0.006/0.006    delta  +0.000 ms  control-no-change
  skill_request_overlay p50    0.038/0.038    p95    0.039/0.048    delta  +0.000 ms  control-no-change
  mcp_config_load   p50    0.053/0.054    p95    0.065/0.062    delta  +0.001 ms  control-no-change
  attach_history    p50  660.937/658.179  p95  671.426/666.060  delta  -2.758 ms  control-no-change
  resume_ready      p50  325.681/324.241  p95  327.146/328.949  delta  -1.440 ms  control-no-change
  provider_dispatch p50   10.914/10.845   p95   11.066/11.046   delta  -0.069 ms  control-no-change
  session_save      p50    4.534/4.825    p95    5.219/5.208    delta  +0.291 ms  control-no-change
  total store_read_bytes p50 28640400  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition skipped: --stage-probe-samples is 0
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23799, 23807, 24514, 24522, 25241, 25249, 25960, 25968, 26671, 26679]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23803, 24514, 24530, 25223, 25237, 25948, 25954, 26661, 26675]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23799..26679 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23803..26675 (9 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    1.303/1.294    p95    1.395/1.310    delta  -0.009 ms  control-no-change
  skill_request_overlay p50    1.870/1.859    p95    2.112/1.997    delta  -0.010 ms  control-no-change
  mcp_config_load   p50    0.058/0.058    p95    0.060/0.059    delta  +0.001 ms  control-no-change
  attach_history    p50  649.513/653.805  p95  660.267/662.220  delta  +4.292 ms  control-no-change
  resume_ready      p50  329.823/327.121  p95  331.761/332.434  delta  -2.702 ms  control-no-change
  provider_dispatch p50   10.905/10.955   p95   11.133/11.232   delta  +0.050 ms  control-no-change
  session_save      p50    5.472/5.456    p95    5.865/5.737    delta  -0.016 ms  control-no-change
  total store_read_bytes p50 28640403  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition skipped: --stage-probe-samples is 0
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23786, 23804, 24523, 24525, 25246, 25953, 25961, 26668, 26678]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23800, 23802, 24513, 24519, 25242, 25246, 25935, 25957, 26672, 26676]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23786..26678 (9 distinct values)
  SPREAD   b session_save store_read_bytes 23800..26676 (10 distinct values)
report   bench-results/loading-warm.json
mode warm, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### Tool latency

| Tool case | median | p95 | min | max | samples |
|---|---|---|---|---|---|
| read (tiny file) | 0.2 ms | 0.3 ms | 0.1 ms | 0.3 ms | 5 |
| read (1000 lines) | 0.2 ms | 0.3 ms | 0.2 ms | 0.3 ms | 5 |
| read (10 lines @ offset) | 0.2 ms | 0.3 ms | 0.2 ms | 0.3 ms | 5 |
| read (repo Cargo.toml) | 0.2 ms | 0.2 ms | 0.1 ms | 0.2 ms | 5 |
| write (small) | 0.6 ms | 0.7 ms | 0.5 ms | 0.7 ms | 5 |
| write (10KB) | 0.8 ms | 1.0 ms | 0.7 ms | 1.0 ms | 5 |
| edit (simple replace) | 0.6 ms | 0.7 ms | 0.6 ms | 0.7 ms | 5 |
| ls (scripts dir) | 0.4 ms | 0.5 ms | 0.4 ms | 0.5 ms | 5 |
| agentgrep (grep in tree) | 1.2 ms | 1.3 ms | 1.2 ms | 1.3 ms | 5 |
| agentgrep (find **/*.rs) | 0.9 ms | 1.0 ms | 0.9 ms | 1.0 ms | 5 |
| bash (true) | 1.4 ms | 1.5 ms | 1.2 ms | 1.5 ms | 5 |
| bash (echo hello) | 1.3 ms | 1.4 ms | 1.3 ms | 1.4 ms | 5 |
| bash (ls fixture tree) | 2.2 ms | 2.3 ms | 2.2 ms | 2.3 ms | 5 |
| invalid tool (dispatch only) | 0.0 ms | 0.1 ms | 0.0 ms | 0.1 ms | 5 |

#### Tool CPU per call

| Tool case | daemon cpu median | min | max | child cpu median | voluntary switches | involuntary switches | windows × calls | source |
|---|---|---|---|---|---|---|---|---|
| read (tiny file) | 0.119 ms | 0.111 ms | 0.123 ms | 0.000 ms | 4.98 | 0.00 | 5 × 50 | schedstat |
| read (1000 lines) | 0.205 ms | 0.202 ms | 0.213 ms | 0.000 ms | 4.98 | 0.00 | 5 × 50 | schedstat |
| read (10 lines @ offset) | 0.196 ms | 0.194 ms | 0.198 ms | 0.000 ms | 4.16 | 0.00 | 5 × 50 | schedstat |
| read (repo Cargo.toml) | 0.149 ms | 0.146 ms | 0.158 ms | 0.000 ms | 5.02 | 0.00 | 5 × 50 | schedstat |
| write (small) | 0.416 ms | 0.385 ms | 0.422 ms | 0.000 ms | 17.20 | 0.80 | 5 × 50 | schedstat |
| write (10KB) | 0.586 ms | 0.506 ms | 0.600 ms | 0.000 ms | 17.76 | 0.12 | 5 × 50 | schedstat |
| edit (simple replace) | 0.411 ms | 0.363 ms | 0.551 ms | 0.000 ms | 17.24 | 0.00 | 5 × 50 | schedstat |
| ls (scripts dir) | 0.433 ms | 0.430 ms | 0.445 ms | 0.000 ms | 5.50 | 0.00 | 5 × 50 | schedstat |
| agentgrep (grep in tree) | 0.857 ms | 0.847 ms | 0.861 ms | 0.000 ms | 9.82 | 0.12 | 5 × 50 | schedstat |
| agentgrep (find **/*.rs) | 0.886 ms | 0.842 ms | 0.902 ms | 0.000 ms | 5.66 | 0.00 | 5 × 50 | schedstat |
| bash (true) | 0.420 ms | 0.413 ms | 0.427 ms | 1.000 ms | 7.74 | 0.00 | 5 × 50 | schedstat |
| bash (echo hello) | 0.439 ms | 0.428 ms | 0.444 ms | 0.800 ms | 8.18 | 0.00 | 5 × 50 | schedstat |
| bash (ls fixture tree) | 0.526 ms | 0.524 ms | 0.526 ms | 2.000 ms | 8.80 | 0.00 | 5 × 50 | schedstat |
| invalid tool (dispatch only) | 0.023 ms | 0.022 ms | 0.024 ms | 0.000 ms | 2.98 | 0.00 | 5 × 50 | schedstat |

#### Reload storm

1 resume(s) per level; each level resumes sessions no other level has opened, from a freshly started daemon.

| Concurrent resumes | history median | p95 | max | storm wall | failures |
|---|---|---|---|---|---|
| 1 | 39.8 ms | 39.8 ms | 39.8 ms | 39.9 ms | 0 |
| 10 | 130.6 ms | 147.7 ms | 147.7 ms | 148.2 ms | 0 |
| 40 | 573.4 ms | 580.3 ms | 581.1 ms | 581.9 ms | 0 |

#### Compile loop

| Checkpoint | Touched | median | min | max | samples |
|---|---|---|---|---|---|
| warm_noop | — | 223.5 ms | 223.5 ms | 223.5 ms | 1 |
| leaf_touch | src/main.rs | 266.9 ms | 266.9 ms | 266.9 ms | 1 |
| core_touch | crates/app-core/src/lib.rs | 18.14 s | 18.14 s | 18.14 s | 1 |
| build_meta_touch | crates/build-meta/src/lib.rs | 25.08 s | 25.08 s | 25.08 s | 1 |

### macOS

#### Coding agent comparison (headless, mock model)

Host: darwin/arm64, 3 CPUs, Python 3.12.10. Baseline: **horon**. Samples per one-shot, skill and MCP cell: 5.

Skipped: syscalls (strace not available)

| Tool | Version | `--version` (median) | Install size |
|---|---|---|---|
| horon | horon-agent v0.1.0-dev (729624b) | 14.7 ms | 93.2 MB |
| jcode | jcode v0.99.1 (7206efd0c) | 10.4 ms | 136.3 MB |
| Claude Code | 2.1.317 (Claude Code) | 28.5 ms | 596.8 MB |
| Codex CLI | codex-cli 0.166.0 | 76.9 ms | 346.5 MB |
| OpenCode | 1.19.5 | 616.6 ms | 344.6 MB |
| pi | 1.5.0 | 254.5 ms | 109.0 MB |
| oh-my-pi (omp) | omp/18.18.0 | 1.04 s | 274.2 MB |
| oh-my-openagent (omo) | omo 5.1.64 (engine: senpi 2026.11.5-1) | 211.8 ms | 502.4 MB |

**One-shot headless prompt** (includes startup, one `ls` tool call, final answer)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Tool round-trip | request bytes | Note |
|---|---|---|---|---|---|---|---|---|
| horon | 88.1 ms | 76.2 ms–544.4 ms | baseline | 31.2 MB | baseline | yes | 3,597 / 2,368 (1 tools) | — |
| jcode | 377.1 ms | 333.0 ms–743.2 ms | 4.28× more | 33.9 MB | 1.09× more | yes | 3,133 / 1,433 (1 tools) | — |
| Claude Code | 628.6 ms | 565.6 ms–1.18 s | 7.14× more | 201.3 MB | 6.46× more | yes | 45,798 / 29,653 (12 tools) | — |
| Codex CLI | 495.0 ms | 474.1 ms–1.56 s | 5.62× more | 142.9 MB | 4.59× more | yes | 39,453 / 17,271 (9 tools) | — |
| OpenCode | 4.51 s | 4.01 s–5.31 s | 51.22× more | 710.2 MB | 22.79× more | yes | 2,659 / 0 (0 tools) | — |
| pi | 480.2 ms | 429.5 ms–508.4 ms | 5.45× more | 120.1 MB | 3.85× more | yes | 5,513 / 2,841 (4 tools) | — |
| oh-my-pi (omp) | 2.59 s | 2.00 s–3.24 s | 29.36× more | 468.6 MB | 15.04× more | yes | 29,853 / 18,533 (11 tools) | — |
| oh-my-openagent (omo) | 11.54 s | 9.76 s–13.21 s | 131.04× more | 882.9 MB | 28.34× more | yes | 27,493 / 2,424 (2 tools) | — |

`request bytes` is the first model request of the first run as the mock server received it: JSON body bytes / tool catalog bytes (tools offered).

**Core-profile one-shot** (the one-shot turn without `--tools`, so the request carries the default core surface: the eager tools, `toolbox` and the listed-tool index)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | Note | request bytes |
|---|---|---|---|---|---|---|---|---|---|
| horon | 104.8 ms | 80.8 ms–156.4 ms | baseline | 32.9 MB | baseline | — | yes | — | 10,966 / 9,079 (8 tools) |
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
| horon | 64.0 ms | 60.0 ms–74.4 ms | baseline | 24.3 MB | baseline | skill-uri | yes | — |
| jcode | 357.6 ms | 317.7 ms–384.3 ms | 5.58× more | 37.2 MB | 1.53× more | skill-tool | yes | — |
| Claude Code | 506.2 ms | 347.2 ms–635.1 ms | 7.90× more | 202.5 MB | 8.35× more | skill-tool | yes | — |
| Codex CLI | 979.7 ms | 656.0 ms–1.70 s | 15.30× more | 143.6 MB | 5.92× more | file-read | yes | — |
| OpenCode | 4.04 s | 2.52 s–5.09 s | 63.09× more | 717.1 MB | 29.55× more | skill-tool | yes | — |
| pi | 994.4 ms | 820.5 ms–1.16 s | 15.53× more | 143.1 MB | 5.90× more | file-read | yes | — |
| oh-my-pi (omp) | 2.36 s | 1.91 s–3.81 s | 36.81× more | 466.2 MB | 19.21× more | file-read | yes | — |
| oh-my-openagent (omo) | 12.19 s | 11.82 s–15.45 s | 190.32× more | 890.6 MB | 36.70× more | file-read | yes | — |

**MCP tool call** (process start to final answer, including starting the fixture stdio MCP server and one `bench_echo` call)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | MCP ready | Note |
|---|---|---|---|---|---|---|---|---|---|
| horon | 115.1 ms | 107.7 ms–117.6 ms | baseline | 44.6 MB | baseline | mcp-call | yes | 88.2 ms | — |
| jcode | 486.4 ms | 417.4 ms–518.7 ms | 4.22× more | 43.9 MB | same | mcp-action | yes | 409.6 ms | — |
| Claude Code | 551.6 ms | 536.6 ms–557.1 ms | 4.79× more | 215.5 MB | 4.83× more | mcp-tool | yes | 421.4 ms | — |
| Codex CLI | 540.7 ms | 418.4 ms–625.6 ms | 4.70× more | 142.8 MB | 3.20× more | mcp-tool | yes | 370.1 ms | — |
| OpenCode | 3.59 s | 2.73 s–4.04 s | 31.15× more | 731.7 MB | 16.40× more | mcp-tool | yes | 2.72 s | — |
| pi | 1.09 s | 858.6 ms–1.20 s | 9.47× more | 151.6 MB | 3.40× more | mcp-tool | yes | 964.6 ms | — |
| oh-my-pi (omp) | 2.59 s | 2.04 s–3.09 s | 22.51× more | 437.1 MB | 9.80× more | mcp-route | yes | 2.12 s | — |
| oh-my-openagent (omo) | — | — | — | — | — | mcp-tool | no | — | failed: the fixture sentinel never came back |

**Session memory** (idle after 3 turn(s) per session; sizes are process-tree sums, RSS; PSS also recorded on Linux). "Each additional" is the least-squares slope across the measured session counts; 0 B means growth stayed below RSS sampling noise. Threads and descriptors come from the daemon's own `server:memory` after the turns. Processes are counted in the same tree walk as the RSS sample (PSS shares it on Linux), so the count and the bytes describe one observation; a PID shared by two session roots counts once, a root that already exited is counted as neither bytes nor process, and a cell that could not be read is —, never 0.

| Tool | First session | Each additional | vs horon (additional) | Threads/session | FDs/session | Processes (idle) | Processes/session | First (post-turn) | Each additional (post-turn) |
|---|---|---|---|---|---|---|---|---|---|
| horon | 29.0 MB | 132.5 KB | baseline | 0.57 | 1.00 | 1 | 0.00 | 35.2 MB | 384.3 KB |
| jcode | 36.3 MB | 200.1 KB | 1.51× more | — | — | 1 | 0.00 | 39.9 MB | 325.4 KB |
| Claude Code | 157.7 MB | 146.3 MB | 1,131× more | — | — | 1 | 1.00 | 191.1 MB | 182.0 MB |
| Codex CLI | 111.2 MB | 81.4 MB | 628.98× more | — | — | 4 | 1.00 | 106.7 MB | 110.2 MB |
| OpenCode | 244.1 MB | 2.5 MB | 19.66× more | — | — | 1 | 0.00 | 552.8 MB | 31.2 MB |
| pi | 120.2 MB | 116.0 MB | 896.82× more | — | — | 1 | 1.00 | 149.0 MB | 148.1 MB |
| oh-my-pi (omp) | 425.3 MB | 159.4 MB | 1,232× more | — | — | 1 | 1.00 | 460.5 MB | 315.5 MB |
| oh-my-openagent (omo) | 57.3 MB | 45.1 MB | 348.53× more | — | — | 1 | 1.00 | 849.9 MB | 313.5 MB |

Threads/descriptors unavailable: jcode (server:memory did not report thread_count, fd_count)

**Post-turn tree RSS per session count**

| Tool | 1 session(s) | 5 session(s) | 10 session(s) |
|---|---|---|---|
| horon | 35.2 MB | 36.7 MB | 38.6 MB |
| jcode | 39.9 MB | 41.2 MB | 42.8 MB |
| Claude Code | 191.1 MB | 931.9 MB | 1.8 GB |
| Codex CLI | 106.7 MB | 563.8 MB | 1.1 GB |
| OpenCode | 552.8 MB | 587.2 MB | 826.9 MB |
| pi | 149.0 MB | 737.7 MB | 1.4 GB |
| oh-my-pi (omp) | 460.5 MB | 1.8 GB | 3.2 GB |
| oh-my-openagent (omo) | 849.9 MB | 2.7 GB | 3.6 GB |

**Live processes per session count** (idle / post-turn)

| Tool | 1 session(s) | 5 session(s) | 10 session(s) |
|---|---|---|---|
| horon | 1 / 1 | 1 / 1 | 1 / 1 |
| jcode | 1 / 1 | 1 / 1 | 1 / 1 |
| Claude Code | 1 / 1 | 5 / 5 | 10 / 10 |
| Codex CLI | 4 / 1 | 8 / 8 | 13 / 13 |
| OpenCode | 1 / 1 | 1 / 1 | 1 / 1 |
| pi | 1 / 1 | 5 / 5 | 10 / 10 |
| oh-my-pi (omp) | 1 / 1 | 5 / 5 | 10 / 10 |
| oh-my-openagent (omo) | 1 / 4 | 5 / 20 | 10 / 40 |

#### 기동

```text
Benchmarking: /Users/runner/work/agent/agent/bench-bin/horon-agent
============================================================

--help (binary load):
  Min:    9.36 ms
  Max:    17.40 ms
  Mean:   13.07 ms
  Median: 12.68 ms
  Stdev:  3.23 ms

--version:
  Min:    8.81 ms
  Max:    17.42 ms
  Mean:   12.47 ms
  Median: 11.04 ms
  Stdev:  3.45 ms

Measuring isolated server startup (10 runs)...

Server ready (isolated socket connectable):
  Min:    26.85 ms
  Max:    98.24 ms
  Mean:   71.58 ms
  Median: 90.42 ms
  Stdev:  30.23 ms

Server startup stages (one extra run with HORON_AGENT_STARTUP_PROFILE=1, not timed):
  stage             at ms  delta ms   share
  panic_hook          0.0       0.0    0.0%
  logging_init        0.6       0.6    7.3%
  nofile_limit        0.9       0.3    3.7%
  perm_harden         1.0       0.1    0.8%
  args_parse          1.0       0.0    0.0%
  config_loaded       8.0       7.0   81.7%
  server_new          8.3       0.3    3.3%
  socket_bound        8.5       0.3    3.1%

Measuring harness bridge readiness (10 runs)...

Bridge stage: api socket connectable:
  Min:    40.44 ms
  Max:    136.59 ms
  Mean:   74.64 ms
  Median: 65.65 ms
  Stdev:  29.03 ms

Bridge stage: hello_ok:
  Min:    41.53 ms
  Max:    138.17 ms
  Mean:   76.33 ms
  Median: 66.89 ms
  Stdev:  29.36 ms

Bridge stage: no-op reply:
  Min:    41.98 ms
  Max:    139.23 ms
  Mean:   77.17 ms
  Median: 67.72 ms
  Stdev:  29.55 ms

Bridge total (socket -> no-op):
  Min:    41.98 ms
  Max:    139.23 ms
  Mean:   77.17 ms
  Median: 67.72 ms
  Stdev:  29.55 ms

============================================================
Summary:
  Binary load median:      ~12.7 ms
  Server ready median:     ~90.4 ms
  Bridge total median:     ~67.7 ms
```

#### 세션 로딩 (COLD)

```text
binary   /Users/runner/work/agent/agent/bench-bin/examples/loading_probe (ece3bdffe20c)
stages   /Users/runner/work/agent/agent/bench-bin/examples/resume_stage_probe (7413b84e1562, 3 cold pass(es) per session fixture)
mode     cold
protocol COLD: every measured run gets its own freshly copied HOME; no derived cache from an earlier run exists, so each sample is a first load of the fixture.
fixtures /var/folders/h5/t85nd63d71x27_8gl5gw3l_w0000gn/T/horon-loading-bench-4epc8lh1
note     --warmup 3 is ignored in cold mode: every sample gets a fresh HOME
fixture events=0 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    0.039/0.028    p95    0.111/0.056    delta  -0.011 ms  control-no-change
  skill_request_overlay p50    0.358/0.269    p95    1.082/0.708    delta  -0.089 ms  control-no-change
  mcp_config_load   p50    0.538/0.394    p95    1.728/0.776    delta  -0.144 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.001/0.001    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.001/0.001    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.001/0.001    delta  -0.000 ms  control-no-change
  session_save      p50   33.095/20.857   p95   46.532/25.112   delta -12.237 ms  control-no-change
  total store_read_bytes p50 22946  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition not_applicable: this fixture has no session log
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 22922..22962 (7 distinct values)
  SPREAD   b session_save store_read_bytes 22930..22954 (6 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    4.506/4.851    p95   12.795/6.968    delta  +0.345 ms  control-no-change
  skill_request_overlay p50    5.433/5.759    p95   11.548/10.378   delta  +0.326 ms  control-no-change
  mcp_config_load   p50    0.277/0.286    p95    0.370/1.040    delta  +0.009 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.001/0.001    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.001    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.001    delta  -0.000 ms  control-no-change
  session_save      p50   25.916/29.804   p95   36.347/159.457  delta  +3.888 ms  control-no-change
  total store_read_bytes p50 22950  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition not_applicable: this fixture has no session log
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 22934..22958 (6 distinct values)
  SPREAD   b session_save store_read_bytes 22934..22958 (6 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.028/0.027    p95    0.049/0.096    delta  -0.001 ms  control-no-change
  skill_request_overlay p50    0.245/0.267    p95    0.433/0.737    delta  +0.022 ms  control-no-change
  mcp_config_load   p50    0.290/0.286    p95    0.534/0.785    delta  -0.004 ms  control-no-change
  attach_history    p50  315.735/324.417  p95  355.945/334.447  delta  +8.682 ms  control-no-change
  resume_ready      p50   85.461/79.431   p95  102.027/107.785  delta  -6.029 ms  control-no-change
  provider_dispatch p50    1.546/1.078    p95    2.449/2.071    delta  -0.468 ms  control-no-change
  session_save      p50   28.561/24.036   p95   38.861/39.673   delta  -4.525 ms  control-no-change
  total store_read_bytes p50 5311300  full_source_hashes p50 2  skill_dir_reads p50 1
  decomposition 3/3 cold pass(es); anchor attach_history p50 315.735 ms (load 269.070 + history 44.891)
    selection          p50     0.131 ms  min     0.088  max     0.632  ratio 0.0% of anchor
    checkpoint_recent  p50   211.321 ms  min   204.021  max   218.393  ratio 66.9% of anchor
    log_read           p50     0.086 ms  min     0.074  max     0.178  ratio 0.0% of anchor
    full_scan          p50    28.863 ms  min    27.306  max    38.983  ratio 9.1% of anchor
    blob_read          p50     0.007 ms  min     0.006  max     0.017  ratio 0.0% of anchor
    graph_replay       p50    19.007 ms  min    15.273  max    21.088  ratio 6.0% of anchor
    metadata_json      p50     0.015 ms  min     0.014  max     0.029  ratio 0.0% of anchor
    messages_json      p50     0.842 ms  min     0.786  max     0.847  ratio 0.3% of anchor
    messages_decode    p50     0.817 ms  min     0.608  max     1.391  ratio 0.3% of anchor
    source_hash        p50     2.195 ms  min     2.125  max     3.017  ratio 0.7% of anchor
    history_rescan     p50    51.595 ms  min    46.403  max    52.760  ratio 16.3% of anchor
    page_index         p50    54.382 ms  min    50.296  max    64.682  ratio 17.2% of anchor
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189203..4189207 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099141..1099145 (2 distinct values)
  SPREAD   a session_save store_read_bytes 22944..22964 (5 distinct values)
  SPREAD   b attach_history store_read_bytes 4189203..4189207 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099141..1099145 (3 distinct values)
  SPREAD   b session_save store_read_bytes 22942..22968 (8 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    4.545/4.981    p95    9.415/6.208    delta  +0.436 ms  control-no-change
  skill_request_overlay p50    5.597/5.840    p95    8.740/13.041   delta  +0.242 ms  control-no-change
  mcp_config_load   p50    0.281/0.309    p95    0.318/0.696    delta  +0.028 ms  control-no-change
  attach_history    p50  274.534/286.745  p95  316.482/355.402  delta +12.211 ms  control-no-change
  resume_ready      p50   71.050/71.740   p95   79.427/86.872   delta  +0.690 ms  control-no-change
  provider_dispatch p50    1.014/1.006    p95    2.075/1.932    delta  -0.009 ms  control-no-change
  session_save      p50   21.946/23.986   p95   35.442/82.423   delta  +2.040 ms  control-no-change
  total store_read_bytes p50 5311304  full_source_hashes p50 2  skill_dir_reads p50 1
  decomposition 3/3 cold pass(es); anchor attach_history p50 274.534 ms (load 234.760 + history 43.869)
    selection          p50     0.075 ms  min     0.073  max     0.090  ratio 0.0% of anchor
    checkpoint_recent  p50   194.912 ms  min   183.106  max   197.702  ratio 71.0% of anchor
    log_read           p50     0.085 ms  min     0.068  max     0.098  ratio 0.0% of anchor
    full_scan          p50    25.019 ms  min    21.544  max    25.730  ratio 9.1% of anchor
    blob_read          p50     0.007 ms  min     0.005  max     0.013  ratio 0.0% of anchor
    graph_replay       p50    15.025 ms  min    14.207  max    16.701  ratio 5.5% of anchor
    metadata_json      p50     0.022 ms  min     0.016  max     0.025  ratio 0.0% of anchor
    messages_json      p50     0.759 ms  min     0.718  max     0.828  ratio 0.3% of anchor
    messages_decode    p50     0.618 ms  min     0.565  max     0.630  ratio 0.2% of anchor
    source_hash        p50     2.124 ms  min     2.103  max     2.374  ratio 0.8% of anchor
    history_rescan     p50    41.889 ms  min    37.351  max    43.820  ratio 15.3% of anchor
    page_index         p50    51.584 ms  min    48.630  max    52.295  ratio 18.8% of anchor
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189201..4189205 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099140..1099144 (2 distinct values)
  SPREAD   a session_save store_read_bytes 22947..22967 (6 distinct values)
  SPREAD   b attach_history store_read_bytes 4189203..4189205 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099142..1099144 (3 distinct values)
  SPREAD   b session_save store_read_bytes 22941..22963 (6 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.029/0.026    p95    0.033/0.028    delta  -0.003 ms  control-no-change
  skill_request_overlay p50    0.222/0.227    p95    0.244/0.246    delta  +0.005 ms  control-no-change
  mcp_config_load   p50    0.258/0.249    p95    0.369/0.358    delta  -0.009 ms  control-no-change
  attach_history    p50 3140.965/3260.469 p95 3481.234/3524.349 delta +119.504 ms  control-no-change
  resume_ready      p50  792.326/825.762  p95  905.519/940.200  delta +33.436 ms  control-no-change
  provider_dispatch p50   13.061/10.807   p95   16.705/16.443   delta  -2.253 ms  control-no-change
  session_save      p50   24.738/25.687   p95   46.329/55.446   delta  +0.949 ms  control-no-change
  total store_read_bytes p50 53417515  full_source_hashes p50 2  skill_dir_reads p50 1
  decomposition 3/3 cold pass(es); anchor attach_history p50 3140.965 ms (load 2642.304 + history 480.437)
    selection          p50     0.087 ms  min     0.086  max     0.546  ratio 0.0% of anchor
    checkpoint_recent  p50  2054.589 ms  min  1987.807  max  2355.783  ratio 65.4% of anchor
    log_read           p50     1.039 ms  min     0.782  max     1.167  ratio 0.0% of anchor
    full_scan          p50   299.411 ms  min   279.908  max   352.943  ratio 9.5% of anchor
    blob_read          p50     0.082 ms  min     0.068  max     0.136  ratio 0.0% of anchor
    graph_replay       p50   211.452 ms  min   189.317  max   237.250  ratio 6.7% of anchor
    metadata_json      p50     0.024 ms  min     0.024  max     0.053  ratio 0.0% of anchor
    messages_json      p50    11.204 ms  min     8.454  max    11.624  ratio 0.4% of anchor
    messages_decode    p50    11.748 ms  min    11.051  max    20.206  ratio 0.4% of anchor
    source_hash        p50    25.318 ms  min    23.465  max    27.902  ratio 0.8% of anchor
    history_rescan     p50   570.385 ms  min   554.592  max   635.762  ratio 18.2% of anchor
    page_index         p50   458.190 ms  min   419.329  max   589.688  ratio 14.6% of anchor
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184303..42184305 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210254..11210255 (2 distinct values)
  SPREAD   a session_save store_read_bytes 22940..22971 (7 distinct values)
  SPREAD   b attach_history store_read_bytes 42184301..42184305 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210251..11210255 (2 distinct values)
  SPREAD   b session_save store_read_bytes 22949..22963 (4 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    4.191/4.032    p95    6.181/5.174    delta  -0.159 ms  control-no-change
  skill_request_overlay p50    5.351/5.343    p95    7.488/5.691    delta  -0.008 ms  control-no-change
  mcp_config_load   p50    0.282/0.266    p95    0.448/0.310    delta  -0.017 ms  control-no-change
  attach_history    p50 2838.033/2894.433 p95 3600.274/3036.894 delta +56.400 ms  control-no-change
  resume_ready      p50  766.929/721.119  p95  865.557/912.863  delta -45.810 ms  control-no-change
  provider_dispatch p50   10.693/11.408   p95   44.621/17.934   delta  +0.715 ms  control-no-change
  session_save      p50   31.793/26.525   p95  117.811/108.052  delta  -5.269 ms  control-no-change
  total store_read_bytes p50 53417521  full_source_hashes p50 2  skill_dir_reads p50 1
  decomposition 3/3 cold pass(es); anchor attach_history p50 2838.033 ms (load 2430.363 + history 390.278)
    selection          p50     0.071 ms  min     0.066  max     0.075  ratio 0.0% of anchor
    checkpoint_recent  p50  2148.668 ms  min  1873.750  max  2463.739  ratio 75.7% of anchor
    log_read           p50     0.861 ms  min     0.861  max     0.921  ratio 0.0% of anchor
    full_scan          p50   244.958 ms  min   196.264  max   313.816  ratio 8.6% of anchor
    blob_read          p50     0.065 ms  min     0.060  max     0.081  ratio 0.0% of anchor
    graph_replay       p50   203.351 ms  min   165.349  max   221.001  ratio 7.2% of anchor
    metadata_json      p50     0.025 ms  min     0.011  max     0.030  ratio 0.0% of anchor
    messages_json      p50     8.504 ms  min     7.533  max     9.582  ratio 0.3% of anchor
    messages_decode    p50    11.187 ms  min    10.059  max    11.651  ratio 0.4% of anchor
    source_hash        p50    20.621 ms  min    18.048  max    21.726  ratio 0.7% of anchor
    history_rescan     p50   418.677 ms  min   387.858  max   434.193  ratio 14.8% of anchor
    page_index         p50   486.247 ms  min   432.226  max   589.658  ratio 17.1% of anchor
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 22937..22967 (5 distinct values)
  SPREAD   b attach_history store_read_bytes 42184301..42184305 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210251..11210255 (3 distinct values)
  SPREAD   b session_save store_read_bytes 22954..22971 (6 distinct values)
report   bench-results/loading-cold.json
mode cold, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### 세션 로딩 (WARM)

```text
binary   /Users/runner/work/agent/agent/bench-bin/examples/loading_probe (ece3bdffe20c)
stages   /Users/runner/work/agent/agent/bench-bin/examples/resume_stage_probe (7413b84e1562, 0 cold pass(es) per session fixture)
mode     warm
protocol WARM: one HOME per side, --warmup discarded runs per side, then measured runs alternate between the two sides on those same HOMEs.
fixtures /var/folders/q2/p1t6rgxj5fj32g1dc7dmd49r0000gn/T/horon-loading-bench-6rwz9gvq
fixture events=0 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    0.038/0.036    p95    0.103/0.142    delta  -0.002 ms  control-no-change
  skill_request_overlay p50    0.270/0.258    p95    0.608/0.697    delta  -0.012 ms  control-no-change
  mcp_config_load   p50    0.312/0.286    p95    0.643/0.627    delta  -0.026 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.001/0.001    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  session_save      p50   22.876/23.834   p95   47.546/28.872   delta  +0.959 ms  control-no-change
  total store_read_bytes p50 23667  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition not_applicable: this fixture has no session log
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [22927, 22947, 23657, 23665, 23669, 24367, 24377, 24379, 24383]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [22939, 22951, 23657, 23661, 23669, 24371, 24373, 24379, 24383]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 22927..24383 (9 distinct values)
  SPREAD   b session_save store_read_bytes 22939..24383 (9 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    5.094/6.400    p95   15.529/11.931   delta  +1.306 ms  control-no-change
  skill_request_overlay p50    6.967/7.137    p95    8.119/12.072   delta  +0.170 ms  control-no-change
  mcp_config_load   p50    0.339/0.335    p95    0.607/0.591    delta  -0.004 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  session_save      p50   19.566/21.399   p95   24.377/27.559   delta  +1.833 ms  control-no-change
  total store_read_bytes p50 23665  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition not_applicable: this fixture has no session log
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [22943, 22955, 23645, 23661, 23669, 24367, 24371, 24375]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [22937, 22945, 22959, 23655, 23659, 23663, 24365, 24367, 24369]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 22943..24375 (8 distinct values)
  SPREAD   b session_save store_read_bytes 22937..24369 (9 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.034/0.037    p95    0.107/0.213    delta  +0.002 ms  control-no-change
  skill_request_overlay p50    0.237/0.264    p95    0.424/0.607    delta  +0.026 ms  control-no-change
  mcp_config_load   p50    0.292/0.277    p95    0.890/0.694    delta  -0.014 ms  control-no-change
  attach_history    p50  123.590/128.520  p95  143.597/136.867  delta  +4.930 ms  control-no-change
  resume_ready      p50   88.057/80.841   p95   96.829/105.502  delta  -7.216 ms  control-no-change
  provider_dispatch p50    1.311/1.288    p95    2.211/1.586    delta  -0.023 ms  control-no-change
  session_save      p50   18.106/18.809   p95   86.005/29.531   delta  +0.703 ms  control-no-change
  total store_read_bytes p50 2841250  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition skipped: --stage-probe-samples is 0
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23665, 23669, 24379, 24383, 25089, 25099, 25813, 25823, 26523, 26529]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23669, 24385, 24387, 25097, 25105, 25811, 25823, 26517, 26529]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23665..26529 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23669..26529 (9 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    4.417/4.179    p95    5.391/4.742    delta  -0.238 ms  control-no-change
  skill_request_overlay p50    6.014/5.524    p95    6.852/6.203    delta  -0.489 ms  control-no-change
  mcp_config_load   p50    0.287/0.276    p95    0.343/0.331    delta  -0.011 ms  control-no-change
  attach_history    p50  108.453/103.623  p95  123.307/138.901  delta  -4.830 ms  control-no-change
  resume_ready      p50   66.558/62.822   p95   93.372/79.137   delta  -3.736 ms  control-no-change
  provider_dispatch p50    1.079/1.031    p95    1.430/1.233    delta  -0.048 ms  control-no-change
  session_save      p50   18.407/15.657   p95   54.698/17.398   delta  -2.750 ms  control-no-change
  total store_read_bytes p50 2841253  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition skipped: --stage-probe-samples is 0
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23656, 23674, 24376, 24378, 25090, 25100, 25802, 25808, 26516, 26528]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23670, 23678, 24380, 25096, 25098, 25808, 25810, 26518, 26532]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23656..26528 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23670..26532 (9 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.027/0.041    p95    0.064/0.078    delta  +0.015 ms  control-drift
  skill_request_overlay p50    0.239/0.243    p95    0.402/0.655    delta  +0.004 ms  control-no-change
  mcp_config_load   p50    0.261/0.280    p95    0.448/0.712    delta  +0.019 ms  control-no-change
  attach_history    p50 1531.769/1413.318 p95 1803.802/1760.951 delta -118.451 ms  control-no-change
  resume_ready      p50  859.896/854.551  p95  996.061/1012.186 delta  -5.346 ms  control-no-change
  provider_dispatch p50   12.572/12.362   p95   16.016/15.280   delta  -0.210 ms  control-no-change
  session_save      p50   26.562/23.047   p95   41.812/31.159   delta  -3.514 ms  control-no-change
  total store_read_bytes p50 28640280  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition skipped: --stage-probe-samples is 0
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23660, 23676, 24386, 24394, 25106, 25108, 25812, 25822, 26530, 26532]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23663, 23673, 24383, 24387, 25091, 25105, 25809, 25827, 26519, 26523]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23660..26532 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23663..26523 (10 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    4.263/4.697    p95    6.544/5.489    delta  +0.434 ms  control-no-change
  skill_request_overlay p50    5.242/5.899    p95    6.938/7.690    delta  +0.657 ms  control-no-change
  mcp_config_load   p50    0.267/0.301    p95    0.349/0.355    delta  +0.034 ms  control-no-change
  attach_history    p50 1253.859/1275.010 p95 1438.820/1664.357 delta +21.150 ms  control-no-change
  resume_ready      p50  815.024/793.989  p95  916.334/910.047  delta -21.035 ms  control-no-change
  provider_dispatch p50   11.749/11.626   p95   17.555/13.859   delta  -0.123 ms  control-no-change
  session_save      p50   24.693/28.575   p95   63.307/68.057   delta  +3.882 ms  control-no-change
  total store_read_bytes p50 28640278  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition skipped: --stage-probe-samples is 0
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23673, 24383, 24391, 25093, 25113, 25815, 25819, 26525, 26533]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23669, 23693, 24379, 24395, 25093, 25105, 25815, 25819, 26537, 26541]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23673..26533 (9 distinct values)
  SPREAD   b session_save store_read_bytes 23669..26541 (10 distinct values)
report   bench-results/loading-warm.json
mode warm, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### Tool latency

| Tool case | median | p95 | min | max | samples |
|---|---|---|---|---|---|
| read (tiny file) | 0.3 ms | 0.4 ms | 0.2 ms | 0.4 ms | 5 |
| read (1000 lines) | 0.9 ms | 1.2 ms | 0.7 ms | 1.2 ms | 5 |
| read (10 lines @ offset) | 0.6 ms | 0.7 ms | 0.5 ms | 0.7 ms | 5 |
| read (repo Cargo.toml) | 0.4 ms | 0.6 ms | 0.4 ms | 0.6 ms | 5 |
| write (small) | 2.2 ms | 2.7 ms | 1.8 ms | 2.7 ms | 5 |
| write (10KB) | 2.7 ms | 2.9 ms | 2.5 ms | 2.9 ms | 5 |
| edit (simple replace) | 2.5 ms | 4.3 ms | 1.9 ms | 4.3 ms | 5 |
| ls (scripts dir) | 1.1 ms | 1.2 ms | 1.0 ms | 1.2 ms | 5 |
| agentgrep (grep in tree) | 2.7 ms | 2.8 ms | 2.4 ms | 2.8 ms | 5 |
| agentgrep (find **/*.rs) | 1.3 ms | 1.5 ms | 1.3 ms | 1.5 ms | 5 |
| bash (true) | 4.3 ms | 5.0 ms | 4.1 ms | 5.0 ms | 5 |
| bash (echo hello) | 3.9 ms | 4.0 ms | 3.8 ms | 4.0 ms | 5 |
| bash (ls fixture tree) | 6.5 ms | 7.4 ms | 5.8 ms | 7.4 ms | 5 |
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
| 1 | 75.4 ms | 75.4 ms | 75.4 ms | 75.5 ms | 0 |
| 10 | 305.6 ms | 307.2 ms | 307.2 ms | 307.7 ms | 0 |
| 40 | 765.1 ms | 771.6 ms | 773.3 ms | 776.3 ms | 0 |

#### Compile loop

| Checkpoint | Touched | median | min | max | samples |
|---|---|---|---|---|---|
| warm_noop | — | 1.57 s | 1.57 s | 1.57 s | 1 |
| leaf_touch | src/main.rs | 502.1 ms | 502.1 ms | 502.1 ms | 1 |
| core_touch | crates/app-core/src/lib.rs | 42.47 s | 42.47 s | 42.47 s | 1 |
| build_meta_touch | crates/build-meta/src/lib.rs | 62.41 s | 62.41 s | 62.41 s | 1 |

### Windows

#### Coding agent comparison (headless, mock model)

Host: windows/AMD64, 4 CPUs, Python 3.12.10. Baseline: **horon**. Samples per one-shot, skill and MCP cell: 5.

Skipped: syscalls (strace not available)

| Tool | Version | `--version` (median) | Install size |
|---|---|---|---|
| horon | horon-agent v0.1.0-dev (729624b) | 10.5 ms | 91.7 MB |
| jcode | jcode v0.99.1 (7206efd0c) | 14.1 ms | 128.4 MB |
| Claude Code | 2.1.317 (Claude Code) | 35.6 ms | 645.2 MB |
| Codex CLI | codex-cli 0.166.0 | 84.9 ms | 479.4 MB |
| OpenCode | 1.19.5 | 458.0 ms | 605.3 MB |
| pi | 1.5.0 | 243.2 ms | 109.0 MB |
| oh-my-pi (omp) | omp/18.18.0 | 671.8 ms | 292.4 MB |
| oh-my-openagent (omo) | omo 5.1.64 (engine: senpi 2026.11.5-1) | 175.2 ms | 514.9 MB |

**One-shot headless prompt** (includes startup, one `ls` tool call, final answer)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Tool round-trip | request bytes | Note |
|---|---|---|---|---|---|---|---|---|
| horon | 141.8 ms | 137.3 ms–222.8 ms | baseline | 36.1 MB | baseline | yes | 3,664 / 2,368 (1 tools) | — |
| jcode | 128.4 ms | 116.5 ms–182.4 ms | 1.10× less | 26.9 MB | 1.34× less | yes | 3,105 / 1,312 (1 tools) | — |
| Claude Code | 1.09 s | 1.07 s–1.86 s | 7.69× more | 273.5 MB | 7.58× more | yes | 50,067 / 33,795 (14 tools) | — |
| Codex CLI | 1.51 s | 1.29 s–1.68 s | 10.68× more | 264.7 MB | 7.34× more | yes | 41,028 / 18,342 (9 tools) | — |
| OpenCode | 4.46 s | 4.25 s–7.17 s | 31.44× more | 819.3 MB | 22.71× more | yes | 2,659 / 0 (0 tools) | — |
| pi | 433.2 ms | 419.4 ms–455.4 ms | 3.05× more | 124.7 MB | 3.46× more | yes | 5,522 / 2,841 (4 tools) | — |
| oh-my-pi (omp) | 2.32 s | 2.19 s–4.50 s | 16.35× more | 415.6 MB | 11.52× more | yes | 29,889 / 18,533 (11 tools) | — |
| oh-my-openagent (omo) | 17.79 s | 14.26 s–18.24 s | 125.44× more | 561.8 MB | 15.57× more | yes | 29,467 / 2,424 (2 tools) | — |

`request bytes` is the first model request of the first run as the mock server received it: JSON body bytes / tool catalog bytes (tools offered).

**Core-profile one-shot** (the one-shot turn without `--tools`, so the request carries the default core surface: the eager tools, `toolbox` and the listed-tool index)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | Note | request bytes |
|---|---|---|---|---|---|---|---|---|---|
| horon | 154.8 ms | 149.4 ms–233.5 ms | baseline | 35.0 MB | baseline | — | yes | — | 11,033 / 9,079 (8 tools) |
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
| horon | 220.0 ms | 158.0 ms–259.6 ms | baseline | 24.7 MB | baseline | skill-uri | yes | — |
| jcode | 172.6 ms | 169.6 ms–175.2 ms | 1.27× less | 23.6 MB | 1.05× less | skill-tool | yes | — |
| Claude Code | 789.4 ms | 776.5 ms–841.8 ms | 3.59× more | 237.2 MB | 9.60× more | skill-tool | yes | — |
| Codex CLI | 1.68 s | 1.51 s–1.93 s | 7.63× more | 262.4 MB | 10.62× more | file-read | yes | — |
| OpenCode | 7.04 s | 6.56 s–7.64 s | 31.99× more | 712.3 MB | 28.83× more | skill-tool | yes | — |
| pi | 747.9 ms | 577.2 ms–850.2 ms | 3.40× more | 97.7 MB | 3.95× more | file-read | yes | — |
| oh-my-pi (omp) | 3.19 s | 2.71 s–3.83 s | 14.48× more | 420.0 MB | 17.00× more | file-read | yes | — |
| oh-my-openagent (omo) | 18.78 s | 17.03 s–22.00 s | 85.35× more | 550.1 MB | 22.27× more | file-read | yes | — |

**MCP tool call** (process start to final answer, including starting the fixture stdio MCP server and one `bench_echo` call)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | MCP ready | Note |
|---|---|---|---|---|---|---|---|---|---|
| horon | 211.0 ms | 190.1 ms–252.9 ms | baseline | 37.3 MB | baseline | mcp-call | yes | 128.8 ms | — |
| jcode | 318.3 ms | 281.8 ms–439.1 ms | 1.51× more | 36.4 MB | 1.03× less | mcp-action | yes | 214.9 ms | — |
| Claude Code | 827.2 ms | 818.5 ms–1.30 s | 3.92× more | 279.5 MB | 7.48× more | mcp-tool | yes | 714.5 ms | — |
| Codex CLI | 772.0 ms | 696.9 ms–1.53 s | 3.66× more | 177.5 MB | 4.75× more | mcp-tool | yes | 573.1 ms | — |
| OpenCode | 6.90 s | 6.69 s–7.65 s | 32.71× more | 754.4 MB | 20.20× more | mcp-tool | yes | 5.46 s | — |
| pi | 681.6 ms | 623.6 ms–785.8 ms | 3.23× more | 118.2 MB | 3.17× more | mcp-tool | yes | 533.2 ms | — |
| oh-my-pi (omp) | 3.12 s | 2.81 s–3.38 s | 14.80× more | 388.1 MB | 10.39× more | mcp-route | yes | 2.63 s | — |
| oh-my-openagent (omo) | — | — | — | — | — | mcp-tool | no | — | failed: the fixture sentinel never came back |

**Session memory** (idle after 3 turn(s) per session; sizes are process-tree sums, RSS; PSS also recorded on Linux). "Each additional" is the least-squares slope across the measured session counts; 0 B means growth stayed below RSS sampling noise. Threads and descriptors come from the daemon's own `server:memory` after the turns. Processes are counted in the same tree walk as the RSS sample (PSS shares it on Linux), so the count and the bytes describe one observation; a PID shared by two session roots counts once, a root that already exited is counted as neither bytes nor process, and a cell that could not be read is —, never 0.

| Tool | First session | Each additional | vs horon (additional) | Threads/session | FDs/session | Processes (idle) | Processes/session | First (post-turn) | Each additional (post-turn) |
|---|---|---|---|---|---|---|---|---|---|
| horon | 22.4 MB | 122.8 KB | baseline | — | — | 1 | 0.00 | 30.2 MB | 416.1 KB |
| jcode | 22.0 MB | 63.8 KB | 1.93× less | — | — | 1 | 0.00 | 27.0 MB | 349.7 KB |
| Claude Code | 153.8 MB | 166.7 MB | 1,390× more | — | — | 2 | 1.97 | 187.9 MB | 190.4 MB |
| Codex CLI | 125.2 MB | 96.4 MB | 803.37× more | — | — | 6 | 3.00 | 169.0 MB | 116.3 MB |
| OpenCode | 234.5 MB | 1.5 MB | 12.22× more | — | — | 2 | 0.00 | 434.2 MB | 6.0 MB |
| pi | 90.7 MB | 90.6 MB | 755.71× more | — | — | 2 | 2.00 | 101.0 MB | 101.2 MB |
| oh-my-pi (omp) | 322.7 MB | 107.5 MB | 896.19× more | — | — | 2 | 2.00 | 352.2 MB | 337.5 MB |
| oh-my-openagent (omo) | 149.1 MB | 132.6 MB | 1,105× more | — | — | 4 | 4.00 | 413.1 MB | 402.3 MB |

Threads/descriptors unavailable: horon (server:memory did not report thread_count, fd_count), jcode (server:memory did not report thread_count, fd_count)

**Post-turn tree RSS per session count**

| Tool | 1 session(s) | 5 session(s) | 10 session(s) |
|---|---|---|---|
| horon | 30.2 MB | 31.8 MB | 33.9 MB |
| jcode | 27.0 MB | 28.6 MB | 30.1 MB |
| Claude Code | 187.9 MB | 945.8 MB | 1.9 GB |
| Codex CLI | 169.0 MB | 604.8 MB | 1.2 GB |
| OpenCode | 434.2 MB | 456.5 MB | 487.9 MB |
| pi | 101.0 MB | 505.1 MB | 1011.3 MB |
| oh-my-pi (omp) | 352.2 MB | 1.7 GB | 3.3 GB |
| oh-my-openagent (omo) | 413.1 MB | 2.1 GB | 3.9 GB |

**Live processes per session count** (idle / post-turn)

| Tool | 1 session(s) | 5 session(s) | 10 session(s) |
|---|---|---|---|
| horon | 1 / 1 | 1 / 1 | 1 / 1 |
| jcode | 1 / 1 | 1 / 1 | 1 / 1 |
| Claude Code | 2 / 2 | 14 / 10 | 20 / 20 |
| Codex CLI | 6 / 6 | 18 / 15 | 33 / 30 |
| OpenCode | 2 / 2 | 2 / 2 | 2 / 2 |
| pi | 2 / 2 | 10 / 10 | 20 / 20 |
| oh-my-pi (omp) | 2 / 2 | 10 / 10 | 20 / 20 |
| oh-my-openagent (omo) | 4 / 6 | 20 / 30 | 40 / 66 |

#### 기동

```text
Benchmarking: D:/a/agent/agent/bench-bin/horon-agent.exe
============================================================

--help (binary load):
  Min:    10.20 ms
  Max:    11.58 ms
  Mean:   10.55 ms
  Median: 10.42 ms
  Stdev:  0.39 ms

--version:
  Min:    10.04 ms
  Max:    10.49 ms
  Mean:   10.24 ms
  Median: 10.21 ms
  Stdev:  0.13 ms

Measuring isolated server startup (10 runs)...

Server ready (isolated socket connectable):
  Min:    21.77 ms
  Max:    27.10 ms
  Mean:   22.43 ms
  Median: 21.86 ms
  Stdev:  1.65 ms

Server startup stages (one extra run with HORON_AGENT_STARTUP_PROFILE=1, not timed):
  stage             at ms  delta ms   share
  panic_hook          0.0       0.0    0.1%
  logging_init        1.4       1.4   23.3%
  nofile_limit        1.9       0.5    8.5%
  perm_harden         4.0       2.1   35.7%
  args_parse          4.0       0.0    0.0%
  config_loaded       4.8       0.8   13.6%
  server_new          5.5       0.8   13.2%
  socket_bound        5.9       0.3    5.7%

Measuring harness bridge readiness (10 runs)...
  skipped: api-bridge is Unix-only, so Windows has no harness bridge to time

Bridge stage: api socket connectable: No successful runs

Bridge stage: hello_ok: No successful runs

Bridge stage: no-op reply: No successful runs

Bridge total (socket -> no-op): No successful runs

============================================================
Summary:
  Binary load median:      ~10.4 ms
  Server ready median:     ~21.9 ms
  Bridge total median:     n/a
```

#### 세션 로딩 (COLD)

```text
binary   D:\a\agent\agent\bench-bin\examples\loading_probe.exe (c33c8d060007)
stages   D:\a\agent\agent\bench-bin\examples\resume_stage_probe.exe (667814629aa4, 3 cold pass(es) per session fixture)
mode     cold
protocol COLD: every measured run gets its own freshly copied HOME; no derived cache from an earlier run exists, so each sample is a first load of the fixture.
fixtures C:\Users\RUNNER~1\AppData\Local\Temp\horon-loading-bench-vsa0x026
note     --warmup 3 is ignored in cold mode: every sample gets a fresh HOME
fixture events=0 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    0.102/0.103    p95    0.158/0.171    delta  +0.001 ms  control-no-change
  skill_request_overlay p50    0.772/0.792    p95    0.814/0.888    delta  +0.020 ms  control-no-change
  mcp_config_load   p50    0.899/0.945    p95    0.971/1.031    delta  +0.046 ms  control-no-change
  attach_history    p50    0.002/0.002    p95    0.002/0.002    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  session_save      p50   49.034/50.020   p95  119.682/74.984   delta  +0.985 ms  control-no-change
  total store_read_bytes p50 22944  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition not_applicable: this fixture has no session log
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 22886..22972 (8 distinct values)
  SPREAD   b session_save store_read_bytes 22904..22960 (7 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50   14.490/14.558   p95   14.967/20.333   delta  +0.068 ms  control-no-change
  skill_request_overlay p50   37.923/37.998   p95   38.888/40.611   delta  +0.075 ms  control-no-change
  mcp_config_load   p50    1.015/0.995    p95    1.059/1.084    delta  -0.019 ms  control-no-change
  attach_history    p50    0.002/0.002    p95    0.003/0.002    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.001    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.001    delta  +0.000 ms  control-no-change
  session_save      p50   63.301/57.808   p95   70.374/66.415   delta  -5.493 ms  control-no-change
  total store_read_bytes p50 22940  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition not_applicable: this fixture has no session log
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 22898..22960 (8 distinct values)
  SPREAD   b session_save store_read_bytes 22896..22960 (7 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.102/0.101    p95    0.139/0.109    delta  -0.001 ms  control-no-change
  skill_request_overlay p50    0.784/0.785    p95    0.826/0.902    delta  +0.001 ms  control-no-change
  mcp_config_load   p50    0.931/0.957    p95    0.986/0.988    delta  +0.025 ms  control-no-change
  attach_history    p50  377.241/376.320  p95  395.002/382.650  delta  -0.922 ms  control-no-change
  resume_ready      p50  149.076/148.542  p95  157.551/157.943  delta  -0.533 ms  control-no-change
  provider_dispatch p50    1.515/1.518    p95    1.971/1.606    delta  +0.003 ms  control-no-change
  session_save      p50   45.254/46.860   p95   60.976/62.460   delta  +1.605 ms  control-no-change
  total store_read_bytes p50 5311215  full_source_hashes p50 2  skill_dir_reads p50 1
  decomposition 3/3 cold pass(es); anchor attach_history p50 377.241 ms (load 328.732 + history 49.127)
    selection          p50     0.137 ms  min     0.128  max     0.152  ratio 0.0% of anchor
    checkpoint_recent  p50   270.823 ms  min   269.668  max   279.310  ratio 71.8% of anchor
    log_read           p50     0.161 ms  min     0.152  max     0.186  ratio 0.0% of anchor
    full_scan          p50    25.753 ms  min    25.552  max    26.236  ratio 6.8% of anchor
    blob_read          p50     0.008 ms  min     0.007  max     0.008  ratio 0.0% of anchor
    graph_replay       p50    20.456 ms  min    20.177  max    21.284  ratio 5.4% of anchor
    metadata_json      p50     0.011 ms  min     0.011  max     0.022  ratio 0.0% of anchor
    messages_json      p50     1.524 ms  min     1.421  max     1.553  ratio 0.4% of anchor
    messages_decode    p50     1.102 ms  min     1.015  max     1.118  ratio 0.3% of anchor
    source_hash        p50     0.533 ms  min     0.519  max     0.551  ratio 0.1% of anchor
    history_rescan     p50    48.404 ms  min    47.352  max    49.350  ratio 12.8% of anchor
    page_index         p50    71.957 ms  min    71.789  max    73.182  ratio 19.1% of anchor
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189151..4189155 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099102..1099106 (3 distinct values)
  SPREAD   a session_save store_read_bytes 22936..22979 (9 distinct values)
  SPREAD   b attach_history store_read_bytes 4189151..4189155 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099102..1099106 (2 distinct values)
  SPREAD   b session_save store_read_bytes 22928..22980 (9 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50   15.276/15.416   p95   15.798/16.606   delta  +0.140 ms  control-no-change
  skill_request_overlay p50   37.501/38.105   p95   39.209/51.262   delta  +0.603 ms  control-no-change
  mcp_config_load   p50    0.991/0.991    p95    1.069/1.101    delta  -0.000 ms  control-no-change
  attach_history    p50  375.563/370.786  p95 3025.507/377.918  delta  -4.777 ms  control-no-change
  resume_ready      p50  149.070/148.556  p95  162.309/162.697  delta  -0.515 ms  control-no-change
  provider_dispatch p50    1.479/1.474    p95    1.520/1.511    delta  -0.005 ms  control-no-change
  session_save      p50   52.301/53.641   p95 3854.875/83.230   delta  +1.340 ms  control-no-change
  total store_read_bytes p50 5311219  full_source_hashes p50 2  skill_dir_reads p50 1
  decomposition 3/3 cold pass(es); anchor attach_history p50 375.563 ms (load 326.923 + history 48.334)
    selection          p50     0.133 ms  min     0.124  max     0.168  ratio 0.0% of anchor
    checkpoint_recent  p50   265.369 ms  min   265.108  max   287.570  ratio 70.7% of anchor
    log_read           p50     0.168 ms  min     0.156  max     0.173  ratio 0.0% of anchor
    full_scan          p50    25.581 ms  min    25.517  max    25.686  ratio 6.8% of anchor
    blob_read          p50     0.005 ms  min     0.005  max     0.005  ratio 0.0% of anchor
    graph_replay       p50    19.724 ms  min    19.178  max    19.892  ratio 5.3% of anchor
    metadata_json      p50     0.011 ms  min     0.011  max     0.012  ratio 0.0% of anchor
    messages_json      p50     1.419 ms  min     1.370  max     1.522  ratio 0.4% of anchor
    messages_decode    p50     1.078 ms  min     0.895  max     1.144  ratio 0.3% of anchor
    source_hash        p50     0.512 ms  min     0.508  max     0.528  ratio 0.1% of anchor
    history_rescan     p50    45.607 ms  min    45.382  max    47.083  ratio 12.1% of anchor
    page_index         p50    77.375 ms  min    66.259  max    81.271  ratio 20.6% of anchor
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 22916..22972 (9 distinct values)
  SPREAD   b attach_history store_read_bytes 4189151..4189155 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099102..1099106 (2 distinct values)
  SPREAD   b session_save store_read_bytes 22924..22980 (8 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.100/0.100    p95    0.119/0.119    delta  +0.000 ms  control-no-change
  skill_request_overlay p50    0.790/0.769    p95    0.832/0.870    delta  -0.021 ms  control-no-change
  mcp_config_load   p50    0.926/0.971    p95    1.032/1.017    delta  +0.044 ms  control-no-change
  attach_history    p50 4196.758/4165.868 p95 4310.474/4342.372 delta -30.890 ms  control-no-change
  resume_ready      p50 1556.757/1557.014 p95 1568.007/1579.818 delta  +0.256 ms  control-no-change
  provider_dispatch p50   17.735/17.468   p95   18.394/19.262   delta  -0.267 ms  control-no-change
  session_save      p50   45.243/43.448   p95 2046.481/2171.729 delta  -1.795 ms  control-no-change
  total store_read_bytes p50 53417432  full_source_hashes p50 2  skill_dir_reads p50 1
  decomposition 3/3 cold pass(es); anchor attach_history p50 4196.758 ms (load 3631.680 + history 564.661)
    selection          p50     0.154 ms  min     0.142  max     0.164  ratio 0.0% of anchor
    checkpoint_recent  p50  3032.321 ms  min  3031.154  max  3211.522  ratio 72.3% of anchor
    log_read           p50     1.775 ms  min     1.761  max     1.842  ratio 0.0% of anchor
    full_scan          p50   262.055 ms  min   260.708  max   265.362  ratio 6.2% of anchor
    blob_read          p50     0.072 ms  min     0.068  max     0.088  ratio 0.0% of anchor
    graph_replay       p50   258.610 ms  min   252.874  max   275.232  ratio 6.2% of anchor
    metadata_json      p50     0.014 ms  min     0.013  max     0.015  ratio 0.0% of anchor
    messages_json      p50    21.030 ms  min    19.963  max    21.332  ratio 0.5% of anchor
    messages_decode    p50    15.984 ms  min    15.201  max    19.536  ratio 0.4% of anchor
    source_hash        p50     4.651 ms  min     4.596  max     4.679  ratio 0.1% of anchor
    history_rescan     p50   554.983 ms  min   530.507  max   560.558  ratio 13.2% of anchor
    page_index         p50   589.393 ms  min   586.542  max   607.304  ratio 14.0% of anchor
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 22951..22987 (7 distinct values)
  SPREAD   b attach_history store_read_bytes 42184251..42184253 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210215..11210216 (2 distinct values)
  SPREAD   b session_save store_read_bytes 22935..22971 (10 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50   14.552/14.414   p95   17.580/15.993   delta  -0.138 ms  control-no-change
  skill_request_overlay p50   36.851/37.137   p95   37.460/41.117   delta  +0.286 ms  control-no-change
  mcp_config_load   p50    0.971/0.971    p95    1.047/1.129    delta  +0.000 ms  control-no-change
  attach_history    p50 4148.426/4143.281 p95 4556.991/4193.027 delta  -5.145 ms  control-no-change
  resume_ready      p50 1535.206/1542.061 p95 1588.641/1560.861 delta  +6.855 ms  control-no-change
  provider_dispatch p50   17.098/16.792   p95   18.076/17.098   delta  -0.306 ms  control-no-change
  session_save      p50   42.330/42.985   p95  802.399/69.102   delta  +0.655 ms  control-no-change
  total store_read_bytes p50 53417430  full_source_hashes p50 2  skill_dir_reads p50 1
  decomposition 3/3 cold pass(es); anchor attach_history p50 4148.426 ms (load 3600.192 + history 546.312)
    selection          p50     0.187 ms  min     0.147  max     0.230  ratio 0.0% of anchor
    checkpoint_recent  p50  3597.138 ms  min  3051.127  max  3771.382  ratio 86.7% of anchor
    log_read           p50     1.793 ms  min     1.624  max     2.563  ratio 0.0% of anchor
    full_scan          p50   277.345 ms  min   264.354  max   370.200  ratio 6.7% of anchor
    blob_read          p50     0.087 ms  min     0.070  max     0.125  ratio 0.0% of anchor
    graph_replay       p50   273.875 ms  min   255.653  max   327.856  ratio 6.6% of anchor
    metadata_json      p50     0.013 ms  min     0.012  max     0.015  ratio 0.0% of anchor
    messages_json      p50    21.397 ms  min    20.063  max    24.364  ratio 0.5% of anchor
    messages_decode    p50    18.324 ms  min    16.520  max    19.142  ratio 0.4% of anchor
    source_hash        p50     4.671 ms  min     4.598  max     4.701  ratio 0.1% of anchor
    history_rescan     p50   555.634 ms  min   543.015  max   599.582  ratio 13.4% of anchor
    page_index         p50   591.885 ms  min   583.238  max   641.393  ratio 14.3% of anchor
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184247..42184253 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210211..11210216 (2 distinct values)
  SPREAD   a session_save store_read_bytes 22935..22983 (8 distinct values)
  SPREAD   b attach_history store_read_bytes 42184249..42184253 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210212..11210216 (2 distinct values)
  SPREAD   b session_save store_read_bytes 22935..22979 (9 distinct values)
report   bench-results\loading-cold.json
mode cold, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### 세션 로딩 (WARM)

```text
binary   D:\a\agent\agent\bench-bin\examples\loading_probe.exe (c33c8d060007)
stages   D:\a\agent\agent\bench-bin\examples\resume_stage_probe.exe (667814629aa4, 0 cold pass(es) per session fixture)
mode     warm
protocol WARM: one HOME per side, --warmup discarded runs per side, then measured runs alternate between the two sides on those same HOMEs.
fixtures C:\Users\RUNNER~1\AppData\Local\Temp\horon-loading-bench-jn8fzkhh
fixture events=0 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    0.156/0.158    p95    0.197/0.206    delta  +0.002 ms  control-no-change
  skill_request_overlay p50    1.223/1.216    p95    1.271/1.286    delta  -0.007 ms  control-no-change
  mcp_config_load   p50    1.415/1.449    p95    1.474/1.816    delta  +0.033 ms  control-no-change
  attach_history    p50    0.002/0.002    p95    0.002/0.002    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.001    delta  +0.000 ms  control-no-change
  session_save      p50   51.493/55.669   p95   55.230/63.434   delta  +4.175 ms  control-no-change
  total store_read_bytes p50 23642  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition not_applicable: this fixture has no session log
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [22941, 22943, 22953, 23610, 23640, 23644, 24339, 24341, 24355]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [22897, 22930, 22957, 22959, 23632, 23639, 23650, 24331, 24340, 24343]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 22941..24355 (9 distinct values)
  SPREAD   b session_save store_read_bytes 22897..24343 (10 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50   24.439/24.235   p95   25.387/27.849   delta  -0.204 ms  control-no-change
  skill_request_overlay p50   55.114/54.474   p95   67.827/57.660   delta  -0.640 ms  control-no-change
  mcp_config_load   p50    1.478/1.437    p95    1.744/1.709    delta  -0.041 ms  control-no-change
  attach_history    p50    0.002/0.002    p95    0.009/0.009    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.001    delta  -0.000 ms  control-drift
  provider_dispatch p50    0.000/0.000    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  session_save      p50   71.926/62.901   p95   88.954/78.942   delta  -9.025 ms  control-no-change
  total store_read_bytes p50 23628  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition not_applicable: this fixture has no session log
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [22941, 22948, 22949, 22953, 23621, 23635, 23656, 24346, 24353, 24371]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [22933, 22941, 22945, 23636, 23642, 23660, 24347, 24351, 24353, 24356]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 22941..24371 (10 distinct values)
  SPREAD   b session_save store_read_bytes 22933..24356 (10 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.157/0.165    p95    0.206/0.209    delta  +0.009 ms  control-no-change
  skill_request_overlay p50    1.316/1.232    p95    1.645/2.561    delta  -0.084 ms  control-no-change
  mcp_config_load   p50    1.461/1.470    p95    1.733/2.653    delta  +0.009 ms  control-no-change
  attach_history    p50  326.702/304.837  p95  386.678/379.418  delta -21.865 ms  control-no-change
  resume_ready      p50  246.718/249.071  p95  311.758/302.662  delta  +2.353 ms  control-no-change
  provider_dispatch p50    1.864/1.816    p95    2.705/2.307    delta  -0.048 ms  control-no-change
  session_save      p50   94.562/125.676  p95  663.331/353.086  delta +31.114 ms  control-no-change
  total store_read_bytes p50 2841110  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition skipped: --stage-probe-samples is 0
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23627, 23632, 24335, 24360, 25016, 25048, 25745, 25767, 26420, 26430]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23656, 23678, 24351, 24353, 25022, 25027, 25739, 25760, 26433, 26434]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23627..26430 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23656..26434 (10 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50   24.479/24.794   p95   27.245/26.000   delta  +0.315 ms  control-no-change
  skill_request_overlay p50   56.530/58.440   p95   61.285/71.206   delta  +1.910 ms  control-no-change
  mcp_config_load   p50    1.478/1.473    p95    1.611/1.542    delta  -0.005 ms  control-no-change
  attach_history    p50  296.498/298.492  p95  305.853/303.731  delta  +1.994 ms  control-no-change
  resume_ready      p50  236.219/240.918  p95  260.005/248.454  delta  +4.699 ms  control-no-change
  provider_dispatch p50    1.840/1.864    p95    2.010/2.353    delta  +0.024 ms  control-no-change
  session_save      p50   53.433/53.916   p95   88.994/76.693   delta  +0.483 ms  control-no-change
  total store_read_bytes p50 2841128  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition skipped: --stage-probe-samples is 0
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23652, 23659, 24344, 24357, 25047, 25048, 25747, 25750, 26455, 26470]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23661, 23664, 24359, 24360, 25047, 25063, 25709, 25742, 26444, 26448]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23652..26470 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23661..26448 (10 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.152/0.150    p95    0.185/0.215    delta  -0.001 ms  control-no-change
  skill_request_overlay p50    1.237/1.230    p95    1.292/1.325    delta  -0.007 ms  control-no-change
  mcp_config_load   p50    1.423/1.403    p95    1.600/1.477    delta  -0.019 ms  control-no-change
  attach_history    p50 3058.603/3069.721 p95 3126.008/3097.379 delta +11.118 ms  control-no-change
  resume_ready      p50 2433.416/2436.148 p95 2460.364/2459.514 delta  +2.732 ms  control-no-change
  provider_dispatch p50   19.615/18.995   p95   21.542/20.863   delta  -0.620 ms  control-no-change
  session_save      p50   49.613/51.212   p95   56.769/53.820   delta  +1.599 ms  control-no-change
  total store_read_bytes p50 28640138  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition skipped: --stage-probe-samples is 0
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23621, 23678, 24354, 24381, 25032, 25065, 25760, 25763, 26467, 26470]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23670, 23681, 24335, 24356, 25018, 25047, 25761, 25774, 26433, 26457]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23621..26470 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23670..26457 (10 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50   23.865/24.106   p95   25.472/25.975   delta  +0.241 ms  control-no-change
  skill_request_overlay p50   57.285/58.171   p95   58.838/62.453   delta  +0.886 ms  control-no-change
  mcp_config_load   p50    1.529/1.545    p95    1.726/1.586    delta  +0.017 ms  control-no-change
  attach_history    p50 3106.651/3095.447 p95 3154.258/3149.570 delta -11.204 ms  control-no-change
  resume_ready      p50 2452.097/2451.724 p95 2519.935/2481.291 delta  -0.373 ms  control-no-change
  provider_dispatch p50   20.534/19.071   p95   21.744/21.245   delta  -1.462 ms  control-no-change
  session_save      p50   54.644/49.478   p95   92.944/62.428   delta  -5.166 ms  control-no-change
  total store_read_bytes p50 28640159  full_source_hashes p50 1  skill_dir_reads p50 1
  decomposition skipped: --stage-probe-samples is 0
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23652, 23670, 24342, 24365, 25053, 25071, 25699, 25743, 26418, 26442]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23650, 24321, 24349, 25065, 25084, 25764, 25767, 26466, 26467]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23652..26442 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23650..26467 (9 distinct values)
report   bench-results\loading-warm.json
mode warm, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### Tool latency

| Tool case | median | p95 | min | max | samples |
|---|---|---|---|---|---|
| read (tiny file) | 2.7 ms | 2.8 ms | 2.2 ms | 2.8 ms | 5 |
| read (1000 lines) | 2.7 ms | 2.8 ms | 2.2 ms | 2.8 ms | 5 |
| read (10 lines @ offset) | 2.7 ms | 2.7 ms | 2.2 ms | 2.7 ms | 5 |
| read (repo Cargo.toml) | 2.7 ms | 2.7 ms | 2.3 ms | 2.7 ms | 5 |
| write (small) | 10.5 ms | 15.6 ms | 10.0 ms | 15.6 ms | 5 |
| write (10KB) | 10.6 ms | 12.1 ms | 9.5 ms | 12.1 ms | 5 |
| edit (simple replace) | 12.5 ms | 12.5 ms | 10.4 ms | 12.5 ms | 5 |
| ls (scripts dir) | 2.7 ms | 2.7 ms | 2.2 ms | 2.7 ms | 5 |
| agentgrep (grep in tree) | 7.9 ms | 8.0 ms | 7.4 ms | 8.0 ms | 5 |
| agentgrep (find **/*.rs) | 5.3 ms | 7.3 ms | 5.3 ms | 7.3 ms | 5 |
| bash (true) | 10.5 ms | 10.5 ms | 10.4 ms | 10.5 ms | 5 |
| bash (echo hello) | 10.1 ms | 10.5 ms | 9.9 ms | 10.5 ms | 5 |
| bash (ls fixture tree) | 13.0 ms | 13.2 ms | 12.6 ms | 13.2 ms | 5 |
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
| 1 | 155.4 ms | 155.4 ms | 155.4 ms | 155.5 ms | 0 |
| 10 | 349.2 ms | 406.3 ms | 406.3 ms | 407.2 ms | 0 |
| 40 | 1.12 s | 1.75 s | 1.75 s | 1.75 s | 0 |

#### Compile loop

| Checkpoint | Touched | median | min | max | samples |
|---|---|---|---|---|---|
| warm_noop | — | 763.6 ms | 763.6 ms | 763.6 ms | 1 |
| leaf_touch | src\main.rs | 785.9 ms | 785.9 ms | 785.9 ms | 1 |
| core_touch | crates\app-core\src\lib.rs | 26.00 s | 26.00 s | 26.00 s | 1 |
| build_meta_touch | crates\build-meta\src\lib.rs | 37.37 s | 37.37 s | 37.37 s | 1 |

## TUI (`horon-tui`)

대화형 클라이언트는 별도 바이너리이고 따로 잽니다. 이 수치는 `tui` 묶음에서 나오며 위의 헤드리스 표와 비교하지 않습니다. `first_frame_ms` 와 `direct_first_frame_ms` 는 서로 다른 두 프로그램을 각각 띄운 값이고, 핸드오프는 그 둘의 차이입니다.

![TUI (`horon-tui`)](charts/tui-01-latency.ko.svg)

![TUI (`horon-tui`)](charts/tui-02-memory.ko.svg)

### Linux

#### TUI (`horon-tui`)

Measured version: the interactive client `horon-tui`. The headless `horon-agent` numbers are in the suites above and are never compared with these: the two binaries are timed against themselves.

| Metric | Median | Min–max | Samples |
| --- | --- | --- | --- |
| First frame (bare `horon-agent`) | 35.4 ms | 34.7 ms–149.9 ms | 5 |
| First frame (direct `horon-tui`) | 39.3 ms | 38.9 ms–40.8 ms | 5 |
| TTY handoff cost | -3.9 ms | -6.1 ms–110.7 ms | 5 |
| Key press to echo (p95) | 0.1 ms | 0.1 ms–1.2 ms | 50 |
| Idle RSS after 5 s | 6.0 MB | 5.9 MB–6.0 MB | 5 |
| RSS after one turn | 6.2 MB | 6.2 MB–6.2 MB | 5 |
| Frame draw time (p50) | 0.8 ms | 0.7 ms–0.8 ms | 5 |
| Frame draw time (p95) | 0.8 ms | 0.8 ms–0.8 ms | 5 |
| Install size (`horon-tui`) | 6.4 MB | — | 1 |

Handoff: median first-frame time of a bare `horon-agent` launch (no arguments, on a PTY) minus the median first-frame time of a direct `horon-tui` launch against an already-running daemon, both on the same machine and build. The bare path also starts the daemon, so the number covers that startup, the sibling lookup and the `exec`; it is the extra cost of the handoff over launching the client itself.

Render bench: 200 frames on a 120×40 terminal.

Scroll bench (a separate measurement, not the frame cells above): 120 one-row scroll steps and 20 no-input redraws over a 2,081-row transcript (72 items, 148.4 KB of tool output, expanded) on a 120×40 terminal. Every scroll step moved the expanded output.

| Scroll metric | Median | Min–max | Samples |
| --- | --- | --- | --- |
| Held frame draw (p50) | 0.3 ms | 0.3 ms–0.3 ms | 5 |
| Held frame draw (p95) | 0.4 ms | 0.4 ms–0.4 ms | 5 |
| Scroll key handling (p50) | 0.1 µs | 0.1 µs–0.2 µs | 5 |
| Scroll key handling (p95) | 0.2 µs | 0.2 µs–0.2 µs | 5 |
| First held frame after scrolling | 0.5 ms | 0.5 ms–0.5 ms | 5 |
| Key dispatch through completed draw (p50) | 0.3 ms | 0.3 ms–0.3 ms | 5 |
| Key dispatch through completed draw (p95) | 0.4 ms | 0.4 ms–0.4 ms | 5 |
| First key dispatch through completed draw | 0.5 ms | 0.5 ms–0.5 ms | 5 |
| Held frame redraw without input (p50) | 0.3 ms | 0.3 ms–0.4 ms | 5 |
| Held frame redraw without input (p95) | 0.4 ms | 0.4 ms–0.4 ms | 5 |

### macOS

#### TUI (`horon-tui`)

Measured version: the interactive client `horon-tui`. The headless `horon-agent` numbers are in the suites above and are never compared with these: the two binaries are timed against themselves.

| Metric | Median | Min–max | Samples |
| --- | --- | --- | --- |
| First frame (bare `horon-agent`) | 115.7 ms | 69.3 ms–524.0 ms | 5 |
| First frame (direct `horon-tui`) | 73.9 ms | 49.0 ms–104.6 ms | 5 |
| TTY handoff cost | 41.8 ms | 1.7 ms–426.4 ms | 5 |
| Key press to echo (p95) | 0.2 ms | 0.2 ms–1.6 ms | 50 |
| Idle RSS after 5 s | 9.4 MB | 9.3 MB–9.5 MB | 5 |
| RSS after one turn | 9.6 MB | 9.6 MB–9.8 MB | 5 |
| Frame draw time (p50) | 0.9 ms | 0.9 ms–0.9 ms | 5 |
| Frame draw time (p95) | 2.3 ms | 2.3 ms–2.4 ms | 5 |
| Install size (`horon-tui`) | 5.4 MB | — | 1 |

Handoff: median first-frame time of a bare `horon-agent` launch (no arguments, on a PTY) minus the median first-frame time of a direct `horon-tui` launch against an already-running daemon, both on the same machine and build. The bare path also starts the daemon, so the number covers that startup, the sibling lookup and the `exec`; it is the extra cost of the handoff over launching the client itself.

Render bench: 200 frames on a 120×40 terminal.

Scroll bench (a separate measurement, not the frame cells above): 120 one-row scroll steps and 20 no-input redraws over a 2,081-row transcript (72 items, 148.4 KB of tool output, expanded) on a 120×40 terminal. Every scroll step moved the expanded output.

| Scroll metric | Median | Min–max | Samples |
| --- | --- | --- | --- |
| Held frame draw (p50) | 0.4 ms | 0.4 ms–0.4 ms | 5 |
| Held frame draw (p95) | 1.0 ms | 1.0 ms–1.1 ms | 5 |
| Scroll key handling (p50) | 0.3 µs | 0.2 µs–0.5 µs | 5 |
| Scroll key handling (p95) | 2.8 µs | 2.5 µs–3.1 µs | 5 |
| First held frame after scrolling | 0.7 ms | 0.6 ms–2.0 ms | 5 |
| Key dispatch through completed draw (p50) | 0.4 ms | 0.4 ms–0.4 ms | 5 |
| Key dispatch through completed draw (p95) | 1.0 ms | 1.0 ms–1.1 ms | 5 |
| First key dispatch through completed draw | 0.7 ms | 0.6 ms–2.0 ms | 5 |
| Held frame redraw without input (p50) | 0.4 ms | 0.4 ms–0.4 ms | 5 |
| Held frame redraw without input (p95) | 0.8 ms | 0.4 ms–1.7 ms | 5 |

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
| Frame draw time (p95) | 1.5 ms | 1.5 ms–2.2 ms | 5 |
| Install size (`horon-tui`) | 5.4 MB | — | 1 |

Handoff: median first-frame time of a bare `horon-agent` launch (no arguments, on a PTY) minus the median first-frame time of a direct `horon-tui` launch against an already-running daemon, both on the same machine and build. The bare path also starts the daemon, so the number covers that startup, the sibling lookup and the `exec`; it is the extra cost of the handoff over launching the client itself.

Render bench: 200 frames on a 120×40 terminal.

Scroll bench (a separate measurement, not the frame cells above): 120 one-row scroll steps and 20 no-input redraws over a 2,081-row transcript (72 items, 148.4 KB of tool output, expanded) on a 120×40 terminal. Every scroll step moved the expanded output.

| Scroll metric | Median | Min–max | Samples |
| --- | --- | --- | --- |
| Held frame draw (p50) | 0.4 ms | 0.4 ms–0.4 ms | 5 |
| Held frame draw (p95) | 0.4 ms | 0.4 ms–0.4 ms | 5 |
| Scroll key handling (p50) | 0.1 µs | 0.1 µs–0.1 µs | 5 |
| Scroll key handling (p95) | 0.3 µs | 0.3 µs–0.3 µs | 5 |
| First held frame after scrolling | 0.6 ms | 0.6 ms–0.7 ms | 5 |
| Key dispatch through completed draw (p50) | 0.4 ms | 0.4 ms–0.4 ms | 5 |
| Key dispatch through completed draw (p95) | 0.4 ms | 0.4 ms–0.4 ms | 5 |
| First key dispatch through completed draw | 0.6 ms | 0.6 ms–0.7 ms | 5 |
| Held frame redraw without input (p50) | 0.4 ms | 0.4 ms–0.4 ms | 5 |
| Held frame redraw without input (p95) | 0.4 ms | 0.4 ms–0.7 ms | 5 |

Skipped: pty unavailable on Windows

<!-- bench-auto:end -->
