# 턴

언어: [English](TURNS.md) | **한국어**

턴 상태와 복구, 실행 중 메시지 삽입, 구조화된 런타임 명령, `--resume` 동작을 다룬다.

## 턴 복구와 턴 상태

이 문서는 한 턴이 예기치 않게 끝났을 때 에이전트가 무엇을 하는지, 그 판단 재료를 누가 들고 있는지를 설명한다. 코드는 `crates/agent-runtime/src/turn_recovery.rs`와 `crates/agent-runtime/src/turn_state.rs`에 있다.

### 판단 재료는 TurnState가 든다

`TurnState`는 에이전트 루프 밖에서 턴의 카운터와 결과를 소유한다. 필드는 열둘이다(`crates/agent-runtime/src/turn_state.rs:83-96`).

| 필드 | 세는 것 |
| --- | --- |
| `context_limit_retries` | 컨텍스트 한도로 다시 시도한 횟수 |
| `rate_limit_retries` | 레이트 리밋으로 다시 시도한 횟수 |
| `incomplete_continuations` | 불완전 응답을 이어 붙인 횟수 |
| `empty_post_tool_continuations` | 도구 결과 뒤 빈 응답을 이어 붙인 횟수 |
| `fable_guardrail_reconsiderations` | 가드레일 재고 횟수 |
| `sequential_single_tool_rounds` | 도구를 하나씩만 부른 라운드 수 |
| `batch_nudge_pending` | 배치 넛지를 기다리는 상태인지 |
| `consecutive_malformed_tool_rounds` | 연속으로 깨진 도구 라운드 수 |
| `unparsed_tool_markup_nudges` | 파싱되지 않은 도구 마크업을 알린 횟수 |
| `auto_continuations` | 자동 계속을 쓴 횟수 |
| `final_text` | 턴의 최종 텍스트 |
| `outcome` | 턴 결과(`TurnOutcome`) |

`TurnState`는 `Agent`를 참조하지 않고, 세션·클라이언트 이벤트·디스크를 만지지 않는다. 그래서 두 턴 루프(순차 실행 `crates/app-core/src/agent/turn_loops.rs`, 스트리밍 실행 `crates/app-core/src/agent/interrupts.rs`)가 같은 판정 함수를 공유한다.

### 예기치 않은 정지의 네 분류

`classify(stop_reason, final_text)`는 정지 이유와 최종 텍스트만 보고 네 갈래 중 하나를 고른다(`turn_recovery.rs:52`).

| 분류 | 뜻 |
| --- | --- |
| `CleanStop` | 턴이 정상적으로 끝났다. |
| `BudgetTruncated` | 프로바이더가 길이·토큰 한도로 멈췄다고 보고했다. |
| `UnclosedStructure` | 최종 텍스트가 코드 펜스나 목록 항목을 닫지 않았다. |
| `SemanticPrematureStop` | 마지막 문장이 "이제 하겠다"고 알린 뒤 아무 일도 없었다. |

### 복구 모드는 세 단계다

`RecoveryMode`가 어느 분류에 실제로 손을 댈지 정한다(`turn_recovery.rs:7-15`, `:40-49`).

| 모드 | 손대는 분류 |
| --- | --- |
| `off` | 없음 |
| `conservative` (기본값) | `BudgetTruncated`, `UnclosedStructure` |
| `aggressive` | 정상이 아닌 모든 분류(의미 추정 포함) |

기본값은 `conservative`다. 증명 가능한 잘림과 닫히지 않은 구조만 이어 붙이고, 의미 추정은 운영자가 `aggressive`를 고를 때만 쓴다.

### 의미 분류에 대한 두 번째 의견

`SemanticPrematureStop`은 분류기가 추측만 하는 유일한 분류다. 마지막 문장을 읽고 다음에 하겠다는 패턴에 맞춰 본다. `[autojudge] enabled = true`이면 턴이 `judge` 역할에 그 분류 하나만 묻는 `bool` 질문을 보낸다. 사용자가 요청한 일을 끝내기 전에 멈췄는지이고, 추측 대신 그 답으로 판단한다.

| 상황 | 동작 |
| --- | --- |
| `[autojudge] enabled = false`(기본값) | 요청하지 않는다. `RecoveryMode`가 이전과 똑같이 정한다. |
| 판단이 `probability >= 0.5`로 답함 | `off`를 뺀 모든 모드에서 그 분류에 따라 행동한다. |
| 판단이 `0.5` 미만으로 답함 | `aggressive`에서도 행동하지 않는다. |
| `judge` 모델을 해석하지 못함, 응답이 규격을 벗어남, 백엔드 실패, `[autojudge] attempt_timeout_ms` 초과 | 페일 오픈. `RecoveryMode`가 정하고 실패는 1분에 최대 한 번 기록한다. |
| `BudgetTruncated`, `UnclosedStructure` | 판단하지 않는다. 증명 가능한 분류라 판단은 결과를 바꾸지 못하고 요청만 쓴다. |

판단은 `judge` 역할의 모델로 실행하고(`docs/provider/MODEL.ko.md`) 시도는 한 번이며 내부 재시도가 없다. 예상 밖 종료 하나에 판단은 최대 한 번이므로 한 실행의 판단은 `MAX_AUTO_CONTINUATIONS + 1`을 넘지 않는다. 예산은 모델 선택, 요청, 파싱을 모두 포함한 시도 전체에 적용되며 네트워크 대기에만 걸리지 않는다.

`attempt_timeout_ms` 기본값은 3000이고 `0`은 기한을 없애는 대신 기본값을 뜻하며 30초를 넘는 값은 30초로 자른다. 기한 없는 값은 일부러 두지 않았다. 판단은 판정 대상 턴을 붙잡으므로 응답하지 않는 제공자에 턴이 멈추는 것이야말로 이 예산이 막으려는 실패다. 설정 키는 `[autojudge] attempt_timeout_ms`이고 `HORON_AGENT_AUTOJUDGE_ATTEMPT_TIMEOUT_MS`가 프로세스 하나에 한해 덮어쓴다.

`conservative`에서 `autojudge`를 켜면 그 모드가 행동하는 범위가 넓어진다. 판단이 확신한 의미상 조기 종료는 이제 이어지고, 추측만으로는 이어지지 않았다. 이것이 이 스위치의 요점이다. 보정된 답은 패턴 일치가 할 수 없는 곳에서 행동할 수 있고, 그래서 기본값이 꺼짐이다.

