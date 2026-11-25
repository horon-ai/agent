# 보안

언어: [English](SECURITY.md) | **한국어**

워크스페이스 신뢰 게이트, 호스팅 모드의 MCP·확장 격리, 로컬 크래시 번들, 의존성 보안 분류를 다룬다.

## 워크스페이스 신뢰

프로젝트 저장소는 에이전트가 실행할 MCP 서버를 지정할 수 있다. 그래서 horon agent는 워크스페이스의 프로젝트 로컬 설정을 그 내용에 대한 동의 없이는 읽지 않는다. 이 문서는 현재 트리의 게이트 범위, 다이제스트 규칙, 클라이언트가 없으면 거부하는 규칙, 재정의를 설명한다.

### 무엇을 게이트하는가

게이트 대상은 워크스페이스에 있는 세 파일이다(`crates/base/src/workspace_trust.rs:22` `PROJECT_CONFIG_SURFACES`).

- `.horon/mcp.json`
- `.mcp.json`
- `.claude/mcp.json`

사용자 수준 설정은 게이트하지 않는다. `~/.horon/mcp.json`, `~/.claude.json`, `~/.claude/mcp.json`은 사용자 자신의 설정이지 저장소가 정한 것이 아니다. 이 가운데 Claude Code 파일은 `[mcp] claude_user`가 켜져 있을 때만 읽고, `~/.codex/config.toml`은 `[mcp] codex_user`가 켜져 있을 때만 읽는다. `~/.claude.json`의 프로젝트별 항목은 세션의 프로젝트 디렉터리와 맞춰 고르므로 한 저장소의 항목이 다른 저장소로 새지 않는다(`crates/base/src/mcp/sources.rs:1-14`).

강제 지점은 하나다. `McpConfig`가 프로젝트 로컬 설정을 읽을 때 `load_authorized_project_locals`가 `authorize_project_config`를 부르고, 허용된 스냅샷의 바이트만 파싱한다(`crates/base/src/mcp/protocol.rs:706`, `workspace_trust.rs:267`). 세 파일이 모두 없으면 게이트할 것이 없으므로 `Absent`이고, 거부되면 경고 한 줄을 남기고 프로젝트 설정을 싣지 않는다.

### 다이제스트 규칙

스냅샷은 고정 순서로 세 표면을 읽고 SHA-256을 계산한다(`workspace_trust.rs:162` `snapshot`).

1. 도메인 구분자 `horon-agent/workspace-trust/v1`로 시작한다.
2. 표면마다 `경로`, 상태, 내용 지문을 넣는다.
3. 상태는 `absent`, `present`, `unreadable` 셋이다. `present`는 바이트 길이와 그 파일의 SHA-256을 함께 넣는다.

동의는 정규화한 워크스페이스 경로와 이 다이제스트의 쌍으로 기록한다. 기록 파일은 에이전트 디렉터리의 `workspace-trust.json`이고(`workspace_trust.rs:439` `trust_file_path`), 워크스페이스에는 아무것도 쓰지 않는다. 항목은 버전 1, 다이제스트, 결정, 기록 시각, 표면별 SHA-256과 길이를 담는다.

- 세 파일 중 하나라도 바이트가 바뀌면 다이제스트가 달라지고 그 워크스페이스는 다시 신뢰를 묻는다. 파일을 새로 만들거나 지워도 마찬가지다.
- 거부 이유는 바뀐 표면의 이름을 최대한 짚어 준다(`workspace_trust.rs:313` `untrusted_reason`).
- 기록은 파일 전체를 읽고 고쳐 쓰는 방식이라 두 프로세스가 같은 순간에 쓰면 한쪽 결정이 사라질 수 있다. 잃는 것은 결정 하나이고, 없던 허용이 생기지는 않는다(`workspace_trust.rs:231` `record`).
- 다이제스트는 저장소를 신뢰할 수 있게 만드는 장치가 아니다. 사용자가 본 내용에 동의를 묶는 장치다.

### 클라이언트가 없으면 거부

붙어 있는 클라이언트가 없으면 물어볼 상대가 없다. 그래서 모르는 상태는 허용이 아니라 거부다.

