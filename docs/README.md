# horon agent 문서

이 폴더는 horon agent의 현재 동작만 설명한다. 문서는 한국어로 쓰고, 경로·명령·식별자는 원형을 유지한다. 파일명은 모두 대문자로 쓰고, 주제별 하위 폴더에 둔다.

문서는 빌드할 때 바이너리에 들어가고(`crates/app-core/build.rs`가 하위 폴더까지 읽는다) `horon_agent_docs` 도구가 읽는다. 문서를 옮기거나 지우면 코드·스크립트·테스트에 남은 경로도 같은 변경에서 고친다.

## 고정 요구사항

모든 변경은 아래 세 가지를 지킨다. 정본은 저장소 루트의 `AGENTS.md`다.

| 요구사항 | 내용 | 현재 상태 |
| --- | --- | --- |
| TUI 제외 | 바이너리는 화면을 열지 않는다. `ratatui`, `crossterm`, `horon-agent-tui*`가 의존성 그래프에 없어야 한다. | 충족 |
| ACP v1·v2 지원 | `horon-agent acp`가 `protocolVersion` 1과 2를 모두 처리하고, 양쪽이 지원하는 가장 높은 버전으로 답한다. v2를 더하면서 v1을 깨지 않는다. | 충족. v2 협상은 `src/cli/acp/v2/negotiate.rs`, v1은 `src/cli/acp/util.rs` |
| 성능 회귀 방지 | 같은 기계·빌드 프로필·작업에서 기록된 기준선과 같거나 빨라야 한다. 느려지는 변경은 병합하지 않는다. | 측정 방법과 기록된 수치가 있음(`performance/BENCHMARK.md`, `performance/RESULTS.md`). 기록된 수치가 기준선이다 |

## 폴더 구조

| 폴더 | 문서 | 내용 |
| --- | --- | --- |
| `docs/` | [`README.md`](README.md) | 이 색인과 갱신 규칙 |
| `architecture/` | [`SERVER.md`](architecture/SERVER.md) | 서버·세션·소켓 구조 |
| | [`CRATES.md`](architecture/CRATES.md) | 크레이트 소유권 경계, 플랫폼 어댑터, 저장 내구성과 TLS 신뢰 집합 |
| `protocol/` | [`ACP.md`](protocol/ACP.md) | ACP v1·v2 어댑터, 에디터 연결과 stdio 계약 |
| | [`HARNESS_API.md`](protocol/HARNESS_API.md) | 하네스 API 자격 증명 설정과 요청·이벤트 표면 |
| | [`SESSION_STORE.md`](protocol/SESSION_STORE.md) | 세션 이벤트 저장 형식, 전송 계약, writer 풀, 영속 런타임 저널 |
| `runtime/` | [`TOOLS.md`](runtime/TOOLS.md) | 도구 로딩, 효과와 승인 등급, 편집 안전 장치, 상위 `AGENTS.md`, 보조 호출 |
| | [`TOOL_SCHEDULING.md`](runtime/TOOL_SCHEDULING.md) | 도구 스케줄링과 반복 방지 |
| | [`TURNS.md`](runtime/TURNS.md) | 턴 복구, 실행 중 메시지 삽입, 런타임 명령, `--resume` |
| | [`IRC.md`](runtime/IRC.md) | 에이전트 간 IRC 메일박스 버스, `irc` 도구, 설정, 성능 게이트 |
| | [`PROMPT.md`](runtime/PROMPT.md) | 시스템 프롬프트 구성과 토큰 추정 |
| | [`HOOKS.md`](runtime/HOOKS.md) | 수명 주기 훅 |
| | [`SWARM_TASK_GRAPH.md`](runtime/SWARM_TASK_GRAPH.md) | swarm 태스크 DAG 설계(기본으로 꺼진 기능, `horon-agent-plan` 크레이트의 설계 근거) |
| `provider/` | [`MODEL.md`](provider/MODEL.md) | OpenAI 호환 엔드포인트 직접 호출, 프로바이더 프로파일과 API 키, 모델 사용 메타데이터 |
| | [`INTEGRATIONS.md`](provider/INTEGRATIONS.md) | 원격 MCP, 사용자 정의 웹 검색 |
| `safety/` | [`SAFETY.md`](safety/SAFETY.md) | 안전 시스템, 무인 승인과 자동 전환 |
| | [`SECURITY.md`](safety/SECURITY.md) | 워크스페이스 신뢰, 크래시 번들, 의존성 보안 분류 |
| `performance/` | [`BENCHMARK.md`](performance/BENCHMARK.md) | OS별(Linux·macOS·Windows) 벤치마크, 구조 게이트, A/B 측정 |
| | [`RESULTS.md`](performance/RESULTS.md) | 측정 결과와 한계 |
| | [`MEMORY.md`](performance/MEMORY.md) | 메모리 회귀 예산, 유휴 세션 파생 캐시 해제, 사고 대응 절차 |
| `release/` | [`INSTALL.md`](release/INSTALL.md) | 릴리스 자산 검증과 `install-release`, Linux 서버 실행, TLS 신뢰 집합 |
| | [`WINDOWS.md`](release/WINDOWS.md) | Windows 빌드와 서명 |

## 갱신 규칙

| 이런 변경이 생기면 | 이렇게 한다 |
| --- | --- |
| 현재 동작이 바뀔 때 | 그 동작을 설명하는 문서를 고친다. |
| 문서를 새로 만들 때 | 맞는 폴더의 기존 문서에 절을 더하는 것을 먼저 검토한다. 새 파일은 대문자 이름으로 만들고 이 표에 올린다. |
| 문서를 옮기거나 지울 때 | 이 표와 코드·스크립트·테스트 속 경로를 같은 변경에서 고친다. |
| 같은 내용이 두 문서에 있을 때 | 하나를 정본으로 두고 나머지는 그것을 가리킨다. |
| 측정 기록·인수 기록·계획을 남기고 싶을 때 | 문서를 만들지 않고 PR 본문이나 이슈에 적는다. 성능 수치는 `performance/RESULTS.md`에만 적는다. |
