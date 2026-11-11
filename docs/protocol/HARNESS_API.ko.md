# 하네스 API

언어: [English](HARNESS_API.md) | **한국어**

데스크톱·SDK 클라이언트가 Unix 소켓으로 쓰는 하네스 API의 자격 증명 설정과 요청·이벤트
표면을 다룬다.

## 자격 증명 설정

이 빌드에는 대화형 로그인 흐름이 없다. SDK가 자격 증명에 대해 노출하는 것은 둘이다.

- `HoronAgentClient::set_api_key(provider, key)` (TypeScript `setApiKey`)는 키를
  horon-agent의 소유자 전용 자격 증명 저장소에 쓰고(`SetApiKey` 요청,
  `credential_updated` 응답) 프로바이더 자격 증명을 다시 읽는다. 키는 응답이나 로그에
  남지 않는다.
- `notify_auth_changed(provider)` (TypeScript `notifyAuthChanged`)는 사용자가 다른
  도구로 OAuth에 로그인한 뒤 갱신된 자격 증명 파일을 다시 읽으라는 알림이다. 자격 증명
  자체를 보내지 않으며 브리지는 `auth_changed_notification`으로 이 기능을 광고한다.

OAuth 토큰은 SDK가 발급하거나 갱신하지 않는다. 런치 옵션 `inherit_logins`(기본 켜짐)가
사용자의 기존 CLI 자격 증명 파일을 인스턴스에 연결하고, 모델 요청은 그 파일을 그대로
쓴다(`crates/agent-sdk/src/launch.rs`). 모델 호출은 `[providers.<name>]`의 OpenAI 호환
프로파일로만 나가므로, 이 빌드에는 OAuth로 인증하는 모델 런타임이 없다. `openai-oauth:`
접두사는 남아 있지만 그 런타임이 없어 호출 전에 실패한다(`docs/provider/MODEL.ko.md`).

## 내구성 런타임 API(프로토콜 1.14)

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

### 상태 요약

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

### 제어와 영수증

`apply_control`은 호출자가 정한 `operation_id`로 제어 동작을 적용한다. 같은 id를 다시 보내면
기록된 영수증을 돌려주고 동작을 두 번 적용하지 않는다. `get_control_receipt`는 그 기록을 읽는다.
`outcome`은 `applying`, `terminal`, `replay`, `conflict`, `unknown`, `indeterminate` 중
하나다. `indeterminate`는 동작이 시작됐지만 종료 기록을 남기지 못해 결과를 알 수 없다는 뜻이며,
자동으로 재시도하지 않는다.

### 스냅샷과 마커

`get_snapshot`은 상태와 제한된 기록 꼬리를 하나의 일관된 시점으로 돌려준다. 응답 직전에 같은
연결로 `snapshot_marker` 이벤트가 먼저 도착하며, 두 프레임의 `marker_id`가 같다. 그 뒤에
도착한 실시간 이벤트는 모두 그 스냅샷보다 새것이다. 마커는 `reply_to`가 없는 스트림
이벤트이므로 클라이언트는 도착 순서를 추측하지 않고 경계를 분류할 수 있다. `generation`은
재시작마다 바뀌며 과거 `seq`의 재생을 약속하지 않는다.

`get_snapshot`은 `tail`(기본 20, 최대 100)과 `max_bytes`(기본 262,144, 최대 1 MiB)를 받고,
예산이 꼬리를 자르면 `truncated`가 참이 된다.

### 답변 영수증

`ask_response`에 선택적 `answer_id`가 붙었다. 같은 `ask_id`에 같은 `answer_id`와 같은 답을
다시 보내면 한 번 답한 것으로 처리되므로, 응답을 잃은 뒤의 재시도가 질문을 두 번 해결하지
않는다. 같은 질문에 다른 `answer_id`를 보내면 충돌이다. `answer_id`를 보내지 않는 기존
경로는 그대로 동작하며 기능 협상도 필요하지 않다.

