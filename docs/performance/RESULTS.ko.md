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
| 비활성 세션 축출 정책 | 채택 안 함 | 세션당 fd 0.999개, 스레드 0.998개, RSS 0.41 MB. 1개에서 1,000개까지 이 기울기가 꺾이지 않는다. debug job 수(`/debug/job_count`)는 다섯 인구 수 모두에서 0이고, 소크에서 세션 수는 0으로 돌아온다 |
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
- **200 사이클 소크는 20 사이클당 자원 1개의 누수를 판별한다.** 계획이 요구한 1,000 사이클은 이제 예약 워크플로 `Attach/Detach Soak`(`.github/workflows/soak.yml`)가 매주 실행하고 첫 실행이 깨끗했으며, `ci.yml` 의 200 사이클 게이트는 풀 리퀘스트용으로 남는다.
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
| 15. attach/detach 소크 | `[x]` | 계획 항목인 1,000 사이클이 이제 돈다. `.github/workflows/soak.yml` 이 매주·수동으로 실행하고, 첫 실행이 1,000 사이클을 7분 38초에 깨끗하게 마쳤다(`fd_retention=0`, `thread_retention=0`, `non_returning=0`, 실행 43600180217). 200 사이클 형은 풀 리퀘스트 게이트로 남는다 |
| 17. CI 구조 게이트와 리포트 템플릿 | `[~]` | 게이트는 실제로 돈다(경계 35개, `EXIT=0`, 위반 시 `EXIT=1`, 측정 누락 시 `EXIT=2`, 계약 테스트 40개). 계획의 두 조건이 남았다. 프레임 계수 카운터가 없고, "B0 에서 통과"는 구조적으로 불가능하다(`STRUCTURAL_PERF` 줄을 인쇄하는 코드가 B0 뒤에 들어왔다). 이제 게이트는 `frame` 구획에서 두 리더의 프레임 상한 자체를 단언하고, 프레임 개수는 여전히 읽지 않는다 |

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

<!-- bench-auto:start commit=f7a53f1ebbc9a5dd4d9461c0aef60877aef898b5 -->

## 헤드리스 (`horon-agent`)

이 구간은 `.github/workflows/benchmark-docs.yml` 이 자동으로 다시 씁니다. 손으로 고치지 않습니다.

- 커밋: `f7a53f1ebbc9a5dd4d9461c0aef60877aef898b5`
- 측정 시각: 2026-11-15 13:29 UTC
- 실행: <https://github.com/horon-ai/agent/actions/runs/43714998312> (원본 JSON 은 OS별 `benchmark-<os>-<run_id>` 아티팩트)

OS마다 다른 GitHub 호스티드 러너에서 따로 잽니다. 기계가 다르므로 OS끼리 절대값을 비교하지 않고, 같은 OS 안의 상대 비교(horon 대비 배수)와 커밋 사이의 추세만 읽습니다.

| OS | 러너 | 바이너리 | 상태 |
| --- | --- | --- | --- |
| Linux | `ubuntu-24.04` | `horon-agent v0.1.0-dev (f7a53f1)` | 성공 |
| macOS | `macos-latest` | `horon-agent v0.1.0-dev (f7a53f1)` | 성공 |
| Windows | `windows-latest` | `horon-agent v0.1.0-dev (f7a53f1)` | 성공 |

### Linux

#### Coding agent comparison (headless, mock model)

Host: linux/x86_64, 4 CPUs, Python 3.12.16. Baseline: **horon**. Samples per one-shot, skill and MCP cell: 5.

| Tool | Version | `--version` (median) | Install size |
|---|---|---|---|
| horon | horon-agent v0.1.0-dev (f7a53f1) | 2.9 ms | 115.6 MB |
| jcode | jcode v0.99.1 (7206efd0c) | 7.5 ms | 163.4 MB |
| Claude Code | 2.1.317 (Claude Code) | 6.2 ms | 632.2 MB |
| Codex CLI | codex-cli 0.166.0 | 30.6 ms | 435.0 MB |
| OpenCode | 1.19.5 | 341.6 ms | 613.4 MB |
| pi | 1.5.0 | 168.4 ms | 108.9 MB |
| oh-my-pi (omp) | omp/18.18.0 | 557.0 ms | 413.1 MB |
| oh-my-openagent (omo) | omo 5.1.64 (engine: senpi 2026.11.5-1) | 84.1 ms | 520.6 MB |

**One-shot headless prompt** (includes startup, one `ls` tool call, final answer)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Tool round-trip | fsync calls | request bytes | Note |
|---|---|---|---|---|---|---|---|---|---|
| horon | 44.2 ms | 42.2 ms–184.5 ms | baseline | 47.8 MB | baseline | yes | 10 | 3,778 / 2,390 (1 tools) | — |
| jcode | 140.4 ms | 139.9 ms–141.1 ms | 3.18× more | 42.1 MB | 1.14× less | yes | 5 | 3,292 / 1,433 (1 tools) | — |
| Claude Code | 348.9 ms | 339.8 ms–361.2 ms | 7.89× more | 207.2 MB | 4.34× more | yes | — | 45,780 / 29,653 (12 tools) | — |
| Codex CLI | 364.0 ms | 357.0 ms–444.1 ms | 8.23× more | 198.6 MB | 4.15× more | yes | 6 | 39,432 / 17,271 (9 tools) | — |
| OpenCode | 1.96 s | 1.92 s–2.13 s | 44.29× more | 589.2 MB | 12.33× more | yes | 4 | 2,659 / 0 (0 tools) | — |
| pi | 277.9 ms | 271.3 ms–288.5 ms | 6.29× more | 113.4 MB | 2.37× more | yes | — | 5,502 / 2,841 (4 tools) | — |
| oh-my-pi (omp) | 1.39 s | 1.38 s–1.65 s | 31.50× more | 413.7 MB | 8.65× more | yes | 66 | 29,858 / 18,533 (11 tools) | — |
| oh-my-openagent (omo) | 4.29 s | 4.20 s–4.70 s | 96.93× more | 677.0 MB | 14.16× more | yes | 36 | 27,443 / 2,424 (2 tools) | — |

`request bytes` is the first model request of the first run as the mock server received it: JSON body bytes / tool catalog bytes (tools offered).

**Core-profile one-shot** (the one-shot turn without `--tools`, so the request carries the default core surface: the eager tools, `toolbox` and the listed-tool index)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | Note | request bytes |
|---|---|---|---|---|---|---|---|---|---|
| horon | 45.8 ms | 45.3 ms–46.2 ms | baseline | 40.4 MB | baseline | — | yes | — | 11,301 / 9,255 (8 tools) |
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
| horon | 40.5 ms | 39.8 ms–41.2 ms | baseline | 38.1 MB | baseline | skill-uri | yes | — |
| jcode | 47.4 ms | 46.7 ms–47.6 ms | 1.17× more | 41.0 MB | 1.08× more | skill-tool | yes | — |
| Claude Code | 331.2 ms | 322.4 ms–342.6 ms | 8.18× more | 203.2 MB | 5.33× more | skill-tool | yes | — |
| Codex CLI | 403.4 ms | 382.6 ms–453.6 ms | 9.96× more | 196.7 MB | 5.16× more | file-read | yes | — |
| OpenCode | 2.24 s | 2.19 s–2.27 s | 55.21× more | 616.3 MB | 16.17× more | skill-tool | yes | — |
| pi | 381.4 ms | 378.6 ms–384.9 ms | 9.42× more | 126.2 MB | 3.31× more | file-read | yes | — |
| oh-my-pi (omp) | 1.32 s | 1.30 s–1.32 s | 32.60× more | 333.9 MB | 8.76× more | file-read | yes | — |
| oh-my-openagent (omo) | 4.69 s | 4.65 s–4.70 s | 115.88× more | 663.4 MB | 17.40× more | file-read | yes | — |

**MCP tool call** (process start to final answer, including starting the fixture stdio MCP server and one `bench_echo` call)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | MCP ready | Note |
|---|---|---|---|---|---|---|---|---|---|
| horon | 61.0 ms | 58.4 ms–62.0 ms | baseline | 44.7 MB | baseline | mcp-call | yes | 49.9 ms | — |
| jcode | 161.4 ms | 160.7 ms–161.7 ms | 2.65× more | 54.1 MB | 1.21× more | mcp-action | yes | 139.6 ms | — |
| Claude Code | 423.4 ms | 410.9 ms–435.3 ms | 6.95× more | 226.6 MB | 5.07× more | mcp-tool | yes | 297.4 ms | — |
| Codex CLI | 347.7 ms | 323.2 ms–390.7 ms | 5.70× more | 193.4 MB | 4.33× more | mcp-tool | yes | 235.5 ms | — |
| OpenCode | 2.07 s | 2.06 s–2.10 s | 33.95× more | 579.0 MB | 12.95× more | mcp-tool | yes | 1.90 s | — |
| pi | 412.0 ms | 407.5 ms–416.8 ms | 6.76× more | 138.1 MB | 3.09× more | mcp-tool | yes | 363.7 ms | — |
| oh-my-pi (omp) | 1.31 s | 1.30 s–1.33 s | 21.57× more | 343.2 MB | 7.67× more | mcp-route | yes | 1.25 s | — |
| oh-my-openagent (omo) | — | — | — | — | — | mcp-tool | no | — | failed: the fixture sentinel never came back |

**Session memory** (idle after 3 turn(s) per session; sizes are process-tree sums, RSS; PSS also recorded on Linux). "Each additional" is the least-squares slope across the measured session counts; 0 B means growth stayed below RSS sampling noise. Threads and descriptors come from the daemon's own `server:memory` after the turns.

| Tool | First session | Each additional | vs horon (additional) | Threads/session | FDs/session | First (post-turn) | Each additional (post-turn) |
|---|---|---|---|---|---|---|---|
| horon | 37.3 MB | 200.5 KB | baseline | 0.80 | 1.00 | 44.8 MB | 560.6 KB |
| jcode | 42.7 MB | 97.8 KB | 2.05× less | 0.00 | — | 46.0 MB | 278.5 KB |
| Claude Code | 151.8 MB | 150.1 MB | 766.55× more | — | — | 187.7 MB | 181.1 MB |
| Codex CLI | 129.8 MB | 132.9 MB | 678.46× more | — | — | 160.7 MB | 164.7 MB |
| OpenCode | 242.5 MB | 612.5 KB | 3.05× more | — | — | 451.1 MB | 8.0 MB |
| pi | 108.8 MB | 108.6 MB | 554.35× more | — | — | 132.4 MB | 127.4 MB |
| oh-my-pi (omp) | 312.6 MB | 117.8 MB | 601.41× more | — | — | 371.1 MB | 352.9 MB |
| oh-my-openagent (omo) | 307.3 MB | 58.2 MB | 297.39× more | — | — | 590.4 MB | 586.8 MB |

