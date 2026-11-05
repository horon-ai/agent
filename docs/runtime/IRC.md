# IRC 메일박스 버스

같은 `horon-agent` 데몬 안의 에이전트들이 `irc` 도구로 메시지를 주고받는 경로를 설명한다. 메일박스 상한, 대기자 FIFO, seq 순서, 답장 의무, 응답 불가 판정, wake 결과 중계, 설정과 성능 게이트를 다룬다.

## IRC 메일박스 버스

### 한눈에

`irc` 도구는 에이전트 사이의 짧은 대화를 위한 창구다. 보내는 쪽은 `send`로 메시지를 넘기고, 받는 쪽은 상태에 따라 대기자에게 즉시 전달되거나, 턴 경계에 주입되거나, 유휴 상태에서 새 턴으로 깨어난다. 받은 메시지는 `<irc …>` 봉투로 모델 앞에 나타나며, 봉투의 `id`를 `reply_to`로 되돌려 답한다.

이 버스는 프로세스 안에서만 동작한다. 데몬이 소유한 tokio 태스크와 세션 상태를 그대로 쓰고, 디스크나 네트워크를 거치지 않는다. 버스 코어는 런타임을 모르는 별도 크레이트 `horon-agent-irc-core`에 있고, 서버 연결은 `crates/app-core/src/server/irc/`가 맡는다.

### oh-my-pi와의 대응

