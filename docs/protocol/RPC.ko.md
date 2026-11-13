# stdio RPC 모드

언어: [English](RPC.md) | **한국어**

RPC 모드를 쓰면 호스트 프로그램이 에이전트를 자식 프로세스로 구동한다. 표준 입력으로 한 줄에 JSON 객체 하나를 보내고, 표준 출력에서 한 줄에 프레임 하나를 받는다. 다른 프로그램 안에 Horon을 넣고 싶고 프로세스를 직접 소유하며 스트리밍 출력·취소·세션 제어가 필요하지만 ACP를 말할 수 없을 때 쓰는 표면이다.

이 문서는 이 빌드가 구현한 계약과, 공개된 omp RPC 레퍼런스의 각 항목을 이 빌드가 어디까지 처리하는지를 행 단위로 밝힌다. Horon은 omp용으로 작성된 호스트가 Horon을 구동할 수 있도록 같은 와이어 프로토콜을 말한다. 처리할 수 없는 항목은 조용히 실패하지 않고 지원 표에 그렇게 적는다.

## 실행

```bash
horon-agent rpc                      # 서브커맨드 형태
horon-agent --mode rpc               # 플래그 형태, 같은 모드
horon-agent rpc --resume <session>   # 기존 세션에 붙기
```

두 형태는 같은 어댑터에 닿고, 호스트가 둘 다 넘기면 서브커맨드가 이긴다. `--session-dir`는 받되 무시한다. 세션은 Horon 홈 아래에 저장되므로 어댑터는 그것을 지킨 척하지 않고 경고를 남긴다.

표준 출력에는 프레임만 나간다. 진단은 로깅 서브시스템을 거쳐 표준 오류로 나가므로 호스트는 표준 출력을 걸러내지 않고 파싱한다. 표준 입력을 닫으면 프로세스가 끝나고, 진행 중인 턴이 있으면 그 턴을 마친 뒤 끝난다.

어댑터는 데몬이 없으면 띄우고, ACP 어댑터와 같은 방식으로 구독한 뒤 세션 기록을 읽는다. 모델 호출은 활성 `[providers.<name>]` 프로파일의 OpenAI 호환 엔드포인트로 가므로 호스트를 띄우기 전에 그 프로파일이 준비돼 있어야 한다.

## 한 턴의 모양

최소 교환은 입력 두 줄과 승인 한 줄, 그 뒤 턴 프레임이다. `prompt` 응답은 턴이 돌기 *전에* 기록되므로, 그 응답을 기다리는 호스트가 긴 답변에서 교착되지 않는다.

```json
{"type":"ready","protocolVersion":1,"supportedProtocolVersions":[1],"maxFrameBytes":1048576,"maxReassembledFrameBytes":67108864}
{"id":"p1","type":"prompt","message":"Reply with only the word ok"}
{"id":"p1","type":"response","command":"prompt","success":true}
```

텍스트만 있는 한 턴은 이 순서로 프레임을 쓴다. `agent_start`, `turn_start`, 사용자 `message_start`와 `message_end`, 어시스턴트 `message_start`, 그 `message_update`들, 어시스턴트 `message_end`, `turn_end`, `agent_end`, 그리고 `prompt_result`와 `session_settled`. 측정한 텍스트 한 턴은 `ready`부터 `session_settled`까지 19프레임이고, 도구 한 번을 쓰는 같은 측정이 58프레임이다. 호스트는 `agent_end` 앞에 끼어드는 `extension_ui_request`도 볼 수 있으므로 무시한다.

모든 프롬프트에 두 규칙이 적용된다.

- 턴이 완료됐든 중단됐든 실패했든 `prompt_result` 하나와 `session_settled` 하나가 정확히 프롬프트를 끝낸다. `prompt_result`에는 `agentInvoked`, `sessionSettled`와 함께 `status`(`completed`, `aborted`, `error`)가 실린다.
- 도구 실행은 어시스턴트 `message_end` *뒤에* 도착한다. 데몬은 응답이 닫히기 전에 도구 호출 인자를 스트리밍하지만, 호스트의 스텝 경계는 실행과 그 결과 메시지를 메시지 뒤에 둔다. 그래서 어댑터가 그것을 붙잡아 두었다가 내보낸다. 이 모드에서 순서를 바꾸는 곳은 여기 하나뿐이다.

