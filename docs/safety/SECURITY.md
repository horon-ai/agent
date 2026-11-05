# 보안

워크스페이스 신뢰 게이트, 로컬 크래시 번들, 의존성 보안 분류를 다룬다.

## 워크스페이스 신뢰

프로젝트 저장소는 에이전트가 실행할 MCP 서버를 지정할 수 있습니다. 그래서 horon agent는 워크스페이스의 프로젝트 로컬 설정을 그 내용에 대한 동의 없이는 읽지 않습니다. 이 문서는 현재 트리의 게이트 범위, 다이제스트 규칙, headless 실패 폐쇄, 재정의를 설명합니다.

### 무엇을 게이트하는가

게이트 대상은 워크스페이스에 있는 세 파일입니다 (`crates/base/src/workspace_trust.rs:22` `PROJECT_CONFIG_SURFACES`).

- `.horon-agent/mcp.json`
- `.mcp.json`
- `.claude/mcp.json`

사용자 수준 설정은 게이트하지 않습니다. `~/.horon-agent/mcp.json`, `~/.claude.json`, `~/.claude/mcp.json`은 사용자 자신의 설정이지 저장소가 정한 것이 아닙니다.

강제 지점은 하나입니다. `McpConfig`가 프로젝트 로컬 설정을 읽을 때 `load_authorized_project_locals`가 `authorize_project_config`를 부르고, 허용된 스냅샷의 바이트만 파싱합니다 (`crates/base/src/mcp/protocol.rs:649`, `workspace_trust.rs:258`). 세 파일이 모두 없으면 게이트할 것이 없으므로 `Absent`이고, 거부되면 경고 한 줄을 남기고 프로젝트 설정을 싣지 않습니다.

### 다이제스트 규칙

스냅샷은 고정 순서로 세 표면을 읽고 SHA-256을 계산합니다 (`workspace_trust.rs:162` `snapshot`).

1. 도메인 구분자 `horon-agent/workspace-trust/v1`로 시작합니다.
2. 표면마다 `경로`, 상태, 내용 지문을 넣습니다.
3. 상태는 `absent`, `present`, `unreadable` 셋입니다. `present`는 바이트 길이와 그 파일의 SHA-256을 함께 넣습니다.

동의는 정규화한 워크스페이스 경로와 이 다이제스트의 쌍으로 기록합니다. 기록 파일은 에이전트 디렉터리의 `workspace-trust.json`이고 (`workspace_trust.rs:408` `trust_file_path`), 워크스페이스에는 아무것도 쓰지 않습니다. 항목은 버전 1, 다이제스트, 결정, 기록 시각, 표면별 SHA-256과 길이를 담습니다.

- 세 파일 중 하나라도 바이트가 바뀌면 다이제스트가 달라지고 그 워크스페이스는 다시 신뢰를 묻습니다. 파일을 새로 만들거나 지워도 마찬가지입니다.
- 거부 이유는 바뀐 표면의 이름을 최대한 짚어 줍니다 (`workspace_trust.rs:304` `untrusted_reason`).
- 기록은 파일 전체를 읽고 고쳐 쓰는 방식이라 두 프로세스가 같은 순간에 쓰면 한쪽 결정이 사라질 수 있습니다. 잃는 것은 결정 하나이고, 없던 허용이 생기지는 않습니다 (`workspace_trust.rs:222` `record`).
- 다이제스트는 저장소를 신뢰할 수 있게 만드는 장치가 아닙니다. 사용자가 본 내용에 동의를 묶는 장치입니다.

### headless 실패 폐쇄

headless 경로에는 질문을 받을 클라이언트가 없습니다. 그래서 모르는 상태는 허용이 아니라 거부입니다.

- 현재 다이제스트에 대한 기록이 없으면 `Skipped`입니다. 경고를 남기고 프로젝트 로컬 설정을 싣지 않으며, 아무 결정도 기록하지 않습니다. 다음 실행에서 다시 묻습니다.
- 기록은 있지만 다이제스트가 다르면 `Skipped`입니다. 바뀐 표면을 경고에 적습니다.
- 표면이 일반 파일로 읽히지 않으면 재정의와 무관하게 `Skipped`입니다 (`workspace_trust.rs:258`).
- 기록된 결정이 거부이면 `Skipped`입니다.
- 세 파일이 모두 없으면 `Absent`이고 게이트가 작동하지 않습니다.

거부는 실패가 아니라 폐쇄입니다. 세션은 계속 돌고, 프로젝트가 지정한 MCP 서버만 빠집니다.

### 재정의

