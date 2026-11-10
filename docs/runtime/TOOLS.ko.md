# 도구

언어: [English](TOOLS.md) | **한국어**

도구 로딩 단계, 효과와 승인 등급, 편집 안전 장치, `read`에 붙는 상위 `AGENTS.md`, 스킬 읽기(`skill://`), 기억 읽기(`memory://`)와 기억 갱신, MCP 설정 출처, 큰 도구 출력의 응축과 `read` 중복 단락, 오래된 결과 축약, 1회 보조 호출을 다룬다.

## 도구 발견과 지연 로딩

`full` 프로필은 내장 도구를 모두 노출하지만 코어 밖 도구의 스키마를 요청의 캐시 접두사에 넣지 않습니다. 기본 `core` 프로필도 코어 열다섯 도구 가운데 일곱 개만 스키마를 싣고, 나머지는 시스템 프롬프트의 색인에 이름만 적습니다. `LoadMode` 세 단계가 각 도구를 즉시 보낼지, 지연할지, 뺄지 정하고 `toolbox`가 지연되거나 색인에만 있는 도구에 닿는 창구입니다. 이 문서는 현재 트리의 동작을 설명합니다.

### 세 단계

`LoadMode`는 `crates/base/src/config.rs:825`에 정의되어 있습니다.

| 값 | 의미 |
| --- | --- |
| `essential` | 요청에 즉시 실리고 참조 없이 바로 호출할 수 있습니다. 기본값입니다. |
| `discoverable` | 지연 정의로 보냅니다. 스키마는 도구 참조가 그 이름을 가리킬 때 모델에 도달합니다. 네이티브 지연 로딩이 없는 프로바이더는 이 정의를 대신 버리고, `core` 세션에서는 색인이 그 이름을 알립니다. |
| `off` | 요청에 보내지 않습니다. |

`discoverable`은 MCP 도구를 이미 옮기는 프로바이더 네이티브 지연 로딩을 재사용합니다. 정의는 요청의 도구 목록에 실리지만 캐시되는 시스템 프롬프트 접두사 밖에 있으므로, 접두사 바이트를 쓰지 않고 도구가 늘거나 줄어도 프롬프트 캐시를 무효화하지 않습니다.

개별 도구의 단계는 `[tools.load_mode]` 표로 덮어씁니다. 이름은 정규화되고, 이 빌드에 없는 이름은 아무 효과가 없습니다.

```toml
[tools.load_mode]
batch = "off"
memory = "essential"
```

### 기본 배치

- `ESSENTIAL_TOOLS`의 일곱 도구(`read`, `write`, `edit`, `apply_patch`, `bash`, `ls`, `agentgrep`)는 어떤 프로필에서도 `essential`입니다 (`config.rs:803` `ESSENTIAL_TOOLS`). 거의 모든 코딩 턴이 쓰는 도구라 스키마가 캐시 접두사에 남습니다.
- `CORE_TOOLS`는 여전히 열다섯 개이고 `core` 프로필의 허용 목록입니다 (`config.rs:779` `CORE_TOOLS`). 목록이 도달 가능성을 정하고, 즉시 보낼지는 세션이 목록 모드인지에 따라 갈립니다.
- `core` 세션이 목록 모드가 되는 조건은 셋입니다. 프로필이 손대지 않은 기본 `core`이고, 허용 목록이 명시되지 않았고, `toolbox`가 닿을 수 있어야 합니다 (`crates/app-core/src/agent.rs:657`, `config.rs:1237` `ToolConfig::toolbox_reachable`). 이 세션은 일곱 도구와 `toolbox`만 스키마와 함께 보내고, 나머지 여덟 코어 도구(`replace`, `webfetch`, `websearch`, `skill_manage`, `mcp`, `monitor`, `undo`, `ask`)는 `discoverable`이 되어 색인에 이름만 올립니다 (`config.rs:889` `ToolLoadTiers::tier`).
- 목록 모드가 아닌 세션은 코어 도구를 모두 `essential`로 보냅니다. `acp`, `minimal`/`lite`, `none`, 명시적 `enabled` 목록, SDK `enabled` 목록, `new_with_session`에 넘긴 허용 목록이 여기에 듭니다. 이 세션들의 도구 목록과 단계는 바뀌지 않았고 색인도 없습니다. 목록 모드의 `core` 세션에 SDK `enabled`가 붙으면 클라이언트가 도구를 정확히 지정한 것이므로 목록 모드가 꺼지고 지정한 도구가 모두 즉시 실립니다 (`crates/app-core/src/agent/turn_execution.rs:800` `apply_builtin_load_tiers`).
- `mcp_search`와 `mcp_call`은 기본적으로 즉시 보냅니다 (`config.rs:861` `ALWAYS_EAGER_TOOLS`). 이 둘이 MCP 스키마를 발견하는 통로라서 네이티브 지연 경로에서는 그대로 즉시 보냅니다. 네이티브 지연 로딩이 없는 프로바이더에서는 목록 모드 `core` 세션이 첫 도구 목록을 확정할 때 활성 MCP 서버가 하나도 설정되어 있지 않으면, 두 도구는 즉시 보내지 않고 색인에 올라 `toolbox`로 부릅니다. 서버가 하나라도 있으면 지금처럼 즉시 보냅니다. 이 판단도 세션 중에 바뀌지 않습니다. 네이티브 프로바이더로 확정된 세션은 동결된 색인에 두 도구가 없으므로, 나중에 네이티브가 아닌 프로바이더로 바꿔도 두 도구는 즉시 실린 채로 남습니다(색인에 없는 이름을 안내하지 않기 위해서입니다). `mcp list`와 연결 결과 문구, 새 MCP 도구 공지가 32개를 넘겨 나머지를 줄여 알리는 끝 문장은 두 도구가 색인에 오른 세션에서만 `toolbox`를 가리키고, 그 밖에는 직접 호출을 안내합니다 (`turn_execution.rs:648` `freeze_mcp_surface`, `turn_execution.rs:681` `mcp_surface_listed`). `mcp_tools = "eager"`이면 서버별 정의가 실리므로 이 규칙은 적용되지 않습니다.
- `toolbox`는 언제나 `essential`입니다. 명시적 `discoverable` 항목은 `essential`로 바뀌고, `off` 항목은 도구를 뺍니다 (`config.rs:889` `ToolLoadTiers::tier`).
- 지연 기본값은 명시적 `full` 프로필에 허용 목록이 없을 때만 켜집니다. `acp`, `minimal`/`lite`, `none`, 명시적 `enabled` 목록, 프로필 미설정은 통과한 도구를 즉시 보냅니다 (`config.rs:1210` `ToolConfig::load_tiers`).
- 세션은 시작할 때 단계를 한 번 확정합니다. 설정을 다시 읽어도 세션 중간에 도구가 즉시 집합과 지연 집합 사이를 옮기지 않습니다.

`irc`는 코어 밖 도구입니다. `[tools] irc = true`일 때 `core`, `acp`, `minimal`, 명시 `enabled` 목록처럼 비어 있지 않은 허용 목록에 들어가고, 허용 목록이 없는 `full` 세션에는 원래부터 보입니다. 스위치가 꺼져 있으면 등록은 유지되지만 실행이 거부되므로 `full` 세션에서도 실제로 쓸 수 없습니다. 빈 허용 목록(`none`)은 켜져 있어도 그대로 비어 있습니다. 메일박스 동작과 설정은 [`IRC.ko.md`](IRC.ko.md)에 있습니다.

#### 즉시 보내도록 되돌리기

- `[tools.load_mode] ask = "essential"`처럼 적으면 그 도구는 목록 모드 세션에서도 스키마와 함께 실립니다. 명시 항목이 기본 배치보다 우선합니다.
- `monitor = "off"`는 그 도구를 요청에서도 색인에서도 뺍니다.
- `toolbox = "off"`로 두거나 `[tools] disabled`에 `toolbox`를 넣으면 목록 모드가 꺼지고 코어 열다섯 도구가 모두 다시 즉시 실립니다. 닿을 방법이 없는 도구를 만들지 않기 위한 동작입니다.

### 목록 색인

목록 모드 `core` 세션의 시스템 프롬프트 정적 부분에는 `# Additional tools` 절이 붙습니다. 짧은 안내 문장 하나와 `- <이름>: <한 줄 설명>` 행이 이름순으로 나옵니다 (`crates/base/src/prompt.rs:483` `build_additional_tools_section`). 안내 문장은 한 문장이고, 목록의 도구를 `toolbox`의 `run` 동작으로 부르는 방법만 알립니다. 행의 설명은 도구 설명의 첫 문장이거나 60자까지이므로, 매개변수는 모델이 `toolbox describe`로 따로 받습니다.

- 프로바이더와 상관없이 같은 색인을 씁니다. 네이티브 지연 로딩이 있는 프로바이더도 지연 정의와 함께 이 색인을 받습니다. 없는 프로바이더는 지연 정의를 버리므로 이 색인이 이름을 알리는 유일한 경로입니다.
- 행은 세션이 `toolbox list`로 보게 될 집합과 같은 기준으로 거릅니다. `mcp__*` 프록시와 `toolbox` 자신은 빠지고, 세션 정책으로 `toolbox`가 거부할 도구(허용 목록 밖, 비활성, SDK가 끈 도구)는 올라가지 않습니다 (`turn_execution.rs:897` `build_listed_tools_index`, `crates/app-core/src/tool/toolbox.rs:232` `session_can_reach`).
- 색인은 첫 도구 목록이 확정될 때 한 번 굳습니다. MCP 서버 등록 전의 워밍업 요청은 색인을 굳히지 않고, 확정 시점에 한 번 다시 만든 뒤 고정합니다. 이후 도구 잠금 해제, 압축, MCP 연결, 프로바이더 전환, 설정 재로드로는 바뀌지 않습니다 (`turn_execution.rs:834` `ensure_listed_tools_index`).
- 세션을 복원할 때만 색인이 초기화되어 새 세션 기준으로 다시 만들어집니다. `/clear`로 대화를 지워도 같은 세션이므로 색인이 유지됩니다 (`crates/app-core/src/agent.rs:1192` `reset_listed_tools_index`).
- SDK가 시스템 프롬프트를 통째로 덮어써도 색인은 그 뒤에 붙습니다 (`crates/app-core/src/agent/prompting.rs:175`). 덮어쓴 세션도 목록 도구의 이름을 잃지 않습니다.
- 한 줄 설명은 도구 설명의 첫 문장까지만 쓰고, 그 문장이 60자를 넘으면 60자에서 줄임표로 자릅니다. 색인 행과 `toolbox list` 행이 같은 헬퍼를 공유하므로 두 목록은 같은 문구를 보여 줍니다 (`crates/base/src/prompt.rs`의 `one_line_description`). 색인에서 도구를 고른 모델은 `toolbox describe`로 스키마를 받고 `toolbox run`으로 실행합니다.
- `todo` 안내 문장은 허용 목록에 `todo`가 있는 세션(`acp` 등)에만 붙고, 목록 모드 `core`에는 `todo`가 없으므로 붙지 않습니다 (`prompting.rs:233` `append_todo_hint`). 두 가지가 함께 붙으면 안내 문장이 색인보다 앞에 옵니다.