- 현재 다이제스트에 대한 기록이 없으면 `Skipped`다. 경고를 남기고 프로젝트 로컬 설정을 싣지 않으며, 아무 결정도 기록하지 않는다. 다음 실행에서 다시 묻는다.
- 기록은 있지만 다이제스트가 다르면 `Skipped`다. 바뀐 표면을 경고에 적는다.
- 표면이 일반 파일로 읽히지 않으면 재정의와 무관하게 `Skipped`다(`workspace_trust.rs:267`).
- 기록된 결정이 거부이면 `Skipped`다.
- 세 파일이 모두 없으면 `Absent`이고 게이트가 작동하지 않는다.

거부는 실패가 아니라 폐쇄다. 세션은 계속 돌고, 프로젝트가 지정한 MCP 서버만 빠진다.

`[tools] bash_direnv`를 켜면 세션 작업 폴더 안의 direnv 파일을 불러오기 전에 Horon이 신뢰 결정을 확인한다. 현재 상태에 맞는 저장 결정, `--trust-workspace` 또는 `HORON_AGENT_WORKSPACE_TRUST=trust`로 direnv 검사를 허용한다. `HORON_AGENT_WORKSPACE_TRUST=deny`가 우선하며 direnv도 실행 전에 파일을 별도로 승인해야 한다. Horon은 direnv의 출력과 환경 변수 값을 진단 메시지에 넣지 않는다.

### 재정의

| 수단 | 범위 | 기록 |
| --- | --- | --- |
| `--trust-workspace` | 이 실행 | 현재 다이제스트에 신뢰를 기록한다. |
| `HORON_AGENT_WORKSPACE_TRUST=trust` | 이 프로세스 | 기록하지 않는다. |
| `HORON_AGENT_WORKSPACE_TRUST=deny` | 이 프로세스 | 기록하지 않고, 저장된 신뢰가 있어도 거부한다. |
| `mcp test`, `mcp list` 같은 명시적 명령 | 그 명령 | 기록하지 않는다. 명령 자체가 동의다. |

환경 변수는 대소문자를 가리지 않고 `trust`와 `deny`만 알아본다. 다른 값은 무시되고 저장된 결정으로 돌아간다(`workspace_trust.rs:372` `env_decision`). 환경 변수의 거부는 `--trust-workspace`보다 먼저 적용되므로, 둘을 함께 주면 거부가 이긴다.

## 호스팅 모드의 MCP·확장 격리

호스팅 배포는 고객 한 명에게 프로세스 하나를 준다. 컨테이너이거나 그 고객의 볼륨을 홈으로 마운트한 Fly Machine이다. 데몬은 [호스팅](../architecture/HOSTING.ko.md) 문서의 모드 중 하나로 실행된다. 모드는 그 고객을 위해 어떤 MCP 서버와 자식 프로세스를 실행할 수 있는지 정하며, 판단은 한 모듈(`crates/base/src/mcp/hosted.rs`)이 한다.

| 모드 | MCP stdio 자식 | 패키지·확장 자식 | 편집기가 준 stdio 서버 | 공유 풀 stdio 연결 | 원격 MCP 연결 |
| --- | --- | --- | --- | --- | --- |
| 호스팅 없음(일반 프로세스) | 허용 | 허용 | 허용 | 허용 | 허용 |
| `per-user-container` | 허용 | 허용 | 허용 | 허용 | 허용 |
| `per-user-container`에 `[hosting] allow_shared_children = false` | 거부 | 거부 | 거부 | 거부 | 허용 |
| `shared-worker` | 거부 | 거부 | 거부 | 거부 | 거부 |

- 공유 워커는 다섯 가지를 모두 거부하고 `horon mcp list`도 `HOSTED_POLICY`(종료 코드 `1`)로 거부한다. 자식 프로세스와 원격 자격 증명 저장소가 한 고객의 것이 되는데 프로세스는 모든 고객을 상대하므로, 사용자별 환경·프로세스 트리·토큰을 증명할 방법이 없다.
- 사용자별 컨테이너 또는 Fly Machine은 자기 서버를 그대로 쓴다. 홈 볼륨, 그 안의 OAuth 저장소, `${VAR}` 확장이 읽는 프로세스 환경이 그 고객의 것이다. `allow_shared_children = false`는 자식 프로세스만 닫고 원격 연결은 그대로 두는데, 원격 연결은 자식을 만들지 않기 때문이다.
- `[hosting] allowed_mcp_servers`는 호스팅 프로세스가 연결할 수 있는 MCP 서버를 좁힌다. 값이 없으면 모두 허용하고, 빈 목록이면 하나도 허용하지 않으며, 이름은 소스나 편집기가 등록한 이름과 정확히 일치해야 한다. 모드를 넓히지 못하고, 호스팅이 아닌 프로세스는 그대로이므로 재사용한 볼륨에 남은 키가 운영자의 서버를 막지 않는다.
- 패키지 자식은 하나의 게이트(`crates/base/src/packages/child_gate.rs`, `PackageExecutable::start_with_arguments`에서 호출)를 지난다. 따라서 패키지의 MCP stdio 서버, 확장 자식, 선언한 명령, 옵서버가 모두 같은 판단을 받는다.