`/judge` 명령은 다른 작업이다. 스웜 안에서 검토 워커를 띄우고 세션 에이전트 모델 덮어쓰기로 모델을 고른다. 역할과 `[autojudge] model` 덮어쓰기를 함께 쓰지만 타입 있는 질문에는 답하지 않는다.

### 자동 계속은 턴당 두 번까지다

합성 계속은 `MAX_AUTO_CONTINUATIONS = 2`로 제한된다(`crates/agent-runtime/src/turn_recovery.rs:31`). `TurnState`는 이 값을 넘기지 않고(`crates/agent-runtime/src/turn_state.rs:215-219` `record_auto_continuation`이 상한으로 자른다), 새 턴은 `TurnState::new()`가 0에서 시작하므로 예산이 초기화된다(`crates/agent-runtime/src/turn_state.rs:82-100`, 테스트 `crates/agent-runtime/src/turn_state_tests.rs:18-26`). 한도를 다 쓰면 복구를 멈추고 사용자에게 알린다(`crates/app-core/src/agent/turn_recovery_wiring.rs:136`).

### 소프트 인터럽트는 재시작을 견딘다

턴 도중 들어온 메시지는 세션별 저장소에 남고, 세션이 다시 로드될 때 복원된다(`crates/base/src/soft_interrupt_store.rs`). 복원 지점은 `crates/app-core/src/agent/turn_execution.rs:1170`이고, 세션을 닫을 때 저장되는 경로는 `crates/app-core/src/agent_tests.rs:2016` 테스트가 고정한다. 주입 방식 자체는 [`docs/runtime/TURNS.ko.md`](TURNS.ko.md)에서 설명한다.

### 확인 방법

```bash
cargo test -p horon-agent-agent-runtime turn_recovery
cargo test -p horon-agent-agent-runtime turn_state
cargo test -p horon-agent-base soft_interrupt_store
```

### 웨이크는 대기열이 아니라 턴 인계를 기다린다

대기열에 들어간 소프트 인터럽트는 실행 중인 턴만 꺼낸다. 그래서 데몬 쪽 웨이크
(`crates/app-core/src/server/live_turn.rs`의 `run_live_system_turn_if_idle`)가 인계
중인 세션을 바쁜 세션으로 오인하면 안 된다. 끝난 턴은 `ForegroundGuard::close`가
모든 연결에 idle 상태를 넘기는 동안(`IDLE_FORWARD_BOUND`, 5초) 에이전트 잠금을 조금
더 쥔다. 그 턴은 이미 활성 실행으로 등록되어 있지 않으므로
`foreground_state::is_running`은 거짓이다. 웨이크는 이때 잠금을 `HANDOVER_WAIT`
(10초)까지 기다렸다가 턴을 시작한다. 메시지를 대기열로 강등해 세션이 무관한 턴을
기다리게 두지 않는다. 실제로 턴이 도는 중이면 예전처럼 즉시 `None`을 돌려주고
메시지는 대기열로 간다.

## 소프트 인터럽트: 끊김 없는 메시지 주입

### 개요

소프트 인터럽트는 이 빌드에 구현되어 있다. 진행 중인 턴에 메시지를 넣어도 현재 생성은 취소되지 않는다. 흐름을 끊고 다시 시작하는 대신 메시지를 대기열에 넣고, 에이전트 루프가 도구 실행을 마친 안전 지점에서 대화 이력에 붙인다. 구현은 세 조각으로 나뉜다. 프로토콜 요청 두 개(`crates/protocol/src/wire.rs:175-197`), 세션별 대기열(`crates/agent-runtime/src/lib.rs:18-175`), 그리고 턴 루프의 주입 지점(`crates/app-core/src/agent/interrupts.rs:536-669`)이다.

### 하드 인터럽트와의 차이

| 항목 | 하드 인터럽트(`cancel`) | 소프트 인터럽트 |
|--------|-------------------------|---------------------|
| 생성 취소 | 한다 | 하지 않는다 |
| 이미 실행한 도구 결과 | 버린다 | 유지한다 |
| 주입 시점 | 취소가 끝난 뒤 새 요청 | 다음 안전 지점 |
| 대기열 영속성 | 없음 | 세션별 파일에 남는다 |

하드 인터럽트는 여전히 필요하다. 사용자가 즉시 멈추기를 원하면 `cancel`을 쓴다. 소프트 인터럽트는 "멈추지 말고 이 말을 반영해 달라"는 요청에 쓴다.

### 대기열과 인플라이트 집합

`SoftInterruptQueue`는 `pending`과 `in_flight` 두 벡터를 하나의 잠금 아래에서 관리한다(`crates/agent-runtime/src/lib.rs:40-63`). `pending`은 아직 대화에 들어가지 않은 메시지고, `in_flight`는 이번 턴에 주입했지만 턴이 아직 끝나지 않은 메시지다.

- `transfer_to_in_flight`는 `pending`을 통째로 `in_flight`로 옮긴다. 두 벡터가 같은 잠금에서 바뀌므로 어느 쪽에도 없는 메시지는 생기지 않는다(`crates/agent-runtime/src/lib.rs:113-120`).
- `commit`은 턴이 성공했을 때 `in_flight`를 비운다(`:124-131`).
- `restore`는 실패하거나 취소되거나 패닉한 턴의 메시지를 `pending` 앞쪽으로 되돌린다(`:136-148`).

턴 하나는 `InFlightGuard`를 하나 만든다(`crates/app-core/src/agent/interrupts.rs:21-70`). 정상 종료나 복원 경로에서는 `disarm`이 걸려 드롭이 아무 일도 하지 않는다. 제공자나 도구 계층에서 패닉이 나면 무장된 가드가 드롭되면서 `restore`를 호출하고, 주입한 메시지가 사라지지 않고 다시 대기열로 돌아간다(`:58-70`).

### 주입 지점

주입 지점은 세 곳이다. 이벤트에는 지점 이름이 그대로 실린다(`crates/protocol/src/wire/events.rs:287-305`).

| 지점 | 위치 | 시점 | 용도 |
|-------|----------|--------|----------|
| **B** | 턴 완료 | 도구 요청 없음 | 기본: 남은 도구가 없어 순서 문제가 없다 |
| **C** | 도구 라운드 중간 | 긴급 중단 | 건너뛴 호출에 결과를 먼저 채운 뒤 주입한다 |
| **D** | 모든 도구 실행 후 | 다음 제공자 호출 전 | 기본값: 대기 중인 인터럽트를 여기서 모두 비운다 |