| 수단 | 범위 | 기록 |
| --- | --- | --- |
| `--trust-workspace` | 이 실행 | 현재 다이제스트에 신뢰를 기록합니다. |
| `HORON_AGENT_WORKSPACE_TRUST=trust` | 이 프로세스 | 기록하지 않습니다. |
| `HORON_AGENT_WORKSPACE_TRUST=deny` | 이 프로세스 | 기록하지 않고, 저장된 신뢰가 있어도 거부합니다. |
| `mcp test`, `mcp list` 같은 명시적 명령 | 그 명령 | 기록하지 않습니다. 명령 자체가 동의입니다. |

환경 변수는 대소문자를 가리지 않고 `trust`와 `deny`만 알아봅니다. 다른 값은 무시되고 저장된 결정으로 돌아갑니다 (`workspace_trust.rs:363` `env_decision`). 환경 변수의 거부는 `--trust-workspace`보다 먼저 적용되므로, 둘을 함께 주면 거부가 이깁니다.

## 크래시 번들

프로세스가 패닉하면 horon agent는 로컬 진단 번들을 남깁니다. 번들은 디스크에만 쓰고 어디로도 보내지 않습니다. 이 문서는 현재 트리의 위치, 내용, 리댁션, 전송하지 않는다는 보장을 설명합니다.

### 위치

번들은 에이전트 디렉터리 아래 `crashes/`에 씁니다 (`crates/base/src/crash.rs:121` `crash_root`). 에이전트 디렉터리는 기본값이 `~/.horon-agent`이고 `HORON_AGENT_HOME`으로 바꿀 수 있습니다.

한 번들은 디렉터리 하나입니다.

```
<agent dir>/crashes/<UTC 시각>-p<pid>/
  bundle.json
  report.txt
```

시각 형식은 `YYYYMMDDTHHMMSS.mmmZ`이고 PID가 붙습니다 (`crash.rs:158` `bundle_stamp`). 같은 이름이 이미 있으면 `-1`, `-2`처럼 번호를 붙여 새 디렉터리를 잡습니다 (`crash.rs:166` `create_bundle_dir`).

### 내용

`bundle.json`은 스키마 1 문서입니다.

| 필드 | 내용 |
| --- | --- |
| `schema` | 번들 형식 버전. 현재 1입니다. |
| `captured_at` | 번들을 만든 UTC 시각과 PID. |
| `pid` | 프로세스 ID. |
| `panic.message` | 패닉 페이로드 문자열. |
| `panic.location` | 파일, 줄, 열. |
| `panic.thread` | 패닉이 난 스레드 이름 또는 ID. |
| `operations` | 마지막 도구 디스패치 최대 64건. 오래된 것부터. |

`operations`는 프로세스 전역 링 버퍼입니다 (`crash.rs:25` `OPERATION_RING_CAPACITY`). 각 항목은 증가하는 `seq`, 도구 이름, 입력 JSON 직렬화 문자열을 담습니다. 기능이 꺼져 있으면 디스패치 기록 자체를 하지 않습니다.

`report.txt`는 같은 내용을 사람이 읽기 좋게 편다. 첫 줄은 `horon-agent crash bundle (local only; nothing was transmitted)`입니다 (`crash.rs:205` `render_report`).

### 리댁션

번들에 들어가는 모든 문자열은 기존 비밀 값 제거 패스인 `redact_secrets`를 거칩니다 (`crates/base/src/message.rs:51`).

- 패닉 메시지, 위치, 스레드 이름, 도구 이름, 도구 입력 JSON에 같은 패스를 적용합니다.
- 필드마다 2048자에서 자르고 `[truncated]`를 붙입니다. 도구 이름은 128자입니다 (`crash.rs:241` `truncate_chars`).
- 도구 입력은 공격자가 영향을 줄 수 있는 텍스트로 취급합니다. 번들에는 불활성 데이터로만 쓰고, 다시 읽어 실행하거나 해석하지 않습니다.

리댁션은 알려진 비밀 모양을 지우는 방어이고 완전한 보장이 아닙니다. 번들은 민감한 자료로 다루는 편이 맞습니다.

### 전송하지 않음

번들은 `std::fs::write`로 로컬 디렉터리에만 씁니다. 네트워크 호출, 업로드, 텔레메트리 전송이 없습니다. `report.txt` 첫 줄도 그 사실을 적습니다.

### 켜고 끄기

`[features] crash_bundles`의 기본값은 `true`입니다 (`crates/config-types/src/lib.rs:1348`). `false`로 두면 패닉 훅이 번들을 쓰지 않고 도구 디스패치도 기록하지 않습니다.

훅은 시작할 때 한 번 설치됩니다 (`src/cli/startup.rs:52`). 이후 호출은 활성 플래그만 갱신합니다 (`crash.rs:67` `install_panic_hook`). 그래서 설정을 다시 읽어 기능을 켜거나 꺼도 이미 설치된 훅은 그 플래그를 따릅니다.

