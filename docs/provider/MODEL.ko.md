# 모델 호출

언어: [English](MODEL.md) | **한국어**

에이전트는 모델 요청을 OpenAI 호환 API의 `/chat/completions`로 직접 보낸다. 사이에 라우터 프로세스나 플러그인 계층이 없고, 모델을 위해 포트를 열지 않는다. 엔드포인트는 `config.toml`의 `[providers.<name>]`에 적고, `[provider] default_provider`나 `--provider-profile`로 고른다.

## 모델 경로

모델 요청은 한 경로로만 나간다.

1. `[provider] default_provider`가 시작 프로파일 이름을 고른다. `--provider-profile <name>`을 주면 그 실행에서만 `[providers.<name>]` 표를 고른다. 내장 프로파일 id는 `[provider] default_provider`나 `--provider <id>`로 고른다.
2. 모델은 `--model <id>`, 프로파일의 `default_model`, `[provider] default_model` 가운데 정해진 값으로 고른다. 셋 다 없으면 호출이 오류로 끝난다.
3. 요청 URL은 `base_url` 뒤에 `/chat/completions`를 붙여 만든다. `base_url`에는 `/v1` 같은 API 접두사까지 적는다.
4. 도구 정의는 같은 요청 본문의 `tools`로 실린다. 별도 모델 서버가 도구를 중계하지 않는다.

프로파일을 정하지 않은 채 모델을 부르면 에이전트는 네트워크에 닿기 전에 멈추고, stderr에 `[providers.<name>]`에 `openai-compatible` 프로파일을 추가하라는 안내가 나온다.

## 명시적 프로바이더 접두사

`--model`이나 `/model`에 `openai:`, `openai-api:`, `openai-oauth:` 접두사를 붙이면 내장 OpenAI 경로를 명시한 것으로 본다. 이 접두사는 모델 이름 추론과 활성 `[providers.<name>]` 프로파일보다 먼저 처리되므로, 접두사가 붙은 모델이 활성 호환 프로파일로 흘러가지 않는다 (`crates/base/src/provider/mod.rs` `set_model`, `crates/provider-core/src/selection.rs` `explicit_model_provider_prefix`). `openai-api:`와 `openai-oauth:`는 각각 API 키와 OAuth 자격 증명 모드까지 고정한다 (`crates/provider-core/src/auth_mode.rs` `parse_explicit_credential_prefix`).

이 빌드에는 내장 OpenAI 런타임이 없다. 그래서 `openai:gpt-5` 같은 지정은 요청을 보내기 전에 실패하고, 오류는 활성 프로파일 이름을 짚으면서 그 프로파일을 쓰려면 `<profile>:<model>`을 쓰라고 안내한다. 접두사가 없는 모델 이름은 지금 활성 프로파일에 남는다. 이 동작은 `explicit_openai_prefix_never_falls_through_to_active_profile` 테스트가 고정한다 (`crates/base/src/provider/tests.rs`).

## 설정

```toml
[provider]
default_provider = "my-gw"

[providers.my-gw]
type = "openai-compatible"
base_url = "https://llm.example.com/v1"
api_key_env = "MY_GW_API_KEY"
env_file = "my-gw.env"
auth = "bearer"
default_model = "my-model"
extra_body = { chat_template_kwargs = { thinking = true } }

[[providers.my-gw.models]]
id = "my-model"
context_window = 128000

[[providers.my-gw.models]]
id = "my-vision-model"
input = ["text", "image"]
```

