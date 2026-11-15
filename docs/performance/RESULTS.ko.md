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

<!-- bench-auto:start commit=cdc8cb87238fdd8433dcf87824f9cca449d230ab -->

## 헤드리스 (`horon-agent`)

이 구간은 `.github/workflows/benchmark-docs.yml` 이 자동으로 다시 씁니다. 손으로 고치지 않습니다.

- 커밋: `cdc8cb87238fdd8433dcf87824f9cca449d230ab`
- 측정 시각: 2026-11-15 17:30 UTC
- 실행: <https://github.com/horon-ai/agent/actions/runs/43736928002> (원본 JSON 은 OS별 `benchmark-<os>-<run_id>` 아티팩트)

OS마다 다른 GitHub 호스티드 러너에서 따로 잽니다. 기계가 다르므로 OS끼리 절대값을 비교하지 않고, 같은 OS 안의 상대 비교(horon 대비 배수)와 커밋 사이의 추세만 읽습니다.

| OS | 러너 | 바이너리 | 상태 |
| --- | --- | --- | --- |
| Linux | `ubuntu-24.04` | `horon-agent v0.1.0-dev (cdc8cb8)` | 성공 |
| macOS | `macos-latest` | `horon-agent v0.1.0-dev (cdc8cb8)` | 성공 |
| Windows | `windows-latest` | `horon-agent v0.1.0-dev (cdc8cb8)` | 성공 |

### Linux

#### Coding agent comparison (headless, mock model)

Host: linux/x86_64, 4 CPUs, Python 3.12.16. Baseline: **horon**. Samples per one-shot, skill and MCP cell: 5.

| Tool | Version | `--version` (median) | Install size |
|---|---|---|---|
| horon | horon-agent v0.1.0-dev (cdc8cb8) | 4.0 ms | 115.7 MB |
| jcode | jcode v0.99.1 (7206efd0c) | 10.6 ms | 163.4 MB |
| Claude Code | 2.1.317 (Claude Code) | 10.6 ms | 632.2 MB |
| Codex CLI | codex-cli 0.166.0 | 43.9 ms | 435.0 MB |
| OpenCode | 1.19.5 | 466.2 ms | 613.4 MB |
| pi | 1.5.0 | 236.8 ms | 108.9 MB |
| oh-my-pi (omp) | omp/18.18.0 | 647.5 ms | 413.1 MB |
| oh-my-openagent (omo) | omo 5.1.64 (engine: senpi 2026.11.5-1) | 119.3 ms | 520.6 MB |

**One-shot headless prompt** (includes startup, one `ls` tool call, final answer)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Tool round-trip | fsync calls | request bytes | Note |
|---|---|---|---|---|---|---|---|---|---|
| horon | 53.5 ms | 40.5 ms–63.2 ms | baseline | 39.8 MB | baseline | yes | 10 | 3,731 / 2,390 (1 tools) | — |
| jcode | 143.8 ms | 143.6 ms–242.8 ms | 2.69× more | 38.8 MB | 1.03× less | yes | 5 | 3,245 / 1,433 (1 tools) | — |
| Claude Code | 440.5 ms | 425.4 ms–476.4 ms | 8.23× more | 206.8 MB | 5.19× more | yes | — | 45,782 / 29,653 (12 tools) | — |
| Codex CLI | 497.0 ms | 471.6 ms–517.7 ms | 9.29× more | 203.6 MB | 5.11× more | yes | 6 | 39,432 / 17,271 (9 tools) | — |
| OpenCode | 2.68 s | 2.62 s–2.89 s | 50.13× more | 569.5 MB | 14.30× more | yes | 4 | 2,659 / 0 (0 tools) | — |
| pi | 417.2 ms | 392.5 ms–425.7 ms | 7.80× more | 115.2 MB | 2.89× more | yes | — | 5,502 / 2,841 (4 tools) | — |
| oh-my-pi (omp) | 2.02 s | 1.80 s–2.26 s | 37.71× more | 395.6 MB | 9.94× more | yes | 71 | 29,860 / 18,533 (11 tools) | — |
| oh-my-openagent (omo) | 6.07 s | 5.94 s–6.52 s | 113.37× more | 667.2 MB | 16.76× more | yes | 36 | 27,452 / 2,424 (2 tools) | — |

`request bytes` is the first model request of the first run as the mock server received it: JSON body bytes / tool catalog bytes (tools offered).

**Core-profile one-shot** (the one-shot turn without `--tools`, so the request carries the default core surface: the eager tools, `toolbox` and the listed-tool index)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | Note | request bytes |
|---|---|---|---|---|---|---|---|---|---|
| horon | 43.9 ms | 43.2 ms–44.5 ms | baseline | 42.0 MB | baseline | — | yes | — | 11,254 / 9,255 (8 tools) |
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
| horon | 36.8 ms | 36.2 ms–37.3 ms | baseline | 18.3 MB | baseline | skill-uri | yes | — |
| jcode | 57.3 ms | 56.3 ms–57.8 ms | 1.56× more | 40.6 MB | 2.22× more | skill-tool | yes | — |
| Claude Code | 433.3 ms | 422.3 ms–457.7 ms | 11.77× more | 207.8 MB | 11.35× more | skill-tool | yes | — |
| Codex CLI | 580.0 ms | 471.4 ms–592.1 ms | 15.75× more | 191.3 MB | 10.45× more | file-read | yes | — |
| OpenCode | 3.15 s | 3.09 s–3.18 s | 85.43× more | 596.1 MB | 32.55× more | skill-tool | yes | — |
| pi | 518.1 ms | 514.4 ms–528.9 ms | 14.07× more | 125.6 MB | 6.86× more | file-read | yes | — |
| oh-my-pi (omp) | 1.99 s | 1.98 s–2.05 s | 54.15× more | 377.6 MB | 20.62× more | file-read | yes | — |
| oh-my-openagent (omo) | 6.63 s | 6.44 s–6.84 s | 179.99× more | 672.2 MB | 36.71× more | file-read | yes | — |

**MCP tool call** (process start to final answer, including starting the fixture stdio MCP server and one `bench_echo` call)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | MCP ready | Note |
|---|---|---|---|---|---|---|---|---|---|
| horon | 63.2 ms | 62.4 ms–64.1 ms | baseline | 45.3 MB | baseline | mcp-call | yes | 46.7 ms | — |
| jcode | 176.4 ms | 175.6 ms–189.8 ms | 2.79× more | 56.4 MB | 1.24× more | mcp-action | yes | 143.8 ms | — |
| Claude Code | 533.5 ms | 525.6 ms–552.6 ms | 8.44× more | 226.7 MB | 5.00× more | mcp-tool | yes | 390.3 ms | — |
| Codex CLI | 534.5 ms | 499.6 ms–616.9 ms | 8.46× more | 194.1 MB | 4.28× more | mcp-tool | yes | 360.5 ms | — |
| OpenCode | 2.93 s | 2.92 s–3.00 s | 46.33× more | 569.1 MB | 12.55× more | mcp-tool | yes | 2.67 s | — |
| pi | 574.9 ms | 556.4 ms–591.7 ms | 9.10× more | 138.9 MB | 3.06× more | mcp-tool | yes | 511.4 ms | — |
| oh-my-pi (omp) | 2.03 s | 2.00 s–2.08 s | 32.07× more | 393.2 MB | 8.67× more | mcp-route | yes | 1.70 s | — |
| oh-my-openagent (omo) | — | — | — | — | — | mcp-tool | no | — | failed: the fixture sentinel never came back |

**Session memory** (idle after 3 turn(s) per session; sizes are process-tree sums, RSS; PSS also recorded on Linux). "Each additional" is the least-squares slope across the measured session counts; 0 B means growth stayed below RSS sampling noise. Threads and descriptors come from the daemon's own `server:memory` after the turns.

| Tool | First session | Each additional | vs horon (additional) | Threads/session | FDs/session | First (post-turn) | Each additional (post-turn) |
|---|---|---|---|---|---|---|---|
| horon | 37.3 MB | 204.7 KB | baseline | 0.45 | 1.00 | 44.8 MB | 509.6 KB |
| jcode | 42.6 MB | 91.4 KB | 2.24× less | 0.12 | — | 46.1 MB | 266.4 KB |
| Claude Code | 150.1 MB | 149.5 MB | 747.82× more | — | — | 186.0 MB | 181.2 MB |
| Codex CLI | 128.5 MB | 132.3 MB | 661.95× more | — | — | 160.7 MB | 164.3 MB |
| OpenCode | 243.7 MB | 526.6 KB | 2.57× more | — | — | 450.6 MB | 6.5 MB |
| pi | 106.4 MB | 108.6 MB | 543.33× more | — | — | 130.2 MB | 123.2 MB |
| oh-my-pi (omp) | 335.3 MB | 74.0 MB | 370.09× more | — | — | 365.3 MB | 357.5 MB |
| oh-my-openagent (omo) | 293.0 MB | 35.2 MB | 175.88× more | — | — | 586.6 MB | 579.4 MB |

Threads/descriptors unavailable: jcode (server:memory did not report fd_count)

**Post-turn tree RSS per session count**

| Tool | 1 session(s) | 5 session(s) | 10 session(s) |
|---|---|---|---|
| horon | 44.8 MB | 47.1 MB | 49.3 MB |
| jcode | 46.1 MB | 47.3 MB | 48.5 MB |
| Claude Code | 186.0 MB | 918.2 MB | 1.8 GB |
| Codex CLI | 160.7 MB | 812.4 MB | 1.6 GB |
| OpenCode | 450.6 MB | 475.4 MB | 509.1 MB |
| pi | 130.2 MB | 627.2 MB | 1.2 GB |
| oh-my-pi (omp) | 365.3 MB | 1.8 GB | 3.5 GB |
| oh-my-openagent (omo) | 586.6 MB | 2.9 GB | 5.7 GB |

#### 기동

