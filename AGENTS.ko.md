# AGENTS.md: horon `agent/`

언어: [English](AGENTS.md) | **한국어**

이 저장소는 horon agent다. 하나의 에이전트에 동등한 두 모드를 둔다. 헤드리스 모드에서는 프로그램이 세션을 구동한다(`run`, SDK와 `api-bridge` 뒤에서 도는 `serve` 데몬, 에디터용 `acp`, 호스트 프로그램용 `rpc`). TUI 모드에서는 사람이 터미널에서 `horon-tui`로 세션을 구동한다. 두 모드는 하나의 엔진, 데몬, 세션 저장소, 도구 집합, 설정을 공유하며 반대편에 누가 있느냐만 다를 뿐 어느 쪽도 다른 쪽의 축소판이 아니다. 모델 호출은 프로파일의 `type`이 고르는 네 가지 통신 규약(wire) 가운데 하나로 대화한다. OpenAI Chat Completions, OpenAI Responses, Anthropic Messages, Gemini generateContent다. 다른 문서는 코드를 설명하고, 이 파일은 코드를 규율한다. 다른 문서가 이 파일과 어긋나면 이 파일이 우선한다.

## 모드, 바이너리, 릴리스 단위

|단위|소스|무엇을 제공하나|어떻게 배포하나|
|---|---|---|---|
|`horon-agent`, 에이전트 바이너리|루트 패키지(`src/`, `crates/*`)|헤드리스 모드, 두 모드가 함께 붙는 데몬, 단순 줄 단위 프롬프트 `repl`|매 릴리스마다 `horon-agent-<suffix>.tar.gz`로 나오며 모든 설치에 필요하다|
|`horon-tui`, TUI 바이너리|`crates/tui`, `horon-agent-sdk`의 클라이언트|TUI 모드|같은 릴리스에서 에이전트 옆에 `horon-tui-<suffix>.tar.gz`로 나오며 선택 사항이고 FreeBSD에서는 제공하지 않는다|
|`@horon-ai/horon`과 6개 플랫폼 패키지|`packages/npm`, `scripts/npm`|하나의 런처로 두 바이너리를 감싼다|`publish-npm.yml`. `NPM_PUBLISH`가 `true`이면 `main` 릴리스마다 배포한다(`release: published`에서도 다시 실행해 드래프트 릴리스 상태에서 온 실행을 복구한다). 스위치가 꺼져 있을 때는 조용히 dry-run하지 않고 복구 명령을 알리며 실패한다|
|SDK|`crates/agent-sdk`, `packages/sdk/typescript`, `packages/sdk/python`|코드에서 헤드리스 모드 구동|TypeScript는 자체 `SDK_NPM_PUBLISH`로 제어하는 `publish-typescript-sdk.yml`, Python은 `pypi` 환경으로 수동 실행하는 `publish-python-sdk.yml`을 쓴다. 아직 둘 다 공개 배포 전이다|
|컨테이너 이미지|`scripts/release/Containerfile`|헤드리스 모드의 데몬|`release-tag.yml`이 `ghcr.io/horon-ai/agent`로 푸시한다|
|호스티드 테넌트별 이미지|`scripts/hosting/Containerfile`|`/data` 홈 볼륨을 가진 고객당 1 컨테이너|같은 릴리스 아카이브로 빌드한다. 공개 배포하지 않으며 컨트롤 플레인이 시작한다(`packages/docs-site/ko/reference/architecture/hosting.mdx`)|

- **하나의 릴리스.** 두 바이너리는 같은 태그 커밋(`vMAJOR.MINOR.PATCH`)에서 같은 `release.yml` 실행으로 빌드되고, `install-release`가 둘을 `builds/versions/<version>/` 한 디렉터리에 배치해 한 단위로 활성화한다. TUI 전용 릴리스나 채널은 없다. 버전 문자열은 서로 다르다. `horon-agent --version`은 태그(`crates/build-meta`)에서, `horon-tui --version`은 `crates/tui` 패키지 버전에서 온다. npm 버전은 앞의 `v`를 뺀 태그다(`packages/docs-site/ko/reference/release/npm.mdx`).
- **용어.** "헤드리스(headless)"는 모드 이름일 뿐 다른 뜻은 없다. 패키지, 바이너리, 엔진, 제품 자체를 "headless"라 부르지 말고 "에이전트 바이너리 `horon-agent`"와 "TUI 바이너리 `horon-tui`"라고 쓴다. 클라이언트가 붙지 않은 세션(swarm 워커, ambient 재개)은 *무인 세션(unattended session)*이라 부른다. 아직 `headless`가 남은 코드 식별자, 파일 이름, 설정 값(`check_headless_graph.py`, `is_headless`, `spawn_mode = "headless"`)은 별도 승인을 받아 이름을 바꾸기 전까지 그대로 둔다.

## 고정 요구사항

이 트리의 모든 변경은 아래 세 가지를 지킨다. 하나라도 깨뜨리면 다른 무엇을 고쳤더라도 완료가 아니다.

1. **에이전트 바이너리에 터미널 UI를 넣지 않는다.** 빌드는 두 바이너리를 내보내며, 이 분리가 원칙이다. `horon-agent`(`serve`, `run`, `repl`, `api-bridge`, `acp`, `rpc`)는 전체 화면 터미널 UI를 열지 않으며 의존성 그래프에 `ratatui`, `crossterm`, `horon-tui`, `horon-agent-tui*` 크레이트를 두지 않는다. TUI 모드는 별도 패키지와 바이너리인 `horon-tui`가 맡고, `horon-agent-sdk`를 통해 데몬과 통신하며, 터미널 스택을 `horon-agent` 안으로 옮기지 않는다. 인자 없는 `horon`은 직접 아무것도 그리지 않는다. 표준 입력과 표준 출력이 터미널이고 `HORON_NO_TUI`가 설정되지 않았으며 실행 중인 바이너리 옆에 `horon-tui`가 있으면 자기 프로세스를 그 클라이언트로 교체하고(`src/cli/tui_handoff.rs`), 아니면 헤드리스 모드에 머무르며 이유를 출력한다. CI에서는 `scripts/ci/check_headless_graph.py`가 이 그래프 규칙을 검사한다.
2. **ACP v1과 v2.** `horon-agent acp`는 Agent Client Protocol `protocolVersion` 1과 2를 모두 제공해 `initialize`에 양쪽이 지원하는 가장 높은 버전으로 답해야 한다. 현재 상태는 둘 다 구현돼 있다. v2는 `src/cli/acp/v2/negotiate.rs`에서 협상하고, v1은 `src/cli/acp/util.rs`에서 응답하며 `ACP_PROTOCOL_VERSION = 1`에 고정돼 있다. v2를 더하면서 v1을 빼거나 깨뜨리지 않는다. 표준 입출력 계약, 에디터 설정, v2 차이는 `packages/docs-site/ko/reference/protocol/acp.mdx`에 있다.
3. **측정된 성능 회귀 금지.** 같은 장비, 빌드 프로필, 워크로드에서 변경 사항은 기록된 기준선과 같거나 더 빨라야 한다. 측정된 경로를 기준선보다 느리게 만든 변경은 병합하지 않는다. 측정 방법은 `packages/docs-site/ko/reference/performance/benchmark.mdx`에, 기록된 수치와 한계는 기준선인 `packages/docs-site/ko/reference/performance/results.mdx`에 있다. 적용 범위에서 기준선과 아래 jcode 목표는 `horon-agent` 워크로드(헤드리스 모드)를 다루며, 두 번째 바이너리가 생겼어도 그 셀들의 정의는 바뀌지 않는다. `horon-tui`는 따로 재며 `horon-agent` 셀에 섞거나 서로 비교하지 않는다. TUI 스위트는 `horon-tui`끼리 시간을 비교하고, TUI만 바꾼 변경에 `horon-agent` 수치를 근거로 들 수 없다.

## 성능 목표: jcode 능가

성능 작업은 가장 가까운 경쟁자인 jcode를 모든 OS와 비교 벤치마크의 모든 셀에서 코드가 허용하는 최대 폭으로 앞서는 것을 목표로 한다. 요구사항 3은 하한(우리 기준선보다 느려지지 않음)이고, 이 목표가 방향(jcode보다 더 빠르고 더 작음)이다.