거부는 명시적이고 한 번 기록된다. 호출자는 정책과 동작 이름이 담긴 타입 오류를 받고, 데몬은 감사 이벤트를 한 줄 남긴다.

**`HOSTED_POLICY`** 필드는 `policy`(`shared-worker`, `per-user-container`, 또는 `unresolved`), `operation`(`mcp-stdio`, `session-mcp-stdio`, `pooled-mcp-stdio`, `mcp-remote`, `package-child`), `subject`(서버 또는 패키지 이름, 게이트가 모르면 `-`), `reason`(`shared-worker`, `children-disabled`, `not-allowed`, `unresolved-policy`), `tenant`(감사 라벨, 호스팅 밖이면 `-`)다. 명령 인자, 환경 값, 헤더, URL, 토큰은 절대 담지 않는다.

거부된 패키지 MCP 서버는 세션의 다음 턴에도 전달되므로, 모델이 도구가 왜 없는지 사용자에게 알릴 수 있다. 편집기가 준 서버 목록이 거부되면 목록 전체를 조용히 줄이지 않고 요청 오류 자체를 돌려준다.

한계: 이 게이트는 MCP 서버와 패키지 자식만 다룬다. `bash`, `eval`, 훅 명령은 대상이 아니며, 호스팅 배포에서는 이 게이트가 아니라 [호스팅](../architecture/HOSTING.ko.md) 문서의 사용자별 경계(컨테이너 또는 Fly Machine, 전용 `HORON_HOME`, 분리한 런타임 디렉터리, TCP 게이트웨이 없음)가 이들을 막는다.

## Python eval

Python 셀은 코드와 작업 폴더를 기준으로 명시 승인을 받는다. 폴더에 준 권한으로 셀을 승인할 수 없다. 커널은 선택한 환경 변수만 물려받는다. 승인된 Python은 사용자에게 허용된 파일, 네트워크와 프로세스에 접근하므로 사용자가 읽을 수 있는 자격 증명 파일도 열 수 있다. 기능 설정과 셀 승인이 실행 시작 여부를 정한다.

`tool.<name>`은 자식 프로세스의 파이프로 현재 세션의 도구 레지스트리에 요청을 보낸다. 호론은 활성 셀과 요청 번호를 검증하고 세션 허용 목록, 로드 단계, 개별 도구 승인과 훅을 적용한다. 이 정책은 도구 브리지에 적용된다. Python이 파일이나 프로세스에 직접 접근할 때는 호론을 실행한 사용자의 운영체제 권한을 따릅니다.

## 크래시 번들

프로세스가 패닉하면 horon agent는 로컬 진단 번들을 남긴다. 번들은 디스크에만 쓰고 어디로도 보내지 않는다. 이 문서는 현재 트리의 위치, 내용, 리댁션, 전송하지 않는다는 보장을 설명한다.

### 위치

번들은 에이전트 디렉터리 아래 `crashes/`에 쓴다(`crates/base/src/crash.rs:180` `crash_root`). 에이전트 디렉터리는 기본값이 `~/.horon`이고 `HORON_HOME`으로 바꿀 수 있다.

한 번들은 디렉터리 하나다.

```text
<agent dir>/crashes/<UTC 시각>-p<pid>/
  bundle.json
  report.txt
```

시각 형식은 `YYYYMMDDTHHMMSS.mmmZ`이고 PID가 붙는다(`crash.rs:217` `bundle_stamp`). 같은 이름이 이미 있으면 `-1`, `-2`처럼 번호를 붙여 새 디렉터리를 잡는다(`crash.rs:225` `create_bundle_dir`).