### toolbox

`toolbox`는 `full`과 `core` 프로필에서 등록됩니다 (`crates/app-core/src/tool/toolbox.rs:55` `enabled`). `acp`를 포함한 다른 프로필은 닫힌 허용 목록을 유지하므로 `toolbox`가 아예 없습니다. 허용 목록이 있는 `full` 세션에서도 그 목록이 도달 범위를 다시 좁힙니다. 스웜 작업자처럼 목록 모드도 `full`도 아닌 세션의 허용 목록에서는 `toolbox`가 빠집니다 (`agent.rs`의 `without_unreachable_dispatcher`).

세 동작이 있습니다.

- `list`: 이 세션에 지연되었거나 색인에 오른 도구의 이름과 한 줄 설명을 보여 줍니다. 자기 자신과 MCP 표면(`mcp__*`, 즉시 실린 `mcp_search`/`mcp_call`)은 빼고, 이 세션에서 닿을 수 있는 도구만 이름순으로 나열합니다 (`toolbox.rs:282` `list`).
- `describe`: 이름을 정규화한 뒤 설명과 입력 스키마를 텍스트로 돌려줍니다 (`toolbox.rs:318` `describe`). 스키마가 요청의 도구 목록에 없는 프로바이더에서도 모델이 매개변수를 알 수 있는 경로입니다.
- `run`: 대상 도구를 `Registry::execute`로 실행합니다 (`toolbox.rs:336` `run`). `toolbox` 자신은 실행할 수 없고 `args`는 JSON 객체여야 합니다.

#### 정책은 대상 도구에 한 번 적용됩니다

`toolbox run`은 직접 호출과 같은 정책을 받되, 바깥 호출이 아니라 대상 도구에 한 번만 적용됩니다. 계획 모드, 승인 요청, `pre_tool`/`post_tool` 훅은 안쪽 `Registry::execute`에서 대상 도구의 이름과 입력으로 돌아갑니다. 바깥 `toolbox` 호출에서는 이 단계를 건너뛰므로 승인을 두 번 묻거나 훅이 `toolbox`라는 이름을 받는 일이 없습니다 (`crates/app-core/src/tool/mod.rs:2144`). 허용 목록과 비활성 목록 검사는 바깥에도 남아서 `toolbox` 자신이 닿을 수 있는지 판단합니다.

계획 모드가 읽기 전용 호출을 가르는 기준도 대상을 따라갑니다. `list`와 `describe`는 읽기 전용이고, `run`은 대상 도구가 선언한 효과를 쓰며, 대상을 알 수 없거나 동작이 올바르지 않으면 쓰기 장벽으로 취급합니다 (`toolbox.rs:132` `effects_for_input`). 그래서 계획 모드에서 `toolbox run write`는 `write` 직접 호출처럼 거부되고, `toolbox run ls`는 통과합니다.

`off` 단계의 도구, 세션 허용 목록 밖의 도구, 비활성 목록에 든 도구는 `list`에 나오지 않고 `describe`와 `run`도 거부합니다.

#### 참조와 다음 턴

`describe`와 `run`은 결과 메타데이터에 `tool_references`를 넣습니다. 트랜스크립트 변환이 이를 `ContentBlock::ToolReference` 하나로 바꿉니다. 네이티브 지연 로딩이 있는 프로바이더는 그 참조로 다음 턴에 지연 스키마를 불러오므로 직접 호출이 가능해집니다. 없는 프로바이더에는 이 승격 효과가 없습니다. 이 경우 모델은 계속 `toolbox run`으로 부르면 됩니다.

#### 클라이언트에서 보이는 모습

목록 모드 `core` 데몬에서는 `replace`, `webfetch`, `ask` 같은 목록 도구가 `toolbox` 호출로 나타납니다. ACP 편집기는 도구 이름으로 종류를 정하는데 `toolbox`는 어느 분류에도 없어서 `other`로 표시됩니다 (`src/cli/acp/util.rs:286` `tool_kind`). 편집기가 `ask`를 직접 호출로 받아 렌더링하는 흐름은 `acp` 프로필 세션에서만 기대할 수 있습니다.

#### 호스티드 검색

모델 요청은 OpenAI 호환 `/chat/completions`로 직접 나가고 요청 본문에 호스티드 도구를 싣지 않으므로, 에이전트가 OpenAI 호스티드 `web_search`를 붙이는 경로는 없습니다. `websearch`는 로컬 도구로만 동작합니다.

### core가 닫혀 있는 이유

`core`는 허용 목록입니다. 기본 headless 표면을 코어 열다섯 도구와 `toolbox` 하나로 고정하고, 나머지 모듈은 트리에 두되 노출하지 않습니다. 허용 목록은 닫힌 채 그대로이고 바뀐 것은 전달 방식뿐입니다. 일곱 도구는 스키마와 함께, 여덟 도구는 색인과 `toolbox`로 닿습니다. 마지막으로 합류한 `monitor`는 파일·명령 감시를 재시작 뒤에도 이어 가는 도구입니다. 이 선택은 세 가지를 지킵니다.

- 기본 표면의 크기가 바뀌지 않아 요청 접두사와 프롬프트 캐시가 안정적입니다.
- 쓰기와 셸을 포함한 기본 권한 범위가 예측 가능합니다.
- 새 도구가 늘어도 headless 기본값이 조용히 넓어지지 않습니다.

넓히려면 `--tool-profile full`, `HORON_AGENT_TOOL_PROFILE=full`, `[tools] profile = "full"` 중 하나를 명시해야 합니다.

### promote가 없는 이유

`toolbox`는 스키마를 참조로 불러올 뿐입니다. 세션의 도구 정의를 고치지 않고, 트랜스크립트를 쓰지 않으며, 지연 도구를 즉시 집합으로 승격하지 않습니다. 승격 동작을 두면 세션 중간에 도구 목록이 바뀌어 프롬프트 캐시가 무효화되고, `full` 프로필의 지연 배치가 무의미해집니다. 그래서 `list`, `describe`, `run` 세 동작만 있고 `promote`는 없습니다.

## 도구 효과, 계획 모드, 승인 등급

도구 하나가 한 라운드에서 무엇을 건드리는지를 선언하는 비트 마스크와, 그 선언을 읽어 동시 실행 여부를 정하는 규칙, 계획 모드의 상태 기계, 그리고 승인 등급이 언제 개입하는지를 설명합니다. 세 장치는 같은 선언 하나를 공유합니다. 도구가 선언한 효과가 동시 실행의 근거이고, 계획 모드의 거절 근거이고, 승인 등급의 판단 근거입니다.

### 효과 비트

`ToolEffects`는 다섯 비트를 가진 `u8` 래퍼입니다 (`crates/tool-core/src/lib.rs:166`).

| 비트 | 값 | 뜻 |
| --- | --- | --- |
| `READ` | `1 << 0` | 파일이나 상태를 읽습니다. |
| `WRITE` | `1 << 1` | 파일을 덮어씁니다. |
| `APPEND` | `1 << 2` | 기존 내용 뒤에 덧붙입니다. |
| `NETWORK` | `1 << 3` | 바깥으로 요청을 보냅니다. |
| `PROCESS` | `1 << 4` | 프로세스를 띄웁니다. |

`BARRIER`는 `WRITE | APPEND | PROCESS`입니다 (`crates/tool-core/src/lib.rs:176`). `NETWORK`는 일부러 빠져 있습니다. 두 번의 조회는 서로 간섭하지 않고, 유료 호출 횟수는 안전 문제가 아니라 예산 문제이기 때문입니다. 그래서 읽기와 네트워크만 선언한 호출은 같은 라운드에서 겹쳐 돌 수 있습니다 (`parallel_safe`, `crates/tool-core/src/lib.rs:223`).

### 동시 실행 등급

`ConcurrencyClass`는 `Ordered`와 `Concurrent` 둘입니다 (`crates/tool-core/src/lib.rs:151`). 등급은 효과에서 파생되며 따로 선언하지 않습니다 (`crates/tool-core/src/lib.rs:270`).

`Tool` 트레이트의 `effects` 기본값은 쓰기 배리어입니다 (`crates/tool-core/src/lib.rs:255`). 아무것도 선언하지 않은 도구는 겹쳐 돌 수 없다고 봅니다. 두 방향의 비용이 다르기 때문입니다.

| 잘못된 선언 | 결과 |
| --- | --- |
| `Concurrent`인데 실제로는 씁니다 | 한 파일에 겹친 두 쓰기, 한 작업 트리에 겹친 두 셸이 서로의 변경을 덮어씁니다. 마지막 기록자가 조용히 이깁니다. 사용자 데이터가 사라집니다. |
| `Ordered`인데 실제로는 읽기만 합니다 | 호출은 그대로 실행됩니다. 혼자 돌 뿐이라 느려질 뿐입니다. |

읽기 전용이고 상태를 바꾸지 않는 도구만 `Concurrent`를 선언합니다.

`irc`는 파일을 쓰지 않지만 `effects()`를 재정의하지 않아 같은 기본 쓰기 배리어를 선언합니다. 그래서 한 라운드의 여러 `irc` 호출은 하나씩 순서대로 실행되고, 같은 발신자의 메시지 순서가 유지됩니다. 동시 실행 등급도 `Concurrent`가 아닙니다.

### 입력에 따라 달라지는 효과

도구의 영향 범위가 인자에 따라 갈리면 `effects_for_input`을 덮어씁니다 (`crates/tool-core/src/lib.rs:260`). 기본 구현은 `effects()`를 그대로 돌려줍니다.

레지스트리는 이름만으로 효과를 묻지 않고 그 호출의 입력까지 넘겨 묻습니다.

- `declared_effects`는 `effects_for_input(input)`을 부릅니다 (`crates/app-core/src/tool/mod.rs:1036`).
- `declared_effects_sync`는 `effects()`만 읽습니다 (`crates/app-core/src/tool/mod.rs:1044`). 라운드를 내보내기 전에 도는 검사라 맵을 기다릴 수 없고, 잠금이 잡혀 있으면 `None`을 돌려줍니다.
- 두 함수 모두 `None`을 "분류할 수 없음"으로 씁니다. 호출자는 그 값을 쓰기 배리어로 읽습니다. 읽기 전용 게이트는 자기가 볼 수 없는 영향 범위를 가진 도구 때문에 열리지 않습니다.

### 두 게이트

계획 모드 검사는 두 경로에 따로 있습니다. 두 경로가 서로 다른 방식으로 도구를 부르기 때문입니다.