- **측정**은 개발자 장비가 아니라 GitHub Actions의 `compare` 스위트로만 돌린다.
  `gh workflow run benchmark.yml --ref <branch> -f os=all -f suites=compare -f competitors=jcode -f oneshot_runs=5 -f baseline_ref=dev`.
  같은 잡이 후보 옆에서 `dev` 기준선(`horon-base`)을 함께 빌드하므로 변경 전·후와 jcode가 같은 러너를 쓴다.
- **비교** 항목은 OS(Linux, macOS, Windows)별로 단발 실행 시간과 최고 RSS, 스킬·MCP 호출 시간, `--version`, 설치 크기, 첫 세션과 추가 세션별 메모리(유휴 및 턴 이후), Linux fsync 횟수다. 최소 5회 실행의 중앙값을 쓰며, 최솟값∼최댓값 구간이 겹치는 차이는 승리가 아니라 동률이다.
- **코드를 바꾸기 전에 원인을 찾는다.** 추측하지 말고 `request_offsets_ms`, `log_tail`, `thread_names`, `live_heap_bytes`로 실행을 나눠 본다(`packages/docs-site/ko/reference/performance/benchmark.mdx`).
- **속도를 위해 내구성이나 정확성을 팔지 않는다.** 원본(이벤트 로그, blob, 로컬 상태, 런타임 저널) flush를 빼거나, 검증을 건너뛰거나, 기본값을 약화하거나, 벤치마크를 감지하는 분기를 넣거나, horon에 유리하도록 픽스처·모의를 바꾸지 않는다. flush는 충돌 시험이 손실 없음을 증명한 때에만 뺄 수 있다(선례는 카탈로그 dirty 마커, `packages/docs-site/ko/reference/protocol/session-store.mdx`). 그 이유로 jcode가 앞서는 셀은 이유를 적어 구조적 차이로 보고한다.

## 설계 원칙

`PHILOSOPHY.md`(한국어: `PHILOSOPHY.ko.md`)의 요약이다. 계약 우선순위는 이 파일이 지키며, 위 고정 요구사항은 바뀌지 않는다.

원칙은 다음과 같다.

1. 모든 셀에서 jcode를 이기고 기록된 기준선 아래로 내려가지 않는다.
2. ACP v1과 v2를 지원한다.
3. 헤드리스 모드와 TUI 모드는 한 엔진 위의 동등한 두 모드다. 에이전트 바이너리에는 터미널 UI가 없고, TUI 모드는 SDK 위의 별도 `horon-tui` 패키지와 바이너리로서 에이전트와 같은 태그에서 릴리스된다.
4. 서버 코드에서 Rust, TypeScript, Python SDK를 통해 엔진을 쓸 수 있다.
5. 주장은 CI에서 측정하며 속도를 위해 내구성을 팔지 않는다.

제안된 변경이 맞는지 보는 기준은 다음과 같다.

1. 요청당 토큰이나 턴당 CPU, fsync, 메모리를 줄인다.
2. CI `compare`나 구조 게이트로 측정할 수 있다.
3. `horon-agent`의 의존성 그래프나 바이너리를 키우지 않는다.
4. ACP v1/v2, SDK, 저장된 세션을 깨뜨리지 않는다.
5. 지운 표면을 다른 이름으로 되살리지 않는다.

사용자 설치 Rust 패키지 구현에는 3번 그래프 제한에 한 가지 예외가 적용된다. 공용 데이터 계약 크레이트 `extension-types`(`crates/extension-types`, 패키지 `horon-agent-extension-types`)는 기존 워크스페이스 의존성 집합만 가져다 쓰며 외부 런타임 의존성을 새로 더하지 않고, 작성자용 `horon-agent-extension-sdk`(`crates/extension-sdk`)와 `horon-package-example`(`tests/fixtures/package-example`)은 `horon-agent` 바이너리 그래프 밖에 남는다. 패키지가 없을 때의 성능과 바이너리 크기 기준선을 지키고, 구조 게이트의 오버헤드 0 `packages` 상한을 바이너리 크기 검사와 함께 유지하면서 표면을 확장하고, ACP v1/v2, SDK, TUI 의존성 경계, 저장된 세션 호환성을 지킨다.

하지 않는 일은 강제 도구 호출 단계, 스트리밍 dry-run, `report_issue` 도구, 도구별 `family@rev` 버전, KDL 모델 카탈로그, `anyhow`나 `SmallVec` 전면 교체, 외부 계약(ACP v1, harness API v1, 저장된 세션)의 컷오버다. 이유와 출처는 `PHILOSOPHY.ko.md`에 있다.

## 이 빌드가 무엇인가

- 두 바이너리. 에이전트 바이너리 `horon-agent`에는 다음 하위 명령이 동작한다. `serve`(에이전트 데몬), `run`(메시지 하나를 보내고 종료), `repl`(전체 화면 UI 없는 일반 줄 프롬프트), `api-bridge`(별칭 `api`; 로컬 소켓, 즉 유닉스 소켓이나 Windows 네임드 파이프 위의 harness API와 `--api-bind`로 여는 선택적 인증 TCP 리스너), `api-credentials`(그 리스너가 받는 베어러 토큰 발급, 회전, 폐기, 조회), `acp`(표준 입출력 ACP 어댑터), `rpc`(표준 입출력 JSONL RPC 어댑터; `--mode rpc`도 같은 어댑터를 고름)와 `update`, `install-release` 같은 유지보수 명령이다. TUI 바이너리 `horon-tui`는 `horon-agent-sdk` 위의 별도 패키지와 바이너리이며, 피처 집합이 `horon-agent`와 합쳐지지 않도록 자체 cargo 호출(`-p horon-tui`)로 빌드한다. 터미널에서 인자 없이 `horon`을 실행하면 서버가 뜬 뒤 형제 바이너리 `horon-tui`로 자기 프로세스를 교체하고, 파이프, `HORON_NO_TUI`, 형제 바이너리 부재 시에는 헤드리스 모드에 머물며 조건을 밝히고 설치 힌트를 출력한다.
- 모델 경로는 하나. 에이전트는 `[provider] default_provider`나 `--provider-profile`이 가리키는 프로파일의 엔드포인트로 POST 요청을 보내며, 프로파일의 `type`이 통신 규약을 고른다. 방식은 넷이다. `openai-compatible`(Chat Completions, `POST {base_url}/chat/completions`, 기본값), `openai-responses`(`POST {base_url}/responses`), `anthropic-compatible`(Anthropic Messages, `POST {base_url}/messages`), `gemini-compatible`(Gemini generateContent, `POST {base_url}/models/{model}:streamGenerateContent?alt=sse`). 이미 해당 경로로 끝나는 `base_url`은 그대로 쓰고, `open-router`는 런타임에 네 값을 나열하는 메시지로 거부한다. 프로파일은 `type`, `base_url`, `api_key_env` 또는 `env_file`, `auth`, `default_model`, 선택적 `[[providers.<name>.models]]`를 담는 `[providers.<name>]` 테이블이다. `auth`를 비우면 타입 기본값을 적용한다. 두 OpenAI 계열은 `Authorization: Bearer`, Anthropic은 `x-api-key`, Gemini는 `x-goog-api-key`이며, 명시된 `auth`가 항상 이긴다. 모델 호출은 하나의 공급자 런타임을 거쳐 엔드포인트에 직접 닿는다. 그 경로에 라우터나 공급자 플러그인을 두지 않는다. 외부 도구는 MCP와 사용자 설치 Rust 패키지(`packages/docs-site/ko/reference/runtime/packages.mdx`)로 들어온다. 활성 프로파일이 `roles.image`를 정해 두면 선택 도구 `generate_image`가 같은 프로파일의 같은 기본 URL, 자격 증명, `extra_body`로 `/images/generations`와 `/images/edits`에 POST한다. 이는 런타임이 아니라 통신 규약 옆의 경로이며, `[tools] generate_image`는 기본값이 `true`이고 프로파일에 `image` 역할이 있을 때만 도구가 등록된다. 따라서 `image` 역할은 모델이 Chat Completions를 말하지 않는 유일한 역할이고, `openai-compatible`이나 `openai-responses` 프로파일만 지원하며, 텍스트 폴백이 없는 유일한 역할이다.
- 사용자 설치 Rust 패키지가 구현됐다(`packages/docs-site/ko/reference/runtime/packages.mdx`). 확장(extension)은 Rust 실행 파일(`extension.toml`)을 선언하고, 플러그인(plugin)은 하나의 `extension.toml`과 선언된 리소스(`plugin.toml`)를 함께 묶는다. 둘 다 `horon package install|list|verify|disable|remove`나 `$HORON_HOME/packages/inbox/<id>/`(실행 중인 데몬이 조정)를 쓴다. 설치는 동의에 해당한다. `PackageStore`는 선언과 네이티브 타깃을 검증하고 `extension.toml`의 SHA-256을 기록하며, 열린 세션의 다음 턴 경계에서 새 카탈로그 세대를 적용하고, 자식 프로세스를 띄우기 전에 그 SHA-256을 다시 검증한다. 실행 파일 호출, 패키지 MCP 표준 입출력 서버, 관찰자(observer)는 모두 기존 도구 정책과 호스팅 정책 게이트(`crates/base/src/packages/child_gate.rs`)를 거쳐 사용자 OS 계정 아래의 별도 자식 프로세스에서 실행된다. 프로세스 내 플러그인 로딩은 계속 금지하고 고정 요구사항을 지킨다.
- 호스팅은 `HORON_AGENT_HOSTING_MODE`(`per-user-container` 또는 `shared-worker`)로 선택 활성화한다. 없으면 운영자 본인의 홈을 뜻하며, 다른 값은 둘을 나열한 시작 오류가 된다. 호스팅 프로세스는 `HORON_AGENT_TENANT_ID`를 검증된 감사 라벨과 디렉터리 이름으로만 쓰고 세션 조회 키로 쓰지 않는다. `per-user-container`는 `HORON_HOME`이 `~/.horon`이 아닌 소유자 전용 디렉터리이고, 런타임 디렉터리가 홈 밖의 절대 경로·소유자 전용이며, TCP 게이트웨이가 꺼져 있고, 테넌트 ID가 유효하지 않으면 시작을 거부한다. `shared-worker`는 시작되지만 `HostingPolicy::allows_shared_children()`이 거짓이다. `per-user-container`는 자체 MCP 표준 입출력 서버와 패키지 자식을 유지하고, `shared-worker`는 단일 게이트(`crates/base/src/mcp/hosted.rs`)로 모든 MCP 표준 입출력 서버, 에디터 제공 서버, 풀 연결, 원격 연결을 거부하며 거부당 `HOSTED_POLICY` 감사 기록 하나를 남긴다. 테넌트가 자신의 설정을 쓸 수 있으므로 `config.toml`의 `[hosting]`은 정책을 좁히기만(`allow_shared_children`, `allowed_mcp_servers`) 할 수 있고, 거기 있는 `mode` 키는 경고 하나와 함께 무시된다(`packages/docs-site/ko/reference/architecture/hosting.mdx`, `packages/docs-site/ko/reference/safety/security.mdx`).
- 핵심(`core`) 도구 프로필은 실행 시 프로필을 고르지 않았을 때의 기본값이며, 이 빌드가 등록하는 내장 도구에 모두 닿는다. 허용 목록은 `CORE_TOOLS`, `CORE_DISCOVERABLE_TOOLS`, `toolbox`이고 `irc`, `checkpoint`, `rewind`는 각자의 스위치로 합류한다. `core` 세션은 열한 개(`read`, `write`, `edit`, `apply_patch`, `bash`, `ls`, `agentgrep`, `todo`, `bg`, `websearch`, `webfetch`)와 `toolbox`, 그리고 `PATH`에 Python 인터프리터가 있으면 `eval`을 즉시 보낸다. 나머지는 시스템 프롬프트에 이름만 적어 모델이 `toolbox`로 꺼내 쓰게 한다(`packages/docs-site/ko/reference/runtime/tools.mdx`). `skill_manage`와 `mcp`는 등록된 상태를 유지한다. SDK로 구동하는 세션은 `enabled` 목록에 이름을 넣지 않으면 `session_search`와 `swarm`을 끄고 둔다. `horon-agent acp`는 기본값의 예외로서, 명시적 프로필이 없으면 `[acp] tool_profile`인 `acp`를 적용한다.