oh-my-pi의 `IrcBus`(커밋 88a69582, GitHub 주소 https://github.com/can1357/oh-my-pi/tree/88a69582c755bd000e8dc0290981fc787a4f400c/packages/coding-agent/src/irc)가 원본 계약이다. 옮긴 범위와 바꾼 점은 다음과 같다.

| 계약 | 이 저장소의 구현 |
| --- | --- |
| 메일박스 상한 100, 넘치면 가장 오래된 것부터 버림 | `crates/irc-core/src/message.rs`의 `IrcLimits`와 `crates/irc-core/src/bus.rs`의 `enqueue` 호출, `crates/irc-core/src/mailbox.rs`의 `State::enqueue` |
| 대기자를 먼저 채우고, 성공한 전달은 메일박스에 남기지 않음 | `crates/irc-core/src/bus.rs`의 `hand_off` |
| 메시지마다 단조 증가 `seq`, 같은 밀리초에도 `seq`로 순서 판정 | `crates/irc-core/src/bus.rs`의 `next_id`와 `crates/irc-core/src/message.rs`의 `IrcMessage::seq` |
| 본문 하나를 한 번만 할당하고 모든 브로드캐스트 레그가 공유 | `crates/irc-core/src/bus.rs`의 `Arc<str>` 본문과 `body_allocations` 카운터 |
| 답장 의무 추적과 응답 불가 판정 | `unanswered_sends`, `can_respond`, `WaitOutcome::Unanswerable` |
| 부모가 보낸 직접 메시지의 긴급 주입 | `crates/app-core/src/server/irc/delivery.rs`의 `urgent` 분기 |
| 유휴 wake와 턴 결과 중계 | `crates/app-core/src/server/irc/relay.rs` |
| 레그별 영수증 | `SendReport`, `Receipt`, `Outcome` |
| 본문이 프롬프트 구조를 위조하지 못하게 이스케이프 | `crates/irc-core/src/envelope.rs` |

바꾼 점은 여섯이다.

- 기존 `swarm` 도구에 얹지 않고 별도 `irc` 도구로 분리했다. `send`, `wait`, `inbox`, `list` 네 동작만 노출한다.
- 본문 상한 `max_body_bytes`(기본 65536)를 두었다. 원본에는 없는 값이다.
- 브로드캐스트 동시 전달 수를 `broadcast_concurrency`(기본 16)로 제한한다.
- 사라진 세션을 되살리는 대신 유휴 세션 wake로 대체했다. 세션이 없으면 그 레그는 실패한다.
- 동료 목록은 `list` 동작으로만 보여 준다. 시스템 프롬프트에 피어 로스터를 넣지 않는다.
- 기존 `swarm`의 완료 보고는 IRC 답장으로 세지 않는다. 답장 의무는 `irc` 도구가 보낸 직접 메시지에만 붙는다.

### 주소와 가시성

받는 사람 `to`에는 세션 id, 사람이 읽는 고유 이름(handle), 또는 `all`을 쓴다. `agent://` 접두사는 떼고 해석한다.

- 세션 id가 정확히 맞으면 그 멤버를 고른다.
- id가 아니면 고유 이름이 정확히 같은 멤버를 찾는다. 둘 이상이면 `handle "…" is ambiguous; use the session id`로 거절한다.
- 어느 쪽도 없으면 `Unknown agent "…" - use irc action=list`로 거절한다.

가시성은 두 멤버의 `swarm_id`가 모두 있고 같거나, 한쪽의 `report_back_to_session_id`가 다른 쪽의 세션 id일 때 성립한다. 자식은 부모를, 부모는 자식을 볼 수 있고, 같은 `HORON_AGENT_SWARM_ID`를 공유한 세션끼리도 서로 보인다. 그 밖의 조합은 `agent "…" is not visible to this session`으로 거절한다.

root 세션은 부모가 없으므로 자기 스웜을 대표한다. `HORON_AGENT_SWARM_ID`를 지정하면 여러 root 세션이 같은 스웜 id를 공유한다. `all`은 root 세션만 쓸 수 있고, 대상은 같은 스웜에 속하면서 발신자를 제외하고 `stopped`, `failed`, `completed`가 아닌 멤버다. 자식 세션이 `all`을 쓰면 `only a root session can broadcast`로 거절한다.

### 전달 경로

`send`는 대상마다 레그를 만들고 다음 순서로 전달한다. 앞 단계가 성공하면 뒤 단계는 돌지 않는다.

| 받는 쪽 상태 | 하는 일 | 영수증 |
| --- | --- | --- |
| 그 메시지를 받아 줄 대기자가 있음 | 필터를 만족하는 가장 오래된 대기자에게 `oneshot`으로 넘김. 메일박스에 넣지 않고 주입도 하지 않음 | `injected` |
| External wake 모드이고 라이브 세션이 있음 | `WakeRequested` 이벤트만 보냄(이유 `irc_delivery`, 알림 본문은 봉투). soft interrupt도 중계도 없음 | `woken` |
| 유휴 세션 | 새 라이브 턴을 시작함. 직접 메시지면 중계를 예약함 | `woken` |
| 바쁘고 보낸 사람이 그 대상의 부모 | 긴급 soft interrupt로 턴 경계에 주입 | `injected` |
| 바쁘고 그 밖의 관계 | 일반 soft interrupt로 턴 경계에 주입 | `injected` |
| 라이브 세션이 없음 | 실패로 기록하고 그 대상의 버스 상태를 지움 | `failed` |
| soft interrupt 큐가 거절함 | 실패로 기록하고 메일박스에 보관 | `failed` |

External wake 모드는 클라이언트가 직접 세션을 깨우는 설정이다. 이 모드에서 살아 있는 세션에는 `WakeRequested`만 나가고, soft interrupt로 폴백하지 않는다. 세션이 아예 없으면 위 표의 라이브 세션이 없는 실패 경로를 탄다.

부모 판정은 대상의 `Endpoint::parent`, 즉 `report_back_to_session_id`가 보낸 사람의 세션 id와 같은지로 한다. 브로드캐스트와 `progress` 메시지는 부모가 보내도 긴급 주입이 아니다.

주입 지점은 턴이 끝나는 자리가 아니라 모델 응답과 도구 라운드의 경계다. 위 표의 `턴 경계`도 턴 종료가 아니라 이 경계를 뜻한다. 스트리밍 경로는 모델 응답이 도구 호출 없이 끝날 때 그 응답을 처리하는 자리(`crates/app-core/src/agent/interrupts.rs`의 `handle_streaming_no_tool_calls`), 도구 라운드가 끝난 뒤 다음 모델 호출 전(`crates/app-core/src/agent/turn_streaming_mpsc.rs`), 긴급 표시가 있을 때 도구 그룹 경계에서 주입한다. 비스트리밍 턴 루프는 도구 라운드가 끝날 때만 주입하므로(`crates/app-core/src/agent/turn_loops.rs`) 도구 없이 끝나는 턴에는 주입 지점이 없다. 그래서 바쁜 수신자의 턴이 마지막 모델 응답을 마치고 그 응답에 대한 주입 검사까지 지난 뒤에 도착한 메시지는 그 턴에 들어가지 않고 대기열에 남는다. 이 메시지는 버려지지 않고 보관되며(`crates/app-core/src/server/live_turn.rs`), 서버는 남은 개수만 `SoftInterruptQueue` 이벤트로 알리고 새 턴을 시작하지 않는다(`crates/app-core/src/server/client_lifecycle.rs`). 다음 턴에서도 주입은 턴 시작이 아니라 첫 모델 응답을 처리하는 경계에서 일어난다. 첫 응답이 도구 호출로 이어지면 그 라운드가 끝난 뒤에, 도구 호출 없이 끝나면 그 응답 직후에 주입된다. 기존 `swarm` DM도 같은 `queue_soft_interrupt_for_session` 경로를 쓴다(`crates/app-core/src/server/client_comm_message.rs`). 받는 쪽이 `irc wait`로 기다리고 있으면 위 표의 첫 행대로 대기자에게 바로 넘어가므로, 답이 필요한 쪽은 `wait`를 쓰는 편이 확실하다.

실패한 레그의 사유는 영수증에 실린다. 도구는 하나라도 전달되면 성공으로 보고, 모든 레그가 실패했을 때만 오류를 낸다. 부분 실패는 재전송으로 인한 중복을 피하려고 결과 본문에만 적는다.

### 순서와 메일박스 상한

`send` 한 번은 메시지 id 하나와 레그별 `seq`를 만든다. id와 `seq`는 같은 단조 카운터에서 나오므로 같은 밀리초에 들어온 전송도 `seq`로 순서가 갈린다. 한 번의 브로드캐스트에서 나간 레그들도 `seq`가 서로 다르고 증가 순서를 따른다.

같은 발신자의 연속 호출은 도구의 효과 등급이 기본 쓰기 배리어라서 한 라운드에서 하나씩 순서대로 실행된다. 그래서 같은 발신자가 보낸 메시지는 수신 측에서도 보낸 순서대로 도착한다. 서로 다른 발신자 사이의 전역 순서는 보장하지 않는다.

메일박스는 라이브 전달이 실패한 메시지를 보관하는 큐다. 대상마다 `VecDeque` 하나를 두고, 길이가 `mailbox_cap`(기본 100)을 넘으면 가장 오래된 것부터 버리며 `mailbox_dropped`를 센다. 대기자에게 바로 넘어간 메시지와 주입에 성공한 메시지는 메일박스에 남지 않는다. `inbox`는 오래된 것부터 꺼낸다.

### 답장 의무와 wait

버스는 발신자와 수신자 쌍마다 마지막으로 보낸 `seq`를 세 표에 기록한다.

- `last_sent`: `progress`가 아닌 성공한 전송.
- `last_direct`: 브로드캐스트와 `wake_relay`가 아닌 전송. `progress`도 여기에 들어간다.
- `last_progress`: `progress = true`인 전송만.

답장 의무는 `last_direct`에 남아 있는 쌍 가운데 상대가 그 뒤로 `last_sent`를 갱신하지 않은 것이다. 상대가 `progress`만 보냈으면 `last_sent`는 그대로이므로 의무가 남고, `last_progress`가 그 `seq`를 넘으면 `promised` 표시가 붙는다. 도구의 `wait`는 이 의무를 기준으로 기다릴 대상을 정한다.

`wait`는 다음 순서로 판정한다. ① 메일박스에 필터를 만족하는 메시지가 있으면 꺼내고 바로 돌려준다. ② 기다릴 대상이 없으면 `NothingToWaitFor`다. ③ 대상 모두가 이미 나를 기다리는 대기자를 등록했으면 `Unanswerable`로 즉시 풀린다. ④ 아니면 대기자를 등록한다. 등록 뒤 ⑤ 이 세션에 새 입력이 대기 중이면 `PendingInput`, ⑥ 대상 중 앞으로 답할 수 있는 세션이 하나도 없으면 `NothingToWaitFor`다. ⑦ 그 밖에는 도착, 마감, 취소, 500밀리초 재확인 중 먼저 오는 것을 기다린다.

`wait`의 결과는 여섯 가지다.

| 결과 | 뜻 |
| --- | --- |
| `Message` | 필터를 만족하는 메시지를 받았다. 메일박스에 있던 것과 대기 중 도착한 것을 모두 포함한다. |
| `Timeout` | 마감 시간이 지났다. 도구의 기본 대기는 30초이고 `wait_max_secs`를 넘지 않는다. |
| `NothingToWaitFor` | 답장 의무가 없거나, 의무가 있는 대상이 모두 더 이상 답할 수 없다. |
| `Unanswerable` | 기다릴 대상이 모두 나를 기다리는 중이다. 상호 대기에서 한쪽을 즉시 풀어 교착을 막는다. |
| `PendingInput` | 이 세션에 처리할 새 입력이 대기 중이다. 사용자 메시지나 단계 경계에 주입된 다른 메시지(IRC 봉투 포함)가 원인이다. |
| `Cancelled` | 세션 종료 신호로 대기를 접었다. |

답장 의무가 없으면 입력 대기 여부보다 먼저 `NothingToWaitFor`로 돌아온다. 막을 이유가 없는데 막지 않는다는 뜻이다.

대기자는 도착 즉시 풀린다. 500밀리초 재확인은 타이머 주기가 아니라 상대가 답할 수 없게 된 상황을 늦게 알아채기 위한 보조 검사다. 재확인 전에 답이 오면 대기자는 재확인 tick을 기다리지 않고 `Message`로 끝난다. 타임아웃과 취소로 끝난 대기자는 큐에서 지우므로 남지 않는다. 대기자를 제거하는 사이에 메시지가 들어왔으면 그 메시지를 우선한다.

`can_respond`는 그 세션이 앞으로 이 프로세스에서 메시지를 보낼 수 있는지를 뜻한다. 멤버 상태가 `running`이나 `spawned`이면 참이고, 새 입력이 대기 중이거나 중계 대기 중이어도 참이다. `completed`, `stopped`, `failed`이면서 그런 작업이 없으면 거짓이다.

### wake 결과 중계

유휴 세션을 직접 메시지로 깨운 턴이 직접 답하지 않으면, 그 턴의 마지막 assistant 텍스트를 보낸 사람에게 `wake_relay`로 중계한다.

- 중계는 대상별로 예약 수를 세고, 턴이 끝나면 하나씩 줄인다. 세션 정리가 먼저 카운터를 지웠으면 아무것도 하지 않는다.
- 턴이 끝난 시점에 대상이 그 사이 메시지를 보냈으면(`last_sent` 또는 `last_progress`의 `seq`가 원래 메시지보다 큼) 중계하지 않는다.
- 중계 본문은 `max_body_bytes`를 넘지 않게 UTF-8 경계에서 자른다. `reply_to`는 원래 메시지 id이고, `progress`는 거짓, `wake_relay`는 참이다.
- 브로드캐스트, `progress`, `wake_relay` 메시지로 깨어난 턴은 중계를 예약하지 않는다. 중계 자체도 `wake_relay`라서 다시 중계를 낳지 않는다. 이것이 핑퐁을 막는 장치다.
- External wake 모드에서는 중계를 예약하지 않는다. 클라이언트가 깨운 턴의 결과는 IRC 경로를 타지 않는다.

### 도구 표면

`irc` 도구는 `send`, `wait`, `inbox`, `list` 네 동작을 받는다.

| 인자 | 형식 | 쓰는 동작 |
| --- | --- | --- |
| `action` | `send` \| `wait` \| `inbox` \| `list`, 필수 | 모두 |
| `intent` | 문자열, 호출 이유를 나타내는 UI 라벨 | 모두. 모델에 나가는 도구 정의에서는 필수다. `irc`의 정의 원본은 `action`만 필수로 적고 공통 계층이 `intent`를 필수 목록에 더한다. |
| `to` | 문자열 | `send` |
| `message` | 문자열 | `send` |
| `reply_to` | 0 이상 정수 | `send` |
| `progress` | 불리언 | `send` |
| `from` | 문자열 | `wait`, `inbox` |
| `timeout_secs` | 1 이상 `wait_max_secs` 이하 정수, 기본 30 | `wait` |
| `direct_only` | 불리언, 기본 거짓 | `wait` |

동작별 결과는 다음과 같다.

- `send`: 첫 줄이 `message {id} sent`이고, 이어서 대상마다 `{to}: injected|woken|failed ({reason})` 한 줄이 나온다. 전달된 레그가 하나도 없을 때만 도구 오류다.
- `wait`: 결과가 메시지면 봉투를 그대로 돌려준다. 그 밖에는 `no message within {n}s`, `nothing to wait for: no one who owes you a reply can still answer`, `unanswerable: {ids} are waiting on you; send them what they need`, `interrupted: new input is queued for this session`, `cancelled` 중 하나다.
- `inbox`: 보관된 봉투를 오래된 순서로 개행으로 잇는다. 비어 있으면 `inbox is empty`다.
- `list`: 대상마다 `{id}\t{handle or -}\t{status}\t{parent or -}` 한 줄을 쓰고, 마지막 줄에 `unread: {n}`을 붙인다.

알 수 없는 `action`은 사용할 수 있는 동작 목록을 붙여 도구 오류로 끝난다. `send`에 `to`나 `message`가 없으면 각각 그 인자가 필요하다는 오류를 낸다.

도구 설명은 보내기가 블로킹이 아니라는 점, 받는 쪽 상태에 따라 전달 방식이 달라진다는 점, `to`에 세션 id와 고유 이름과 `all`을 쓸 수 있다는 점, 같은 스웜만 보인다는 점, 중간 확인은 `progress`로 보내고 답으로 세지 않는다는 점, `wait`는 막혀서 할 일이 없을 때만 쓰고 폴링하지 말라는 점을 모델에 알린다.

스위치가 꺼져 있으면 실행을 거부한다. 문구는 `IRC is off for this runtime. Set irc = true under [tools] in config.toml.`이다. 데몬 없이 단독 실행 중이라 전역 버스가 없으면 `IRC is unavailable: this session is not hosted by a horon-agent daemon`으로 거부한다.

### 설정

설정은 `[tools]`의 스위치와 `[irc]`의 상한으로 나뉜다. 기본 설정 파일은 `crates/base/src/config/default_file.rs`, 타입은 `crates/config-types/src/irc.rs`가 소유한다.

```toml
[tools]
irc = false

[irc]
mailbox_cap = 100
max_body_bytes = 65536
wait_max_secs = 300
broadcast_concurrency = 16
```

- `[tools] irc`는 기본으로 꺼져 있다. 세션 사이에 메시지 통로를 여는 기능이라 기본값을 꺼 두었다.
- 스위치를 켜면 `core`, `acp`, `minimal`, 명시 `enabled` 목록처럼 비어 있지 않은 허용 목록에 `irc`가 들어간다. 프로필 `none`처럼 빈 목록은 그대로 비어 있고, 허용 목록이 없는 `full`은 원래 모든 도구를 노출한다.
- 스위치가 꺼져 있으면 `full`처럼 허용 목록이 없는 세션에서도 실행이 거부된다. 도구 등록 자체를 거르지 않는 이유가 이것이다.
- `HORON_AGENT_IRC` 환경변수가 `[tools] irc`를 덮어쓴다. 값은 기존 불리언 환경변수와 같은 규칙으로 읽고, 알아볼 수 없는 값은 무시한다.
- 네 상한은 모두 하한 1로 보정한다. 0이면 메일박스가 모든 메시지를 버리거나 브로드캐스트가 진행하지 못하기 때문이다.

### 승인과 계획 모드

`irc`는 파일을 쓰지 않지만 `effects()`를 재정의하지 않으므로 트레이트 기본값인 쓰기 배리어를 선언한다. 프로세스 비트와 네트워크 비트는 없다. 이 선언이 두 게이트에 그대로 쓰인다.

계획 모드는 쓰기 배리어 호출을 거부한다. 그래서 계획 모드가 켜진 세션에서는 `irc` 호출도 `plan_mode` 사유로 막힌다. 계획을 승인한 뒤에 다시 호출할 수 있다.

승인 등급은 도구의 효과 비트로 판정한다(`crates/app-core/src/approval.rs`). `off`는 묻지 않고, `write`는 파일 변이에 해당하는 효과를 자동 승인하며, `all`은 읽기 전용이 아닌 호출을 자동 승인한다. `irc`는 쓰기 비트만 있고 프로세스·네트워크 비트가 없어서 세 모드 모두 자동 승인으로 분류된다. 즉 `approval_mode = "write"`에서 `irc`는 일반 파일 변이와 같은 취급을 받고 승인 창을 띄우지 않는다. `dual_confirm_classes`에 걸리는 위험 등급도 없다(`crates/app-core/src/approval.rs`의 `danger_class`는 셸 명령만 분류한다).

### 성능 상한과 게이트

구조 게이트의 `irc` 섹션이 버스의 모양이 유계인지 매 PR마다 검사한다. 측정은 `crates/irc-core/src/perf_tests.rs`의 `irc_bus_stays_bounded`가 직접 인쇄하고, 게이트는 그 줄을 읽는다.

| 카운터 | 경계 | 뜻 |
| --- | --- | --- |
| `mailbox_high_water` | 100 이하 | 메일박스가 상한을 넘지 않는다. |
| `mailbox_dropped` | 1000건 유입에서 정확히 900 | 넘친 만큼만 오래된 것부터 버린다. |
| `broadcast_body_allocations` | 1 | 64개 대상 브로드캐스트 한 번에 본문을 한 번만 할당한다. |
| `waiters_live_after_churn` | 0 | 타임아웃·취소 뒤 남은 대기자가 없다. |
| `broadcast_inflight_high_water` | 16 이하 | 동시에 진행하는 브로드캐스트 레그가 설정 상한을 넘지 않는다. |

경계 값은 `scripts/perf/structural_budget.json`에 있고, 게이트 구현은 `scripts/perf/check_structural.py`에 있다. 한 섹션만 돌리는 명령은 다음과 같다.

```bash
python3 scripts/perf/check_structural.py --only irc
```

처리량은 별도 예제로 잰다. 세 시나리오는 대기자에게 직접 전달하는 경로, 즉시 주입을 돌려주는 가짜 전달 경로, 64개 대상 브로드캐스트다.

```bash
cargo run --locked --release -p horon-agent-irc-core --example irc_throughput
```

환경변수 `IRC_THROUGHPUT_MESSAGES`로 메시지 수를 바꾼다. 기본값은 100000이다.

### 한계

- 같은 데몬 안에서만 동작한다. 다른 데몬이나 원격 호스트로 보내지 않는다.
- 메일박스를 디스크에 남기지 않는다. 데몬이 끝나면 버스 상태도 사라진다.
- 세션이 제거되면 그 세션의 메일박스, 대기자, 답장 표시를 모두 지운다. 제거된 세션에 남아 있던 메시지는 복구하지 않는다.
- 세션 id는 바뀌지 않으므로 고유 이름을 바꿔도 이미 보낸 메시지는 그대로 도착한다. 다만 이름은 보내는 순간 해석하므로, 이름을 바꾼 뒤 이전 이름으로 보내면 대상을 찾지 못한다.
- 채널, 토픽, 구독, 메시지 TTL은 없다.
- 데몬 없이 단독으로 실행한 세션에서는 버스가 없어 도구가 거부된다.
- `wait`는 모델 호출이 아니라 프로세스 안의 메시지 도착만 기다린다. 상대 턴이 모델 응답을 만드는 시간은 `timeout_secs` 안에 들어와야 한다.
- 바쁜 세션의 턴이 마지막 모델 응답을 마치고 그 응답에 대한 주입 검사까지 지난 뒤 도착한 메시지는 그 턴에 들어가지 않고 대기열에 남는다. 버려지지도, 새 턴을 시작하지도 않으며 다음 턴의 첫 모델 응답 경계에서야 주입되므로, 받는 쪽이 그 뒤로 턴을 이어 가지 않으면 전달이 그만큼 늦어진다.