| 키 | 뜻 |
| --- | --- |
| `type` | `"openai-compatible"`. 이 빌드의 모델 경로가 쓰는 값이다. |
| `base_url` | 필수. API 접두사까지 포함한 주소다. `http://localhost`와 `http://127.0.0.1`도 받는다. |
| `api_key_env` | 키가 든 환경 변수 이름이다. 프로세스 환경을 먼저 보고, 값이 없으면 `env_file`을 읽는다. |
| `env_file` | 설정 디렉터리에 두는 키 파일 이름이다. `MY_GW_API_KEY=...`처럼 한 줄에 하나씩 적는다. |
| `auth` | `bearer`(기본), `header`, `none` 가운데 하나다. `header`는 `auth_header`로 헤더 이름을 정하고, 비우면 `api-key`를 쓴다. |
| `default_model` | 프로파일의 기본 모델 id다. |
| `models` | `[[providers.<name>.models]]` 배열이다. `id`가 필수이고 `context_window`, `input`, `reasoning`, `reasoning_effort`를 곁들일 수 있다. |
| `headers` | 모든 요청에 붙는 추가 HTTP 헤더다. |
| `extra_body` | 요청 본문에 합칠 최상위 JSON 객체다. 백엔드 전용 필드(`chat_template_kwargs` 등)를 넣을 때 쓴다. |
| `requires_api_key` | 키가 필요한지 명시한다. 로컬 엔드포인트처럼 키가 없으면 `false`로 둔다. |
| `prompt_cache_key` | 요청 본문에 계보 고정 `prompt_cache_key`를 실을지 정한다. `true`는 항상 싣고, `false`는 싣지 않으며, 비우면 `api.openai.com`과 `openrouter.ai` 호스트에만 싣는다. 엄격한 스키마의 게이트웨이가 모르는 필드로 400을 내면 `false`로 둔다. |

`default_model` 한 줄만으로도 호출은 된다. `models` 배열은 컨텍스트 크기와 입력 형식을 알려 모델 선택과 압축 계산을 돕는다.

## 모델 역할

모델 역할은 세션 모델이 아닌 모델이 필요한 기능들이 함께 쓰는, 작업 이름과 모델의 대응이다. 역할은 설정에만 존재한다. 역할 표가 없는 파일은 이 빌드와 똑같이 모델을 고른다.

### 역할 이름

