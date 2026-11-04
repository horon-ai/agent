# 생명주기 훅

horon-agent는 잘 정의된 생명주기 지점에서 외부 명령을 실행할 수 있으므로 다른
프로그램이 horon-agent의 소스를 고치지 않고도 에이전트 동작을 관찰하거나 통제할 수 있다.
생명주기 훅은 세션 안에서 *무슨 일이 일어나는지*를 외부 명령에 알려 준다.

## 설정

```toml
# ~/.horon-agent/config.toml
[hooks]
turn_start    = "~/bin/horon-agent-turn-busy"       # observer
turn_end      = "~/bin/horon-agent-turn-notify"     # observer
session_start = ""                            # observer
session_end   = ""                            # observer
pre_tool      = "~/bin/horon-agent-tool-policy"     # gate
post_tool     = ""                            # observer
post_tool_feedback = "~/bin/horon-agent-vale"       # feeds output back to the model
pre_tool_timeout_ms = 5000
post_tool_feedback_timeout_ms = 10000
```

명령 키 여덟 개(`pre_tool_transform`, `turn_start`, `turn_end`,
`session_start`, `session_end`, `pre_tool`, `post_tool`,
`post_tool_feedback`)는 명령 문자열이나 문자열 배열을 받는다.
독립 플러그인은 디스패처 없이 한 이벤트를 함께 쓸 수 있다.

```toml
[hooks]
turn_end = ["~/bin/horon-agent-turn-notify", "~/bin/horon-agent-event-log"]
pre_tool = ["~/bin/horon-agent-tool-policy", "~/bin/horon-agent-audit-policy"]
```

빈 명령과 공백만 있는 명령은 건너뛴다. 관찰자 명령은 선언 순서로 스폰되지만
독립적으로 실행되므로 완료 순서는 정해져 있지 않다. 게이트 명령은 차단이 일어난
뒤에도 선언 순서대로 순차 실행되고, 첫 번째 차단 결과가 반환된다. 변환기와 피드백
훅도 아래에서 설명하는 대로 선언 순서대로 순차 실행된다.

환경변수 재정의는 항상 우선하며, 빈 값은 설정된 훅을 끈다.
`HORON_AGENT_HOOK_TURN_START`, `HORON_AGENT_HOOK_TURN_END`,
`HORON_AGENT_HOOK_SESSION_START`, `HORON_AGENT_HOOK_SESSION_END`,
`HORON_AGENT_HOOK_PRE_TOOL`, `HORON_AGENT_HOOK_POST_TOOL`,
`HORON_AGENT_HOOK_PRE_TOOL_TIMEOUT_MS`,
`HORON_AGENT_HOOK_POST_TOOL_FEEDBACK`,
`HORON_AGENT_HOOK_POST_TOOL_FEEDBACK_TIMEOUT_MS`,
`HORON_AGENT_HOOK_PRE_TOOL_TRANSFORM`,
`HORON_AGENT_HOOK_PRE_TOOL_TRANSFORM_TIMEOUT_MS`.
명령 재정의는 명령 문자열이나 TOML 문자열 배열을 받는다. 예를 들어
`HORON_AGENT_HOOK_TURN_END='["notify-one", "notify-two"]'`처럼 쓴다.

## 공통 계약

- 훅 명령줄은 셸 방식으로 파싱하지만(따옴표와 백슬래시 이스케이프가 동작한다)
  셸을 거치지 않고 **직접** 실행한다. 프로그램 경로 앞의 `~/`는 확장한다.
- 훅은 세션 작업 디렉터리를 알 수 있으면 그 디렉터리에서 실행된다.
- 모든 훅은 다음을 받는다.

| 변수 | 의미 |
| --- | --- |
| `HORON_AGENT_HOOK_EVENT` | `turn_start`, `turn_end`, `session_start`, `session_end`, `pre_tool`, `post_tool`, `post_tool_feedback`, `pre_tool_transform` |
| `HORON_AGENT_HOOK_SESSION_ID` | 이벤트가 속한 세션 |
| `HORON_AGENT_HOOK_CWD` | 세션 작업 디렉터리 |
| `HORON_AGENT_HOOK_PAYLOAD` | 모든 필드를 그대로 담은 JSON 객체(최대 16 KB) |
| `HORON_AGENT_HOOKS_DISABLED` | 항상 `1`이다. 중첩된 horon-agent 호출에서 훅을 억제한다(재귀 방지) |

