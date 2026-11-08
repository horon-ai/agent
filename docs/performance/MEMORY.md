# 메모리

메모리 회귀 예산과 서버 메모리 사고 대응 절차를 다룬다.

## 메모리 회귀 예산

상태: 활성 가드레일
갱신: 2026-10-20

이 문서는 horon-agent의 현재 메모리 회귀 예산을 정한다.

실서버 진단과 원인별 조치 방법은
[HoronAgent Server Memory Incident Runbook](MEMORY.md)을 본다.

목표는 메모리 사용량을 영원히 고정하는 것이 아니다. 메모리 변경을 다음 조건에 맞추는 것이다.
- 측정 가능
- 검토 가능
- 의도가 설명됨

가능한 곳에서는 아래 예산을 추측한 RSS 수치가 아니라 코드베이스가 이미 노출하는 카운터와 상한에 묶는다.

### 지표 수집 방법

임시 계측을 새로 넣지 말고 기존 디버그 표면을 쓴다.

- 디버그 소켓을 통한 에이전트·세션 메모리 프로필: `agent:memory`
  (`crates/app-core/src/server/debug_command_exec.rs:401`)
- 서버 사고 빠른 분류: `server:memory-incident`
  (`crates/app-core/src/server/debug_server_state.rs:170`)
- 서버 전체 귀속: `server:memory`
  (`crates/app-core/src/server/debug_server_state.rs:184`)
- 프로세스 수명 타임라인: `python scripts/perf/analyze_memory_log.py --days 1`

코드의 주요 출처:
- `crates/base/src/runtime_memory_log.rs`
- `crates/app-core/src/server/debug_server_state.rs`

터미널 UI를 쓰던 `:debug memory`, `:debug markdown:memory` 같은 표면은 이 빌드에 없다.
headless 전환 때 TUI와 그 캐시가 함께 사라졌기 때문에, 그 캐시에 걸려 있던 상한도 이
문서에서 내렸다.

### 예산 모델

예산은 두 종류를 쓴다.

1. 하드 상한
- 캐시가 이미 강제하는 명시적 한도다.
- 여기서의 회귀는 코드가 상한을 바꿨거나 우회했다는 뜻이다.

2. 래칫 기대값
- 메모리 카운터 사이에 기대하는 관계다.
- 여기서의 회귀는 설명과 문서·테스트 갱신이 있을 때만 허용한다.

### 하드 상한

#### 런타임 메모리 로그 예산

출처: `crates/base/src/runtime_memory_log.rs`

로그 파일, 보류 이벤트 큐, 샘플링 간격은 코드가 직접 막는 값이다. 샘플링 간격
상한은 설정에서 더 작은 값을 줘도 그 아래로 내려가지 않는다 (`crates/base/src/runtime_memory_log.rs:482-495`, `:521-524`).

| 지표 | 예산 | 이유 |
|---|---:|---|
| 서버 로그 파일 수 | `<= 90` | `MAX_SERVER_LOG_FILES`. 넘으면 오래된 파일부터 지운다(`:19`, `:602-606`) |
| 클라이언트 로그 파일 수 | `<= 90` | `MAX_CLIENT_LOG_FILES`(`:28`) |
| 보류 이벤트 수 | `<= 64` | `MAX_PENDING_EVENTS`. 넘으면 오래된 이벤트를 버린다(`:31`, `:343-344`) |
| 보류 범주 수 | `<= 8` | `MAX_PENDING_CATEGORIES`(`:32`, `:364`) |
| 프로세스 샘플 간격 | `>= 15s` | `MIN_PROCESS_INTERVAL_SECS`(`:10`) |
| 귀속 샘플 간격 | `>= 60s` | `MIN_ATTRIBUTION_INTERVAL_SECS`(`:12`) |

위반했을 때 필요한 검토 조치:
- 한도를 바꾼 이유를 설명한다
- 이 문서를 갱신한다
- 로그 회전이 여전히 동작하는지 확인한다

#### Markdown 캐시 예산

headless 전환 이전에는 TUI의 마크다운·다이어그램 렌더러가 하이라이트·렌더·소스·이미지
상태 캐시에 명시적 상한을 두었다. 그 파일과 캐시는 현재 트리에 없으므로 지킬 상한도
없다. 대응하는 캐시를 다시 들이면 그때 상한과 근거를 이 절에 되살린다.

### 래칫 기대값

#### 세션과 트랜스크립트 메모리

출처: `crates/app-core/src/server/debug_server_state.rs:660-760`