프롬프트가 도는 동안에도 어댑터는 표준 입력을 계속 읽으므로 `steer`, `follow_up`, `abort_and_prompt`, `abort`, `get_state`, `negotiate_protocol`이 턴 중간에 동작한다. 나머지 명령은 대기열에 넣지 않고 `code: "busy"`로 거절한다. 대기시킨 `message`가 진행 중인 턴과 뒤섞이기 때문이다.

표준 입력은 전용 태스크가 읽는다. `select!`로 취소된 읽기는 이미 소비한 바이트를 잃어 최악의 순간에 프레임을 자르므로, 리더가 완성된 줄을 채널로 넘긴다.

## 명령

`Horon` 열은 그 명령을 처리하는 파일과 줄을, `omp` 열은 공개 레퍼런스의 절과 호스트가 보는 결과를 가리킨다. "partial"은 응답은 하되 필드나 변형이 빠졌다는 뜻이다.

| Command | Horon | omp | 판정 |
| --- | --- | --- | --- |
| `negotiate_protocol` | `src/cli/rpc/adapter.rs:232` answers `{protocolVersion:1}` for any request | Transport versions, "negotiate_protocol" | 부분 — v1만 지원하고 v2 청킹 없음 |
| `prompt` | `src/cli/rpc/adapter.rs:218`, turn driven at `src/cli/rpc/adapter.rs:349` | Commands table, `prompt` | 있음 |
| `steer` | `src/cli/rpc/adapter.rs:426` sends a soft interrupt with `urgent: false` | "Sending work while a turn is active" | 있음 |
| `follow_up` | Same arm, `urgent: true` | Same section | 부분 — 스티어링·팔로업 모드가 `all`로 고정이고 `set_steering_mode`, `set_follow_up_mode`가 없음 |
| `abort` | `src/cli/rpc/adapter.rs:237` while idle, `src/cli/rpc/adapter.rs:437` mid-turn | "Cancel safely" | 있음 — 턴은 이 응답이 아니라 데몬의 인터럽트로 끝남 |
| `abort_and_prompt` | `src/cli/rpc/adapter.rs:441` | "Cancel safely" | 부분 — 대체 프롬프트를 예약하지 않고 새 텍스트를 스티어로 보냄 |
| `get_state` | `src/cli/rpc/state.rs:108` | Commands table, `get_state` | 있음 |
| `set_model` | `src/cli/rpc/state.rs:237` | Commands table, `set_model` | 있음 — 응답은 선택된 모델이며 `provider`와 `modelId`를 `modelId` 한 필드에서 읽음 |
| `get_available_models` | `src/cli/rpc/state.rs:135` | Commands table, `get_available_models` | 있음 — 쓸 수 없는 라우트를 빼서 호스트가 실패할 라우트를 고르지 못함 |
| `set_thinking_level` | `src/cli/rpc/state.rs:258` | Commands table, `set_thinking_level` | 있음 |
| `get_available_commands` | `src/cli/rpc/adapter.rs:253` | Commands table, `get_available_commands` | 부분 — `compact`와 `model` 두 항목만 |
| `set_todos` | `src/cli/rpc/state.rs:294` | Commands table, `set_todos` | 있음 — `blocked`와 `blocker`가 이 빌드에 실재하므로 왕복해도 그대로 남음 |
| `compact` | `src/cli/rpc/state.rs:309` | Commands table, `compact` | 부분 — `CompactionResult`에 `summary`만 실리고 `customInstructions`는 무시 |
| `get_session_stats` | `src/cli/rpc/state.rs:328` | Commands table, `get_session_stats` | 있음 |
| `get_messages` | `src/cli/rpc/state.rs:359` | Commands table, `get_messages` | 있음 |
| `set_session_name` | `src/cli/rpc/state.rs:278` | Commands table, `set_session_name` | 있음 |
| `bash` | `src/cli/rpc/state.rs:423` runs the command locally with `sh -c` | Commands table, `bash` | 부분 — 응답 전에 끝까지 돌므로 다른 명령처럼 직렬화됨 |
| `abort_bash` | `src/cli/rpc/adapter.rs:323` | Commands table, `abort_bash` | 부분 — 이 빌드에서 `bash`가 동시 실행되지 않으므로 미지원으로 응답 |
| `new_session` | `src/cli/rpc/adapter.rs:241` | Commands table, `new_session` | 없음 — 프로세스당 세션 하나이므로 프로세스를 하나 더 띄움 |
| `switch_session`, `branch`, `get_branch_messages`, `handoff` | Not in `src/cli/rpc/command.rs:93` | Commands table, session rows | 없음 |
| `get_last_assistant_text` | Absent | Commands table | 없음 — `get_messages`가 전체 기록을 반환 |
| `get_messages_page` | Absent | Commands table, `get_messages_page` | 없음 — 커서 페이징이 없고 `get_messages`가 전부 반환 |
| `export_html` | Absent | Commands table | 없음 |
| `set_host_tools`, `set_host_uri_schemes` | Absent | "Host tools and host URIs" | 없음 — 도구는 데몬 쪽에서 결정됨 |
| `set_subagent_subscription`, `get_subagents`, `get_subagent_messages` | Absent | "Subagent frames" | 없음 |
| `cycle_model`, `cycle_thinking_level` | Absent | Commands table, model rows | 없음 |
| `set_steering_mode`, `set_follow_up_mode`, `set_interrupt_mode` | Absent | "Sending work while a turn is active" | 없음 — `all`, `all`, `immediate`로 고정 |
| `set_auto_compaction`, `set_auto_retry`, `abort_retry` | Absent | "Errors and recovery" | 없음 |
| `set_fast_mode` | Absent | Commands table | 없음 |
| `get_login_providers`, `login` | Absent | Commands table, login rows | 없음 — 자격 증명은 세션 밖에서 설정 |