지점 판정은 `crates/app-core/src/agent/interrupts.rs:883`(B)와 `:894`(D)에 있고, C는 긴급 인터럽트가 있을 때만 열린다. 스트리밍 경로는 같은 지점을 `crates/app-core/src/agent/turn_streaming_mpsc.rs:1474`(B), `:1612`(D), `:2316`(C)에서 쓴다.

#### 전달 모드(`[messages]`)

`all`과 `one-at-a-time`은 턴이 아니라 지점마다 정해진다
(`crates/app-core/src/agent/interrupts.rs`). 턴의 첫 전달 지점은 앞 턴에서 남은
메시지를 가져가므로 `[messages] follow_up_mode`가 적용되고, 그 뒤 지점은 이번
턴에 도착한 메시지를 가져가므로 `[messages] steering_mode`가 적용된다. 모드를
개수로 옮기는 곳은 `crates/base/src/config/messages.rs`의 `batch_limit()` 하나뿐이고,
`transfer_prefix_to_in_flight`가 그 개수만큼 오래된 것부터 옮긴다.

두 모드의 기본값은 `all`이다. 지금까지의 동작이며 기본값이 `one-at-a-time`인
omp와 다르다. `one-at-a-time`으로 두면 남은 메시지는 순서대로 대기열에 남아
각각 따로 주입되므로, 대화 기록에는 메시지 하나가 한 단계로 남는다.

`[messages] interrupt_mode`는 대기 중인 메시지가 실행 중인 도구를 언제 멈추는지
정한다. `wait`(기본값, 지금까지의 시점)는 메시지 자체의 `urgent` 플래그를 따르므로
긴급한 메시지만 지점 C에서 회차를 끊는다. `immediate`는 대기 중인 모든 메시지를
긴급으로 취급하므로, 무언가 대기하는 즉시 회차를 끊는다.

세 값은 하네스 API의 `get_message_modes`로 확인한다. 세션 상태가 아니라 데몬
설정이므로 세션이 없어도 답한다.

#### 지점 C의 긴급 처리

긴급 플래그가 선 메시지가 대기 중이면 도구 라운드를 끝까지 돌리지 않는다. 판정 함수는 `urgent_interrupt_pending` 하나뿐이다(`crates/app-core/src/agent/interrupts.rs:163-166`). 라운드를 자를 때는 아직 결과를 쓰지 않은 호출마다 `is_error: true`인 도구 결과를 먼저 넣는다(`crates/app-core/src/agent/turn_streaming_mpsc.rs:2205-2227`). 내용은 `[Skipped: user interrupted]`다(`:12`). 그 뒤 사용자 메시지를 넣고, 건너뛴 개수를 이벤트의 `tools_skipped`로 알린다.

긴급하지 않은 인터럽트는 도구 사이에 넣지 않는다. 모두 지점 D로 미룬다(`crates/app-core/src/agent/turn_streaming_mpsc.rs:2104-2106`).

### 도구 호출과 결과의 짝

이 빌드의 모델 호출은 OpenAI 호환 Responses 경로 하나로 나간다. 그 요청 조립기는 호출과 결과의 순서를 스스로 지킨다. 각 도구 결과가 어느 메시지에 있는지 먼저 훑고(`crates/provider-openai/src/request.rs:184-193`), 결과가 아직 뒤에 있으면 그 출력을 미뤄 호출 순서를 보존하며(`:390-420`), 끝까지 결과가 없는 호출에는 합성 출력 `[Error] ...`를 넣는다(`:436-441`). 결과가 어느 호출에도 붙지 않으면 사용자 메시지로 바꿔 내보낸다(`:449-464`).

그래서 주입 규칙은 순서 위반을 만들지 않는다. 지점 D는 모든 결과가 이미 이력에 들어간 뒤이고, 지점 C는 건너뛴 호출의 결과를 먼저 채운 뒤다.

### API 표면

클라이언트는 두 요청을 보낸다.

```rust
// crates/protocol/src/wire.rs
SoftInterrupt {
    id: u64,
    content: String,
    images: Vec<(String, String)>,
    urgent: bool,
    client_id: Option<String>,
}

CancelSoftInterrupts {
    id: u64,
    client_ids: Option<Vec<String>>,
}
```

`client_id`는 주입 이벤트에 그대로 되돌아오고, 필터가 붙은 취소 요청에서 대상 지정에 쓰인다(`crates/protocol/src/wire.rs:175-197`).

서버는 세 가지 이벤트를 낸다.

- `soft_interrupt_injected`: 주입한 내용, 표시 역할, 지점, 건너뛴 도구 수, 합쳐진 호출자 id(`crates/protocol/src/wire/events.rs:287-305`).
- `soft_interrupts_cancelled`: 취소된 id와 대기열에 없던 id를 나눠 보고한다(`:308-316`).
- `soft_interrupt_queue`: 턴이 끝났는데 대기열에 남은 메시지 수. 턴의 종료 이벤트 바로 앞에 온다(`:332-334`).

### 영속성과 복원

대기열은 세션별 파일에 남는다. 경로는 `<에이전트 데이터 디렉터리>/pending-soft-interrupts/<session-id>.json`이다(`crates/base/src/soft_interrupt_store.rs:69-75`).

- 서버가 메시지를 대기열에 넣을 때 디스크에도 덧붙인다(`crates/app-core/src/server/state.rs:1288-1298`).
- 취소 요청은 `client_ids`에 해당하는 항목만 디스크에서 지운다(`crates/app-core/src/server/client_lifecycle.rs:3821`). 전체 취소는 파일을 비운다(`:3851`).
- 에이전트는 상태를 바꿀 때마다 스냅숏을 다시 쓴다(`crates/app-core/src/agent.rs:1369`, `:1401` → `crates/app-core/src/agent/interrupts.rs:181-200`).

세션을 복원하면 디스크의 대기열을 메모리로 읽어 들인다(`crates/app-core/src/agent/turn_execution.rs:1170` → `crates/app-core/src/agent/interrupts.rs:143-175`). 이 읽기는 파괴적이지 않다. 첫 턴이 성공적으로 커밋되기 전에는 디스크 사본을 지우지 않으므로, 재시작과 재시도에서도 메시지가 한 번만 주입된다.

### 취소된 턴

사용자가 턴을 취소하면 주입을 하지 않는다. `inject_soft_interrupts`는 정상 종료 신호가 선 경우 빈 목록을 돌려주고 메시지를 대기열에 그대로 둔다(`crates/app-core/src/agent/interrupts.rs:536-554`). 그래야 사용자가 이어서 친 다음 프롬프트가 답 없는 메시지로 이력에 남지 않고, 클라이언트가 그것을 다음 턴으로 보낼 수 있다.