### 내용

`bundle.json`은 스키마 1 문서다.

| 필드 | 내용 |
| --- | --- |
| `schema` | 번들 형식 버전. 현재 1이다. |
| `captured_at` | 번들을 만든 UTC 시각과 PID. |
| `pid` | 프로세스 ID. |
| `panic.message` | 패닉 페이로드 문자열. |
| `panic.location` | 파일, 줄, 열. |
| `panic.thread` | 패닉이 난 스레드 이름 또는 ID. |
| `operations` | 마지막 도구 디스패치 최대 64건. 오래된 것부터. |

`operations`는 프로세스 전역 링 버퍼다(`crash.rs:26` `OPERATION_RING_CAPACITY`). 각 항목은 증가하는 `seq`, 도구 이름, 리댁션과 자르기를 거친 입력 텍스트를 담는다(`crash.rs:61` `OperationRecord`). 기능이 꺼져 있으면 디스패치 기록 자체를 하지 않는다.

`report.txt`는 같은 내용을 사람이 읽기 좋게 편다. 첫 줄은 `horon-agent crash bundle (local only; nothing was transmitted)`다(`crash.rs:264` `render_report`).

### 리댁션

번들에 들어가는 모든 문자열은 기존 비밀 값 제거 패스인 `redact_secrets`를 거친다(`crates/base/src/message.rs:45`).

- 패닉 메시지, 위치, 스레드 이름, 도구 이름, 도구 입력 JSON에 같은 패스를 적용한다.
- 필드마다 2048자에서 자르고 `[truncated]`를 붙인다. 도구 이름은 128자다(`crash.rs:318` `truncate_chars`).
- 도구 입력은 공격자가 영향을 줄 수 있는 텍스트로 취급한다. 번들에는 불활성 데이터로만 쓰고, 다시 읽어 실행하거나 해석하지 않는다.

리댁션은 알려진 비밀 모양을 지우는 방어이고 완전한 보장이 아니다. 번들은 민감한 자료로 다루는 편이 맞다.

### 전송하지 않음

번들은 `std::fs::write`로 로컬 디렉터리에만 쓴다. 네트워크 호출, 업로드, 텔레메트리 전송이 없다. `report.txt` 첫 줄도 그 사실을 적는다.

### 켜고 끄기

`[features] crash_bundles`의 기본값은 `true`다(`crates/config-types/src/feature.rs:54`). `false`로 두면 패닉 훅이 번들을 쓰지 않고 도구 디스패치도 기록하지 않는다.

훅은 시작할 때 한 번 설치된다(`src/cli/startup.rs:143`). 이후 호출은 활성 플래그만 갱신한다(`crash.rs:86` `install_panic_hook`). 그래서 설정을 다시 읽어 기능을 켜거나 꺼도 이미 설치된 훅은 그 플래그를 따릅니다.

### 한계

- 쓰기는 최선 노력이다. 디렉터리 생성이나 파일 쓰기가 실패해도 패닉 처리 자체는 계속된다.
- 스택 트레이스, 환경 변수 전체, 세션 트랜스크립트는 담지 않는다. 패닉 위치와 최근 도구 호출만 담는다.
- 번들은 로컬에 남으므로 사용자가 직접 정리해야 한다.

## API 노출과 자격 증명

하네스 API는 기본적으로 로컬에 머문다. 소유 OS 사용자만 열 수 있는 Unix
소켓이나 네임드 파이프, 또는 stdio 연결 하나다. `--api-bind`가 TCP 리스너를
추가하며, 그 리스너가 다른 기기에서 접근할 수 있는 유일한 하네스 표면이다.

- 모든 TCP 연결은 `hello` 앞에 `Authorization: Bearer <token>`을 보낸다.
  실패하면 프레임을 쓰지 않고 데몬에도 접속하지 않은 채 연결을 닫으므로,
  인증되지 않은 피어의 요청은 절대 처리되지 않는다. 인증 전 입력은 1 KiB와
  10초로 제한되어, 조용하거나 느리게 흘리는 피어가 연결이나 메모리를 붙잡을 수
  없다.
