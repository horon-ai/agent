# 하네스 API

데스크톱·SDK 클라이언트가 Unix 소켓으로 쓰는 하네스 API의 로그인 흐름과 요청·이벤트 표면을 다룬다.

## Rust SDK로 네이티브 데스크톱 로그인

`horon-agent-sdk`는 `AuthClient`, `AuthOptions`, `LoginProvider`, `LoginMethod`,
`AuthFlow`, `AuthPrompt`, `AuthInputKind`, `AuthResult`를 내보낸다.

### UI 계약

- `/login`을 `send_message` 전에 로컬에서 가로챈다. 클릭 가능한 로그인 동작도 같은
  컨트롤러를 쓴다. 인증 입력, URL, 하위 프로세스 출력을 채팅에 더하면 안 된다.
- 선택기는 `AuthClient::providers()`로 만든다. `resolve_provider()`는 공유 카탈로그
  ID, 별칭, 표시 이름을 받으며 지원하는 방식으로 걸러 낸다.
- `LoginMethod::ApiKey`는 `HoronAgentClient::set_api_key(provider.id, key)`를 쓴다.
  키는 가려진 일시 입력에 둔다. 제출한 뒤 그 입력을 비운다.
- OAuth/기기 코드는 `begin(provider.id, account)`로 I/O를 시작하지 않고 `AuthFlow`를
  할당한다. 취소 버튼을 위해 복제본을 보관한다. 워커 스레드에서 `start()`를 호출하고
  그 `AuthPrompt`는 로그인 패널 안에서만 렌더링한다.
- `prompt.auth_url`은 네이티브 브라우저 열기 버튼으로 시스템 브라우저에서 연다.
  `AuthInputKind`가 지시하는 대로 전용 콜백/코드 입력이나 기기 코드를 보여 준다.
  `submit_callback`, `submit_code`, `complete_device`를 워커 스레드에서 호출한다.
  `start()`는 로컬 HTTP 리다이렉트가 있는 OAuth URL용 선택적 루프백 수신기도 묶는다.
  `start` 뒤에 `has_callback_listener()`를 확인하고 별도 워커에서
  `wait_for_callback()`을 호출해 브라우저 로그인을 자동으로 마친다. 대기하는 동안
  수동 붙여넣기를 계속 쓸 수 있게 둔다. 포트가 바쁘거나 호스팅 리다이렉트면 수동
  입력으로 물러난다. 예상 밖 경로/상태는 흐름을 소비하지 않으며 CLI는 교환 중에도
  state와 PKCE를 계속 검증한다. 취소와 수동 완료 성공은 콜백 워커를 중단한다. UI가
  이미 흐름을 마쳤거나 교체했으면 그 낡은 결과는 무시한다.
- 패널을 닫거나 취소할 때, 기기 폴링이나 begin이 실행 중일 때도 UI 스레드 밖에서
  `cancel()`을 호출한다. 소유한 자식을 죽이고 회수하며 그 흐름 ID만 정리한다. 마지막
  복제본을 버리면 제한된 정리도 예약된다. 취소는 교환으로 이미 발급된 자격 증명을
  폐기하지 않는다.
- `AuthResult.validation_warning`은 자격 증명을 저장했지만 검증에 실패했다는 뜻이다.
  이미 쓴 코드를 재시도하지 않는다. 대신 모델 선택/복구를 제공한다.

### 런타임과 호환성

`AuthOptions`는 신뢰하는 로컬 실행 파일, `HORON_AGENT_HOME`, 그리고 **데몬**
소켓(하네스 API 소켓이 아니다)을 고른다. 기본값은 `HORON_AGENT_BIN`이나 PATH의
`horon-agent`를 쓰고, 자격 증명 홈을 상속하고, 일반 데몬 소켓을 쓴다. 클라이언트는
로컬 전용이다. SSH로 붙은 데스크톱은 이 로컬 흐름을 명시적으로 꺼야 한다.