### 스트림 규칙 주입과의 관계

`[features] stream_rules`가 켜져 있으면 스트리밍 중간에 규칙 위반을 잡아 같은 이벤트 채널로 메시지를 넣는다. 이 경로는 지점 이름 `S`를 쓴다(`crates/app-core/src/agent/stream_rules.rs:42`, `crates/app-core/src/agent/turn_streaming_mpsc.rs:738-780`). 사용자 입력 대기열과 별개이며, 켜지 않으면 매처 자체가 만들어지지 않는다(`crates/app-core/src/agent/stream_rules.rs:83-90`).

### 사용자 경험

데몬은 스스로 화면을 열지 않는다. 클라이언트가 쓸 수 있는 피드백은 프로토콜 이벤트 세 가지다.

기본 모드(`urgent = false`)에서 클라이언트가 `soft_interrupt`를 보내면 데몬은 메시지를
대기열에 넣는다. 주입되면 `soft_interrupt_injected`가 지점 이름과 함께 온다
(`crates/protocol/src/wire/events.rs:287-305`). 턴이 끝났는데 대기열에 메시지가
남아 있으면 `soft_interrupt_queue`가 남은 수를 알리고, 그 이벤트는 종료 이벤트 바로 앞에 온다
(`crates/app-core/src/server/live_turn.rs:199-201`). 남은 수가 0이면
이벤트를 보내지 않는다.

메시지는 출처별로 묶여 `\n\n`로 이어 붙는다. 같은 출처의 연속 메시지는 한 번의 주입으로
들어가고, 출처가 바뀌면 그 지점에서 묶음이 끊긴다(`crates/app-core/src/agent/interrupts.rs:596-640`).

긴급 모드(`urgent = true`)는 지점 C에서 남은 도구를 건너뛸 수 있다는 뜻이다
(`crates/protocol/src/wire.rs:183`). 건너뛴 호출에는 `is_error` 결과가 먼저
채워지고, 이벤트의 `tools_skipped`에 개수가 실린다. 이 처리는
`crates/app-core/src/agent/turn_streaming_mpsc.rs:2205-2227`에 있다.

### 예외 상황

1. **소프트 인터럽트가 여러 개인 경우**: 출처별로 묶고 `\n\n` 구분자로 이어 하나의 메시지로 만든다.
2. **텍스트만 응답하는 중의 소프트 인터럽트**: 지점 B에서 주입하고 루프를 계속한다.
3. **턴이 취소된 경우**: 주입하지 않고 메시지를 대기열에 남긴다. 클라이언트가 다음 턴으로 보낸다(`crates/app-core/src/agent/interrupts.rs:536-554`).
4. **턴이 실패하거나 패닉한 경우**: `InFlightGuard`가 주입한 메시지를 대기열 앞으로 되돌린다(`crates/app-core/src/agent/interrupts.rs:58-70`).
5. **대기열 잠금이 깨진 경우**: 주입을 건너뛰고 경고만 남긴다(`crates/app-core/src/agent/interrupts.rs:556-565`).

### 테스트

- `crates/agent-runtime/src/queue_tests.rs`: 대기열 이동, 커밋, 복원.
- `crates/app-core/src/agent/soft_interrupt_commit_tests.rs`: 커밋 전후 디스크 상태, 재시도, 취소, 패닉 복원.
- `crates/app-core/src/agent/turn_streaming_mpsc_interrupt_tests.rs`: 긴급 중단 시 건너뛴 호출의 결과 채우기와 이벤트.
- `crates/app-core/src/server/queue_tests.rs`: 서버 대기열과 디스크 사본의 동기.

## 구조화된 런타임 명령

하네스 API는 연결된 데몬의 안전 기능 프로브가 `runtime_commands`를 알릴 때만 그 기능을 알린다.
구형 데몬은 명시적인 미지원 오류를 돌려준다.
이것은 추가형 API이며 현재 마이너 버전은 15이고 Rust와 TypeScript SDK 모두에서 쓸 수 있다
(`crates/harness-api/src/lib.rs:53-55`).

```rust,no_run
use horon_agent_sdk::{HoronAgentClient, RuntimeCommand};
# fn example(client: &HoronAgentClient, session: &str) -> horon_agent_sdk::Result<()> {
let result = client.runtime_command(session, RuntimeCommand::Memory, vec!["status".into()])?;
println!("{}", result.message);
# Ok(()) }
```

```json
{"v":1,"id":7,"req":"runtime_command","session_id":"session-id","command":"memory","args":["status"]}
{"v":1,"reply_to":7,"ev":"runtime_command_result","session_id":"session-id","command":"memory","data":{"enabled":true},"message":"..."}
```

Rust 이벤트는 `RuntimeCommandResult { session_id, command, data, message }`
필드를 직접 쓴다. Rust SDK는 `RuntimeCommandOutput`을 돌려준다. TypeScript는
같은 필드를 가진 `runtimeCommand(sessionId, command, args)`를 노출한다. `data`는
명령별이며 추가형이다. `message`는 표시용으로 적합하다. 명령은
일반 에이전트 프롬프트로 변환되지 않는다. 상태는 SSH와 관리형 원격 런타임을 포함한
세션의 런타임 호스트에서 읽고 바꾼다.

### 사용 가능한 동작

- `skills`: 로드된 전역/프로젝트 오버레이 스킬, 설명, 경로, 활성
  스킬, 공유 승인 추천. 전역 레지스트리를 새로 고친다.
- `context`: 조립된 정적/동적 시스템 프롬프트, 유효/현재 도구,
  활성 대화 메시지, 컨텍스트 창 메타데이터. 대기 중인 메모리를 소비하거나 프로바이더를
  미리 데우거나 추론을 실행하지 않는다.
- `info`: 실제 프로바이더/세션 정체성과 토큰/캐시 카운터.
  캐시 만료/내용 검사는 쓸 수 없고 그렇게 명시한다.
- `memory`: 토글, 명시적 on/off/status, 또는 `extract` 예약. 추출
  응답은 완료가 아니라 예약을 뜻한다.
- `swarm`: 네이티브 멤버십 관리로 status/on/off/toggle.
- `agents [global] [swarm|review|judge|memory|ambient] [model|inherit]`: 기본값과
  세션 재정의를 읽거나 요청한 정책 범위를 유지한다.
