# 터미널 클라이언트

언어: [English](TUI.md) | **한국어**

`horon-tui`는 하네스의 대화형 클라이언트다. 별도 패키지이자 별도 실행 파일이며 데몬과는 `horon-agent-sdk`로만 통신한다. 그래서 헤드리스 `horon-agent`의 의존성 그래프에는 터미널 라이브러리가 남지 않고, 클라이언트의 버그가 서버를 멈추게 하지도 못한다.

## 실행 방법

터미널에서 인자 없이 실행한 `horon`(또는 `hrn`, `horon-agent`)은 필요하면 서버를 띄운 뒤 자기 프로세스를 형제 `horon-tui`로 넘긴다. 넘길 때 작업 디렉터리를 `--cwd`로 전달하므로 새 세션은 명령을 실행한 디렉터리에 속한다.

프로세스를 넘기기 전에 세 조건을 모두 확인한다(`src/cli/tui_handoff.rs`).

- 표준 입력과 표준 출력이 모두 터미널이어야 한다. 파이프, 파일, 닫힌 스트림은 헤드리스 경로를 유지한다. 스크립트와 CI가 그 출력을 읽기 때문이다.
- `HORON_NO_TUI`가 설정되어 있지 않아야 한다. 값이 무엇이든 설정하면 터미널에서도 헤드리스 경로를 쓴다.
- 실행 중인 실행 파일 옆에 `horon-tui` 파일이 있어야 한다. 실행 파일이 심볼릭 링크인 런처는 실제 위치에서 다시 찾는다. macOS에서 필요한 처리다.

조건이 하나라도 맞지 않으면 프로세스는 헤드리스 에이전트로 남고, 메시지가 그 조건(`stdin and stdout are not both terminals`, `HORON_NO_TUI is set`, `no horon-tui beside this binary`)과 클라이언트를 설치하는 명령을 함께 알려 준다. `horon-tui`를 직접 실행하면 이 판정을 거치지 않는다.

재개한 세션도 그대로 넘긴다. `horon --resume <id>`는 클라이언트에 `--session <id>`를 전달하므로, 클라이언트는 새 세션을 만들지 않고 그 세션에 붙는다. 소켓은 명령줄로 넘기지 않는다. 이미 `HORON_AGENT_SOCKET`으로 내보내져 있고, 클라이언트가 하네스 API 소켓을 직접 찾는다.

클라이언트는 자체 인자를 받는다.

```text
horon-tui [--socket <path>] [--session <id>] [--cwd <dir>] [--config <file>]...
```

| 플래그 | 뜻 |
| --- | --- |
| `--socket <path>` | 찾아낸 기본값 대신 이 하네스 API 소켓을 쓴다 |
| `--session <id>` | 새 세션을 만들지 않고 기존 세션에 붙는다 |
| `--cwd <dir>` | 이 클라이언트가 만드는 세션의 작업 디렉터리 |
| `--config <file>` | 설정 파일을 하나 더 읽는다. 여러 번 쓸 수 있다 |
| `-V`, `--version` | 버전을 출력하고 끝낸다 |
| `-h`, `--help` | 사용법을 출력하고 끝낸다 |

연결이 끊기면 클라이언트가 끝난다. 이벤트 스트림이 만든 문장(예: `the event stream ended: ...`)이 배너에 남고 루프가 멈추며, 프로세스가 종료 코드 1로 끝나므로 감싼 스크립트도 실패를 알아챈다.

## 사용자 파일

다섯 파일이 클라이언트의 동작을 정한다. 넷은 `$HORON_HOME`(설정하지 않으면 `~/.horon`) 아래에, 하나는 프로젝트 아래에 둔다.