아직 엄격한 상한은 아니지만 기대하는 관계는 있다. `agent:memory`와
`server:memory`가 세션별로 합산해 내보내는 값이다.

| 지표 관계 | 기대 |
|---|---|
| `session_message_count` vs `session_provider_cache_json_bytes` | 프로바이더 캐시는 트랜스크립트를 따라가되 따로 폭증하면 안 된다 |
| `session_provider_cache_json_bytes` vs `session_json_bytes` | 일반 채팅 흐름에서 같은 자릿수를 유지해야 한다 |
| `session_provider_cache_tool_result_bytes` vs `session_tool_result_bytes` | 캐시가 정본보다 큰 도구 결과를 들고 있으면 안 된다 |
| `session_provider_cache_large_blob_bytes` vs `session_large_blob_bytes` | 큰 블롭은 정본 쪽에만 남아야 한다 |
| `session_payload_text_bytes` | 큰 값은 설명이 필요하다. 보통 원시 도구 출력이 세션에 지나치게 오래 남는다는 뜻이다 |

위반했을 때 필요한 검토 조치:
- 변경 전후 메모리 프로필을 보여준다
- 어느 보존 경로가 커졌는지 설명한다
- 예산을 올리기 전에 중복을 먼저 고친다

#### 런타임 메모리 로그 기대값

출처: `crates/base/src/runtime_memory_log.rs`

런타임 메모리 로그는 디버그 기능이 아니라 회귀 감지 장치다.

기대하는 동작:
- 서버와 클라이언트 로그만으로 다음 항목의 큰 변화를 설명할 수 있어야 한다.
  - 세션·트랜스크립트 합계
  - 프로바이더 캐시 합계
  - 백그라운드 작업 합계
  - 사이드 패널 합계
- 새로 생긴 큰 메모리 소유자는 설명 없는 RSS 증가로만 나타나지 말고 귀속 가능한 신호를 내야 한다

위반했을 때 필요한 검토 조치:
- 메모리 증가를 받아들이기 전에 귀속 정보를 추가하거나 개선한다

### 메모리에 영향을 주는 변경의 검토 목록

메모리를 많이 쓰는 코드를 바꿀 때는 다음을 기록해 함께 낸다.

1. 어떤 카운터가 바뀌었나?
- 세션·프로바이더 캐시 동작이 바뀌면 `agent:memory`
- 서버 전체 귀속이 바뀌면 `server:memory`

2. 하드 상한을 바꿨나?
- 바꿨다면 예전 상한이 왜 부족했는지 설명한다

3. 중복이 늘었나?
- 정본 트랜스크립트
- 프로바이더 캐시
- 구체화된 프로바이더 뷰
- 표시용 사본
- 사이드 패널 사본

4. 관측 가능성이 충분했나?
- 메모리가 늘었다면 로그와 프로필로 어디서 늘었는지 설명할 수 있나?

### 현재 초기 예산 요약

오늘 코드가 실제로 강제하는 구체적 한도는 다음과 같다
(`crates/base/src/runtime_memory_log.rs`).

- 서버·클라이언트 로그 파일: 각각 90개
- 보류 이벤트: 64개, 보류 범주: 8개
- 프로세스 샘플 간격 하한: 15초, 귀속 샘플 간격 하한: 60초

이 한도를 의도적으로 바꾸면 같은 PR에서 이 문서도 갱신해야 한다.

## Idle session hibernation: 유휴 세션의 파생 캐시 해제

상태: 활성 동작
갱신: 2026-11-07

데몬은 턴 사이에도 세션을 상주시키므로, 유휴 세션 하나가 들고 있는 파생 캐시가 세션 수만큼
쌓인다. 이 절은 그중 원본에서 다시 만들 수 있는 부분만 언제, 무엇을, 어떤 조건에서 해제하는지
정한다. 코드는 `crates/app-core/src/agent/idle_hibernation.rs`(해제)와
`crates/app-core/src/server/idle_hibernation.rs`(10초 sweep)다.

### 설정

| 항목 | 기본값 | 다른 값 |
|---|---:|---|
| `[server] idle_hibernation_secs` | `1800` | `0`이면 sweep을 끈다. `HORON_AGENT_IDLE_HIBERNATION_SECS`가 프로세스 하나에 대해 덮어쓴다 |

sweep은 10초마다 임계값을 다시 읽으므로 설정 파일을 고치면 재시작 없이 적용된다. 임계값을 넘은
뒤 실제 해제는 최대 한 tick 늦고, 게이트에 걸려 거부된 세션은 매 tick이 아니라 온전한 임계값이
지난 뒤에 다시 시도한다. `0`이면 세션별 유휴 창 상태도 함께 비운다.