기존 스크립트형 CLI는 범위가 정해진 대기 상태, PKCE/state 검증, 자격 증명 영속화를
처리한다. 콜백/코드 입력은 argv가 아니라 표준 입력으로 흐른다. SDK는 하위 프로세스
출력을 제한하고 표준 오류를 억제하며 가린 오류를 돌려준다. `AuthPrompt`에는
의도적으로 `Debug`/직렬화 구현이 없다.

정상 CLI 완료는 이미 데몬에 알린다. SDK는 저장했지만 검증하지 않은 완료 뒤에 최선
노력 레거시 인증 변경 알림을 보내므로 이 경로는 데몬이나 하네스 업그레이드가 필요
없다. 추가된 `HoronAgentClient::notify_auth_changed()` / TypeScript
`notifyAuthChanged()` API는 갱신된 하네스 브리지를 요구하며
`auth_changed_notification`으로 광고된다. 그 응답은 비동기 모델 탐색의 완료가 아니라
알림 접수를 확인한다.

OAuth는 OpenAI만 지원한다. HoronAgent 구독은 여기서 API 키 입력을 지원하며 대화형
기기 로그인은 지원하지 않는다. 그 밖의 벤더는 이 빌드에서 삭제했으므로 데스크톱에서도
노출하지 않는다.

선택기는 `AuthClient::providers()`가 공유 카탈로그를 `supported()`로 거른 결과다
(`crates/agent-sdk/src/auth.rs:60-78`). 그 함수에는 Claude, Gemini,
Antigravity 대상을 OAuth로 매핑하는 갈래가 남아 있지만, 공유 카탈로그가 그
프로바이더 행을 더는 담지 않으므로
(`crates/provider-metadata/src/catalog.rs:1146-1192`) 선택기에도 나오지
않는다. 그래서 OAuth 항목은 OpenAI 하나이고, 나머지는 OpenAI 호환
API 키 항목과 HoronAgent 구독이다.

### 검증

단위 테스트는 카탈로그 해석, 비밀 없는 표준 입력 전송, 루프백 완료와 요청 거부, 포트
충돌 대체, 제한된 오류, 검증 경고, 데몬 알림, 시간 초과/회수, 동시 취소, 드롭 정리를
다룬다. 테스트는 `crates/agent-sdk/src/auth/tests.rs`에 있고 옵트인 표시가
없다. 설치된 CLI 바이너리를 대상으로 begin/cancel을 돌리는 테스트는 이 빌드에 없고,
`HORON_AGENT_AUTH_TEST_BINARY` 같은 환경변수도 읽지 않는다. 테스트는 브라우저를 열지
않고 OAuth를 완료하지도 않으며 인가 URL을 인쇄하지도 않는다.

### 내구성 런타임 API(프로토콜 1.14)

주 버전은 `1`에 머물고 부 버전이 13에서 14로 올랐다. 아래 표면은 모두 추가형이다.
기존 요청의 바이트 표현과 응답 모양은 바뀌지 않았고, 새 요청을 보내지 않는 클라이언트는
이전과 똑같이 동작한다.

브리지는 다음 네 기능을 광고한다. 클라이언트는 무엇이든 보내기 전에 확인한다.

| 기능 | 게이트하는 표면 |
| --- | --- |
| `durable_runtime_v1` | `get_runtime_state`, `apply_control`, `get_control_receipt` |
| `monitor_v1` | 없음. 감시는 기존 도구 호출로 시작하고 멈추므로 결과를 어떻게 읽을지 알려 주는 표시다 |
| `snapshot_marker_v1` | `get_snapshot`과 `snapshot_marker` 이벤트 |
| `ask_receipts_v1` | `ask_response`의 선택적 `answer_id` |

기능 없이 보낸 요청은 빠진 기능 이름을 담은 구조화된 `unsupported` 오류로 거부된다.
성공으로 처리되지 않고, 버전 오류도 아니다. 두 SDK는 같은 이름의 메서드
(`get_runtime_state`/`getRuntimeState`, `apply_control`/`applyControl`,
`get_control_receipt`/`getControlReceipt`, `get_snapshot`/`getSnapshot`,
`respond_to_ask_with_id`/`respondToAskWithId`)를 제공하며, 기능이 없으면 요청을 소켓에
쓰기 전에 거부한다. 구서버가 알 수 없는 요청을 받아 잘못 처리하는 일을 막는 지점이다.