Threads/descriptors unavailable: jcode (server:memory did not report fd_count)

**Post-turn tree RSS per session count**

| Tool | 1 session(s) | 5 session(s) | 10 session(s) |
|---|---|---|---|
| horon | 44.8 MB | 47.7 MB | 49.8 MB |
| jcode | 46.0 MB | 47.9 MB | 48.5 MB |
| Claude Code | 187.7 MB | 924.6 MB | 1.8 GB |
| Codex CLI | 160.7 MB | 812.8 MB | 1.6 GB |
| OpenCode | 451.1 MB | 526.2 MB | 526.5 MB |
| pi | 132.4 MB | 645.1 MB | 1.2 GB |
| oh-my-pi (omp) | 371.1 MB | 1.8 GB | 3.5 GB |
| oh-my-openagent (omo) | 590.4 MB | 2.9 GB | 5.7 GB |

#### 기동

```text
Benchmarking: /home/runner/work/agent/agent/bench-bin/horon-agent
============================================================

--help (binary load):
  Min:    3.88 ms
  Max:    4.19 ms
  Mean:   4.00 ms
  Median: 3.99 ms
  Stdev:  0.09 ms

--version:
  Min:    3.75 ms
  Max:    4.24 ms
  Mean:   3.87 ms
  Median: 3.85 ms
  Stdev:  0.14 ms

Measuring isolated server startup (10 runs)...

Server ready (isolated socket connectable):
  Min:    20.53 ms
  Max:    20.62 ms
  Mean:   20.57 ms
  Median: 20.57 ms
  Stdev:  0.03 ms

Server startup stages (one extra run with HORON_AGENT_STARTUP_PROFILE=1, not timed):
  stage             at ms  delta ms   share
  panic_hook          0.0       0.0    0.0%
  logging_init        0.2       0.2   15.8%
  nofile_limit        0.6       0.4   26.0%
  perm_harden         0.6       0.0    0.9%
  args_parse          0.6       0.0    0.0%
  config_loaded       0.8       0.2   13.9%
  server_new          1.3       0.5   35.6%
  socket_bound        1.4       0.1    7.8%

Measuring harness bridge readiness (10 runs)...

Bridge stage: api socket connectable:
  Min:    20.68 ms
  Max:    20.80 ms
  Mean:   20.76 ms
  Median: 20.76 ms
  Stdev:  0.03 ms

Bridge stage: hello_ok:
  Min:    21.34 ms
  Max:    21.89 ms
  Mean:   21.44 ms
  Median: 21.39 ms
  Stdev:  0.16 ms

Bridge stage: no-op reply:
  Min:    21.78 ms
  Max:    22.46 ms
  Mean:   21.91 ms
  Median: 21.87 ms
  Stdev:  0.20 ms

Bridge total (socket -> no-op):
  Min:    21.78 ms
  Max:    22.46 ms
  Mean:   21.91 ms
  Median: 21.87 ms
  Stdev:  0.20 ms

============================================================
Summary:
  Binary load median:      ~4.0 ms
  Server ready median:     ~20.6 ms
  Bridge total median:     ~21.9 ms
```

#### 세션 로딩 (COLD)

```text
binary   /home/runner/work/agent/agent/bench-bin/examples/loading_probe (d204df43dbd0)
mode     cold
protocol COLD: every measured run gets its own freshly copied HOME; no derived cache from an earlier run exists, so each sample is a first load of the fixture.
fixtures /tmp/horon-loading-bench-7b9ile2g
note     --warmup 3 is ignored in cold mode: every sample gets a fresh HOME
fixture events=0 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    0.011/0.011    p95    0.012/0.012    delta  -0.000 ms  control-no-change
  skill_request_overlay p50    0.087/0.087    p95    0.109/0.107    delta  +0.000 ms  control-no-change
  mcp_config_load   p50    0.114/0.114    p95    0.115/0.117    delta  +0.000 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.001    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.001    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  session_save      p50    6.137/7.259    p95   31.410/22.950   delta  +1.122 ms  control-drift
  total store_read_bytes p50 23078  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 23056..23084 (5 distinct values)
  SPREAD   b session_save store_read_bytes 23064..23080 (5 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    1.961/1.950    p95    2.017/2.426    delta  -0.010 ms  control-no-change
  skill_request_overlay p50    3.517/3.528    p95    3.573/4.157    delta  +0.011 ms  control-no-change
  mcp_config_load   p50    0.116/0.117    p95    0.122/0.169    delta  +0.001 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.001/0.000    delta  +0.000 ms  control-drift
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.001    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  session_save      p50   15.186/26.266   p95   31.444/74.404   delta +11.081 ms  control-drift
  total store_read_bytes p50 23072  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 23064..23084 (6 distinct values)
  SPREAD   b session_save store_read_bytes 23072..23088 (5 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.011/0.011    p95    0.012/0.013    delta  +0.000 ms  control-no-change
  skill_request_overlay p50    0.085/0.086    p95    0.098/0.103    delta  +0.001 ms  control-no-change
  mcp_config_load   p50    0.113/0.115    p95    0.115/0.131    delta  +0.001 ms  control-no-change
  attach_history    p50  179.424/186.260  p95  186.567/245.960  delta  +6.836 ms  control-no-change
  resume_ready      p50   34.799/34.855   p95   35.358/35.637   delta  +0.056 ms  control-no-change
  provider_dispatch p50    0.823/0.818    p95    0.848/0.836    delta  -0.005 ms  control-no-change
  session_save      p50    8.958/5.696    p95   23.059/68.603   delta  -3.262 ms  control-no-change
  total store_read_bytes p50 5311405  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189191..4189195 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099132..1099136 (3 distinct values)
  SPREAD   a session_save store_read_bytes 23066..23090 (7 distinct values)
  SPREAD   b attach_history store_read_bytes 4189191..4189195 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099132..1099136 (3 distinct values)
  SPREAD   b session_save store_read_bytes 23062..23086 (8 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    1.948/1.971    p95    2.093/2.056    delta  +0.023 ms  control-no-change
  skill_request_overlay p50    3.507/3.515    p95    3.653/3.696    delta  +0.008 ms  control-no-change
  mcp_config_load   p50    0.115/0.118    p95    0.121/0.121    delta  +0.003 ms  control-no-change
  attach_history    p50  178.585/179.497  p95  218.057/189.456  delta  +0.912 ms  control-no-change
  resume_ready      p50   34.531/34.533   p95   35.639/35.989   delta  +0.002 ms  control-no-change
  provider_dispatch p50    0.821/0.807    p95    0.864/0.854    delta  -0.014 ms  control-no-change
  session_save      p50    5.609/5.700    p95  148.017/59.941   delta  +0.090 ms  control-no-change
  total store_read_bytes p50 5311406  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189191..4189195 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099132..1099136 (2 distinct values)
  SPREAD   a session_save store_read_bytes 23064..23090 (8 distinct values)
  SPREAD   b attach_history store_read_bytes 4189193..4189195 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099135..1099136 (2 distinct values)
  SPREAD   b session_save store_read_bytes 23070..23094 (8 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.012/0.012    p95    0.012/0.013    delta  +0.000 ms  control-no-change
  skill_request_overlay p50    0.085/0.086    p95    0.091/0.088    delta  +0.001 ms  control-no-change
  mcp_config_load   p50    0.112/0.114    p95    0.116/0.122    delta  +0.002 ms  control-no-change
  attach_history    p50 2335.812/2318.679 p95 2388.838/2399.083 delta -17.133 ms  control-no-change
  resume_ready      p50  388.529/389.608  p95  398.475/398.996  delta  +1.079 ms  control-no-change
  provider_dispatch p50    9.872/9.824    p95   10.980/10.776   delta  -0.048 ms  control-no-change
  session_save      p50    6.151/6.038    p95   71.446/6.518    delta  -0.112 ms  control-no-change
  total store_read_bytes p50 53417617  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184289..42184293 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210242..11210246 (3 distinct values)
  SPREAD   a session_save store_read_bytes 23069..23089 (7 distinct values)
  SPREAD   b attach_history store_read_bytes 42184289..42184293 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210242..11210246 (3 distinct values)
  SPREAD   b session_save store_read_bytes 23069..23093 (6 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    1.968/1.986    p95    2.028/2.092    delta  +0.018 ms  control-no-change
  skill_request_overlay p50    3.473/3.514    p95    3.916/3.709    delta  +0.041 ms  control-no-change
  mcp_config_load   p50    0.117/0.119    p95    0.140/0.125    delta  +0.003 ms  control-no-change
  attach_history    p50 2330.769/2324.514 p95 2426.805/2400.118 delta  -6.255 ms  control-no-change
  resume_ready      p50  389.217/386.862  p95  414.361/398.748  delta  -2.355 ms  control-no-change
  provider_dispatch p50   10.051/10.078   p95   10.671/10.704   delta  +0.027 ms  control-no-change
  session_save      p50    6.107/6.112    p95    6.266/6.617    delta  +0.006 ms  control-no-change
  total store_read_bytes p50 53417622  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184291..42184293 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210245..11210246 (2 distinct values)
  SPREAD   a session_save store_read_bytes 23077..23093 (7 distinct values)
  SPREAD   b attach_history store_read_bytes 42184291..42184293 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210245..11210246 (2 distinct values)
  SPREAD   b session_save store_read_bytes 23073..23089 (9 distinct values)
report   bench-results/loading-cold.json
mode cold, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### 세션 로딩 (WARM)

```text
binary   /home/runner/work/agent/agent/bench-bin/examples/loading_probe (d204df43dbd0)
mode     warm
protocol WARM: one HOME per side, --warmup discarded runs per side, then measured runs alternate between the two sides on those same HOMEs.
fixtures /tmp/horon-loading-bench-bd9sbtdx
fixture events=0 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    0.012/0.012    p95    0.016/0.014    delta  -0.000 ms  control-no-change
  skill_request_overlay p50    0.092/0.092    p95    0.094/0.112    delta  +0.000 ms  control-no-change
  mcp_config_load   p50    0.124/0.124    p95    0.135/0.147    delta  -0.000 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  session_save      p50    6.581/6.512    p95   15.042/6.770    delta  -0.068 ms  control-no-change
  total store_read_bytes p50 23804  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23075, 23077, 23798, 23804, 24499, 24515, 24517, 24523]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23073, 23081, 23093, 23792, 23800, 24515, 24519, 24523]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23075..24523 (8 distinct values)
  SPREAD   b session_save store_read_bytes 23073..24523 (8 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    2.425/2.394    p95    2.447/2.489    delta  -0.031 ms  control-no-change
  skill_request_overlay p50    3.988/3.996    p95    4.109/4.053    delta  +0.008 ms  control-no-change
  mcp_config_load   p50    0.133/0.133    p95    0.144/0.140    delta  +0.000 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  session_save      p50   11.184/10.677   p95   23.367/11.714   delta  -0.507 ms  control-no-change
  total store_read_bytes p50 23808  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23057, 23065, 23081, 23792, 23804, 23812, 24515, 24519, 24523]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23069, 23077, 23079, 23083, 23784, 23792, 23796, 24515, 24523]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23057..24523 (9 distinct values)
  SPREAD   b session_save store_read_bytes 23069..24523 (9 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.012/0.012    p95    0.013/0.014    delta  -0.000 ms  control-no-change
  skill_request_overlay p50    0.091/0.091    p95    0.103/0.091    delta  -0.000 ms  control-no-change
  mcp_config_load   p50    0.122/0.122    p95    0.126/0.130    delta  +0.000 ms  control-no-change
  attach_history    p50   78.150/79.102   p95   81.100/80.116   delta  +0.952 ms  control-no-change
  resume_ready      p50   43.022/42.939   p95   43.750/43.752   delta  -0.084 ms  control-no-change
  provider_dispatch p50    1.070/1.075    p95    1.112/1.086    delta  +0.005 ms  control-no-change
  session_save      p50    6.804/6.824    p95   49.112/28.260   delta  +0.019 ms  control-no-change
  total store_read_bytes p50 2841369  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23793, 23799, 24512, 24526, 25227, 25231, 25243, 25942, 25948, 26665]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23781, 23801, 24512, 24516, 25223, 25231, 25958, 25966, 26677, 26685]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23793..26665 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23781..26685 (10 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    2.360/2.385    p95    2.456/2.441    delta  +0.025 ms  control-no-change
  skill_request_overlay p50    3.886/3.869    p95    3.901/3.958    delta  -0.018 ms  control-no-change
  mcp_config_load   p50    0.133/0.132    p95    0.142/0.142    delta  -0.001 ms  control-no-change
  attach_history    p50   80.472/80.430   p95   81.718/81.225   delta  -0.042 ms  control-no-change
  resume_ready      p50   44.207/43.881   p95   46.077/44.257   delta  -0.327 ms  control-no-change
  provider_dispatch p50    1.069/1.042    p95    1.094/1.084    delta  -0.026 ms  control-no-change
  session_save      p50    8.603/8.553    p95   15.285/16.059   delta  -0.051 ms  control-no-change
  total store_read_bytes p50 2841374  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23793, 23797, 24508, 24524, 25233, 25235, 25950, 25960, 26673, 26683]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23793, 23797, 24512, 24524, 25235, 25237, 25952, 25958, 26671, 26677]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23793..26683 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23793..26677 (10 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.013/0.013    p95    0.017/0.015    delta  +0.000 ms  control-no-change
  skill_request_overlay p50    0.094/0.096    p95    0.104/0.097    delta  +0.002 ms  control-no-change
  mcp_config_load   p50    0.127/0.128    p95    0.136/0.129    delta  +0.000 ms  control-no-change
  attach_history    p50  921.309/919.546  p95  929.746/942.751  delta  -1.764 ms  control-no-change
  resume_ready      p50  496.900/505.218  p95  515.314/528.214  delta  +8.318 ms  control-no-change
  provider_dispatch p50   12.589/12.273   p95   13.482/14.400   delta  -0.317 ms  control-no-change
  session_save      p50    6.771/6.881    p95   77.300/41.912   delta  +0.110 ms  control-no-change
  total store_read_bytes p50 28640394  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23796, 23802, 24511, 24513, 25236, 25238, 25947, 25953, 26670, 26688]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23804, 23812, 24523, 24527, 25226, 25234, 25957, 25969, 26680, 26684]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23796..26688 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23804..26684 (10 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    2.388/2.424    p95    2.510/2.667    delta  +0.036 ms  control-no-change
  skill_request_overlay p50    3.884/3.900    p95    3.968/4.055    delta  +0.016 ms  control-no-change
  mcp_config_load   p50    0.131/0.134    p95    0.136/0.144    delta  +0.003 ms  control-drift
  attach_history    p50  929.840/920.739  p95  936.083/935.284  delta  -9.101 ms  control-no-change
  resume_ready      p50  532.815/520.177  p95  548.800/543.525  delta -12.638 ms  control-no-change
  provider_dispatch p50   12.892/12.428   p95   13.686/12.821   delta  -0.465 ms  control-no-change
  session_save      p50   10.757/10.695   p95   94.025/158.924  delta  -0.062 ms  control-no-change
  total store_read_bytes p50 28640399  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23796, 23804, 24517, 24523, 25238, 25246, 25953, 25969, 26672]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23796, 24519, 24527, 25216, 25230, 25953, 25961, 26672, 26680]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23796..26672 (9 distinct values)
  SPREAD   b session_save store_read_bytes 23796..26680 (9 distinct values)