### 새 이벤트

`runtime_state`, `control_receipt`, `snapshot_marker`, `snapshot` 네 가지가 추가됐다.
모두 기능으로 게이트되며, 그 기능을 협상하지 않은 연결에는 서버가 보내지 않고 SDK도 받은
뒤 버린다. 기존 연결의 바이트 수준 동작은 바뀌지 않는다.

### 아직 연결되지 않은 부분

`apply_control`/`get_control_receipt`와 `get_snapshot`의 프로토콜 모양과 SDK 메서드는
확정됐지만, 그 뒤의 저장·관측 구현은 별도 작업이 소유한다. 그 작업이 붙기 전까지 브리지는
이 요청들에 `unsupported`로 답한다. 클라이언트 입장에서는 "이 서버가 아직 제공하지 않는다"와
같은 응답이므로 분기 처리가 달라지지 않는다.

## 핸드오프 압축

`compact`는 선택 필드 `method`를 받는다. 없으면 클라이언트가 늘 요청해 온 모델 요약이고,
응답 `compacted`는 요청이 접수됐다는 뜻이다. 데몬은 다음 안전 지점에서 요약하기 때문이다.

`method: "handoff"`는 그 대신 생성한 핸드오프 문서를 경계로 쓴다. 응답은 그 쓰기가 끝난
뒤 도착하며, 접수 확인이 아니라 결과다.

요청:

```json
{"id": 4, "req": "compact", "session_id": "s-1", "method": "handoff"}
```

응답은 같은 `compacted` 이벤트에 `handoff`가 붙은 모양이다.

```json
{"id": 4, "event": "compacted", "session_id": "s-1", "message": "Committed a handoff document covering the first 120 messages.",
 "handoff": {"outcome": "committed", "covered": 120}}
```

| `handoff.outcome` | 뜻 |
| --- | --- |
| `committed` | 문서가 경계가 됐다. `covered`는 덮은 메시지 수이고 최근 토큰 꼬리는 원문으로 남는다 |
| `nothing_to_cover` | 모든 메시지가 최근 토큰 예산 안에 들어가 문서를 쓰지 않았다 |
| `provider_error` | 완성 요청이 실패했다. `detail`에 프로바이더 메시지가 온다. 대화록은 그대로다 |
| `cancelled` | 응답 전에 생성이 취소됐다. 대화록은 그대로다 |

이 필드를 게이트하는 기능은 없다. 이 필드가 없던 서버는 요청의 모르는 필드를 무시하고
`handoff` 없이 응답하므로, `method`를 보내는 클라이언트는 `handoff`가 빠진 응답을
`nothing_to_cover`가 아니라 "이 서버가 지원하지 않음"으로 다뤄야 한다. 서버가 모르는 값은
`invalid_request`로 거절한다.

요청 자체에는 취소 신호가 없다. 완성 요청 한 번이 결과를 정하므로, 기다리기를 그만둔
클라이언트가 있어도 생성은 계속된다. 이미 커밋된 경계를 되돌리지는 않는다.

## 도구 인자 복구(프로토콜 1.16)

프로바이더의 JSON 작성기는 때때로 도구 인자의 타입을 틀리게 씁니다. 숫자가
들어갈 자리에 `"5000"`을, 불리언 자리에 `1`을 보내는 식입니다. 디스패처는
도구가 실행되기 전에 그것을 복구하고, 이제 그 복구를 세 가지 방법으로 볼 수
있습니다. 여기서 추가된 것은 모두 덧붙이는 변경입니다. 이전 클라이언트는 예전과
같은 요청을 보내고 예전과 같은 이벤트를 읽습니다.

### 이벤트: `tool_args_repaired`

해당 호출의 `tool_done` 바로 앞에, 복구된 호출마다 한 번 보냅니다. 이 이벤트를
모르는 클라이언트는 무시하며, 호출은 어느 쪽이든 끝까지 진행됩니다.