| 경로 | 위치 | 무엇을 하는가 |
| --- | --- | --- |
| 레지스트리 | `Registry::execute` (`crates/app-core/src/tool/mod.rs:1450`) | 이름을 해석하고 세션 정책을 확인한 뒤, 계획 모드 게이트(`:1513`)를 지나 도구를 꺼내고 승인 게이트(`:1561`)와 `pre_tool` 훅(`:1582`)을 차례로 지납니다. |
| 에이전트 | `Agent::validate_tool_allowed` (`crates/app-core/src/agent/turn_execution.rs:1066`) | 라운드가 호출을 내보내기 전에 이름 하나를 검사합니다. 계획 모드 게이트가 `:1081`에 있습니다. |

에이전트 경로가 계획 모드 검사를 함수 맨 앞에 두는 이유는 그 아래 SDK 분기 때문입니다. SDK가 켠 도구나 콜백 도구는 허용 목록 검사에서 곧바로 `Ok`로 빠져나갑니다. 그 분기 뒤에 게이트를 두면 정확히 영향 범위를 볼 수 없는 호출이 검사를 지나칩니다. 그래서 계획 모드 검사는 그 분기보다 먼저 돌고, 레지스트리 경로도 같은 이유로 같은 기본값을 씁니다.

두 경로 모두 거절할 때 `TOOL_LIFECYCLE` 경고를 남기고 `block_reason`을 `plan_mode` 또는 `approval`로 적습니다 (`crates/app-core/src/tool/mod.rs:1517`, `:1573`).

### 계획 모드 상태 기계

`PlanState`는 네 값입니다 (`crates/app-core/src/plan_mode.rs:16`).

| 상태 | 저장 이름 | 읽기 전용인가 | 뜻 |
| --- | --- | --- | --- |
| `Off` | `off` | 아니요 | 계획 모드를 쓰지 않습니다. 모든 호출이 평소대로 돕니다. |
| `Planning` | `planning` | 예 | 모델이 계획을 세우는 중입니다. |
| `PendingApproval` | `pending_approval` | 예 | 계획을 제출했고 사용자가 아직 결정하지 않았습니다. |
| `Approved` | `approved` | 아니요 | 계획이 승인되어 효과가 다시 허용됩니다. |

읽기 전용 여부는 `is_read_only`가 정합니다 (`crates/app-core/src/plan_mode.rs:50`). 게이트는 `allows_effects`로 판정하며, 읽기 전용 상태에서는 `parallel_safe`인 호출만 통과시킵니다 (`crates/app-core/src/plan_mode.rs:164`). 아무것도 선언하지 않은 도구는 트레이트 기본값이 쓰기 배리어라서 거절됩니다.

상태는 세션 ID로 키를 잡은 프로세스 전역 맵에 있습니다. 레지스트리 필드로 두지 않은 이유는 `Registry::Clone`이 호출마다 복제되기 때문입니다. 필드로 두면 복제본마다 상태가 갈라집니다.

- `state_for`는 항목이 없는 세션에 `Off`를 돌려줍니다 (`crates/app-core/src/plan_mode.rs:83`).
- `set_state`는 `Off`를 저장하지 않고 항목을 지웁니다 (`crates/app-core/src/plan_mode.rs:94`).
- `clear_state`는 세션 항목을 지웁니다 (`crates/app-core/src/plan_mode.rs:105`).
- `transition`은 상태를 바꾸고 그 전이를 세션 재생 타임라인에 기록합니다 (`crates/app-core/src/plan_mode.rs:185`).

거절 문구는 `block_message`가 만듭니다 (`crates/app-core/src/plan_mode.rs:173`). 무엇이 거절됐는지, 아무것도 바뀌지 않았는지, 빠져나갈 두 길이 무엇인지를 한 문단에 담습니다. 같은 호출을 다시 보내도 결과가 같다는 점도 적습니다.

계획 모드는 기본값이 꺼짐입니다. 설정 필드의 기본값이 `false`이고 (`crates/base/src/config.rs:919`), 기본 설정 파일도 `plan_mode = false`입니다 (`crates/base/src/config/default_file.rs:455`). 설정을 건드리지 않은 세션은 상태 맵에 항목이 없으므로 항상 `Off`로 판정됩니다.

### 승인 등급

`ApprovalMode`는 `off`, `write`, `all` 셋입니다 (`crates/config-types/src/lib.rs:511`). 기본값은 `off`이고 (`crates/base/src/config.rs:922`), 이는 지금까지처럼 묻지 않는 동작입니다.

판정은 `evaluate`가 합니다 (`crates/app-core/src/approval.rs:70`). 순서가 중요합니다.

1. `dual_confirm_classes`에 든 위험 등급이면 모드와 무관하게 승인을 요구합니다. 이 목록에 등급을 넣는 이유가 바로 어떤 모드도 그것을 자동 승인하지 못하게 하는 것입니다.
2. 모드가 `off`면 여기서 곧바로 자동 승인으로 돌려줍니다. 읽기 전용 분기보다 앞입니다. 순서가 뒤집히면 아무것도 바꾸지 않아야 할 모드가 호출마다 감사 줄을 쓰기 시작합니다.
3. 읽기만 하는 호출은 어떤 모드에서도 묻지 않습니다 (`crates/app-core/src/approval.rs:43`).
4. `write` 모드는 파일 변이만 자동 승인합니다. 프로세스나 네트워크가 섞이면 승인을 요구합니다.
5. `all` 모드는 읽기 전용이 아닌 모든 호출을 자동 승인합니다.

위험 등급은 새로 분류하지 않고 기존 명령 위험 분류기의 판정을 그대로 씁니다 (`crates/app-core/src/approval.rs:52`). 이름이 붙는 등급은 `destructive-shell` 하나입니다 (`crates/app-core/src/approval.rs:20`). 셸 명령만이 영향 범위를 따질 대상이기 때문입니다.

`dual_confirm_classes`의 기본값은 빈 배열입니다 (`crates/base/src/config.rs:940`, `crates/base/src/config/default_file.rs:476`). 비어 있으면 모든 판단이 모드에 맡겨집니다.

게이트는 `pre_tool` 훅보다 앞에 있습니다 (`crates/app-core/src/tool/mod.rs:1561`). 물어야 할 호출을 외부 게이트가 이미 내보낸 것처럼 관찰하지 않게 하려는 순서입니다. 셸 안의 명령 위험 하드 거절은 그대로 남습니다. 승인 등급은 사용자에게 물을지만 정합니다.

`irc`는 쓰기 비트만 있고 프로세스·네트워크 비트가 없어서 `write` 모드에서 파일 변이와 같은 자동 승인을 받고, `all` 모드에서도 묻지 않습니다. 승인 창이 뜨는 도구가 아닙니다.

### 감사 기록

자동 승인에는 감사 기록이 붙습니다. 새 파일이나 새 큐를 만들지 않고 기존 로그 이벤트 `TOOL_APPROVAL`을 씁니다 (`crates/app-core/src/approval.rs:139`). 필드는 도구 이름, 모드, 사유, 호출 ID 넷입니다 (`crates/app-core/src/approval.rs:125`).

`off` 모드는 감사 줄을 쓰지 않습니다. 승인 등급이 없던 시절과 로그가 같아야 하기 때문입니다. 읽기 전용 호출도 감사 대상이 아닙니다. 그래서 `off`가 읽기 전용 분기보다 먼저 반환합니다.

## 편집 안전 장치

파일을 쓰는 도구는 쓰기가 성공한 뒤 그 호출의 변경 전후 스냅샷을 편집 저널에 남깁니다. `undo`는 가장 최근 단위를 되돌리고, `redo`는 마지막 `undo`가 지운 단위를 다시 적용합니다. `read`의 hashline 앵커와 `hashline_edit`는 줄 내용을 짧은 태그로 묶어 편집 대상 줄을 지정합니다. 이 문서는 현재 트리에 있는 이 동작의 범위와 한계를 설명합니다.

### 저널

저널은 `horon-agent-edit-safety` 크레이트가 소유합니다 (`crates/edit-safety/src/store.rs:162` `SnapshotStore`). 파일 내용은 SHA-256으로 주소를 매기고, 같은 바이트는 한 번만 저장합니다 (`crates/edit-safety/src/blob.rs:17` `BlobId`).

- 도구 호출 하나가 변이 단위 하나입니다. 여러 파일을 건드린 호출은 한 단위에 파일별 before/after를 담습니다.
- 한 호출이 같은 경로를 두 번 건드리면 첫 before와 마지막 after만 남겨 순변경 하나로 기록합니다 (`crates/app-core/src/tool/undo.rs:95` `record`).
- 기록은 쓰기가 성공한 뒤에만 합니다. 실패한 쓰기는 단위를 남기지 않습니다.
- `commit`은 단위를 실행 취소 이력으로 옮기고, `abort`는 단위와 그 바이트를 버립니다 (`store.rs:228`, `store.rs:261`).
- 이력은 세션마다 하나이고 프로세스 수명 동안만 유지합니다. 재시작하면 비어 있습니다.
- 예산은 64 MiB입니다 (`store.rs:25` `DEFAULT_BYTE_BUDGET`). 커밋할 때 사용량이 예산을 넘으면 가장 오래된 단위부터 통째로 버립니다. 방금 커밋한 단위는 자기 커밋으로는 버려지지 않으므로, 예산보다 큰 단위도 최신인 동안은 남습니다 (`store.rs:228` `commit`).
- 활성 단위의 바이트도 저장소 사용량에 잡히지만, 예산 정리는 커밋 시점에만 일어납니다.

### undo

`undo`는 커밋된 이력의 가장 최신 단위만 대상으로 합니다 (`crates/app-core/src/tool/undo.rs:271` `UndoTool`).

1. 각 파일의 현재 바이트를 단위의 after 상태와 비교합니다.
2. 하나라도 다르면 기본적으로 호출을 거부합니다. `force: true`이면 덮어쓰고, 덮어쓴 경로를 결과에 나열합니다.
3. 기록된 상태에 읽을 수 없던 파일이 있으면 단위 전체를 거부합니다. 일부만 복원하고 성공으로 보고하지 않습니다 (`store.rs:277` `undo`).
4. 성공하면 파일을 before 상태로 되돌립니다. 파일이 없던 상태였으면 지우고, 바이트가 있던 상태였으면 그 바이트를 씁니다.

저널은 파일을 직접 건드리지 않습니다. 복원할 바이트를 돌려주고, 쓰기는 호출자인 `undo` 도구가 합니다. 이 분리 덕분에 크레이트 단위 테스트는 파일 시스템 없이 돕니다.

### redo

`redo`는 `[tools] redo = true`일 때만 등록됩니다 (`crates/app-core/src/tool/undo.rs:133` `redo_enabled`). 기본값은 `false`입니다. 등록되어도 프로필의 허용 목록을 통과해야 모델에 보이므로, `core` 프로필에서는 `full`이나 명시적 허용 목록이 함께 필요합니다.