- `subagent_model`: 생성된 워커 정책을 보기/설정/상속.
- `autoreview`, `autojudge`: 상태를 보여주고 명시적 on/off/toggle을 유지한다.
- `plan`: 계획 모드 상태를 읽고 바꾼다. 인자가 없거나 `status`면 현재 상태를,
  `on`/`off`는 계획 모드를 켜고 끄며, `approve`/`reject`는 세션에 대기 중인 계획을
  승인하거나 거절한다. 설정에서 계획 모드가 꺼져 있으면 `on`과 `approve`는 오류로 답한다
  (`crates/app-core/src/plan_mode.rs`).
- `transport`: 이름만 남아 있고 바꿀 것이 없다. `Provider` 트레이트에서 `transport`는 `None`,
  `set_transport`는 "This provider does not support transport switching" 오류, `available_transports`는
  빈 배열이 기본값이다(`crates/provider-core/src/lib.rs:437-447`). 이 트리의 구현은 그 기본값을
  그대로 쓴다(`crates/base/src/provider/mod.rs:1128-1147`). 그래서 인자 없이 부르면
  `{"transport": null, "provider": ...}`가 오고, `auto`·`https`·`websocket`을 넘기면 위 오류가 난다
  (`crates/app-core/src/server/runtime_commands.rs:200-210`).
  설정 파일과 환경변수의 `openai_transport`는 값을 받아 두기만 하고 어떤 프로바이더도 읽지 않는다
  (`crates/config-types/src/lib.rs:1341`, `crates/base/src/config/env_overrides.rs:476`).
- `fast`: 인자 없는 명령은 우선순위/off를 토글하고, 명시적 on/off/flex/status를 지원한다.
  적용할 수 없는 프로바이더는 실제 오류를 돌려준다.
- `refresh_model_list`: 실제 프로바이더 카탈로그 새로고침.
- `continue`, `resume_all`: 중단된 라이브 세션의 네이티브 이어가기.
  바쁜/완료/새 세션은 건드리지 않는다.
- `review`, `judge`, `subagent`: 세션/전역 모델 정책을 쓰는 실제 독립 swarm 워커 실행.
  구조화된 생성 세션 메타데이터를 돌려준다.
  `--model`과 `--type`을 지원한다. 폐기된 직접 subagent 도구는
  등록되지 않은 채로 남는다. `--continue`는 여기서 쓸 수 없고 명시적 오류를 보고한다.
- `overnight`: 네이티브 감독 하위 실행과 세션 범위 status/log/review/
  cancel 제어. `productivity`: 런타임 호스트의 실제 보고서와 이미지 페이로드.
  `initiatives`: 정직한 폐기 설명. 다시 켜지지 않는다.
- `remote`: 네이티브 게이트웨이 status/on/off/pair/revoke/help. 설정 토글은 리스너가 즉시 바뀐 척하지 않고
  재시작 필요를 보고한다.
- `server_reload`: 더 새 바이너리가 있을 때만 강제하지 않는 상태 보존 업그레이드를 요청한다.
  첫 응답은 완료가 아니라 요청 접수다.
- `auth`, `logout`: 런타임 호스트 인증 검사와 명시적으로 확인한
  프로바이더 자격 증명 제거. 지원되는 공유 인증 저장소가 대상이다.

위 이름들은 런타임 명령 열거형이 정한다
(`crates/harness-api/src/runtime_commands.rs:7-33`).
열거형에 없는 이름을 보내면 오류로 답한다.
`cache`, `cache stats`, `cache extend`, `cache 1h`, `cache 5m`은 이 열거형에 없다.
캐시 TTL을 늘리는 런타임 명령도, 프리미엄 모드를 바꾸는 클라이언트 요청도 이 빌드에 없다.

`transfer_session`은 별도 수명주기 작업이며
`transferSession` / `transfer_session`으로 노출된다. 실제 런타임 압축 하위를 만들고
`SessionForked`를 응답한다. 호출자를 하위에 붙이지는 않는다.

### 제한과 배포

대부분의 에이전트 기반 읽기/변경은 활성 모델 턴 동안 임의로 긴 생성 뒤에서 기다리지 않고
바쁨 오류를 돌려준다. 프로바이더별
설정은 지원되지 않으면 오류를 돌려준다. 클라우드 UI 설정, 캐시 제거/예열,
폐기된 이니셔티브 추적은 동작한다고 거짓 광고하지 않는다. 캐시 TTL을 늘리는
런타임 명령도 없다. 프로바이더가 캐시를 보고하면 `info`의 카운터로만 보인다.

새 SDK나 브리지는 실행 중인 데몬을 업그레이드하지 않는다. 런타임과
브리지를 빌드하고 새 런타임 후보를 설치한 뒤 기존의 우아한 서버
업그레이드 워크플로를 쓴다. 세션 상태를 보존하고 라이브 데몬이나 하네스
브리지를 죽이지 않는다. 양쪽이 새 코드를 실행할 때까지 그 기능은 쓸 수 없다.

## 모델이 요청하는 컨텍스트 전환

`context_notes`는 모델에게 작은 메모 파일을 준다. `new_context`는 그 메모만으로 충분하다고
모델이 판단해 다음 턴을 깨끗한 활성 컨텍스트에서 시작하게 한다. 이 도구는
`[tools] context_notes`가 켜져 있을 때만 등록된다
(`crates/app-core/src/tool/mod.rs:1095`). 전환이 유지하는 것이 바로 동적 시스템
프롬프트가 싣는 메모이기 때문이다.

호출 자체는 세션별 대기 요청만 기록한다(`crates/app-core/src/tool/new_context.rs`).
그래서 한 턴에 두 번 불러도 전환은 하나만 대기한다. 도구 라운드는 자기 컨텍스트를
다시 만들 수 없다. `new_context`를 호출한 어시스턴트 메시지와 아직 나오지 않은 도구
결과가 같은 턴에 속하므로, 그 시점에 경계를 옮기면 둘이 고아가 된다.

두 턴 루프 모두 턴의 대화록이 확정된 뒤, 두 루프가 함께 쓰는 턴 정리 단계에서
요청을 적용한다(`crates/app-core/src/agent/interrupts.rs:364`).
적용은 `Agent::apply_pending_context_rollover`
(`crates/app-core/src/agent/compaction.rs:49`)가 한다. 가장 최근 사용자 턴 앞의 모든
메시지를 덮는 압축 경계를 기록한다. 요약은 새로 만들지 않는다. 경계에는 한 줄 표지가
들어가고(`compaction.rs:9`), 세션에 이미 있던 텍스트 요약은 그 앞에 남는다. 프로바이더
네이티브 창은 대신 버린다. 경계를 그 앞으로 옮기면 서버가 이전 범위에 맞춰 만든
페이로드를 다시 보내게 되기 때문이다. 그래서 다음 요청은 동적 시스템 프롬프트(작업 메모
포함), 그 사용자 메시지, `history://current`를 가리키는 표지로 구성된다. 삭제되는 것은
없다. `history://current`는 전환이 걸러낸 도구 결과까지 포함한 전체 대화록을 그대로
보여준다.