```json
{"ev":"tool_args_repaired","session_id":"s-1","call_id":"call-9","name":"bash",
 "original_input":{"command":"ls","timeout":"5000"},"changes":["coerced `timeout` to a number"]}
```

`original_input`은 모델이 보낸 그대로의 인자이고, `changes`는 복구를 적용한
순서대로 한 줄에 하나씩 적은 목록입니다.

### 저장되는 기록

저장된 도구 결과는 같은 두 값을 `original_input`과 `arg_repairs`로 함께
보관합니다. 복구가 필요 없었던 호출에서는 두 값을 생략하므로, 나머지 기록은
이 변경 전과 정확히 같은 바이트를 유지합니다.

### 카운터

`HORON_AGENT_PERF_COUNTERS=1`일 때 `snapshot().tool_arg_repairs`가 도구별
복구 호출 수를 셉니다.

## 세션 이벤트 저장소와 복제(프로토콜 1.12)

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

### 헤드

세션은 되감기나 분기된 가져오기 뒤에 헤드를 여러 개 보유할 수 있다. 선택된 헤드는
**호스트 로컬**이다. 복제되지 않고 세션의 영구 정체성에도 속하지 않는다.
`list_session_heads`는 헤드와 현재 선택을 보고하며, 여러 헤드 중 명시적 선택이
필요하면 `selected_head`가 없다. 하나를 선택할 때까지 실행을 거부하고, 다른
프로세스가 쓰기 권한을 쥐고 있으면 `select_session_head`가 `busy`로 응답한다.
클라이언트는 사용자의 다음 턴이 무엇에서 이어지는지를 바꾸므로, 몰래 하나를
고르지 말고 그 선택을 사용자에게 보여 줘야 한다.

### 기록

`get_history`는 바뀌지 않았고 여전히 보이는 전체 대화 기록을 돌려준다.
`get_history_page`는 긴 세션에서 UI가 원하는 제한 읽기다. `session_id`, 선택적
`head_event_id`(기본값은 선택된 헤드), 불투명 이어가기 값 `before_event_id`,
`limit`(기본 50, 최대 200)을 받는다. 응답은 고정된 `head`, 페이지 메시지를 오래된
것부터 담은 목록, `before_event_id`로 되돌려 넘길 `next_cursor`를 가진
`SessionHistoryPage`다. 사용자가 스크롤하는 동안 `head_event_id`를 고정한다. 헤드를
고정한 커서는 선택이 바뀐 뒤에도 같은 기록을 가리킨다.

### 복제

`export_session_events`는 고정 헤드에 대한 원본을 부모 우선으로 한 페이지씩 제한해
돌려준다. `import_session_events`는 커밋 전에 페이지 전체를 검증하고
`accepted_ids`, `heads`, `retry_cursor`, `conflict_heads`로 응답한다. 원본 blob은
1 MiB 청크로 따로 이동하며 발행 전에 SHA-256과 크기로 검증한다. 중단된 전송은
`get_session_blob_status`에서 이어 간다.

### Markdown 대화록

`export_session_markdown`은 `session_id`를 받아 읽을 수 있는 대화록 전체를
`markdown`에 담은 `session_markdown` 이벤트로 응답한다. 각 턴은 `### User` 또는
`### Assistant` 제목과 그 내용으로 이루어진다. 텍스트는 그대로, 도구 호출은 JSON
코드 블록으로, 도구 결과는 코드 블록으로, 이미지와 오디오 블록은 짧은 자리표시자로
표시한다. 비밀 값은 디스크 경로가 쓰는 같은 패턴 표로 가리므로, 이벤트 로그에
들어가지 않은 토큰은 대화록에도 나타날 수 없다. 4 KiB를 넘는 도구 결과는 UTF-8
경계에서 잘리고 `… (truncated, N bytes total)` 표시로 바뀐다.