- 마지막 `undo`가 넘긴 단위의 after 상태를 다시 씁니다.
- 각 파일의 현재 바이트가 그 단위의 before 상태와 같은지 먼저 확인합니다. 다르면 거부하고, `force: true`이면 덮어쓰고 경로를 나열합니다.
- 성공하면 그 변경을 새 변이 단위로 다시 커밋합니다. 그래서 다시 `undo`할 수 있습니다.
- redo 대기열은 세션별 모듈 상태에 있고 프로세스가 끝나면 사라집니다.

### hashline 앵커

`read`에 `hashline: true`를 주면 각 줄 앞에 `{줄번호:>5}#{태그}:{본문}`을 붙입니다 (`crates/app-core/src/tool/read.rs:153`). 태그는 그 줄 바이트의 SHA-256 첫 두 바이트를 소문자 16진수 네 자리로 쓴 것입니다 (`crates/edit-safety/src/hashline.rs:27` `tag_for`). 16비트 값이므로 암호학적 약속이 아니라, 모델이 읽은 줄을 가리키는 값싼 표식입니다.

- 2000바이트를 넘는 줄은 잘리고 `----` 태그를 받습니다 (`read.rs:13` `MAX_LINE_LEN`). 이 앵커는 내용을 가리키지 않으므로 편집에 쓸 수 없습니다.
- `hashline_edit`는 `[tools] hashline_edit = true`일 때만 등록됩니다. 기본값은 `false`이고 코어 집합 밖입니다.
- 입력은 `file_path`와 `ops` 배열입니다. 각 op는 `start_anchor`, 선택적 `end_anchor`, `new_text`를 가집니다.
- 파일 단위 동작을 지정하면 `ops`를 생략할 수 있습니다. `delete: true`는 파일을 지우고, `move_to: "<경로>"`는 줄 편집을 마친 내용을 그 경로에 쓴 뒤 원본을 지웁니다. 둘 다 줄 편집 뒤에 같은 호출, 같은 변이 단위로 적용되므로 `undo` 한 번이 내용과 원래 경로를 함께 되돌립니다.
- 이동은 이미 있는 대상을 덮어쓰지 않고 거부합니다. 대상을 의도적으로 교체하려면 `apply_patch`를 씁니다. `move_to`와 `delete`는 함께 쓸 수 없습니다.
- 경로 정책과 생성 파일 가드는 원본과 이동 대상 모두에 적용되고, `delete`는 보호 경로 검사도 통과해야 합니다.
- 모든 앵커를 디스크의 현재 내용과 먼저 대조합니다. 하나라도 낡았으면 호출 전체를 거부하고 파일을 쓰지 않습니다 (`crates/app-core/src/tool/hashline_edit.rs:128` `resolve_ops`).
- 줄 번호는 1부터 시작하고, span은 아래로만 진행하며, 겹치는 span은 거부합니다.
- `new_text`가 빈 문자열이면 span을 지웁니다. 끝의 줄바꿈 하나는 내용이 아니라 구분자로 봅니다.
- 쓰기 경로는 `edit`와 같습니다. 경로별 잠금, 변이 단위 하나, 같은 파일 접촉 이벤트를 씁니다.

### 한계와 실패 방식

| 상황 | 동작 |
| --- | --- |
| 프로세스 재시작 | 이력이 비어 있어 `undo`가 되돌릴 것이 없다고 거부합니다. |
| 기록 후 다른 프로세스가 파일을 바꿈 | `undo`가 거부합니다. `force: true`이면 덮어쓰고 경로를 나열합니다. |
| 기록 당시 파일을 읽을 수 없었음 | 그 단위의 `undo` 전체가 거부됩니다. |
| 예산 초과 | 가장 오래된 단위부터 사라지고, 그 단위는 되돌릴 수 없습니다. |
| 예산보다 큰 단위 | 최신인 동안은 남습니다. 예산은 강제 상한이 아닙니다. |
| 낡은 hashline 앵커 | 호출 전체가 거부되고 파일은 그대로입니다. |
| `----` 앵커 | 편집에 쓸 수 없다는 오류로 거부합니다. |
| 겹치는 span | 호출 전체가 거부됩니다. |

크레이트는 파일 시스템을 읽지 않으므로 외부 변경을 스스로 감지하지 않습니다. 감지는 호출자가 `BlobId::of`로 현재 바이트를 기록된 상태와 비교할 때 일어납니다. 이력은 프로세스 밖으로 나가지 않으며, 다른 프로세스나 재시작한 세션과 공유하지 않습니다.

## write가 새로 만든 실행 파일

`write`가 새 파일을 만들 때 내용이 `#!`로 시작하면 유닉스에서 실행 권한을 함께 줍니다 (`crates/app-core/src/tool/write_shebang.rs` `make_executable`). 방금 쓴 스크립트를 다음 턴에 `bash ./script.sh`로 부르다 권한 오류로 한 턴을 쓰는 일을 없애려는 처리입니다.

- 권한은 새로 만들 때만 바뀝니다. 이미 있는 파일을 덮어쓸 때는 그 파일의 기존 모드를 그대로 둡니다. 모드는 파일 주인이 정한 값이고, 이번 호출은 내용만 바꿨기 때문입니다.
- 목표 모드는 만들기 모드에 실행 비트를 더한 값입니다. 만들기 모드는 `0o666 & !umask`이므로 읽을 수 있는 분류에만 실행 비트를 붙입니다. umask가 022이면 644가 755가 되고, 077이면 600이 700이 됩니다 (`script_mode`).
- `umask()`를 부르지 않습니다. 그 호출은 프로세스 전체 값이라 다른 스레드가 파일을 만드는 순간과 경쟁합니다. 대신 방금 만들어진 파일의 모드를 읽어 실행 비트를 그 모드에서 계산합니다.
- 만들기일 때만 적용합니다. ACP 클라이언트 버퍼로 쓴 경우에는 디스크에 파일이 없으므로 실행 비트를 건드리지 않고, `edit`, `apply_patch`, `hashline_edit`도 그대로입니다.
- 결과 본문에 파일을 실행 가능하게 만들었다는 문장이 붙습니다. 권한을 바꾸지 않았으면 붙지 않습니다.
- 윈도우에는 실행 비트가 없으므로 아무것도 하지 않습니다. `#[cfg(not(unix))]`의 같은 이름 함수가 `false`를 돌려줍니다.

## todo의 blocked 상태

`todo` 도구는 `blocked`를 상태로 받습니다. 외부 의존 때문에 진행할 수 없는 작업을 `pending`이나 `in_progress`로 적으면 이유가 사라지고, 목록만 보고는 무엇을 기다리는지 알 수 없습니다.

- `blocked`에는 `blocker` 문자열이 필요합니다. 500자까지이고, 비어 있거나 공백뿐이면 거부합니다 (`crates/app-core/src/tool/todo_blocked.rs` `normalize`). 이유 없는 차단은 저장하지 않습니다.
- `blocked`가 아닌 항목에 붙은 `blocker`는 버립니다. 상태가 바뀐 뒤에도 남은 메모가 살아 있는 이유처럼 읽히기 때문입니다.
- 저장 전에 앞뒤 공백을 지웁니다.
- 완료 검사는 `blocked`를 완료로 보지 않습니다. 남은 항목이 `blocked` 하나뿐인 목록은 완료가 아니고, 자동 재촉도 그 항목을 남은 일로 셉니다 (`crates/todo-policy/src/followup.rs` `is_incomplete_todo`). 진행 중 항목을 하나만 두는 규칙에서도 `in_progress`로 세지 않습니다.
- ACP 계획 항목은 세 상태만 있어 `blocked`를 `pending`으로 내보냅니다 (`crates/app-core/src/tool/todo.rs` `plan_entries`). 지금 아무도 하지 않는 열린 일이라는 뜻이고, 이유는 항목 자체의 `blocker`에 남습니다.
- 도구 결과의 JSON에 `blocker`가 함께 나오므로 목록을 보는 쪽이 이유를 읽을 수 있습니다.

## read 결과에 붙는 중첩 AGENTS.md

`read` 도구는 파일 본문을 돌려줄 때, 그 파일 위쪽 디렉터리에 있는 `AGENTS.md`를 찾아 결과 끝에 덧붙입니다. 세션이 실제로 읽는 디렉터리의 규칙을 그 파일과 함께 보게 하려는 장치입니다. 이 문서는 현재 트리의 동작을 설명합니다. 코드는 `crates/app-core/src/tool/nested_agents.rs`의 `inject_for_read`와, `crates/app-core/src/tool/read.rs`의 `execute`가 그 함수를 부르는 자리에 있습니다.

### 무엇을 하는가

주입 위치는 시스템 프롬프트가 아니라 도구 결과입니다. `read`의 `execute`가 텍스트 본문을 다 만든 뒤 `nested_agents::inject_for_read(&path, output.len(), &ctx)`를 부르고, 돌려받은 문자열을 결과 끝에 `push_str`로 붙입니다.

빈 본문 판정은 주입 호출 앞에서 `let base_is_empty = output.is_empty();`로 먼저 재어 둡니다. 그래야 `(empty file)` 표시가 주입문에 밀리지 않고, 빈 파일에도 조상 계약이 붙습니다(`if base_is_empty` 분기에서도 주입 결과를 붙입니다). 주입이 하는 일은 덧붙이기 하나뿐입니다. 파일 내용을 해석하거나 병합하거나 규칙으로 강제하지 않습니다.

### 어떤 파일이 붙는가

#### 조상 탐색 규칙

탐색은 `find_agents_md_up`이 맡습니다. 읽는 파일이 든 디렉터리에서 시작해 위로 올라가고, 세션의 작업 디렉터리에서 멈춥니다. `while let Some(dir) = current` 루프가 `dir == root || !dir.starts_with(root)`에서 `break`합니다. 찾은 파일은 `found.reverse()`로 루트에서 리프 방향으로 정렬해 붙입니다. 가까운 디렉터리의 계약이 뒤에 오므로 더 구체적인 규칙을 나중에 읽습니다.

세 가지는 대상에서 빠집니다.

- 작업 디렉터리 자신의 `AGENTS.md`는 붙지 않습니다. 위의 `break`가 작업 디렉터리에 닿는 순간 탐색을 끝내기 때문입니다. 그 파일은 시스템 프롬프트 계층이 이미 다룹니다([`docs/runtime/PROMPT.ko.md`](PROMPT.ko.md)의 4 계층).
- 읽고 있는 파일이 그 디렉터리의 `AGENTS.md` 자신이면 자기 자신은 넣지 않습니다. `find_agents_md_up`의 `canonical != start` 조건이 그 경우를 거릅니다. 상위 계약만 붙습니다.
- 조상 디렉터리에 `AGENTS.md`가 없으면 그 디렉터리는 건너뜁니다. 있는 것만 순서대로 모읍니다.

#### 경계 제한

주입 대상 경로와 작업 디렉터리를 모두 실제 경로로 정규화한 뒤 비교합니다. `contain`이 `std::fs::canonicalize`를 양쪽에 걸고, `file == root || !file.starts_with(&root)`이면 `None`을 돌려줍니다. 그때는 아무것도 붙지 않습니다.