JSON 객체가 아니거나 `type`이 없는 줄은 무시한다. 호스트가 시작하면서 빈 줄을 섞어 보낼 수 있고, 전송이 한 번 흔들렸다고 세션이 끝나면 안 되기 때문이다. 1메비바이트를 넘는 줄은 `code: "frame_too_large"`로 실패시키고 연결은 열어 둔다.

## 표준 출력의 프레임

`Horon` 열은 그 프레임을 쓰는 곳을, `omp` 열은 공개 레퍼런스의 프레임 표를 가리킨다.

| Frame | Horon | omp | 판정 |
| --- | --- | --- | --- |
| `ready` | `src/cli/rpc/frame.rs:32` | Other stdout frames, `ready` | 부분 — `supportedProtocolVersions`가 `[1,2]`가 아니라 `[1]` |
| `response` | `src/cli/rpc/frame.rs:43`, `src/cli/rpc/frame.rs:53` | Response envelope | 있음 |
| `prompt_result` | `src/cli/rpc/frame.rs:82` | Other stdout frames | 있음 — `status`를 더해 항상 보냄 |
| `session_settled` | `src/cli/rpc/frame.rs:92` | Not documented as a separate frame | 있음 |
| `agent_start`, `agent_end` | `src/cli/rpc/mapper.rs:82`, `src/cli/rpc/mapper.rs:169` | Agent lifecycle events | 있음 |
| `turn_start`, `turn_end` | `src/cli/rpc/mapper.rs:90`, `src/cli/rpc/mapper.rs:555` | Agent lifecycle events | 있음 |
| `message_start`, `message_end` | `src/cli/rpc/frame.rs:100`, `src/cli/rpc/frame.rs:104` | Agent lifecycle events | 있음 — `message` 옆에 `messageId`를 더함 |
| `message_update` | `src/cli/rpc/frame.rs:109` | Agent lifecycle events | 있음 — `text_*`, `thinking_*`, `toolcall_*`만 내보내고 `image_end`, `done`, `error` 이벤트는 없음 |
| `tool_execution_start`, `tool_execution_update`, `tool_execution_end` | `src/cli/rpc/mapper.rs:520`, `src/cli/rpc/mapper.rs:527`, `src/cli/rpc/mapper.rs:539` | Agent lifecycle events | 있음 — 어시스턴트 `message_end` 뒤에 내보냄 |
| `available_commands_update` | `src/cli/rpc/adapter.rs:173` | Other stdout frames | 부분 — 시작할 때 한 번 쓰고 갱신하지 않음 |
| `session_info_update` | `src/cli/rpc/mapper.rs:141` | Other stdout frames | 있음 |
| `model_changed`, `thinking_level_changed` | `src/cli/rpc/mapper.rs:147`, `src/cli/rpc/mapper.rs:150` | Session events | 있음 |
| `notice` | `src/cli/rpc/frame.rs:96` | Session events, `notice` | 부분 — 가드레일 중단과 컴팩션에 쓰고 `source`는 없음 |
| `rpc_frame_error` | `src/cli/rpc/frame.rs:73` | Other stdout frames | 있음 — 1메비바이트를 넘는 프레임은 자르지 않고 대체함 |
| `rpc_chunk` | Absent by design: `src/cli/rpc/frame.rs:18` pins version 1 | "Transport versions and large frames" | 없음 — 청크 재조립은 v2 전용 |
| `command_output` | Absent | Other stdout frames | 없음 — 어댑터가 직접 처리하는 슬래시 명령이 텍스트를 내지 않음 |
| `config_update` | Absent | Other stdout frames | 없음 — 대신 `model_changed`와 `thinking_level_changed`가 변경을 전함 |
| `extension_ui_request` | `src/cli/rpc/adapter.rs:164` sends one empty `setWidget` | "Extension UI sub-protocol" | 부분 — 이 프레임을 기다리는 호스트가 멈추지 않게 승인만 한 번 보내고 대화상자, `cancel`, 값 응답은 없음 |
| `extension_error` | Absent | Other stdout frames | 없음 |
| `host_tool_call`, `host_tool_cancel`, `host_uri_request`, `host_uri_cancel` | Absent | "Host tools and host URIs" | 없음 |
| `subagent_lifecycle`, `subagent_progress`, `subagent_event` | Absent | "Subagent frames" | 없음 |
| `auto_compaction_start`, `auto_compaction_end`, `auto_retry_start`, `auto_retry_end` | Absent | Session events | 없음 — `notice`가 컴팩션을 알림 |