이 응답은 복제 프로토콜에 속하지 않으므로 `protocol_version`이 필요 없다.
다른 저장 세션 읽기를 이미 막는 `session_replication_v1` 기능을 요구한다. 없는
세션에는 다른 읽기와 같은 `unknown_session` 오류로 답한다.
`horon-agent export <session-id> [--out <path>]`는 두 경로가 같은 렌더러를 쓰므로
같은 바이트를 쓴다.

클라이언트가 어겨서는 안 되는 규칙 두 가지:

- **`canonical_json`을 다시 직렬화하지 않는다.** 이벤트의 정규 JSON은 문자열로
  이동한다. 2^53을 넘는 `u64`, `1.0`, `-0.0`이 JavaScript 숫자 왕복을 견디지
  못하기 때문이다. 문자열을 그대로 통과시킨다.
- **`conflict_heads`를 데이터로 다룬다.** 분기된 헤드는 모두 보존된다. 타임스탬프로
  승자를 정하지 않으며, 가져오기는 사용자가 고르지 않은 분기를 버리면 안 된다.

가져오기를 다시 보내는 것은 안전하다. 같은 ID에 같은 해시면 아무 일도 일어나지
않는다.

### 오류

안정된 `ErrorCode` 값 다섯 개는 바뀌지 않았으므로 구버전 클라이언트도 계속 해독한다.
더 자세한 원인은 선택적 `details` 객체로 온다. `store_code`(예: `hash_conflict`,
`missing_dependency`, `conflict_heads`, `busy`, `invalid_cursor`,
`record_too_large`), `retryable`, `accepted_ids`, `retry_cursor`,
`conflict_heads`가 여기에 들어간다. `details`를 쓰지 않으면 무시한다. 재시도 전에
`retryable`로 분기하고, `hash_conflict`는 바꾸지 않은 채 절대 재시도하지 않는다.

### 상태

`runtime_info`는 현재 호스트 런타임에서 생존 여부를 구성한다. 오래된 세션
메타데이터에 기록된 PID는 과거 보조 정보일 뿐 살아 있는 프로세스가 아니며, 어떤
클라이언트도 이를 실행 중으로 표시하면 안 된다.

### 런타임 식별 필드

`get_runtime_info`는 세션의 식별 정보를 답한다. 그중 두 필드는 한 턴을 답한 모델이
세션이 고른 모델과 항상 같지는 않다는 사실 때문에 있다.

| 필드 | 타입 | 의미 |
|---|---|---|
| `fallback_models` | `array<string>` | 활성 이름 지정 공급자 프로필에 설정된 폴백 모델 ID이며, 한 턴이 시도하는 순서다. 목록이 비어 있으면 키 자체가 없다. |
| `serving_model` | `string` | 가장 최근 어시스턴트 응답을 만든 모델이다. 폴백이 답한 뒤에는 세션 자체 선택으로 돌아간 `model`과 값이 다르다. 아직 지목할 응답이 없으면 없다. |

두 필드 모두 선택적이다. 구버전 데몬은 이 필드를 생략하고, 이를 무시하는
클라이언트는 계속 동작한다. 카탈로그와 식별 정보를 한 번 받은 뒤에는, 예비 모델 목록이
없을 때 브리지가 `get_runtime_info`에 캐시로 답한다. 이때 응답한 모델은 세션의 모델뿐이기
때문이다. 예비 모델 목록이 있거나 식별 정보를 아직 받지 못했으면 매 `get_runtime_info`마다
데몬에 카탈로그를 묻는다. 카탈로그가 바뀌지 않아도 응답한 모델이 바뀔 수 있기 때문이다.

전체 저장 형식, 한도, 레거시 마이그레이션 규칙은
`docs/protocol/SESSION_STORE.ko.md`에 있다.

## 하네스 API 요청·이벤트 표면: usage_report, search_sessions, list_artifacts, durable_runtime