## 저장소 구조와 크레이트 경계

|경로|담당|
|---|---|
|`src/`|`horon-agent` 패키지: 에이전트 루프, 데몬, harness API 브리지, ACP/RPC 어댑터, CLI 분기|
|`crates/`|워크스페이스 크레이트. `crates/tui`가 `horon-tui` 패키지, `crates/agent-sdk`가 Rust SDK|
|`tests/`, `crates/*/tests/`|통합 테스트 타깃|
|`docs/`|에이전트용 문서. 영어로만 쓰고 주제별 평평한 페이지로 둔다. 에이전트 바이너리에 컴파일되어 `horon://<페이지>`와 `horon` 도구로 제공된다|
|`packages/sdk/typescript`, `packages/sdk/python`|TypeScript 및 Python SDK|
|`packages/npm/horon`|`@horon-ai/horon` 런처|
|`packages/docs-site`|agent.horon.ai 사이트(Mintlify 프로젝트)이자 개발자 레퍼런스의 원본. `reference/`(영어), `ko/reference/`(한국어, 같은 경로), `images/reference/`를 직접 고친다. 생성되는 페이지는 `reference/philosophy`, `ko/reference/philosophy`, `reference/sdk/*`뿐이며 `scripts/ci/sync_docs_site.py`가 `PHILOSOPHY*.md`와 SDK·npm README에서 만든다. Mintlify는 `deploy/docs` 브랜치만 배포하고, `main`에서 이 폴더가 바뀌면 `docs-site-deploy.yml`만 그 브랜치를 `main`으로 앞당긴다(`packages/docs-site/README.md`)|
|`scripts/ci`, `scripts/perf`, `scripts/release`, `scripts/install`, `scripts/npm`|게이트, 벤치마크, 릴리스, 설치, 패키징 스크립트|

경계와 각 경계를 강제하는 게이트는 다음과 같다.

- `horon-agent`는 `dev-dependencies`를 포함해 `ratatui`, `crossterm`, `horon-tui`, `horon-agent-tui*` 패키지에 닿지 않는다(`scripts/ci/check_headless_graph.py`).
- `horon-tui`는 클라이언트다. 유일한 워크스페이스 의존성은 `horon-agent-sdk`와 계약 크레이트 `horon-agent-tool-types`뿐이며 엔진 크레이트에 의존하지 않고 자체 엔진을 키우지 않는다(`crates/tui/Cargo.toml`, `packages/docs-site/ko/reference/architecture/crates.mdx`; 리뷰로 확인하며 아직 전용 게이트는 없음).
- `horon-agent-*-types` 크레이트는 데이터 계약이다. 루트 패키지, `horon-agent-core`, 또는 `scripts/ci/check_dependency_boundaries.py`가 나열한 런타임·공급자 크레이트에 의존하지 않는다.
- OS별 `cfg` 줄은 다섯 개 `crates/platform*` 크레이트에 둔다. 그 외 위치의 모든 줄은 파일별로 이유와 함께 `scripts/ci/platform_cfg_allowlist.json`에 기록해 관리한다(`scripts/ci/check_platform_cfg.py`).
- Rust와 TypeScript SDK 표면은 동등성을 유지한다(`cargo test --locked -p horon-agent-sdk parity`).
- 타입이나 헬퍼를 루트 패키지에서 뺄지는 건별 판단이 아니라 `packages/docs-site/ko/reference/architecture/crates.mdx`의 규칙으로 결정한다.

## 기본값을 유지해야 하는 설정