경계는 도구 결과 메시지에 놓이지 않는다(`compaction.rs:17`). 결과를 만든 호출이 경계
앞에 있으므로, 결과에 경계를 두면 고아 쌍으로 재생되고 세션 투영이 그 대화록을
거부한다.

도구는 쓰기 효과를 선언하므로 플랜 모드는 다른 변경과 똑같이 이를 거부하고, 승인
절차도 변경으로 다룬다.

테스트는 `crates/app-core/src/agent/context_rollover_tests.rs`가 재구성된 요청, 저장된
대화록, 요청당 전환 하나를 다루고, `crates/app-core/src/tool/new_context_tests.rs`가
플랜 모드 거부, 등록 조건, 턴당 대기 요청 하나를 다룬다.

## prewalk 인계

`[prewalk]`는 세션마다 한 번, 계획에서 실행으로 넘어갈 때 모델을 바꾼다. 계획하는 동안은 계획 모델로 돌고, 계획이 생기면 실행 모델로 이어 간다. 계획 준수 품질을 비용과 맞바꾸므로 이 절은 기본으로 꺼져 있다.

```toml
[prewalk]
enabled = true
into = "exec-model"
from = "plan-model"
```

| 키 | 뜻 |
| --- | --- |
| `enabled` | 세션이 인계를 하는지. 기본값은 `false`다. |
| `into` | 실행 모델. 모델 식별자 또는 `role:<이름>`. `enabled = true`이면 필수이고, 쓸 수 있는 값이 없는 절은 `prewalk.into`를 지목하는 오류로 설정 검증에 실패한다. |
| `from` | 적용 조건일 뿐이다. 설정하면 세션이 지금 이 모델로 돌 때만 인계한다. 돌아갈 모델을 뜻하지 않으며, 생략하면 모든 세션에 적용한다. |

바꾸는 지점은 플랜 모드에 달렸다. `[tools] plan_mode`가 꺼져 있으면 항목을 하나 이상 남긴 첫 `todo` 쓰기이고, 켜져 있으면 계획 승인이다. 계획 중에 쓴 todo 목록이 계획 밑에서 모델을 바꾸지 않게 하려는 것이다. 검사는 턴 시작에 돌아서 턴 사이에 들어온 승인도 잡는다.

모델 교체는 `/model`과 같은 경로를 써서 `session.model`에 실행 모델이 저장되고, 재개나 복원 뒤의 요청도 모두 그 모델로 간다. 판정은 저장된 세션의 재생 이벤트 `prewalk` 하나이고, 이 이벤트가 인계를 프로세스를 넘어 한 번만 일어나게 한다. 이미 이벤트가 있는 세션은 다시 판정하지 않는다. 거부된 `into`도 같은 방식으로 기록한다. 알 수 없는 모델, `default`·`plan`·`aux` 밖의 `role:` 이름, 아무것도 가리키지 않는 역할이 그렇다. 세션은 지금 모델을 유지하고 경고를 한 번 남기며, 다음 라운드에서 되풀이하지 않는다. 세션이 이미 쓰는 모델로 풀리는 `into`는 바꾸지 않고 판정만 기록한다.

표와 검증은 `crates/config-types/src/prewalk.rs`, 판정은 `crates/app-core/src/agent/prewalk.rs`, 수용 사례는 `crates/app-core/src/agent/prewalk_tests.rs`에 있다. `role:<이름>`이 활성 프로파일에서 풀리는 방식은 [provider/MODEL.ko.md](../provider/MODEL.ko.md)에 있다.

## 목표의 토큰 예산

목표는 실행을 이어 간다. 턴이 끝나면 계속 훅이 붙은 목표를 읽고, 상태가 `active`인
동안 합성 메시지를 하나 저장하고 다음 턴을 요청한다. 이 반복은
`GOAL_CONTINUATION_CAP`까지다. 목표는 그 반복을 멈출 `budget_tokens`, 즉 세션 전체가
쓸 수 있는 토큰 상한을 선택적으로 가질 수 있다.

판정은 `horon_agent_agent_runtime::goal_continuation`에 있고 순수 함수다. 목표 상태,
대기 중인 입력, 이미 쓴 계속 횟수, 지금까지 쓴 토큰, 예산을 읽는다. 먼저 맞는 조건이
이기고, 예산은 상한보다 먼저 검사한다. 돈이 떨어져 멈춘 목표는 `reason=cap`이 아니라
`reason=budget`으로 남아야 하기 때문이다.

쓴 토큰은 데몬이 이미 보고하는 값과 같은 집계다. 대화 전체의 `TokenUsageTotals`에
캐시 읽기와 캐시 쓰기를 더한다. 공급자는 캐시 입력을 싸게 청구하지만, 그 값을 빼고
계산하면 캐시를 많이 쓰는 목표가 상한을 훨씬 넘겨 달린다.

합계는 목표를 붙인 시점이 아니라 대화 전체를 덮는다. 그래서 이미 예산보다 많이 쓴
세션에 목표를 붙이면 다음 턴 끝에서 바로 랩업한다. 세션에 건 상한을 그대로 읽은
결과다. 붙인 시점에서 다시 세면 같은 세션이 상한을 두 번, 즉 목표가 생기기 전과
후에 한 번씩 쓰게 된다.

예산에 닿았다고 실행이 즉시 끊기지는 않는다. 다른 메시지
(`GOAL_BUDGET_WRAP_UP_MESSAGE`)로 턴 하나를 더 주고, 그 메시지는 새 일을 시작하지 말고
해 온 일을 기록하라고 지시한다. 같은 시점에 목표는 `budget_limited`로 옮겨 간다. 순서가
중요하다. 상태를 먼저 쓰고 턴을 요청하므로, 둘 사이에서 죽으면 남는 것은 멈추는
목표이지 계속 쓰는 목표가 아니다. 그다음 턴 끝은 `active`가 아닌 상태를 보고 추가
턴 없이 멈춘다.

