# horon agent 문서

언어: [English](README.md) | **한국어**

이 폴더는 horon agent의 현재 동작만 설명한다. 문서는 같은 폴더의 영어 `X.md`와 한국어 `X.ko.md` 한 쌍으로 두고, 영어가 기본이며 한국어 파일은 `.ko.md` 형제 파일이다. 경로·명령·식별자는 원형을 유지한다. 파일명은 모두 대문자로 쓰고, 주제별 하위 폴더에 둔다.

문서는 빌드할 때 바이너리에 들어가고(`crates/app-core/build.rs`가 하위 폴더까지 읽으며 `.ko.md`는 건너뛴다) `horon_agent_docs` 도구가 읽는다. 문서를 옮기거나 지우면 코드·스크립트·테스트에 남은 경로도 같은 변경에서 고친다. 같은 문서를 [agent.horon.ai](https://agent.horon.ai)에도 게시하며 `packages/docs`에서 빌드한다. 영어 파일을 고치면 바이너리와 사이트에 함께 반영된다.

## 고정 요구사항

모든 변경은 아래 세 가지를 지킨다. 정본은 저장소 루트의 `AGENTS.md`다.

| 요구사항 | 내용 | 현재 상태 |
| --- | --- | --- |
| `horon-agent` TUI 제외 | 헤드리스 실행 파일(`serve`, `run`, `api-bridge`, `acp`, `rpc`)은 화면을 열지 않고, 의존성 그래프에 `ratatui`, `crossterm`, `horon-tui`, `horon-agent-tui*` 크레이트가 없어야 한다. 터미널 클라이언트는 `horon-agent-sdk`를 쓰는 별도 `horon-tui` 패키지이고, 터미널에서 인자 없이 실행한 `horon`은 그 형제 실행 파일로 자기 프로세스를 넘긴다. | 충족. `scripts/ci/check_headless_graph.py`가 해석된 그래프를 따라 확인한다 |
| ACP v1·v2 지원 | `horon acp`가 `protocolVersion` 1과 2를 모두 처리하고, 양쪽이 지원하는 가장 높은 버전으로 답한다. v2를 더하면서 v1을 깨지 않는다. | 충족. v2 협상은 `src/cli/acp/v2/negotiate.rs`, v1은 `src/cli/acp/util.rs` |
| 성능 회귀 방지 | 같은 기계·빌드 프로필·작업에서 기록된 기준선과 같거나 빨라야 한다. 느려지는 변경은 병합하지 않는다. | 측정 방법과 기록된 수치가 있음(`performance/BENCHMARK.md`, `performance/RESULTS.md`). 기록된 수치가 기준선이다 |

## 폴더 구조

| 폴더 | 문서 | 내용 |
| --- | --- | --- |
| `docs/` | [`README.ko.md`](README.ko.md) | 이 색인과 갱신 규칙 |
| `architecture/` | [`SERVER.ko.md`](architecture/SERVER.ko.md) | 서버·세션·소켓 구조 |
| | [`CRATES.ko.md`](architecture/CRATES.ko.md) | 크레이트 소유권 경계, 플랫폼 어댑터, 저장 내구성과 TLS 신뢰 집합 |
| `protocol/` | [`ACP.ko.md`](protocol/ACP.ko.md) | ACP v1·v2 어댑터, 에디터 연결과 stdio 계약 |
| | [`HARNESS_API.ko.md`](protocol/HARNESS_API.ko.md) | 하네스 API 자격 증명 설정과 요청·이벤트 표면 |
| | [`SESSION_STORE.ko.md`](protocol/SESSION_STORE.ko.md) | 세션 이벤트 저장 형식, 전송 계약, writer 풀, 영속 런타임 저널 |
| | [`RPC.ko.md`](protocol/RPC.ko.md) | stdio JSONL RPC 모드: 명령, 프레임, omp 대조 표 |
| `runtime/` | [`TOOLS.ko.md`](runtime/TOOLS.ko.md) | 도구 로딩, 효과와 승인 등급, 편집 안전 장치, 상위 `AGENTS.md`, `skill://`·`memory://` 읽기와 MCP 설정 출처, 보조 호출 |
| | [`TOOL_SCHEDULING.ko.md`](runtime/TOOL_SCHEDULING.ko.md) | 도구 스케줄링과 반복 방지 |
| | [`TURNS.ko.md`](runtime/TURNS.ko.md) | 턴 복구, 실행 중 메시지 삽입, `[prewalk]` 계획→실행 모델 인계, 목표 계속과 토큰 예산, 런타임 명령, `--resume` |
| | [`IRC.ko.md`](runtime/IRC.ko.md) | 에이전트 간 IRC 메일박스 버스, `irc` 도구, 설정, 성능 게이트 |
| | [`TUI.ko.md`](runtime/TUI.ko.md) | `horon-tui` 터미널 클라이언트: 인자 없는 `horon`의 핸드오프, 사용자 파일(`tui.toml`, 테마, 키 배치, 명령 파일, 상태 줄), 키, 명령, 설정 화면, 선택 자산 설치, 한계 |
| | [`PROMPT.ko.md`](runtime/PROMPT.ko.md) | 시스템 프롬프트 구성, 스킬 출처와 호출, 토큰 추정 |
| | [`HOOKS.ko.md`](runtime/HOOKS.ko.md) | 수명 주기 훅 |
| | [`SWARM_TASK_GRAPH.ko.md`](runtime/SWARM_TASK_GRAPH.ko.md) | swarm 태스크 DAG 설계(기본으로 꺼진 기능, `horon-agent-plan` 크레이트의 설계 근거) |
| `provider/` | [`MODEL.ko.md`](provider/MODEL.ko.md) | OpenAI 호환 엔드포인트 직접 호출, 프로바이더 프로파일과 API 키, 모델 역할과 폴백 순서, 한 번만 실행되는 `[prewalk]` 핸드오프, 모델 사용 메타데이터 |
| | [`INTEGRATIONS.ko.md`](provider/INTEGRATIONS.ko.md) | 원격 MCP, 사용자 정의 웹 검색 |
| `safety/` | [`SAFETY.ko.md`](safety/SAFETY.ko.md) | 안전 시스템, 무인 승인과 자동 전환 |
| | [`SECURITY.ko.md`](safety/SECURITY.ko.md) | 워크스페이스 신뢰, 크래시 번들, 의존성 보안 분류 |
| `performance/` | [`BENCHMARK.ko.md`](performance/BENCHMARK.ko.md) | jcode를 이기는 목표와 측정 기준, OS별(Linux·macOS·Windows) 벤치마크, 구조 게이트, A/B 측정, 스킬·MCP 설정 로딩 A/B |
| | [`RESULTS.ko.md`](performance/RESULTS.ko.md) | 측정 결과와 한계 |
| | [`MEMORY.ko.md`](performance/MEMORY.ko.md) | 메모리 회귀 예산, 유휴 세션 파생 캐시 해제, 사고 대응 절차 |
| `release/` | [`INSTALL.ko.md`](release/INSTALL.ko.md) | 릴리스 자산 검증과 `install-release`, npm 설치, Linux 서버 실행, TLS 신뢰 집합 |
| | [`NPM.ko.md`](release/NPM.ko.md) | CLI의 npm 패키지 구성, 설치와 실행, 버전 규칙, 게시 워크플로, 한 번만 하는 trusted publisher 설정, provenance |
| | [`WINDOWS.ko.md`](release/WINDOWS.ko.md) | Windows 빌드와 서명 |

## 갱신 규칙

| 이런 변경이 생기면 | 이렇게 한다 |
| --- | --- |
| 현재 동작이 바뀔 때 | 그 동작을 설명하는 문서를 고친다. |
| 문서를 새로 만들 때 | 맞는 폴더의 기존 문서에 절을 더하는 것을 먼저 검토한다. 새 파일은 대문자 이름으로 만들고 이 표에 올린다. |
| 문서를 옮기거나 지울 때 | 이 표와 코드·스크립트·테스트 속 경로를 같은 변경에서 고친다. |
| 같은 내용이 두 문서에 있을 때 | 하나를 정본으로 두고 나머지는 그것을 가리킨다. |
| 측정 기록·인수 기록·계획을 남기고 싶을 때 | 문서를 만들지 않고 PR 본문이나 이슈에 적는다. 성능 수치는 `performance/RESULTS.md`에만 적는다. |
| 문서를 고칠 때 | 짝을 함께 맞춘다. 모든 `X.md` 옆에 한국어 `X.ko.md`를 두고, `python3 scripts/ci/check_docs_pairs.py`가 두 파일의 존재와 제목·표 행·코드 펜스·링크 수 일치, 상대 링크 해석을 확인한다. |