### 해제하는 것

`hibernate_idle_caches`는 다음 두 가지만 버린다.

- **provider projection**: `release_provider_messages_cache()`를 거쳐 `provider_messages_cache`,
  접두부 해시 캐시, 캐시 길이와 그 메모리 프로필 카운터를 비운다. 캐시 모드를 `Full`로 되돌리므로
  다음 `provider_messages()`는 빈 트랜스크립트를 보내지 않고 다시 만든다.
- **도구 출력 인덱스**: `tool_call_ids`와 `tool_result_ids`를 `clear()`가 아니라 새 `HashSet`으로
  바꿔 할당 자체를 놓고, `tool_output_scan_index`를 0으로 되돌린다.

### 남기는 것

해제는 파생 캐시에만 손대고 영속·정본 상태는 건드리지 않는다.

- 정본 트랜스크립트와 이벤트 원본(`events.jsonl`, `blobs/`), 상주 active context
- 얼린 시스템 프롬프트와 잠긴 도구 표면(`listed_tools_index`). 다음 턴이 도구 목록을 다시 협상하지
  않는다.
- `pruned_tool_results`(잘라내기 결정)와 `stream_rule_ledger`(스트림 규칙 원장), `event_state`의
  원본 보조 상태
- 되감기 undo 상태, provider 핸들, 대기 중인 알림과 재개 상태

해제 경로는 저장·체크포인트를 하지 않는다. 그래서 마이그레이션 리스가 건너뛰었을 저장을 이 경로가
가릴 수 없다.

### 게이트

sweep과 해제는 다음을 모두 만족할 때만 진행한다.

- 세션 로스터의 상태가 `running`이 아니고, 그 세션에 진행 중인 턴이 없다
  (`turn_cancel_registry`)
- 마이그레이션 리스가 없고, `pending_alerts`·`pending_resume`이 비었고, 소프트 인터럽트 큐 깊이가 0
- 압축 관리자를 `try_read`로 잡을 수 있고 압축 중이 아님
- 실행 중인 도구가 없고(`running_tool_registry`), active context가 이름 대는 도구 중 인플라이트인
  것이 없음. 백그라운드로 넘어간 도구가 남아 있으면 다음 턴의 복구가 결과를 중복 생성하므로 건드리지
  않는다.
- 마지막 활동(`session_metrics`)과 상태 변경 뒤 흐른 시간이 둘 다 임계값 이상. 기록된 활동이 없는
  세션은 활동 조건을 통과한 것으로 본다. 한 번 해제한 세션은 상태가 바뀌기 전까지 다시 해제하지
  않는다.

로스터 락, 세션 락, Agent 락 가운데 하나라도 잡히지 않으면 기다리지 않고 그 tick을 건너뛴다. sweep이
턴 앞에 큐로 서면 안 되기 때문이다. 해제가 하나라도 있으면 모든 락을 놓은 뒤
`release_retained_heap`을 한 번 부르고, 그 한 줄 로그에 세션 목록, 해제한 provider 캐시 바이트,
도구 인덱스 항목 수, 해제 전후 RSS를 남긴다.

### 깨어날 때

해제는 디스크 의존도, 수명 주기 훅도, 환경 재스냅샷도 만들지 않는다. 다음 턴이 상주 active
context에서 두 캐시를 다시 만들 뿐이고, 그 턴의 요청 본문은 해제 전과 같은 도구 쌍을 담는다
(`idle_hibernation_next_turn_sends_original_tool_pairs_without_duplicates`).

### 실제로 줄어드는 양

horon은 이미 요청 직전마다 provider projection을 놓는다(`turn_loops.rs`,
`turn_streaming_mpsc.rs`). 그래서 턴 사이에 남는 것은 대개 도구 인덱스 할당이고, 유휴 세션이 다시
들고 있던 projection(압축 점검이나 메모리 프로필 조회가 다시 구체화한 것)도 함께 사라진다. 즉
hibernation의 몫은 도구 인덱스 + 다시 만들어진 projection이며, `server:memory`의
`session_provider_cache_*`가 이미 0인 세션에서는 도구 인덱스 항목 수만 관찰된다. 두 값 모두 위 로그
한 줄과 `server:memory`로 확인한다.

## HoronAgent 서버 메모리 인시던트 런북

상태: 현재 운영 중인 런북
갱신: 2026-07-14