| 파일 | 범위 | 용도 |
| --- | --- | --- |
| `$HORON_HOME/tui.toml` | 모든 프로젝트 | 설정 전반과 [상태 줄](#상태-줄) |
| `$HORON_HOME/themes/<name>.json` | 모든 프로젝트 | 색 테마와 글리프 프리셋 |
| `$HORON_HOME/keybindings.toml` | 모든 프로젝트 | 키 재배치 |
| `$HORON_HOME/commands/<name>.md` | 모든 프로젝트 | 슬래시 명령 |
| `<cwd>/.horon/commands/<name>.md` | 프로젝트 하나 | 그 프로젝트의 슬래시 명령 |

다섯 파일 모두 시작할 때 읽고 클라이언트가 도는 동안 다시 읽는다. 설정 파일은 1초에 한 번 확인하며, 테마와 키 파일도 같은 주기를 따른다.

### 설정: `tui.toml`

한 프로세스의 설정은 레지스트리 기본값, 전역 파일, 프로젝트 파일, 준 순서대로의 모든 `--config` 파일, 환경 변수 순으로 정해진다. 키마다 위 계층이 이기고, 어떤 계층도 이름을 대지 않은 키는 아래 계층의 값을 유지한다. 설정 화면은 키마다 이긴 계층을 `default`, `global <path>`, `project <path>`, `config <path>`, `env <name>`으로 보여 준다.

| 키 | 형식 | 기본값 | 환경 변수 |
| --- | --- | --- | --- |
| `theme` | 문자열 | `dark` | `HORON_TUI_THEME` |
| `symbols` | `unicode`, `nerd`, `ascii` | `unicode` | `HORON_TUI_SYMBOLS` |
| `statusline.preset` | `default`, `minimal`, `custom` | `default` | `HORON_TUI_STATUSLINE_PRESET` |
| `statusline.left` | 세그먼트 이름 배열 | `["model"]` | `HORON_TUI_STATUSLINE_LEFT` |
| `statusline.right` | 세그먼트 이름 배열 | `["status"]` | `HORON_TUI_STATUSLINE_RIGHT` |
| `statusline.path_max` | 숫자 | `40` | `HORON_TUI_STATUSLINE_PATH_MAX` |
| `statusline.context_window` | 숫자 | `0` | `HORON_TUI_STATUSLINE_CONTEXT_WINDOW` |
| `editor.submit_on_enter` | 참·거짓 | `true` | `HORON_TUI_EDITOR_SUBMIT_ON_ENTER` |
| `transcript.show_reasoning` | 참·거짓 | `false` | `HORON_TUI_TRANSCRIPT_SHOW_REASONING` |

`statusline.path_max`는 `path` 세그먼트가 왼쪽부터 줄이기 전에 보여 줄 디렉터리의 최대 길이이고, `0`이면 줄이지 않는다. `statusline.context_window`는 `context_pct` 세그먼트가 나누는 토큰 수이며 `0`이면 이 세그먼트를 감춘다. `editor.submit_on_enter`와 `transcript.show_reasoning`은 다른 키처럼 값을 정하고 편집할 수 있지만, 아직 키 처리와 트랜스크립트 그리기가 이 값을 읽지 않는다.

프로젝트 파일은 `<cwd>/.horon/tui.toml`이다. 작업 디렉터리를 읽을 수 없으면 건너뛴다. 파일의 크기나 수정 시각이 바뀐 뒤 1초가 지나면 다시 읽으므로, 손으로 고친 값이 재시작 없이 적용된다.

```toml
# ~/.horon/tui.toml
theme = "dark"
symbols = "nerd"

[statusline]
preset = "custom"
left = ["status", "model", "path"]
right = ["tokens", "context_pct", "time"]
path_max = 28
context_window = 200000
```

형식이 맞지 않는 키는 경고를 남기고 건너뛰며, 파일의 나머지는 그대로 적용한다. 모르는 키도 같은 방식으로 알린다.

### `$HORON_HOME/themes/`의 테마

테마는 `name`, `colors` 표, 선택 사항인 `vars`와 `symbols` 표를 가진 JSON 객체 하나다. `vars`는 색에 이름을 한 번 붙이고 `colors`가 그 이름을 부르거나 색을 직접 적는다(`#rrggbb`). 깨진 파일은 이전 테마를 유지하고 경고를 남기며, 인터페이스를 스타일 없이 두지 않는다.

```json
{
  "name": "mine",
  "vars": { "fg": "#c0caf5", "blue": "#7aa2f7" },
  "colors": {
    "accent": "blue",
    "border": "#3b4261",
    "muted": "#565f89",
    "text": "fg",
    "success": "#9ece6a",
    "warning": "#e0af68",
    "error": "#f7768e",
    "user_text": "#7dcfff",
    "assistant_text": "fg",
    "tool_title": "blue",
    "tool_running": "#e0af68",
    "tool_done": "#9ece6a",
    "tool_error": "#f7768e",
    "notice": "#565f89",
    "prompt_border": "blue",
    "prompt_text": "fg",
    "selected_bg": "#283457",
    "status_bg": "#1f2335",
    "status_text": "fg",
    "status_accent": "blue"
  },
  "symbols": { "preset": "unicode" }
}
```

파일 이름을 테마 이름에 맞추고(`name = "mine"`이면 `themes/mine.json`) `theme = "mine"`으로 고른다. 내장 이름 `dark`와 `light`를 먼저 찾으므로 사용자 파일이 그 둘을 가릴 수 없다. 테마의 `symbols.preset`은 기본값 위 계층이 `symbols`를 정하지 않았을 때 적용되며, 프리셋은 `unicode`, `nerd`(사설 영역 너드 폰트 글리프), `ascii` 셋이다.

### 키 배치: `keybindings.toml`

이 파일은 동작 아이디를 그 동작이 응답할 키에 연결한다. 항목 하나가 그 동작의 기본 키를 모두 대체하고, 빈 배열은 동작을 끈다.

```toml
# ~/.horon/keybindings.toml
"app.submit" = ["enter", "shift+enter"]
"editor.complete" = []
"app.cancel" = ["ctrl+c", "esc"]
```

키는 수정자 `ctrl`, `alt`, `shift`를 아무 순서로 키 이름 앞에 적는다(`Control+J`와 `ctrl+j`는 같은 키다). 문자 키는 시프트를 글자 자체에 담으므로 `A`와 `shift+a`는 한 바인딩이고, 나머지 키는 시프트를 따로 가지므로 `shift+enter`가 `enter`와 다른 바인딩이 된다. 터미널이 수정자를 더 붙여 보고한 키도 같은 키의 바인딩에 응답한다. 예를 들어 `alt+left`는 `left`처럼 커서를 옮긴다.

잘못된 내용은 거절이 아니라 경고다. 모르는 동작 아이디, 배열이 아닌 값, 읽을 수 없는 키 이름, 한 범위에서 같은 키를 주장하는 두 동작이 각각 트랜스크립트에 한 줄을 남긴다. 키를 다투면 선언 순서가 앞선 동작이 그 키를 가진다.

### 명령 파일

`.horon/commands`(프로젝트)나 `$HORON_HOME/commands`(모든 프로젝트) 아래의 마크다운 파일은 파일 이름을 딴 슬래시 명령이 된다. 이름이 같은 프로젝트 파일이 사용자 파일을 이긴다. 이름이 같은 내장 명령이나 서버 명령이 있으면 파일 명령을 완전히 가리며, `/help`가 그 줄에 `(shadowed by the built-in)`을 붙인다.

두 `---` 줄 사이의 선택 블록에는 `key: value` 줄을 적는다. 읽는 키는 `description`과 `argument-hint` 둘뿐이다. 설명이 없으면 본문 첫 줄을 `/help`에 보여 준다.

```markdown
---
description: Review a diff against the project rules
argument-hint: <path>
---

Read $1 and review it against the rules in AGENTS.md. Extra context: $ARGUMENTS
```

`/review src/main.rs`는 `$1`을 `src/main.rs`로, `$ARGUMENTS`를 인자 문자열 전체로 바꾸고, `$2`부터 `$9`는 나머지 낱말이 된다. 치환은 한 번만 도므로 인자 본문에서 자리 표시자를 다시 찾지 않는다. 치환 결과가 비면 빈 메시지 대신 오류가 된다.

## 기본 키

| 동작 | 키 | 범위 |
| --- | --- | --- |
| `app.submit` | `enter` | 입력한 메시지를 보낸다 |
| `app.cancel` | `ctrl+c` | 실행 중인 턴을 취소하고, 아무것도 돌지 않으면 나간다 |
| `app.quit` | `ctrl+d` | 나간다 |
| `editor.backspace` | `backspace` | 커서 앞 글자를 지운다 |
| `editor.delete` | `delete` | 커서 자리 글자를 지운다 |
| `editor.left` | `left` | 커서를 한 글자 왼쪽으로 옮긴다 |
| `editor.right` | `right` | 커서를 한 글자 오른쪽으로 옮긴다 |
| `editor.home` | `home` | 커서를 줄 맨 앞으로 옮긴다 |
| `editor.end` | `end` | 커서를 줄 맨 끝으로 옮긴다 |
| `editor.complete` | `tab` | 입력 중인 슬래시 명령을 완성한다 |
| `prompt.allow` | `y` | 대기 중인 도구 호출을 한 번 승인한다 |
| `prompt.allow_always` | `a` | 대기 중인 도구 호출과 이후 같은 호출을 승인한다 |
| `prompt.deny` | `n` | 대기 중인 도구 호출을 거절한다 |
| `prompt.dismiss` | `esc` | 프롬프트를 무시한다. 턴은 대기 상태로 남는다 |
| `prompt.confirm` | `enter` | 여러 선택지 질문을 확정한다 |

프롬프트가 열려 있으면 모든 키를 프롬프트가 받으므로, 프롬프트에 쓰려던 글자가 입력 줄에 닿지 않는다. 탭은 슬래시 명령 이름만 완성한다. 줄에 공백이 들어가면 인자 자리는 사용자의 몫이다.

터미널이 ctrl+j를 LF 제어 문자로 보내면 `enter`로 도착하므로 그 경로로는 어떤 바인딩에도 닿을 수 없다. 다른 키를 지정한다.

## 상태 줄

화면 맨 아래 줄은 이름 붙은 세그먼트로 조립한다. 데이터가 없는 세그먼트는 아무것도 그리지 않고, 모르는 이름은 한 번 경고한 뒤 건너뛰므로 나머지 줄은 그대로 그려진다.

| 세그먼트 | 보여 주는 것 |
| --- | --- |
| `status` | 턴이 하는 일. `idle`, `running`, `requires_action` |
| `model` | `provider/model`, 또는 데몬이 알린 둘 중 있는 값 |
| `session` | 줄인 세션 아이디 |
| `path` | 작업 디렉터리. 홈 디렉터리는 `~`로 줄인다 |
| `tokens` | 마지막 요청의 입력·출력 토큰. `↑in ↓out` |
| `context_pct` | `statusline.context_window`에서 쓴 비율. 설정이 `0`이면 감춘다 |
| `time` | 현지 시각. `HH:MM` |
| `pending` | 대기 중인 승인이나 질문 |

`statusline.preset`이 배치를 고른다.

| 프리셋 | 왼쪽 | 오른쪽 |
| --- | --- | --- |
| `default` | `status`, `model`, `path` | `tokens`, `context_pct`, `time` |
| `minimal` | `model` | `status` |
| `custom` | `statusline.left`가 이름을 대는 세그먼트 | `statusline.right`가 이름을 대는 세그먼트 |

줄이 터미널 폭에 들어가지 않으면 중요도가 낮은 세그먼트부터 버린다. 중요도는 버리는 순서대로 `time`, `session`, `tokens`, `context_pct`, `path`, `pending`, `status`, `model`이다. 마지막 하나로 남은 세그먼트는 폭보다 넓어도 남긴다. 잘린 모델 이름이 빈 줄보다 쓸모 있기 때문이다.

## 명령

`/`로 시작하는 줄은 내장 명령, 명령 파일, 하네스 명령 순으로 찾는다. 모르는 이름은 `/help`를 가리키는 오류가 되고, `//`로 시작하는 줄은 슬래시 하나를 뺀 그대로의 메시지로 보낸다.

| 명령 | 하는 일 |
| --- | --- |
| `/quit` | 나간다 |
| `/help` | 명령과 각 명령의 출처, 요약을 보여 준다 |
| `/clear` | 트랜스크립트 창을 비운다 |
| `/model [name]` | 모델을 나열하고, 이름을 주면 그 모델로 바꾼다 |
| `/settings` | 설정 화면을 연다 |

하네스 명령은 붙어 있는 런타임에서 실행하며 모델에 프롬프트로 전달되지 않는다. `/help`는 한 줄에 여섯 개씩 보여 준다.

```text
/skills /context /info /memory /swarm /agents
/subagent_model /autoreview /autojudge /transport /fast /overnight
/productivity /initiatives /refresh_model_list /continue /resume_all /remote
/server_reload /review /judge /subagent /auth /logout
/plan
```

명령 파일은 인자 힌트와 설명을 붙여 `/help`에 나온다. 명령 파일이 하나도 없으면 `/help`가 파일을 둘 위치를 알려 준다.

## 화면

트랜스크립트는 보이는 행을 고르기 전에 창 폭에 맞춰 먼저 접는다. 접힌 줄은 필요한 셀만큼 여러 행을 차지한다. 최신 행이 화면에 남는다. 창은 접힌 출력의 앞부분이 아니라 꼬리를 유지한다. 행은 마지막 공백 문자 뒤에서 끊기므로, 다른 곳에서 끊을 수 있는 한 낱말을 쪼개지 않는다. 공백이 없는 행은 셀 한계에서 끊고, 들어가지 않는 넓은 문자는 쪼개지 않고 다음 행에서 시작한다. 각 셀이 원래 스팬의 스타일을 들고 있으므로 서식도 끊김을 넘어 유지된다.

입력은 한 줄이다. 줄이 창보다 길어지면 커서 앞 텍스트를 왼쪽부터 버리고, 오른쪽 끝에서 커서가 보이도록 셀 하나를 남긴다. 커서 열은 바이트가 아니라 표시 셀로 재므로 넓은 문자는 둘로 센다.

## 설정 화면

`/settings`는 설정 레지스트리로 만든다. 키마다 행이 하나씩 있고 Appearance, Status line, Editor, Transcript 탭으로 묶이며, 레지스트리가 선언한 레이블과 설명을 쓴다. 각 행은 값이 온 계층도 함께 보여 준다.

| 키 | 하는 일 |
| --- | --- |
| `up`, `down` | 행 사이를 옮긴다 |
| `left`, `tab` | 이전 탭으로 옮긴다 |
| `right`, `shift+tab` | 다음 탭으로 옮긴다 |
| `enter` | 그 행의 편집기나 선택 목록을 열고, 열려 있으면 확정한다 |
| `esc` | 편집기나 선택 목록을 닫고, 그것도 없으면 화면을 닫는다 |

키의 형식이 거절하는 값은 인라인 오류로 보이고 아무것도 쓰지 않는다. 저장은 전역 `tui.toml`에만 한다. 프로젝트 계층과 `--config` 계층은 다시 쓰지 않는다.

파일을 파싱해 그 자리에서 고치므로 이미 있는 주석, 키 순서, 배치가 저장 뒤에도 남고, 클라이언트가 시작한 뒤 손으로 고친 키도 값을 지킨다. 파싱할 수 없는 파일은 `cannot parse <path>: ...; refusing to overwrite it`으로 거절한다. 덮어쓰면 사용자의 작업이 사라지기 때문이다. 저장이 끝나면 쓴 경로를 트랜스크립트에 알리고, 파일보다 우선하는 환경 변수는 그 뒤에도 이긴 계층으로 표시된다.

## 클라이언트 설치

릴리스는 클라이언트를 에이전트 옆의 별도 압축 파일로 배포한다. 이름은 `horon-tui-<플랫폼 접미사>.tar.gz`이고 접미사는 에이전트 자산과 같다(`horon-tui-linux-x86_64.tar.gz`, `horon-tui-darwin-aarch64.tar.gz`, `horon-tui-windows-x86_64.tar.gz` 등). 설치의 나머지는 달라지지 않는다.

`scripts/install/install.sh`와 `scripts/install/install.ps1`이 이 파일을 에이전트 압축 파일 옆으로 받아 `SHA256SUMS`와 대조한다. `horon install-release`가 `builds/versions/<version>/`에 `horon-agent` 옆으로 넣으므로, 한 버전 디렉터리가 두 실행 파일을 들고 활성화가 둘을 함께 바꾼다.

압축 파일은 모든 단계에서 선택 사항이고, 검증하지 못한 사본은 에이전트 설치를 막지 않는다.

- 플랫폼용 압축 파일을 배포하지 않는 릴리스는 그렇다고 한 줄 알리고 `horon-agent`만 설치한다.
- `SHA256SUMS`에 항목이 없거나 다이제스트가 다르면 한 줄을 출력하고 헤드리스 에이전트만 설치한다.
- 소스 빌드(`--source-binary`)와 FreeBSD는 배포된 클라이언트를 넣지 않는다. FreeBSD는 `horon-agent`만 배포한다.
- `horon-agent`만 든 기존 버전 디렉터리는 파일이 없을 때만 클라이언트를 더한다. 그 밖에는 버전 디렉터리를 바꾸지 않는다.

`scripts/install/uninstall.sh`와 `scripts/install/uninstall.ps1`은 설치 디렉터리의 `horon-tui` 런처를 다른 대상과 함께 목록에 올리고 함께 지운다. 예전 설치가 남긴 런처도 포함한다.

## 알려진 한계

- Windows에는 PTY 스모크 테스트가 없다. `tui` 벤치마크는 그곳에서 렌더와 크기 지표를 기록하고 PTY 지표에는 `pty unavailable on Windows`를 적으며 실행을 실패시키지 않는다.
- FreeBSD는 `horon-agent`만 배포하므로 그곳에는 클라이언트가 설치되지 않고, 인자 없는 `horon`이 형제 파일이 없다고 알린다.
- 핸드오프는 한 프로세스가 다른 프로세스를 대체하는 방식이므로, 클라이언트가 끝나면 터미널 명령도 함께 끝난다. 종료 코드는 클라이언트의 것이다.
- `editor.submit_on_enter`와 `transcript.show_reasoning`은 선언되고 편집할 수 있지만, 아직 클라이언트가 이 값을 읽지 않는다.