#### 상태 요약

`get_runtime_state`는 세션의 내구성 저널에서 실행·대기열·영수증·감시를 **요약**해 돌려준다.
접수된 입력 본문, 명령 인자, 명령 환경변수는 여기에 실리지 않는다. 상태 판넬에 필요한 것은
개수·단계·원인이지 대기열에 든 문장이 아니다.

| 필드 | 뜻 |
| --- | --- |
| `journal` | 저널 존재 여부, 세대, 마지막 seq, 바이트, 레코드 수, 읽기 전용 여부 |
| `runs` | 아직 끝나지 않은 실행. `run_id`, `started_at`, 선택적 `input_id` |
| `queue` | 살아 있는 입력 수, 결과가 확정되지 않은 입력 수, 보유 바이트 |
| `receipts` | 완료·취소된 입력 영수증(최근 것부터). `input_id`, `phase`, `seq` |
| `monitors` | 감시 id, 종류, 중지 여부, 마지막 체크포인트 요약 문장 |
| `pending_wakes` | 전달되지 않은 깨우기 수 |
| `pending_controls` | 아직 끝나지 않은 제어 수 |
| `last_stop` | 마지막 실행이 끝난 원인과 시각 |

#### 제어와 영수증

`apply_control`은 호출자가 정한 `operation_id`로 제어 동작을 적용한다. 같은 id를 다시 보내면
기록된 영수증을 돌려주고 동작을 두 번 적용하지 않는다. `get_control_receipt`는 그 기록을 읽는다.
`outcome`은 `applying`, `terminal`, `replay`, `conflict`, `unknown`, `indeterminate` 중
하나다. `indeterminate`는 동작이 시작됐지만 종료 기록을 남기지 못해 결과를 알 수 없다는 뜻이며,
자동으로 재시도하지 않는다.

#### 스냅샷과 마커

`get_snapshot`은 상태와 제한된 기록 꼬리를 하나의 일관된 시점으로 돌려준다. 응답 직전에 같은
연결로 `snapshot_marker` 이벤트가 먼저 도착하며, 두 프레임의 `marker_id`가 같다. 그 뒤에
도착한 실시간 이벤트는 모두 그 스냅샷보다 새것이다. 마커는 `reply_to`가 없는 스트림
이벤트이므로 클라이언트는 도착 순서를 추측하지 않고 경계를 분류할 수 있다. `generation`은
재시작마다 바뀌며 과거 `seq`의 재생을 약속하지 않는다.

`get_snapshot`은 `tail`(기본 20, 최대 100)과 `max_bytes`(기본 262,144, 최대 1 MiB)를 받고,
예산이 꼬리를 자르면 `truncated`가 참이 된다.

#### 답변 영수증

`ask_response`에 선택적 `answer_id`가 붙었다. 같은 `ask_id`에 같은 `answer_id`와 같은 답을
다시 보내면 한 번 답한 것으로 처리되므로, 응답을 잃은 뒤의 재시도가 질문을 두 번 해결하지
않는다. 같은 질문에 다른 `answer_id`를 보내면 충돌이다. `answer_id`를 보내지 않는 기존
경로는 그대로 동작하며 기능 협상도 필요하지 않다.

#### 새 이벤트

`runtime_state`, `control_receipt`, `snapshot_marker`, `snapshot` 네 가지가 추가됐다.
모두 기능으로 게이트되며, 그 기능을 협상하지 않은 연결에는 서버가 보내지 않고 SDK도 받은
뒤 버린다. 기존 연결의 바이트 수준 동작은 바뀌지 않는다.

#### 아직 연결되지 않은 부분