### 한계

- 쓰기는 최선 노력입니다. 디렉터리 생성이나 파일 쓰기가 실패해도 패닉 처리 자체는 계속됩니다.
- 스택 트레이스, 환경 변수 전체, 세션 트랜스크립트는 담지 않습니다. 패닉 위치와 최근 도구 호출만 담습니다.
- 번들은 로컬에 남으므로 사용자가 직접 정리해야 합니다.

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
2026-10-29에 제거됐다(`ls crates/`).

### 현재 권고

| 권고 | 크레이트 | 의존성 경로 | horon agent에서 영향받는 영역 | 분류 | 계획한 조치 |
|---|---|---|---|---|---|
| `RUSTSEC-2025-0141` | `bincode` | `syntect -> bincode` | TUI의 Markdown/코드 하이라이팅 | 유지보수가 끊긴 전이 의존성. 프로바이더/인증 흐름에 직접 노출되지 않는다. | `syntect` 업그레이드를 추적하고, 업스트림이 `bincode`에서 곧 벗어나지 않으면 `syntect`를 교체한다. |
| `RUSTSEC-2024-0436` | `paste` | `ratatui -> paste`, `tokenizers -> paste`, `tract-* -> paste` | TUI 렌더링, 토크나이저, 임베딩/모델 지원 | 전이 의존성이 널리 퍼져 있다. 한 모듈에 국한되지 않는다. | 로컬 우회보다 업스트림 의존성 업그레이드를 먼저 한다. `ratatui`, `tokenizers`, `tract-*`를 올린 뒤 다시 평가한다. |
| `RUSTSEC-2026-0002` | `lru` | `ratatui -> lru` | TUI 렌더링/캐시 내부 | UI 의존성의 불건전성 경고. 인증/프로바이더 로직은 아니지만 프로세스에 함께 실린다. | 호환되는 시점에 `ratatui` / `ratatui-image`를 함께 올린다. |
| `RUSTSEC-2026-0097` | `rand` | `azure_core`, `tungstenite`, `tract-*`, `ratatui-image` 등 | Azure 인증, 웹소켓, 임베딩, UI 전이 경로 | `rand::rng()`를 쓰는 사용자 정의 로거와 관련된 불건전성 경고. HoronAgent는 그 패턴을 의도적으로 쓰지 않지만 그래프에서 이 크레이트가 넓게 퍼져 있다. | `rand` 0.9 호환 의존성 스택으로 업스트림 업그레이드를 우선한다. |
| `RUSTSEC-2026-0141` | `lettre` | `horon-agent-notify-email -> lettre` | 알림 이메일 발송 | 취약점이 Boring TLS 백엔드의 호스트 이름 검증 경로에 적용된다. HoronAgent의 `lettre` 의존성은 rustls/native-tls 기능을 쓰고 `boring-tls`를 쓰지 않으므로 현재 빌드에서 악용 가능하다고 보지 않는다. | `scripts/ci/check_security.sh`에서 무시를 유지한다. `lettre`가 패치 릴리스하거나 기능 사용이 바뀌면 무시를 뺀다. |
| `RUSTSEC-2026-0098` | `rustls-webpki` | `rustls` 의존성 스택 | rustls 사용처의 TLS 인증서 검증 | URI 이름에 대한 이름 제약을 잘못 받아들인다. TLS 라이브러리를 거친 전이 문제다. | 호환 릴리스가 나오면 rustls/webpki 스택을 올린다. |
| `RUSTSEC-2026-0099` | `rustls-webpki` | `rustls` 의존성 스택 | rustls 사용처의 TLS 인증서 검증 | 와일드카드 인증서에 이름 제약을 받아들인다. TLS 라이브러리를 거친 전이 문제다. | 호환 릴리스가 나오면 rustls/webpki 스택을 올린다. |
| `RUSTSEC-2026-0104` | `rustls-webpki` | `rustls` 의존성 스택 | TLS 인증서 폐기 목록 파싱 | CRL 파싱에서 도달 가능한 패닉이 있다. TLS 라이브러리를 거친 전이 문제다. | 호환 릴리스가 나오면 rustls/webpki 스택을 올린다. |
| `RUSTSEC-2026-0049` | `rustls-webpki` | `rustls` 의존성 스택(`aws-smithy` rustls 0.21, `imap`/`rustls-connector` rustls 0.22) | TLS 인증서 폐기 목록 처리 | 잘못된 매칭 로직 때문에 Distribution Point가 지정한 CRL을 권위 있는 것으로 보지 않는다. 더 오래된 rustls 스택을 거친 전이 문제이며, 수정에는 rustls-webpki >=0.103.10이 필요하고 이는 `aws-sdk`/`imap` 스택의 메이저 업그레이드를 요구한다. | 호환 릴리스가 나오면 rustls/webpki 스택을 올린다. |
| `RUSTSEC-2026-0187` | `lopdf` | `horon-agent-pdf -> pdf-extract 0.8.2 -> lopdf 0.34` | PDF 텍스트 추출(`/pdf`, 이미지/PDF 읽기) | 깊게 중첩된 PDF 객체를 파싱할 때 스택이 넘친다. 사용자가 여는(악의적일 수 있는) PDF에서 텍스트를 추출할 때만 도달하며 인증/프로바이더/네트워크 경로는 아니다. `pdf-extract 0.8.2`가 `lopdf 0.34`를 고정하므로 업스트림 `pdf-extract` 릴리스 없이는 수정된 `>=0.42`로 올릴 수 없다. | `pdf-extract`가 `lopdf >=0.42`에 의존하는 릴리스를 내면 올리고 그때 무시를 뺀다. |
| `RUSTSEC-2023-0086` | `lexical-core` | `imap -> imap-proto -> lexical-core` | Gmail/IMAP 지원 경로 | 메일 스택의 오래된 불건전 전이 의존성. 네트워크에서 파싱한 데이터를 다루므로 UI 전용 발견보다 우선순위가 높다. | `imap` / `imap-proto` 업그레이드나 교체를 조사한다. 유지되는 경로가 없으면 IMAP 의존성을 격리하거나 제거한다. |