## 호스트가 처리해야 하는 차이

- 버전 2를 협상하는 호스트는 버전 1로 응답을 받는다. 청크 없는 프레임을 받아들이도록 내려가야 한다. 이 빌드는 `rpc_chunk`를 절대 내보내지 않는다.
- 1메비바이트를 넘는 프레임은 대체된 프레임과 함께 `rpc_frame_error`로 보고되고 전달되지 않는다. 큰 도구 출력은 그래서 청크가 아니라 그 자신의 `tool_execution_end`로 호스트에 도달한다.
- `prompt_result`는 항상 기록되고 항상 `sessionSettled: true`를 실으므로, 호스트는 둘 다 기다리지 않고 `prompt_result`나 `session_settled` 중 하나로 턴을 끝낼 수 있다.
- 이 모드에서 호스트에 결정을 묻는 일은 없다. 승인과 질문은 데몬 쪽에서 해결되므로 `extension_ui_request`가 턴을 막지 않는다.

## 검증

어댑터에는 데몬을 프로세스 안 소켓 쌍으로 스크립트하는 통합 시험이 있어, 데몬도 모델도 없이 실제 프로토콜 경로를 돌린다. 시작 핸드셰이크, 버전 협상, 알 수 없는 명령, JSON이 아닌 줄, `get_state`, 프롬프트 한 턴, 유휴 상태의 `abort`, 한도를 넘는 프레임, `set_todos` 저장, 모델 카탈로그 투영을 다룬다.

```bash
cargo test -p horon-agent --lib cli::rpc
python3 scripts/ci/check_docs_pairs.py
```

위 표의 증거 행은 각 명령을 처리하거나 각 프레임을 쓰는 파일과 줄을 밝히므로, 검토자는 아무것도 실행하지 않고 한 행을 확인할 수 있다.