`apply_control`/`get_control_receipt`와 `get_snapshot`의 프로토콜 모양과 SDK 메서드는
확정됐지만, 그 뒤의 저장·관측 구현은 별도 작업이 소유한다. 그 작업이 붙기 전까지 브리지는
이 요청들에 `unsupported`로 답한다. 클라이언트 입장에서는 "이 서버가 아직 제공하지 않는다"와
같은 응답이므로 분기 처리가 달라지지 않는다.

### 세션 이벤트 저장소와 복제(프로토콜 1.12)

하네스 API는 주 버전 1을 유지하고 부 버전 11에서 12로 옮겼다. 아래 내용은 모두
추가형이다. 이미 보내던 요청만 보내는 클라이언트는 계속 동작하고, 대화 기록을 읽는
데 새 요청이 필요하지 않다.

클라이언트는 핸드셰이크를 협상하고 무엇이든 보내기 전에 필요한 기능을 확인한다.
브리지는 기존 집합에 더해 다음을 광고한다.

| 기능 | 통제 대상 |
| --- | --- |
| `session_event_store_v1` | `list_session_heads`, `select_session_head` |
| `session_replication_v1` | `export_session_events`, `import_session_events`, `export_session_blob`, `import_session_blob`, `get_session_blob_status`, 그리고 전체 레코드 blob 쌍 |
| `session_history_cursor_v1` | `get_history_page` |
| `structured_compaction_v1` | `compacted` 레코드의 구조화 요약 필드 읽기. 이 기능으로 통제되는 메서드는 없다 |

기능 없이 보낸 요청은 빠진 기능 이름을 담아 `unsupported`로 응답한다. 이는 버전
오류가 아니다. 데몬과 브리지를 모두 갱신해 재시작하거나, 이 빌드에 대해 그 메서드를
쓰지 않아야 한다.

#### 헤드

세션은 되감기나 분기된 가져오기 뒤에 헤드를 여러 개 보유할 수 있다. 선택된 헤드는
**호스트 로컬**이다. 복제되지 않고 세션의 영구 정체성에도 속하지 않는다.
`list_session_heads`는 헤드와 현재 선택을 보고하며, 여러 헤드 중 명시적 선택이
필요하면 `selected_head`가 없다. 하나를 선택할 때까지 실행을 거부하고, 다른
프로세스가 쓰기 권한을 쥐고 있으면 `select_session_head`가 `busy`로 응답한다.
데스크톱 클라이언트는 사용자의 다음 턴이 무엇에서 이어지는지를 바꾸므로, 몰래 하나를
고르지 말고 그 선택을 사용자에게 보여 줘야 한다.

#### 기록

`get_history`는 바뀌지 않았고 여전히 보이는 전체 대화 기록을 돌려준다.
`get_history_page`는 긴 세션에서 UI가 원하는 제한 읽기다. `session_id`, 선택적
`head_event_id`(기본값은 선택된 헤드), 불투명 이어가기 값 `before_event_id`,
`limit`(기본 50, 최대 200)을 받는다. 응답은 고정된 `head`, 페이지 메시지를 오래된
것부터 담은 목록, `before_event_id`로 되돌려 넘길 `next_cursor`를 가진
`SessionHistoryPage`다. 사용자가 스크롤하는 동안 `head_event_id`를 고정한다. 헤드를
고정한 커서는 선택이 바뀐 뒤에도 같은 기록을 가리킨다.

#### 복제

`export_session_events`는 고정 헤드에 대한 원본을 부모 우선으로 한 페이지씩 제한해
돌려준다. `import_session_events`는 커밋 전에 페이지 전체를 검증하고
`accepted_ids`, `heads`, `retry_cursor`, `conflict_heads`로 응답한다. 원본 blob은
1 MiB 청크로 따로 이동하며 발행 전에 SHA-256과 크기로 검증한다. 중단된 전송은
`get_session_blob_status`에서 이어 간다.

클라이언트가 어겨서는 안 되는 규칙 두 가지:

- **`canonical_json`을 다시 직렬화하지 않는다.** 이벤트의 정규 JSON은 문자열로
  이동한다. 2^53을 넘는 `u64`, `1.0`, `-0.0`이 JavaScript 숫자 왕복을 견디지
  못하기 때문이다. 문자열을 그대로 통과시킨다.