`budget_limited`를 `paused`와 따로 두는 이유는 자동으로 재개되지 않기 때문이다. 재개는
운영자의 결정이고 `goal` 도구로 한다. 예산도 거기서 정한다. `create`와 `update`가
`budget_tokens`를 받고 0은 무제한으로 읽지 않고 거부한다. 무제한은 이미 `None`이 뜻하는
값이고, 0이 판정에 닿으면 첫 턴 끝에서 목표를 랩업해 버려서 정책이 아니라 결함처럼
보이기 때문이다.

목표를 되살리는 것은 운영자의 결정이고 두 동작이 필요하다. `budget_tokens`를 올리고
상태를 `active`로 되돌린다. `resume`만으로는 집히지 않는다. 예산을 다 쓴 목표가 스스로
재개하면 다시 쓰게 되기 때문이다.

시험: `crates/agent-runtime/src/goal_continuation.rs`가 판정 표를 덮는다. 예산이 상한을
이기고 사용자 입력이 둘 다를 이기는 경우가 포함된다.
`crates/app-core/src/agent/goal_continuation_tests.rs`는 저장되는 랩업 메시지, 상태 전이,
다음 턴의 정지, 예산 없는 목표가 랩업되지 않음, 0 예산 거부, 왕복 저장과 옛 파일 적재를
덮는다.

## 핸드오프 압축

핸드오프 압축은 모델 요약 대신 생성한 핸드오프 문서를 경계로 쓴다. 이 문서는 세션이
무엇을 하던 중이었는지, 무엇을 결정했는지, 무엇이 남았는지를 적어 두므로 다음 세션이
그 문서에서 이어 갈 수 있다(`crates/app-core/src/agent/handoff_compaction.rs`).

### 트리거

세 곳에서 요청할 수 있다. 하네스 API는 기존 `compact` 요청에 `method: "handoff"`를
붙여 받고, 응답은 접수 확인이 아니라 결과다. 야간 감독자는 핸드오프 준비 시점에 한 번
실행하며, 그 시점은 목표 기상 시각의 30분 전이거나 실행 시간의 4분의 1 전이다. 그 뒤의
조정자 턴은 밤새 쌓인 대화록 대신 이 문서를 읽는다. 이 빌드의 ACP에는 압축 명령이
없으므로 ACP 클라이언트는 트리거할 수 없다.

### 토큰 예산 꼬리

최근 토큰 예산(`[compaction] recent_tokens`, 기본 20,000)에 드는 메시지는 원문 그대로
남는다. 문서는 그 꼬리 앞의 모든 메시지를 덮고, 결과는 덮은 메시지 수를 보고한다.
모든 메시지가 예산 안에 들어가면 덮을 것이 없으므로 빈 문서를 쓰지 않고 그 사실을
보고한다.

### 문서는 Goal 절만 확인한다

생성은 완성 요청 한 번이다. 경계가 되기 전에 두 가지 응답을 거부한다. 텍스트가 없는 응답과
`Goal` 제목이 없는 응답이다(`# Goal`, `### goal`, `## Goal:` 모두 인정한다). 둘 다 `detail`에
이유를 담아 `provider_error`로 보고하며 아무것도 커밋하지 않는다. 그 밖의 제목이 빠지거나
절이 생략돼도 실패가 아니다. 텍스트를 모델이 쓴 그대로 커밋하고, 경계는 그 문서와 원문
꼬리를 함께 유지한다.

### 전략으로 실행할 때