report   bench-results/loading-warm.json
mode warm, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### Tool latency

| Tool case | median | p95 | min | max | samples |
|---|---|---|---|---|---|
| read (tiny file) | 0.2 ms | 0.3 ms | 0.2 ms | 0.3 ms | 5 |
| read (1000 lines) | 0.3 ms | 0.3 ms | 0.3 ms | 0.3 ms | 5 |
| read (10 lines @ offset) | 0.3 ms | 0.3 ms | 0.3 ms | 0.3 ms | 5 |
| read (repo Cargo.toml) | 0.3 ms | 0.3 ms | 0.2 ms | 0.3 ms | 5 |
| write (small) | 1.3 ms | 143.1 ms | 1.2 ms | 143.1 ms | 5 |
| write (10KB) | 1.5 ms | 1.8 ms | 1.4 ms | 1.8 ms | 5 |
| edit (simple replace) | 1.3 ms | 1.3 ms | 1.2 ms | 1.3 ms | 5 |
| ls (scripts dir) | 0.7 ms | 0.7 ms | 0.6 ms | 0.7 ms | 5 |
| agentgrep (grep in tree) | 1.9 ms | 2.0 ms | 1.9 ms | 2.0 ms | 5 |
| agentgrep (find **/*.rs) | 1.3 ms | 1.7 ms | 1.2 ms | 1.7 ms | 5 |
| bash (true) | 1.8 ms | 1.9 ms | 1.7 ms | 1.9 ms | 5 |
| bash (echo hello) | 1.8 ms | 1.8 ms | 1.6 ms | 1.8 ms | 5 |
| bash (ls fixture tree) | 2.8 ms | 2.9 ms | 2.8 ms | 2.9 ms | 5 |
| invalid tool (dispatch only) | 0.1 ms | 0.1 ms | 0.1 ms | 0.1 ms | 5 |

#### Reload storm

1 resume(s) per level; each level resumes sessions no other level has opened, from a freshly started daemon.

| Concurrent resumes | history median | p95 | max | storm wall | failures |
|---|---|---|---|---|---|
| 1 | 46.4 ms | 46.4 ms | 46.4 ms | 46.5 ms | 0 |
| 10 | 156.2 ms | 195.7 ms | 195.7 ms | 198.7 ms | 0 |
| 40 | 687.8 ms | 748.8 ms | 792.9 ms | 794.4 ms | 0 |

#### Compile loop

| Checkpoint | Touched | median | min | max | samples |
|---|---|---|---|---|---|
| warm_noop | — | 249.8 ms | 249.8 ms | 249.8 ms | 1 |
| leaf_touch | src/main.rs | 381.8 ms | 381.8 ms | 381.8 ms | 1 |
| core_touch | crates/app-core/src/lib.rs | 18.50 s | 18.50 s | 18.50 s | 1 |
| build_meta_touch | crates/build-meta/src/lib.rs | 26.62 s | 26.62 s | 26.62 s | 1 |

### macOS

#### Coding agent comparison (headless, mock model)

Host: darwin/arm64, 3 CPUs, Python 3.12.10. Baseline: **horon**. Samples per one-shot, skill and MCP cell: 5.

Skipped: syscalls (strace not available)

| Tool | Version | `--version` (median) | Install size |
|---|---|---|---|
| horon | horon-agent v0.1.0-dev (f7a53f1) | 12.9 ms | 97.2 MB |
| jcode | jcode v0.99.1 (7206efd0c) | 8.7 ms | 136.3 MB |
| Claude Code | 2.1.317 (Claude Code) | 20.4 ms | 596.8 MB |
| Codex CLI | codex-cli 0.166.0 | 87.9 ms | 346.5 MB |
| OpenCode | 1.19.5 | 381.8 ms | 344.6 MB |
| pi | 1.5.0 | 224.8 ms | 108.9 MB |
| oh-my-pi (omp) | omp/18.18.0 | 835.1 ms | 273.5 MB |
| oh-my-openagent (omo) | omo 5.1.64 (engine: senpi 2026.11.5-1) | 142.2 ms | 502.3 MB |

**One-shot headless prompt** (includes startup, one `ls` tool call, final answer)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Tool round-trip | request bytes | Note |
|---|---|---|---|---|---|---|---|---|
| horon | 93.7 ms | 82.9 ms–368.7 ms | baseline | 31.1 MB | baseline | yes | 3,619 / 2,390 (1 tools) | — |
| jcode | 374.4 ms | 351.9 ms–579.0 ms | 4.00× more | 34.0 MB | 1.10× more | yes | 3,133 / 1,433 (1 tools) | — |
| Claude Code | 511.1 ms | 381.0 ms–858.4 ms | 5.46× more | 197.7 MB | 6.36× more | yes | 45,798 / 29,653 (12 tools) | — |
| Codex CLI | 473.4 ms | 363.2 ms–1.37 s | 5.05× more | 142.6 MB | 4.59× more | yes | 39,453 / 17,271 (9 tools) | — |
| OpenCode | 3.51 s | 3.27 s–6.18 s | 37.52× more | 776.8 MB | 25.00× more | yes | 2,659 / 0 (0 tools) | — |
| pi | 450.1 ms | 433.2 ms–488.3 ms | 4.81× more | 121.3 MB | 3.90× more | yes | 5,513 / 2,841 (4 tools) | — |
| oh-my-pi (omp) | 3.33 s | 2.75 s–5.90 s | 35.54× more | 470.2 MB | 15.13× more | yes | 29,853 / 18,533 (11 tools) | — |
| oh-my-openagent (omo) | 8.38 s | 6.45 s–10.63 s | 89.46× more | 866.0 MB | 27.86× more | yes | 27,493 / 2,424 (2 tools) | — |

`request bytes` is the first model request of the first run as the mock server received it: JSON body bytes / tool catalog bytes (tools offered).

**Core-profile one-shot** (the one-shot turn without `--tools`, so the request carries the default core surface: the eager tools, `toolbox` and the listed-tool index)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | Note | request bytes |
|---|---|---|---|---|---|---|---|---|---|
| horon | 84.3 ms | 71.1 ms–98.0 ms | baseline | 31.5 MB | baseline | — | yes | — | 11,142 / 9,255 (8 tools) |
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
| horon | 67.9 ms | 61.8 ms–84.6 ms | baseline | 24.5 MB | baseline | skill-uri | yes | — |
| jcode | 338.8 ms | 296.1 ms–582.8 ms | 4.99× more | 37.2 MB | 1.52× more | skill-tool | yes | — |
| Claude Code | 445.4 ms | 426.6 ms–513.7 ms | 6.56× more | 198.1 MB | 8.10× more | skill-tool | yes | — |
| Codex CLI | 441.8 ms | 412.2 ms–594.7 ms | 6.50× more | 143.2 MB | 5.86× more | file-read | yes | — |
| OpenCode | 4.73 s | 4.46 s–5.93 s | 69.57× more | 799.2 MB | 32.68× more | skill-tool | yes | — |
| pi | 585.5 ms | 531.8 ms–629.1 ms | 8.62× more | 134.5 MB | 5.50× more | file-read | yes | — |
| oh-my-pi (omp) | 2.19 s | 2.04 s–2.46 s | 32.22× more | 413.2 MB | 16.90× more | file-read | yes | — |
| oh-my-openagent (omo) | 8.33 s | 6.39 s–11.44 s | 122.69× more | 888.8 MB | 36.35× more | file-read | yes | — |

**MCP tool call** (process start to final answer, including starting the fixture stdio MCP server and one `bench_echo` call)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | MCP ready | Note |
|---|---|---|---|---|---|---|---|---|---|
| horon | 91.2 ms | 88.8 ms–142.2 ms | baseline | 41.8 MB | baseline | mcp-call | yes | 74.7 ms | — |
| jcode | 392.7 ms | 356.3 ms–440.2 ms | 4.30× more | 52.9 MB | 1.27× more | mcp-action | yes | 366.6 ms | — |
| Claude Code | 625.2 ms | 510.1 ms–646.2 ms | 6.85× more | 222.8 MB | 5.34× more | mcp-tool | yes | 471.4 ms | — |
| Codex CLI | 388.0 ms | 329.1 ms–484.8 ms | 4.25× more | 142.8 MB | 3.42× more | mcp-tool | yes | 252.8 ms | — |
| OpenCode | 3.84 s | 3.71 s–4.30 s | 42.09× more | 727.1 MB | 17.41× more | mcp-tool | yes | 3.27 s | — |
| pi | 657.8 ms | 616.2 ms–881.0 ms | 7.21× more | 138.2 MB | 3.31× more | mcp-tool | yes | 565.5 ms | — |
| oh-my-pi (omp) | 1.84 s | 1.55 s–2.04 s | 20.19× more | 412.5 MB | 9.88× more | mcp-route | yes | 1.53 s | — |
| oh-my-openagent (omo) | — | — | — | — | — | mcp-tool | no | — | failed: the fixture sentinel never came back |

**Session memory** (idle after 3 turn(s) per session; sizes are process-tree sums, RSS; PSS also recorded on Linux). "Each additional" is the least-squares slope across the measured session counts; 0 B means growth stayed below RSS sampling noise. Threads and descriptors come from the daemon's own `server:memory` after the turns.

| Tool | First session | Each additional | vs horon (additional) | Threads/session | FDs/session | First (post-turn) | Each additional (post-turn) |
|---|---|---|---|---|---|---|---|
| horon | 29.0 MB | 130.9 KB | baseline | 0.45 | 1.00 | 35.0 MB | 340.9 KB |
| jcode | 36.5 MB | 207.0 KB | 1.58× more | — | — | 40.1 MB | 344.9 KB |
| Claude Code | 159.1 MB | 148.8 MB | 1,164× more | — | — | 190.2 MB | 181.8 MB |
| Codex CLI | 113.7 MB | 82.0 MB | 641.25× more | — | — | 107.3 MB | 107.7 MB |
| OpenCode | 243.5 MB | 2.6 MB | 20.41× more | — | — | 558.4 MB | 12.8 MB |
| pi | 120.5 MB | 116.9 MB | 914.28× more | — | — | 149.0 MB | 148.2 MB |
| oh-my-pi (omp) | 341.2 MB | 163.5 MB | 1,279× more | — | — | 464.5 MB | 310.1 MB |
| oh-my-openagent (omo) | 56.7 MB | 45.2 MB | 353.45× more | — | — | 831.3 MB | 319.8 MB |

Threads/descriptors unavailable: jcode (server:memory did not report thread_count, fd_count)

**Post-turn tree RSS per session count**

| Tool | 1 session(s) | 5 session(s) | 10 session(s) |
|---|---|---|---|
| horon | 35.0 MB | 36.6 MB | 38.0 MB |
| jcode | 40.1 MB | 41.4 MB | 43.1 MB |
| Claude Code | 190.2 MB | 933.6 MB | 1.8 GB |
| Codex CLI | 107.3 MB | 536.3 MB | 1.1 GB |
| OpenCode | 558.4 MB | 622.9 MB | 674.4 MB |
| pi | 149.0 MB | 740.1 MB | 1.4 GB |
| oh-my-pi (omp) | 464.5 MB | 1.8 GB | 3.2 GB |
| oh-my-openagent (omo) | 831.3 MB | 2.9 GB | 3.7 GB |

#### 기동

```text
Benchmarking: /Users/runner/work/agent/agent/bench-bin/horon-agent
============================================================

--help (binary load):
  Min:    10.56 ms
  Max:    22.08 ms
  Mean:   13.68 ms
  Median: 12.89 ms
  Stdev:  3.16 ms

--version:
  Min:    11.13 ms
  Max:    22.84 ms
  Mean:   13.90 ms
  Median: 12.45 ms
  Stdev:  3.59 ms

Measuring isolated server startup (10 runs)...

Server ready (isolated socket connectable):
  Min:    27.10 ms
  Max:    165.00 ms
  Mean:   80.13 ms
  Median: 79.91 ms
  Stdev:  44.15 ms

Server startup stages (one extra run with HORON_AGENT_STARTUP_PROFILE=1, not timed):
  stage             at ms  delta ms   share
  panic_hook          0.0       0.0    0.2%
  logging_init        0.6       0.6   32.7%
  nofile_limit        1.1       0.5   24.6%
  perm_harden         1.1       0.0    2.5%
  args_parse          1.1       0.0    0.0%
  config_loaded       1.4       0.2   12.9%
  server_new          1.6       0.2   13.1%
  socket_bound        1.9       0.3   14.1%

Measuring harness bridge readiness (10 runs)...

Bridge stage: api socket connectable:
  Min:    40.09 ms
  Max:    160.66 ms
  Mean:   109.12 ms
  Median: 116.94 ms
  Stdev:  43.39 ms

Bridge stage: hello_ok:
  Min:    40.72 ms
  Max:    161.68 ms
  Mean:   110.04 ms
  Median: 117.65 ms
  Stdev:  43.65 ms

Bridge stage: no-op reply:
  Min:    41.26 ms
  Max:    162.13 ms
  Mean:   110.61 ms
  Median: 118.12 ms
  Stdev:  43.68 ms

Bridge total (socket -> no-op):
  Min:    41.26 ms
  Max:    162.13 ms
  Mean:   110.61 ms
  Median: 118.12 ms
  Stdev:  43.68 ms

============================================================
Summary:
  Binary load median:      ~12.9 ms
  Server ready median:     ~79.9 ms
  Bridge total median:     ~118.1 ms
```

#### 세션 로딩 (COLD)

```text
binary   /Users/runner/work/agent/agent/bench-bin/examples/loading_probe (f436a434a010)
mode     cold
protocol COLD: every measured run gets its own freshly copied HOME; no derived cache from an earlier run exists, so each sample is a first load of the fixture.
fixtures /var/folders/h5/t85nd63d71x27_8gl5gw3l_w0000gn/T/horon-loading-bench-vw269k4y
note     --warmup 3 is ignored in cold mode: every sample gets a fresh HOME
fixture events=0 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    0.028/0.026    p95    0.076/0.074    delta  -0.002 ms  control-no-change
  skill_request_overlay p50    0.326/0.250    p95    0.622/1.277    delta  -0.075 ms  control-no-change
  mcp_config_load   p50    0.324/0.313    p95    0.655/0.530    delta  -0.011 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.001/0.006    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.001/0.000    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.012/0.000    delta  -0.000 ms  control-no-change
  session_save      p50   19.971/21.265   p95   31.416/49.735   delta  +1.295 ms  control-no-change
  total store_read_bytes p50 22946  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 22942..22954 (4 distinct values)
  SPREAD   b session_save store_read_bytes 22934..22954 (6 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    4.977/6.733    p95    9.754/15.791   delta  +1.756 ms  control-no-change
  skill_request_overlay p50    7.888/10.328   p95   16.858/17.008   delta  +2.440 ms  control-no-change
  mcp_config_load   p50    0.333/0.298    p95    4.047/1.241    delta  -0.036 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.001/0.000    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.001/0.000    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.001/0.000    delta  -0.000 ms  control-no-change
  session_save      p50   26.871/32.985   p95   38.967/130.751  delta  +6.114 ms  control-no-change
  total store_read_bytes p50 22946  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 22942..22950 (3 distinct values)
  SPREAD   b session_save store_read_bytes 22938..22958 (5 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.029/0.026    p95    0.093/0.056    delta  -0.002 ms  control-no-change
  skill_request_overlay p50    0.228/0.260    p95    0.614/0.594    delta  +0.031 ms  control-no-change
  mcp_config_load   p50    0.293/0.272    p95    0.712/0.638    delta  -0.021 ms  control-no-change
  attach_history    p50  361.874/346.623  p95  455.593/463.027  delta -15.252 ms  control-no-change
  resume_ready      p50   83.339/68.942   p95   98.523/93.703   delta -14.397 ms  control-no-change
  provider_dispatch p50    1.368/1.092    p95    2.512/1.732    delta  -0.276 ms  control-no-change
  session_save      p50   21.690/22.239   p95   52.587/49.749   delta  +0.549 ms  control-no-change
  total store_read_bytes p50 5311304  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189201..4189207 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099142..1099145 (3 distinct values)
  SPREAD   a session_save store_read_bytes 22944..22960 (8 distinct values)
  SPREAD   b attach_history store_read_bytes 4189205..4189207 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099144..1099145 (2 distinct values)
  SPREAD   b session_save store_read_bytes 22946..22964 (6 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    5.883/4.578    p95    8.068/7.549    delta  -1.305 ms  control-no-change
  skill_request_overlay p50    7.216/6.087    p95   15.326/12.008   delta  -1.129 ms  control-no-change
  mcp_config_load   p50    0.285/0.277    p95    0.893/0.625    delta  -0.008 ms  control-no-change
  attach_history    p50  382.144/358.293  p95  443.192/410.265  delta -23.852 ms  control-no-change
  resume_ready      p50   74.600/82.474   p95  113.156/96.725   delta  +7.874 ms  control-no-change
  provider_dispatch p50    1.052/1.041    p95    2.195/1.295    delta  -0.011 ms  control-no-change
  session_save      p50   24.229/25.985   p95   33.604/44.697   delta  +1.757 ms  control-no-change
  total store_read_bytes p50 5311310  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189205..4189207 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099144..1099145 (2 distinct values)
  SPREAD   a session_save store_read_bytes 22946..22976 (8 distinct values)
  SPREAD   b attach_history store_read_bytes 4189203..4189207 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099141..1099145 (2 distinct values)
  SPREAD   b session_save store_read_bytes 22948..22964 (6 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.030/0.028    p95    0.036/0.073    delta  -0.002 ms  control-no-change
  skill_request_overlay p50    0.238/0.229    p95    0.282/0.687    delta  -0.009 ms  control-no-change
  mcp_config_load   p50    0.277/0.411    p95    0.399/0.952    delta  +0.134 ms  control-drift
  attach_history    p50 4723.265/4645.659 p95 5360.487/5233.260 delta -77.606 ms  control-no-change
  resume_ready      p50 1006.184/968.793  p95 1183.687/1039.243 delta -37.390 ms  control-no-change
  provider_dispatch p50   14.464/11.379   p95   21.905/16.165   delta  -3.085 ms  control-no-change
  session_save      p50   47.282/27.973   p95  112.339/29.766   delta -19.309 ms  control-no-change
  total store_read_bytes p50 53417517  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184301..42184305 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210251..11210255 (2 distinct values)
  SPREAD   a session_save store_read_bytes 22943..22971 (8 distinct values)
  SPREAD   b attach_history store_read_bytes 42184301..42184305 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210251..11210255 (3 distinct values)
  SPREAD   b session_save store_read_bytes 22951..22971 (7 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    4.519/5.413    p95    7.215/22.632   delta  +0.894 ms  control-no-change
  skill_request_overlay p50    6.213/7.952    p95   10.974/14.659   delta  +1.739 ms  control-no-change
  mcp_config_load   p50    0.281/0.311    p95    0.578/0.748    delta  +0.030 ms  control-no-change
  attach_history    p50 4548.510/4482.762 p95 5907.361/5348.775 delta -65.748 ms  control-no-change
  resume_ready      p50  946.558/889.013  p95 1200.717/1126.128 delta -57.546 ms  control-no-change
  provider_dispatch p50   14.226/15.704   p95   18.788/18.510   delta  +1.479 ms  control-no-change
  session_save      p50   31.534/28.966   p95   51.306/75.213   delta  -2.568 ms  control-no-change
  total store_read_bytes p50 53417519  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184301..42184305 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210253..11210255 (2 distinct values)
  SPREAD   a session_save store_read_bytes 22953..22971 (5 distinct values)
  SPREAD   b attach_history store_read_bytes 42184297..42184305 (5 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210247..11210255 (5 distinct values)
  SPREAD   b session_save store_read_bytes 22951..22967 (7 distinct values)
report   bench-results/loading-cold.json
mode cold, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### 세션 로딩 (WARM)

```text
binary   /Users/runner/work/agent/agent/bench-bin/examples/loading_probe (f436a434a010)
mode     warm
protocol WARM: one HOME per side, --warmup discarded runs per side, then measured runs alternate between the two sides on those same HOMEs.
fixtures /var/folders/h5/t85nd63d71x27_8gl5gw3l_w0000gn/T/horon-loading-bench-a32f1fmj
fixture events=0 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    0.024/0.022    p95    0.037/0.028    delta  -0.002 ms  control-no-change
  skill_request_overlay p50    0.218/0.210    p95    0.247/0.219    delta  -0.008 ms  control-no-change
  mcp_config_load   p50    0.245/0.233    p95    0.263/0.251    delta  -0.012 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  session_save      p50   11.059/10.761   p95   12.043/12.060   delta  -0.297 ms  control-no-change
  total store_read_bytes p50 23655  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [22927, 22939, 22943, 22959, 23653, 23657, 23673, 24359, 24375, 24379]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [22937, 22951, 22955, 23657, 23669, 23671, 24367, 24371, 24377]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 22927..24379 (10 distinct values)
  SPREAD   b session_save store_read_bytes 22937..24377 (9 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    3.364/3.480    p95    3.959/4.818    delta  +0.116 ms  control-no-change
  skill_request_overlay p50    4.379/4.389    p95    4.984/5.506    delta  +0.010 ms  control-no-change
  mcp_config_load   p50    0.229/0.231    p95    0.251/0.288    delta  +0.002 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.001/0.001    delta  -0.000 ms  control-drift
  session_save      p50   11.626/11.369   p95   12.812/16.761   delta  -0.258 ms  control-no-change
  total store_read_bytes p50 23662  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [22945, 22947, 23657, 23659, 23665, 24361, 24373, 24375, 24383]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [22945, 22947, 22951, 23659, 23671, 23681, 24369, 24375, 24385]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 22945..24383 (9 distinct values)
  SPREAD   b session_save store_read_bytes 22945..24385 (9 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.024/0.025    p95    0.029/0.027    delta  +0.001 ms  control-no-change
  skill_request_overlay p50    0.205/0.210    p95    0.275/0.248    delta  +0.004 ms  control-no-change
  mcp_config_load   p50    0.234/0.239    p95    0.305/0.271    delta  +0.005 ms  control-no-change
  attach_history    p50   88.308/95.209   p95  101.409/114.716  delta  +6.902 ms  control-no-change
  resume_ready      p50   61.370/59.404   p95   81.219/64.385   delta  -1.966 ms  control-no-change
  provider_dispatch p50    1.044/1.018    p95    1.141/1.235    delta  -0.026 ms  control-no-change
  session_save      p50   11.760/11.835   p95   21.064/23.123   delta  +0.075 ms  control-no-change
  total store_read_bytes p50 2841248  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23670, 23674, 24368, 24376, 25080, 25100, 25778, 25790, 26528]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23658, 23660, 24384, 24386, 25094, 25104, 25804, 25810, 26520, 26536]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23670..26528 (9 distinct values)
  SPREAD   b session_save store_read_bytes 23658..26536 (10 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    3.569/3.370    p95    4.735/3.814    delta  -0.199 ms  control-no-change
  skill_request_overlay p50    4.625/4.329    p95    5.477/4.856    delta  -0.296 ms  control-no-change
  mcp_config_load   p50    0.240/0.231    p95    0.266/0.258    delta  -0.010 ms  control-no-change
  attach_history    p50   88.266/86.708   p95   91.831/91.706   delta  -1.558 ms  control-no-change
  resume_ready      p50   59.347/59.276   p95   60.302/63.850   delta  -0.071 ms  control-no-change
  provider_dispatch p50    1.062/1.039    p95    1.083/2.049    delta  -0.024 ms  control-no-change
  session_save      p50   13.597/13.284   p95   16.210/20.968   delta  -0.313 ms  control-no-change
  total store_read_bytes p50 2841253  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23668, 23676, 24378, 24382, 25088, 25102, 25806, 25824, 26524, 26528]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23661, 23669, 24379, 24391, 25095, 25105, 25803, 25809, 26519, 26521]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23668..26528 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23661..26521 (10 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.026/0.025    p95    0.031/0.046    delta  -0.000 ms  control-no-change
  skill_request_overlay p50    0.208/0.227    p95    0.220/0.285    delta  +0.019 ms  control-no-change
  mcp_config_load   p50    0.239/0.250    p95    0.260/0.305    delta  +0.012 ms  control-no-change
  attach_history    p50 1083.293/989.526  p95 1485.675/1122.172 delta -93.766 ms  control-no-change
  resume_ready      p50  678.890/598.983  p95  903.966/881.845  delta -79.907 ms  control-no-change
  provider_dispatch p50    9.605/9.703    p95   17.382/13.231   delta  +0.098 ms  control-no-change
  session_save      p50   13.586/13.508   p95   23.344/83.296   delta  -0.078 ms  control-no-change
  total store_read_bytes p50 28640263  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23669, 23673, 24387, 24391, 25095, 25097, 25815, 25821, 26521, 26535]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23669, 23677, 24383, 24399, 25093, 25109, 25795, 25811, 26533, 26537]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23669..26535 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23669..26537 (10 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    4.270/4.378    p95   10.765/6.621    delta  +0.109 ms  control-no-change
  skill_request_overlay p50    5.819/5.784    p95   10.564/9.749    delta  -0.035 ms  control-no-change
  mcp_config_load   p50    0.295/0.288    p95    0.368/0.703    delta  -0.007 ms  control-no-change
  attach_history    p50 1258.967/1288.654 p95 1691.201/1710.623 delta +29.687 ms  control-no-change
  resume_ready      p50  778.115/822.441  p95  989.330/1064.124 delta +44.326 ms  control-no-change
  provider_dispatch p50   11.740/11.354   p95   16.086/12.903   delta  -0.386 ms  control-no-change
  session_save      p50   22.551/28.926   p95   48.665/68.621   delta  +6.375 ms  control-no-change
  total store_read_bytes p50 28640267  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23657, 23675, 24379, 24395, 25085, 25099, 25811, 25825, 26531, 26533]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23669, 23673, 24391, 24395, 25105, 25107, 25805, 25815, 26535, 26537]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23657..26533 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23669..26537 (10 distinct values)
report   bench-results/loading-warm.json
mode warm, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### Tool latency

| Tool case | median | p95 | min | max | samples |
|---|---|---|---|---|---|
| read (tiny file) | 0.2 ms | 0.5 ms | 0.2 ms | 0.5 ms | 5 |
| read (1000 lines) | 0.6 ms | 0.7 ms | 0.5 ms | 0.7 ms | 5 |
| read (10 lines @ offset) | 0.5 ms | 1.0 ms | 0.5 ms | 1.0 ms | 5 |
| read (repo Cargo.toml) | 0.3 ms | 0.4 ms | 0.3 ms | 0.4 ms | 5 |
| write (small) | 3.0 ms | 4.3 ms | 2.2 ms | 4.3 ms | 5 |
| write (10KB) | 2.9 ms | 5.1 ms | 2.8 ms | 5.1 ms | 5 |
| edit (simple replace) | 2.9 ms | 3.7 ms | 2.5 ms | 3.7 ms | 5 |
| ls (scripts dir) | 1.0 ms | 2.4 ms | 0.9 ms | 2.4 ms | 5 |
| agentgrep (grep in tree) | 5.2 ms | 6.0 ms | 3.9 ms | 6.0 ms | 5 |
| agentgrep (find **/*.rs) | 1.4 ms | 1.6 ms | 1.4 ms | 1.6 ms | 5 |
| bash (true) | 5.7 ms | 6.3 ms | 4.8 ms | 6.3 ms | 5 |
| bash (echo hello) | 8.3 ms | 9.1 ms | 6.3 ms | 9.1 ms | 5 |
| bash (ls fixture tree) | 9.8 ms | 13.7 ms | 8.5 ms | 13.7 ms | 5 |
| invalid tool (dispatch only) | 0.2 ms | 0.2 ms | 0.1 ms | 0.2 ms | 5 |