- **`conflict_heads`를 데이터로 다룬다.** 분기된 헤드는 모두 보존된다. 타임스탬프로
  승자를 정하지 않으며, 가져오기는 사용자가 고르지 않은 분기를 버리면 안 된다.

가져오기를 다시 보내는 것은 안전하다. 같은 ID에 같은 해시면 아무 일도 일어나지
않는다.

#### 오류

안정된 `ErrorCode` 값 다섯 개는 바뀌지 않았으므로 구버전 클라이언트도 계속 해독한다.
더 자세한 원인은 선택적 `details` 객체로 온다. `store_code`(예: `hash_conflict`,
`missing_dependency`, `conflict_heads`, `busy`, `invalid_cursor`,
`record_too_large`), `retryable`, `accepted_ids`, `retry_cursor`,
`conflict_heads`가 여기에 들어간다. `details`를 쓰지 않으면 무시한다. 재시도 전에
`retryable`로 분기하고, `hash_conflict`는 바꾸지 않은 채 절대 재시도하지 않는다.

#### 상태

`runtime_info`는 현재 호스트 런타임에서 생존 여부를 구성한다. 오래된 세션
메타데이터에 기록된 PID는 과거 보조 정보일 뿐 살아 있는 프로세스가 아니며, 어떤
클라이언트도 이를 실행 중으로 표시하면 안 된다.

전체 저장 형식, 한도, 레거시 마이그레이션 규칙은
`docs/protocol/SESSION_STORE.md`에 있다.

## 하네스 API 요청·이벤트 표면: usage_report, search_sessions, list_artifacts, durable_runtime

이 문서는 하네스 API에서 추가된 요청 표면을 적습니다. 앞부분은 프로토콜 1.13의 세 요청과 그 응답 이벤트이고, 끝부분은 1.14의 내구 런타임 표면 넷입니다. 필드 이름과 타입은 `crates/harness-api/src/requests.rs`와 `crates/harness-api/src/events.rs`의 정의를 그대로 옮겼고, 동작과 한도는 `crates/harness-api-server/src/translate/`의 구현에서 확인한 값입니다.

세 요청 모두 추가형입니다. 이 요청들을 보내지 않는 클라이언트는 계속 동작하고, 브리지는 `hello_ok`의 `capabilities`에 `usage_report`, `session_search`, `artifacts`를 광고합니다(`crates/harness-api-server/src/lib.rs`의 `BRIDGE_CAPABILITIES`).

### 공통 규칙

- 프레임 하나는 JSON 객체 하나, 한 줄입니다(NDJSON). 요청은 `req`로 종류를 구분하고 `id`를 싣고, 응답은 `reply_to`로 그 `id`를 되돌려 줍니다.
- 요청 본문은 `#[serde(tag = "req", rename_all = "snake_case")]`로 직렬화되므로 필드 이름은 snake_case입니다.
- 모르는 필드는 무시하고, 모르는 이벤트 종류는 건너뜁니다.
- 실패는 `{"ev":"error","code":...,"message":...}`로 옵니다. 이 표면들이 쓰는 `ErrorCode` 값은 `invalid_request`, `unknown_session`, `internal`입니다.
- 세션을 지목하는 요청은 `session_id`를 받습니다. 없는 세션은 빈 결과가 아니라 `unknown_session`으로 답합니다. "이 세션은 아무것도 쓰지 않았다"와 "그런 세션이 없다"는 다른 사실이기 때문입니다.

### usage_report

첨부된 세션과 저장된 모든 세션의 토큰 사용량, 컨텍스트 창, 모델별 집계, 구독 한도를 돌려줍니다. 능력 이름은 `usage_report`입니다.

#### 요청

```json
{ "v": 1, "id": 7, "req": "usage_report", "session_id": "session_..." }
```

| 필드 | 타입 | 필수 | 뜻 |
| --- | --- | --- | --- |
| `session_id` | string | 아니요 | 이 세션으로 보고를 좁힙니다. 생략하면 저장된 모든 세션과 첨부된 세션을 함께 보고합니다 |

