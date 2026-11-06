# 턴

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

### 자동 계속은 턴당 두 번까지다

합성 계속은 `MAX_AUTO_CONTINUATIONS = 2`로 제한된다(`crates/agent-runtime/src/turn_recovery.rs:31`). `TurnState`는 이 값을 넘기지 않고(`crates/agent-runtime/src/turn_state.rs:215-219` `record_auto_continuation`이 상한으로 자른다), 새 턴은 `TurnState::new()`가 0에서 시작하므로 예산이 초기화된다(`crates/agent-runtime/src/turn_state.rs:82-100`, 테스트 `crates/agent-runtime/src/turn_state_tests.rs:18-26`). 한도를 다 쓰면 복구를 멈추고 사용자에게 알린다(`crates/app-core/src/agent/turn_recovery_wiring.rs:136`).

### 소프트 인터럽트는 재시작을 견딘다

턴 도중 들어온 메시지는 세션별 저장소에 남고, 세션이 다시 로드될 때 복원된다(`crates/base/src/soft_interrupt_store.rs`). 복원 지점은 `crates/app-core/src/agent/turn_execution.rs:1170`이고, 세션을 닫을 때 저장되는 경로는 `crates/app-core/src/agent_tests.rs:2016` 테스트가 고정한다. 주입 방식 자체는 [`docs/runtime/TURNS.md`](TURNS.md)에서 설명한다.

### 확인 방법

```bash
cargo test -p horon-agent-agent-runtime turn_recovery
cargo test -p horon-agent-agent-runtime turn_state
cargo test -p horon-agent-base soft_interrupt_store
```

## 소프트 인터럽트: 끊김 없는 메시지 주입

### 개요

소프트 인터럽트는 이 빌드에 구현되어 있다. 진행 중인 턴에 메시지를 넣어도 현재 생성은 취소되지 않는다. 흐름을 끊고 다시 시작하는 대신 메시지를 대기열에 넣고, 에이전트 루프가 도구 실행을 마친 안전 지점에서 대화 이력에 붙인다. 구현은 세 조각으로 나뉜다. 프로토콜 요청 두 개(`crates/protocol/src/wire.rs:99-124`), 세션별 대기열(`crates/agent-runtime/src/lib.rs:18-175`), 그리고 턴 루프의 주입 지점(`crates/app-core/src/agent/interrupts.rs:536-669`)이다.

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

주입 지점은 세 곳이다. 이벤트에는 지점 이름이 그대로 실린다(`crates/protocol/src/wire/events.rs:266-284`).

| 지점 | 위치 | 시점 | 용도 |
|-------|----------|--------|----------|
| **B** | 턴 완료 | 도구 요청 없음 | 기본: 남은 도구가 없어 순서 문제가 없다 |
| **C** | 도구 라운드 중간 | 긴급 중단 | 건너뛴 호출에 결과를 먼저 채운 뒤 주입한다 |
| **D** | 모든 도구 실행 후 | 다음 제공자 호출 전 | 기본값: 대기 중인 인터럽트를 여기서 모두 비운다 |

지점 판정은 `crates/app-core/src/agent/interrupts.rs:695`(B)와 `:706`(D)에 있고, C는 긴급 인터럽트가 있을 때만 열린다. 스트리밍 경로는 같은 지점을 `crates/app-core/src/agent/turn_streaming_mpsc.rs:1504`(B), `:1636`(D), `:2231`(C)에서 쓴다.

#### 지점 C의 긴급 처리

긴급 플래그가 선 메시지가 대기 중이면 도구 라운드를 끝까지 돌리지 않는다. 판정 함수는 `urgent_interrupt_pending` 하나뿐이다(`crates/app-core/src/agent/interrupts.rs:135-140`). 라운드를 자를 때는 아직 결과를 쓰지 않은 호출마다 `is_error: true`인 도구 결과를 먼저 넣는다(`crates/app-core/src/agent/turn_streaming_mpsc.rs:2205-2227`). 내용은 `[Skipped: user interrupted]`다(`:12`). 그 뒤 사용자 메시지를 넣고, 건너뛴 개수를 이벤트의 `tools_skipped`로 알린다.

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

`client_id`는 주입 이벤트에 그대로 되돌아오고, 필터가 붙은 취소 요청에서 대상 지정에 쓰인다(`crates/protocol/src/wire.rs:105-124`).

서버는 세 가지 이벤트를 낸다.

- `soft_interrupt_injected`: 주입한 내용, 표시 역할, 지점, 건너뛴 도구 수, 합쳐진 호출자 id(`crates/protocol/src/wire/events.rs:266-284`).
- `soft_interrupts_cancelled`: 취소된 id와 대기열에 없던 id를 나눠 보고한다(`:288-294`).
- `soft_interrupt_queue`: 턴이 끝났는데 대기열에 남은 메시지 수. 턴의 종료 이벤트 바로 앞에 온다(`:309-312`).

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

이 빌드에는 TUI가 없다. 클라이언트가 쓸 수 있는 피드백은 프로토콜 이벤트 세 가지다.

기본 모드(`urgent = false`)에서 클라이언트가 `soft_interrupt`를 보내면 데몬은 메시지를
대기열에 넣는다. 주입되면 `soft_interrupt_injected`가 지점 이름과 함께 온다
(`crates/protocol/src/wire/events.rs:266-284`). 턴이 끝났는데 대기열에 메시지가
남아 있으면 `soft_interrupt_queue`가 남은 수를 알리고, 그 이벤트는 종료 이벤트 바로 앞에 온다
(`crates/app-core/src/server/client_lifecycle.rs:3554-3561`). 남은 수가 0이면
이벤트를 보내지 않는다.