이 런북은 두 가지 질문에 답한다.

1. horon-agent 서버의 메모리를 무엇이 쓰고 있는가?
2. 그 원인에 대해 가장 안전한 다음 조치는 무엇인가?

목표는 RSS가 높다는 이유만으로 반응하는 것이 아니다. 무엇도 바꾸거나 멈추기 전에 살아 있는 애플리케이션 상태, 할당자가 붙들고 있는 메모리, 힙이 아닌 매핑을 구분하는 것이 목표다.

### 명령 하나로 하는 분류

다음을 실행한다.

```bash
horon-agent debug 'server:memory-incident'
```

인시던트 때 가장 먼저 실행하는 명령이다. 일부러 가볍게 만들었다. 모든 Agent 기록을 잠그거나 직렬화하지 않으므로 세션이 수천 개 떠 있어도 쓸 수 있다.

보고서에 들어가는 항목:

- RSS, PSS, 익명 PSS, 할당자 live 바이트
- 15분 동안의 PSS 증가
- live, headless, detached, connected 세션 수
- 상주 세션의 상태별 개수
- 상주 Agent가 가장 많은 스웜
- 심각도와 주 원인 분류
- 원인별로 정렬한 조치 목록

상태를 바꾸기 전에 JSON 출력을 인시던트 노트에 보관한다.

### 오프라인 타임라인 분석

런타임 메모리 로깅은 기본으로 켜져 있고 매일 JSONL 파일을 다음 경로에 쓴다.

```text
~/.horon-agent/logs/memory/
```

가장 최근 서버 프로세스 수명 구간을 분석한다.

```bash
python scripts/perf/analyze_memory_log.py --days 1
```

분석기는 기본으로 가장 최근 서버와 클라이언트 프로세스 인스턴스를 고른다. 서버를 다시 띄운 전후로 PSS를 비교하면 없는 급증이 생기기 때문에 이 동작이 중요하다. 인스턴스를 넘나드는 포렌식을 할 때만 `--all-instances`를 쓴다.

기록된 프로세스 수명 구간을 나열하고 재시작 전 인시던트를 직접 고른다.

```bash
python scripts/perf/analyze_memory_log.py --days 1 --list-instances
python scripts/perf/analyze_memory_log.py --days 1 --instance <server-instance-id>
```

사후 분석에는 `--instance`를 권한다. 메모리가 높은 서버와 그 뒤를 이은 메모리 낮은 서버가 섞이지 않고 하나의 프로세스 수명 구간으로 유지된다.

기계가 읽을 수 있는 출력:

```bash
python scripts/perf/analyze_memory_log.py --days 1 --json > /tmp/horon-agent-memory-analysis.json
```

### 심각도 기준값

내장 인시던트 보고서는 다음 운영 기준값으로 시작한다.

| 신호 | 경고 | 심각 |
|---|---:|---:|
| PSS | 1 GiB | 2 GiB |
| 15분 동안 PSS 증가 | 256 MiB | 1 GiB |
| 상주 Agent 세션 | 128 | 512 |

기준값을 넘으면 조사를 시작한다. 그 자체로 파괴적인 정리를 해도 된다는 뜻은 아니다.

### 판단 트리

#### 1. `runaway_live_session_population`

근거:

- live Agent 수가 많거나 빠르게 늘고 있다
- headless 또는 detached 세션이 붙어 있는 클라이언트보다 훨씬 많다
- 세션 수가 늘면서 할당자 live 바이트도 는다
- `top_live_swarms`에서 하나 이상의 스웜이 큰 비중을 차지한다

조치:

1. 세션을 만드는 쪽을 멈추거나 상한을 건다.
2. `horon-agent debug 'swarm:list'`를 실행하고 가장 큰 live 스웜을 살펴본다.
3. 소유 코디네이터에서 `swarm list`와 `swarm cleanup`으로 더 이상 필요 없는 워커를 제거한다.
4. 세션을 무작정 없애지 않는다. 진행 중인 작업을 버려도 되는지 먼저 확인한다.
5. `server:memory-incident`를 다시 실행한다. live 세션, 할당자 live 바이트, PSS가 함께 줄어야 한다.
6. 그 뒤에도 해제됐지만 붙들린 메모리가 높게 남을 때만 `allocator:purge`를 실행한다.

이유: 할당자 purge는 살아 있는 Agent 런타임을 해제하지 못한다.

#### 2. `allocator_retention`

근거:

- 할당자가 붙들고 있다고 추정하는 상주 메모리가 256 MiB 이상이다
- 붙들린 상주 메모리가 PSS의 25% 이상이다
- 할당자 live 바이트가 익명 PSS보다 뚜렷하게 낮다

조치:

```bash
horon-agent debug 'server:memory-incident' > /tmp/before.json
horon-agent debug 'allocator:purge'
horon-agent debug 'server:memory-incident' > /tmp/after.json
```

PSS가 크게 떨어지면 할당자가 붙들고 있던 메모리였다는 뜻이다. 다시 반복해서 늘어나면 메모리 예산을 올리지 말고 할당 churn과 할당자 감쇠를 살펴본다.

#### 3. `session_payload_growth`

근거:

- 추적 중인 기록, 제공자 캐시, 도구, blob 바이트가 할당자 live 메모리의 절반 이상을 설명한다
- `top_by_json_bytes`에서 하나 이상의 세션이 큰 비중을 차지한다

조치:

1. `horon-agent debug 'server:memory'`를 실행해 전체 귀속 내역을 본다.
2. 제공자 캐시, 도구 결과, 큰 blob, 페이로드 텍스트를 살펴본다.
3. 압축, 요약, 잘라내기, 또는 큰 산출물을 본문 밖으로 옮기기 중 하나를 한다.
4. 더 큰 안정 상태를 받아들이기 전에 하드 상한을 추가하거나 조인다.

#### 4. `unattributed_live_heap`

근거:

- 할당자 live 바이트가 1 GiB를 넘는다
- 세션 수와 할당자 보유량으로 힙을 설명할 수 없다
- live 힙 귀속 범위가 50% 아래로 남는다

조치:

1. `server:memory`와 런타임 로그 분석 결과를 확보한다.
2. 빠진 소유자가 뚜렷하면 그 소유자용 카운터를 추가한다.
3. 그래도 소유 관계가 불분명하면 `jemalloc-prof` 빌드를 쓴다.

```bash
horon-agent debug 'allocator:profile:on'
horon-agent debug 'allocator:profile:dump /tmp/horon-agent-server.heap'
```

일반 시스템 할당자 빌드는 할당 스택 프로필을 만들지 못한다. RSS만 보고 힙 소유 관계를 단정하지 않는다.

#### 5. `non_heap_or_mapping_growth`

근거:

- PSS는 높은데 할당자 live 바이트는 높지 않다
- 파일 기반, 공유 메모리, 스레드 스택 매핑이 늘고 있다

조치:

```bash
cat /proc/<server-pid>/smaps_rollup
pmap -x <server-pid> | sort -k3 -nr | head -40
ps -T -p <server-pid> -o pid,tid,%cpu,time,comm,wchan:32
```

모델 매핑, 공유 메모리, 스레드 생성, 할당자 밖의 큰 익명 매핑을 살펴본다.

### 에스컬레이션 단계

값싸고 믿을 만한 근거부터 쓴다.

1. `server:memory-incident`, 보통 1초 안에 끝나고 블로킹이 없다
2. 런타임 JSONL 분석기, 프로세스 수명 구간 추세와 인시던트 분류
3. `server:memory`, Agent별 귀속이라 비싸다
4. 할당자 purge A/B 시험, 보유 메모리일 때만
5. jemalloc 힙 프로필, 설명되지 않는 live 힙일 때만
6. OS 매핑과 CPU 프로파일러 상관 분석

### 인시던트 기록 필수 항목

다음을 저장한다.

- 서버 ID, 버전, git 해시, 가동 시간
- PSS, 익명 PSS, 할당자 live, 붙들린 상주 바이트
- live/headless/connected 세션 수
- 상위 live 스웜과 상태별 개수
- 15분 증가량
- 고른 조치와 조치 전후 측정값
- 진행 중인 작업을 보존했는지 여부

### 해결 판정 기준

다음 중 하나가 참일 때만 인시던트가 해결된 것으로 본다.

- 확인한 live 소유자가 줄고 그에 맞춰 PSS가 떨어졌다
- 할당자 purge로 보유 메모리임을 입증하고 재발 원인을 고쳤다
- 매핑 증가를 확인하고 상한을 걸었다
- 힙 프로필로 소유자를 찾고 회귀 테스트나 상한을 추가했다
- 높은 안정 상태가 의도한 것임을 입증하고 문서화하고 명시적 예산을 정했다

"재시작하니 메모리가 줄었다"는 말만으로 인시던트를 닫지 않는다. 재시작은 증거를 지우고 원인을 밝혀 주지 않는다.