없는 `session_id`는 `unknown_session`입니다.

#### 응답: `usage_report`

```json
{
  "v": 1,
  "reply_to": 7,
  "ev": "usage_report",
  "session_id": "session_...",
  "sessions": [],
  "totals": {},
  "models": [],
  "context_window": 200000,
  "limits": [],
  "pricing": "unknown",
  "estimated_cost": null
}
```

| 필드 | 타입 | 뜻 |
| --- | --- | --- |
| `session_id` | string, 없을 수 있음 | 보고를 좁힌 세션. 전체 보고에는 없습니다 |
| `sessions` | `UsageSession[]` | 범위 안의 세션마다 한 행, 세션 id 순 |
| `totals` | `UsageTotals` | `sessions` 전체의 합 |
| `models` | `UsageModel[]` | 모델별 집계, 모델 id 순 |
| `context_window` | number, 없을 수 있음 | 첨부된 세션의 활성 경로 컨텍스트 창(토큰). 없으면 "모른다"이고 다른 모델의 기본 창이 아닙니다 |
| `limits` | `UsageLimitInfo[]` | 데몬이 알린 구독 한도. 로그인·구독이 없거나 데몬이 알리지 않으면 빈 배열 |
| `pricing` | string | 항상 `"unknown"`. 이 저장소에는 가격표가 없습니다 |
| `estimated_cost` | number, 없을 수 있음 | 항상 `null`. 가격표가 없으니 비용 추정도 없습니다 |

`UsageSession`:

| 필드 | 타입 | 뜻 |
| --- | --- | --- |
| `session_id` | string | 세션 id |
| `model` | string, 없을 수 있음 | 세션 메타데이터가 기록한 모델 |
| `input` | number, 없을 수 있음 | 입력 토큰 합. 기록한 요청이 없으면 없음 |
| `output` | number, 없을 수 있음 | 출력 토큰 합 |
| `cache_read_input` | number, 없을 수 있음 | 캐시 읽기 입력 토큰 합 |
| `cache_creation_input` | number, 없을 수 있음 | 캐시 쓰기 입력 토큰 합 |
| `requests` | number | 사용량을 기록한 제공자 요청 수. 사용자 턴 수가 아닙니다. 한 턴이 여러 요청을 내고 도구 왕복이 따로 셉니다 |

`UsageTotals`는 `input`, `output`, `cache_read_input`, `cache_creation_input`(각각 없을 수 있음), `requests`, `sessions`(합이 덮는 세션 수, 사용량이 없는 세션 포함)를 가집니다.

`UsageModel`은 `model`(메타데이터가 모델을 밝히지 않은 세션은 `unknown`), 위 네 토큰 합, `requests`, `sessions`를 가집니다.

`UsageLimitInfo`는 `name`(예: `subscription_spend`), `used`, `limit`, `unit`(예: `usd`), `resets_at`(RFC 3339)을 가집니다. `used`, `limit`, `resets_at`은 데몬이 알렸을 때만 있습니다.

#### 동작

보고는 세 출처를 권위 순으로 합칩니다. 첨부된 세션의 데몬 누적치, 저장된 세션 원본의 `token_usage` 기록, 마지막 실시간 `token_usage` 이벤트입니다. 어떤 요청도 기록하지 않은 지표는 `0`이 아니라 `null`입니다. 제공자가 캐시 토큰을 보고하지 않은 것과 0을 보고한 것은 다른 사실이기 때문입니다.

구독 한도는 런타임 명령 뒤에 있습니다. 데몬이 `runtime_commands`를 알리면 첫 보고가 그 조회로 미뤄지고, 이후 보고는 캐시에서 답합니다. `limits`가 비었는데 아직 조회하지 않은 상태라면 "이 계정에 한도가 없다"가 아니라 "아직 묻지 않았다"입니다.

### search_sessions