정규화를 거치므로 작업 디렉터리 안을 가리키는 심볼릭 링크가 실제로는 바깥 디렉터리를 향하면 거부됩니다. 링크 이름만 보고 통과시키지 않습니다. `contain_rejects_symlink_escaping_root` 테스트가 이 경우를 고정합니다.

#### 크기 상한

상한은 세 겹이고, 앞의 두 상한은 파일 본문만이 아니라 결과에 실제로 붙는 덧붙임의 바이트를 셉니다. 헤더와 절단 안내도 그 안에 들어갑니다.

| 상수 | 값 | 막는 것 |
| --- | --- | --- |
| `MAX_BYTES_PER_FILE` | 16 KiB | 조상 `AGENTS.md` 한 파일이 만드는 블록 전체(헤더, 본문, 절단 안내) |
| `MAX_BYTES_PER_READ` | 32 KiB | 한 번의 `read`가 붙이는 모든 블록의 합 |
| `MAX_BASE_OUTPUT_BYTES` | 32 KiB | 이 크기 이상인 읽기 결과에는 아예 붙이지 않음. 가드가 시작되는 지점이 아니라, 덧붙임 때문에 아직 받아들일 만한 결과가 가드의 실제 한도 너머로 밀리지 않도록 잡아 둔 보수적 여유 |

블록을 만들 때 헤더와 절단 안내의 길이를 먼저 빼고, 남은 만큼만 본문에 씁니다. 그래서 상한을 넘는 블록이 나오지 않습니다. 본문이 잘리면 UTF-8 문자 경계에서 자르고, 잘렸다는 `[Note: content truncated ...]` 줄을 그 블록 끝에 남깁니다. `MAX_BYTES_PER_READ`도 마찬가지로, 그 읽기가 붙이는 모든 블록과 건너뛰었다는 안내를 합친 양을 막습니다.

다음 조상 파일의 헤더조차 들어가지 않거나 잘린 본문에 붙일 안내가 들어가지 않으면 거기서 멈추고, 건너뛴 조상 파일이 몇 개인지 밝히는 `[Note: N more ancestor AGENTS.md files skipped: ...]` 안내를 결과 끝에 붙입니다. 이 안내가 들어갈 자리를 남기려고, 블록마다 쓸 수 있는 양을 정할 때 읽기 예산의 일부를 미리 떼어 둡니다(`SKIPPED_MARKER_RESERVE`). 그런데도 안내가 들어가지 않으면 안내를 버립니다. 상한을 넘겨서 붙이지는 않습니다.

### 언제 붙지 않는가

다음 중 하나라도 해당하면 결과는 원래 본문 그대로입니다.

- 설정 `[tools] nested_agents_md`가 꺼져 있음. `inject_for_read`가 `crate::config::config().tools.nested_agents_md`를 가장 먼저 봅니다.
- 세션에 작업 디렉터리가 없음. `ctx.working_dir.as_deref()`가 `None`이면 그 자리에서 반환하고 `nested_agents: no working_dir; skipping ancestor AGENTS.md injection` 로그를 남깁니다.
- 읽은 파일이 작업 디렉터리 밖이거나, 심볼릭 링크가 바깥을 가리킴.
- 위쪽 어디에도 `AGENTS.md`가 없음.
- 읽기 결과가 이미 32 KiB 이상임(`base_output_bytes >= MAX_BASE_OUTPUT_BYTES`). 이 상한은 도구 결과 가드가 시작되는 지점이 아닙니다. 가드의 실제 한도는 `single_output_max_tokens`가 정하는 `min(예산 × 0.30, 50,000)` 토큰이고(`crates/app-core/src/tool/mod.rs:1201-1203`, 상수 정의는 `:1361`과 `:1372`), 이 값을 토큰당 4글자로 환산하므로(`APPROX_CHARS_PER_TOKEN = 4`, `crates/core/src/util.rs:17`) 예산이 16만 6,667 토큰 이상이면 20만 자에 머뭅니다. 문맥이 거의 찬 세션에서는 남은 여유가 이 값보다 먼저 걸립니다(`mod.rs:1668-1671`). 실제 모델의 문맥 창 규모에서는 32 KiB가 그 한도에 한참 못 미칩니다. 이 상한을 두는 이유는 덧붙임 때문에 아직 받아들일 만한 결과가 가드 한도 너머로 밀리지 않게 하려는 것입니다. 그 대가로 32 KiB와 가드의 실제 한도 사이에 드는 결과는 본문을 그대로 돌려받으면서 계약은 하나도 붙지 않고, 그 사실을 알리는 표시도 없습니다. 가드의 한도를 넘으면 결과 자체가 보류되고, 호출자가 `accept_large_output`을 넘길 때만 잘려서 돌아옵니다.
- 같은 세션에서 그 경로를 이미 주입했음. 아래 세션 범위 캐시를 따릅니다.
- `read`가 텍스트 분기까지 오지 않음. `agent://` 참조는 `is_agent_url`, 이미지는 `is_image_file`, PDF는 `is_pdf_file`, 바이너리는 `is_binary_file` 분기에서 먼저 반환하므로 주입 지점에 닿지 않습니다.

### 설정 방법

`config.toml`의 `[tools]` 표에 스위치가 있습니다. 기본값은 켜짐입니다(`crates/base/src/config.rs:902-905`, `crates/base/src/config.rs:934`).

```toml
[tools]
nested_agents_md = true
```

환경변수로 덮어쓸 때는 `HORON_AGENT_NESTED_AGENTS_MD`를 씁니다. `0`이나 `false` 계열 값이 꺼짐으로 해석됩니다(`crates/base/src/config/env_overrides.rs:161-165`). 기본 설정 파일에도 같은 키와 설명이 들어 있습니다(`crates/base/src/config/default_file.rs:435-441`).

### 세션 범위 캐시

같은 세션에서 같은 `AGENTS.md`를 두 번 붙이지 않습니다. 주입한 경로를 세션 id별로 모아 두고, 이미 있는 경로는 건너뜁니다(`InjectedSessions`의 `already_injected`와 `mark_injected`).

- 캐시는 프로세스 전역 하나이고, 추적하는 세션은 최대 64개입니다(`MAX_TRACKED_SESSIONS`). `ensure_session`이 세션을 등록한 뒤 `order.len() > MAX_TRACKED_SESSIONS`인 동안 가장 먼저 등록된 세션의 기록을 버립니다.
- 기록이 밀려난 세션은 같은 경로를 다시 주입받습니다. `inserting_more_than_max_sessions_evicts_the_oldest` 테스트가 그 순서를 고정합니다.
- 세션 안에서 파일 내용이 바뀌어도 다시 붙지 않습니다. 경로만 기억합니다.

### 출력 형식 예시

블록은 줄바꿈 두 번 뒤에 대괄호 헤더가 오고, 그 아래에 파일 본문이 그대로 붙습니다(`inject_for_read`가 쓰는 `block_header`의 `format!("\n\n[Directory Context: {}]\n", ...)`).

```text
    1	use crate::config;

[Directory Context: /Users/me/repo/crates/base/AGENTS.md]
crates/base 규칙 본문

[Directory Context: /Users/me/repo/crates/base/config/AGENTS.md]
config 하위 디렉터리 규칙 본문
```

본문이 잘리면 그 블록 끝에 `[Note: content truncated to save context window; read the file directly for the rest: <경로>]`가 붙습니다. 예산이 모자라 남은 조상 파일을 건너뛰었으면, 그 대신 `[Note: N more ancestor AGENTS.md files skipped: this read's context budget is full; read them directly]`가 결과 끝에 붙습니다.

### 알려진 한계

- 대화가 압축돼도 다시 붙지 않습니다. 캐시는 세션 id와 경로만 보고, 압축을 감지해 기록을 지우는 경로가 없습니다.
- 캐시는 프로세스 메모리에만 있습니다. 프로세스를 다시 띄우면 비어 있고, 그 세션의 첫 `read`에서 다시 붙습니다.
- 세션 기록이 64개를 넘으면 오래된 세션의 기록이 밀려나므로, 그 세션은 같은 계약을 한 번 더 받습니다.
- 주입은 `read` 한 곳에서만 일어납니다. `agentgrep`, `ls`, `bash` 출력에는 붙지 않습니다.
- 규칙을 강제하지 않습니다. 붙은 본문은 모델이 읽는 텍스트일 뿐이고, 위반을 막는 검사는 없습니다.
- 작업 디렉터리 자신의 `AGENTS.md`는 이 경로로 오지 않습니다. 그 파일과 전역 `~/AGENTS.md`는 프롬프트 계층이 맡습니다.
- 읽기 결과가 32 KiB 이상이면 계약이 붙지 않는데, 그 사실이 결과에 드러나지 않습니다. 독자는 "여기에 AGENTS.md가 없다"와 "본문이 커서 계약을 건너뛰었다"를 구분할 수 없습니다. 가드의 실제 한도에 닿기 전까지 본문은 정상으로 돌아오므로, 결과만 보고는 어느 쪽인지 알 길이 없습니다. 이 조용한 누락에 책임이 있는 상수는 `MAX_BASE_OUTPUT_BYTES`입니다.

## 스킬 읽기

스킬이 어디서 오는지, 이름이 겹칠 때 어떻게 되는지는 [`PROMPT.ko.md`](PROMPT.ko.md)의 "스킬" 절에 있습니다. 이 절은 도구가 스킬을 읽는 방법만 다룹니다.

### `read skill://`

- `skill://<이름>`은 그 스킬의 본문을 돌려줍니다. 결과는 `## Skill: <이름>`, `Base directory: <스킬 폴더>` 두 줄로 시작하고, 앞머리는 빠집니다.
- `skill://<이름>/<경로>`는 스킬 폴더 기준의 파일을 읽습니다. 이름에 namespace가 있으면(`claude/review`) 등록된 이름 가운데 가장 긴 접두가 이름이 되고 나머지가 경로입니다.
- 줄 선택자(`skill://qa-skill/references/a.md:20-40`)와 `offset`/`limit`은 파일 읽기와 같게 동작합니다.
- 이름은 이 세션의 실효 목록에서 찾습니다. 사용자 스킬에 그 세션 작업 폴더의 스킬을 더한 목록이라, 다른 저장소의 프로젝트 스킬은 보이지 않습니다.
- 경로는 실제 경로로 바꾼 뒤 스킬 폴더의 실제 경로 안에 있어야 합니다. `..`, 절대 경로, 밖을 가리키는 심링크는 `skill:// path resolves outside the skill directory` 오류가 됩니다. 없는 스킬은 `skill:// names no installed skill` 오류입니다.

### 경로 정책과의 관계