## 관찰자 훅

`turn_start`, `turn_end`, `session_start`, `session_end`, `post_tool`은
**관찰자**다. 분리해서 스폰하고 결과를 기다리지 않는다. 에이전트는 이들의 완료를
기다리지 않으며 실패는 기록만 한다.

### `turn_start`

모델 생성과 첫 도구 호출 전에 발생한다. 대상은 스트리밍 턴 루프를 쓰는 `chat`
진입점과 일회성 `run` 경로다. 추가 필드는 `HORON_AGENT_HOOK_MODEL`과
`HORON_AGENT_HOOK_SOURCE`(`chat` 또는 `run`)다.

현재 구현은 `resume`이나 `ambient`라는 별도 source 값을 보내지 않는다. 오래된 설정
주석에 있는 그 이름들은 런타임 보장이 아니다.

### `turn_end`

에이전트 턴이 끝날 때 발생한다(스트리밍 턴 경로는 데스크톱, swarm 워커, headless
세션을 포함하며, 일회성 `horon-agent run`도 여기에 해당한다).

추가 필드는 `HORON_AGENT_HOOK_STATUS`(`ok`/`error`), `HORON_AGENT_HOOK_DURATION_MS`,
`HORON_AGENT_HOOK_MODEL`, `HORON_AGENT_HOOK_LAST_ASSISTANT_TEXT`(앞 4000자),
`HORON_AGENT_HOOK_PROMPT_TOKENS`, `HORON_AGENT_HOOK_INPUT_TOKENS`,
`HORON_AGENT_HOOK_OUTPUT_TOKENS`,
`HORON_AGENT_HOOK_CACHE_READ_TOKENS`,
`HORON_AGENT_HOOK_CACHE_WRITE_TOKENS`, 그리고 실패 시
`HORON_AGENT_HOOK_ERROR`다.

토큰 필드는 현재 턴의 어시스턴트 메시지에서 합산한다
(`crates/app-core/src/agent/turn_execution.rs:256-270`). `PROMPT_TOKENS`는
제공자가 보고한 전체 프롬프트 크기이며 있을 때만 채운다. `INPUT_TOKENS`는 제공자가
보고한 원시 입력 수다. 이 값들은 제공자 사이에서 정규화되지 않는다. 이 빌드는 모델
호출이 OpenAI 호환 엔드포인트 하나로 나가므로
(`crates/base/src/provider/openai.rs:16`) 캐시 입력이 `INPUT_TOKENS`에
포함되는지는 그 상류의 보고 방식을 따른다. 캐시 필드는 턴 안에 그 지표를 보고하는
어시스턴트 메시지가 없으면 생략한다.

### `session_start` / `session_end`

`session_start`는 에이전트 세션이 활성화될 때 발생하며
`HORON_AGENT_HOOK_SOURCE`는 `create`(완전히 새 세션), `attach`(기존 세션 객체
연결), `resume`(ID로 복원) 중 하나다. `session_end`는 정상 종료 시 발생한다
(`HORON_AGENT_HOOK_SOURCE=close`).

### `post_tool`

모든 도구 호출 뒤에 발생한다. 추가 필드는 다음과 같다.

| 변수 | 의미 |
| --- | --- |
| `HORON_AGENT_HOOK_TOOL_NAME` | 해석된 도구 이름 |
| `HORON_AGENT_HOOK_TOOL_CALL_ID` | 제공자 도구 호출 id |
| `HORON_AGENT_HOOK_TOOL_INPUT` | 도구 입력 JSON(최대 16 KB) |
| `HORON_AGENT_HOOK_TOUCHED_PATHS` | 입력이 지목한 절대 경로를 줄바꿈으로 구분한 목록(`file_path`, `path`, `paths`, `apply_patch` 헤더, ...) |
| `HORON_AGENT_HOOK_STATUS` | `ok` / `error` |
| `HORON_AGENT_HOOK_DURATION_MS` | 도구 실행 시간 |
| `HORON_AGENT_HOOK_OUTPUT_BYTES` | 출력 크기(성공 시) |
| `HORON_AGENT_HOOK_ERROR` | 오류 메시지(실패 시) |