|설정|기본값|다른 값|
|---|---|---|
|도구 프로필(`--tool-profile`, `HORON_AGENT_TOOL_PROFILE`, `[tools] profile`)|`core`|`full`, `acp`, `minimal`/`lite`, `none`|
|`[acp] profile`|`standard`|`extended`, `full`|
|`[tools] redo`, `hashline_edit`|`true`|`false`면 도구가 빠진다|
|`[tools] ast_tools`, `lsp`, `context_notes`, `managed_skills`, `codemode`|`true`|`false`면 도구가 빠진다|
|`[tools] ask_policy`|`recommended`|`error`|
|`[tools] bash_pty`|`auto`|`off`, `always`|
|`[tools] irc`, `checkpoint`|`true`|`false`면 도구가 빠진다. `HORON_AGENT_IRC`가 단일 프로세스에 대해 `irc`를 덮어쓴다|
|`[irc] mailbox_cap`, `max_body_bytes`, `wait_max_secs`, `broadcast_concurrency`|`100`, `65536`, `300`, `16`|1 이상의 값|
|`[tools] eval`|`true`|`false`면 도구가 빠진다. 셀은 `PATH`에 Python 3.10 이상이 있어야 하고 `bash`와 똑같이 승인을 묻는다|
|`[features] crash_bundles`|`true`|`false`|
|`[features] turn_recovery`|`conservative`|`aggressive`, `off`|
|`[features] foreign_rules`, `stream_rules`, `repeat_guard`|`false`|`true`|
|`[features] effort_keywords`|`true`|`false`|
|`[prewalk] enabled`|`false`|`true`. 켜면 설정 검증을 통과하는 `into`(모델 ID 또는 `role:<name>`)가 필요하다|
|`[storage] sync`|`full`|`fast`. `HORON_AGENT_STORAGE_SYNC`가 단일 프로세스에 대해 덮어쓰며, 알 수 없는 값은 경고 한 줄과 함께 `full`로 폴백한다|
|`[features] pricing_refresh`|`false`|`true`. `HORON_AGENT_PRICING_REFRESH=1` 또는 `0`이 단일 프로세스에 대해 덮어쓴다|
|`[server] idle_hibernation_secs`|`1800`|`0`이면 정리를 끈다. `HORON_AGENT_IDLE_HIBERNATION_SECS`가 단일 프로세스에 대해 덮어쓴다|
|`HORON_AGENT_HOSTING_MODE`|미설정(호스팅 아님)|`per-user-container`, `shared-worker`. 다른 값은 시작 오류|
|`HORON_AGENT_TENANT_ID`|미설정|ASCII 소문자·숫자·하이픈 1∼63자(시작과 끝은 영문자나 숫자). 모든 호스티드 모드에서 필수이며 감사 라벨로만 쓴다|
|`[hosting] allow_shared_children`|모드별 기본값(`per-user-container`에서만 참)|`false`는 좁히고 `true`는 넓히지 못한다. `[hosting] mode`는 경고 하나와 함께 무시된다|
|`[hosting] allowed_mcp_servers`|미설정(모든 서버 도달 가능)|호스팅 프로세스용 정확한 MCP 서버 이름 목록. 빈 목록이면 모든 서버를 거부한다. 좁히기만 가능하며 모드를 넓히지 못하고, 비호스팅 프로세스에는 변화 없음|
|`HORON_AGENT_API_BIND`, `[harness_api] bind`, `--api-bind`|미설정(로컬 소켓만)|인증 TCP harness API 리스너의 `IP:PORT`. 플래그가 환경변수를, 둘 다 파일을 이긴다|
|`HORON_AGENT_API_INSECURE_BEHIND_PROXY`, `[harness_api] insecure_behind_proxy`, `--api-insecure-behind-proxy`|`false`|비루프백 바인드 앞단에 TLS 종료 프록시가 있을 때만 `true`. 발급된 자격 증명(`horon-agent api-credentials issue`)도 함께 필요하다|
|`serve`의 업데이트 확인|실행 안 함|`--no-update`가 어디서든 건너뛴다|

`[storage] sync`는 시스템 콜이 아니라 내구성 수준을 뜻한다. `full`은 정전 내구성(macOS `F_FULLFSYNC`)이고 `fast`는 가장 값싼 충돌 내구성 flush(macOS에서는 일반 `fsync`, 그 외 플랫폼에서는 `full`과 같음)다. 원본은 이 수준으로 확정하고 파생 캐시는 다시 빌드하는 것이 `packages/docs-site/ko/reference/architecture/crates.mdx`가 정한 경계다. `pricing_refresh`가 꺼져 있으면 단발 실행은 네트워크를 타지 않고, 디스크에 이미 있는 카탈로그만 읽는다.

`[server] idle_hibernation_secs`는 데몬이 세션의 파생 캐시와 선택적 Python 커널을 해제하기 전 유휴 시간이다. 공급자 투영과 도구 출력 색인은 메모리에 상주한 상태로 디스크 읽기 없이 다시 만든다. 다음 Python 셀은 새 상태에서 시작한다. 해제 대상과 유지 대상은 `packages/docs-site/ko/reference/performance/memory.mdx`에 나열돼 있다. `0`이면 정리를 끄고, 데몬이 10초마다 확인하므로 임계값을 넘긴 뒤 최대 한 틱 안에 해제가 일어난다.

메모리는 `$HORON_HOME/memory` 아래 사용자별 마크다운 위키(메모리당 마크다운 페이지 하나 + 그 페이지에서 재구축하는 SQLite 색인)에 저장하며, 위키 이전 `memory/*.json` 파일은 마이그레이션 원본으로만 쓴다(`packages/docs-site/ko/reference/runtime/memory.mdx`). 프로젝트 메모리는 호출자가 적은 경로 표기가 아니라 정규화된 프로젝트 디렉터리로 키를 매기며, 모든 `memory` 동작과 프롬프트 주입 경로가 이 저장소를 읽고 쓴다.

내장 도구는 기본으로 모두 닿고, 스위치를 `false`로 두거나 `[tools] disabled`, `[tools.load_mode] <이름> = "off"`로 하나씩 뺀다. `irc`는 한 데몬 안의 에이전트 간 메시징이며, 기본값인 `[tools] irc = true`이면 `core` 목록에 추가되고(다른 목록은 이름을 적어야 닿는다) 스위치가 꺼져 있으면 `full`에서도 실행을 거부한다(`packages/docs-site/ko/reference/runtime/irc.mdx`). `full` 아래에서 선택 도구는 `toolbox` 뒤의 지연 정의로 로드된다(`packages/docs-site/ko/reference/runtime/tools.mdx`). `[tools.load_mode]`는 도구 하나의 단계(`essential`, `discoverable`, `off`)를 덮어쓴다. `ask = "essential"`은 목록에 있던 도구를 다시 즉시 전송하고, `toolbox = "off"`는 목록화를 꺼서 닿는 도구를 모두 즉시 보낸다. `acp`, `minimal`, `none` 세션, 명시적 `enabled` 목록을 가진 세션, 그리고 SDK `enabled` 목록을 가진 `core` 세션은 허용하는 모든 도구를 즉시 보내며 색인을 쓰지 않는다. `full`에서 SDK 목록은 비핵심 도구를 계속 지연 로드한다.

두 설정은 환경변수로만 제어한다. `HORON_AGENT_JOBS_ARTIFACT_RETENTION`은 기본값 `rotate`로 예산이 차면 가장 오래된 백그라운드 작업 아티팩트를 회수하고, `preserve`는 대신 새 작업 시작을 거부한다. `HORON_AGENT_WORKSPACE_TRUST`는 단일 프로세스에 대해 `trust`나 `deny`를 받는다. 프로젝트 로컬 MCP 설정은 다이제스트가 신뢰되기 전까지 건너뛰며 `--trust-workspace`가 결정을 기록한다(`packages/docs-site/ko/reference/safety/security.mdx`).

## 빌드, 실행, 검증

### CI 우선 로컬 검증(에이전트 세션에 우선 적용)

이 저장소의 전체 검증은 GitHub Actions가 맡는다. 아래 참고 명령어나 표와 이 하위 절이 충돌하면 아래 규칙이 우선한다.

1. **GitHub CI가 전체 검사를 돌린다.** 워크스페이스 전체 `cargo clippy`, `cargo test`, `cargo doc`, 크로스 OS 검사(`cargo xwin`, `justrust`), 전체 `scripts/ci/check_all.sh`를 로컬에서 실행하지 않는다. PR CI(`scripts/ci/run_warnings.py` 경고 0건)를 확인한다.
2. **범위가 좁은 로컬 검사.** 단일 크레이트 검사(`cargo check --locked -p <crate>`, 표적 테스트 하나)나 비-Cargo 게이트(`actionlint`, `check_docs_links.py`), 그리고 최적화가 로컬 근거를 필요로 할 때 최소 1∼2회의 범위 한정 전후 측정만 실행한다.
3. **`build-lock` 규율.** 무거운 로컬 Cargo 작업은 `~/.local/bin/build-lock`으로 직렬화한다. 세션당 대기자 하나만 둔다(폴링 루프나 중복 `hc wait` 대기자 금지, 오래된 중복 제거). 코어당 부하가 2 미만이면 대기 없이 진행하고, 경합이 높으면 CI로 미룬다.
4. **PR 우선 루프.** 먼저 PR을 열거나 갱신한다. CI가 실패하면 실패한 테스트 하나만 로컬에서 재현한다. 경고 0건으로 녹색일 때만 병합한다.
5. **엄격한 테스트 제한.** 로컬 테스트는 90초 같은 엄격한 타임아웃과 `-- --test-threads=1`로 실행해 멈춘 테스트가 `build-lock`을 붙잡지 않게 한다.
6. **채택일 2026-11-24.** 공유 MacBook의 `build-lock` 대기열이 동시 세션들을 막는 문제 이후 소유자 지시로 채택됐다.