```text
Benchmarking: /home/runner/work/agent/agent/bench-bin/horon-agent
============================================================

--help (binary load):
  Min:    3.26 ms
  Max:    3.70 ms
  Mean:   3.46 ms
  Median: 3.42 ms
  Stdev:  0.14 ms

--version:
  Min:    3.18 ms
  Max:    3.43 ms
  Mean:   3.27 ms
  Median: 3.26 ms
  Stdev:  0.08 ms

Measuring isolated server startup (10 runs)...

Server ready (isolated socket connectable):
  Min:    20.48 ms
  Max:    20.70 ms
  Mean:   20.55 ms
  Median: 20.54 ms
  Stdev:  0.06 ms

Server startup stages (one extra run with HORON_AGENT_STARTUP_PROFILE=1, not timed):
  stage             at ms  delta ms   share
  panic_hook          0.0       0.0    0.3%
  logging_init        0.2       0.2   19.5%
  nofile_limit        0.5       0.2   22.6%
  perm_harden         0.5       0.0    1.3%
  args_parse          0.5       0.0    0.1%
  config_loaded       0.6       0.1   12.9%
  server_new          1.0       0.4   34.7%
  socket_bound        1.1       0.1    8.6%

Measuring harness bridge readiness (10 runs)...

Bridge stage: api socket connectable:
  Min:    20.59 ms
  Max:    20.70 ms
  Mean:   20.66 ms
  Median: 20.67 ms
  Stdev:  0.04 ms

Bridge stage: hello_ok:
  Min:    21.17 ms
  Max:    21.45 ms
  Mean:   21.27 ms
  Median: 21.24 ms
  Stdev:  0.08 ms

Bridge stage: no-op reply:
  Min:    21.57 ms
  Max:    21.85 ms
  Mean:   21.65 ms
  Median: 21.62 ms
  Stdev:  0.09 ms

Bridge total (socket -> no-op):
  Min:    21.57 ms
  Max:    21.85 ms
  Mean:   21.65 ms
  Median: 21.62 ms
  Stdev:  0.09 ms

============================================================
Summary:
  Binary load median:      ~3.4 ms
  Server ready median:     ~20.5 ms
  Bridge total median:     ~21.6 ms
```

#### 세션 로딩 (COLD)

```text
binary   /home/runner/work/agent/agent/bench-bin/examples/loading_probe (eb44aea698aa)
mode     cold
protocol COLD: every measured run gets its own freshly copied HOME; no derived cache from an earlier run exists, so each sample is a first load of the fixture.
fixtures /tmp/horon-loading-bench-n9en460v
note     --warmup 3 is ignored in cold mode: every sample gets a fresh HOME
fixture events=0 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    0.007/0.007    p95    0.007/0.008    delta  -0.000 ms  control-no-change
  skill_request_overlay p50    0.039/0.039    p95    0.040/0.040    delta  -0.000 ms  control-no-change
  mcp_config_load   p50    0.053/0.053    p95    0.055/0.055    delta  +0.000 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  session_save      p50    4.577/4.560    p95    6.460/4.705    delta  -0.017 ms  control-no-change
  total store_read_bytes p50 23080  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 23060..23084 (5 distinct values)
  SPREAD   b session_save store_read_bytes 23068..23084 (4 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    1.196/1.196    p95    1.294/1.215    delta  -0.000 ms  control-no-change
  skill_request_overlay p50    1.869/1.880    p95    1.907/1.933    delta  +0.011 ms  control-no-change
  mcp_config_load   p50    0.058/0.058    p95    0.061/0.060    delta  -0.000 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.001/0.000    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  session_save      p50    6.043/6.109    p95    6.665/7.632    delta  +0.066 ms  control-no-change
  total store_read_bytes p50 23074  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 23060..23088 (6 distinct values)
  SPREAD   b session_save store_read_bytes 23068..23084 (4 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.006/0.006    p95    0.008/0.006    delta  -0.001 ms  control-no-change
  skill_request_overlay p50    0.038/0.038    p95    0.040/0.040    delta  -0.000 ms  control-no-change
  mcp_config_load   p50    0.054/0.054    p95    0.067/0.064    delta  +0.000 ms  control-no-change
  attach_history    p50  189.888/189.412  p95  191.745/191.246  delta  -0.476 ms  control-no-change
  resume_ready      p50   27.242/27.197   p95   27.365/27.370   delta  -0.045 ms  control-no-change
  provider_dispatch p50    0.937/0.940    p95    0.956/0.957    delta  +0.003 ms  control-no-change
  session_save      p50    4.601/4.463    p95    5.144/4.680    delta  -0.138 ms  control-no-change
  total store_read_bytes p50 5311403  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189191..4189195 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099132..1099136 (3 distinct values)
  SPREAD   a session_save store_read_bytes 23060..23082 (6 distinct values)
  SPREAD   b attach_history store_read_bytes 4189189..4189195 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099132..1099136 (3 distinct values)
  SPREAD   b session_save store_read_bytes 23070..23090 (6 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    1.199/1.200    p95    1.220/1.244    delta  +0.001 ms  control-no-change
  skill_request_overlay p50    1.871/1.883    p95    1.899/1.926    delta  +0.012 ms  control-no-change
  mcp_config_load   p50    0.059/0.059    p95    0.066/0.061    delta  -0.001 ms  control-no-change
  attach_history    p50  192.895/192.808  p95  194.607/193.292  delta  -0.087 ms  control-no-change
  resume_ready      p50   27.379/27.398   p95   27.685/27.627   delta  +0.019 ms  control-no-change
  provider_dispatch p50    0.944/0.961    p95    0.966/0.978    delta  +0.018 ms  control-no-change
  session_save      p50    4.646/4.760    p95    5.063/4.945    delta  +0.114 ms  control-no-change
  total store_read_bytes p50 5311411  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189191..4189195 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099132..1099136 (3 distinct values)
  SPREAD   a session_save store_read_bytes 23066..23089 (7 distinct values)
  SPREAD   b session_save store_read_bytes 23068..23090 (8 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.006/0.006    p95    0.007/0.007    delta  -0.000 ms  control-no-change
  skill_request_overlay p50    0.038/0.038    p95    0.039/0.052    delta  +0.000 ms  control-no-change
  mcp_config_load   p50    0.055/0.054    p95    0.056/0.058    delta  -0.001 ms  control-no-change
  attach_history    p50 2444.677/2446.968 p95 2488.196/2485.743 delta  +2.291 ms  control-no-change
  resume_ready      p50  325.747/325.300  p95  329.878/329.312  delta  -0.447 ms  control-no-change
  provider_dispatch p50   11.207/11.225   p95   11.453/11.335   delta  +0.018 ms  control-no-change
  session_save      p50    4.956/4.830    p95    5.198/5.013    delta  -0.126 ms  control-no-change
  total store_read_bytes p50 53417616  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184289..42184293 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210242..11210246 (3 distinct values)
  SPREAD   a session_save store_read_bytes 23075..23093 (7 distinct values)
  SPREAD   b attach_history store_read_bytes 42184289..42184293 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210242..11210246 (2 distinct values)
  SPREAD   b session_save store_read_bytes 23065..23085 (5 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    1.195/1.203    p95    1.228/1.215    delta  +0.008 ms  control-no-change
  skill_request_overlay p50    1.836/1.857    p95    1.877/1.870    delta  +0.020 ms  control-no-change
  mcp_config_load   p50    0.059/0.059    p95    0.071/0.069    delta  +0.000 ms  control-no-change
  attach_history    p50 2408.976/2408.600 p95 2437.541/2445.675 delta  -0.376 ms  control-no-change
  resume_ready      p50  322.861/321.503  p95  325.835/325.520  delta  -1.358 ms  control-no-change
  provider_dispatch p50   11.108/11.116   p95   11.419/11.559   delta  +0.008 ms  control-no-change
  session_save      p50    4.795/4.847    p95    5.058/5.140    delta  +0.052 ms  control-no-change
  total store_read_bytes p50 53417618  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184287..42184293 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210240..11210246 (2 distinct values)
  SPREAD   a session_save store_read_bytes 23073..23093 (7 distinct values)
  SPREAD   b attach_history store_read_bytes 42184289..42184293 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210242..11210246 (3 distinct values)
  SPREAD   b session_save store_read_bytes 23065..23093 (7 distinct values)
report   bench-results/loading-cold.json
mode cold, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### 세션 로딩 (WARM)

```text
binary   /home/runner/work/agent/agent/bench-bin/examples/loading_probe (eb44aea698aa)
mode     warm
protocol WARM: one HOME per side, --warmup discarded runs per side, then measured runs alternate between the two sides on those same HOMEs.
fixtures /tmp/horon-loading-bench-n85dep3_
fixture events=0 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    0.008/0.008    p95    0.009/0.009    delta  +0.000 ms  control-no-change
  skill_request_overlay p50    0.051/0.051    p95    0.071/0.052    delta  -0.000 ms  control-no-change
  mcp_config_load   p50    0.073/0.074    p95    0.089/0.104    delta  +0.001 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.001    delta  -0.000 ms  control-no-change
  session_save      p50    6.140/6.270    p95    7.782/7.010    delta  +0.129 ms  control-no-change
  total store_read_bytes p50 23791  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23069, 23077, 23081, 23787, 23791, 24501, 24513, 24517]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23065, 23073, 23787, 23799, 24505, 24509, 24517]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23069..24517 (8 distinct values)
  SPREAD   b session_save store_read_bytes 23065..24517 (7 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    1.712/1.723    p95    2.323/2.031    delta  +0.011 ms  control-no-change
  skill_request_overlay p50    2.484/2.461    p95    2.608/2.632    delta  -0.023 ms  control-no-change
  mcp_config_load   p50    0.079/0.079    p95    0.082/0.098    delta  -0.001 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.001/0.000    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.001    delta  +0.000 ms  control-no-change
  session_save      p50    7.736/7.961    p95    8.677/8.387    delta  +0.225 ms  control-no-change
  total store_read_bytes p50 23799  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23075, 23077, 23085, 23789, 23795, 23803, 24495, 24505, 24507, 24517]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23053, 23069, 23073, 23787, 23791, 24497, 24505, 24511, 24521]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23075..24517 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23053..24521 (9 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.008/0.008    p95    0.010/0.009    delta  -0.000 ms  control-no-change
  skill_request_overlay p50    0.052/0.052    p95    0.069/0.066    delta  +0.000 ms  control-no-change
  mcp_config_load   p50    0.073/0.072    p95    0.105/0.074    delta  -0.001 ms  control-no-change
  attach_history    p50   76.474/76.569   p95   77.403/79.047   delta  +0.096 ms  control-no-change
  resume_ready      p50   37.206/37.106   p95   37.579/37.418   delta  -0.100 ms  control-no-change
  provider_dispatch p50    1.216/1.214    p95    1.249/1.236    delta  -0.001 ms  control-no-change
  session_save      p50    6.622/7.165    p95    7.014/14.778   delta  +0.543 ms  control-no-change
  total store_read_bytes p50 2841355  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23772, 23788, 24518, 25218, 25224, 25940, 25942, 26654, 26664]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23792, 23794, 24496, 24516, 25220, 25226, 25936, 25938, 26658, 26660]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23772..26664 (9 distinct values)
  SPREAD   b session_save store_read_bytes 23792..26660 (10 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    1.717/1.716    p95    1.740/1.731    delta  -0.000 ms  control-no-change
  skill_request_overlay p50    2.451/2.430    p95    2.508/2.466    delta  -0.021 ms  control-no-change
  mcp_config_load   p50    0.077/0.078    p95    0.078/0.084    delta  +0.000 ms  control-no-change
  attach_history    p50   76.003/76.914   p95   76.975/77.791   delta  +0.911 ms  control-no-change
  resume_ready      p50   37.045/37.072   p95   37.502/37.368   delta  +0.027 ms  control-no-change
  provider_dispatch p50    1.192/1.203    p95    1.224/1.279    delta  +0.011 ms  control-no-change
  session_save      p50    8.032/8.031    p95    8.609/8.507    delta  -0.001 ms  control-no-change
  total store_read_bytes p50 2841358  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23796, 24514, 24522, 25220, 25228, 25950, 25954, 26660, 26664]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23792, 23796, 24514, 24518, 25232, 25244, 25930, 25938, 26668]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23796..26664 (9 distinct values)
  SPREAD   b session_save store_read_bytes 23792..26668 (9 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.008/0.008    p95    0.010/0.009    delta  -0.000 ms  control-no-change
  skill_request_overlay p50    0.053/0.052    p95    0.077/0.076    delta  -0.000 ms  control-no-change
  mcp_config_load   p50    0.073/0.072    p95    0.074/0.075    delta  -0.001 ms  control-no-change
  attach_history    p50  856.072/851.085  p95  859.650/862.664  delta  -4.987 ms  control-no-change
  resume_ready      p50  437.438/436.177  p95  446.950/439.177  delta  -1.261 ms  control-no-change
  provider_dispatch p50   14.302/14.297   p95   14.833/14.776   delta  -0.005 ms  control-no-change
  session_save      p50    6.654/6.690    p95    7.217/6.978    delta  +0.035 ms  control-no-change
  total store_read_bytes p50 28640392  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23799, 24517, 24525, 25239, 25243, 25957, 25961, 26667, 26679]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23787, 23803, 24513, 24517, 25231, 25235, 25937, 25965, 26667]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23799..26679 (9 distinct values)
  SPREAD   b session_save store_read_bytes 23787..26667 (9 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    1.741/1.695    p95    1.808/1.729    delta  -0.046 ms  control-no-change
  skill_request_overlay p50    2.425/2.430    p95    2.462/2.563    delta  +0.006 ms  control-no-change
  mcp_config_load   p50    0.077/0.078    p95    0.079/0.083    delta  +0.001 ms  control-no-change
  attach_history    p50  857.397/853.750  p95  864.692/864.272  delta  -3.647 ms  control-no-change
  resume_ready      p50  443.696/446.835  p95  451.232/450.961  delta  +3.140 ms  control-no-change
  provider_dispatch p50   14.621/14.285   p95   16.448/14.951   delta  -0.336 ms  control-no-change
  session_save      p50    8.281/8.129    p95    9.174/8.731    delta  -0.152 ms  control-no-change
  total store_read_bytes p50 28640378  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23789, 23797, 24511, 24519, 25225, 25229, 25947, 25951, 26657, 26677]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23795, 24517, 24521, 25231, 25239, 25945, 25957, 26663, 26667]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23789..26677 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23795..26667 (9 distinct values)