- 토큰은 32바이트 난수이고 `$HORON_HOME/harness-api/credentials.json`에
  `sha256:<hex>` 다이제스트로만 저장된다. 이 파일은 `0700` 디렉터리 안의
  `0600` 파일이다. 평문은 `horon-agent api-credentials issue`나 `rotate`가 한
  번만 출력한다. 브리지는 연결마다 파일을 다시 읽으므로 `revoke`와 `rotate`가
  즉시 적용되며, 심볼릭 링크이거나 소유자가 다르거나 소유자 전용 권한이 아닌
  저장소는 신뢰하지 않고 거부한다.
- 이 빌드에는 내장 TLS가 없다. 그래서 루프백이 아닌 바인드는 운영자가
  `--api-insecure-behind-proxy`나 `insecure_behind_proxy = true`로 TLS 종료
  프록시를 확인하고 자격 증명이 하나 이상 있을 때만 시작한다. 그 프록시가
  없으면 베어러 토큰이 평문으로 이동한다. 프록시에서의 클라이언트 인증서가
  지원되는 상호 인증 경로다.
- 자격 증명은 `HORON_HOME` 단위다. 사용자별 컨테이너의 홈 볼륨은 그 테넌트의
  것이므로, 한 테넌트의 토큰으로 다른 테넌트의 리스너를 인증할 수 없다.
- 게이트웨이(`[gateway]`, WebSocket 페어링 표면)와 하네스 API 리스너는 서로
  다른 표면이고 저장소도 따로 쓴다. 한쪽 토큰을 다른 쪽은 받아들이지 않는다.
- 노출이 인증하는 것은 사람이 아니라 자격 증명이다. 호스팅 프로바이더의 사설
  네트워크(Fly.io의 `6PN`)는 다른 조직을 막지만 그 위의 모든 프로세스를 신뢰하고,
  공개 포트는 공개다. 두 경로 모두 토큰을 요구하며, 전달 헤더(`Fly-Client-IP`나
  그 밖의 무엇이든)를 인증이나 권한 부여에 신뢰하지 않는다.
- 바인드 주소는 리터럴 IPv4 또는 IPv6 주소로 해석한다. `[::]`, `0.0.0.0`,
  사설 `fdaa::/16` 주소는 모두 루프백이 아니고 모두 프록시 확인을 요구하며,
  `127.0.0.0/8`과 `::1`만 루프백이다.

## 의존성 보안 분류

> **상태:** 날짜가 박힌 기록
> **측정일:** 2026-05-14
> **읽는 법:** 이 문서는 2026-05-14에 찍은 `cargo audit` 스냅숏이며 현재 상태가 아니다. 아래 표의 의존성 경로와 분류는 그날 트리를 본 결과다. 지금 값을 확인하려면 `scripts/ci/check_security.sh`를 직접 돌린다.

이 파일은 horon agent의 2026-05-14 `cargo audit` 결과와 그때 의도한 해결 경로를 기록한다.
허용 목록이 아니다. 보안 권고가 드러나고 조치 가능하도록 남기는 분류 기록이다.

이 기록은 두 번의 제거 이전 트리를 기준으로 작성됐다. 먼저 TUI 크레이트가 빠졌고, 이어서 벤더
프로바이더 런타임 크레이트가 빠졌다. 그래서 `ratatui`·`syntect` 경로를 적은 행과 프로바이더
런타임을 거치던 행은 과거 기록으로 읽는다. 남은 프로바이더 관련 크레이트는 `provider-core`,
`provider-env`, `provider-metadata`, `provider-openai` 넷이며 `provider-openai-runtime`은
2026-10-29에 제거됐다(`ls crates/`). TUI는 그 뒤 별도 패키지 `horon-tui`(`crates/tui`)로
돌아왔다. 그래서 `ratatui`와 `crossterm`은 다시 `Cargo.lock`에 있지만 TUI 바이너리에서만 닿고
`horon-agent`에서는 닿지 않는다(`scripts/ci/check_headless_graph.py`). `syntect`는 돌아오지 않았다.

### 현재 권고