`HORON_AGENT_HOOK_TOUCHED_PATHS`는 도구 입력에서 도출하므로 도구가 읽거나 쓰라고
지시받은 경로를 나열한다. `bash` 명령이 우연히 바꾼 파일은 추적하지 않는다.

## 피드백 훅: `post_tool_feedback`

`post_tool_feedback`은 **도구 호출이 성공할 때마다 동기적으로** 실행되며 모델이 보는
결과에 텍스트를 더할 수 있다. `post_tool`과 같은 환경 필드를 받고 표준 입력으로 전체
도구 입력 JSON을 받는다.

- 다듬은 **표준 출력**은 `[post_tool_feedback]` 표시 아래 도구 결과에 덧붙는다(훅당
  최대 8 KB). 표준 출력이 비어 있으면 아무것도 더하지 않는다.
- 종료 코드는 무시한다. 린터는 보통 발견 사항을 보고할 때 0이 아닌 코드로 끝나기
  때문이다. 표준 오류는 버린다.
- 시간 초과(`post_tool_feedback_timeout_ms`, 기본 10초)와 스폰 실패는 아무것도 더하지
  않고 기록만 한다.
- 여러 훅은 선언 순서대로 실행되고 출력은 이어 붙인다.

빠르게 유지하고 앞단에서 걸러야 한다. 이 훅은 읽기를 포함한 모든 도구 호출에서
실행된다.

```bash
#!/usr/bin/env bash
# ~/bin/horon-agent-vale: lint prose the agent just wrote
case "$HORON_AGENT_HOOK_TOOL_NAME" in
  write|edit|multiedit|apply_patch) ;;
  *) exit 0 ;;
esac
while IFS= read -r path; do
  case "$path" in
    *.md|*.mdx|*.txt|*.rst) [ -f "$path" ] && vale --output=line "$path" ;;
  esac
done <<<"$HORON_AGENT_HOOK_TOUCHED_PATHS"
exit 0
```

## 게이트 훅: `pre_tool`

`pre_tool`은 **도구 호출 전마다 동기적으로** 실행되며 호출을 막을 수 있다.

- 훅은 `HORON_AGENT_HOOK_TOOL_NAME`과 전체 도구 입력 JSON을 **표준 입력**으로
  받는다(그리고 16 KB로 자른 사본을 `HORON_AGENT_HOOK_TOOL_INPUT`으로 받는다).
- **종료 코드 0**: 호출을 허용한다.
- **종료 코드 2**: 호출을 막는다. 훅의 표준 오류(다듬고 최대 2000자)가 도구 오류로
  모델에 반환되므로 모델이 대응할 수 있다.
- **그 밖의 경우는 열린 실패(fail open)** 로 처리하고 경고를 기록한다. 다른 종료
  코드, 시간 초과(`pre_tool_timeout_ms`, 기본 5초), 바이너리 없음, 스폰 오류가
  여기에 해당한다.

열린 실패는 의도한 것이다. 망가진 정책 스크립트는 모든 세션을 망가뜨리는 대신 "정책
없음"으로 약화되어야 한다. 닫힌 실패(fail-closed)가 필요하면 훅 자체를 견고하게
만들어야 한다. 신뢰 경계는 horon-agent가 아니라 훅이다.

## 도구 입력 변환기: `pre_tool_transform`

변환기는 도구의 JSON 입력을 검증하고 실행하기 전에 교체할 수 있는 동기 외부
플러그인이다. 순서대로 하나 이상 설정한다.

```toml
[hooks]
pre_tool_transform = ["~/.horon-agent/plugins/rtk-transform"]
pre_tool_transform_timeout_ms = 500
```