report   bench-results/loading-warm.json
mode warm, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### Tool latency

| Tool case | median | p95 | min | max | samples |
|---|---|---|---|---|---|
| read (tiny file) | 0.2 ms | 0.3 ms | 0.2 ms | 0.3 ms | 5 |
| read (1000 lines) | 0.3 ms | 0.3 ms | 0.2 ms | 0.3 ms | 5 |
| read (10 lines @ offset) | 0.3 ms | 0.3 ms | 0.2 ms | 0.3 ms | 5 |
| read (repo Cargo.toml) | 0.2 ms | 0.3 ms | 0.2 ms | 0.3 ms | 5 |
| write (small) | 0.6 ms | 0.7 ms | 0.6 ms | 0.7 ms | 5 |
| write (10KB) | 0.8 ms | 0.9 ms | 0.8 ms | 0.9 ms | 5 |
| edit (simple replace) | 0.7 ms | 0.9 ms | 0.6 ms | 0.9 ms | 5 |
| ls (scripts dir) | 0.5 ms | 0.7 ms | 0.5 ms | 0.7 ms | 5 |
| agentgrep (grep in tree) | 1.2 ms | 1.3 ms | 1.2 ms | 1.3 ms | 5 |
| agentgrep (find **/*.rs) | 1.0 ms | 1.0 ms | 0.9 ms | 1.0 ms | 5 |
| bash (true) | 1.3 ms | 1.7 ms | 1.3 ms | 1.7 ms | 5 |
| bash (echo hello) | 1.4 ms | 1.5 ms | 1.4 ms | 1.5 ms | 5 |
| bash (ls fixture tree) | 2.3 ms | 2.3 ms | 2.2 ms | 2.3 ms | 5 |
| invalid tool (dispatch only) | 0.1 ms | 0.1 ms | 0.0 ms | 0.1 ms | 5 |

#### Reload storm

1 resume(s) per level; each level resumes sessions no other level has opened, from a freshly started daemon.

| Concurrent resumes | history median | p95 | max | storm wall | failures |
|---|---|---|---|---|---|
| 1 | 76.6 ms | 76.6 ms | 76.6 ms | 76.8 ms | 0 |
| 10 | 250.1 ms | 256.4 ms | 256.4 ms | 256.8 ms | 0 |
| 40 | 845.5 ms | 923.7 ms | 924.8 ms | 926.0 ms | 0 |

#### Compile loop

| Checkpoint | Touched | median | min | max | samples |
|---|---|---|---|---|---|
| warm_noop | — | 264.0 ms | 264.0 ms | 264.0 ms | 1 |
| leaf_touch | src/main.rs | 407.1 ms | 407.1 ms | 407.1 ms | 1 |
| core_touch | crates/app-core/src/lib.rs | 19.80 s | 19.80 s | 19.80 s | 1 |
| build_meta_touch | crates/build-meta/src/lib.rs | 27.00 s | 27.00 s | 27.00 s | 1 |

### macOS

#### Coding agent comparison (headless, mock model)

Host: darwin/arm64, 3 CPUs, Python 3.12.10. Baseline: **horon**. Samples per one-shot, skill and MCP cell: 5.

Skipped: syscalls (strace not available)

| Tool | Version | `--version` (median) | Install size |
|---|---|---|---|
| horon | horon-agent v0.1.0-dev (cdc8cb8) | 11.2 ms | 97.3 MB |
| jcode | jcode v0.99.1 (7206efd0c) | 16.4 ms | 136.3 MB |
| Claude Code | 2.1.317 (Claude Code) | 30.6 ms | 596.8 MB |
| Codex CLI | codex-cli 0.166.0 | 94.5 ms | 346.5 MB |
| OpenCode | 1.19.5 | 525.3 ms | 344.6 MB |
| pi | 1.5.0 | 378.0 ms | 108.9 MB |
| oh-my-pi (omp) | omp/18.18.0 | 1.08 s | 273.5 MB |
| oh-my-openagent (omo) | omo 5.1.64 (engine: senpi 2026.11.5-1) | 198.4 ms | 502.3 MB |

**One-shot headless prompt** (includes startup, one `ls` tool call, final answer)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Tool round-trip | request bytes | Note |
|---|---|---|---|---|---|---|---|---|
| horon | 92.2 ms | 81.3 ms–441.9 ms | baseline | 30.0 MB | baseline | yes | 3,619 / 2,390 (1 tools) | — |
| jcode | 516.6 ms | 469.0 ms–957.5 ms | 5.60× more | 34.0 MB | 1.13× more | yes | 3,133 / 1,433 (1 tools) | — |
| Claude Code | 773.9 ms | 597.6 ms–1.20 s | 8.39× more | 202.7 MB | 6.76× more | yes | 45,798 / 29,653 (12 tools) | — |
| Codex CLI | 584.7 ms | 454.6 ms–1.80 s | 6.34× more | 127.6 MB | 4.25× more | yes | 39,453 / 17,271 (9 tools) | — |
| OpenCode | 3.20 s | 2.71 s–7.24 s | 34.73× more | 746.0 MB | 24.87× more | yes | 2,659 / 0 (0 tools) | — |
| pi | 580.4 ms | 527.3 ms–738.7 ms | 6.29× more | 122.9 MB | 4.10× more | yes | 5,513 / 2,841 (4 tools) | — |
| oh-my-pi (omp) | 2.34 s | 2.25 s–4.33 s | 25.42× more | 441.2 MB | 14.71× more | yes | 29,853 / 18,533 (11 tools) | — |
| oh-my-openagent (omo) | 13.11 s | 11.98 s–14.18 s | 142.18× more | 908.8 MB | 30.29× more | yes | 27,493 / 2,424 (2 tools) | — |

`request bytes` is the first model request of the first run as the mock server received it: JSON body bytes / tool catalog bytes (tools offered).

**Core-profile one-shot** (the one-shot turn without `--tools`, so the request carries the default core surface: the eager tools, `toolbox` and the listed-tool index)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | Note | request bytes |
|---|---|---|---|---|---|---|---|---|---|
| horon | 95.2 ms | 74.1 ms–181.2 ms | baseline | 31.2 MB | baseline | — | yes | — | 11,142 / 9,255 (8 tools) |
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
| horon | 90.6 ms | 73.4 ms–152.8 ms | baseline | 30.1 MB | baseline | skill-uri | yes | — |
| jcode | 482.5 ms | 449.9 ms–659.7 ms | 5.33× more | 37.4 MB | 1.24× more | skill-tool | yes | — |
| Claude Code | 764.3 ms | 660.5 ms–875.1 ms | 8.44× more | 185.0 MB | 6.14× more | skill-tool | yes | — |
| Codex CLI | 676.4 ms | 631.8 ms–766.4 ms | 7.47× more | 144.2 MB | 4.79× more | file-read | yes | — |
| OpenCode | 5.41 s | 4.64 s–7.19 s | 59.74× more | 798.2 MB | 26.50× more | skill-tool | yes | — |
| pi | 902.0 ms | 730.0 ms–928.0 ms | 9.96× more | 134.0 MB | 4.45× more | file-read | yes | — |
| oh-my-pi (omp) | 3.06 s | 2.54 s–3.47 s | 33.75× more | 456.9 MB | 15.17× more | file-read | yes | — |
| oh-my-openagent (omo) | 13.04 s | 12.77 s–17.26 s | 143.94× more | 875.2 MB | 29.05× more | file-read | yes | — |

**MCP tool call** (process start to final answer, including starting the fixture stdio MCP server and one `bench_echo` call)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | MCP ready | Note |
|---|---|---|---|---|---|---|---|---|---|
| horon | 151.4 ms | 120.8 ms–221.9 ms | baseline | 39.7 MB | baseline | mcp-call | yes | 108.6 ms | — |
| jcode | 557.3 ms | 510.3 ms–658.5 ms | 3.68× more | 52.4 MB | 1.32× more | mcp-action | yes | 493.6 ms | — |
| Claude Code | 710.1 ms | 565.2 ms–1.27 s | 4.69× more | 214.7 MB | 5.41× more | mcp-tool | yes | 561.8 ms | — |
| Codex CLI | 688.4 ms | 395.3 ms–839.1 ms | 4.55× more | 142.4 MB | 3.59× more | mcp-tool | yes | 533.6 ms | — |
| OpenCode | 5.16 s | 4.85 s–5.79 s | 34.06× more | 734.2 MB | 18.50× more | mcp-tool | yes | 4.39 s | — |
| pi | 940.3 ms | 809.2 ms–1.29 s | 6.21× more | 131.6 MB | 3.32× more | mcp-tool | yes | 825.5 ms | — |
| oh-my-pi (omp) | 3.57 s | 2.61 s–3.81 s | 23.57× more | 467.2 MB | 11.77× more | mcp-route | yes | 2.62 s | — |
| oh-my-openagent (omo) | — | — | — | — | — | mcp-tool | no | — | failed: the fixture sentinel never came back |

**Session memory** (idle after 3 turn(s) per session; sizes are process-tree sums, RSS; PSS also recorded on Linux). "Each additional" is the least-squares slope across the measured session counts; 0 B means growth stayed below RSS sampling noise. Threads and descriptors come from the daemon's own `server:memory` after the turns.

| Tool | First session | Each additional | vs horon (additional) | Threads/session | FDs/session | First (post-turn) | Each additional (post-turn) |
|---|---|---|---|---|---|---|---|
| horon | 28.7 MB | 136.5 KB | baseline | 0.34 | 1.00 | 35.0 MB | 354.2 KB |
| jcode | 36.5 MB | 186.0 KB | 1.36× more | — | — | 40.2 MB | 342.3 KB |
| Claude Code | 158.8 MB | 142.4 MB | 1,068× more | — | — | 186.9 MB | 181.4 MB |
| Codex CLI | 113.5 MB | 78.7 MB | 590.17× more | — | — | 107.4 MB | 107.4 MB |
| OpenCode | 261.4 MB | 910.8 KB | 6.67× more | — | — | 576.6 MB | 11.6 MB |
| pi | 118.7 MB | 116.8 MB | 875.75× more | — | — | 148.5 MB | 148.5 MB |
| oh-my-pi (omp) | 381.0 MB | 56.0 MB | 419.81× more | — | — | 450.0 MB | 317.9 MB |
| oh-my-openagent (omo) | 56.6 MB | 41.4 MB | 310.49× more | — | — | 784.4 MB | 328.6 MB |

Threads/descriptors unavailable: jcode (server:memory did not report thread_count, fd_count)

**Post-turn tree RSS per session count**

| Tool | 1 session(s) | 5 session(s) | 10 session(s) |
|---|---|---|---|
| horon | 35.0 MB | 36.5 MB | 38.1 MB |
| jcode | 40.2 MB | 41.2 MB | 43.2 MB |
| Claude Code | 186.9 MB | 920.5 MB | 1.8 GB |
| Codex CLI | 107.4 MB | 536.0 MB | 1.0 GB |
| OpenCode | 576.6 MB | 604.7 MB | 679.4 MB |
| pi | 148.5 MB | 740.0 MB | 1.4 GB |
| oh-my-pi (omp) | 450.0 MB | 1.8 GB | 3.2 GB |
| oh-my-openagent (omo) | 784.4 MB | 3.2 GB | 3.7 GB |

#### 기동

```text
Benchmarking: /Users/runner/work/agent/agent/bench-bin/horon-agent
============================================================

--help (binary load):
  Min:    9.27 ms
  Max:    61.30 ms
  Mean:   19.53 ms
  Median: 16.01 ms
  Stdev:  15.12 ms

--version:
  Min:    11.80 ms
  Max:    27.24 ms
  Mean:   18.53 ms
  Median: 17.80 ms
  Stdev:  4.21 ms

Measuring isolated server startup (10 runs)...

Server ready (isolated socket connectable):
  Min:    34.56 ms
  Max:    104.76 ms
  Mean:   73.61 ms
  Median: 83.26 ms
  Stdev:  27.50 ms

Server startup stages (one extra run with HORON_AGENT_STARTUP_PROFILE=1, not timed):
  stage             at ms  delta ms   share
  panic_hook          0.0       0.0    0.1%
  logging_init        0.6       0.5    6.4%
  nofile_limit        1.5       1.0   11.7%
  perm_harden         1.7       0.1    1.8%
  args_parse          1.7       0.0    0.4%
  config_loaded       7.5       5.7   67.5%
  server_new          8.1       0.6    6.9%
  socket_bound        8.5       0.5    5.3%

Measuring harness bridge readiness (10 runs)...

Bridge stage: api socket connectable:
  Min:    22.81 ms
  Max:    128.20 ms
  Mean:   57.24 ms
  Median: 48.38 ms
  Stdev:  33.63 ms

Bridge stage: hello_ok:
  Min:    23.49 ms
  Max:    129.39 ms
  Mean:   58.25 ms
  Median: 49.88 ms
  Stdev:  33.74 ms

Bridge stage: no-op reply:
  Min:    23.93 ms
  Max:    129.99 ms
  Mean:   58.85 ms
  Median: 50.70 ms
  Stdev:  33.77 ms

Bridge total (socket -> no-op):
  Min:    23.93 ms
  Max:    129.99 ms
  Mean:   58.85 ms
  Median: 50.70 ms
  Stdev:  33.77 ms

============================================================
Summary:
  Binary load median:      ~16.0 ms
  Server ready median:     ~83.3 ms
  Bridge total median:     ~50.7 ms
```

#### 세션 로딩 (COLD)

```text
binary   /Users/runner/work/agent/agent/bench-bin/examples/loading_probe (d3aa68ac9905)
mode     cold
protocol COLD: every measured run gets its own freshly copied HOME; no derived cache from an earlier run exists, so each sample is a first load of the fixture.
fixtures /var/folders/h5/t85nd63d71x27_8gl5gw3l_w0000gn/T/horon-loading-bench-8pebf7yk
note     --warmup 3 is ignored in cold mode: every sample gets a fresh HOME
fixture events=0 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    0.029/0.032    p95    0.082/0.072    delta  +0.003 ms  control-no-change
  skill_request_overlay p50    0.254/0.291    p95    0.850/0.902    delta  +0.037 ms  control-no-change
  mcp_config_load   p50    0.310/0.290    p95    0.753/0.850    delta  -0.019 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.001/0.000    delta  +0.000 ms  control-drift
  resume_ready      p50    0.000/0.000    p95    0.001/0.000    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  session_save      p50   27.188/24.992   p95   38.415/34.700   delta  -2.197 ms  control-no-change
  total store_read_bytes p50 22946  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 22926..22954 (7 distinct values)
  SPREAD   b session_save store_read_bytes 22930..22954 (7 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    5.375/5.506    p95    7.596/8.654    delta  +0.131 ms  control-no-change
  skill_request_overlay p50    7.407/6.022    p95   10.268/11.776   delta  -1.385 ms  control-no-change
  mcp_config_load   p50    0.315/0.295    p95    0.718/0.550    delta  -0.020 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.001/0.001    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.001/0.000    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  session_save      p50   21.545/28.652   p95   48.492/63.355   delta  +7.106 ms  control-no-change
  total store_read_bytes p50 22942  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 22934..22954 (6 distinct values)
  SPREAD   b session_save store_read_bytes 22934..22954 (5 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.032/0.031    p95    0.083/0.431    delta  -0.001 ms  control-no-change
  skill_request_overlay p50    0.268/0.229    p95    0.553/0.772    delta  -0.039 ms  control-no-change
  mcp_config_load   p50    0.299/0.288    p95    0.388/0.610    delta  -0.011 ms  control-no-change
  attach_history    p50  363.879/389.337  p95  429.791/450.669  delta +25.458 ms  control-no-change
  resume_ready      p50   77.963/86.094   p95  101.758/97.777   delta  +8.131 ms  control-no-change
  provider_dispatch p50    1.102/1.165    p95    1.835/1.691    delta  +0.063 ms  control-no-change
  session_save      p50   19.640/22.529   p95   23.871/30.379   delta  +2.890 ms  control-no-change
  total store_read_bytes p50 5311308  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189203..4189207 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099141..1099145 (4 distinct values)
  SPREAD   a session_save store_read_bytes 22952..22972 (8 distinct values)
  SPREAD   b attach_history store_read_bytes 4189203..4189207 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099141..1099145 (2 distinct values)
  SPREAD   b session_save store_read_bytes 22948..22972 (5 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    3.934/4.078    p95    6.029/5.976    delta  +0.144 ms  control-no-change
  skill_request_overlay p50    4.980/5.124    p95    7.158/6.795    delta  +0.145 ms  control-no-change
  mcp_config_load   p50    0.264/0.265    p95    0.301/0.396    delta  +0.001 ms  control-no-change
  attach_history    p50  275.377/283.181  p95  392.095/434.773  delta  +7.804 ms  control-no-change
  resume_ready      p50   59.517/58.971   p95   95.546/105.513  delta  -0.546 ms  control-no-change
  provider_dispatch p50    1.010/0.994    p95    1.097/3.239    delta  -0.015 ms  control-no-change
  session_save      p50   17.383/16.431   p95   38.165/76.452   delta  -0.953 ms  control-no-change
  total store_read_bytes p50 5311307  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189203..4189207 (3 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099141..1099145 (3 distinct values)
  SPREAD   a session_save store_read_bytes 22946..22968 (9 distinct values)
  SPREAD   b session_save store_read_bytes 22948..22972 (7 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.027/0.030    p95    0.032/0.059    delta  +0.003 ms  control-no-change
  skill_request_overlay p50    0.247/0.234    p95    0.931/0.443    delta  -0.013 ms  control-no-change
  mcp_config_load   p50    0.281/0.275    p95    0.487/0.523    delta  -0.006 ms  control-no-change
  attach_history    p50 3658.807/4282.357 p95 4444.951/4679.815 delta +623.550 ms  control-no-change
  resume_ready      p50  741.936/756.099  p95 1058.313/927.943  delta +14.163 ms  control-no-change
  provider_dispatch p50   11.563/11.387   p95   16.070/17.825   delta  -0.175 ms  control-no-change
  session_save      p50   26.965/22.697   p95   51.671/54.265   delta  -4.268 ms  control-no-change
  total store_read_bytes p50 53417517  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184301..42184305 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210251..11210255 (3 distinct values)
  SPREAD   a session_save store_read_bytes 22949..22975 (7 distinct values)
  SPREAD   b attach_history store_read_bytes 42184303..42184305 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210254..11210255 (2 distinct values)
  SPREAD   b session_save store_read_bytes 22942..22966 (6 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    4.015/4.802    p95    5.182/7.132    delta  +0.787 ms  control-no-change
  skill_request_overlay p50    5.025/6.143    p95   10.171/8.557    delta  +1.118 ms  control-drift
  mcp_config_load   p50    0.273/0.319    p95    0.375/0.554    delta  +0.046 ms  control-no-change
  attach_history    p50 3323.214/3512.878 p95 3745.697/4059.582 delta +189.664 ms  control-no-change
  resume_ready      p50  688.569/672.233  p95 1003.910/781.887  delta -16.336 ms  control-no-change
  provider_dispatch p50   10.804/9.716    p95   18.724/12.554   delta  -1.088 ms  control-no-change
  session_save      p50   23.748/22.040   p95   58.133/48.146   delta  -1.708 ms  control-no-change
  total store_read_bytes p50 53417513  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184299..42184305 (4 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210250..11210255 (4 distinct values)
  SPREAD   a session_save store_read_bytes 22951..22967 (7 distinct values)
  SPREAD   b session_save store_read_bytes 22938..22967 (6 distinct values)
report   bench-results/loading-cold.json
mode cold, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### 세션 로딩 (WARM)

```text
binary   /Users/runner/work/agent/agent/bench-bin/examples/loading_probe (d3aa68ac9905)
mode     warm
protocol WARM: one HOME per side, --warmup discarded runs per side, then measured runs alternate between the two sides on those same HOMEs.
fixtures /var/folders/h5/t85nd63d71x27_8gl5gw3l_w0000gn/T/horon-loading-bench-8fr0m2ze
fixture events=0 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    0.025/0.027    p95    0.058/0.102    delta  +0.002 ms  control-no-change
  skill_request_overlay p50    0.239/0.245    p95    0.462/0.693    delta  +0.006 ms  control-no-change
  mcp_config_load   p50    0.264/0.269    p95    0.298/1.171    delta  +0.005 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.001/0.001    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.000/0.003    delta  +0.000 ms  control-no-change
  session_save      p50   15.897/18.367   p95   21.652/23.721   delta  +2.470 ms  control-no-change
  total store_read_bytes p50 23661  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [22947, 22957, 23661, 23667, 24375, 24379]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [22937, 22939, 22941, 22951, 23649, 23661, 23669, 24375, 24391]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 22947..24379 (6 distinct values)
  SPREAD   b session_save store_read_bytes 22937..24391 (9 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    6.284/5.346    p95   15.755/12.007   delta  -0.938 ms  control-no-change
  skill_request_overlay p50    8.042/6.570    p95   12.388/11.309   delta  -1.472 ms  control-no-change
  mcp_config_load   p50    0.296/0.298    p95    0.615/0.907    delta  +0.002 ms  control-no-change
  attach_history    p50    0.000/0.000    p95    0.001/0.001    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.000/0.000    p95    0.000/0.000    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.000/0.000    p95    0.002/0.003    delta  -0.000 ms  control-no-change
  session_save      p50   22.999/24.627   p95   56.092/37.758   delta  +1.628 ms  control-no-change
  total store_read_bytes p50 23658  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [22939, 22941, 22951, 23655, 23661, 23665, 24373, 24375, 24383]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [22943, 22945, 22947, 22957, 23655, 23657, 23663, 24373, 24379, 24387]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 22939..24383 (9 distinct values)
  SPREAD   b session_save store_read_bytes 22943..24387 (10 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.028/0.031    p95    0.041/0.103    delta  +0.003 ms  control-no-change
  skill_request_overlay p50    0.231/0.252    p95    0.243/0.462    delta  +0.021 ms  control-no-change
  mcp_config_load   p50    0.258/0.284    p95    0.276/0.504    delta  +0.026 ms  control-no-change
  attach_history    p50  121.948/127.216  p95  140.973/144.460  delta  +5.268 ms  control-no-change
  resume_ready      p50   70.974/75.760   p95   76.108/94.791   delta  +4.786 ms  control-no-change
  provider_dispatch p50    1.088/1.156    p95    1.483/2.318    delta  +0.068 ms  control-no-change
  session_save      p50   20.011/20.679   p95   23.820/59.566   delta  +0.669 ms  control-no-change
  total store_read_bytes p50 2841251  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23662, 23668, 24378, 24384, 25090, 25096, 25818, 25824, 26512, 26532]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23666, 23678, 24380, 24388, 25090, 25092, 25806, 25808, 26520, 26522]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23662..26532 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23666..26522 (10 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    4.419/5.046    p95    7.611/7.471    delta  +0.627 ms  control-no-change
  skill_request_overlay p50    5.370/6.007    p95    5.900/8.910    delta  +0.637 ms  control-no-change
  mcp_config_load   p50    0.274/0.283    p95    0.402/0.301    delta  +0.009 ms  control-no-change
  attach_history    p50  110.386/108.742  p95  141.105/123.595  delta  -1.644 ms  control-no-change
  resume_ready      p50   68.577/61.680   p95   90.244/89.996   delta  -6.897 ms  control-no-change
  provider_dispatch p50    1.089/1.165    p95    1.521/1.575    delta  +0.076 ms  control-no-change
  session_save      p50   17.575/18.612   p95   31.159/25.181   delta  +1.037 ms  control-no-change
  total store_read_bytes p50 2841260  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23658, 23674, 24384, 24392, 25102, 25812, 25816, 26518, 26526]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23654, 23678, 24384, 24396, 25098, 25106, 25800, 25806, 26522, 26532]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23658..26526 (9 distinct values)
  SPREAD   b session_save store_read_bytes 23654..26532 (10 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.035/0.032    p95    0.068/0.045    delta  -0.003 ms  control-no-change
  skill_request_overlay p50    0.245/0.227    p95    0.583/0.327    delta  -0.018 ms  control-no-change
  mcp_config_load   p50    0.286/0.262    p95    0.654/0.354    delta  -0.023 ms  control-no-change
  attach_history    p50 1337.974/1406.527 p95 1545.544/1591.726 delta +68.553 ms  control-no-change
  resume_ready      p50  873.576/850.192  p95  917.134/890.752  delta -23.385 ms  control-no-change
  provider_dispatch p50   11.975/11.944   p95   16.348/18.448   delta  -0.032 ms  control-no-change
  session_save      p50   19.349/18.483   p95   24.322/30.223   delta  -0.866 ms  control-no-change
  total store_read_bytes p50 28640290  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23677, 23685, 24375, 24391, 25113, 25117, 25807, 25815, 26529, 26533]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23663, 23667, 24385, 24389, 25101, 25811, 25815, 26517, 26529]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23677..26533 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23663..26529 (9 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    4.530/4.389    p95    5.773/11.557   delta  -0.141 ms  control-no-change
  skill_request_overlay p50    5.515/6.072    p95    6.788/10.767   delta  +0.557 ms  control-no-change
  mcp_config_load   p50    0.281/0.306    p95    0.694/0.939    delta  +0.025 ms  control-no-change
  attach_history    p50 1231.209/1255.346 p95 1483.173/1559.813 delta +24.137 ms  control-no-change
  resume_ready      p50  793.989/814.290  p95  957.775/1032.826 delta +20.301 ms  control-no-change
  provider_dispatch p50   12.495/10.790   p95   16.305/17.981   delta  -1.704 ms  control-no-change
  session_save      p50   24.281/22.186   p95   42.511/77.513   delta  -2.095 ms  control-no-change
  total store_read_bytes p50 28640284  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 23490  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23665, 23681, 24387, 24391, 25105, 25113, 25807, 25819, 26521, 26529]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23672, 23684, 24386, 24390, 25096, 25104, 25818, 25822, 26524]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23665..26529 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23672..26524 (9 distinct values)
report   bench-results/loading-warm.json
mode warm, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### Tool latency

| Tool case | median | p95 | min | max | samples |
|---|---|---|---|---|---|
| read (tiny file) | 0.3 ms | 0.4 ms | 0.2 ms | 0.4 ms | 5 |
| read (1000 lines) | 0.5 ms | 0.5 ms | 0.4 ms | 0.5 ms | 5 |
| read (10 lines @ offset) | 0.5 ms | 0.6 ms | 0.5 ms | 0.6 ms | 5 |
| read (repo Cargo.toml) | 0.3 ms | 0.4 ms | 0.3 ms | 0.4 ms | 5 |
| write (small) | 2.0 ms | 2.1 ms | 1.7 ms | 2.1 ms | 5 |
| write (10KB) | 2.1 ms | 2.4 ms | 2.0 ms | 2.4 ms | 5 |
| edit (simple replace) | 1.9 ms | 2.2 ms | 1.7 ms | 2.2 ms | 5 |
| ls (scripts dir) | 0.9 ms | 1.2 ms | 0.8 ms | 1.2 ms | 5 |
| agentgrep (grep in tree) | 2.8 ms | 2.9 ms | 2.7 ms | 2.9 ms | 5 |
| agentgrep (find **/*.rs) | 1.6 ms | 1.7 ms | 1.5 ms | 1.7 ms | 5 |
| bash (true) | 3.9 ms | 4.3 ms | 3.8 ms | 4.3 ms | 5 |
| bash (echo hello) | 3.9 ms | 4.4 ms | 3.7 ms | 4.4 ms | 5 |
| bash (ls fixture tree) | 6.6 ms | 6.7 ms | 6.4 ms | 6.7 ms | 5 |
| invalid tool (dispatch only) | 0.1 ms | 0.1 ms | 0.1 ms | 0.1 ms | 5 |