#### Reload storm

1 resume(s) per level; each level resumes sessions no other level has opened, from a freshly started daemon.

| Concurrent resumes | history median | p95 | max | storm wall | failures |
|---|---|---|---|---|---|
| 1 | 58.0 ms | 58.0 ms | 58.0 ms | 58.1 ms | 0 |
| 10 | 216.3 ms | 217.0 ms | 217.0 ms | 217.2 ms | 0 |
| 40 | 1.37 s | 1.38 s | 1.38 s | 1.38 s | 0 |

#### Compile loop

| Checkpoint | Touched | median | min | max | samples |
|---|---|---|---|---|---|
| warm_noop | — | 1.57 s | 1.57 s | 1.57 s | 1 |
| leaf_touch | src/main.rs | 1.21 s | 1.21 s | 1.21 s | 1 |
| core_touch | crates/app-core/src/lib.rs | 50.23 s | 50.23 s | 50.23 s | 1 |
| build_meta_touch | crates/build-meta/src/lib.rs | 45.90 s | 45.90 s | 45.90 s | 1 |

### Windows

#### Coding agent comparison (headless, mock model)

Host: windows/AMD64, 4 CPUs, Python 3.12.10. Baseline: **horon**. Samples per one-shot, skill and MCP cell: 5.

Skipped: syscalls (strace not available)