이 문서는 하네스 API에서 추가된 요청 표면을 적습니다. 앞부분은 프로토콜 1.13의 세 요청과 그 응답 이벤트이고, 그다음은 1.14의 내구 런타임 표면 넷이고, 마지막은 1.16의 인자 복구입니다. 필드 이름과 타입은 `crates/harness-api/src/requests.rs`와 `crates/harness-api/src/events.rs`의 정의를 그대로 옮겼고, 동작과 한도는 `crates/harness-api-server/src/translate/`의 구현에서 확인한 값입니다.

세 요청 모두 추가형입니다. 이 요청들을 보내지 않는 클라이언트는 계속 동작하고, 브리지는 `hello_ok`의 `capabilities`에 `usage_report`, `session_search`, `artifacts`를 광고합니다(`crates/harness-api-server/src/lib.rs`의 `BRIDGE_CAPABILITIES`).

### 공통 규칙

- 프레임 하나는 JSON 객체 하나, 한 줄입니다(NDJSON). 요청은 `req`로 종류를 구분하고 `id`를 싣고, 응답은 `reply_to`로 그 `id`를 되돌려 줍니다.
- 요청 본문은 `#[serde(tag = "req", rename_all = "snake_case")]`로 직렬화되므로 필드 이름은 snake_case입니다.
- 모르는 필드는 무시하고, 모르는 이벤트 종류는 건너뜁니다.
- 실패는 `{"ev":"error","code":...,"message":...}`로 옵니다. 이 표면들이 쓰는 `ErrorCode` 값은 `invalid_request`, `unknown_session`, `internal`입니다.
- 세션을 지목하는 요청은 `session_id`를 받습니다. 없는 세션은 빈 결과가 아니라 `unknown_session`으로 답합니다. "이 세션은 아무것도 쓰지 않았다"와 "그런 세션이 없다"는 다른 사실이기 때문입니다.
- `session_id`는 6자 이상인 앞부분 일부도 받습니다. 온전한 id가 항상 이기고, 연결이 이미 아는 id는 다시 해석하지 않습니다. 여러 세션이 공유하는 앞부분은 `details.accepted_ids`에 최대 10개의 후보를 담고 `details.store_code`를 `ambiguous_session`으로 둔 `unknown_session`으로 답합니다. 오류 코드 자체는 `unknown_session`으로 남아 이전 빌드의 클라이언트도 응답을 해석합니다. 아무 세션도 해당하지 않는 앞부분은 그대로 `unknown_session`으로 답합니다.

### usage_report

첨부된 세션과 저장된 모든 세션의 토큰 사용량, 컨텍스트 창, 모델별 집계를 돌려줍니다. 한도 필드(`limits`)는 남아 있지만 이 빌드에는 그 출처가 없어 항상 빈 배열입니다. 능력 이름은 `usage_report`입니다.

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
| `limits` | `UsageLimitInfo[]` | 한도 목록. 이 빌드는 한도를 알리는 계정 출처가 없으므로 항상 빈 배열 |
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

`UsageLimitInfo`는 `name`, `used`, `limit`, `unit`(예: `usd`), `resets_at`(RFC 3339)을 가집니다. `used`, `limit`, `resets_at`은 데몬이 알렸을 때만 있습니다.

#### 동작

보고는 세 출처를 권위 순으로 합칩니다. 첨부된 세션의 데몬 누적치, 저장된 세션 원본의 `token_usage` 기록, 마지막 실시간 `token_usage` 이벤트입니다. 어떤 요청도 기록하지 않은 지표는 `0`이 아니라 `null`입니다. 제공자가 캐시 토큰을 보고하지 않은 것과 0을 보고한 것은 다른 사실이기 때문입니다.

이 빌드에는 계정 구독이 없으므로 데몬은 한도를 알리지 않고 `limits`는 항상 빈 배열입니다. 빈 배열은 "이 계정에 한도가 없다"가 아니라 "이 빌드에 한도 출처가 없다"는 뜻입니다.

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
- 프로토콜 버전: `API_VERSION_MAJOR = 1`, `API_VERSION_MINOR = 15`