#### Reload storm

1 resume(s) per level; each level resumes sessions no other level has opened, from a freshly started daemon.

| Concurrent resumes | history median | p95 | max | storm wall | failures |
|---|---|---|---|---|---|
| 1 | 136.8 ms | 136.8 ms | 136.8 ms | 137.0 ms | 0 |
| 10 | 440.5 ms | 441.3 ms | 441.3 ms | 441.8 ms | 0 |
| 40 | 2.36 s | 2.36 s | 2.36 s | 2.36 s | 0 |

#### Compile loop

| Checkpoint | Touched | median | min | max | samples |
|---|---|---|---|---|---|
| warm_noop | — | 2.18 s | 2.18 s | 2.18 s | 1 |
| leaf_touch | src/main.rs | 1.32 s | 1.32 s | 1.32 s | 1 |
| core_touch | crates/app-core/src/lib.rs | 39.06 s | 39.06 s | 39.06 s | 1 |
| build_meta_touch | crates/build-meta/src/lib.rs | 54.39 s | 54.39 s | 54.39 s | 1 |

### Windows

#### Coding agent comparison (headless, mock model)

Host: windows/AMD64, 4 CPUs, Python 3.12.10. Baseline: **horon**. Samples per one-shot, skill and MCP cell: 5.

Skipped: syscalls (strace not available)