### 우선순위

1. rustls 스택을 거치는 `rustls-webpki` TLS 권고
2. `imap-proto`를 거치는 `lexical-core`
3. HoronAgent가 `boring-tls`를 켜게 되면 `lettre`
4. `ratatui`를 거치는 `lru` (headless 전환 이전 기록. 지금 트리에는 `ratatui` 가 없다)
5. `syntect`를 거치는 `bincode`
6. 여러 전이 의존성을 거치는 `paste` / `rand`

### 지금 잠금 파일과의 차이

이 절은 위 표를 다시 측정한 값이 아니라, 같은 크레이트가 오늘 `Cargo.lock`에 있는지만 센 결과다.
`grep -c '^name = "<crate>"' Cargo.lock`으로 확인했고, 권고가 해결됐다는 뜻이 아니다.

| 크레이트 | `Cargo.lock`에 있는가 | 관련 기록 |
|---|---|---|
| `ratatui` | 없다 | `lru` 행, TUI 경로 |
| `syntect` | 없다 | `bincode` 행 |
| `bincode` | 없다 | `RUSTSEC-2025-0141` |
| `lru` | 없다 | `RUSTSEC-2026-0002` |
| `quick-xml` | 없다 | `RUSTSEC-2026-0194`, `RUSTSEC-2026-0195` |
| `lexical-core` | 없다 | `RUSTSEC-2023-0086`의 전이 경로 |
| `paste` | 있다 | `RUSTSEC-2024-0436` |
| `rand` | 있다 | `RUSTSEC-2026-0097` |
| `imap` | 있다 | `RUSTSEC-2026-0049`, `RUSTSEC-2023-0086` |
| `azure_core` | 있다 | `RUSTSEC-2026-0097` |
| `tungstenite` | 있다 | `RUSTSEC-2026-0097` |
| `tract-nnef` | 있다 | `RUSTSEC-2026-0097`, 아래 `RUSTSEC-2026-0217` 항목 |
| `lettre` | 있다 | `RUSTSEC-2026-0141` |
| `lopdf` | 있다 | `RUSTSEC-2026-0187` |
| `rustls-webpki` | 있다 | `RUSTSEC-2026-0049`, `0098`, `0099`, `0104` |
| `crossterm` | 있다 | `horon-agent-terminal-image`의 선택 기능 `display`로 남아 있다(`crates/terminal-image/Cargo.toml:14`, `:20`) |

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
- `scripts/ci/check_security.sh`는 위에서 명시적으로 분류한 취약점 권고
  (`lettre`와 `rustls-webpki`)를 무시해 CI가 계속 조치 가능하게 유지한다
  (`scripts/ci/check_security.sh:98-112`). 새
  취약점은 기본적으로 여전히 CI를 실패시킨다.
- 의존성 버전을 바꾸기 전에 다음을 실행한다.
  - `cargo check`
  - `cargo test -j 1`
  - `scripts/ci/check_security.sh`

2026-10-29: `crates/provider-openai-runtime`와 8317 기본 base는 이후 제거되었다.

2026-11-04: 네이티브 플러그인 모델 경로도 제거되어 모델 요청은 `[providers.<name>]`의 OpenAI 호환 엔드포인트로 직접 나간다.