| Tool | Version | `--version` (median) | Install size |
|---|---|---|---|
| horon | horon-agent v0.1.0-dev (f7a53f1) | 11.7 ms | 96.9 MB |
| jcode | jcode v0.99.1 (7206efd0c) | 12.6 ms | 128.4 MB |
| Claude Code | 2.1.317 (Claude Code) | 41.5 ms | 645.2 MB |
| Codex CLI | codex-cli 0.166.0 | 90.7 ms | 479.4 MB |
| OpenCode | 1.19.5 | 461.0 ms | 605.3 MB |
| pi | 1.5.0 | 242.9 ms | 108.9 MB |
| oh-my-pi (omp) | omp/18.18.0 | 655.4 ms | 291.7 MB |
| oh-my-openagent (omo) | omo 5.1.64 (engine: senpi 2026.11.5-1) | 172.8 ms | 514.8 MB |

**One-shot headless prompt** (includes startup, one `ls` tool call, final answer)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Tool round-trip | request bytes | Note |
|---|---|---|---|---|---|---|---|---|
| horon | 160.1 ms | 153.4 ms–230.9 ms | baseline | 35.5 MB | baseline | yes | 3,686 / 2,390 (1 tools) | — |
| jcode | 173.5 ms | 122.6 ms–207.6 ms | 1.08× more | 31.1 MB | 1.14× less | yes | 3,105 / 1,312 (1 tools) | — |
| Claude Code | 1.13 s | 1.12 s–1.84 s | 7.05× more | 273.8 MB | 7.71× more | yes | 50,067 / 33,795 (14 tools) | — |
| Codex CLI | 2.00 s | 1.90 s–2.55 s | 12.52× more | 257.1 MB | 7.24× more | yes | 41,028 / 18,342 (9 tools) | — |
| OpenCode | 4.48 s | 4.43 s–7.30 s | 28.01× more | 796.3 MB | 22.43× more | yes | 2,659 / 0 (0 tools) | — |
| pi | 426.6 ms | 414.1 ms–461.4 ms | 2.67× more | 125.7 MB | 3.54× more | yes | 5,522 / 2,841 (4 tools) | — |
| oh-my-pi (omp) | 3.09 s | 2.86 s–5.91 s | 19.32× more | 417.9 MB | 11.77× more | yes | 29,889 / 18,533 (11 tools) | — |
| oh-my-openagent (omo) | 18.29 s | 16.40 s–24.54 s | 114.27× more | 527.8 MB | 14.86× more | yes | 29,467 / 2,424 (2 tools) | — |