| Tool | Version | `--version` (median) | Install size |
|---|---|---|---|
| horon | horon-agent v0.1.0-dev (cdc8cb8) | 13.7 ms | 97.0 MB |
| jcode | jcode v0.99.1 (7206efd0c) | 16.0 ms | 128.4 MB |
| Claude Code | 2.1.317 (Claude Code) | 39.9 ms | 645.2 MB |
| Codex CLI | codex-cli 0.166.0 | 97.7 ms | 479.4 MB |
| OpenCode | 1.19.5 | 468.3 ms | 605.3 MB |
| pi | 1.5.0 | 253.0 ms | 108.9 MB |
| oh-my-pi (omp) | omp/18.18.0 | 637.9 ms | 291.7 MB |
| oh-my-openagent (omo) | omo 5.1.64 (engine: senpi 2026.11.5-1) | 187.5 ms | 514.8 MB |

**One-shot headless prompt** (includes startup, one `ls` tool call, final answer)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Tool round-trip | request bytes | Note |
|---|---|---|---|---|---|---|---|---|
| horon | 206.5 ms | 155.6 ms–264.0 ms | baseline | 36.5 MB | baseline | yes | 3,686 / 2,390 (1 tools) | — |
| jcode | 161.8 ms | 136.0 ms–234.0 ms | 1.28× less | 28.2 MB | 1.29× less | yes | 3,105 / 1,312 (1 tools) | — |
| Claude Code | 1.23 s | 1.23 s–1.86 s | 5.97× more | 273.2 MB | 7.49× more | yes | 50,067 / 33,795 (14 tools) | — |
| Codex CLI | 1.88 s | 1.39 s–3.35 s | 9.09× more | 262.7 MB | 7.21× more | yes | 41,028 / 18,342 (9 tools) | — |
| OpenCode | 4.53 s | 4.43 s–6.92 s | 21.93× more | 865.3 MB | 23.74× more | yes | 2,659 / 0 (0 tools) | — |
| pi | 431.4 ms | 430.1 ms–472.6 ms | 2.09× more | 125.6 MB | 3.44× more | yes | 5,522 / 2,841 (4 tools) | — |
| oh-my-pi (omp) | 2.69 s | 2.63 s–3.06 s | 13.02× more | 413.6 MB | 11.35× more | yes | 29,889 / 18,533 (11 tools) | — |
| oh-my-openagent (omo) | 19.34 s | 18.78 s–26.61 s | 93.63× more | 546.3 MB | 14.99× more | yes | 29,467 / 2,424 (2 tools) | — |