이 예시의 플러그인은 표준 입출력으로 실행하는 외부 프로그램이며, 제거된 네이티브 플러그인 시스템과는 무관하다. 훅은 지금도 이렇게 별도 프로세스를 띄우는 방식으로 동작한다.

각 변환기는 표준 입력으로 현재의 전체 도구 입력 JSON을 받고 환경에서
`HORON_AGENT_HOOK_TOOL_NAME`, `HORON_AGENT_HOOK_SESSION_ID`, `HORON_AGENT_HOOK_CWD`를
받는다. 표준 출력으로 교체할 JSON 객체를 인쇄할 수 있다. 표준 출력이 비었거나, JSON이
잘못됐거나, 종료 코드가 0이 아니거나, 스폰 오류나 시간 초과가 나면 입력은 그대로 둔다.
이 열린 실패 계약 덕분에 선택 통합을 안전하게 켤 수 있다.

변환기는 설정 순서대로 합성되고 `pre_tool` 정책 게이트보다 먼저 실행되므로 정책은
최종 변환된 입력을 평가한다. 플러그인은 작게, 도구에 맞게 유지한다. 예를 들어 RTK
플러그인은 `rtk rewrite`가 대체 문자열을 돌려줄 때만 `bash.command`를 바꿔야 한다.

### 예제 정책 스크립트

```bash
#!/usr/bin/env bash
# ~/bin/horon-agent-tool-policy
# stdin: tool input JSON. Env: HORON_AGENT_HOOK_TOOL_NAME, HORON_AGENT_HOOK_SESSION_ID...
input=$(cat)

case "$HORON_AGENT_HOOK_TOOL_NAME" in
  bash)
    if grep -qE 'rm -rf /([^a-zA-Z]|$)|mkfs|dd if=' <<<"$input"; then
      echo "blocked: destructive shell command" >&2
      exit 2
    fi
    ;;
  write|edit)
    if grep -q '"file_path":"/etc/' <<<"$input"; then
      echo "blocked: writes to /etc are not allowed" >&2
      exit 2
    fi
    ;;
esac
exit 0
```

## 예제: 턴 종료 시 tmux 상태 표시와 데스크톱 알림

```bash
#!/usr/bin/env bash
# ~/bin/horon-agent-turn-notify
if [ "$HORON_AGENT_HOOK_STATUS" = ok ]; then icon=✅; else icon=❌; fi
tmux display-message "horon-agent $icon ${HORON_AGENT_HOOK_SESSION_ID:0:12}" 2>/dev/null
notify-send "horon-agent turn $HORON_AGENT_HOOK_STATUS" \
  "${HORON_AGENT_HOOK_LAST_ASSISTANT_TEXT:0:120}" 2>/dev/null
exit 0
```

## 예제: 모든 훅 활동을 JSON 이벤트 로그로 남기기

여러 훅을 한 스크립트에 연결하고 `HORON_AGENT_HOOK_EVENT`로 분기한다.

```bash
#!/usr/bin/env bash
# ~/bin/horon-agent-event-log
echo "$HORON_AGENT_HOOK_PAYLOAD" >> ~/.local/state/horon-agent-events.jsonl
```

```toml
[hooks]
turn_end      = "~/bin/horon-agent-event-log"
session_start = "~/bin/horon-agent-event-log"
session_end   = "~/bin/horon-agent-event-log"
post_tool     = "~/bin/horon-agent-event-log"
```

## 설계 메모

- 훅 조회는 설정 기반이며 설정을 다시 읽을 때 다시 조회한다. horon-agent를
  재시작하지 않고 훅을 추가하거나 바꿀 수 있다.
- 핫 경로(`pre_tool`/`post_tool`)는 페이로드를 만들기 전에 훅이 설정됐는지 확인하므로
  설정되지 않은 훅은 거의 비용이 들지 않는다.
- 재귀 방지(`HORON_AGENT_HOOKS_DISABLED=1`) 덕분에 훅은 중첩 프로세스에서 훅을 다시
  발동시키지 않고 `horon-agent` CLI 명령을 호출할 수 있다.