경로 거부 정책(`path_policy`)은 `skill://` 글자 자체를 작업 폴더 경로로 해석하지 않습니다. 대신 `skill://<이름>/<경로>`가 가리키는 실제 파일에 기존 거부 규칙을 그대로 적용하므로, 정책이 막는 파일은 스킬 경로로도 읽을 수 없습니다. 작업 폴더 밖을 막는 정책을 둔 세션에서는 `~/.horon/skills` 같은 사용자 수준 스킬의 하위 파일이 이 규칙에 걸립니다. 그런 세션은 사용자 수준 스킬을 `skill_manage`(action `load`)로 읽습니다.

## 스킬 관리

`skill_manage`의 `create`, `update`, `delete`는 모델이 직접 쓴 스킬만 다룹니다. 그 스킬은 `~/.horon/managed-skills/<이름>/SKILL.md`에 있고, 사람이 쓴 스킬과 이름이 겹치면 언제나 사람이 쓴 쪽이 이깁니다. 그래서 세 동작 모두 사람이 쓴 스킬을 고치거나 지우지 못합니다.

| 동작 | 하는 일 | 거절하는 경우 |
| --- | --- | --- |
| `create` | 관리 스킬을 새로 씁니다. | 같은 이름의 스킬이 이미 로드되거나 관리 디렉터리에 있으면 거절합니다. |
| `update` | 관리 스킬의 설명과 본문을 바꿉니다. | 관리 스킬이 없으면 `create`를 쓰라고 알립니다. |
| `delete` | 관리 스킬 디렉터리를 지우고 스킬 목록을 다시 읽습니다. | 같은 이름이 사람이 쓴 스킬로 로드되면 그 출처를 밝히고 아무 파일도 지우지 않습니다. |

`create`와 `update`는 `[tools] managed_skills`가 켜져 있어야 실행됩니다. `delete`는 이 스위치를 보지 않습니다. 스위치를 끈 뒤에 남은 관리 스킬도 정리해야 하기 때문입니다.

`delete`의 안전 규칙은 `crates/base/src/managed_skills.rs`의 `remove` 한 곳에 모여 있습니다.

- 이름을 다시 검증하므로 `..`이나 경로 구분자로 관리 디렉터리 밖에 닿지 못합니다.
- 관리 디렉터리의 항목이 심링크면 링크만 지웁니다. 링크가 가리키는 디렉터리와 그 안의 파일은 그대로 남습니다.
- 실제 디렉터리는 실제 경로로 바꾼 뒤 관리 디렉터리 안에 있는지 확인하고 지웁니다. 재귀 삭제는 안쪽의 심링크를 따라가지 않고 링크만 지웁니다.
- 같은 이름이 없으면 not-found 오류를 냅니다.
- 지운 뒤에는 공유 스킬 레지스트리를 다시 읽으므로 다음 턴의 목록과 로드에 바로 반영됩니다. 재읽기에 실패하면 파일은 이미 지워진 상태이므로 그 사실을 결과에 함께 적습니다.

거절 문구는 이름이 어느 출처에 속하는지 밝힙니다. 예를 들어 작업 폴더의 `.agents/skills`에서 온 스킬이면 "a project skill from the agents source (<디렉터리>)"처럼 나옵니다. 이 문구는 스킬 출처 표(`crates/base/src/skill/sources.rs`의 `SOURCES`)를 그대로 읽어 만들므로, 표에 없는 디렉터리를 지어내지 않습니다.