| 역할 | 그 역할을 쓰는 작업 | 표기 |
| --- | --- | --- |
| `default` | 더 구체적인 역할이 없을 때 세션의 모든 턴. 접두사 없는 `--model`이나 `/model`은 그 실행에서 이 역할을 가리킨다. | `default` |
| `plan` | 계획 턴. 실행을 넘기기 전에 계획을 쓰는 모델이다(#70). | `plan` |
| `aux` | 값싼 보조 작업. 압축 요약, 제목, 분류, 기억 추출이다. | `aux` |
| `judge` | 타입이 있는 판단. 턴이 이미 낸 텍스트에 대해 `bool`, `choice`, `score` 질문 하나를 한 번의 제한된 호출로 답한다. 지금 유일한 호출자는 예상 밖 종료 복구다(`docs/runtime/TURNS.ko.md`). | `judge` |

다른 역할은 없다. 새 역할은 그 역할을 부르는 작업과 함께 추가한다. 이유: 부르는 곳이 없는 역할은 `default`의 두 번째 표기에 지나지 않고, 쓰는 곳보다 먼저 생긴 역할은 어느 모델을 가리켜야 하는지 검토할 수 없다.

### 역할의 위치

역할은 그 역할을 서비스하는 프로파일의 하위 표다.

```toml
[providers.my-gw]
default_model = "my-model"

[providers.my-gw.roles]
default = "my-model"
plan = "my-planner"
aux = "my-small-model"
```

역할 값은 그 프로파일 안의 모델 id다. 경로 지정이 아니므로 `openai:`, `openai-api:`, `openai-oauth:`, `<profile>:` 접두사를 붙이면 역할 이름을 짚는 설정 오류로 거부한다. 역할은 두 번째 프로파일, 엔드포인트, 자격 증명을 고르지 않으며 `[provider] default_provider`와 `--provider-profile`만 그것을 고른다. 이유: 한 세션은 엔드포인트 하나와 자격 증명 하나를 유지하므로, 역할이 호출자 몰래 요청을 다른 계정으로 옮길 수 없고 아래 폴백 순서도 하나의 사슬로 남는다.

모델 id가 있는지는 엔드포인트만 판단한다. 역할 계층은 id를 `[[providers.<name>.models]]`와 대조하지 않는다. 그 배열은 컨텍스트 크기와 모델 선택기용 메타데이터이고 이미 목록에 없는 `default_model`도 받아들인다(`crates/base/src/provider/external.rs`). 목록에 없는 모델을 가리키는 역할은 적힌 그대로 쓰고, 배열에 그 id가 들어오기 전까지는 일반 컨텍스트 기본값을 적용한다.

### 기존 작업별 키

`[compaction] summary_model`, `[agents] swarm_model`, `[agents] memory_model`은 지금 뜻을 그대로 유지하며 역할의 별칭이 아니다. 이유: 각 키는 자기 경로로 해석되고 모델 id가 아닌 값도 이해한다. 두 에이전트 키는 `inherit`을 받는다(`crates/base/src/session/agent_models.rs` `effective_agent_model`, `crates/base/src/sidecar.rs`). 그래서 이 키들을 역할 이름으로 읽으면 센티널이 모델 id로 바뀐다. 한 작업에 자기 키와 역할이 모두 있으면 작업 키가 이기고, `roles.aux`는 자기 키가 비어 있는 작업만 채운다. `roles`를 언급하지 않는 설정 파일은 이 빌드와 똑같이 동작한다.

### 해석 순서

역할을 요청한 작업은 다음 순서로 해석하고, 비어 있지 않은 값이 처음 나온 단계에서 멈춘다.

1. 이번 실행이나 턴의 덮어쓰기(`--model`은 `default`를 가리키고, 핸드오프는 기존 모델 전환으로 `plan`을 정한다).
2. `[providers.<name>.roles]`의 그 역할 항목.
3. 이 역할이 상속하는 역할들의 항목을 그 역할이 선언한 순서대로. `plan`과 `aux`는 `default`를, `judge`는 `aux` 다음 `default`를 상속한다.
4. 프로파일의 `default_model`.
5. `[provider] default_model`.
6. 비어 있던 키와 작업 이름을 짚는 오류.

빈 문자열은 어느 단계에서든 미설정으로 본다. `aux`도 같은 사슬을 걸으므로 `default_model`만 있는 프로파일은 보조 작업에 지금의 모델을 쓴다. `judge`는 `default`보다 `aux`를 먼저 상속하므로, 보조 작업용으로 값싼 모델 하나를 이미 지정한 프로파일은 세션 모델을 쓰지 않고 그 모델로 판단한다. 이유: 1, 4, 5단계는 에이전트가 지금 모델 하나를 고르는 순서다. 역할은 그 위에 끼어들므로 기존 파일은 모두 지금과 같은 모델로 풀리고, 미설정 역할은 실패하지 않고 상속한다.

### 폴백과 페일오버 순서

#73이 추가하는 순서 목록은 `[providers.<name>]`의 `fallback_models = ["m2", "m3"]`이고 역할이 아니라 프로파일에 붙으며, 역할이 해석한 모델에 적용한다. 복구는 아래 고정 순서로 들어가고, 앞 단계가 소진된 뒤에만 다음 단계로 간다.

1. 모델 폴백: 재시도 가능한 오류의 재시도 예산을 다 쓴 뒤 `fallback_models`의 다음 항목으로, 항목마다 한 번씩.
2. 같은 프로바이더 계정 페일오버(`same_provider_account_failover`, 기본 `true`).
3. 다른 프로바이더 페일오버(`cross_provider_failover`, 기본 `countdown`. 무인 실행은 `autonomous_failover`가 필요하고 기본 `false`다).

전환은 그 턴의 나머지 동안 유지되고 다음 턴은 역할의 모델로 다시 시작한다. 재시도 불가능한 오류(401, 잘못된 요청, 컨텍스트 초과)는 사슬을 시작하지 않는다. 이유: 단계마다 치르는 비용이 다르다. 모델 전환은 엔드포인트와 자격 증명, 캐시 호스트를 그대로 두고, 계정 전환은 자격 증명을 바꾸며, 프로바이더 전환은 엔드포인트를 바꾼다. 싼 것부터 하면 프롬프트 캐시 지역성을 잃지 않고 속도 제한을 흡수하고, 목록을 프로파일에 두면 위 배치 규칙이 금지한 다른 엔드포인트의 모델을 가리킬 수 없다.

### 구현 주체

#73이 공유 타입을 쓴다. `crates/config-types/src/lib.rs`의 `NamedProviderConfig`에 `roles` 표를 넣고, 모든 호출자가 쓰는 해석 함수 하나를 만든다. #70은 자기 `[prewalk]` 절만 더하고 `into`와 `from`에 그 함수를 부르며 역할을 `role:<name>`으로 적는다(`into = "role:aux"`). 이유: #73은 어차피 `NamedProviderConfig`를 바꾸고 #70의 선언된 의존성이다(`depends_on: [73, 112]`). 구현이 둘이면 서로 어긋나고, `role:` 접두사는 기존 `openai:`와 `<profile>:` 접두사처럼 역할 참조를 모델 id와 구분한다.

역할 설정이 없으면 이 빌드와 정확히 같게 동작한다. `roles` 표도, 새 키도 없고, 기존 사슬이 모델을 고르며, 추가 시도도 없다.

### 실제로 쓰이는 `judge` 역할

`crates/provider-core/src/judge.rs`가 판단의 순수한 절반을 맡는다. 프롬프트(`JudgeRequest::prompt`), 응답 스키마(`response_schema`), 한도(질문 64개, 상태 48 KiB, 질문당 지시 4 KiB, 선택지 64개, 점수 단계 64개), 그리고 없는 라벨, 범위를 벗어난 확률, 빠지거나 중복된 id, 여분 필드를 거부하는 엄격한 파서다. `crates/base/src/judge.rs`가 설정이 필요한 절반을 맡는다. `resolve_judge_model`이 위 사슬을 걷고, `judge_with`가 호출자의 프로바이더를 포크해 해석한 모델을 고르고, 시도 예산 안에서 기다린 뒤, 파싱한 답과 프로바이더 사용량을 돌려준다.

판단은 평범한 `/chat/completions` 호출이므로 이 역할은 엔드포인트도 자격 증명도 더하지 않는다. 빌드의 단일 모델 경로 규칙은 그대로다. 모델을 고를 수 없으면 오류로 끝나고 세션 모델을 조용히 물려받지 않는다. 판단의 요점이 두 번째 모델의 의견이기 때문이다. 시도 예산은 `[autojudge] attempt_timeout_ms`이고 기본 3000이며 `0`은 기한 없음이 아니라 기본값을 뜻하고, 30초를 넘는 값은 30초로 자른다. 예산은 모델 선택까지 포함한 시도 전체에 적용된다. 판단이 판정 대상 턴을 붙잡기 때문이다.

역할 이름은 `judge`이고 `/judge` 명령은 다른 작업이다. `/judge`는 스웜 안에서 검토 워커를 띄우고 세션 에이전트 모델 덮어쓰기(`/model judge`, `/agents judge <model>`)로 모델을 고른다. 둘 다 `[autojudge] model`을 모델 덮어쓰기로 읽으므로 한 설정이 두 곳의 모델을 정하지만, 판단은 워커를 띄우지 않고 워커는 타입 있는 질문에 답하지 않는다.

### 런타임에서 폴백 목록이 보이는 모습

`crates/config-types/src/model_roles.rs`의 `ModelRoleConfig::fallback_models()`는 항목마다 앞뒤 공백을 지우고 빈 문자열을 버린다. 빈 문자열이 시도로 바뀌지 않는다.

이 단계가 쓸 수 있는 목록은 활성 이름 지정 프로파일에만 있다. `crates/app-core/src/agent/model_fallback.rs`의 `active_profile_fallback_models()`가 `HORON_AGENT_NAMED_PROVIDER_PROFILE`, 그다음 `HORON_AGENT_PROVIDER_PROFILE_NAME`으로 프로파일을 찾고, 항목이 비어 있지 않을 때만 돌려준다. 이름 지정 프로파일이 아닌 세션은 이 단계가 생기기 전과 똑같이 동작한다.

`Agent::try_model_fallback`은 전환을 적용하기 전에 턴 상태에 그 항목을 기록한다. 그래야 프로바이더가 거부한 전환도 항목을 소모하고, 같은 거부를 한도 없이 재시도하지 않는다. `restore_model_fallback`은 다음 턴 시작(`begin_turn_state`), 인터럽트, 실패한 턴에서 세션 자체 모델을 되돌린다. 전환은 `session.model`에 닿지 않으므로, 다시 읽는 클라이언트나 재로드는 사용자가 고른 선택을 본다.

결과를 읽는 접근자는 둘이다. `Agent::fallback_models()`는 설정된 항목을 돌려주고, 활성 프로파일이 없으면 빈 목록이다. `Agent::last_serving_model()`은 응답을 저장할 때 `Agent::record_model_turn_usage`가 설정하므로, 다음 턴이 세션 모델을 되돌린 뒤에도 답한 모델을 가리킨다. `get_runtime_info`는 이 둘을 `fallback_models`와 `serving_model`로 보고하고, 카탈로그 푸시도 같은 쌍을 `identity` 객체에 실어 보낸다. 푸시된 카탈로그를 유지하는 클라이언트는 폴링할 필요가 없다.

### Prewalk 핸드오프

`[prewalk]`는 세션마다 한 번, 계획에서 실행으로 넘어갈 때 모델 전환 하나를 쓴다. 세션은 계획하는 동안 계획 모델로 돌고, 계획이 만들어지면 실행 모델로 계속한다.

```toml
[prewalk]
enabled = true
into = "exec-model"
from = "plan-model"
```

| 키 | 의미 |
| --- | --- |
| `enabled` | 세션이 핸드오프를 하는지 여부다. 기본값은 꺼짐이다. |
| `into` | 실행 모델이다. 모델 id 또는 `role:<name>`을 적는다. `enabled = true`일 때 필수이며, 쓸 수 있는 값이 없는 절은 `prewalk.into`를 지목하는 오류와 함께 설정 검증에 실패한다. |
| `from` | 적용 조건일 뿐이다. 값을 적으면 세션이 지금 이 모델로 돌 때만 핸드오프한다. 돌아갈 모델을 뜻하지 않으며, 비워 두면 모든 세션에 적용된다. |

핸드오프 지점은 계획 모드에 달렸다. `[tools] plan_mode`가 꺼져 있으면 항목을 하나 이상 남긴 첫 `todo` 쓰기이고, 켜져 있으면 계획 승인이다. 그래서 계획 중에 쓴 할 일 목록이 계획 아래에서 모델을 바꾸지 않는다. 전환은 `/model`과 같은 경로를 쓰므로 `session.model`에 실행 모델이 저장되고, 재개나 복원 뒤의 요청까지 모두 그 모델로 실행된다.

결정은 저장된 세션의 리플레이 이벤트 `prewalk` 하나이고, 이 이벤트가 프로세스를 넘어 핸드오프를 한 번으로 만든다. 이미 이벤트를 가진 세션은 다시 결정하지 않는다. 거부된 `into`도 같은 방식으로 기록한다. 알 수 없는 모델, `default`, `plan`, `aux` 밖의 `role:` 이름, 아무 모델로도 해석되지 않는 역할이 그렇다. 세션은 지금 모델을 유지하고 경고는 한 번만 남으며, 이후 라운드는 같은 거부를 되풀이하지 않는다. 세션이 이미 쓰는 모델로 해석되는 `into`는 전환 없이 결정만 기록한다.

`role:<name>`은 위에서 설명한 활성 프로파일의 역할 체인으로 해석된다. 그래서 `default_model`만 정한 프로파일도 `into = "role:aux"`로 핸드오프하면 같은 모델을 얻는다. 결정은 `crates/app-core/src/agent/prewalk.rs`에 있고, 인수 조건은 `crates/app-core/src/agent/prewalk_tests.rs`에 있다.

## 프롬프트 캐시 키

`prompt_cache_key`를 싣는 요청은 같은 세션 계보(포크 계보의 뿌리, 없으면 세션 id)와 같은 시스템 프롬프트, 같은 도구 목록에 대해 언제나 같은 키를 쓴다. 키는 `pck_` 뒤에 sha256 앞 12바이트를 16진수로 붙인 24자이고, 대화 턴이나 압축 결과가 바뀌어도 그대로다. 그래서 압축 뒤에도 앞부분 프롬프트가 같은 캐시 칸에 남고, 다른 계보는 다른 칸을 쓴다.

세션 id가 없는 호출에는 키를 싣지 않는다. `prompt_cache_key = true`는 호스트를 가리지 않고 싣지만, 모르는 필드를 거부하는 엔드포인트에는 `false`를 쓴다.

`api.openai.com` 호스트에는 `prompt_cache_retention`도 함께 실린다. 값은 `HORON_AGENT_OPENAI_PROMPT_CACHE_RETENTION`(`in_memory` 또는 `24h`)이 있으면 그것이고, 없으면 모델이 확장 보존을 지원할 때만 `24h`다. 기본 30분 보존을 쓰는 모델에는 필드를 싣지 않는다. 다른 호스트에는 이 필드를 싣지 않는다. `extra_body`는 두 필드보다 나중에 합쳐지므로 프로파일에서 값을 덮어쓸 수 있다.

## API 키와 파일 위치

키는 다음 순서로 찾는다.

1. `api_key_env`가 가리키는 환경 변수.
2. `env_file`이 가리키는 키 파일.
3. 내장 프로파일이면 그 프로파일이 정한 환경 변수와 파일.

`env_file`과 `config.toml`은 서로 다른 디렉터리에 있다.

| 파일 | 위치 |
| --- | --- |
| `config.toml` | `HORON_HOME`이 있으면 `<홈>/config.toml`, 없으면 `~/.horon/config.toml` |
| 키 파일 | `HORON_HOME`이 있으면 `<홈>/config/horon-agent/<env_file>`, 없으면 플랫폼 설정 디렉터리 아래 `horon-agent/<env_file>`(macOS는 `~/Library/Application Support/horon-agent/`, Linux는 `~/.config/horon-agent/`) |

키 파일은 읽을 때 권한을 좁힌다. 키 값을 `config.toml`의 `api_key`에 직접 적을 수도 있지만, 파일이 그대로 복사되기 쉬우므로 `provider add --api-key-stdin`으로 키 파일에 저장하는 편을 권한다.

## 프로파일 만들기

`provider add`가 같은 표를 `config.toml`에 쓰고, `--api-key-stdin`으로 받은 키를 키 파일에 저장한다.

```bash
printf '%s' "$MY_GW_KEY" | horon provider add my-gw \
  --base-url https://llm.example.com/v1 \
  --model my-model \
  --api-key-env MY_GW_API_KEY \
  --api-key-stdin \
  --set-default
```

`--set-default`는 `[provider] default_provider = "my-gw"`와 `[provider] default_model`을 함께 쓴다. 기존 이름을 바꾸려면 `--overwrite`를 준다. 로컬 엔드포인트처럼 키가 필요 없으면 `--auth none`을 쓴다.

만든 프로파일은 다음처럼 바로 확인한다.

```bash
horon --provider-profile my-gw --model my-model run "hello"
horon --provider-profile my-gw auth-test --prompt "Reply exactly OK"
```

## 준비 상태 확인

```bash
horon provider current --json
```

출력 필드는 `requested_provider`, `requested_model`, `resolved_provider`, `selected_model`, `ready`, 그리고 준비가 안 됐을 때의 `unavailable_reason`이다(`src/cli/commands/report_info.rs`). `ready`가 `true`면 지금 설정으로 `/chat/completions`를 부를 수 있다는 뜻이다.

## 오류 해결

| 증상 | 원인과 조치 |
| --- | --- |
| `Unknown provider profile '<name>'. Add [providers.<name>] to config.toml.` | `default_provider`나 `--provider-profile`이 가리키는 이름이 설정에 없다. 표 이름과 값을 맞춘다. |
| `Provider profile '<name>' has invalid base_url ...` | `base_url`이 비었거나 `https://` 또는 `http://localhost` 형식이 아니다. 접두사까지 적는다. |
| 키가 없다는 오류 | 오류에 채워야 할 환경 변수 이름이 나온다. 키 값은 오류에 나오지 않는다. 변수를 내보내거나 `env_file`에 적는다. |
| 401 또는 403 | 키가 틀렸거나 `auth` 형식이 엔드포인트와 맞지 않는다. 대부분 `bearer`이고, `x-api-key` 계열은 `auth = "header"`와 `auth_header`를 쓴다. |
| 404 | `base_url`에 `/v1` 같은 접두사가 빠졌을 수 있다. 에이전트는 `base_url` 뒤에 `/chat/completions`만 붙인다. |
| 모델을 찾지 못함 | `--model`, 프로파일 `default_model`, `[provider] default_model` 가운데 엔드포인트가 모르는 id가 있다. `[[providers.<name>.models]]`의 `id`와 맞춘다. |

## 내장 프로파일

`crates/provider-metadata/src/catalog.rs`에 OpenAI 호환 프로파일 43개가 있다. `openrouter`, `zai`, `kimi`, `cerebras`, `ollama`, `lmstudio`, `nvidia-nim` 같은 id를 `[provider] default_provider`에 그대로 쓸 수 있고, 어느 id든 `--provider <id>`로도 고를 수 있다. 각 프로파일은 자기 `base_url`, 키 환경 변수, 키 파일 이름을 정해 두고, 대부분은 기본 모델도 정해 둔다. 기본 모델이 없는 프로파일은 게이트웨이(`openrouter`, `orcarouter`, `conifer`, `chutes`), 로컬 런타임(`ollama`, `lmstudio`), 범용 `openai-compatible`, 그리고 `openai-api`·`anthropic-api`이며, 이때는 `--model`을 주거나 `[provider] default_model`을 적어야 한다. `[providers.<name>]` 표를 직접 만들었다면 그 표의 `default_model`이 같은 역할을 한다. `--provider-profile`은 `[providers.<name>]` 표를 고르는 플래그이므로 사용자가 정의한 이름에만 쓴다. 내장 프로파일과 같은 이름의 표를 두면 내장 프로파일이 먼저 잡히므로 다른 이름을 쓴다.

## 컨텍스트 압축

직접 프로바이더는 프로바이더 고유 압축 산출물을 만들지 않는다. 대신 horon-agent의 구조화 요약 압축이 같은 엔드포인트의 현재 모델에 요약을 요청하므로, 임계치 자동 압축과 수동 `/compact`가 모두 동작한다 (`crates/provider-openai/src/chat/mod.rs` `supports_compaction`, `crates/provider-openai/src/chat/tests.rs` `summary_compaction_is_available_for_the_chat_provider`). `[compaction] summary_model`(환경 변수 `HORON_AGENT_COMPACTION_SUMMARY_MODEL`)을 지정하면 세션 프로바이더를 fork해 그 모델로 요약하고, 그 호출이 실패하면 세션 모델로 한 번 다시 요청한다 (`crates/base/src/compaction/summary.rs`). `openai_auto`와 `openai_explicit` 같은 프로바이더 고유 전략은 이 직접 경로에 없다.

## 이전 릴리스 정리

예전 릴리스가 내려받던 네이티브 플러그인 번들 폴더 `~/.horon/native-plugin-bundles/`는 이제 쓰지 않으므로 직접 지워도 된다.

## 모델 사용 메타데이터

공유 런타임은 `ModelRoute.usage`를 제공한다. 공개 API와 Rust SDK는
`get_runtime_info()`와 푸시되는 `RuntimeInfo` 이벤트로 이를
`ModelRouteInfo.usage: Option<ModelUsage>`로 노출한다. 기존 `list_models()` 반환
타입은 바뀌지 않는다. TypeScript SDK는 같은 선택 필드 `usage`를 노출한다.

```json
{
  "count": 12,
  "last_used_unix_secs": 1789210000,
  "tracking_started_unix_secs": 1789209000,
  "selection_count": 8,
  "last_selected_unix_secs": 1789208000
}
```

### 단위와 적용 범위

- `count`는 **이 라우트에서 저장된 어시스턴트 응답이 하나 이상 있는 추적된 에이전트
  턴**을 센다. 턴은 저장된 입력 메시지에 고정된다. 도구 이어하기, 잘린 답변
  이어하기, 재적재 이어하기는 라우트마다 한 번씩 센다. 세션 저널은 핸드오프와
  재시작을 거쳐도 턴 정체성을 보존한다. 새 입력은 새 턴을 시작한다. 이후 요청이
  실패하거나 취소되어도 앞서 저장된 응답은 사용으로 남는다. 이 값은 작업 성공을
  재지 않는다.
- `last_used_unix_secs`는 마지막으로 기록된 저장 응답 시각이다. 긴 턴이 진행되는
  동안 count를 늘리지 않고 앞으로 나아갈 수 있다.
- `tracking_started_unix_secs`는 이 로컬 원장의 적용 범위를 표시한다. 추적이
  시작되기 전에는 값이 없다. count는 전체 기간의 과거 추정치가 아니다. count가
  0이면 이 범위에 기록된 턴이 없다는 뜻이며 "한 번도 쓰지 않았다"는 뜻이 아니다.
- `selection_count`와 `last_selected_unix_secs`는 headless 전환 이전 TUI가 남긴
  `model_picker_usage.json`에서 온다. 이 값은 선택기 선택을 재며 모델 요청을
  재지 않는다. effort 변형은 한 라우트 안에서 합산된다. 이 카운터는 따로 유지된다.
- `usage`가 없으면 메타데이터를 알 수 없다는 뜻이며 구버전 서버도 여기에 해당한다.
  이를 "한 번도 쓰지 않음"으로 표시하면 안 된다.

원장은 `HORON_HOME`(보통 `~/.horon`) 아래 `model-usage-v1.sqlite3`에
저장된다. SQLite가 동시 쓰기를 직렬화한다. 기본 키는 영구 세션/입력 턴 ID와 라우트
정체성을 합치므로 도구 이어하기가 반복되어도 count가 늘지 않는다. 읽기는 원장을
만들거나 바꾸지 않는다. 런타임 기록은 스트리밍 경로와 블로킹 경로가 함께 쓰며,
데스크톱, CLI, swarm 에이전트가 여기에 해당한다. 디버그 세션과 숨은 보조 모델
호출은 세지 않는다. 하나로 확정할 수 없는 서빙 라우트는 추측하지 않는다. 모델
폴백은 응답을 실제로 서빙한 모델에 귀속된다.

오래된 세션은 정확히 채울 수 없다. 그 메시지는 토큰 수는 저장하지만 모델/라우트
정체성은 저장하지 않고, 환경 스냅샷은 여덟 개만 보존한다. 세션의 최신 모델도 마지막
수정 시각도 정확한 과거 요청 귀속을 제공하지 않는다. 선택기를 열 때 대화 기록을
스캔하지 않는다.

### 순위와 최신성

`horon_agent_sdk::compare_model_usage`와 TypeScript `compareModelUsage`는 추적된 턴
수, 마지막 사용 시각, 과거 선택 횟수, 마지막 선택 시각 순으로 사용량을 좋은 순서로
비교한다. 검색 관련성과 명시적인 current/favorite 정책은 이 비교기보다 앞서고, 그
뒤에 안정적인 모델/라우트 타이브레이커가 붙는다. headless 전환 이전 TUI도 같은 비교기를 썼고
기존 current/favorite 정책을 그대로 두었다.

레거시 프로토콜 클라이언트는 `get_model_catalog.subscribe_usage_updates: true`로
`model_usage_updated` 델타 이벤트를 옵트인한다. 이 플래그의 기본값은 false이므로
새 enum 변형을 해독할 수 없는 구버전 클라이언트는 새 이벤트 종류를 받지 않는다.
새 API 브리지가 옵트인한다. 각 델타는 라우트 하나를 실어 나르므로 전체 카탈로그를
다시 만들지 않고 바쁜 Agent 잠금도 피한다. 브리지는 캐시한 카탈로그를 갱신하고
`RuntimeInfo`를 발행한다. 단조 병합은 한 추적 에포크 안에서 늦은 스냅샷이나 다른
세션의 이벤트가 count를 되돌리지 못하게 한다.

### 배포

데스크톱 핫 리로드만으로는 데몬이 갱신되지 않는다. 먼저 격리된 소켓에서 런타임을
빌드하고 검증한다. 검증한 소스 빌드를 발행한 뒤 안전한 유휴 시간에
`horon server promote <installed-version>`과 `horon server reload --json`을
쓴다. `server stop --force`를 쓰거나 kill 신호를 보내면 안 된다. 지원되는 재적재는
세션을 체크포인트하지만 진행 중인 모델 생성에는 여전히 신호를 보낸다. 세션이
처리 중이면 활성화를 미룬다.