참고 명령과 푸시 전 게이트 표(위 규칙에 따라 CI나 건드린 단일 구성요소에만 선택적으로 실행한다):

```bash
cargo build -p horon-agent
cargo build -p horon-tui
./target/debug/horon-agent --version
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --workspace
```

변경 영역별 CI 게이트와 선택 수동 명령:

|변경|명령|
|---|---|
|모든 변경|`scripts/ci/check_all.sh`: `ci.yml`의 Format 및 Quality Guardrails 단계. `--skip-slow`는 `cargo check`, clippy, `cargo machete`를 생략하고, `--fix`는 포맷팅과 래칫 기준을 다시 맞춘다|
|Rust 코드|위 `cargo` 명령과 건드린 모든 크레이트의 테스트|
|`crates/tui`|`cargo clippy --locked --all-targets --all-features -p horon-tui -- -D warnings`|
|`docs/`, `packages/docs-site/`, README, `PHILOSOPHY*.md`|`python3 scripts/ci/check_docs_links.py`, `python3 scripts/ci/check_docs_pairs.py`, `python3 scripts/ci/sync_docs_site.py --check`, `python3 -m unittest scripts/ci/test_docs_install_claims.py`, `docs/`가 바뀌었으면 `cargo test -p horon-agent-app-core --lib horon_agent_docs`|
|`.github/workflows`|`actionlint`|
|푸시 후|완료된 실행에 `python3 scripts/ci/run_warnings.py <run-id>`(다음 절 참조)|

요구사항 1 검사. 각 `cargo tree` 명령은 "did not match any packages"를 출력해야 하고 그래프 게이트는 종료 코드 0이어야 한다.

```bash
cargo tree -p horon-agent -i ratatui
cargo tree -p horon-agent -i crossterm
cargo tree -p horon-agent -i horon-tui
cargo tree -p horon-agent -i horon-agent-tui
python3 scripts/ci/check_headless_graph.py
```

`cargo tree` 줄들은 그 이름들만 확인하며, `check_headless_graph.py`는 `horon-agent`에서 도달 가능한 모든 패키지를 훑어 `horon-agent-tui*` 접두사까지 실패로 잡는다.

요구사항 2 검사: `cargo test -p horon-agent --lib acp`(`src/cli/acp.rs`의 ACP 단위 테스트). 요구사항 3 검사: `packages/docs-site/ko/reference/performance/benchmark.mdx`의 구조 게이트와 A/B 러너.

## 테스트