첨부된 세션이 아니라 저장된 모든 세션의 트랜스크립트를 검색합니다. 능력 이름은 `session_search`입니다. 세션 전환기가 붙은 세션만 거르면 "그 대화가 어디 있었더라"에 답할 수 없기 때문에 필요합니다.

#### 요청

```json
{ "v": 1, "id": 8, "req": "search_sessions", "query": "zanzibar", "limit": 10 }
```

| 필드 | 타입 | 필수 | 뜻 |
| --- | --- | --- | --- |
| `query` | string | 예 | 검색어. 공백만 있으면 `invalid_request`입니다 |
| `limit` | number | 아니요 | 최대 적중 수. 기본 20, 상한 50 |

`query`는 경로·glob·SQL 조각으로 해석하지 않습니다. 문자열 그대로 검색합니다. `limit`이 0이면 빈 결과를 돌려줍니다.

#### 응답: `session_search_results`

```json
{
  "v": 1,
  "reply_to": 8,
  "ev": "session_search_results",
  "query": "zanzibar",
  "results": []
}
```

| 필드 | 타입 | 뜻 |
| --- | --- | --- |
| `query` | string | 요청의 검색어를 그대로 되돌려 줍니다. 검색을 여러 번 보낸 클라이언트가 화면에 남은 답을 구분할 수 있게 하려는 것입니다 |
| `results` | `SessionSearchHit[]` | 적중 목록 |

`SessionSearchHit`:

| 필드 | 타입 | 뜻 |
| --- | --- | --- |
| `session_id` | string | 적중이 속한 저장 세션 |
| `title` | string, 없을 수 있음 | 그 세션의 표시 제목 |
| `role` | string | `user`, `assistant`, 또는 제목·세션 수준 적중이면 `metadata` |
| `preview` | string | 일치 지점 앞뒤를 잘라낸 평문 발췌. 잘린 구간은 `...`로 표시합니다 |
| `at` | string | 일치한 메시지의 RFC 3339 시각. 메타데이터 적중이면 세션 시각 |

#### 동작

에이전트 자신의 세션 검색과 같은 엔진, 같은 인덱스, 같은 점수를 씁니다. 그래서 에이전트에서 검색한 결과와 클라이언트에서 검색한 결과가 같습니다. 저장 디렉터리가 없으면 빈 결과이고, 엔진이 실패하면 `internal`입니다.

### list_artifacts

대화가 만든 파일, 즉 대화가 실어 온 이미지와 세션이 시작된 뒤 작업 디렉터리에 쓰인 HTML·이미지 파일을 나열합니다. 능력 이름은 `artifacts`입니다.

#### 요청

```json
{ "v": 1, "id": 9, "req": "list_artifacts", "session_id": "session_..." }
```

| 필드 | 타입 | 필수 | 뜻 |
| --- | --- | --- | --- |
| `session_id` | string | 예 | 나열할 세션. 없거나 저장 기록이 없으면 `unknown_session`입니다 |

#### 응답: `artifacts`

```json
{
  "v": 1,
  "reply_to": 9,
  "ev": "artifacts",
  "session_id": "session_...",
  "artifacts": []
}
```

| 필드 | 타입 | 뜻 |
| --- | --- | --- |
| `session_id` | string | 요청한 세션 |
| `artifacts` | `ArtifactEntry[]` | 최신 순 목록 |

`ArtifactEntry`:

| 필드 | 타입 | 뜻 |
| --- | --- | --- |
| `path` | string | 세션 작업 디렉터리 기준 상대 경로. `/` 구분자입니다 |
| `kind` | string | `html` 또는 `image` |
| `bytes` | number | 응답 시점의 파일 크기 |
| `mtime_ms` | number | 마지막 수정 시각, epoch 밀리초 |

#### 동작

메타데이터만 돌려줍니다. 바이트는 클라이언트가 `read_file`로 따로 읽습니다. 그래서 40MB짜리 HTML 보고서도 후보마다 `stat` 한 번이면 됩니다.