메시지는 출처별로 묶여 `\n\n`로 이어 붙는다. 같은 출처의 연속 메시지는 한 번의 주입으로
들어가고, 출처가 바뀌면 그 지점에서 묶음이 끊긴다(`crates/app-core/src/agent/interrupts.rs:596-640`).

긴급 모드(`urgent = true`)는 지점 C에서 남은 도구를 건너뛸 수 있다는 뜻이다
(`crates/protocol/src/wire.rs:103-104`). 건너뛴 호출에는 `is_error` 결과가 먼저
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
이것은 추가형 API v1.12이며 Rust와 TypeScript SDK 모두에서 쓸 수 있다
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
- `transport`: 이름만 남아 있고 바꿀 것이 없다. `Provider` 트레이트에서 `transport`는 `None`,
  `set_transport`는 "This provider does not support transport switching" 오류, `available_transports`는
  빈 배열이 기본값이다. 이 트리의 구현은 그 기본값을 그대로 쓰거나 다른 프로바이더로 넘기는 위임뿐이다
  (`crates/provider-core/src/lib.rs:463-473`, `crates/base/src/provider/mod.rs:1651-1675`,
  `crates/base/src/provider/horon_agent.rs:205-215`). 그래서 인자 없이 부르면
  `{"transport": null, "provider": ...}`가 오고, `auto`·`https`·`websocket`을 넘기면 위 오류가 난다
  (`crates/app-core/src/server/runtime_commands.rs:218-229`).
  설정 파일과 환경변수의 `openai_transport`도 값을 읽어 표시만 하고 동작을 바꾸지 않는다
  (`crates/base/src/config/env_overrides.rs:859-864`, `crates/base/src/config/display_summary.rs:261`).
- `fast`: 인자 없는 명령은 우선순위/off를 토글하고, 명시적 on/off/flex/status를 지원한다.
  적용할 수 없는 프로바이더는 실제 오류를 돌려준다.
- `subscription`: 로그인 상태의 실제 라이브 호스팅 계정 사용량. 자격 증명은
  돌려주지 않는다. 네트워크/인증 실패는 오류이며 지어낸 상태가 아니다.
- `provider_test_coverage`: 유지된 커버리지 보고서. 유료 라이브 프로브는 아니다.
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
(`crates/harness-api/src/runtime_commands.rs:5-33`).
`premium`은 런타임 명령이 아니라 클라이언트 요청 `SetPremiumMode`이며 그 요청은 `crates/harness-api/src/harness_api_tests/capability_coverage.rs:68`에 나온다.
`cache`, `cache stats`, `cache extend`, `cache 1h`, `cache 5m`은 이 열거형에 없다.
프리미엄 모드처럼 클라이언트 요청으로만 닿는 상태를 런타임 명령으로 보내면 오류가 난다.

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
업그레이드 워크플로를 쓴다. 세션 상태를 보존하고 라이브 데몬이나
Desktop 하네스 브리지를 죽이지 않는다. 양쪽이 새 코드를 실행할 때까지 그 기능은 쓸 수 없다.

## `--resume` 동작

이 빌드에는 대화형 세션 선택기가 없다. 세션 재개는 전역 `--resume` 플래그 하나로 한다
(`src/cli/args.rs:76-78`). 값을 주지 않고 `--resume`만 넘기면 데몬은 시작하지 않고 오류로
끝난다. 문구는 "the interactive session picker is unavailable in this headless build; pass an
explicit session id to --resume"다(`src/cli/dispatch.rs:97-101`).

### 기본 동작

- `--resume <id>`는 세션 id를 받아 그 세션으로 시작한다. 값이 전체 id가 아니면 로컬 저장소에서
  먼저 해석한다(`src/cli/dispatch.rs:696-736`).
- 해석에 실패하면 원문 id를 그대로 서버로 넘겨 서버가 다시 해석하게 한다
  (`src/cli/dispatch.rs:721`).
- 재개 여부는 다른 전역 플래그와 함께 쓸 수 있다. `--fresh-spawn`은 새 창으로 띄운 경우
  무거운 로컬 재개 준비를 건너뛴다(`src/cli/args.rs:80-82`).

`session_picker_enter` 키는 설정 스키마에 남아 있다(`crates/config-types/src/lib.rs:1193`,
`crates/base/src/config/default_file.rs:127`).
기본값은 `"current-terminal"`이고 다른 값은 `"new-terminal"`이다. 이 빌드에서 그 값을 읽는
선택기 UI는 없다.

### 재개한 세션을 어디에 열지

세션을 새 터미널이나 멀티플렉서 페인에서 여는 경로는 터미널 실행기가 맡는다.
`[terminal] spawn_hook`이 설정돼 있으면 그것이 먼저 쓰인다
(`crates/base/src/terminal_launch.rs:13-30`). 설정이 없으면 실행기는 클라이언트의
터미널 환경 변수를 보고 대상을 고른다(`crates/terminal-launch/src/lib.rs:92-115`).
Herdr 안에서 실행 중이면 호출한 페인을 나누고 새 페인에서 재개 명령을 실행한다
(`crates/terminal-launch/src/lib.rs:630-648`).

### 저장한 세션

`/save`, `/unsave`는 클라이언트 슬래시 명령이며 이 빌드에는 그 클라이언트가 없다. 세션에 붙는
저장 레이블은 하네스 API 응답의 `save_label` 필드로 남아 있다
(`crates/harness-api-server/src/translate.rs:255`, `:883` 참고).