`request bytes` is the first model request of the first run as the mock server received it: JSON body bytes / tool catalog bytes (tools offered).

**Core-profile one-shot** (the one-shot turn without `--tools`, so the request carries the default core surface: the eager tools, `toolbox` and the listed-tool index)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | Note | request bytes |
|---|---|---|---|---|---|---|---|---|---|
| horon | 162.0 ms | 156.8 ms–205.7 ms | baseline | 34.6 MB | baseline | — | yes | — | 11,209 / 9,255 (8 tools) |
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
| horon | 176.3 ms | 169.2 ms–220.6 ms | baseline | 25.0 MB | baseline | skill-uri | yes | — |
| jcode | 223.5 ms | 213.0 ms–253.4 ms | 1.27× more | 23.7 MB | 1.06× less | skill-tool | yes | — |
| Claude Code | 820.5 ms | 798.8 ms–1.05 s | 4.65× more | 238.5 MB | 9.54× more | skill-tool | yes | — |
| Codex CLI | 2.45 s | 2.43 s–3.40 s | 13.90× more | 253.2 MB | 10.13× more | file-read | yes | — |
| OpenCode | 7.67 s | 7.60 s–8.21 s | 43.51× more | 773.4 MB | 30.94× more | skill-tool | yes | — |
| pi | 584.0 ms | 564.8 ms–602.6 ms | 3.31× more | 97.8 MB | 3.91× more | file-read | yes | — |
| oh-my-pi (omp) | 2.96 s | 2.56 s–15.56 s | 16.76× more | 390.4 MB | 15.62× more | file-read | yes | — |
| oh-my-openagent (omo) | 21.07 s | 18.38 s–22.34 s | 119.49× more | 547.7 MB | 21.91× more | file-read | yes | — |

**MCP tool call** (process start to final answer, including starting the fixture stdio MCP server and one `bench_echo` call)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | MCP ready | Note |
|---|---|---|---|---|---|---|---|---|---|
| horon | 323.4 ms | 220.0 ms–349.5 ms | baseline | 37.5 MB | baseline | mcp-call | yes | 153.3 ms | — |
| jcode | 338.9 ms | 332.8 ms–383.8 ms | 1.05× more | 36.7 MB | same | mcp-action | yes | 223.8 ms | — |
| Claude Code | 940.6 ms | 879.0 ms–1.23 s | 2.91× more | 274.1 MB | 7.32× more | mcp-tool | yes | 804.2 ms | — |
| Codex CLI | 904.1 ms | 816.3 ms–3.74 s | 2.80× more | 177.6 MB | 4.74× more | mcp-tool | yes | 672.1 ms | — |
| OpenCode | 7.28 s | 6.98 s–9.52 s | 22.52× more | 808.9 MB | 21.60× more | mcp-tool | yes | 6.00 s | — |
| pi | 644.6 ms | 623.4 ms–739.5 ms | 1.99× more | 116.9 MB | 3.12× more | mcp-tool | yes | 507.5 ms | — |
| oh-my-pi (omp) | 5.11 s | 3.39 s–17.05 s | 15.79× more | 431.6 MB | 11.52× more | mcp-route | yes | 3.45 s | — |
| oh-my-openagent (omo) | 21.09 s | 19.77 s–26.74 s | 65.22× more | 564.5 MB | 15.07× more | mcp-tool | yes | 18.04 s | — |

**Session memory** (idle after 3 turn(s) per session; sizes are process-tree sums, RSS; PSS also recorded on Linux). "Each additional" is the least-squares slope across the measured session counts; 0 B means growth stayed below RSS sampling noise. Threads and descriptors come from the daemon's own `server:memory` after the turns.

| Tool | First session | Each additional | vs horon (additional) | Threads/session | FDs/session | First (post-turn) | Each additional (post-turn) |
|---|---|---|---|---|---|---|---|
| horon | 22.5 MB | 125.5 KB | baseline | — | — | 29.9 MB | 452.7 KB |
| jcode | 22.0 MB | 60.7 KB | 2.07× less | — | — | 27.1 MB | 321.5 KB |
| Claude Code | 154.0 MB | 179.5 MB | 1,464× more | — | — | 187.5 MB | 188.9 MB |
| Codex CLI | 145.2 MB | 115.4 MB | 941.48× more | — | — | 164.7 MB | 117.1 MB |
| OpenCode | 243.7 MB | 14.8 MB | 120.91× more | — | — | 428.1 MB | 26.7 MB |
| pi | 91.1 MB | 90.8 MB | 740.47× more | — | — | 101.0 MB | 100.9 MB |
| oh-my-pi (omp) | 307.6 MB | 156.5 MB | 1,276× more | — | — | 342.4 MB | 336.1 MB |
| oh-my-openagent (omo) | 145.2 MB | 132.2 MB | 1,078× more | — | — | 412.4 MB | 405.3 MB |

Threads/descriptors unavailable: horon (server:memory did not report thread_count, fd_count), jcode (server:memory did not report thread_count, fd_count)

**Post-turn tree RSS per session count**

| Tool | 1 session(s) | 5 session(s) | 10 session(s) |
|---|---|---|---|
| horon | 29.9 MB | 32.3 MB | 33.9 MB |
| jcode | 27.1 MB | 28.7 MB | 29.9 MB |
| Claude Code | 187.5 MB | 945.5 MB | 1.8 GB |
| Codex CLI | 164.7 MB | 648.5 MB | 1.2 GB |
| OpenCode | 428.1 MB | 491.9 MB | 665.1 MB |
| pi | 101.0 MB | 505.0 MB | 1009.1 MB |
| oh-my-pi (omp) | 342.4 MB | 1.6 GB | 3.3 GB |
| oh-my-openagent (omo) | 412.4 MB | 2.1 GB | 4.0 GB |

#### 기동