`request bytes` is the first model request of the first run as the mock server received it: JSON body bytes / tool catalog bytes (tools offered).

**Core-profile one-shot** (the one-shot turn without `--tools`, so the request carries the default core surface: the eager tools, `toolbox` and the listed-tool index)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | Note | request bytes |
|---|---|---|---|---|---|---|---|---|---|
| horon | 172.9 ms | 161.2 ms–218.6 ms | baseline | 33.6 MB | baseline | — | yes | — | 11,209 / 9,255 (8 tools) |
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
| horon | 169.3 ms | 155.9 ms–305.4 ms | baseline | 24.9 MB | baseline | skill-uri | yes | — |
| jcode | 213.8 ms | 188.6 ms–386.5 ms | 1.26× more | 23.6 MB | 1.05× less | skill-tool | yes | — |
| Claude Code | 946.2 ms | 828.4 ms–1.23 s | 5.59× more | 240.5 MB | 9.66× more | skill-tool | yes | — |
| Codex CLI | 1.98 s | 1.86 s–3.33 s | 11.71× more | 264.1 MB | 10.61× more | file-read | yes | — |
| OpenCode | 7.11 s | 6.73 s–7.36 s | 41.98× more | 739.9 MB | 29.73× more | skill-tool | yes | — |
| pi | 587.9 ms | 578.0 ms–606.6 ms | 3.47× more | 97.7 MB | 3.92× more | file-read | yes | — |
| oh-my-pi (omp) | 12.96 s | 3.52 s–13.69 s | 76.57× more | 409.2 MB | 16.44× more | file-read | yes | — |
| oh-my-openagent (omo) | 21.11 s | 17.54 s–29.50 s | 124.70× more | 540.3 MB | 21.71× more | file-read | yes | — |

**MCP tool call** (process start to final answer, including starting the fixture stdio MCP server and one `bench_echo` call)

| Tool | Time (median) | Min–max | vs horon | Peak tree RSS | vs horon | Via | Tool round-trip | MCP ready | Note |
|---|---|---|---|---|---|---|---|---|---|
| horon | 241.2 ms | 189.2 ms–362.3 ms | baseline | 37.4 MB | baseline | mcp-call | yes | 149.7 ms | — |
| jcode | 318.1 ms | 305.4 ms–779.2 ms | 1.32× more | 36.6 MB | 1.02× less | mcp-action | yes | 220.4 ms | — |
| Claude Code | 1.24 s | 998.1 ms–2.27 s | 5.15× more | 271.2 MB | 7.26× more | mcp-tool | yes | 1.10 s | — |
| Codex CLI | 1.02 s | 735.5 ms–2.08 s | 4.24× more | 179.1 MB | 4.79× more | mcp-tool | yes | 790.0 ms | — |
| OpenCode | 6.39 s | 5.99 s–7.13 s | 26.47× more | 3.2 GB | 87.37× more | mcp-tool | yes | 5.00 s | — |
| pi | 645.0 ms | 626.2 ms–1.58 s | 2.67× more | 119.0 MB | 3.18× more | mcp-tool | yes | 516.7 ms | — |
| oh-my-pi (omp) | 3.55 s | 3.21 s–3.72 s | 14.71× more | 439.3 MB | 11.75× more | mcp-route | yes | 2.66 s | — |
| oh-my-openagent (omo) | 22.20 s | 17.49 s–29.16 s | 92.03× more | 566.9 MB | 15.16× more | mcp-tool | yes | 19.01 s | — |

**Session memory** (idle after 3 turn(s) per session; sizes are process-tree sums, RSS; PSS also recorded on Linux). "Each additional" is the least-squares slope across the measured session counts; 0 B means growth stayed below RSS sampling noise. Threads and descriptors come from the daemon's own `server:memory` after the turns.

| Tool | First session | Each additional | vs horon (additional) | Threads/session | FDs/session | First (post-turn) | Each additional (post-turn) |
|---|---|---|---|---|---|---|---|
| horon | 22.5 MB | 133.3 KB | baseline | — | — | 30.2 MB | 277.7 KB |
| jcode | 22.0 MB | 84.6 KB | 1.57× less | — | — | 26.6 MB | 401.1 KB |
| Claude Code | 154.8 MB | 171.0 MB | 1,313× more | — | — | 187.8 MB | 189.7 MB |
| Codex CLI | 124.7 MB | 97.5 MB | 749.14× more | — | — | 162.9 MB | 116.5 MB |
| OpenCode | 246.7 MB | 15.8 MB | 121.74× more | — | — | 436.9 MB | 17.8 MB |
| pi | 90.8 MB | 90.6 MB | 696.21× more | — | — | 100.5 MB | 101.0 MB |
| oh-my-pi (omp) | 322.0 MB | 130.5 MB | 1,003× more | — | — | 335.6 MB | 340.4 MB |
| oh-my-openagent (omo) | 145.3 MB | 130.3 MB | 1,001× more | — | — | 411.7 MB | 403.4 MB |

Threads/descriptors unavailable: horon (server:memory did not report thread_count, fd_count), jcode (server:memory did not report thread_count, fd_count)

**Post-turn tree RSS per session count**

| Tool | 1 session(s) | 5 session(s) | 10 session(s) |
|---|---|---|---|
| horon | 30.2 MB | 32.4 MB | 32.7 MB |
| jcode | 26.6 MB | 28.8 MB | 30.2 MB |
| Claude Code | 187.8 MB | 950.3 MB | 1.9 GB |
| Codex CLI | 162.9 MB | 648.1 MB | 1.2 GB |
| OpenCode | 436.9 MB | 458.2 MB | 593.8 MB |
| pi | 100.5 MB | 504.6 MB | 1009.4 MB |
| oh-my-pi (omp) | 335.6 MB | 1.7 GB | 3.3 GB |
| oh-my-openagent (omo) | 411.7 MB | 2.1 GB | 4.0 GB |

#### 기동

```text
Benchmarking: D:/a/agent/agent/bench-bin/horon-agent.exe
============================================================

--help (binary load):
  Min:    11.01 ms
  Max:    11.62 ms
  Mean:   11.19 ms
  Median: 11.12 ms
  Stdev:  0.20 ms

--version:
  Min:    10.73 ms
  Max:    13.90 ms
  Mean:   12.03 ms
  Median: 11.74 ms
  Stdev:  1.14 ms

Measuring isolated server startup (10 runs)...

Server ready (isolated socket connectable):
  Min:    21.76 ms
  Max:    27.77 ms
  Mean:   22.51 ms
  Median: 21.94 ms
  Stdev:  1.86 ms

Server startup stages (one extra run with HORON_AGENT_STARTUP_PROFILE=1, not timed):
  stage             at ms  delta ms   share
  panic_hook          0.0       0.0    0.1%
  logging_init        1.3       1.3   23.7%
  nofile_limit        1.7       0.4    7.4%
  perm_harden         3.7       1.9   34.3%
  args_parse          3.7       0.0    0.0%
  config_loaded       4.5       0.8   14.7%
  server_new          5.3       0.8   14.4%
  socket_bound        5.6       0.3    5.5%

Measuring harness bridge readiness (10 runs)...
  skipped: api-bridge is Unix-only, so Windows has no harness bridge to time

Bridge stage: api socket connectable: No successful runs

Bridge stage: hello_ok: No successful runs

Bridge stage: no-op reply: No successful runs

Bridge total (socket -> no-op): No successful runs

============================================================
Summary:
  Binary load median:      ~11.1 ms
  Server ready median:     ~21.9 ms
  Bridge total median:     n/a
```