`[compaction] strategy = "handoff"`를 지정하면 평소의 자동·수동 압축이 요약 대신
핸드오프 문서를 쓴다. 다른 전략과 같은 잡을 쓰므로 경계 계산, 기한, 취소, 단일 슬롯
규칙이 그대로다. 결과에는 핸드오프 표시가 붙고, 다음 요청은 그 문서를 남의 노트가 아니라
같은 에이전트가 앞서 쓴 글로 소개한다(`## Handoff From Your Earlier Self`, "continue the
work; do not summarize it back"). 이전 경계는 프롬프트로 넘어가므로 한 세션에서 문서를
두 번 쓰면 첫 문서가 남긴 목표와 제약이 유지된다.

프롬프트가 덮을 범위 전체를 담지 못하면 프롬프트를 자르는 대신 범위를 줄인다. 범위를
조용히 줄여 쓴 문서는 읽지 않은 메시지 너머로 경계를 옮기기 때문이다. 가장 작은
범위조차 들어가지 않으면 문서를 거부하고 같은 범위의 구조화 요약으로 대체한다. 어느
쪽이든 그 턴은 압축된다.

### 실패와 취소

프로바이더 오류나 취소는 아무것도 커밋하지 않는다. 메시지 수, 대화록, 이전 경계가 모두
그대로다. 두 경우 모두 호출자에게 결과로 보고하고, 야간 실행은 이를 기록한 뒤 남은 턴을
계속한다.

테스트: `crates/app-core/src/agent/handoff_compaction_tests.rs`가 커밋된 경계, 꼬리 밖
범위, 프로바이더 오류, 취소를 다룬다.
`crates/app-core/src/overnight_handoff_tests.rs`는 감독자 실행과 실패 경로를,
`crates/harness-api-server/src/translate_handoff_tests.rs`는 요청 필드와 결과 보고를
다룬다.

### 핸드오프 뒤의 복원 정보

문서는 산문이라 후임자는 덮인 작업이 어떤 파일에 기대었는지 알 수 없다. 그래서 모든
핸드오프 경계는 문서 옆에 복원 매니페스트를 함께 담는다. 구조화 요약이 저장하는 것과
같은 형태로, 최대 `MAX_RESTORATION_ENTRIES`(10)개 경로에 각 경로의 종류(`file` 또는
`skill`)와 그 경로를 만진 도구를 기록한다. 허용량은 컨텍스트 윈도의 15%이고
`MAX_RESTORATION_BYTES`(50,000)를 넘지 않는다.

매니페스트는 모델의 응답이 아니라 덮인 메시지에서 수집한다. 문서가 빠뜨린 파일도
매니페스트에는 남는다. `StoredSummaryMetadata.restoration`에 기록하므로 재시작을
견디고, 압축된 이벤트 페이로드에도 구조화 요약과 같은 방식으로 실린다. 목록이 비면 빈
매니페스트가 아니라 없는 것으로 저장한다.

테스트: `crates/base/src/compaction/handoff_tests.rs`가 수집된 항목과 빈 경우를,
`crates/app-core/src/agent/handoff_compaction_tests.rs`가 커밋된 경계와 이벤트
그래프의 매니페스트를 다룬다.

### 여러 방법을 순서대로: `method_order`

`strategy`는 방법 하나를 고른다. `[compaction] method_order`는 사슬을 고르므로, 이번 턴의
경계를 만들지 못한 방법이 턴을 압축되지 않은 채로 두지 않고 같은 잡을 다음 방법으로
넘긴다.

```toml
[compaction]
method_order = ["handoff", "structured"]
```

트리거, 경계 계산, 기한, 단일 슬롯, 취소 규칙은 잡의 것이고 바뀌지 않는다. 경계를 만드는
주체만 목록을 따라 내려간다. 남는 경계에는 그것을 쓴 방법의 표시가 붙으므로, 구조화 요약
대체물이 같은 에이전트가 앞서 쓴 글로 소개되는 일은 없다. 일시적 프로바이더 오류(속도
제한, 5xx, 타임아웃, 끊긴 연결)는 같은 방법에서 `MAX_TRANSIENT_RETRIES`까지 다시 시도한다.
넘침(overflow)은 일시적 오류가 아니며 덮을 범위를 줄여 처리한다.

해석은 설정을 읽을 때가 아니라 잡을 시작할 때 하고, 잘못된 항목이 재로드를 실패시키지
않도록 관대하다(`crates/base/src/compaction/method_order.rs`).

- 목록이 없거나 비면 `strategy`를 그대로 재현한다. `handoff`는 구조화 요약 대체 하나를
  그대로 유지하고, 네이티브 전략은 우리 방법을 하나도 실행하지 않는다.
- `structured`로 끝나지 않는 목록은 통째로 무시한다. 마지막 방법만이 다음 대체가 없다.
- 프로바이더 네이티브 이름(`openai_auto`, `openai_explicit`)이 섞이면 목록도 폐기한다.
  네이티브 압축이 대화록을 단독으로 소유하기 때문이다.
- 알 수 없는 이름은 그 항목만 버리고 나머지 목록은 유지한다.

한 방법이 연속 두 번 실패하면 이후 잡에서 건너뛰며, 경계가 커밋되면 모든 횟수가 초기화된다.
한 번이 아니라 두 번인 이유는 프로바이더의 일시적 흔들림 하나로 방법을 퇴역시키지 않기
위해서다. 모든 방법이 퇴역하면 전체 사슬을 다시 쓰는데, 방법이 하나도 없는 잡은 압축을
할 수 없고 압축 중단 여부는 이미 회로 차단기가 결정하기 때문이다.

`tool_result_aging`은 일부러 사슬 밖에 둔다. 어떤 요약보다 먼저 실행되고, 턴 순서가 아니라
캐시 온도와 임계 근접도로 발동하므로 "대화록이 이미 바뀌었다"는 효과를 다른 방법과의
상대 위치로 표현할 수 없다.

테스트: `crates/base/src/compaction/method_order_tests.rs`가 해석, 퇴역 예산, 사슬 실행을
다룬다.

### horon이 이미 구현한 세 가지 개념

이 저장소 밖에서는 압축에 재작성 방지, 다중 신호 트리거, 최신 관측 규칙이 필요하다고
설명하기도 합니다. 셋 다 새 코드 없이 여기에 있습니다.

- **한 번 바꾼 바이트는 다시 바꾸지 않습니다.** 축약된 결과는 `AGED_MARKER_PREFIX`를
  달고 다음 차례에 건너뜁니다(`crates/app-core/src/agent/tool_result_aging.rs`). 가지치기한
  도구 호출은 원장에 남겨 두 번 계획하지 않습니다(`crates/compaction-core/src/pruning.rs`).
- **축약에는 질량, 이득, 주기가 모두 필요합니다.** 압박 비율(`AGING_PRESSURE_RATIO`,
  임계의 0.75)과 최소 묶음 크기(`AGING_MIN_PRESSURE_BATCH_BYTES`, 64 KiB)를 넘고, 같은
  구간을 지날 때 한 번만 실행합니다.
- **대상마다 최신 관측만 남습니다.** 가지치기는 `newest_by_key`와
  `newest_complete_by_path`를 유지하므로, 같은 경로의 부분 읽기·실패 호출·나중 전체 읽기는
  지금도 유효한 한 항목으로 정리됩니다.

시험: `crates/app-core/src/agent/tool_result_aging/tests.rs`가 접두어 방지와 묶음 게이트를,
`crates/compaction-core/src/pruning.rs`가 최신 선택을 다룹니다.

## `--resume` 동작

이 빌드에는 대화형 세션 선택기가 없다. 세션 재개는 전역 `--resume` 플래그 하나로 한다
(`src/cli/args.rs:63-68`). 값을 주지 않고 `--resume`만 넘기면 데몬은 시작하지 않고 오류로
끝난다. 문구는 "the interactive session picker is unavailable in this headless build; pass an
explicit session id to --resume"다(`src/cli/dispatch.rs:97-101`).

### 기본 동작

- `--resume <id>`는 세션 id를 받아 그 세션으로 시작한다. 값이 전체 id가 아니면 로컬 저장소에서
  먼저 해석한다(`src/cli/dispatch.rs:696-736`).
- 해석에 실패하면 원문 id를 그대로 서버로 넘겨 서버가 다시 해석하게 한다
  (`src/cli/dispatch.rs:721`).
- `--resume`은 다른 전역 플래그와 함께 쓸 수 있다. 세션은 이 프로세스에서 그대로
  재개되고, 그것을 어떤 화면에 보여 줄지는 연결한 클라이언트가 정한다.

### 재개한 세션의 표시

이 빌드는 데몬이나 `run` 프로세스 안에서 세션을 재개하고 터미널 창을 열지 않는다.
어떤 창이나 페인으로 보여 줄지는 클라이언트 몫이다. 함께 배포되는 터미널 클라이언트는
`--session <id>`로 붙고, 인자 없는 `horon` 인계는 `--resume`을 그 플래그로 넘긴다.
예전 TUI가 쓰던 세션 선택기와 새 터미널·멀티플렉서 페인에서 여는 실행기는 이 트리에 없다. 그래서 옛 config 파일에
`session_picker_enter`나 `[terminal]` 키가 남아 있어도 읽지 않고, 예전 `--fresh-spawn`처럼
창을 고르는 플래그도 없다. `crates/terminal-launch`에는 클라이언트 터미널 환경 스냅숏과
훅 명령 파싱만 남아 있다.

### 저장한 세션

`/save`, `/unsave`는 클라이언트의 슬래시 명령이며, 함께 배포되는 클라이언트에는 이 명령이 없다. 세션에 붙는
저장 레이블은 하네스 API 응답의 `save_label` 필드로 남아 있다
(`crates/harness-api-server/src/translate.rs:255`, `:883` 참고).