```text
Benchmarking: D:/a/agent/agent/bench-bin/horon-agent.exe
============================================================

--help (binary load):
  Min:    10.74 ms
  Max:    11.39 ms
  Mean:   11.13 ms
  Median: 11.18 ms
  Stdev:  0.23 ms

--version:
  Min:    10.45 ms
  Max:    11.27 ms
  Mean:   10.81 ms
  Median: 10.82 ms
  Stdev:  0.25 ms

Measuring isolated server startup (10 runs)...

Server ready (isolated socket connectable):
  Min:    16.79 ms
  Max:    22.43 ms
  Mean:   21.54 ms
  Median: 21.98 ms
  Stdev:  1.68 ms

Server startup stages (one extra run with HORON_AGENT_STARTUP_PROFILE=1, not timed):
  stage             at ms  delta ms   share
  panic_hook          0.0       0.0    0.1%
  logging_init        1.3       1.3   23.7%
  nofile_limit        1.7       0.4    7.6%
  perm_harden         3.7       1.9   35.4%
  args_parse          3.7       0.0    0.1%
  config_loaded       4.5       0.8   14.5%
  server_new          5.2       0.7   13.1%
  socket_bound        5.5       0.3    5.5%

Measuring harness bridge readiness (10 runs)...
  skipped: api-bridge is Unix-only, so Windows has no harness bridge to time

Bridge stage: api socket connectable: No successful runs

Bridge stage: hello_ok: No successful runs

Bridge stage: no-op reply: No successful runs

Bridge total (socket -> no-op): No successful runs

============================================================
Summary:
  Binary load median:      ~11.2 ms
  Server ready median:     ~22.0 ms
  Bridge total median:     n/a
```

#### 세션 로딩 (COLD)

```text
binary   D:\a\agent\agent\bench-bin\examples\loading_probe.exe (7295f759c74d)
mode     cold
protocol COLD: every measured run gets its own freshly copied HOME; no derived cache from an earlier run exists, so each sample is a first load of the fixture.
fixtures C:\Users\RUNNER~1\AppData\Local\Temp\horon-loading-bench-8w4vugq9
note     --warmup 3 is ignored in cold mode: every sample gets a fresh HOME
fixture events=0 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    0.103/0.104    p95    0.110/0.186    delta  +0.002 ms  control-no-change
  skill_request_overlay p50    0.746/0.741    p95    0.964/0.818    delta  -0.006 ms  control-no-change
  mcp_config_load   p50    0.961/0.933    p95    1.012/1.002    delta  -0.027 ms  control-no-change
  attach_history    p50    0.002/0.002    p95    0.003/0.002    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.001/0.001    p95    0.002/0.002    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.001/0.001    p95    0.002/0.002    delta  +0.000 ms  control-no-change
  session_save      p50   46.873/48.026   p95   65.201/50.967   delta  +1.152 ms  control-no-change
  total store_read_bytes p50 22944  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 22894..22964 (7 distinct values)
  SPREAD   b session_save store_read_bytes 22904..22960 (7 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50   14.771/14.862   p95   15.534/19.116   delta  +0.091 ms  control-no-change
  skill_request_overlay p50   39.714/39.225   p95   42.920/51.308   delta  -0.488 ms  control-no-change
  mcp_config_load   p50    1.011/1.022    p95    1.041/1.141    delta  +0.010 ms  control-no-change
  attach_history    p50    0.002/0.002    p95    0.002/0.002    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.001/0.001    p95    0.002/0.002    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.001/0.001    p95    0.002/0.001    delta  -0.000 ms  control-no-change
  session_save      p50   56.496/52.860   p95  133.406/86.507   delta  -3.636 ms  control-no-change
  total store_read_bytes p50 22936  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 22918..22956 (8 distinct values)
  SPREAD   b session_save store_read_bytes 22906..22968 (9 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.100/0.104    p95    0.104/0.172    delta  +0.004 ms  control-no-change
  skill_request_overlay p50    0.767/0.849    p95    0.794/1.035    delta  +0.082 ms  control-no-change
  mcp_config_load   p50    0.969/0.936    p95    1.070/1.122    delta  -0.033 ms  control-no-change
  attach_history    p50  429.774/430.257  p95  444.617/453.461  delta  +0.483 ms  control-no-change
  resume_ready      p50  152.559/149.972  p95  167.925/158.242  delta  -2.587 ms  control-no-change
  provider_dispatch p50    1.512/1.482    p95    1.880/1.610    delta  -0.030 ms  control-no-change
  session_save      p50   40.806/47.209   p95   52.242/137.938  delta  +6.403 ms  control-no-change
  total store_read_bytes p50 5311208  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189153..4189155 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099105..1099106 (2 distinct values)
  SPREAD   a session_save store_read_bytes 22919..22979 (9 distinct values)
  SPREAD   b attach_history store_read_bytes 4189153..4189155 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099105..1099106 (2 distinct values)
  SPREAD   b session_save store_read_bytes 22927..22972 (9 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50   15.748/15.887   p95   16.859/18.976   delta  +0.139 ms  control-no-change
  skill_request_overlay p50   39.635/40.389   p95   45.058/42.097   delta  +0.754 ms  control-no-change
  mcp_config_load   p50    1.038/1.020    p95    1.135/1.107    delta  -0.018 ms  control-no-change
  attach_history    p50  433.860/436.367  p95  447.435/555.423  delta  +2.507 ms  control-no-change
  resume_ready      p50  150.434/151.553  p95  160.621/157.715  delta  +1.119 ms  control-no-change
  provider_dispatch p50    1.469/1.468    p95    2.033/1.534    delta  -0.001 ms  control-no-change
  session_save      p50   55.128/43.492   p95  120.998/87.106   delta -11.636 ms  control-no-change
  total store_read_bytes p50 5311207  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189149..4189153 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099101..1099105 (2 distinct values)
  SPREAD   a session_save store_read_bytes 22931..22971 (8 distinct values)
  SPREAD   b attach_history store_read_bytes 4189148..4189153 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099100..1099105 (3 distinct values)
  SPREAD   b session_save store_read_bytes 22935..22971 (8 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.100/0.106    p95    0.108/0.147    delta  +0.006 ms  control-no-change
  skill_request_overlay p50    0.772/0.757    p95    0.880/0.883    delta  -0.015 ms  control-no-change
  mcp_config_load   p50    0.988/0.968    p95    1.075/1.082    delta  -0.020 ms  control-no-change
  attach_history    p50 4877.090/4907.949 p95 5094.289/5212.117 delta +30.859 ms  control-no-change
  resume_ready      p50 1585.274/1580.715 p95 1702.828/1619.578 delta  -4.559 ms  control-no-change
  provider_dispatch p50   16.346/16.025   p95   19.857/19.064   delta  -0.320 ms  control-no-change
  session_save      p50   54.683/44.183   p95  411.835/343.526  delta -10.500 ms  control-no-change
  total store_read_bytes p50 53417428  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184249..42184253 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210212..11210216 (3 distinct values)
  SPREAD   a session_save store_read_bytes 22923..22983 (9 distinct values)
  SPREAD   b attach_history store_read_bytes 42184250..42184253 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210213..11210216 (2 distinct values)
  SPREAD   b session_save store_read_bytes 22919..22975 (9 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50   15.234/15.476   p95   18.190/18.367   delta  +0.242 ms  control-no-change
  skill_request_overlay p50   39.314/40.958   p95   42.805/44.010   delta  +1.644 ms  control-no-change
  mcp_config_load   p50    1.010/1.022    p95    1.125/1.392    delta  +0.012 ms  control-no-change
  attach_history    p50 4921.730/4923.471 p95 5033.648/5173.808 delta  +1.742 ms  control-no-change
  resume_ready      p50 1583.760/1589.096 p95 1599.248/1604.569 delta  +5.336 ms  control-no-change
  provider_dispatch p50   16.601/17.144   p95   18.788/19.319   delta  +0.542 ms  control-no-change
  session_save      p50   43.856/65.408   p95  240.469/416.159  delta +21.552 ms  control-no-change
  total store_read_bytes p50 53417430  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184251..42184253 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210215..11210216 (2 distinct values)
  SPREAD   a session_save store_read_bytes 22922..22979 (9 distinct values)
  SPREAD   b attach_history store_read_bytes 42184249..42184253 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210212..11210216 (2 distinct values)
  SPREAD   b session_save store_read_bytes 22927..22975 (9 distinct values)
report   bench-results\loading-cold.json
mode cold, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### 세션 로딩 (WARM)

```text
binary   D:\a\agent\agent\bench-bin\examples\loading_probe.exe (7295f759c74d)
mode     warm
protocol WARM: one HOME per side, --warmup discarded runs per side, then measured runs alternate between the two sides on those same HOMEs.
fixtures C:\Users\RUNNER~1\AppData\Local\Temp\horon-loading-bench-z6anpfmv
fixture events=0 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    0.105/0.099    p95    0.127/0.116    delta  -0.006 ms  control-no-change
  skill_request_overlay p50    0.753/0.789    p95    0.827/0.858    delta  +0.035 ms  control-no-change
  mcp_config_load   p50    0.917/0.895    p95    1.035/0.971    delta  -0.022 ms  control-no-change
  attach_history    p50    0.002/0.001    p95    0.002/0.002    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.001/0.001    p95    0.002/0.001    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.001/0.001    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  session_save      p50   33.444/33.585   p95   47.944/36.097   delta  +0.142 ms  control-no-change
  total store_read_bytes p50 23647  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [22913, 22939, 22953, 23642, 23646, 23648, 24325, 24329, 24333, 24359]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [22927, 22938, 22943, 22949, 23622, 23629, 23664, 24318, 24330, 24341]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 22913..24359 (10 distinct values)
  SPREAD   b session_save store_read_bytes 22927..24341 (10 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50   14.481/14.424   p95   14.695/14.987   delta  -0.057 ms  control-no-change
  skill_request_overlay p50   36.918/37.132   p95   37.559/38.259   delta  +0.214 ms  control-no-change
  mcp_config_load   p50    0.944/0.931    p95    1.020/0.994    delta  -0.013 ms  control-no-change
  attach_history    p50    0.002/0.002    p95    0.005/0.002    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.001/0.001    p95    0.002/0.002    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.001/0.001    p95    0.002/0.002    delta  +0.000 ms  control-no-change
  session_save      p50   38.826/38.001   p95  258.359/81.518   delta  -0.825 ms  control-no-change
  total store_read_bytes p50 23648  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [22939, 22953, 22957, 22969, 23642, 23654, 23664, 24329, 24339, 24361]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [22930, 22931, 22951, 22957, 23628, 23647, 23648, 24323, 24357, 24358]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 22939..24361 (10 distinct values)
  SPREAD   b session_save store_read_bytes 22930..24358 (10 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.103/0.102    p95    0.156/0.226    delta  -0.001 ms  control-no-change
  skill_request_overlay p50    0.779/0.837    p95    0.843/1.203    delta  +0.058 ms  control-no-change
  mcp_config_load   p50    0.917/0.932    p95    0.967/1.180    delta  +0.016 ms  control-no-change
  attach_history    p50  202.836/200.081  p95  223.993/214.433  delta  -2.755 ms  control-no-change
  resume_ready      p50  151.162/152.660  p95  162.692/167.606  delta  +1.498 ms  control-no-change
  provider_dispatch p50    1.460/1.473    p95    1.516/1.539    delta  +0.012 ms  control-no-change
  session_save      p50   33.032/34.599   p95 2379.986/38.683   delta  +1.567 ms  control-no-change
  total store_read_bytes p50 2841114  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23645, 23675, 24364, 24370, 25031, 25036, 25722, 25743, 26438, 26439]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23645, 23675, 24353, 24372, 25031, 25052, 25766, 25767, 26449, 26450]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23645..26439 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23645..26450 (10 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50   14.538/14.646   p95   16.800/15.272   delta  +0.108 ms  control-no-change
  skill_request_overlay p50   36.967/37.227   p95   38.035/37.903   delta  +0.260 ms  control-no-change
  mcp_config_load   p50    0.946/0.962    p95    1.044/1.060    delta  +0.016 ms  control-no-change
  attach_history    p50  201.567/202.262  p95  203.749/205.193  delta  +0.695 ms  control-no-change
  resume_ready      p50  150.555/150.133  p95  155.278/154.122  delta  -0.422 ms  control-no-change
  provider_dispatch p50    1.448/1.443    p95    1.541/1.613    delta  -0.005 ms  control-no-change
  session_save      p50   32.692/38.941   p95   85.215/43.043   delta  +6.249 ms  control-drift
  total store_read_bytes p50 2841138  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23653, 23675, 24342, 24350, 25055, 25061, 25710, 25754, 26425, 26451]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23635, 23648, 24340, 24357, 25029, 25056, 25748, 25759, 26427, 26442]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23653..26451 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23635..26442 (10 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.099/0.100    p95    0.121/0.129    delta  +0.001 ms  control-no-change
  skill_request_overlay p50    0.820/0.778    p95    0.878/1.042    delta  -0.042 ms  control-no-change
  mcp_config_load   p50    0.915/0.945    p95    1.001/1.178    delta  +0.030 ms  control-no-change
  attach_history    p50 2104.557/2120.867 p95 2168.053/2140.203 delta +16.310 ms  control-no-change
  resume_ready      p50 1586.920/1573.747 p95 1601.251/1607.933 delta -13.173 ms  control-no-change
  provider_dispatch p50   17.904/17.870   p95   18.536/18.280   delta  -0.034 ms  control-no-change
  session_save      p50   35.850/35.519   p95   54.451/2972.025 delta  -0.331 ms  control-no-change
  total store_read_bytes p50 28640146  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23614, 23651, 24341, 24355, 25035, 25067, 25742, 25768, 26441, 26463]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23669, 23673, 24328, 24360, 25044, 25060, 25759, 25765, 26440, 26470]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23614..26463 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23669..26470 (10 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50   14.145/13.932   p95   14.546/16.601   delta  -0.213 ms  control-no-change
  skill_request_overlay p50   36.944/36.895   p95   37.564/42.977   delta  -0.049 ms  control-no-change
  mcp_config_load   p50    0.991/0.951    p95    1.052/1.160    delta  -0.039 ms  control-no-change
  attach_history    p50 2108.544/2096.896 p95 2120.841/2113.263 delta -11.648 ms  control-no-change
  resume_ready      p50 1569.082/1571.582 p95 1576.904/1578.862 delta  +2.501 ms  control-no-change
  provider_dispatch p50   17.306/17.185   p95   18.044/17.768   delta  -0.122 ms  control-no-change
  session_save      p50   33.965/35.224   p95   38.214/47.220   delta  +1.259 ms  control-no-change
  total store_read_bytes p50 28640142  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23668, 23670, 24327, 24361, 25023, 25068, 25744, 25751, 26443, 26454]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23650, 23662, 24365, 24367, 25052, 25076, 25738, 25750, 26451, 26467]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23668..26454 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23650..26467 (10 distinct values)
