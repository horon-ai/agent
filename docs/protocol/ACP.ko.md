# ACP 지원 (v1과 v2)

언어: [English](ACP.md) | **한국어**

horon agent는 에디터에서 외부 에이전트로 붙을 수 있도록 Agent Client Protocol(ACP) v1과 v2를 한 엔드포인트에서 함께 지원한다. `horon acp`를 실행하면 표준 입출력으로 JSON-RPC 메시지를 주고받는 ACP 에이전트가 된다. v1은 안정판이고 메시지는 공식 `schema-v1.26.0`에 맞춘다. v2는 `schema-v2.0.0-alpha.13`의 안정(stable) 부분에 맞춘다. 이 문서의 v1 절은 v1 연결의 동작을 설명하고, v2 연결의 차이는 [ACP v2](#acp-v2) 절에 모았다.

## 실행

```bash
horon acp            # 표준 입출력으로 ACP v1을 처리한다
```

`acp`는 stdout에 ACP 메시지(줄 단위 UTF-8 JSON)만 쓴다. 로그는 stderr로 나가고, stdin이 닫히면 바로 종료한다. 처리 중이던 요청은 응답을 받지 못한다.

모델 호출은 `[providers.<name>]`에 적은 OpenAI 호환 엔드포인트의 `/chat/completions`로 직접 나가므로 그 프로파일이 준비돼 있어야 한다. 설정과 API 키는 [MODEL.ko.md](../provider/MODEL.ko.md)를 따른다.

`acp`는 에이전트 데몬에 연결한다. 데몬이 없으면 `acp`가 직접 띄우고, 이때 도구 프로필을 `[acp] tool_profile`(기본 `acp`)로 요청한다. 이미 떠 있는 데몬은 자기 설정을 그대로 쓴다. `--tool-profile`을 직접 주면 그 값이 우선한다.

### 에디터 설정

에디터마다 설정 위치와 키 이름이 다르다. 공통점은 명령어 `horon`(또는 `horon-agent`)과 인자 `["acp"]`를 외부 에이전트로 등록한다는 것이다. 아래는 Zed와 JetBrains 예시다. 키 이름은 각 에디터 문서에서 확인한다.

Zed(`settings.json`):

```json
{
  "agent_servers": {
    "horon agent": {
      "type": "custom",
      "command": "/절대/경로/horon",
      "args": ["acp"]
    }
  }
}
```

JetBrains(`~/.jetbrains/acp.json`):

```json
{
  "agent_servers": {
    "horon agent": {
      "command": "/절대/경로/horon",
      "args": ["acp"]
    }
  }
}
```

Neovim과 VS Code의 ACP 플러그인도 같은 두 값(command, args)을 받는다. 설정 키는 플러그인마다 다르다.

## 인증

`initialize` 응답의 `authMethods`는 다음과 같다.

| id | 제공 조건 | 하는 일 |
|---|---|---|
| `horon-agent-credentials` | 항상 | `[providers.<name>]` 프로파일의 자격 증명으로 모델을 쓸 수 있는지 확인한다 |

프로파일은 `horon provider add`로 등록한다. 클라이언트가 `auth.terminal`을 알려도 방식은 하나뿐이다. 이 빌드에는 에디터가 터미널에서 실행할 로그인 명령이 없다.

`authenticate`는 데몬이 쓸 수 있는 모델을 보고하면 `{}`로 성공한다. 모델이 없으면 `-32000`을 돌려준다. `session/new`도 쓸 수 있는 모델이 없으면 `-32000`이다. 모르는 `methodId`는 `-32602`다.

`logout`은 광고하지 않는다. 자격 증명을 지우는 기능이 없기 때문이다.

## 광고하는 기능

`initialize`는 항상 `protocolVersion: 1`로 답한다. 클라이언트가 65535를 보내도 같다. `protocolVersion`이 0~65535 범위의 정수가 아니거나 없으면 `-32602`다. `initialize` 전에 다른 요청을 보내면 `-32600`이다. 클라이언트의 `clientCapabilities`와 `clientInfo`는 연결마다 저장하고, `initialize`를 다시 보내면 새 값으로 갱신한다.

| 항목 | 값 |
|---|---|
| `loadSession` | `true` |
| `promptCapabilities` | `image: true`, `embeddedContext: true`, `audio: false` |
| `mcpCapabilities` | `http: true`, `sse: true` (stdio는 항상 지원) |
| `sessionCapabilities` | `close`, `resume`, `list`, `delete`, `additionalDirectories` |

광고하지 않는 기능을 부르는 메서드는 `-32601`로 거절한다. `_`로 시작하는 알 수 없는 요청도 `-32601`이고, 알 수 없는 알림은 무시한다. 알림에는 응답하지 않는다.

에러 코드는 다음 규칙을 따른다.

| 코드 | 쓰임 |
|---|---|
| `-32000` | 인증 필요(쓸 수 있는 모델 없음) |
| `-32002` | 모르거나 활성화되지 않은 세션 (`data.sessionId` 포함) |
| `-32600` | `initialize` 전 요청 |
| `-32601` | 지원하지 않는 메서드 |
| `-32602` | 잘못된 파라미터. 이미 prompt가 진행 중인 세션에 다시 prompt를 보낸 경우도 포함 |
| `-32603` | 내부 오류 |
| `-32800` | 취소된 요청 |

## 세션

| 메서드 | 동작 |
|---|---|
| `session/new` | `cwd`는 필수이고 절대 경로여야 한다. 아니면 `-32602`. `additionalDirectories`와 `mcpServers`를 받는다 |
| `session/load` | 저장된 세션을 연다. 기록을 응답 전에 `session/update`로 다시 보낸다 |
| `session/resume` | 기록을 다시 보내지 않고 세션만 연다 |
| `session/close` | 진행 중인 prompt를 취소하고 세션 연결을 놓는다. 그 prompt는 `cancelled`로 끝난다 |
| `session/list` | 숨기지 않은 세션을 최근 활동 순으로 보여준다 |
| `session/delete` | 세션을 목록에서 숨긴다 |

`session/list`는 `cwd`로 거를 수 있고, 한 번에 50개씩 돌려준다. 다음 페이지는 `nextCursor`로 받는다. 잘못된 cursor는 `-32602`이고, 결과가 없으면 빈 배열이다.

`session/delete`는 디스크의 세션 파일을 지우지 않는다. 데몬이 세션을 보관 처리(archive)해서 목록에서 빠질 뿐이다. 이미 보관된 세션에 다시 보내도 성공하므로 몇 번을 보내도 결과가 같다. 다른 클라이언트가 그 세션을 쓰고 있으면 `-32603`(다른 horon agent 클라이언트에서 열려 있음)으로 거절한다.

`session/load`와 `session/resume`이 세션을 새로 붙일 때 데몬이 그 세션의 저장소를 아직 다른 연결에 내주고 있으면, 데몬은 저장소의 거절(`store error [internal:busy] retryable session writer is already owned`)을 오류 프레임으로 보낸다. 어댑터는 이 거절에만 새 연결로 한 번 더 시도하고(데몬이 `retry_after_secs`를 주면 최대 2초까지 기다린 뒤), 그 시도도 거절되면 데몬의 사유를 담은 `-32603`으로 바로 답한다. 기록 재생이 시작된 뒤에는 다시 시도하지 않는다. 다른 오류, 다른 클라이언트가 세션을 쓰고 있다는 거절, 연결 끊김은 다시 시도하지 않고 바로 답한다. 기록이 많아 복원이 오래 걸리는 세션은 데몬이 기록을 보낼 때까지 기다리며, 데몬이 아예 멈춘 경우에만 시도마다 300초 안전 한도에서 응답 없음 사유로 답한다.

### load와 resume의 차이

`load`는 기록을 먼저 보내고 응답한다. `resume`은 기록을 보내지 않고 바로 응답한다. 이미 이 연결에서 열려 있는 세션에 다시 `load`나 `resume`을 보내면 새로 붙지 않고 열려 있는 연결을 그대로 쓴다.

`load`는 응답하기 전에 저장된 기록을 `session/update`로 다시 보낸다. 사용자 프롬프트는 `user_message_chunk`로, 추론은 `agent_thought_chunk`로, 답변은 `agent_message_chunk`로 간다. 추론은 `reasoning_display`가 `full`일 때만 저장된 대화 기록에 나타나 재생된다. `full`이 기본값이므로 기본 설정이면 추론도 함께 재생된다. 도구 호출은 한 건마다 완료 상태의 `tool_call`과 저장된 텍스트를 담은 `tool_call_update` 한 쌍으로 간다. 편집 도구라면 그 텍스트에 unified diff가 들어 있다. 구조화된 diff 내용은 저장하지 않으므로 재생하지 않는다. 실패한 도구도 기록에 오류 표시가 남지 않아서 완료로 재생된다. 마지막 계획은 `todo` 도구 입력에서 복원한다. 재생하는 메시지의 `messageId`는 `replay-<행 번호>` 형태다.

`compaction_update`는 클라이언트가 `session.compaction`을 광고했을 때만 재생한다. 광고하지 않은 클라이언트에는 압축 정보를 보내지 않는다. 압축된 세션은 데몬이 보여주는 최근 64행만 재생한다. 재생은 `load`를 보낸 그 연결에만 가고, `session/resume`은 재생하지 않는다.

## prompt 한 번의 흐름

`session/prompt`는 `text`, `resource_link`, `resource`(텍스트와 blob), `image` 블록을 받는다. `audio`는 광고하지 않으므로 거절한다.

그 턴의 `session/update`는 모두 응답보다 먼저 나간다. 응답의 `stopReason`은 데몬이 보고한 실제 원인이다.

| stopReason | 의미 |
|---|---|
| `end_turn` | 정상 종료 |
| `max_tokens` | 출력 한도에 걸림 |
| `max_turn_requests` | 한 턴에서 허용한 요청 수를 넘김 |
| `refusal` | 모델이 거절함 |
| `cancelled` | `session/cancel`, `$/cancel_request`, `session/close` 중 하나로 취소됨 |

취소한 턴은 데몬 스트림이 어떤 모양으로 끝나도 항상 `cancelled`로 답한다. 현재 턴을 외부에서 중단해도 `cancelled`다. 취소한 턴에 남은 데몬 이벤트(늦게 오는 텍스트, 도구, 계획, diff 프레임과 그 턴의 종료 신호)는 어댑터가 비우므로 다음 `session/prompt`로 넘어가지 않는다. 취소된 턴 다음의 `session/prompt`는 메시지를 보내기 전에 세션이 아직 바쁜지 데몬에 확인한다. 이 대기는 최대 약 5초다. 그때도 바쁘면 메시지를 그대로 보내고, 데몬이 거절하면 `-32603`으로 답한다. 취소한 턴이 응답한 뒤 약 10초가 지나서 도착한 데몬 이벤트는 비우지 못하고 다음 prompt의 업데이트에 섞일 수 있다. 토큰 사용량은 응답의 `_meta["horon-agent"].usage`에 담긴다.

`session/cancel`은 알림이다. 진행 중인 prompt가 없을 때 보낸 `session/cancel`은 아무 일도 하지 않는다. 요청을 취소하는 `$/cancel_request`도 두 방향으로 처리한다. 진행 중인 prompt를 가리키면 그 prompt가 응답을 하나 받고 끝난다. 그 밖의 진행 중 요청은 `-32800`으로 답한다. 모르는 id는 무시한다.

## 에디터에 보이는 것

턴 도중 `session/update`로 다음이 흐른다.

| 업데이트 | 내용 |
|---|---|
| `agent_message_chunk` | 답변 텍스트. 도구 실행 라운드가 지나면 새 `messageId`로 시작한다 |
| `agent_thought_chunk` | 모델의 추론 |
| `plan` | 할 일 목록 |
| `tool_call` | 도구 호출 시작. 종류(kind)와 제목을 담는다 |
| `tool_call_update` | 입력(`rawInput`)과 절대 경로 `locations`, 결과 텍스트, 파일 diff(절대 경로, `oldText`, `newText`), 터미널 내용 |
| `usage_update` | 컨텍스트 사용량 |
| `session_info_update` | 세션 제목 변경 |
| `available_commands_update` | 슬래시 명령 목록 |
| `config_option_update`, `current_mode_update` | 설정이나 모드가 바뀜 |

`notice`와 `compaction_update`는 클라이언트가 `session.notices`, `session.compaction`을 `clientCapabilities`로 알린 경우에만 보낸다. 알리지 않으면 보내지 않는다. 압축이 끝나면 `compactionId`는 `<턴 id>-cmp-<순번>` 모양이다.

슬래시 명령은 `/model`, `/models`, `/effort` 세 가지다. 문장 맨 앞에 공백을 하나 두면 명령으로 해석하지 않고 모델에 그대로 보낸다.

계획(`plan`)은 `todo` 도구가 있어야 나온다. 기본 설정에서는 `horon acp`가 `acp` 프로필로 데몬을 띄운 경우에만 이 도구가 있다. 데스크톱이나 `serve`가 먼저 띄운 데몬은 `core` 프로필이라 계획 업데이트가 나오지 않는다.

### acp 도구 프로필

`acp` 프로필은 도구 12개만 허용한다: `agentgrep`, `apply_patch`, `ask`, `bash`, `batch`, `edit`, `ls`, `mcp`, `read`, `replace`, `todo`, `write`.

## 권한 모드

세션은 모드 세 가지를 가진다.

| 모드 id | 동작 | 데몬 승인 설정으로 바뀌는 값 |
|---|---|---|
| `default` | 데몬 설정(`approval_mode`)을 그대로 따른다 | 세션 덮어쓰기를 해제 |
| `ask-commands` | 셸·네트워크·파일 변이 도구 앞에서 묻음 | `write` |
| `auto` | 묻지 않고 실행 | `off` |

모드는 `modes`와 `configOptions`의 첫 항목(`mode`, category `mode`) 두 곳에 나온다. 모드는 prompt가 진행 중이어도 `session/set_mode`나 `session/set_config_option`으로 바꿀 수 있고, 바꾸면 `current_mode_update`와 `config_option_update`가 나간다. 모르는 모드 id는 `-32602`다.

승인이 필요한 도구가 나오면 에디터에 `session/request_permission`을 보내고 도구는 답을 기다린다. 같은 호출이 겹치면 한 질문을 함께 쓴다.

| optionId | 결과 |
|---|---|
| `allow-once` | 이번 한 번 허용 |
| `allow-always` | 허용. 같은 도구 이름은 세션 동안 다시 묻지 않음 |
| `allow-path` | v2 전용. 이 파일을 앞으로 허용. 범위 승인으로 기록됨 |
| `allow-folder` | v2 전용. 이 파일의 폴더를 앞으로 허용. 범위 승인으로 기록됨 |
| `reject-once` | 이번 한 번 거절 |
| `reject-always` | 거절. 같은 도구 이름은 세션 동안 다시 묻지 않음 |

v1 세션은 늘 그렇던 네 옵션을 같은 순서로 받으므로 기존 에디터는 영향받지 않는다. v2 세션은 호출이 파일시스템 경로를 정확히 하나만 댈 때만 `allow-path`와 `allow-folder`를 받는다. 한 번의 답이 사용자가 보지 못한 여러 자리를 결정해서는 안 되기 때문이다. 두 범위 옵션은 `allow-always`가 쓰는 도구별 기억을 건드리지 않는다. 승인은 도구가 아니라 자리에 대한 것이기 때문이다. 승인은 데몬이 직접 저장하고(`docs/runtime/TOOLS.md`의 "범위 승인") 단일 경로 검사를 다시 하므로, 클라이언트가 자기 답의 범위를 넓힐 수 없다.

취소 결과, 모르는 optionId, 깨진 응답, 클라이언트 오류는 모두 거절로 처리한다. 다른 화면에서 먼저 결정하면 에디터의 질문은 `$/cancel_request`로 거둔다.

## 설정 옵션

| id | category | 값 |
|---|---|---|
| `mode` | `mode` | `default`, `ask-commands`, `auto`. 항상 첫 번째 |
| `model` | `model` | 제공자가 내놓는 모델 |
| `reasoning_effort` | `thought_level` | 추론 강도 |

`model`과 `reasoning_effort`는 현재 값과 선택지가 모두 있을 때만 나온다. `session/set_config_option`은 선택형 값만 받고 전체 옵션 목록을 돌려준다. boolean 값이나 문자열이 아닌 값, 모르는 id, 선택지에 없는 값, prompt가 진행 중일 때의 `model`과 `reasoning_effort` 변경은 `-32602`다.

옛 클라이언트를 위해 `session/set_model`(`modelId` 또는 `model`)과 `session/set_reasoning_effort`(`effort` 또는 `reasoningEffort`)도 받는다.

## 에디터 기능 위임

클라이언트가 해당 기능을 `clientCapabilities`로 알렸을 때만 요청을 보낸다. 알리지 않은 기능은 요청하지 않고, 아래 표의 대체 경로를 쓴다.

| 클라이언트 기능 | 알렸을 때 | 알리지 않았을 때 |
|---|---|---|
| `fs.readTextFile`, `fs.writeTextFile` | `read`, `write`, `edit`, `replace`, `apply_patch`의 텍스트 읽기와 쓰기가 `fs/read_text_file`, `fs/write_text_file`로 나간다. 저장하지 않은 버퍼도 읽는다 | 로컬 디스크를 직접 읽고 쓴다 |
| `terminal` | 포그라운드 `bash`(PTY나 백그라운드가 아닌 경우)가 `terminal/create`, `terminal/wait_for_exit`, `terminal/output`, `terminal/release`로 실행된다. 시간이 넘으면 `terminal/kill`을 보낸다. `tool_call`에 터미널이 붙는다 | 로컬에서 실행한다 |
| `elicitation.form` | `ask` 질문이 `elicitation/create` 폼(mode `form`)으로 나간다 | `ask`는 `[tools] ask_policy`를 따른다 |

폼에서 `accept`하면 고른 항목이 답이 된다. `decline`, `cancel`, 깨진 응답, 시간 초과는 빈 답으로 처리하고, 이후는 `ask_policy`가 정한다. `recommended`면 추천 항목을 고르고, `error`면 `ask` 도구가 실패한다. 질문은 비밀번호 같은 비밀 입력에 쓰지 않으며, URL 방식 폼도 쓰지 않는다.

클라이언트에 보내는 요청의 대기 시간은 파일 60초, 터미널 제어 30초, 폼 300초다. 터미널의 `wait_for_exit`은 따로 제한하지 않는다. 넘기면 `$/cancel_request`를 보낸다. 파일과 터미널 요청은 데몬에 `-32800`으로 알리고, 폼은 빈 답으로 처리한다. prompt를 취소하거나 세션을 닫을 때도 열려 있는 클라이언트 요청을 같은 방식으로 거둔다. 다만 `terminal/kill`과 `terminal/release`는 거두지 않는다.

## 세션별 MCP 서버

`session/new`, `session/load`, `session/resume`의 `mcpServers`는 그 세션과 그 ACP 연결에서만 쓴다. `stdio`, `http`, `sse`를 지원한다. 세션이 닫히거나 연결이 끊기면 stdio 자식 프로세스도 끝나고, 다른 세션과는 공유하지 않는다. `type`을 비우면 `stdio`다. 모르는 `type`이나 필수 필드가 빠진 항목은 `-32602`다.

## 다른 클라이언트와의 격리

ACP 전용 이벤트(클라이언트 요청, 권한 질문, 계획, diff)는 ACP 연결이 가진 자기 전송 경로로만 나간다. 같은 데몬에 붙은 데스크톱, 웹, `run`, `serve` 클라이언트는 이 이벤트를 받지 않는다. 그 클라이언트의 턴에서 생기는 파일, 터미널, 권한 요청이 에디터로 가는 일도 없다. ACP 연결이 끊긴 뒤에는 ACP 모드가 세션에 남지 않는다.

## ACP v2

v2는 `horon acp` 하나로 v1과 같이 제공한다. 별도 명령이나 포트는 없다. 연결마다 `initialize`의 `protocolVersion`으로 v1과 v2 중 하나를 고르고, 한 연결 안에서는 고른 쪽의 메시지만 오간다. v1 클라이언트가 받는 응답은 v2를 넣기 전과 같다.

### 버전 협상

`protocolVersion`이 없거나 정수가 아니거나 65535를 넘으면 `-32602`다. 그 밖의 값은 다음과 같이 고른다.

| `protocolVersion` | v2 켜짐(기본) | v2 꺼짐 |
|---|---|---|
| `1` | v1 | v1 |
| `2` 이상 | v2 (더 큰 값도 2로 답한다) | v1 |
| `0` | `-32022`, `data: {requested: 0, supported: [1, 2]}`. 연결은 초기화되지 않은 채로 남는다 | v1 |

v2 연결에서 `initialize`를 다시 보내면 다른 요청이 오기 전에만 받고 마지막 값을 쓴다. 다시 보낸 값이 `-32022`나 `-32602`로 실패해도 앞선 초기화는 그대로 유지된다. 다시 보낸 값이 1이면 그 연결은 v1로 바뀐다. v2를 고르면서 `info`가 빠진 요청은 거절한다.

### v2 끄기

v2 협상이 문제를 일으키면 v1만 쓰게 되돌릴 수 있다.

```toml
[acp]
protocol_v2 = false
```

환경 변수 `HORON_AGENT_ACP_PROTOCOL_V2=0`도 같다. 켜면 `1`이나 `true`를 쓴다. 꺼 두면 모든 요청에 v1처럼 답한다. 0이나 65535를 보내도 `protocolVersion: 1`이 돌아온다. 설정을 바꾼 뒤에는 `horon acp`를 새로 띄워야 적용된다.

### 광고하는 기능 (v2)

v2 `initialize` 응답은 `protocolVersion: 2`, `info`(`name: "horon-agent"`, `title: "HoronAgent"`, `version`), `capabilities`, `authMethods`로 이루어진다.

| 항목 | 값 |
|---|---|
| `capabilities.session.prompt` | `image: {}`, `audio: {}`, `embeddedContext: {}` |
| `capabilities.session.mcp` | `stdio: {}`. `http: {}`는 v1 어댑터가 `mcpCapabilities.http`를 광고할 때만 붙는다 |
| `capabilities.session` | `delete: {}`, `additionalDirectories: {}` |
| `capabilities._meta["horon-agent"]` | `profile`과 `extensions`. `extended`나 `full` 프로필에서만 나온다 |

v1과 달리 `true`/`false` 표시나 `loadSession`은 없고, `list`/`resume`/`close`도 광고 항목으로 두지 않는다. 클라이언트가 보낸 `info`와 `capabilities`는 연결마다 저장한다.

v2 연결이 받는 메서드는 `initialize`, `auth/login`, `auth/logout`, `session/new`, `session/list`, `session/resume`, `session/close`, `session/prompt`, `session/cancel`, `session/set_config_option`, `session/delete`, `tool_call/cancel`, `terminal/cancel`, `$/cancel_request`다. `session/load`, `session/set_mode`, `authenticate`, `logout`, `session/set_model`, `session/set_reasoning_effort`와 그 밖의 `_`가 아닌 메서드는 `-32601`이다. `_`로 시작하는 요청도 `-32601`이고 알림에는 답하지 않는다. `id: null` 요청은 `id: null`과 함께 `-32600`이다. v2로 협상한 연결에서 JSON 배열 프레임은 JSON-RPC 배치로 처리하며, 항목마다 일반 요청 경로를 탄다.

### 에디터에 보이는 것 (v2)

**prompt 확인.** `session/prompt`는 데몬이 사용자 메시지를 저장했다고 알리는 즉시 `{messageId}`로 답한다. 턴이 끝나기를 기다리지 않는다. `messageId`는 데몬이 저장한 메시지 id다. 저장 전에 거절되면 그 제출에는 업데이트가 없다. 알 수 없는 세션은 `-32002`, 잘못된 내용은 `-32602`, 쓸 수 있는 모델이 없거나 로그아웃 상태면 `-32000`이고, 데몬이 설정을 다시 읽는 중이면 `-32603`이다. 턴이 진행 중일 때 보낸 prompt는 거절하지 않는다. 이 연결의 앞 prompt가 아직 처리 중이면 어댑터가 받은 순서대로 보류했다가 세션이 다음 prompt를 받을 수 있을 때 데몬에 보내고, 다른 화면의 턴이 진행 중이면 데몬이 그 턴 뒤에 저장한다. 어느 쪽이든 응답은 저장된 뒤에 온다. 저장 전에 `$/cancel_request`, `session/cancel`, `session/close`가 오면 `-32800`이고 보류하거나 대기 중이던 메시지는 거둔다. 저장된 뒤에 실패하면 응답이 아니라 `stopReason: "error"`와 `error: {code, message}`를 담은 idle `state_update`로 알린다.

**입력.** prompt에는 `text`, `image`, `audio`, `resource`, `resource_link` 블록을 쓸 수 있다. `audio`는 `mimeType`이 `audio/wav`, `audio/mpeg`, `audio/mp3` 중 하나여야 하고, 모델 요청에는 `input_audio` 파트로 들어간다. 그 밖의 오디오 형식은 데몬에 보내기 전에 `-32602`다. v1 연결은 `audio`를 광고하지 않으며 지금처럼 거절한다. v2에서 빈 `prompt` 배열은 `-32602`이고, v1은 빈 배열을 지금처럼 받는다.

**상태 업데이트.** 세션의 포그라운드 턴은 이 에디터가 시작했든 다른 화면이 시작했든 에이전트가 스스로 시작했든 `state_update`로 보고한다. 순서는 시작할 때 `running`, 권한이나 질문 답을 기다리는 동안 `requires_action`, 답을 받은 뒤 첫 이벤트 전에 다시 `running`, 끝나면 `idle`이다. `idle`에는 `stopReason`(`end_turn`, `max_tokens`, `max_turn_requests`, `refusal`, `cancelled`, `error`)이 붙고, 원인을 모르면 생략한다. `idle`은 그 턴의 다른 업데이트가 모두 나간 뒤에 한 번만 온다. `session/cancel`은 남은 업데이트를 보낸 뒤 `idle`(`cancelled`)로 끝난다. 어댑터가 보고 있는 턴의 run id를 알면 데몬에 `cancel_turn{run_id}`를 보내므로, 그 사이 다른 화면이 새로 시작한 턴은 멈추지 않는다. run id를 아직 모르면(이 에디터의 prompt가 `running`을 보고하기 전) 기존 `cancel`을 보낸다. `session/close`는 진행 중인 prompt를 같은 방식으로 취소하고, 마지막 업데이트와 `idle`(`cancelled`)을 닫기 응답보다 먼저 보낸다. 접수했지만 아직 `running`을 보고하지 않은 prompt를 닫을 때도 같은 순서다. `usage_update`는 데몬이 사용량을 알린 자리에 다른 업데이트와 섞이지 않게 나간다.

**메시지 id.** 모든 `*_message_chunk`와 통째 메시지 업데이트에 `messageId`가 있다. 에디터가 보낸 prompt의 사용자 메시지는 `user_message` 하나로 한 번 보고하며, id는 prompt 응답의 `messageId`와 같다. 다른 화면이 시작한 턴의 사용자 메시지도 보고한다. 단, 표시 역할 없이 저장된 행만 대상이고 `[auto]` 시스템 알림은 사용자 메시지가 아니다. 턴 도중에 끼어든 소프트 인터럽트는 데몬이 행을 저장하는 즉시 그 행 id를 `messageId`로 한 `user_message`로 온다. 답변과 추론 id는 턴을 시작한 메시지의 저장 id에서 `<id>:a<n>`, `<id>:t<n>` 형태로 만든다. `n`은 도구 실행 라운드마다 늘어난다. 기록을 재생해도 같은 규칙으로 계산하므로 실시간 id와 재생 id가 같다.

**도구 호출.** v2에는 `tool_call` 업데이트가 없다. 한 `toolCallId`의 첫 `tool_call_update`가 호출을 만들고, 그때 `title`, `name`, `kind`, `status: "pending"`을 담는다. 입력이 완성되면 `rawInput`, 절대 경로 `locations`, bash 명령이 이후 업데이트로 채워진다. 내용은 `tool_call_content_chunk`로 오고, 값을 생략한 필드는 바뀌지 않는다. 실행을 시작한 호출은 `capabilities: {cancel: {}}`를 광고하고, 최종 상태가 되면 `capabilities: null`로 거둔다. `tool_call/cancel {sessionId, toolCallId}`는 광고 중인 그 호출 하나만 멈추고, 데몬이 취소를 시작했다고 답하면 바로 `{}`로 답한다. 그 호출은 `status: "cancelled"`로 끝나고 턴은 계속된다. 모르는 호출, 이미 끝난 호출, 광고하지 않은 호출은 데몬에 보내지 않고 `-32602`다. 턴이 취소될 때 실행 중이던 도구는 `status: "cancelled"`로 끝난다. `session/close`가 제한 시간 안에 턴을 멈추지 못해도 열린 도구는 `cancelled`로 끝나고, 데몬 연결이 끊기면 `failed`로 끝난 뒤 idle `error`가 온다.

**구조화된 diff.** 파일 편집은 `{type: "diff", changes: [{operation, path, oldPath, fileType: "text"}], patch: {format: "git_patch", text}}`로 간다. `operation`은 `add`, `modify`, `delete`, `move`이고 `oldPath`는 `move`에만 붙는다. `apply_patch`의 `Add File`처럼 호출 전에 없던 파일은 `add`이고, 패치에 `new file mode`와 `--- /dev/null`이 들어간다. 경로는 절대 경로이며 `a/`, `b/` 접두어가 없고, 없는 쪽은 `/dev/null`이다. v1과 달리 `oldText`/`newText`는 보내지 않는다.

**표시용 터미널.** 포그라운드 `bash`는 에이전트가 소유한 표시 전용 터미널로 보인다. 내용에 `{type: "terminal", terminalId}`가 들어가고, 명령이 정해지면 `terminal_update {terminalId, command, cwd}`가 오며, 실행 중 출력은 `terminal_output_chunk {terminalId, data}`로 실시간으로 온다. 끝나면 `terminal_update {terminalId, output: {data}, exitStatus: {exitCode, signal}}`가 온다. `data`는 base64로 인코딩한 값이다. 에디터에 터미널을 만들라고 요청하는 일은 없다.

**백그라운드 작업.** 도구 호출이 모델에 제어를 돌려준 뒤에도 세션에서 계속 도는 작업은 그 세션의 모든 `state_update`(`running`, `requires_action`, `idle`, `session/resume`의 현재 상태)에 `background` 목록으로 붙는다. 없으면 `background: []`다. 백그라운드로 넘어간 `bash`는 터미널(`{type: "terminal", terminalId}`)로, 다른 도구는 그 호출(`{type: "tool_call", toolCallId}`)로 넣는다. 데몬의 백그라운드 작업 관리자가 맡은 작업만 넣고, monitor, 예약된 ambient 실행, swarm 대기처럼 세션을 관리하는 작업은 넣지 않는다. 항목은 목록에 넣기 전에 `terminal_update`나 `tool_call_update`로 먼저 알리고, 빼기 전에 최종 상태(`exitStatus` 또는 `completed`·`failed`·`cancelled`)를 보낸다. 목록이 바뀌면 같은 상태를 바뀐 목록과 함께 다시 보내며 새 `stopReason`은 붙이지 않는다. 세션이 `idle`일 때 항목을 빼는 갱신은 최대 250 ms 미룬다. 그 사이 새 턴이 시작하면 항목을 뺀 `running` 하나로 보내고, 아니면 항목을 뺀 `idle`을 보낸다. 백그라운드 터미널은 `capabilities: {cancel: {}}`를 광고하고, `terminal/cancel {sessionId, terminalId}`는 그 프로세스를 멈춘 뒤 `{}`로 답한다. 목록에 없거나 이미 끝난 터미널은 데몬에 보내지 않고 `-32602`다.

**권한 요청.** 승인이 필요한 도구는 `session/request_permission {sessionId, title, description, subject, options}`로 묻는다. `subject`는 `bash`면 `{type: "command", command, cwd, toolCallId}`, 다른 도구면 `{type: "tool_call", toolCall}`이다. 선택지와 항상 허용·항상 거절의 기억 방식은 v1과 같고, 취소와 모르는 결과나 optionId는 승인으로 치지 않는다.

**계획.** 할 일 목록은 `plan_update {plan: {type: "items", planId: "todo", entries}}`로 오고, 업데이트마다 항목 전체를 바꾼다.

**설정 옵션과 슬래시 명령.** 옵션은 `configId`와 `groupId`를 쓰고 `mode`(category `mode`)가 항상 첫 번째다. `session/set_config_option`은 문자열 값을 가진 `type: "id"` 또는 boolean을 가진 `type: "boolean"`을 받고 전체 목록을 돌려준다. boolean 옵션이 없으므로 boolean 값은 `-32602`다. 변경은 `current_mode_update`가 아니라 `config_option_update`로 알린다. 슬래시 명령은 `type: "command"`와 `input: {type: "text", hint}`를 담는다. `session/new`와 `session/resume` 응답에는 `configOptions`와 `availableCommands`가 들어가며(비어 있으면 필드를 생략한다) `modes`는 없다.

**세션 설정.** `session/new`와 `session/resume`은 절대 경로 `cwd`와 선택 항목 `additionalDirectories`(매번 전체 목록), `mcpServers`를 받는다. `mcpServers`의 각 항목은 `type`이 `stdio`나 `http`여야 한다. `sse`나 빠진 `type`, 광고하지 않은 `http`는 `-32602`다. `session/resume`에 `replayFrom`이 없거나 `null`이면 기록 없이 붙고, `{type: "start"}`이면 응답 전에 대화 전체를 `user_message`, `agent_thought`, `agent_message`, 완료 상태 `tool_call_update`로 다시 보낸다. 다른 cursor는 `-32602`다. `session/list`는 `{sessions: [{sessionId, cwd, title?, updatedAt?, additionalDirectories?}], nextCursor?}`를 돌려주고, 잘못된 cursor는 `-32602`다. `sessionId`를 받는 메서드에 모르는 세션을 보내면 `data.sessionId`를 담은 `-32002`다. v1 세션을 v2로 다시 열면 저장된 id로 재생한다. 이미 떠 있는 데몬이 v2를 모르는 이전 빌드면 `session/new`와 `session/resume`은 데몬을 재시작하라는 `-32603`으로 답한다. 다른 클라이언트가 쓰고 있을 수 있으므로 데몬을 직접 끄지는 않는다.

**요청 id.** 아직 답하지 않은 요청과 같은 id로 다시 요청하면 그 요청은 `-32600`이다. batch 안에서 id가 겹쳐도 같고, batch 응답 배열은 항목마다 답을 하나씩 담아 항상 나간다. `session/close`가 시작된 뒤 도착한 `session/prompt`는 데몬에 전달하지 않고 `-32603`으로 답한다.

**질문.** `ask` 질문은 클라이언트가 `capabilities.elicitation.form`을 객체로 알렸을 때만 `elicitation/create`(form 방식, `sessionId` 포함)로 나가고, 기다리는 동안 `requires_action`을 보고한다. 데몬이 질문을 낸 `ask` 호출을 먼저 알려 주면 `toolCallId`도 싣는다. `accept`는 답이 되고 `decline`, `cancel`, 알 수 없는 action은 v1처럼 `[tools] ask_policy`를 따른다.

### 연결 단위 `auth/logout`

`authMethods`에는 `type: "agent"`인 `horon-agent-credentials` 하나만 있다. 클라이언트가 `capabilities.auth.terminal`을 알려도 터미널 방식은 붙지 않는다. `auth/login {methodId}`는 v1의 `authenticate`와 같다. 쓸 수 있는 모델이 없으면 `-32000`이고, 모르는 `methodId`는 `-32602`다.

`auth/logout {}`은 그 연결의 인증 상태만 끝낸다. 저장된 자격 증명은 지우지 않으며, 테스트가 격리된 홈의 자격 증명 파일이 바이트 단위로 같음을 확인한다. 같은 연결에서 `auth/login`이 다시 성공하기 전까지 `session/new`, `session/resume`, `session/prompt`는 `-32000`으로 답한다.

### v2에서 하지 않는 것

- 클라이언트에 `fs/*`나 `terminal/create`를 비롯한 `terminal/*` 요청을 보내지 않는다. v2 안정판에서 에이전트가 클라이언트에 보내는 메서드는 `session/request_permission`, `session/update`, `elicitation/create`, `elicitation/complete` 넷뿐이다. 에디터의 파일을 읽고 쓰려면 에디터가 제공하는 MCP 서버를 `mcpServers`로 붙이고, 터미널은 위의 표시용 터미널로만 보인다.
- 초안(draft)이나 unstable 단계에만 있는 메서드와 기능, 예를 들어 `session/fork`나 `providers/*`는 지원하지 않는다. 광고하지 않으므로 `-32601`이다.
- URL 방식 질문(`elicitation.url`)은 보내지 않는다. 데몬의 `ask` 질문은 선택지를 고르는 form 질문뿐이라 URL로 보낼 질문이 없다. 그래서 URL 질문이 끝났다고 알리는 `elicitation/complete`도 보내지 않는다.
- 도구 상태 `paused`는 보내지 않는다. 데몬에는 시작한 도구를 멈춰 두는 상태가 없다.
- 빈 `prompt` 배열은 v2에서만 `-32602`다. v1은 이전과 같이 받는다.
- SSE 방식 MCP 서버는 `-32602`로 거절한다.

### v2의 알려진 한계

- horon-agent를 이전 빌드로 내렸다가 다시 올리면, 세션 캐시가 다시 만들어질 때까지 `session/resume`의 재생이 모든 사용자 행을 턴의 시작으로 취급한다. 이전 빌드가 저장한 턴은 재생에서 만든 `messageId`가 실시간으로 본 id와 다를 수 있다.
- 모델 엔드포인트가 스트리밍을 지원하지 않으면 응답이 끝나기 전에 부분 텍스트가 오지 않는다. 그래서 응답이 한꺼번에 도착한 것처럼 보일 수 있다.
- 공식 acp-tck의 v2 조건부 항목(패치, 권한, 터미널, 계획 경로)은 TCK가 항상 `"hi"`만 보내기 때문에 건너뛴다. 이 경로는 `tests/acp_stdio_v2.rs`가 대신 검증한다.

### v2 적합성 검증

```bash
cargo test --locked -p horon-agent --test acp_stdio_v2     # v2 통합 시나리오
bash scripts/test/acp_tck.sh --protocol-version 2               # 공식 acp-tck v2
cargo test --locked -p horon-agent --test acp_tck -- --ignored --test-threads=1   # v1과 v2 TCK
```

`tests/acp_stdio_v2.rs`는 v2 시나리오 IS-1부터 IS-21까지와 v1·v2 병행 시나리오를 돌리고, 프레임을 `tests/fixtures/acp/schema-v2.0.0-alpha.13.json`으로 검증한다. IS-16은 `tool_call/cancel`, IS-17은 `terminal/cancel`과 `background` 목록, IS-18은 `terminal_output_chunk`, IS-19는 오디오 prompt, IS-20은 실시간 소프트 인터럽트, IS-21은 작업 중 prompt 보류를 실제 데몬으로 확인한다. `scripts/test/acp_tck.sh`는 `--protocol-version 1|2`를 받고 기본값은 v1이다. 공식 acp-tck는 자체 v2 스키마 사본(alpha.9, unstable)으로 판정하므로 TCK 결과는 alpha.13 적합성의 근거가 아니다. alpha.13으로 바꾸기 전 통합 브랜치에서 TCK 테스트 두 개(v1, v2)가 모두 통과했고 판정은 둘 다 적합(conformant)이었다. 그때 v2 필수 항목은 24건 통과, 기능 항목은 49건 통과에 16건 건너뜀이며 실패는 없었다. v1 필수 항목은 31건 통과였다.

## 설정

`config.toml`의 `[acp]`:

```toml
[acp]
profile = "standard"   # standard | extended | full
tool_profile = "acp"   # acp가 데몬을 직접 띄울 때 요청하는 도구 프로필
protocol_v2 = true     # false면 모든 클라이언트에 v1로만 답한다
```

`protocol_v2`는 v2 협상 여부를 정한다. 기본값은 `true`다.

`standard`는 스펙에 맞는 메시지만 보낸다. `extended`와 `full`은 무시해도 되는 `_horon_agent/*` 확장 알림을 더 보낸다. 환경 변수 `HORON_AGENT_ACP_PROFILE`, `HORON_AGENT_ACP_TOOL_PROFILE`, `HORON_AGENT_ACP_PROTOCOL_V2`가 같은 값을 덮어쓴다.

## 알려진 한계

- v1 연결은 `logout`을 지원하지 않는다. v2 연결의 `auth/logout`은 [연결 단위 `auth/logout`](#연결-단위-authlogout)을 본다.
- v1 연결은 `audio` 입력을 광고하지 않는다. `systemPrompt` 세션 기능은 v1과 v2 모두 광고하지 않는다.
- v2의 한계는 [v2의 알려진 한계](#v2의-알려진-한계)에 따로 적었다.
- URL 방식 elicitation은 쓰지 않는다.
- 터미널 위임은 포그라운드, 비PTY, 비백그라운드 `bash` 호출에만 적용한다.
- 모델 엔드포인트가 스트리밍을 지원하지 않으면 응답이 끝나기 전에 부분 델타가 오지 않는다. 그래서 응답이 한꺼번에 도착하는 것처럼 보일 수 있다.
- `session/delete`는 보관 처리라서 디스크 파일은 남는다.
- stdin이 닫히면 진행 중이던 요청은 응답을 잃는다.

## 적합성 검증

`cargo test --locked -p horon-agent --test acp_stdio`는 통합 시나리오 IS-1부터 IS-15까지 한 개씩, 테스트 15개를 돌린다. 모든 프레임은 `tests/fixtures/acp/schema-v1.26.0.json`으로 검증한다.

```bash
# 공식 acp-tck v1 스위트 (고정 커밋, 격리된 데몬)
bash scripts/test/acp_tck.sh
ACP_TCK_REQUIRE_PASS=<요구사항 id,...> bash scripts/test/acp_tck.sh
```

`scripts/test/acp_tck.sh`는 `uv`, `git`, `cargo`, `python3`가 필요하고, 처음 실행하면 스위트를 내려받는다. 보고서의 판정이 적합(conformant)이고, 인증이나 버전 불일치로 막히지 않았으며, 필수(MANDATORY) 항목의 실패와 미실행, 기능(CAPABILITY) 항목의 실패가 없을 때만 0으로 끝난다. `ACP_TCK_REQUIRE_PASS`(또는 `--require-pass`)에 요구사항 id를 쉼표로 나열하면, 그 항목이 모두 PASS여야 통과한다.

같은 판정을 Rust 테스트로도 돌릴 수 있다.

```bash
cargo test --locked -p horon-agent --test acp_tck -- --ignored --nocapture
```

세션 생명주기와 인증은 `tests/acp_lifecycle.rs`가, 테스트 지원 코드는 `tests/acp_infra.rs`가 다룬다. 공식 스키마 사본과 출처는 `tests/fixtures/acp/README.md`에 있다.