#### 세션 로딩 (COLD)

```text
binary   D:\a\agent\agent\bench-bin\examples\loading_probe.exe (bd20c45db24b)
mode     cold
protocol COLD: every measured run gets its own freshly copied HOME; no derived cache from an earlier run exists, so each sample is a first load of the fixture.
fixtures C:\Users\RUNNER~1\AppData\Local\Temp\horon-loading-bench-nngru5v_
note     --warmup 3 is ignored in cold mode: every sample gets a fresh HOME
fixture events=0 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50    0.155/0.155    p95    0.213/0.213    delta  -0.001 ms  control-no-change
  skill_request_overlay p50    1.179/1.192    p95    1.279/1.235    delta  +0.013 ms  control-no-change
  mcp_config_load   p50    1.410/1.410    p95    1.543/1.447    delta  -0.001 ms  control-no-change
  attach_history    p50    0.002/0.002    p95    0.002/0.002    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.002/0.002    p95    0.002/0.003    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.002/0.001    p95    0.002/0.002    delta  -0.000 ms  control-no-change
  session_save      p50   54.304/55.576   p95   63.361/65.912   delta  +1.272 ms  control-no-change
  total store_read_bytes p50 22942  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 22920..22960 (8 distinct values)
  SPREAD   b session_save store_read_bytes 22916..22964 (7 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median - vacuous, this fixture has no session)
  skill_catalog     p50   23.202/23.483   p95   33.191/35.470   delta  +0.281 ms  control-no-change
  skill_request_overlay p50   53.020/53.368   p95   58.012/57.962   delta  +0.348 ms  control-no-change
  mcp_config_load   p50    1.467/1.465    p95    1.593/1.672    delta  -0.002 ms  control-no-change
  attach_history    p50    0.002/0.002    p95    0.005/0.005    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.002/0.002    p95    0.002/0.002    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.002/0.002    p95    0.002/0.002    delta  +0.000 ms  control-no-change
  session_save      p50   61.386/62.669   p95   69.989/74.017   delta  +1.283 ms  control-no-change
  total store_read_bytes p50 22956  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 22910..22964 (6 distinct values)
  SPREAD   b session_save store_read_bytes 22926..22960 (8 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.155/0.150    p95    0.253/0.155    delta  -0.005 ms  control-no-change
  skill_request_overlay p50    1.213/1.202    p95    1.460/1.268    delta  -0.011 ms  control-no-change
  mcp_config_load   p50    1.429/1.409    p95    1.795/1.497    delta  -0.021 ms  control-no-change
  attach_history    p50  556.363/559.558  p95  562.346/567.784  delta  +3.196 ms  control-no-change
  resume_ready      p50  225.802/227.612  p95  232.395/235.091  delta  +1.810 ms  control-no-change
  provider_dispatch p50    1.718/1.748    p95    1.796/2.024    delta  +0.030 ms  control-no-change
  session_save      p50   46.892/46.494   p95   59.466/109.512  delta  -0.398 ms  control-no-change
  total store_read_bytes p50 5311217  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 4189149..4189155 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 1099100..1099106 (2 distinct values)
  SPREAD   a session_save store_read_bytes 22907..22972 (7 distinct values)
  SPREAD   b attach_history store_read_bytes 4189151..4189155 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099102..1099106 (3 distinct values)
  SPREAD   b session_save store_read_bytes 22912..22976 (8 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50   23.704/23.998   p95   26.049/27.277   delta  +0.294 ms  control-no-change
  skill_request_overlay p50   52.388/52.674   p95   58.730/55.569   delta  +0.285 ms  control-no-change
  mcp_config_load   p50    1.436/1.461    p95    1.621/2.001    delta  +0.025 ms  control-no-change
  attach_history    p50  554.105/556.213  p95  568.980/570.666  delta  +2.108 ms  control-no-change
  resume_ready      p50  227.900/225.849  p95  235.897/237.280  delta  -2.051 ms  control-no-change
  provider_dispatch p50    1.707/1.681    p95    1.735/1.735    delta  -0.026 ms  control-no-change
  session_save      p50   49.532/46.547   p95   61.579/55.355   delta  -2.985 ms  control-no-change
  total store_read_bytes p50 5311226  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 22944..22976 (6 distinct values)
  SPREAD   b attach_history store_read_bytes 4189151..4189155 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 1099102..1099106 (3 distinct values)
  SPREAD   b session_save store_read_bytes 22940..22976 (8 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50    0.151/0.155    p95    0.166/0.218    delta  +0.003 ms  control-no-change
  skill_request_overlay p50    1.229/1.234    p95    1.318/1.861    delta  +0.005 ms  control-no-change
  mcp_config_load   p50    1.436/1.420    p95    1.496/2.157    delta  -0.016 ms  control-no-change
  attach_history    p50 6316.728/6334.928 p95 8243.767/6677.237 delta +18.200 ms  control-no-change
  resume_ready      p50 2387.696/2382.965 p95 2562.764/2456.150 delta  -4.731 ms  control-no-change
  provider_dispatch p50   17.956/18.297   p95   21.763/23.116   delta  +0.341 ms  control-no-change
  session_save      p50   51.686/52.347   p95   98.799/271.147  delta  +0.661 ms  control-no-change
  total store_read_bytes p50 53417418  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a attach_history store_read_bytes 42184249..42184253 (2 distinct values)
  SPREAD   a resume_ready store_read_bytes 11210212..11210216 (2 distinct values)
  SPREAD   a session_save store_read_bytes 22927..22963 (9 distinct values)
  SPREAD   b attach_history store_read_bytes 42184251..42184253 (2 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210215..11210216 (2 distinct values)
  SPREAD   b session_save store_read_bytes 22930..22970 (9 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=cold -> measured (fixture-stage counters stable: True, criterion within 1% of the median)
  skill_catalog     p50   23.611/23.534   p95   24.354/24.669   delta  -0.078 ms  control-no-change
  skill_request_overlay p50   52.473/52.591   p95   54.394/58.076   delta  +0.118 ms  control-no-change
  mcp_config_load   p50    1.476/1.478    p95    1.511/1.534    delta  +0.001 ms  control-no-change
  attach_history    p50 6310.731/6312.044 p95 6353.843/6332.412 delta  +1.313 ms  control-no-change
  resume_ready      p50 2385.441/2372.398 p95 2405.375/2397.799 delta -13.043 ms  control-no-change
  provider_dispatch p50   17.441/18.042   p95   18.323/25.058   delta  +0.601 ms  control-drift
  session_save      p50   45.811/48.499   p95   61.327/55.241   delta  +2.688 ms  control-no-change
  total store_read_bytes p50 53417423  full_source_hashes p50 2  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  SPREAD   a session_save store_read_bytes 22926..22969 (9 distinct values)
  SPREAD   b attach_history store_read_bytes 42184249..42184253 (3 distinct values)
  SPREAD   b resume_ready store_read_bytes 11210212..11210216 (3 distinct values)
  SPREAD   b session_save store_read_bytes 22938..22971 (9 distinct values)
report   bench-results\loading-cold.json
mode cold, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### 세션 로딩 (WARM)

```text
binary   D:\a\agent\agent\bench-bin\examples\loading_probe.exe (bd20c45db24b)
mode     warm
protocol WARM: one HOME per side, --warmup discarded runs per side, then measured runs alternate between the two sides on those same HOMEs.
fixtures C:\Users\RUNNER~1\AppData\Local\Temp\horon-loading-bench-coeskliq
fixture events=0 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50    0.106/0.098    p95    0.130/0.099    delta  -0.008 ms  control-no-change
  skill_request_overlay p50    0.744/0.753    p95    0.769/0.831    delta  +0.008 ms  control-no-change
  mcp_config_load   p50    0.871/0.934    p95    1.006/0.993    delta  +0.064 ms  control-no-change
  attach_history    p50    0.002/0.001    p95    0.002/0.002    delta  -0.000 ms  control-no-change
  resume_ready      p50    0.001/0.001    p95    0.002/0.001    delta  +0.000 ms  control-no-change
  provider_dispatch p50    0.001/0.001    p95    0.002/0.002    delta  +0.000 ms  control-no-change
  session_save      p50   39.197/41.553   p95   56.156/53.646   delta  +2.355 ms  control-no-change
  total store_read_bytes p50 23655  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [22933, 22943, 22951, 23630, 23654, 23656, 24319, 24321, 24339]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [22927, 22933, 22957, 23616, 23656, 24306, 24332, 24339]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 22933..24339 (9 distinct values)
  SPREAD   b session_save store_read_bytes 22927..24339 (8 distinct values)
fixture events=0 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical - vacuous, this fixture has no session)
  skill_catalog     p50   15.158/15.324   p95   15.313/20.521   delta  +0.166 ms  control-no-change
  skill_request_overlay p50   40.666/40.878   p95   41.740/43.536   delta  +0.211 ms  control-no-change
  mcp_config_load   p50    1.002/1.024    p95    1.079/1.104    delta  +0.021 ms  control-no-change
  attach_history    p50    0.002/0.002    p95    0.005/0.004    delta  +0.000 ms  control-no-change
  resume_ready      p50    0.001/0.001    p95    0.002/0.002    delta  -0.000 ms  control-no-change
  provider_dispatch p50    0.001/0.001    p95    0.002/0.002    delta  +0.000 ms  control-drift
  session_save      p50   66.555/47.611   p95  164.102/171.072  delta -18.944 ms  control-no-change
  total store_read_bytes p50 23615  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [22929, 22931, 22941, 22953, 23612, 23618, 23640, 23660, 24347, 24359]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [22931, 22933, 22949, 22957, 23630, 23632, 23658, 24349, 24351, 24355]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 22929..24359 (10 distinct values)
  SPREAD   b session_save store_read_bytes 22931..24355 (10 distinct values)