report   bench-results\loading-warm.json
mode warm, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### Tool latency

| Tool case | median | p95 | min | max | samples |
|---|---|---|---|---|---|
| read (tiny file) | 2.7 ms | 2.7 ms | 2.7 ms | 2.7 ms | 5 |
| read (1000 lines) | 2.7 ms | 2.7 ms | 2.2 ms | 2.7 ms | 5 |
| read (10 lines @ offset) | 2.7 ms | 2.7 ms | 2.2 ms | 2.7 ms | 5 |
| read (repo Cargo.toml) | 2.7 ms | 2.7 ms | 2.2 ms | 2.7 ms | 5 |
| write (small) | 7.9 ms | 10.5 ms | 7.4 ms | 10.5 ms | 5 |
| write (10KB) | 7.4 ms | 10.1 ms | 7.4 ms | 10.1 ms | 5 |
| edit (simple replace) | 7.4 ms | 10.4 ms | 7.4 ms | 10.4 ms | 5 |
| ls (scripts dir) | 2.7 ms | 2.7 ms | 2.2 ms | 2.7 ms | 5 |
| agentgrep (grep in tree) | 7.4 ms | 7.5 ms | 5.3 ms | 7.5 ms | 5 |
| agentgrep (find **/*.rs) | 4.9 ms | 5.3 ms | 4.8 ms | 5.3 ms | 5 |
| bash (true) | 10.5 ms | 13.1 ms | 10.0 ms | 13.1 ms | 5 |
| bash (echo hello) | 10.5 ms | 10.5 ms | 10.1 ms | 10.5 ms | 5 |
| bash (ls fixture tree) | 15.0 ms | 17.9 ms | 12.9 ms | 17.9 ms | 5 |
| invalid tool (dispatch only) | 2.7 ms | 2.7 ms | 2.2 ms | 2.7 ms | 5 |

#### Reload storm

1 resume(s) per level; each level resumes sessions no other level has opened, from a freshly started daemon.

| Concurrent resumes | history median | p95 | max | storm wall | failures |
|---|---|---|---|---|---|
| 1 | 230.8 ms | 230.8 ms | 230.8 ms | 230.9 ms | 0 |
| 10 | 725.8 ms | 729.2 ms | 729.2 ms | 730.0 ms | 0 |
| 40 | 2.64 s | 2.67 s | 3.28 s | 3.28 s | 0 |

#### Compile loop

| Checkpoint | Touched | median | min | max | samples |
|---|---|---|---|---|---|
| warm_noop | — | 584.3 ms | 584.3 ms | 584.3 ms | 1 |
| leaf_touch | src\main.rs | 807.7 ms | 807.7 ms | 807.7 ms | 1 |
| core_touch | crates\app-core\src\lib.rs | 22.48 s | 22.48 s | 22.48 s | 1 |
| build_meta_touch | crates\build-meta\src\lib.rs | 32.03 s | 32.03 s | 32.03 s | 1 |

## TUI (`horon-tui`)

대화형 클라이언트는 별도 바이너리이고 따로 잽니다. 이 수치는 `tui` 묶음에서 나오며 위의 헤드리스 표와 비교하지 않습니다. `first_frame_ms` 와 `direct_first_frame_ms` 는 서로 다른 두 프로그램을 각각 띄운 값이고, 핸드오프는 그 둘의 차이입니다.

![TUI (`horon-tui`)](charts/tui-01-latency.ko.svg)

![TUI (`horon-tui`)](charts/tui-02-memory.ko.svg)

### Linux

#### TUI (`horon-tui`)

Measured version: the interactive client `horon-tui`. The headless `horon-agent` numbers are in the suites above and are never compared with these: the two binaries are timed against themselves.

| Metric | Median | Min–max | Samples |
| --- | --- | --- | --- |
| First frame (bare `horon-agent`) | 26.0 ms | 25.7 ms–27.8 ms | 5 |
| First frame (direct `horon-tui`) | 26.8 ms | 26.6 ms–27.0 ms | 5 |
| TTY handoff cost | -0.8 ms | -1.1 ms–0.8 ms | 5 |
| Key press to echo (p95) | 0.2 ms | 0.2 ms–1.4 ms | 50 |
| Idle RSS after 5 s | 5.4 MB | 5.4 MB–5.5 MB | 5 |
| RSS after one turn | 5.6 MB | 5.6 MB–5.7 MB | 5 |
| Frame draw time (p50) | 1.5 ms | 1.5 ms–1.5 ms | 5 |
| Frame draw time (p95) | 1.6 ms | 1.6 ms–1.6 ms | 5 |
| Install size (`horon-tui`) | 6.0 MB | — | 1 |

Handoff: median first-frame time of a bare `horon-agent` launch (no arguments, on a PTY) minus the median first-frame time of a direct `horon-tui` launch against an already-running daemon, both on the same machine and build. The bare path also starts the daemon, so the number covers that startup, the sibling lookup and the `exec`; it is the extra cost of the handoff over launching the client itself.

Render bench: 200 frames on a 120×40 terminal.

### macOS

#### TUI (`horon-tui`)

Measured version: the interactive client `horon-tui`. The headless `horon-agent` numbers are in the suites above and are never compared with these: the two binaries are timed against themselves.

| Metric | Median | Min–max | Samples |
| --- | --- | --- | --- |
| First frame (bare `horon-agent`) | 82.8 ms | 51.1 ms–489.1 ms | 5 |
| First frame (direct `horon-tui`) | 65.5 ms | 56.3 ms–102.6 ms | 5 |
| TTY handoff cost | -5.2 ms | -21.0 ms–423.6 ms | 5 |
| Key press to echo (p95) | 0.2 ms | 0.1 ms–0.6 ms | 50 |
| Idle RSS after 5 s | 8.8 MB | 8.7 MB–8.9 MB | 5 |
| RSS after one turn | 9.0 MB | 8.9 MB–9.1 MB | 5 |
| Frame draw time (p50) | 1.7 ms | 1.3 ms–1.8 ms | 5 |
| Frame draw time (p95) | 3.2 ms | 3.0 ms–3.4 ms | 5 |
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
| Frame draw time (p50) | 2.0 ms | 1.9 ms–2.0 ms | 5 |
| Frame draw time (p95) | 2.5 ms | 2.3 ms–3.7 ms | 5 |
| Install size (`horon-tui`) | 5.0 MB | — | 1 |

Handoff: median first-frame time of a bare `horon-agent` launch (no arguments, on a PTY) minus the median first-frame time of a direct `horon-tui` launch against an already-running daemon, both on the same machine and build. The bare path also starts the daemon, so the number covers that startup, the sibling lookup and the `exec`; it is the extra cost of the handoff over launching the client itself.

Render bench: 200 frames on a 120×40 terminal.

Skipped: pty unavailable on Windows

<!-- bench-auto:end -->