| 권고 | 크레이트 | 의존성 경로 | horon agent에서 영향받는 영역 | 분류 | 계획한 조치 |
|---|---|---|---|---|---|
| `RUSTSEC-2025-0141` | `bincode` | `syntect -> bincode` | TUI의 Markdown/코드 하이라이팅 | 유지보수가 끊긴 전이 의존성. 프로바이더/인증 흐름에 직접 노출되지 않는다. | `syntect` 업그레이드를 추적하고, 업스트림이 `bincode`에서 곧 벗어나지 않으면 `syntect`를 교체한다. |
| `RUSTSEC-2024-0436` | `paste` | `ratatui -> paste`, `tokenizers -> paste`, `tract-* -> paste` | TUI 렌더링, 토크나이저, 임베딩/모델 지원 | 전이 의존성이 널리 퍼져 있다. 한 모듈에 국한되지 않는다. | 로컬 우회보다 업스트림 의존성 업그레이드를 먼저 한다. `ratatui`, `tokenizers`, `tract-*`를 올린 뒤 다시 평가한다. |
| `RUSTSEC-2026-0002` | `lru` | `ratatui -> lru` | TUI 렌더링/캐시 내부 | UI 의존성의 불건전성 경고. 인증/프로바이더 로직은 아니지만 프로세스에 함께 실린다. | 호환되는 시점에 `ratatui` / `ratatui-image`를 함께 올린다. |
| `RUSTSEC-2026-0097` | `rand` | `azure_core`, `tungstenite`, `tract-*`, `ratatui-image` 등 | Azure 인증, 웹소켓, 임베딩, UI 전이 경로 | `rand::rng()`를 쓰는 사용자 정의 로거와 관련된 불건전성 경고. HoronAgent는 그 패턴을 의도적으로 쓰지 않지만 그래프에서 이 크레이트가 넓게 퍼져 있다. | `rand` 0.9 호환 의존성 스택으로 업스트림 업그레이드를 우선한다. |
| `RUSTSEC-2026-0098` | `rustls-webpki` | `rustls` 의존성 스택 | rustls 사용처의 TLS 인증서 검증 | URI 이름에 대한 이름 제약을 잘못 받아들인다. TLS 라이브러리를 거친 전이 문제다. | 호환 릴리스가 나오면 rustls/webpki 스택을 올린다. |
| `RUSTSEC-2026-0099` | `rustls-webpki` | `rustls` 의존성 스택 | rustls 사용처의 TLS 인증서 검증 | 와일드카드 인증서에 이름 제약을 받아들인다. TLS 라이브러리를 거친 전이 문제다. | 호환 릴리스가 나오면 rustls/webpki 스택을 올린다. |
| `RUSTSEC-2026-0104` | `rustls-webpki` | `rustls` 의존성 스택 | TLS 인증서 폐기 목록 파싱 | CRL 파싱에서 도달 가능한 패닉이 있다. TLS 라이브러리를 거친 전이 문제다. | 호환 릴리스가 나오면 rustls/webpki 스택을 올린다. |
| `RUSTSEC-2026-0049` | `rustls-webpki` | `rustls` 의존성 스택(`aws-smithy` rustls 0.21) | TLS 인증서 폐기 목록 처리 | 잘못된 매칭 로직 때문에 Distribution Point가 지정한 CRL을 권위 있는 것으로 보지 않는다. 더 오래된 rustls 스택을 거친 전이 문제이며, 수정에는 rustls-webpki >=0.103.10이 필요하고 이는 `aws-sdk` 스택의 메이저 업그레이드를 요구한다. | 호환 릴리스가 나오면 rustls/webpki 스택을 올린다. |
| `RUSTSEC-2026-0187` | `lopdf` | `horon-agent-pdf -> pdf-extract 0.8.2 -> lopdf 0.34` | PDF 텍스트 추출(`/pdf`, 이미지/PDF 읽기) | 깊게 중첩된 PDF 객체를 파싱할 때 스택이 넘친다. 사용자가 여는(악의적일 수 있는) PDF에서 텍스트를 추출할 때만 도달하며 인증/프로바이더/네트워크 경로는 아니다. `pdf-extract 0.8.2`가 `lopdf 0.34`를 고정하므로 업스트림 `pdf-extract` 릴리스 없이는 수정된 `>=0.42`로 올릴 수 없다. | `pdf-extract`가 `lopdf >=0.42`에 의존하는 릴리스를 내면 올리고 그때 무시를 뺀다. |

### 우선순위

1. rustls 스택을 거치는 `rustls-webpki` TLS 권고
2. `ratatui`를 거치는 `lru` (TUI가 에이전트 바이너리에서 분리되기 전 기록. 지금은 `horon-tui`만 `ratatui`에 닿고 `horon-agent`는 닿지 않는다)
3. `syntect`를 거치는 `bincode`
4. 여러 전이 의존성을 거치는 `paste` / `rand`