fixture events=1000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.102/0.102    p95    0.109/0.146    delta  +0.001 ms  control-no-change
  skill_request_overlay p50    0.781/0.779    p95    0.885/0.863    delta  -0.001 ms  control-no-change
  mcp_config_load   p50    0.940/0.946    p95    1.002/0.993    delta  +0.005 ms  control-no-change
  attach_history    p50  205.947/206.107  p95  220.144/209.566  delta  +0.160 ms  control-no-change
  resume_ready      p50  155.807/154.881  p95  167.307/157.816  delta  -0.925 ms  control-no-change
  provider_dispatch p50    1.534/1.538    p95    2.263/1.636    delta  +0.004 ms  control-no-change
  session_save      p50   38.445/67.018   p95   45.964/177.095  delta +28.573 ms  control-drift
  total store_read_bytes p50 2841142  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23647, 23649, 24335, 24366, 25053, 25072, 25741, 25756, 26451, 26462]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23663, 23671, 24321, 24358, 25026, 25070, 25725, 25729, 26426, 26452]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23647..26462 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23663..26452 (10 distinct values)
fixture events=1000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50   15.527/15.468   p95   18.768/17.021   delta  -0.059 ms  control-no-change
  skill_request_overlay p50   42.022/40.615   p95   48.031/45.098   delta  -1.407 ms  control-no-change
  mcp_config_load   p50    1.013/1.012    p95    1.098/1.104    delta  -0.002 ms  control-no-change
  attach_history    p50  203.305/204.596  p95  208.143/213.421  delta  +1.291 ms  control-no-change
  resume_ready      p50  155.189/154.415  p95  162.616/160.743  delta  -0.774 ms  control-no-change
  provider_dispatch p50    1.536/1.548    p95    1.840/1.990    delta  +0.013 ms  control-no-change
  session_save      p50   39.757/44.051   p95   45.435/144.915  delta  +4.294 ms  control-no-change
  total store_read_bytes p50 2841134  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23635, 23663, 24331, 24362, 25052, 25056, 25739, 25763, 26438, 26462]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23639, 23642, 24319, 24330, 25025, 25048, 25731, 25754, 26434, 26465]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23635..26462 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23639..26465 (10 distinct values)
fixture events=10000 skills=0 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50    0.099/0.098    p95    0.122/0.105    delta  -0.002 ms  control-no-change
  skill_request_overlay p50    0.779/0.757    p95    0.979/0.823    delta  -0.022 ms  control-no-change
  mcp_config_load   p50    0.935/0.933    p95    1.276/1.012    delta  -0.003 ms  control-no-change
  attach_history    p50 2188.091/2202.699 p95 2241.224/2228.971 delta +14.608 ms  control-no-change
  resume_ready      p50 1639.350/1633.845 p95 1648.391/1678.293 delta  -5.505 ms  control-no-change
  provider_dispatch p50   19.286/19.577   p95   19.889/20.576   delta  +0.291 ms  control-no-change
  session_save      p50   38.623/38.499   p95  216.703/48.126   delta  -0.124 ms  control-no-change
  total store_read_bytes p50 28640159  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 0  skill_bytes_read p50 0  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23658, 23674, 24327, 24333, 25056, 25068, 25759, 25779, 26426, 26474]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23638, 23674, 24353, 24377, 25011, 25044, 25756, 25763, 26434, 26447]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23658..26474 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23638..26447 (10 distinct values)
fixture events=10000 skills=200 body=0 mcp=0 mode=warm -> measured (fixture-stage counters stable: True, criterion identical)
  skill_catalog     p50   14.939/15.041   p95   16.680/15.734   delta  +0.102 ms  control-no-change
  skill_request_overlay p50   40.891/41.863   p95   50.506/52.536   delta  +0.972 ms  control-no-change
  mcp_config_load   p50    1.026/1.065    p95    1.144/1.166    delta  +0.040 ms  control-no-change
  attach_history    p50 2171.594/2180.637 p95 2208.250/2192.909 delta  +9.043 ms  control-no-change
  resume_ready      p50 1610.340/1609.501 p95 1618.371/1624.595 delta  -0.839 ms  control-no-change
  provider_dispatch p50   18.173/17.819   p95   19.259/19.788   delta  -0.354 ms  control-no-change
  session_save      p50   42.346/38.917   p95  110.871/75.029   delta  -3.428 ms  control-no-change
  total store_read_bytes p50 28640138  full_source_hashes p50 1  skill_dir_reads p50 1
  total skill_file_reads p50 200  skill_bytes_read p50 24690  mcp_config_reads p50 0  mcp_config_bytes p50 0
  DRIFT    a session_save {'store_read_bytes': [23636, 23638, 24355, 24375, 25015, 25066, 25748, 25753, 26427, 26451]} (outside the stability claim)
  DRIFT    b session_save {'store_read_bytes': [23650, 23670, 24365, 25064, 25066, 25731, 25745, 26440, 26460]} (outside the stability claim)
  SPREAD   a session_save store_read_bytes 23636..26451 (10 distinct values)
  SPREAD   b session_save store_read_bytes 23650..26460 (9 distinct values)
report   bench-results\loading-warm.json
mode warm, fixtures 6, measurement failures 0, unstable fixtures 0, verdict control-no-change
```

#### Tool latency

| Tool case | median | p95 | min | max | samples |
|---|---|---|---|---|---|
| read (tiny file) | 2.6 ms | 2.7 ms | 2.2 ms | 2.7 ms | 5 |
| read (1000 lines) | 2.7 ms | 2.7 ms | 2.6 ms | 2.7 ms | 5 |
| read (10 lines @ offset) | 2.7 ms | 2.7 ms | 2.1 ms | 2.7 ms | 5 |
| read (repo Cargo.toml) | 2.7 ms | 2.7 ms | 2.6 ms | 2.7 ms | 5 |
| write (small) | 10.4 ms | 12.4 ms | 10.3 ms | 12.4 ms | 5 |
| write (10KB) | 10.4 ms | 12.9 ms | 9.8 ms | 12.9 ms | 5 |
| edit (simple replace) | 10.3 ms | 20.1 ms | 9.8 ms | 20.1 ms | 5 |
| ls (scripts dir) | 2.7 ms | 2.7 ms | 2.7 ms | 2.7 ms | 5 |
| agentgrep (grep in tree) | 4.7 ms | 5.2 ms | 4.2 ms | 5.2 ms | 5 |
| agentgrep (find **/*.rs) | 5.2 ms | 5.2 ms | 4.7 ms | 5.2 ms | 5 |
| bash (true) | 7.8 ms | 7.8 ms | 7.3 ms | 7.8 ms | 5 |
| bash (echo hello) | 7.8 ms | 7.8 ms | 7.3 ms | 7.8 ms | 5 |
| bash (ls fixture tree) | 10.4 ms | 12.5 ms | 9.9 ms | 12.5 ms | 5 |
| invalid tool (dispatch only) | 2.6 ms | 2.6 ms | 2.1 ms | 2.6 ms | 5 |

#### Reload storm

1 resume(s) per level; each level resumes sessions no other level has opened, from a freshly started daemon.

| Concurrent resumes | history median | p95 | max | storm wall | failures |
|---|---|---|---|---|---|
| 1 | 198.3 ms | 198.3 ms | 198.3 ms | 198.4 ms | 0 |
| 10 | 531.1 ms | 532.0 ms | 532.0 ms | 533.9 ms | 0 |
| 40 | 2.03 s | 2.04 s | 2.04 s | 2.04 s | 0 |

#### Compile loop

| Checkpoint | Touched | median | min | max | samples |
|---|---|---|---|---|---|
| warm_noop | — | 792.9 ms | 792.9 ms | 792.9 ms | 1 |
| leaf_touch | src\main.rs | 1.02 s | 1.02 s | 1.02 s | 1 |
| core_touch | crates\app-core\src\lib.rs | 29.33 s | 29.33 s | 29.33 s | 1 |
| build_meta_touch | crates\build-meta\src\lib.rs | 40.08 s | 40.08 s | 40.08 s | 1 |

## TUI (`horon-tui`)

대화형 클라이언트는 별도 바이너리이고 따로 잽니다. 이 수치는 `tui` 묶음에서 나오며 위의 헤드리스 표와 비교하지 않습니다. `first_frame_ms` 와 `direct_first_frame_ms` 는 서로 다른 두 프로그램을 각각 띄운 값이고, 핸드오프는 그 둘의 차이입니다.

![TUI (`horon-tui`)](charts/tui-01-latency.ko.svg)

![TUI (`horon-tui`)](charts/tui-02-memory.ko.svg)

### Linux

#### TUI (`horon-tui`)

Measured version: the interactive client `horon-tui`. The headless `horon-agent` numbers are in the suites above and are never compared with these: the two binaries are timed against themselves.

| Metric | Median | Min–max | Samples |
| --- | --- | --- | --- |
| First frame (bare `horon-agent`) | 36.8 ms | 35.7 ms–176.4 ms | 5 |
| First frame (direct `horon-tui`) | 39.9 ms | 39.3 ms–41.3 ms | 5 |
| TTY handoff cost | -2.9 ms | -4.2 ms–135.1 ms | 5 |
| Key press to echo (p95) | 0.1 ms | 0.1 ms–0.9 ms | 50 |
| Idle RSS after 5 s | 5.4 MB | 5.4 MB–5.5 MB | 5 |
| RSS after one turn | 5.7 MB | 5.6 MB–5.8 MB | 5 |
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
| First frame (bare `horon-agent`) | 53.1 ms | 34.6 ms–98.9 ms | 5 |
| First frame (direct `horon-tui`) | 49.5 ms | 41.5 ms–68.2 ms | 5 |
| TTY handoff cost | 2.0 ms | -33.5 ms–49.4 ms | 5 |
| Key press to echo (p95) | 0.2 ms | 0.1 ms–0.4 ms | 50 |
| Idle RSS after 5 s | 8.9 MB | 8.9 MB–8.9 MB | 5 |
| RSS after one turn | 9.1 MB | 9.0 MB–9.1 MB | 5 |
| Frame draw time (p50) | 1.0 ms | 0.9 ms–1.0 ms | 5 |
| Frame draw time (p95) | 1.1 ms | 1.1 ms–1.1 ms | 5 |
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
| Frame draw time (p50) | 3.0 ms | 2.9 ms–3.1 ms | 5 |
| Frame draw time (p95) | 3.2 ms | 3.0 ms–3.3 ms | 5 |
| Install size (`horon-tui`) | 5.0 MB | — | 1 |

Handoff: median first-frame time of a bare `horon-agent` launch (no arguments, on a PTY) minus the median first-frame time of a direct `horon-tui` launch against an already-running daemon, both on the same machine and build. The bare path also starts the daemon, so the number covers that startup, the sibling lookup and the `exec`; it is the extra cost of the handoff over launching the client itself.

Render bench: 200 frames on a 120×40 terminal.

Skipped: pty unavailable on Windows

<!-- bench-auto:end -->