- **테스트는 동작을 지킨다.** 테스트는 의도된 동작에 묶인 경우(그 동작이 바뀔 때만 바뀌고, 그 동작이 깨질 때만 실패)에만 유지한다. 과거에 묶인 테스트(제거된 코드가 없는지 검사), 아무것도 검증하지 않는 테스트(표준 라이브러리, 타입 시스템, 테스트 대상의 모의 객체를 검증해 어떤 코드 변경으로도 실패하지 않음), 구현에 묶인 테스트(기댓값이 코드 논리를 반복하거나 비공개 상태·내부 호출 순서·실행 헬퍼를 검사), 또는 같은 코드 경로와 분기를 중복 검증하는 테스트는 쓰지 않고 발견하면 지운다. 그런 테스트가 호출자가 기대는 동작도 덮고 있으면 리터럴 기댓값으로 그 동작을 검증하도록 고쳐 쓰고 이름과 모듈은 유지한다(워크플로와 게이트가 이름으로 테스트를 고르기 때문이다). 해당 테스트를 지울 때는 테스트만을 위해 존재하는 운영 코드(`#[cfg(test)]` 헬퍼, `*_for_tests` 함수)도 지운다. 통신 규약·저장소 계약, 사용자가 보는 거부, 내구성·동시성·보안 테스트, 게이트 계약 테스트, 소스 스캔 규칙 테스트는 동작에 해당한다. 테스트를 지울 때는 해당 테스트를 선택하는 `--min-passed`와 `min_passed` 하한을 실제로 통과로 집계되던 수만큼 낮춘다(`#[ignore]`나 `cfg` 제외 테스트는 원래 제외). 1 아래로는 내리지 않으며 `skipped` 목록에 있던 제거 테스트는 목록에서 뺀다. 변경 사항은 PR에 적는다(#419).
- **CI가 실행해야 테스트로 친다.** 통합 테스트 타깃(`tests/*.rs`, `crates/*/tests/*.rs`)은 `min_passed` 수치와 함께 `scripts/ci/integration_targets.json`에, 실행하지 않는 것은 이유와 함께 `scripts/ci/test_target_allowlist.json`에 등재해야 한다(`scripts/ci/check_test_targets.py`). 라이브러리 테스트 모듈은 `ci.yml`의 코호트 필터가 선택해야 한다(`scripts/ci/check_lib_test_coverage.py`). 컴파일만 되는 테스트는 근거가 아니다.
- **건너뛰기는 적어 둔다.** 통합 타깃에서 빠진 케이스는 해당 타깃의 `skipped` 목록에 `skip_reason`과 함께 적는다.
- **격리.** `HORON_HOME`을 임시 디렉터리로 돌리고 실제 `~/.horon`을 읽거나 쓰지 않는다. 프로세스 환경변수를 바꾸는 테스트는 `crates/storage/src/active_pids.rs`처럼 직렬 실행한다.
- **크기.** 1,200줄을 넘는 테스트 파일은 `scripts/ci/test_size_budget.json`이 추적한다. 추적 중인 파일은 커지면 안 되고 새 초과 파일은 실패한다. 큰 새 스위트는 별도 파일에 둔다.
- **게이트도 테스트한다.** 게이트 스크립트는 계약 테스트(`scripts/ci/test_*.py`)와 함께 두고 CI가 실행한다.
- **성능과 내구성.** 성능 주장은 구조 게이트(카운터, `HORON_AGENT_PERF_COUNTERS=1`)나 Actions 벤치마크로 증명하며 개발자 장비의 시간 측정으로 증명하지 않는다. flush 제거는 "성능 목표"에서 말한 충돌 시험을 요구한다.

## CI 경고: 0건이 완료 조건

변경이 유발한 CI 실행의 어떤 잡에서도 경고가 보이면 작업은 끝나지 않았다. 경고가 남은 녹색 실행은 완료가 아니다. 마지막 커밋 실행에 경고가 하나도 없을 때까지 고치며, 그전에는 병합하거나 완료를 보고하지 않는다.

- **대상.** 변경이 촉발한 모든 워크플로(`CI`, `Structural performance`, `Release Recovery Validation`, ...)의 모든 잡 로그에 나온 모든 경고: rustc/clippy/cargo `warning:`, rustup `warn:`, cargo-audit `Warning:`, 스크립트 `warning:` 줄, `cargo:warning=` 빌드 스크립트 출력, `##[warning]` 또는 `warning` 수준 어노테이션.
- **확인 방법.** 실행이 끝나면 `python3 scripts/ci/run_warnings.py <run-id>`를 돌린다(로그인된 `gh` 필요; ID는 `gh run list --branch <branch>`로 찾음). 잡·단계별 고유 경고를 모두 출력하고 없을 때만 종료 코드 0을 돌려준다. 단순 체크 표시가 아니라 이 출력 결과를 근거로 붙인다.
- **원인을 고친다.** `#[allow]`, `-A`, `--ignore`, 로그 수준 하향, 메시지 이름 변경으로 경고를 숨기지 않는다. 저장소가 고칠 수 없는 외부 원인(유지보수 중단 전이 크레이트, Rust 툴체인 결함)은 가장 좁은 범위에 허용하고 그 옆에 이유를 적으며 보고에 이름을 남긴다. `crates/app-core/src/lib.rs`, `src/lib.rs`, `src/main.rs`의 `linker_messages` 허용은 비최적화 macOS 링크를 다룬다. 의존성 감사는 권고 예외를 두지 않는다.
- **0을 지키는 게이트.** `ci.yml`은 `CARGO_BUILD_WARNINGS=deny`를 설정해 워크스페이스 크레이트의 rustc나 빌드 스크립트 경고를 실패 처리하고, `scripts/ci/check_security.sh`는 `cargo audit --deny warnings`를 실행한다. 따라서 RustSec 데이터베이스의 새 권고가 코드 변경 없이 CI를 빨갛게 만들 수 있으며, 똑같이 분류·대응한다. `notice` 수준 어노테이션(macOS 러너 용량 안내 등)은 GitHub 안내이며 경고가 아니다.

## 변경 유형별 CI 게이트

`ci.yml`과 `structural-perf.yml`은 `Changed Paths` 잡(`scripts/ci/classify_changes.py`, 16개 불리언 레인 출력)으로 시작해 파일별로 변경을 분류한다. PR은 대상 베이스와 비교하고, `dev` 첫 시도 푸시는 같은 워크플로의 최근 성공 `dev` 푸시 실행 헤드와 비교한다(two-dot diff).

- **문서 전용**(`README`, `PHILOSOPHY*.md`, `docs/`, `AGENTS.md`, 템플릿, `packages/docs-site/`; `docs_only=true`): `Documentation Checks`(링크, UTF-8, 영어/한국어 쌍, 설치 주장 표 일관성)와 `Documentation Corpus Test`(`docs/`가 바이너리에 컴파일되므로 `horon_agent_docs` 테스트는 여전히 실행)를 돌리고, 사이트 페이지가 바뀌었으면 `Documentation Site`(`sync_docs_site.py --check`, Mintlify 빌드 및 깨진 링크 검사)도 돌린다. 릴리스 빌드, OS 테스트 매트릭스, SDK 잡, `Quality Guardrails`, 구조 성능 게이트는 건너뛴다. 문서 변경과 그 변경이 다시 만든 사이트 페이지는 풀 리퀘스트에서도, 그 `dev` 푸시에서도 이 레인에 남는다.
- **범위 한정 비-Rust 레인:**
  - `.github/workflows/*`(`workflows=true`, `rust=false`)는 `Workflow Lint`(`actionlint`), `Release Automation`, `PowerShell Syntax`를 돌리고 Rust 빌드를 건너뛴다.
  - 단독 TypeScript SDK, Python SDK, npm 런처, 설치 스크립트 수정(`ts_sdk`, `py_sdk`, `npm_pkg`, `install_scripts`이며 `rust=false`)은 일치하는 레인(`TypeScript SDK`, `Python SDK`, `Setup Friction Eval`, `PowerShell Syntax`, `Release Automation`)과 비-Rust `Quality Guardrails` 검사를 돌리고 `Build & Test`, Windows 크로스 검사, `structural-perf`를 건너뛴다.
- **Rust 변경(`rust=true`):** `Quality Guardrails`, `Build & Test`(PR에서는 `scripts/ci/workspace_impact.py`로 영향받은 크레이트와 통합 타깃만 컴파일·테스트하며 매니페스트, 락파일, 공용 인프라나 `ci:all-os`/`ci:full`이면 전체 워크스페이스로 폴백), `windows-cross-check`(`cargo-xwin`), `windows-arm64-check`(`justrust`), `Release Automation`, `Structural performance gate`(Linux에서 `mold`로 통합 `--no-run` 컴파일)를 돌린다.
- **PR OS 및 soak 범위:** 이식성 Rust PR은 `ubuntu-latest`와 Linux 호스트 Windows 크로스 검사 둘을 `HORON_AGENT_SOAK_CYCLES=20`으로 실행한다. `macos-latest`, 네이티브 `windows-build-test`, `tui_smoke`는 해당하는 플랫폼/TUI 경로, 공용 Linux-macOS `#[path]` 소스, 매니페스트, 락파일 또는 `ci:all-os`/`ci:full` 라벨이 촉발할 때만 PR에서 돈다. 모호한 diff나 조회 실패는 보수적으로 `rust=true`와 `full_matrix=true`로 간주한다.
- **`dev`·`main` 푸시, 수동 실행, 재실행:** 문서 전용이 아닌 `dev` 푸시는 3개 OS 모두, 전체 크레이트·타깃 스위트, `HORON_AGENT_SOAK_CYCLES=200`을 실행한다. 헤드가 이미 `origin/main`에 있는 `dev` 푸시, `main` 푸시, 수동 실행, `dev` 재실행, 그리고 조회·diff 실패는 모두 전체를 실행한다. 릴리스 근거(`scripts/release/check_ci.py`)는 완전히 성공한 푸시 실행만 인정하며, 문서 전용 `dev` 실행은 릴리스 근거가 아니다. 그 실행에서 `Re-run all jobs`를 돌리면 전체 실행이 된다.
- **이미 `dev`에서 검증된 릴리스 풀 리퀘스트:** `main`으로 향하는 풀 리퀘스트의 헤드가 같은 워크플로의 최근 성공 `dev` 푸시 실행 헤드와 같고, 머지 커밋(`GITHUB_SHA`, 체크아웃된 `refs/pull/N/merge`)이 그 헤드와 문서만 다르다면 문서 전용 레인(`docs_only=true`)으로 분류된다. 그 푸시 실행이 같은 코드에서 모든 게이트를 이미 돌렸기 때문이다. `main`의 이력이 아니라 머지 커밋을 비교하므로 `main`이 cherry-pick으로 이미 받은 변경은 세지 않는다. `ci:all-os`/`ci:full` 라벨이 붙거나 다른 헤드이거나 머지가 코드를 더하거나 머지 커밋이 없거나 조회·diff가 실패하면 일반 풀 리퀘스트로 분류한다. `package-compat.yml`과 `package-windows.yml`은 `main` 대상 풀 리퀘스트에서 전혀 돌지 않는다. 두 워크플로의 `dev`와 `main` 푸시 실행이 같은 파일을 다룬다.
- **Rust 캐시는 `main`에서만 저장한다.** `main`이 기본 브랜치이므로 모든 브랜치와 풀 리퀘스트가 `main`의 항목을 복원할 수 있는 반면, `main`은 `dev` 항목을 복원하지 못하며 두 사본을 두면 10 GB 캐시 한도를 넘는다. CI, 구조 성능, 패키지, 소크(soak) 캐시는 첫 시도 `main` 실행에서 저장하고, 벤치마크·로딩 A/B·Windows 스모크 캐시는 실행 ref가 `main`일 때만 저장한다. `tests/test_ci_workflow_guards.py`가 각 `save-if` 조건을 고정한다.
- `CI Required`와 `Structural performance required`는 항상 완료되며 게이트가 실패했거나 취소됐을 때만 실패한다. 건너뛴 게이트 때문에 검사가 대기 상태로 남지 않도록 이 둘을 필수 체크로 사용한다.
- 모든 액션은 `# vX` 주석과 함께 커밋 해시로 고정한다(Dependabot이 최신 유지). `release.yml`, `release-tag.yml`, 배포 워크플로는 이 게이팅 대상이 아니다. `actionlint`는 아무것도 보고하지 않아야 한다(`Workflow Lint`).
- **`main`과 `dev` 밖에서는 후속 실행이 이전 실행을 취소한다.** 그 밖의 모든 ref에서는 같은 ref의 새 실행이 이전 실행을 취소한다. `main`의 푸시 실행은 릴리스 근거이므로 실행 중인 것은 취소하지 않는다. `dev`에서는 새 실행이 실행 중인 것을 기다린다. 취소된 `dev` 실행은 녹색 헤드를 남기지 않아 다음 푸시가 더 오래된 실행과 비교되고 모든 게이트를 다시 돌리기 때문이다. GitHub가 대기 중인 실행을 더 새 실행으로 교체하므로 기다리는 실행은 많아야 하나이고, 그 실행은 자신이 기다린 실행과 비교해 분류된다. `dev`의 푸시와 수동 실행은 같은 그룹을 쓴다. 취소되거나 실패한 `dev` 푸시 실행 뒤에는 그 실행의 `Re-run all jobs`를 사용한다. `dev`를 `main` 커밋으로 fast-forward하지 않는다(동기화는 머지 커밋 PR). 그러면 취소된 `dev` 실행이 릴리스 커밋의 최신 푸시 실행이 돼 `scripts/release/check_ci.py`가 거부한다.

## 작업 규칙: 워크트리, 브랜치, 커밋, 풀 리퀘스트

- **워크트리.** 파일은 `.hyper/worktree/<name>`(`.hyper/`는 무시됨)에서 `dev`로부터 딴 git 워크트리 안에서 작업당 하나씩 바꾼다. 모든 명령과 편집은 그 워크트리를 작업 디렉터리로 삼아 실행한다. 다른 세션의 워크트리를 건드리지 않는다. 병합이 확인되면 워크트리와 브랜치를 지운다.
- **브랜치.** `<type>/<slug>` 형태이며 커밋 타입(`feat/`, `fix/`, `perf/`, `docs/`)을 따른다. 베이스와 PR 대상은 `dev`다. `main`은 아래 릴리스 풀 리퀘스트나 거기 설명한 `main` 문서 최신화로만 바뀐다.
- **커밋.** Conventional Commits(`type(scope): summary`)를 따른다. 에이전트가 작성하는 커밋 끝에는 트레일러 `Co-authored-by: Horon AI Bot <353694987+horon-ai-bot[bot]@users.noreply.github.com>`를 붙인다.
- **풀 리퀘스트.** 이 저장소의 실제 이슈를 연결해야 한다(`require-issue.yml`; PR 번호는 인정되지 않으며 Dependabot만 예외). 본문에는 변경 내용과 근거를 적고, 함께 바꾼 문서나 문서가 다루지 않는 이유 한 줄("코드를 따르는 문서" 참조)을 적는다. 모든 잡이 녹색이고 `run_warnings.py`가 경고를 내지 않을 때 `dev`에 squash-merge한다. 강제 푸시는 절대 하지 않는다.

## 릴리스와 브랜치 동기화: dev를 main으로, 그다음 main을 다시 dev로

`dev`는 작업이 모이는 곳이고, `main`은 배포되는 곳이다. `main`에 병합하면 릴리스가 나가므로 소유자가 요청할 때만 수행한다. 두 브랜치는 머지 커밋과 자동 `docs(perf)` 커밋 때문에 달라지며, 이 때문에 릴리스 뒤에 역방향 동기화가 따른다.

1. **릴리스.** 코드 게이트가 실행된 가장 최근 `dev` CI 실행을 `scripts/ci/run_warnings.py`로 확인한다(경고 0건, 모든 잡 녹색, 미취소). 최신 `dev` 실행이 문서 전용이면 그전 코드 게이트 실행을 확인하거나 최신 실행에서 `Re-run all jobs`를 돌린다. 베이스 `main`, 헤드 `dev`, 제목 `Release: dev into main (vX.Y.Z)`로 PR을 열고 포함된 이슈들을 `Refs`로 적으며 배포 내용을 나열한다. squash가 아니라 반드시 merge commit으로 병합한다. `main` 푸시 CI 실행이 성공하면 `release-tag.yml`이 머지 커밋에 태그를 달고 `release.yml`을 실행하며 `ghcr.io/horon-ai/agent`를 푸시한다. `gh run list --branch main`으로 릴리스 실행이 끝날 때까지 지켜본다.
2. **역방향 동기화.** 릴리스가 끝나면 베이스 `dev`, 헤드 `main`, 제목 `Sync: main into dev`로 PR을 열고 merge commit으로 병합해 `dev`가 `main`의 모든 커밋을 갖게 한다. 여느 PR과 마찬가지로 연결된 이슈가 필요하다.
3. **확인.** `git rev-list --left-right --count origin/dev...origin/main`이 동기화 머지 자체를 빼면 양쪽 누락 0커밋을 보이고, `git diff origin/dev origin/main`이 비어 있어야 한다.

**`main`의 문서 최신화.** 릴리스 사이에 `main`의 문서는 `main`이 배포하는 코드보다 뒤처질 수 있다. 이때는 #327, #328처럼 릴리스 없이 최신화한다. 먼저 `dev`에 문서 최신화를 반영한 뒤 `origin/main`에서 딴 `docs/` 브랜치에 체리픽하고, 바뀐 모든 진술·포인터·다이어그램을 `main` 코드와 대조해 `dev`에만 있는 변경은 `main` 값으로 유지한다. `packages/docs-site/reference/performance/results.mdx`의 자동 생성 블록은 `main`에 있는 그대로 둔다. PR의 베이스는 `main`이고 문서만 포함하며 커밋 제목에 `[skip ci]`를 넣어 squash-merge한다. 그러면 `main` 푸시에서 CI가 돌지 않고 `release-tag.yml`이 태그를 만들지 않는다. 문서 사이트 배포도 돌지 않으므로 `packages/docs-site/`를 건드렸다면 `gh workflow run docs-site-deploy.yml --ref main`을 실행한다. 다음 릴리스와 역방향 동기화가 두 사본을 다시 맞춘다.

## 문서

문서 세트는 읽는 사람이 다른 두 가지다.

|세트|읽는 사람|위치|언어|형태|
|---|---|---|---|---|
|에이전트 문서|Horon 안에서 도는 에이전트와 그 사용자|`docs/<주제>.md`, 평평한 구조. `docs/README.md`가 목차|영어만(바이너리에 들어가는 언어)|경우별: 어떤 필요에 어떤 도구·명령을 쓰는지, 경우마다 무엇이 일어나는지, 기본값과 해결법|
|개발자 레퍼런스|유지보수자|`packages/docs-site/reference/<영역>/<페이지>.mdx`와 `packages/docs-site/ko/reference/<영역>/<페이지>.mdx`|영어·한국어 쌍|설계, 계약, `path:line` 포인터, CI, 벤치마크|

`docs/` 아래 모든 마크다운 파일은 바이너리에 컴파일되어(`crates/app-core/build.rs`) `horon` 도구와 `read horon://<페이지>`로 제공되며, `docs/`가 뿌리다(`docs/tools.md`는 `horon://tools.md`). 네트워크 없이 실행 중인 버전을 설명한다. 페이지는 짧고 사실만 담고, 깊은 내용은 개발자 레퍼런스로 연결한다.

개발자 레퍼런스 페이지와 한국어 짝은 같은 커밋에서 고치고 제목, 표, 코드 펜스, 링크를 같게 유지한다. `python3 scripts/ci/check_docs_pairs.py`가 `README`, `PHILOSOPHY` 쌍과 함께 검사하고, `python3 scripts/ci/check_docs_links.py`가 루트와 `docs/`의 링크를 검사한다. 새 페이지를 만들기 전에 기존 페이지를 먼저 확장하고, 새 페이지는 `packages/docs-site/docs.json`에 넣는다.

`packages/docs-site/reference/diagram/index.mdx`는 `packages/docs-site/images/reference/diagram/`의 손수 작성 SVG 다이어그램(영어 라벨 `NN-name.svg`, 한국어 라벨 `NN-name.ko.svg`)을 보여 준다. 스타일은 `packages/docs-site/images/reference/assets/architecture.svg`를 따른다. 자체 완결이고 스크립트나 외부 참조가 없으며, 밝은 색과 어두운 색은 `prefers-color-scheme`으로 바꾼다. 페이지를 옮기거나 지웠으면 코드, 스크립트, 테스트에 낡은 경로가 남지 않아야 한다.

**문서는 코드를 따른다.** 문서가 설명하는 동작을 바꾸는 변경은 같은 PR 안에서 해당 문서, 한국어 짝, 관련 다이어그램까지 함께 고치고 후속 작업으로 미루지 않는다. 문서가 여전히 예전 동작을 적고 있으면 변경은 끝난 것이 아니다. 해당 페이지를 찾는 방법은 다음과 같다.

1. 바뀐 파일 경로, 심볼, CLI 플래그, 설정 키, 기본값을 모두 검색한다: `rg -n '<path-or-name>' README.md README.ko.md AGENTS.md docs/ packages/docs-site/`. 이름을 적고 있는 페이지가 그 대상을 설명한다.
2. 파일 이름을 직접 적지 않았더라도 동작을 설명하는 페이지를 아래 영역 표에서 찾는다.
3. 찾은 각 문서를 새 코드와 대조해 다시 읽는다(도구·필드·프로필 개수, 프로토콜 마이너 버전, 기본값, `path:line` 포인터, 명령 출력). 현재 시제로 고친다. 변경 이력은 문서가 아니라 PR 본문에 적는다.

|바뀐 영역|확인할 문서|
|---|---|
|`src/cli/` 명령, 플래그, 분기, `src/main.rs`|`README.md`, `packages/docs-site/reference/release/install.mdx`, `packages/docs-site/images/reference/diagram/02-run-modes.svg`|
|`src/cli/tui_handoff.rs`, `crates/tui/`|`packages/docs-site/reference/runtime/tui.mdx`, `packages/docs-site/images/reference/diagram/02-run-modes.svg`|
|`src/cli/acp/`, `src/cli/acp.rs`|`packages/docs-site/reference/protocol/acp.mdx`, `packages/docs-site/images/reference/diagram/05-acp-negotiation.svg`|
|`src/cli/rpc/`|`packages/docs-site/reference/protocol/rpc.mdx`|
|`crates/harness-api*`, `crates/agent-sdk`, `packages/sdk/`, `src/cli/api_credentials.rs`, `crates/base/src/harness_api.rs`, `crates/config-types/src/harness_api.rs`|`packages/docs-site/reference/protocol/harness-api.mdx`|
|`crates/app-core/src/server/`|`packages/docs-site/reference/architecture/server.mdx`, `packages/docs-site/reference/runtime/turns.mdx`, `packages/docs-site/images/reference/diagram/03-turn-lifecycle.svg`|
|`crates/base/src/hosting.rs`, `crates/config-types/src/hosting.rs`, `crates/app-core/src/server/hosted_tenant.rs`, `crates/agent-sdk/src/launch_hosted.rs`, `scripts/hosting/`|`packages/docs-site/reference/architecture/hosting.mdx`|
|`crates/session-store/`, `crates/storage/`, `[storage] sync`|`packages/docs-site/reference/protocol/session-store.mdx`, `packages/docs-site/reference/architecture/crates.mdx`, `packages/docs-site/images/reference/diagram/06-session-storage.svg`|
|`crates/app-core/src/tool/`, `crates/tool-*`, `crates/edit-safety/`, 도구 프로필|`packages/docs-site/reference/runtime/tools.mdx`, `packages/docs-site/reference/runtime/tool-scheduling.mdx`, `packages/docs-site/images/reference/diagram/07-tool-loading.svg`, 위 "이 빌드가 무엇인가"|
|`crates/extension-*`, `crates/base/src/packages/`, `crates/app-core/src/extensions/`, `src/cli/packages.rs`|`packages/docs-site/reference/runtime/packages.mdx`, 위 "이 빌드가 무엇인가"|
|`crates/base/src/memory*`, `crates/memory-types/`, `horon memory migrate`, `horon volume`, `src/cli/volume.rs`|`packages/docs-site/reference/runtime/memory.mdx`, `packages/docs-site/reference/runtime/tools.mdx`, `packages/docs-site/reference/architecture/crates.mdx`, `packages/docs-site/reference/architecture/hosting.mdx`|
|`crates/provider-*`, `crates/base/src/provider*`, `crates/config-types/`|`packages/docs-site/reference/provider/model.mdx`, `packages/docs-site/images/reference/diagram/04-provider-wires.svg`, 위 "이 빌드가 무엇인가"|
|`crates/irc-core/`, `crates/app-core/src/server/irc/`|`packages/docs-site/reference/runtime/irc.mdx`|
|`crates/plan/`, `crates/swarm-core/`|`packages/docs-site/reference/runtime/swarm-task-graph.mdx`|
|승인, 워크스페이스 신뢰, 충돌 번들, `scripts/ci/check_security.sh`|`packages/docs-site/reference/safety/safety.mdx`, `packages/docs-site/reference/safety/security.mdx`|
|`crates/platform*`, `Cargo.toml`에서 추가·제거·이름 변경된 크레이트|`packages/docs-site/reference/architecture/crates.mdx`, 위 "저장소 구조와 크레이트 경계", `packages/docs-site/images/reference/diagram/01-system-overview.svg`|
|`.github/workflows/`, `scripts/ci/classify_changes.py`, `scripts/release/`|위 "변경 유형별 CI 게이트"와 "릴리스와 브랜치 동기화", `packages/docs-site/reference/release/`, `packages/docs-site/images/reference/diagram/08-branch-release-flow.svg`|
|`scripts/perf/`, `benchmark*.yml`, `scripts/perf/structural_budget.json`|`packages/docs-site/reference/performance/benchmark.mdx`, `packages/docs-site/reference/performance/memory.mdx`|
|`packages/npm/`, `publish-*.yml`|`packages/docs-site/reference/release/npm.mdx`, `packages/docs-site/reference/release/install.mdx`|
|"기본값을 유지해야 하는 설정"의 값|그 표와 해당 설정을 설명하는 페이지|

영역마다 `docs/`에 에이전트용 페이지도 있다(`docs/README.md` 목차가 이름을 적는다). 사용자에게 보이는 변경은 그 페이지도 고친다. 같은 규칙이 `README.md`, 이 파일, `packages/docs-site`의 손으로 쓴 가이드에도 적용된다. 문서 수정이 필요 없는 코드 PR은 본문에 그 이유를 적는다.

**용어 규칙.** "모드, 바이너리, 릴리스 단위"의 용어를 따른다. "headless"와 "TUI"는 두 모드를 뜻하며, 바이너리는 에이전트 바이너리와 TUI 바이너리라 부른다. `packages/docs-site/reference/performance/results.mdx`(와 한국어 짝)의 `{/* bench-charts */}`와 `{/* bench-auto */}` 구간은 `benchmark-docs.yml`에서 `scripts/perf/bench_docs.py`가 쓴다. 구간이 아니라 스크립트를 고친다.

## 소유자의 명시적 요청 없이 하지 않을 작업

- **릴리스.** `main` 병합, 태그 푸시, `release.yml`이나 `release-tag.yml` 실행. `main` 병합은 릴리스 배포다.
- **패키지 배포.** npm과 PyPI 경로는 `publish-npm.yml`, `publish-typescript-sdk.yml`, `publish-python-sdk.yml`뿐이다. 기기에서 직접 배포(`npm publish`, 수동 PyPI 업로드)하거나, `NPM_PUBLISH`를 켜거나, `dry_run=false`로 배포 워크플로를 실행하지 않는다.
- **이름 변경.** 코드 식별자, CLI 플래그, 설정 키, 파일 이름, CI 잡 이름은 문구 이유만으로 바꾸지 않는다. ACP v1, harness API v1, 저장된 세션은 외부 계약이다.
- **이력 다시 쓰기.** 강제 푸시 금지, 다른 세션이 소유한 브랜치나 워크트리 삭제 금지.
- **상한 완화.** 래칫의 `--update`와 성능 예산 변경은 의도한 증가가 있을 때만 쓰고, PR에 명시하며, 실패한 게이트를 통과시키기 위한 수단으로 쓰지 않는다.

## 자주 빠지는 함정

- **잘못된 체크아웃.** 메인 체크아웃 기준으로 상대 경로를 쓰면 워크트리가 아니라 메인 체크아웃을 고치게 된다. 매 편집 전에 경로를 확인한다.
- **두 개의 문서 세트.** `docs/`는 에이전트 문서이며 바이너리에 컴파일되는 유일한 세트다. `packages/docs-site/reference/`는 개발자 레퍼런스이고 직접 고친다. 생성되는 페이지는 philosophy와 SDK·npm 페이지뿐이다. `PHILOSOPHY*.md`나 패키지 README를 고치고 `python3 scripts/ci/sync_docs_site.py`를 실행한 뒤 결과를 커밋한다. 동작 변경은 보통 두 세트와 그 동작을 되풀이하는 손으로 쓴 가이드를 함께 건드린다.
- **제목 이름 변경.** 양쪽 언어의 `#anchor` 링크를 깨뜨린다. `check_docs_pairs.py`가 이를 보고한다.
- **낡은 `Cargo.lock`.** `--locked`를 안 쓰는 잡은 통과하고 쓰는 잡에서 실패한다. 로컬에서 `cargo metadata --locked --format-version 1`로 잡는다.
- **버전 문자열.** 같은 릴리스에서도 `horon-agent --version`과 `horon-tui --version`은 서로 다른 값을 출력한다. 문자열이 아니라 릴리스 태그를 비교한다.
- **이름 속 `headless`.** `check_headless_graph.py`, `is_headless`, `spawn_mode = "headless"`는 제품 설명이 아니라 식별자다.
- **GitHub API 예산.** `gh run watch` 기본값은 3초마다 폴링하며 모든 세션이 시간당 한도를 공유한다. `--interval 60`을 쓴다.
- **문서 전용 `dev` 실행.** 아무것도 빌드하지 않고 녹색이 되므로 릴리스 근거가 아니다("변경 유형별 CI 게이트" 참조).