### 지금 잠금 파일과의 차이

이 절은 위 표를 다시 측정한 값이 아니라, 같은 크레이트가 오늘 `Cargo.lock`에 있는지만 센 결과다.
`grep -c '^name = "<crate>"' Cargo.lock`으로 확인했고, 권고가 해결됐다는 뜻이 아니다.

| 크레이트 | `Cargo.lock`에 있는가 | 관련 기록 |
|---|---|---|
| `ratatui` | 있다. `horon-tui`(`crates/tui`)로만 들어오고 `scripts/ci/check_headless_graph.py`가 `horon-agent`에 들어오지 못하게 막는다 | `lru` 행, TUI 경로 |
| `syntect` | 없다 | `bincode` 행 |
| `bincode` | 없다 | `RUSTSEC-2025-0141` |
| `lru` | 있다. `ratatui-core`를 거친다 | `RUSTSEC-2026-0002` |
| `quick-xml` | 없다 | `RUSTSEC-2026-0194`, `RUSTSEC-2026-0195` |
| `paste` | 없다 | `RUSTSEC-2024-0436` |
| `rand` | 있다 | `RUSTSEC-2026-0097` |
| `azure_core` | 없다 | `RUSTSEC-2026-0097` |
| `tungstenite` | 있다 | `RUSTSEC-2026-0097` |
| `tract-nnef` | 있다 | `RUSTSEC-2026-0097`, 아래 `RUSTSEC-2026-0217` 항목 |
| `lopdf` | 있다 | `RUSTSEC-2026-0187` |
| `chrono` | 없다 | `RUSTSEC-2026-0543` |
| `rustls-webpki` | 있다 | `RUSTSEC-2026-0049`, `0098`, `0099`, `0104` |
| `crossterm` | 있다. `horon-tui`로만 들어온다 | 터미널 이미지 크레이트(`horon-agent-terminal-image`) 삭제와 함께 빠졌다가 `crates/tui`와 함께 돌아왔다 |

### 참고

- 위 권고 중 프로바이더 인증 리팩터링이 만든 것은 없다.
- 프로바이더/인증 강화 작업은 이 의존성 업그레이드와 별개로 계속해야 한다.
- `RUSTSEC-2026-0217`(`tract-nnef` 0.21.10, NNEF 텐서 파서의 정수 오버플로)은
  2026-07-30에 `horon-agent-embedding`을 `tract` 0.23으로 옮겨 해결했다.
  인라인 `0.21.16` 수정은 도달할 수 없었다. `tract-data 0.21.16`이
  `half ^2.5`를 고정하기 때문이다. 0.23 계열은 그 고정을 없앤다. 이 문제가
  중요했던 이유는 파서가 바이너리에 실린 모델이 아니라 런타임에 내려받은 모델을
  대상으로 돌기 때문에, 무시로 넘기는 것이 분명히 안전하지 않았기 때문이다.
- `RUSTSEC-2024-0320`(`yaml-rust`)은 2026-03-05에 `syntect` 기능을 YAML 로딩
  대신 내장 구문/테마 덤프로 줄여 의존성 그래프에서 제거했다.
- `RUSTSEC-2026-0194` / `RUSTSEC-2026-0195`(`quick-xml` 0.39.2)는 데스크톱
  크레이트의 winit 스택에 있는 빌드 시점 proc-macro인 `wayland-scanner`를 통해서만
  도달한다. 컴파일 중에 신뢰하는 벤더링된 Wayland 프로토콜 XML을 파싱하고 런타임에
  신뢰할 수 없는 입력을 건드리지 않는다. 해결은 업스트림 몫이다.
  `wayland-scanner`가 `quick-xml >= 0.41`로 옮겨가야 한다. 2026-07-04에 분류해
  `scripts/ci/check_security.sh`에서 무시했다.
- 2026-11-09에 CI 경고를 0으로 맞추면서 `cargo audit`가 경고로만 보고하던 항목을 정리했다.
  `anyhow`(RUSTSEC-2026-0190), `event-listener`(RUSTSEC-2026-0221), `tokio`(RUSTSEC-2026-0386)의
  불건전성 권고와 `chacha20` yanked 경고는 `Cargo.lock`을 올려 해결했다
  (`anyhow` 1.0.104, `event-listener` 5.4.2, `tokio` 1.53.3, `chacha20` 0.10.2).
  당시에는 유지보수가 중단된 `paste`, `pin-utils`, `rustybuzz`, `ttf-parser`,
  `unsafe-libyaml` 다섯 개를 검사에서 제외했다.