목록은 두 경로에서 모읍니다. 트랜스크립트가 지목한 경로와 작업 디렉터리 스캔입니다. 두 경로 모두 세션 루트 밖으로 나가는 경로와 심볼릭 링크를 거부하므로, 트랜스크립트가 `../../.ssh/id_rsa`를 지목해도 목록에 오르지 않습니다. 세션 시작 시각을 기록이 없으면 세션 파일의 수정 시각으로 대신하고, 그보다 오래된 파일은 제외합니다. 결과는 `mtime_ms` 내림차순, 같은 시각이면 경로 오름차순으로 정렬하고 200개에서 자릅니다.

### durable_runtime_v1 · monitor_v1 · snapshot_marker_v1 · ask_receipts_v1

프로토콜 minor 14가 추가한 네 표면입니다. 넷 다 **추가형**이라, 능력을 협상하지 않은 클라이언트는
이 요청을 보내지도 받지도 않습니다. 기존 클라이언트의 동작은 바뀌지 않습니다.

| 능력 | 여는 요청·이벤트 | 무엇을 보장하는가 |
| --- | --- | --- |
| `durable_runtime_v1` | `get_runtime_state`, `apply_control`, `get_control_receipt` | 재시작 뒤에도 남는 실행 상태와, 같은 operation id의 재전송이 한 번만 집행되는 제어 영수증 |
| `monitor_v1` | 도구 `monitor`의 파일·명령 감시 | 재시작 뒤 `needs_rearm`으로 남고 명령을 다시 실행하지 않음 |
| `snapshot_marker_v1` | `get_snapshot` | `SnapshotMarker`가 `Snapshot` 바로 앞에 1:1로 오고, 그 뒤 라이브 이벤트의 `seq`가 이어짐 |
| `ask_receipts_v1` | `ask_response`의 `answer_id` | 같은 `question_id`+`answer_id`+`digest`는 저장된 결과를 재생하고, 다른 digest는 충돌 |

#### 경계 규칙

- `SnapshotMarker`와 `Snapshot`은 다른 연결로 갈 수 없습니다. 마커를 받은 연결만 스냅샷을 받습니다.
- 스냅샷 이후의 라이브 이벤트는 관측 소유자가 발급한 `seq`를 싣습니다. 클라이언트는 "N 이후는 새것"을
  스스로 증명할 수 있고, 순서를 추측하지 않습니다.
- `apply_control`은 지문을 서버가 만듭니다. 같은 id·같은 지문은 저장된 결과 재생, 같은 id·다른 지문은
  `conflict`, 동시 applying은 `pending`이며 두 번째 실행은 하지 않습니다. 재시작에서 applying으로
  남은 것은 `indeterminate`로 봉인하고 다시 실행하지 않습니다.
- 보존 범위를 벗어난 operation id는 `unknown`입니다. SDK는 그 경우 자동 재시도하지 않습니다.
- 지원 동작은 `abort_run(expected_run_id)`와 `cancel_inputs(input_ids)` 둘뿐입니다. 모델 변경·파일·셸·
  임의 명령은 이 표면에 없습니다.
- 브리지가 광고하지 않는 능력에 걸린 요청은 도달할 수 없습니다. 그래서 네 문자열이 모두
  `BRIDGE_CAPABILITIES`에 있습니다.

### 근거

- 요청 정의: `crates/harness-api/src/requests.rs`의 `GetRuntimeState`, `ApplyControl`, `GetControlReceipt`, `GetSnapshot`, `AskResponse.answer_id`
- 이벤트 정의: `crates/harness-api/src/events.rs`의 `SnapshotMarker`, `Snapshot`, `RuntimeStateSummary`, `ControlReceipt`
- 구현: `crates/app-core/src/server/control_receipts.rs`, `crates/app-core/src/server/session_observation.rs`, `crates/base/src/durable_runtime/`
- 능력 상수: `crates/harness-api/src/lib.rs`의 `CAPABILITY_DURABLE_RUNTIME_V1`, `CAPABILITY_MONITOR_V1`, `CAPABILITY_SNAPSHOT_MARKER_V1`, `CAPABILITY_ASK_RECEIPTS_V1`
- 프로토콜 버전: `API_VERSION_MINOR = 14`