계획 모드에서는 세 동작이 모두 거절됩니다. `skill_manage`는 쓰기 효과를 선언하고, 계획 모드 게이트는 쓰기 효과를 가진 호출을 실행 전에 막습니다. 그 게이트는 [도구 효과, 계획 모드, 승인 등급](#도구-효과-계획-모드-승인-등급)에 설명되어 있습니다.

## 기억 읽기

프롬프트에 주입되는 기억은 잘려 있고, 전체 내용을 보려면 `memory` 도구를 따로 불러야 했습니다. 이제 `read`가 `memory://` 참조를 그대로 처리합니다. 파일과 같은 도구, 같은 쪽 나누기 규칙을 씁니다.

### `read memory://`

- `memory://<id>`는 그 기억 한 건의 전체 내용을 돌려줍니다. 출력은 `id`, `category`, `scope`, `tags`, `created`, `updated`, `links` 머리말과 `content:` 본문으로 이뤄집니다. `links`는 다른 기억으로 향하는 그래프 간선(관계, 대체, 모순, 파생)의 대상 id이고, 태그·클러스터 간선은 메타데이터라 빠집니다.
- `memory://`(id 없음)는 저장된 기억을 최신순으로 나열합니다. 각 행은 `- <id> [<scope>/<category>] <한 줄 미리보기>`이고 미리보기는 첫 줄 80자까지입니다. 같은 시각에 저장된 기억은 id로 고정해 순서가 매번 같습니다.
- 줄 선택자(`memory://<id>:1-3`)와 `start_line`/`end_line`/`offset`/`limit`은 파일 읽기와 똑같이 동작하고, 다음 쪽 이어 읽기 안내도 같습니다.
- 없는 id는 `memory:// record not found: <id>` 오류입니다. 다른 프로젝트에서 저장한 기억은 이 세션의 범위 밖이므로 목록에도 나오지 않고 id로도 읽히지 않습니다.
- 읽을 수 있는 범위는 `read`를 부른 호출의 작업 폴더 프로젝트 저장소와 사용자 수준 전역 저장소입니다.
- 이 참조는 읽기 전용이고 파일 경로로 바뀌지 않습니다. 경로 거부 정책은 `memory://` 글자를 작업 폴더 경로로 해석하지 않으므로, 정책이 걸린 세션에서도 기억 읽기는 그대로 동작합니다.
- `write`나 `edit`로 `memory://`에 쓰는 경로는 없습니다. 기억을 바꾸려면 `memory` 도구를 씁니다.

### `memory` action `update`

- `{"action": "update", "id": "...", "content": "...", "category": "...", "tags": [...]}`는 기억 한 건을 제자리에서 고칩니다. 세 필드는 모두 선택이고 준 필드만 바뀝니다.
- id, 생성 시각(`created_at`), 접근 횟수, 보강 기록, 그래프 간선은 그대로 남습니다. `updated_at`만 새 시각으로 바뀝니다.
- 본문을 바꾸면 저장된 임베딩을 지우므로 다음 recall이 새 본문으로 다시 계산합니다. 태그를 바꾸면 그래프의 태그 간선도 함께 맞춥니다.
- 본문은 저장 전에 비밀값 마스킹을 거칩니다. 세션 기록을 저장할 때와 같은 규칙입니다.
- 없는 id는 `Not found: <id>`를 돌려줍니다. 아무 필드도 주지 않거나 본문이 공백뿐이면 도구 오류로 거부합니다.

### `memory` 액션 `learn`

- `{"action": "learn", "memory": "...", "context": "...", "scope": "project" | "global", "skill": {...}}`는 교훈 한 건을 기록합니다. 쉽게 알기 어려웠던 수정, 재사용할 절차, 프로젝트 관례가 여기에 해당합니다. omp의 `learn`과 같은 호출입니다.
- 교훈 본문은 `lesson`이 아니라 `memory`에 넣습니다. `lesson`을 보내면 "Unknown field: `lesson`. The lesson text goes in `memory`."로 거부합니다. serde가 모르는 필드를 무시해서 아무것도 저장하지 않고 성공으로 보이는 일을 막으려는 것입니다.
- `memory`는 2,000자, `context`는 400자까지입니다. 넘으면 도구 오류입니다. `context`는 교훈과 함께 `Context:` 줄로 저장됩니다.
- `scope`는 `project`(기본) 또는 `global`이고, `remember`와 같은 저장소에 들어갑니다. `all`은 읽기 범위라 거부합니다.
- 기록의 범주는 `fact`가 아니라 `lesson`입니다. 그래야 recall이 교훈으로 보여 주고 같은 시기의 사실보다 앞에 둡니다. `from_extracted`도 `lesson`과 `learning`을 이 범주로 옮기므로, 추출된 교훈이 더 이상 `fact`로 접히지 않습니다.
- 본문은 세션 기록을 저장할 때와 같은 비밀값 마스킹을 거친 뒤 저장되고, 같은 교훈을 다시 보내면 새 기록을 만들지 않고 저장된 기록을 보강합니다.
- `skill`을 주면 `{"action": "create" | "update", "name": "...", "description": "...", "body": "..."}`로 `skill_manage create`/`update`와 같은 코드를 거쳐 관리형 스킬을 씁니다. `name`은 필수라, `skill` 객체에 `name`이 없으면 "`skill.name` is required"로 알리고 아무것도 쓰지 않습니다. 스킬 쓰기가 실패하면 결과 둘째 줄에 알리고 교훈은 되돌리지 않습니다. 교훈은 이미 저장됐기 때문입니다. `skill_manage`와 달리 스킬 목록은 다시 읽지 않으므로, 새 스킬은 다음 `reload_all`이나 재시작 뒤에 보입니다.
- 스킬 작성에는 여전히 `[tools] managed_skills = true`가 필요합니다. 꺼져 있으면 교훈만 저장하고 스킬을 쓰지 않았다고 결과에 적습니다.

## 세션 사이 기억 통합

추출은 세션 하나씩 기억을 저장할 뿐 세션 사이를 합치지 않습니다. 그래서 같은 주제가 표현만 조금씩 달라진 채 쌓이고, 주입할 요약도 없습니다. 통합은 두 번째 단계로, 추출이 끝난 직후 추출 작업 안에서 실행되며 턴 경로에서는 돌지 않습니다.

- 같은 주제의 기억은 활성 기억 하나로 합쳐집니다. 살아남은 기억은 id와 본문을 그대로 유지하고, 나머지는 디스크에 남긴 채 대체됨으로 표시합니다(`superseded_by`와 `Supersedes` 간선). 그래서 id로 계속 읽을 수 있고 지워지는 기억은 없습니다.
- 두 기억의 주제가 같다는 판정은 본문 낱말을 두 개 이상 공유하면서 두 낱말 집합의 합집합 가운데 60% 이상을 공유하거나, 태그가 하나라도 겹칠 때입니다. 이 단계는 거의 같은 기억을 남기는 쪽보다 합치는 쪽을 택합니다. 잘못 합쳐도 잃는 것은 순위 자리 하나뿐이고, 합칠 때마다 대체된 id와 그 기억을 저장한 세션·도구를 로그에 남깁니다.
- `correction`은 같은 주제에서 다른 범주를 모두 앞섭니다. 사용자가 참이라고 말한 기록이기 때문에, 나중에 추출된 `fact`가 이를 덮지 않습니다. 같은 범주 안에서는 최신 기억이 이깁니다.
- 두 범위를 모두 통합합니다. 작업 폴더의 프로젝트 저장소와 사용자 수준 전역 저장소이고, 각각 자기 실행 표시와 요약을 가집니다(`crates/base/src/memory/consolidation.rs`).
- 범위별 요약은 그 범위의 기억 파일 옆에 저장되고, 다음 새 사용자 턴에 그 턴의 관련 기억 주입과 같은 자리에서 기억 안내로 주입됩니다. 통합이 한 번이라도 돌기 전에는 아무것도 더하지 않습니다.
- `[agents] memory_consolidation_min_new`(Horon 기본값 `20`)는 지난 실행 뒤 이만큼 새 기억이 쌓여야 다음 실행을 한다는 뜻입니다. 한 번 실행할 때 그래프를 다시 읽고 합쳐서 다시 쓰므로, 세션마다 돌리지 않고 모아서 돌립니다.
- `[agents] memory_consolidation_summary_bytes`(Horon 기본값 `4096`)는 주입하는 요약의 바이트 상한입니다. 넘으면 글자 경계에서 잘라내고 버리지 않습니다.

## MCP 서버 설정 출처

MCP 서버 정의는 `crates/base/src/mcp/sources.rs`의 순서대로 합칩니다. 위쪽 출처가 먼저이고, 같은 이름은 먼저 온 정의가 이깁니다.

| 순서 | 출처 | 파일 | 켜는 조건 |
| --- | --- | --- | --- |
| 1 | 작업 폴더 | `.horon/mcp.json` > `.mcp.json` > `.claude/mcp.json` | 작업 폴더를 신뢰한 경우(`docs/safety/SECURITY.md`) |
| 2 | horon-agent | `~/.horon/mcp.json` | 항상 |
| 3 | Claude Code | `~/.claude.json`(이 프로젝트 항목 > 전역 `mcpServers`) > `~/.claude/mcp.json` | `[mcp] claude_user = true` |
| 4 | Codex CLI | `~/.codex/config.toml`의 `[mcp_servers.*]` | `[mcp] codex_user = true` |

- 예외가 하나 있습니다. 같은 이름에 로컬 stdio 정의와 원격(HTTP, SSE) 정의가 함께 있으면 출처 순서와 상관없이 stdio 정의가 남고, 로그에 한 줄을 남깁니다. 잘 도는 로컬 서버의 원격 짝은 보통 따로 로그인이 필요하기 때문입니다(issue #653).
- 다른 호스트의 사용자 설정(Claude Code, Codex)은 기본으로 꺼져 있습니다. 그 서버는 다른 호스트에서 검토한 것이고, 불러오면 여기서 프로세스가 뜹니다. 꺼진 파일에 서버가 있으면 프로세스마다 로그 한 줄로 개수와 켜는 설정을 알립니다. `HORON_AGENT_DISABLE_CLAUDE_MCP`가 있으면 `claude_user`가 켜져 있어도 Claude 출처를 읽지 않습니다.
- Codex 설정은 매번 그 자리에서 읽습니다. 예전처럼 첫 실행 때 `~/.horon/mcp.json`으로 복사하지 않으므로, 환경 값(비밀일 수 있음)이 복제되지 않고 Codex에서 지운 서버는 다음 로드에서 사라집니다.
- `~/.claude.json`의 프로젝트 항목은 정규화한 작업 폴더 경로로 고릅니다. 한 프로젝트의 서버가 다른 저장소 세션에 섞이지 않습니다.
- 사용자 수준 파일은 `FileStamp`가 같은 동안 다시 파싱하지 않고 프로세스 캐시(최대 16개)를 씁니다. 세션이 시작될 때 바뀐 사용자 설정이 없으면 파일을 하나도 읽지 않습니다. 작업 폴더 파일은 캐시하지 않습니다. 신뢰 판정과 같은 한 번의 읽기에서 나온 바이트만 파싱해야 하기 때문입니다.
- 환경 변수 펼치기(`${VAR}`)와 실행할 수 없는 항목 제거는 합친 뒤 매번 합니다.

### 리소스와 프롬프트

리소스나 프롬프트를 공개하는 서버도 같은 `mcp` 도구로 다룹니다. 그래서 코어 도구 목록과 지연 로딩 구성이 달라지지 않습니다.

| 액션 | 인자 | 결과 |
| --- | --- | --- |
| `resources` | 선택 `server` | 리소스마다 한 줄(URI, 이름, MIME 타입, 설명). 서버 이름을 주면 그 서버만, 빼면 연결된 모든 서버를 봅니다. |
| `read_resource` | `server`, `uri` | 리소스 본문. 텍스트는 도구 결과와 같은 비밀 마스킹을 거치고 `accept_large_output: true`가 아니면 MCP 결과 예산에 맞춰 줄입니다. 바이너리 본문은 MIME 타입과 디코딩한 크기로 요약합니다. |
| `prompts` | 선택 `server` | 프롬프트마다 인자와 함께 한 줄. 필수 인자는 이름 뒤에 `*`를 붙입니다. |
| `get_prompt` | `server`, `name`, 선택 `arguments` | 서버가 펼친 프롬프트를 텍스트로 돌려줍니다. 다른 도구 출력처럼 호출자에게 반환할 뿐 대화에 자동으로 넣지 않습니다. |

- `resources`나 `prompts` 기능을 알리지 않은 서버에는 `MCP resources are not supported by '<server>' …`를 돌려주고 JSON-RPC 요청을 보내지 않습니다.
- `notifications/resources/list_changed`와 `notifications/prompts/list_changed`는 캐시한 목록을 버리므로 다음 `resources`나 `prompts` 호출이 다시 받아 옵니다. 캐시는 연결에 속하고 재연결 뒤에는 빈 상태로 시작합니다.
- 리소스 구독(`resources/subscribe`), 샘플링, 루트는 구현하지 않았습니다.

## 큰 도구 출력과 컨텍스트 절약

도구 출력이 모델 컨텍스트에 들어가는 양을 줄이는 장치 세 가지입니다. 셋 모두 원문을 버리지 않습니다. 모델은 줄어든 결과와 함께 원문을 다시 읽는 주소를 받습니다.

### 세션 아티팩트와 `artifact://`

예산을 넘는 출력의 전체 본문은 에이전트 홈의 `artifacts/<세션 id>/<id>.<도구>.log`에 저장됩니다 (`crates/base/src/artifacts.rs`). `id`는 그 디렉터리에 이미 있는 가장 큰 id보다 하나 큰 수라서, 재시작해도 트랜스크립트가 아직 가리킬 수 있는 id를 다시 쓰지 않고 이어 갑니다. 세션 id는 세션 저장소가 쓰는 문자 집합으로 검증합니다. 본문은 저장 전에 히스토리와 같은 비밀값 마스킹을 거칩니다.

- 모델은 `read file_path="artifact://<id>"`에 `start_line`/`end_line`을 주어 원문을 쪽 단위로 읽습니다. 다른 세션의 아티팩트는 읽지 못합니다.
- 아티팩트 하나는 앞 8 MiB와 뒤 8 MiB를 남기고, 생략된 가운데를 줄 경계에서 `[ARTIFACT TRUNCATED: ...]` 안내로 바꿔 씁니다.
- 한 세션의 아티팩트가 256 MiB를 넘으면 가장 낮은 id부터 지워 세션을 다시 상한 아래로 되돌립니다.
- 세션 저장소(`sessions/<id>/`) 밖에 두는 이유는 그 디렉터리를 `doctor`와 복제가 고정 배치로 가정하기 때문입니다.

### 응축 규칙

예산을 넘는 출력은 앞부분, 생략 구간의 신호 줄, 뒷부분, 원문 주소로 바뀝니다. 앞뒤는 각각 예산의 40%이고 줄 경계에서 자릅니다. 생략 구간에서 `error`, `warning`, `failed`, `panic`, `exception`, `traceback`, `fatal`, `assert` 같은 단어가 있는 줄을 `L<원본 줄번호>: <줄>` 형식으로 남은 예산만큼 옮깁니다(줄당 300바이트).

| 지점 | 예산 |
| --- | --- |
| `bash` | 30,000바이트 |
| `webfetch` | 40,000바이트 (다운로드 상한 5 MB는 그대로) |
| `read`의 PDF 텍스트 | 64 KiB |
| 그 밖의 도구(MCP, `agentgrep` 등) | 48 KiB, `Registry::execute`에서 컨텍스트 가드 직전에 적용 |

`accept_large_output: true`를 준 호출과 `read`의 텍스트 파일은 응축하지 않습니다. 텍스트 `read`는 대신 한 번에 64 KiB까지만 돌려주고, 다음 `start_line`(또는 `offset`)을 안내합니다. 파일은 디스크에 있으므로 따로 저장하지 않습니다. SDK 도구 결과는 레지스트리를 거치지 않으므로 히스토리 상한(512 KiB, 바이트 기준)만 받습니다.

### `read` 중복 단락

같은 세션에서 이미 읽은 파일을 다시 읽으면 내용 해시를 비교합니다 (`crates/app-core/src/tool/read_state.rs`).

- 내용이 같고 요청한 줄이 앞서 보여 준 범위 안에 있으면 본문 대신 `[read: <경로> lines a-b are unchanged since your earlier read (tool call <id>) ...]` 한 줄을 돌려줍니다.
- 내용이 바뀌었고 앞서 파일 전체를 읽었으며 이번에도 전체를 읽으면, 앞선 읽기와의 unified diff를 돌려줍니다. diff가 파일의 40%보다 크면 전체를 보냅니다. 기준 스냅샷은 1 MiB 이하 파일만 남기고 `artifact://`로는 읽을 수 없습니다.
- `full: true`를 주면 항상 본문을 보냅니다. `hashline` 형식이 앞선 읽기와 다르면 다시 그립니다.
- 압축 적용, 대화 되감기·수정, 긴급 절단, 오래된 결과 축약, 세션 종료 때 그 세션의 기록을 지웁니다. 앞선 출력이 컨텍스트에 그대로 남아 있을 때만 단락이 맞기 때문입니다.

설정은 `[tools] read_dedup = true`(기본값), 환경변수 `HORON_AGENT_READ_DEDUP`입니다.

### 오래된 결과 묶음 축약

앞쪽 메시지를 고치면 그 지점부터 프롬프트 캐시가 깨지므로, 결과를 한 턴에 하나씩 줄이지 않고 한 번에 묶어 줄입니다 (`crates/app-core/src/agent/tool_result_aging.rs`). 대상은 최근 24개 메시지 밖, 압축 경계 뒤에 있는 8 KiB 초과 도구 결과입니다.

- 마지막 프로바이더 응답 뒤 캐시 수명(경로가 알려 주지 않으면 300초)이 지났으면 실행합니다. 캐시가 이미 식어 다시 쓰는 비용이 없습니다.
- 캐시가 살아 있으면 컨텍스트가 압축 임계의 75% 이상이고 대상 합계가 64 KiB 이상일 때만 실행합니다.
- 압축 작업이 진행 중이거나 프로바이더 고유 압축 경로이면 실행하지 않습니다.

축약된 결과는 `[horon: condensed older tool output: ...]`로 시작하고 앞뒤 1 KiB와 원문 `artifact://` 주소를 남깁니다. 이 접두어가 있는 결과는 다시 축약하지 않습니다. 묶음마다 KV 캐시 모니터에 "tool result aging"을 문서화된 무효화로 기록하고 세션을 한 번 저장합니다. 설정은 `[tools] tool_result_aging = true`(기본값), 환경변수 `HORON_AGENT_TOOL_RESULT_AGING`입니다.

### 계측

`horon debug 'server:memory'`의 `process.stage_counters`에 `tool_output_spills`, `tool_output_spill_original_bytes`, `tool_output_spill_kept_bytes`, `read_unchanged_hits`, `read_delta_hits`, `read_bytes_avoided`, `tool_result_aging_batches`, `tool_result_aged_results`, `tool_result_aged_bytes`가 나옵니다. 구조 게이트(`scripts/perf/check_structural.py`)의 `context` 섹션은 1 MiB 출력이 48 KiB 안에 들어오는지, 반복 읽기가 512바이트 미만인지, 압박 구간을 두 번 지나도 축약 묶음이 정확히 한 번인지 확인합니다.

## 보조 완료와 비밀값 왕복

대화를 건드리지 않는 곁가지 호출 하나의 계약과, 자격증명이 프로바이더로 나갔다가 도구에서 실제 값으로 돌아오는 왕복을 설명합니다. 두 장치는 서로 다른 문제를 다루지만 경계를 긋는 방식이 같습니다. 호출자가 정할 수 있는 것과 정할 수 없는 것을 타입으로 가르고, 경계를 넘는 값은 이름이 붙은 오류로 거절합니다.

### 보조 완료 계약

`CompletionRequest`는 필드 넷만 가집니다 (`crates/base/src/aux_completion.rs:90`). 프롬프트, 선택적 시스템 텍스트, 선택적 출력 토큰 상한, 선택적 응답 스키마입니다. 모델, 엔드포인트, 자격증명, 도구를 고르는 필드도 설정자도 인자도 없습니다. 그래서 호출자는 어느 모델이 도는지, 요청이 어디로 가는지, 누가 비용을 내는지, 모델이 행동할 수 있는지를 바꾸지 못합니다. 그 넷은 요청을 내보내는 세션에 남습니다.

| 항목 | 상한 | 위치 |
| --- | --- | --- |
| `prompt` | 65,536바이트 (64 KiB) | `crates/base/src/aux_completion.rs:70` |
| `system` | 16,384바이트 (16 KiB) | `crates/base/src/aux_completion.rs:73` |
| `max_tokens` | 1 이상 4,096 이하 | `crates/base/src/aux_completion.rs:76`, `:79` |

상한은 글자 수가 아니라 바이트로 잽니다. 두 문자열 모두 `len()`으로 비교합니다 (`crates/base/src/aux_completion.rs:101`, `:118`). 한국어 프롬프트는 같은 글자 수에서 바이트가 세 배라, 글자로 재면 상한이 조용히 세 배가 됩니다.

계약을 벗어난 요청은 값이 만들어지는 자리에서 거절됩니다. 모델 호출이 이미 돈을 쓴 뒤에야 문제를 알게 되는 구조를 만들지 않으려는 선택입니다.

| 오류 접두사 | 언제 | 위치 |
| --- | --- | --- |
| `AUX_COMPLETION_INPUT` | 프롬프트나 시스템 텍스트가 상한을 넘거나 `max_tokens`가 범위 밖일 때 | `crates/base/src/aux_completion.rs:64` |
| `AUX_COMPLETION_SCHEMA` | 스키마가 객체가 아니거나 응답이 스키마를 어겼을 때 | `crates/base/src/aux_completion.rs:67` |

시스템 텍스트를 주지 않으면 고정 문구가 대신 쓰입니다 (`crates/base/src/aux_completion.rs:82`). 그 문구는 텍스트로만 답하고 도구를 부르지 말라고 지시합니다. 호출자가 준 텍스트가 있으면 그것이 고정 문구를 대체합니다.

### 스키마 부분집합

스키마 검사는 JSON Schema 크레이트가 아니라 이 모듈 안의 작은 검증기입니다 (`crates/base/src/aux_completion.rs:207`). `base` 크레이트는 의존 그래프의 바닥이라 `agent-sdk`가 `base`를 의존하는 방향입니다. 여기에 전체 검증기를 들이면 그 방향이 뒤집히거나, 필드 하나 때문에 새 외부 의존성이 붙습니다.

지원하는 키워드는 정확히 다섯입니다.

| 키워드 | 뜻 |
| --- | --- |
| `type` | `object`, `array`, `string`, `number`, `integer`, `boolean`, `null`, 또는 그 이름들의 배열 |
| `required` | 객체가 반드시 가져야 하는 속성 이름 |
| `properties` | 속성별 하위 스키마. 그 속성이 있을 때만 검사합니다. |
| `items` | 배열 원소마다 만족해야 하는 하위 스키마 |
| `enum` | 응답이 가질 수 있는 값 |

나머지 키워드는 무시합니다. `additionalProperties`, `minLength`, `pattern`, `format`, 숫자 범위, `oneOf`/`anyOf`/`allOf`, `$ref`가 모두 여기 들어갑니다. 그 키워드에 기대는 스키마는 보이는 것보다 약합니다. 그래서 부분집합을 암시하지 않고 문서에 적습니다.

### 도구 등록

`aux_complete` 도구는 `[tools] aux_completion`이 켜져 있을 때만 등록됩니다 (`crates/app-core/src/tool/mod.rs:2425`). 등록 함수가 설정을 읽고 꺼져 있으면 곧바로 반환합니다. 꺼진 상태에서는 도구 목록에 나타나지 않고 이름으로도 찾히지 않습니다.

`CORE_TOOLS`에는 없습니다 (`crates/base/src/config.rs:756`). 코어 프로필의 닫힌 목록은 넓어지지 않습니다. 호출마다 곁가지 모델 요청을 하나 더 쓴다는 점이 이 스위치를 기본 꺼짐으로 두는 이유입니다 (`crates/base/src/config.rs:930`).

도구 자체는 얇은 어댑터입니다 (`crates/app-core/src/tool/aux_complete.rs:99`). 모델 인자를 계약 타입으로 바꿔 넘기고 텍스트를 돌려줄 뿐, 상한도 스키마 검사도 직접 하지 않습니다. 검사가 두 곳에 있으면 표면마다 규칙이 갈라지기 때문입니다.

### 비밀값 자리표시자

자격증명이 프로바이더로 나갈 때 실제 값 대신 자리표시자가 실립니다. 모양은 `«SECRET:<label>:<base>»`입니다. `label`은 검출 규칙의 이름이고, `base`는 실제 값 하나에 대응하는 12자 문자열입니다. `base`는 horon-agent 홈의 `secret-placeholder.key`에 저장된 32바이트 키로 계산한 HMAC-SHA256 앞 8바이트를 36진수(`A`–`Z`, `0`–`9`)로 적은 값입니다. 키 파일은 0600으로 만들고, 없으면 새로 만듭니다. 키 파일을 만들지 못하면 프로세스마다 새 임시 키를 쓰고 경고를 남깁니다.

같은 실제 값은 언제나 같은 자리표시자를 받습니다. 한 값이 대화에 여러 번 나와도 자리표시자가 바뀌지 않으므로, 캐시되는 프롬프트 접두사가 흔들리지 않습니다. 키가 파일에 남으므로 재시작 뒤 이어서 여는 세션도 예전 자리표시자를 그대로 복원합니다. 세션 보관함을 만들 때 이름이 `KEY`, `SECRET`, `TOKEN`, `PASSWORD`, `PASS`, `AUTH`, `CREDENTIAL`, `PRIVATE`에 해당하고 값이 8자 이상인 환경 변수를 미리 등록하므로, 환경 변수에서 온 자격증명의 자리표시자도 재시작 뒤에 풀립니다.

세 모드가 있습니다 (`crates/config-types/src/lib.rs:556`).

| 모드 | 동작 |
| --- | --- |
| `off` | 텍스트를 그대로 보냅니다. 기본값입니다. |
| `obfuscate` | 검출된 자격증명을 안정된 자리표시자로 바꾸고, 실제 값이 필요한 도구에는 원래 값을 돌려줍니다. |
| `block` | 자격증명이 바깥으로 나갈 호출을 거절합니다. |

모드 이름은 케밥 표기로 읽고, 모르는 값은 기본값으로 떨어집니다 (`crates/config-types/src/lib.rs:589`). 설정 파일의 다른 항목까지 못 읽게 되지 않도록 한 처리입니다. 설정 항목 이름은 `secrets_mode`이고 (`crates/base/src/config.rs:933`), 기본 설정 파일에도 `secrets_mode = "off"`로 적혀 있습니다 (`crates/base/src/config/default_file.rs:470`).

검출 규칙은 새로 만들지 않고 기존 마스킹과 같은 표를 씁니다. 디스크 경로의 `redact_secrets`가 쓰는 패턴 목록이 그대로 와이어 경로의 목록입니다 (`crates/base/src/message.rs:51`). 표가 하나이므로 디스크를 위해 추가한 패턴이 와이어에도 자동으로 적용됩니다. 이미 마스킹된 텍스트를 다시 마스킹하지 않는 것도 규칙입니다. 자리표시자를 다른 패턴이 한 번 더 감싸면, 바깥것을 복원해도 안쪽 자리표시자만 돌아옵니다.

### 세 배선 지점

왕복은 세 자리에서 일어납니다.

1. 프로바이더로 나가는 메시지. 요청 사본에만 자리표시자가 실립니다. `off` 모드에서는 사본을 만들지 않고 들고 있던 메시지를 그대로 보냅니다.
2. 도구 입력. 게이트에 닿기 전에 자리표시자를 실제 값으로 되돌립니다.
3. 도구 출력. 이력에 들어가기 전에 다시 자리표시자로 가립니다.

복원이 게이트보다 앞이라는 점이 이 설계의 불변식입니다. 자리표시자를 본 승인자는 아무도 실행하지 않을 명령을 승인하게 됩니다. 도구는 복원된 입력을 받아 실행하므로, 게이트가 본 것과 실제로 실행되는 것이 같아야 합니다. 두 단계는 한 함수로 묶여 있어 호출 지점이 순서를 바꾸지 못합니다.

자리표시자와 실제 값의 대응은 프로세스 메모리에만 있습니다. 세션별 보관함은 실제 값에서 자리표시자로 가는 표, 그 반대 표, 이름별 번호 카운터로 이루어지고 디스크에 쓰지 않습니다. 프로세스가 끝나면 사라집니다. 디버그 출력도 값을 찍지 않고 항목 수만 찍습니다. 로그 한 줄이나 크래시 번들이 보관함 자체를 통해 자격증명을 흘리지 않게 하려는 처리입니다.

`block` 모드는 거절할 때 `SECRET_BLOCK`으로 시작하는 오류를 돌려주고, 몇 건이 어느 이름으로 걸렸는지와 `obfuscate`로 바꾸는 방법을 함께 적습니다.