- 현재 잠금 파일에는 다섯 패키지가 모두 없다. `futures` 0.3.34와 `hyper` 1.12에서
  `pin-utils` 의존성이 사라졌다. 스킬 파일의 YAML 머리말은 `yaml_serde`로 읽고,
  `resvg`/`usvg` 0.48과 `lopdf` 0.45에서는 이전 글꼴 파서를 사용하지 않는다.
  호환되는 배포판이 나올 때까지 PDF, 토크나이저, 할당기는 각 프로젝트의 커밋으로
  고정한다. 보안 검사에서는 모든 권고에 `cargo audit --deny warnings`를 적용한다.
- 2026-11-13: 같은 날 공개된 권고 세 건이 의존성을 바꾸지 않은 push에서도 preflight를 실패시켰다.
  `RUSTSEC-2026-0524`, `RUSTSEC-2026-0523`(`quinn-proto` 0.11.18)과 `RUSTSEC-2026-0538`
  (`quinn-udp` 0.5.14)이고 모두 `reqwest`의 QUIC 스택을 통해 들어온다. 무시하지 않고
  `Cargo.lock`에서 고쳤다(`quinn-proto` 0.11.19, `quinn-udp` 0.5.16). 그래서 `audit_ignores`
  항목은 추가하지 않았다. `main`도 같은 버전을 들고 있었으므로 이 실패는 `dev`에 한정된 것이 아니다.
- 2026-11-17: `RUSTSEC-2026-0543`(`chrono` 0.4.42, 유지보수 중단, 모든 버전 영향, 패치 버전 없음,
  같은 날 발행)이 의존성을 바꾸지 않은 PR에서도 preflight를 실패시켰다. 같은 `dev` 커밋이 09:38에는
  통과하고 14:14에는 실패했다. 그사이 권고 데이터베이스에 항목이 추가됐다. #377에서 시간 라이브러리를 교체하고 임시 검사 예외를 삭제했다.
- 의존성 버전을 바꾸기 전에 다음을 실행한다.
  - `cargo check`
  - `cargo test -j 1`
  - `scripts/ci/check_security.sh`

2026-10-29: `crates/provider-openai-runtime`와 8317 기본 base는 이후 제거되었다.

2026-11-04: 네이티브 플러그인 모델 경로도 제거되어 이제 모델 요청은 `[providers.<name>]` 프로파일의 `type`이 고른 방식(`openai-compatible`, `openai-responses`, `anthropic-compatible`, `gemini-compatible`)으로 해당 엔드포인트에 직접 나간다.

확장은 Rust 실행파일을 선언하고 플러그인은 그 확장에 부속 파일을 더한다. `horon package install <directory>`는 두 패키지 유형을 검증해 사용자 카탈로그에 등록한다. `--project <root>`를 명시하면 정규화한 프로젝트 루트 하나로 설치 범위를 제한한다. 설치는 동의로 간주한다. 데몬은 사용자가 관리하는 `$HORON_HOME/packages/inbox/<id>/`의 완성 폴더를 같은 설치 경로로 처리한다. 원본 폴더는 그대로 두고 제거 이력을 포함해 카탈로그에 이미 기록된 ID는 건너뛴다. 데몬이 실행 중이면 CLI는 데몬에 변경을 요청한다. 요청을 보낸 뒤 응답이 끊기면 오류를 반환한다. 열려 있는 세션은 다음 턴 경계에서 갱신된 카탈로그 세대를 가져온다.

SHA-256은 내부 무결성 검사에 쓴다. 버전을 교섭하는 JSON-RPC 초기화에서 호출 전에 자식이 보고한 선언 해시를 대조한다. 도구 호출과 실행형 명령은 기존 도구 및 호스팅 정책 게이트(`crates/base/src/packages/child_gate.rs`)를 따른다. 자식 프로세스에는 사용자 OS 권한이 있다. 호스트 검사는 자식의 직접 파일 및 네트워크 접근을 제한할 수 없으므로 신뢰하는 패키지만 설치한다.
