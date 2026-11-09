# 세션 저장소

세션 이벤트 로그의 디스크 형식·전송 계약과, 그 위에 쌓이는 영속 런타임 상태 저널을 다룬다.

## 세션 이벤트 저장소: 디스크 형식과 전송 계약

이 문서는 horon-agent가 세션에 무엇을 저장하고, 그 바이트를 어떻게 해시하고 검증하며, 두 인스턴스가
어떻게 복제하고, 레거시 세션을 어떻게 마이그레이션하는지 설명하는 기준 문서다. 권위 있는 타입은
`crates/session-store/src/schema.rs`에 있고, 이 문서는 그 타입을 설명하며 각 규칙을 소유한
모듈을 밝힌다.

저장소의 루트는 하나다. `$HORON_HOME/sessions`(보통 `~/.horon/sessions`)이다. 아래 경로는
모두 이 루트 기준이다. 모든 파일은 소유자 전용이며(디렉터리 `0700`, 파일 `0600`), 모든 접근자는 세션
id를 다시 검증하고 루트 아래에서 심볼릭 링크, 디렉터리가 아닌 요소, 다른 소유자의 요소를 거부한다.
그래서 형식이 올바른 id도 저장소 밖 경로를 가리킬 수 없다(`schema.rs::SessionPaths`). Windows에는
권한 비트와 `O_NOFOLLOW`가 없어 소유자 전용은 홈 디렉터리의 상속 ACL이 맡고, 파일은 연 뒤에 확인한다.
자세한 내용은 [`release/WINDOWS.md`](../release/WINDOWS.md)의 아키텍처 메모에 있다.

### 배치

```
sessions/
  <id>/                      canonical session directory
    events.jsonl             append-only canonical event log (the original)
    blobs/<sha256>.json      externalized payload bytes, named by their own hash
    local-head.json          local original: the selected head, written only for an explicit selection
    local-head-explicit.json local original: whether the head selection is explicit (absent = not explicit)
    local-explicit-source.json local original: path and SHA-256 of the explicit snapshot the session came from
    local-runtime.json       local original: resume binding (working_dir, migration_epoch, provider_session_id, status)
    checkpoint.json          derived cache: head, source stamp, metadata, compaction, tail
    sparse-index.jsonl       derived cache: every 128th event plus message offsets
    summary.json             derived cache: one catalog row for this session
    facade-cache.json        derived cache: session facade payload and its hash
    .writer.lock             session-lifetime exclusive writer lock (never unlinked)
    .blob-staging/<sha256>/  resumable inbound blob chunks
    .record-transfer/        resumable inbound whole-record staging
  <id>.json                  legacy snapshot (preserved, never rewritten)
  <id>.journal.jsonl         legacy journal tail (preserved, never rewritten)
  <id>.bak                   legacy recoverable snapshot (preserved)
  .catalog.index.jsonl       derived cache: one row per session, for listing
  .catalog.lock              catalog write lock
  .catalog.dirty/<id>        rebuild hint: empty file, one per session being rebuilt
                             (created before the originals, never flushed)
  .origin-id                 this HOME's writer identity, created once at 0600
  edit-stats/                per-session edit counters (not a session)
```

세션 내용의 원본은 `events.jsonl`과 `blobs/`다. 여기에 이벤트 로그로 다시 만들 수 없는 로컬 상태
파일 네 개가 원본으로 더해진다. `local-head.json`은 선택이 명시적일 때만 기록하고,
`local-head-explicit.json`은 그 선택 여부를, `local-explicit-source.json`은 세션을 만든 명시
스냅샷의 경로와 해시를, `local-runtime.json`은 재개에 쓰는 `working_dir`, `migration_epoch`,
`provider_session_id`와 표시용 `status`를 담는다. `local-runtime.json`은 이 필드 가운데 하나가
바뀔 때만 다시 쓰고, `last_pid`와 `last_active_at`만 움직인 배치는 파일을 쓰지 않는다.
`status`, `last_pid`, `last_active_at`의 원본은 `local-runtime.json` 하나다. 이벤트 로그의 보조 기록
(`AuxiliaryRecorded`)에는 이 값들을 넣지 않고, 로더도 바인딩에서 되살린다. 그래서 실행 끝의 닫기처럼
상태만 바뀐 저장은 바인딩만 확정하고 로그에는 아무것도 덧붙이지 않는다(`event_save.rs`의 바인딩 전용 경로).

`local-head-explicit.json`이 없으면 모든 리더(`event_store.rs`, `event_cache.rs`, `archive.rs`,
`history_page.rs`, 복제 `replication.rs`)가 "명시 선택 없음"으로 읽는다. 그래서 `local-head.json`과
플래그가 둘 다 없는 세션에 `false`를 쓰지 않는다(`event_store.rs::select`). 명시 선택을 한 번도
하지 않은 새 세션은 첫 커밋에서 파일 flush 한 번을 덜 내고, 크래시나 전원 차단 뒤에도 "파일 없음"과
"`false`"는 같은 상태로 읽히므로 잃는 것이 없다. 한 번이라도 명시 선택을 했던 세션은 플래그가
남아 있으므로 해제할 때 지금처럼 `false`를 기록한다.

원본은 `sync_file`과 `sync_dir`로 확정한다. `checkpoint.json`, `sparse-index.jsonl`, `summary.json`,
`facade-cache.json`과 `.catalog.index.jsonl`은 파생물이다. 파생물은 임시 파일에 쓰고 rename만 하며
flush하지 않으므로, 크래시는 재생성 비용만 낸다. 스탬프나 sha256 검사에 실패한 파생 파일은 writer
락 아래에서 다시 만든다. 원본과 파생의 경계, 재생성 조건, `[storage] sync`는
[`../architecture/CRATES.md`](../architecture/CRATES.md)의 "저장 내구성은 core 정책이고 flush 호출만 OS에 맡긴다"에
있다. `doctor`는 원본의 손상을 보고하며 고치지는 않는다.

새로 만든 폴더는 그 항목을 부모 폴더에 확정해야 정전 뒤에도 남는다. 세션 writer는 `sessions/`
루트를 열 때 루트를 새로 만든 경우에만 부모를 flush한다(`writer.rs`의 `ensure_dir`). 이미 있는
루트의 항목은 만들 때 확정됐으므로, 세션을 열 때마다 부모를 다시 flush하지 않는다.

`.catalog.dirty/<id>` 표식은 내용이 없는 빈 파일이다. 리더는 바이트가 아니라 존재만 검사한다
(`cache_io.rs::dirty_present`). 표식은 "이 세션의 파생물을 곧 다시 만들어야 한다"는 힌트이고, 원본이
아니다. 그래서 writer는 표식을 원본보다 먼저 만들지만 fsync하지 않는다.

- 표식은 그 묶음의 첫 원본 append 전에 만든다. 만들기가 실패하면 append는 오류로 돌아가고 이벤트
  로그에는 바이트가 들어가지 않는다(`IoPoint::DirtyCreated`).
- 표식 파일과 그 디렉터리 항목은 flush하지 않는다. 커밋마다 내던 디렉터리 fsync 한 번이 이 결정으로
  없어졌다(원샷 질문 1건 기준 약 5회).
- 같은 묶음 안에서 한 번 만든 표식은 그대로 재사용한다.

**fsync를 빼도 되는 근거.** 표식이 없어도 낡은 파생물을 믿는 경로가 없다. 파생물은 모두 원본 이벤트
로그의 `Stamp`(device, inode, 길이, mtime, ctime)를 함께 저장하고, 읽을 때마다 지금 로그와 대조한다.
체크포인트는 `checkpoint.rs::load`가 로그·색인·요약의 스탬프를, 목록 행은 `catalog.rs::list`가 행의
스탬프와 `summary.json`을 확인한다. 원본 append는 로그를 늘리므로 스탬프가 반드시 바뀌고, 맞지
않는 파생물은 writer 락 아래에서 원본으로 다시 만든다. 로그는 저장이 끝나기 전에 flush되므로,
전원이 나가 표식이나 표식 디렉터리가 사라져도 남는 것은 "늘어난 로그 + 낡은 스탬프의 파생물"이고,
그 조합은 언제나 재생성으로 이어진다. 비용은 다음 조회의 재생성 한 번이다.

이 근거는 테스트가 고정한다(`tests/dirty_batches.rs`).

| 테스트 | 흉내 내는 상황 | 확인하는 것 |
| --- | --- | --- |
| `an_empty_marker_left_by_a_crash_repairs_the_next_listing` | 같은 프로세스에서 파생 게시 직전에 멈춤 | 다음 목록이 원본을 읽어 행을 고친다 |
| `a_marker_lost_to_power_loss_never_serves_a_stale_listing_or_context` | 전원 차단으로 flush하지 않은 표식, 또는 표식 디렉터리 전체가 사라짐 | 목록 head와 대화 문맥이 둘 다 최신 원본까지 오고, 다음 저장도 정상이다 |
| `a_writer_killed_inside_a_save_leaves_originals_the_next_reader_serves` | 저장 묶음 안에서 writer 프로세스를 `abort`로 죽임(표식을 남긴 경우와 지운 경우) | 죽은 저장의 원본이 로그에 남고 리더가 그것까지 돌려준다 |

이전 계약은 표식을 원본으로 보고 디렉터리 fsync로 확정했다. 위 스탬프 대조가 이미 같은 보장을 주고
있었고, 이 문서도 "그 항목을 잃어도 비용은 목록 행 하나를 다시 만드는 것"이라고 적고 있었으므로
fsync는 중복이었다. 가져오기(`legacy_publish.rs`)가 만드는 표식은 원본이 나타나기 전의 순서
표시라 지금처럼 flush한다.

한 묶음 커밋은 로그 flush를 한 번만 낸다. 묶음 안의 append는 원본 바이트를 다 쓰고도 flush를
미루고, 묶음 끝의 `flush_pending_log`가 그 파일을 한 번에 확정한다. `local-head.json`을 비롯한
선택 상태 파일은 그 flush가 성공한 뒤에 쓴다. flush가 실패하면 저장은 오류로 돌아가고 선택
상태는 남지 않으므로, 같은 이벤트를 다시 보내면 각 이벤트가 `events.jsonl`에 한 번만 들어간다.
블롭은 지금처럼 블롭마다 flush한다.

`edit-stats/`는 세션이 아닌 장부를 담는 루트의 예약된 자식이고, 카탈로그는 이를 명시적으로
건너뛴다(`reader.rs::EDIT_STATS_DIR`).

### 이벤트 레코드 하나

`events.jsonl`은 줄마다 정규 JSON 객체 하나를 담고, 줄바꿈으로 끝난다
(`schema.rs::EventEnvelope`).

| 필드 | 의미 |
| --- | --- |
| `schema_version` | 레코드 스키마 버전, 현재 `1` |
| `session_id` | 검증된 세션 식별자. ASCII 문자, 숫자, `.`, `-`, `_`만 허용하고 문자나 숫자로 시작해야 하며 점으로 시작하거나 끝날 수 없고 최대 160자이며 Windows 장치 이름은 안 된다 |
| `event_id` | UUIDv4 문자열, 어떤 I/O보다 먼저 예약한다 |
| `parent_event_id` | 이 분기의 이전 이벤트. 루트에만 없다 |
| `origin_id` | 설치된 HOME의 UUID. 사용자가 아니라 writer를 식별한다 |
| `timestamp` | UTC RFC3339, 나노초 정밀도. 해시에 포함되므로 신원의 일부다 |
| `kind` | 이벤트 종류 문자열 |
| `payload` | 인라인 JSON 본문, `payload_ref`와 상호 배타 |
| `payload_ref` | 외부화한 본문의 `{sha256, size, encoding}` |
| `content_hash` | 다른 모든 필드에 대한 SHA-256 |

그 줄은 정규 JSON이다. 공백이 없고, 객체 키는 UTF-8 바이트 순서로 재귀 정렬하며, 배열 순서는
유지한다(`schema.rs::canonicalize_value`, `canonical_json`). `content_hash`는 `content_hash`를 비운
봉투에 대해 계산하고, `timestamp`는 항상 같은 고정 형식으로 직렬화한다. 그래서 같은 시각이 다르게
해시될 수 없다.

**외부화는 해싱보다 먼저 일어난다.** 인라인 레코드가 레코드당 8 MiB 한도(`MAX_RECORD_BYTES`)를 넘을
페이로드는 `blobs/<sha256>.json`에 쓰고 `payload_ref`로 대체한다. 그다음 레코드를 그 최종 형태로
해시한다(`schema.rs::EventDraft::seal`). 따라서 인라인 레코드와 blob 참조 레코드는 서로 다른
신원이고, 리더는 어느 쪽을 보고 있는지 추측할 필요가 없다.

디코딩은 다음 순서로 검증한다. 줄이 파싱되는가, 버전이 지원되는가, 페이로드 표현이 둘 중 정확히
하나인가, `payload_ref`의 해시/크기/인코딩이 유효한가, 타입 지정 페이로드가 그 종류와 맞는가,
`content_hash`가 다시 계산한 값과 같은가. 이 중 하나라도 실패한 레코드는 건너뛸 대상이 아니라
손상이다.

### 이벤트 종류

이 빌드는 여덟 종류를 투영한다. 알 수 없는 종류는 그대로 보존하고, 투영이 해석해야 하는 지점에서는
거부한다(`schema.rs::KnownEventKind`).

| 종류 | 페이로드 | 의미 |
| --- | --- | --- |
| `session_created` | `SessionCreatedPayload` | 완전한 내구 메타데이터. `branch_root`/`reset_provenance`는 명시적 리셋 루트를 표시한다 |
| `message_appended` | `MessageAppendedPayload` | 보이는 메시지 하나 |
| `message_revised` | `MessageRevisedPayload` | 기존 append의 대체. 이벤트 id로 지목한다 |
| `metadata_changed` | `MetadataChangedPayload` | 완전한 소형 메타데이터. 델타가 아니다 |
| `compacted` | `CompactedPayload` | 요약. 그것이 덮는 원본 범위에 묶인다 |
| `compaction_blocked` | `CompactionBlockedPayload` | 압축이 거부되거나 멈춘 이유 |
| `archive_changed` | `ArchiveChangedPayload` | 숨김/숨김 해제. 아카이브는 삭제하지 않는다 |
| `auxiliary_recorded` | `AuxiliaryRecordedPayload` | 환경 스냅샷, 메모리 주입, 재생 이벤트, 보존된 레거시 보조 데이터 |

`SessionMetadata`는 메시지, 압축, 환경 스냅샷, 메모리 주입, 재생 이벤트, 모든 런타임 캐시와 PID,
소켓 필드를 의도적으로 뺀다. 알 수 없는 옛 키는 `legacy_extensions`에 남고 런타임 상태로
되살아나지 않는다.

### 분기와 헤드

로그는 불변 부모 그래프다. `EventGraph`는 열 때 검증한다. 부모가 존재해야 하고, id가 고유해야
하며, 순환과 충돌하는 신원은 거부하고, 같은 레코드는 중복 제거한다(`heads.rs::EventGraph`). 헤드는
자식이 없는 이벤트다.

- 선택된 헤드는 **호스트 로컬**이며 인스턴스 사이에 교환하지 않는다.
- 헤드가 하나면 자동으로 실행한다. 여럿이면 헤드를 명시적으로 고를 때까지 실행을 거부한다.
- 리비전은 `message_revised`를 덧붙인다. 되감기는 이전 헤드를 골라 새 분기를 덧붙인다. `clear`는
  리셋 출처를 가진 새 빈 분기 루트를 시작한다. 어떤 연산도 원본을 삭제하지 않는다.

### 압축 레코드

`CompactedPayload`는 재개 계약이고, 모든 필드는 낡았거나 부분적인 요약을 찾아낼 수 있게
존재한다(`schema.rs::CompactedPayload`).

| 필드 | 의미 |
| --- | --- |
| `summary_text` | 사람용 요약. 유효한 프로바이더 네이티브 윈도에서는 비어 있어도 된다 |
| `source_head_id` | 작업이 시작될 때의 분기 헤드 |
| `covered_through_event_id` | 요약이 실제로 덮은 마지막 원본 이벤트 |
| `source_prefix_digest` | 덮은 접두부의 정규 다이제스트 |
| `first_kept_event_id` | 요약하지 않고 꼬리로 남긴 첫 이벤트 |
| `prior_compaction_id` | 앞서 적용한 압축. 증분 요약에 쓴다 |
| `provider` / `model` / `route` / `settings_digest` / `generation` | 무엇이 어떤 설정으로 만들었는가 |
| `input_tokens` / `output_tokens` | 사용량, 알 수 있을 때 |
| `outcome` | `summary`, `native`, `lossy_emergency` |
| `native_window` | 프로바이더 네이티브 윈도 항목, 순서대로, 통째로 보존 |
| `restoration` | 한도가 있는 `{path, kind, note}` 참조와 바이트 예산. 파일 본문은 아니다 |

진행할 수 없는 압축은 요약이 성공한 척하지 않고 이유와 함께 `compaction_blocked`를 덧붙인다
(`context_recovery_blocked`, `context_budget_too_small`,
`provider_error`, `overflow_no_progress`, `stale_source`). 길이 알림이나 빈 구조 요약은 성공한
`summary`로 기록하지 않는다. `summary_text`가 빈 유효한 네이티브 윈도는 기록한다.

압축을 소유하는 전략은 `[compaction]` 아래 `strategy`다. `structured`(기본), `openai_auto`,
`openai_explicit` 중 하나다. `[provider] openai_native_compaction_mode`를 `auto`/`explicit`로 명시해
둔 경우 `strategy`가 설정되지 않았을 때만 존중한다. 없음, 빈 값, `off`는 모두 `structured`로
해석한다. 같은 절의 예산 기본값은 `recent_tokens` 20,000, `reserve_tokens` 16,384를
`min(window * 0.04, 49,152)`까지 올린 값, 그리고 16k에서 0.45부터 512k 초과에서 0.80까지 가는 윈도
등급 트리거 비율이다(`crates/compaction-core/src/lib.rs`).

구조화 요약을 만드는 모델은 같은 절의 `summary_model`로 바꿀 수 있다(환경변수
`HORON_AGENT_COMPACTION_SUMMARY_MODEL`). 비우거나 세션 모델과 같으면 세션 모델을 쓴다. 값이 있으면
세션 프로바이더를 fork해 그 모델로 요약을 요청하고, 그 호출이 프로바이더 오류로 실패하면 세션 모델로
한 번 다시 요청한다. 프로바이더가 모델을 거부하면 경고만 남기고 세션 모델을 쓴다. 프로바이더 고유
압축 경로에는 적용되지 않고, 잡 정체성(경로 식별자와 설정 다이제스트)에도 들어가지 않는다
(`crates/base/src/compaction/summary.rs` `generate_structured_summary_with_model`).

### 파생 캐시

- `checkpoint.json`은 선택된 헤드, 소스 스탬프(로그 epoch/inode, 커밋된 오프셋, 앵커 해시), 현재
  메타데이터, 마지막 압축, 보존된 꼬리를 담는다. 정상 append는 접두부를 다시 쓰지 않는다.
- `sparse-index.jsonl`은 128번째 이벤트마다와 메시지 오프셋을 담는다. 그래서 오래된 이벤트를 전체
  재생 없이 바이트 범위로 가져온다.
- `summary.json`과 `.catalog.index.jsonl`은 목록 행이다. 목록 조회는 트랜스크립트를 파싱하지
  않는다.
- `facade-cache.json`은 세션 파사드 레코드와 그 해시를 담는다. 크기 상한을 넘는 세션은 캐시를
  두지 않고 원본에서 다시 만든다.
- 캐시 적중은 `checkpoint.json`과 꼬리만 읽는다. 크기 감소, inode/epoch 변경, 앵커 불일치, 같은
  길이 편집이면 전체 재생을 강제한다. 변하지 않은 접두부 안의 임의 변조를 찾는 일은 `doctor`의
  몫이고, 웜 연결은 접두부 전체를 해시한다고 주장하지 않는다.

### 락과 writer 풀

세션마다 `BlockingWriter` 하나가 세션 수명 동안 `.writer.lock`에 `flock`을 잡는다. 외부 프로세스를
포함한 두 번째 writer는 `busy`로 답한다. 카탈로그 쓰기는 `.catalog.lock`을 잡고, 세션 락을 잡는
동안 카탈로그 락을 쥐지 않는다. 더러운 세션은 그 리더가 다시 만들므로, 실패한 캐시 갱신보다 낡은
목록이 오래 살지 못한다.

커밋 I/O는 세션마다 전용 스레드를 만들지 않는다. 프로세스 전역 writer 풀이
`clamp(available_parallelism, 2, 4)`개의 워커(`session-writer-0`…)를 한 번 만들고, 세션은
`registry_key`(정규화한 세션 디렉터리 경로)의 해시로 워커 하나에 고정 배정된다. 같은 세션 디렉터리의
두 표기가 서로 다른 워커로 가서 `BlockingWriter`를 두 개 여는 일이 없도록 키를 먼저 정규화한다.
해셔는 프로세스 내내 같은 값을 내므로 한 세션은 수명 동안 같은 워커에 남는다.

풀은 이전 구조의 계약을 그대로 지킨다.

- **제출 순서**: 한 세션의 job은 제출한 순서대로 그 워커에서 실행된다. `call()`은 job이 끝날 때까지
  응답 랑데부로 기다리므로 호출부 시그니처와 블로킹 의미가 바뀌지 않는다.
- **락 수명**: `.writer.lock`은 워커의 세션 항목이 들고 있는 `BlockingWriter`가 쥔다. 세션 항목은
  마지막 파사드의 drop이 큐에 넣은 `Close`가 도착할 때 사라지므로, drop은 앞선 job을 모두 drain한
  뒤에 락을 놓는다. 항목에서 `BlockingWriter`를 슬롯 리스보다 먼저 선언해, 같은 세션을 다시 열려는
  대기자는 락이 풀린 뒤에 깨어난다.
- **panic 격리**: job은 `catch_unwind`로 감싼다. panic하면 그 세션 항목만 제거되어 락이 풀리고,
  같은 워커의 다른 세션은 계속 커밋한다. 그 세션의 대기 호출자는 응답이 사라져 "reply lost" 오류를
  받고, 프로세스는 살아 있다.
- **재진입**: 워커 스레드에서 자기 writer를 다시 `call`하거나 drop하면 교착 대신 오류가 된다.
  열기 경로도 워커에서 호출되면 같은 이유로 오류를 돌려준다.
- **머리 막힘**: 저장 하나가 이벤트 로그를 세 번 읽으므로 큰 세션의 저장은 그 워커를 수백 ms 붙잡을
  수 있고, 같은 워커에 배정된 다른 세션의 저장은 그만큼 기다린다. 다른 워커의 세션은 영향받지 않는다.
  이것은 스레드 수를 세션 수에서 떼어 낸 대가이며, 세션당 스레드·RSS 기울기를 재는 측정에서 함께
  본다.

### 복제 전송

두 인스턴스는 **원본만** 교환한다. 정규 이벤트 레코드와 원본 blob이다. 체크포인트, 인덱스, PID,
리스, 로컬 헤드 선택과 로컬 런타임 바인딩은 보내지 않고, 로컬 파일 오프셋을 원격 커서로 쓰지 않는다
(`crates/session-store/src/replication*.rs`).

- `protocol_version`은 `1`이다. 페이지는 이벤트를 최대 256개(`MAX_PAGE_EVENTS`), 알려진 id를 최대
  256개 나르고, 실제 이스케이프된 전송 바이트 16 MiB(`MAX_WIRE_BYTES`)로 제한된다.
- `CanonicalEventRecord { event_id, content_hash, canonical_json }`은 정규 이벤트 JSON을 **문자열**로
  나른다. 클라이언트는 이것을 파싱해 다시 직렬화하면 안 된다. 2^53을 넘는 `u64`, `1.0`, `-0.0`은
  JavaScript 숫자 왕복에서 살아남지 못한다. Rust 저장소는 문자열을 파싱하고 정규화하고 검증하며
  바깥 id와 해시에 대조한다.
- 내보내기는 고정 헤드의 부모/참조 폐쇄를 부모 우선으로 순회한다. 첫 페이지가 헤드를 고정한다.
  다른 헤드를 가진 뒤 페이지나 그 폐쇄를 설명하지 않는 커서는 `invalid_cursor`다.
- 커서는 스스로를 검사하는 base64url 객체다. 버전, 세션, 헤드, 폐쇄 다이제스트, 다음 정규 위상
  인덱스를 담는다. 부모 동률은 이벤트 id로 깬다.
- 가져오기는 아무것도 커밋하기 전에 페이지 전체를 검증한다. 스키마, 해시, id, 경로, 부모, blob과
  압축 참조다. 같은 id와 해시는 무동작이고, 같은 id에 다른 해시는 `hash_conflict`, 빠진 부모나
  blob은 `missing_dependency`다. I/O 실패는 동기화된 접두부를 남길 수 있다. 페이지가 원자 묶음이
  아니기 때문이다. `accepted_ids`와 `retry_cursor`가 다음에 무엇을 보낼지 알려 주고, 다시 보내도
  멱등이다.
- 갈라진 헤드는 모두 보존하고 `conflict_heads`로 보고한다. 타임스탬프로 이기는 쪽을 정하지 않는다.
- 쓰기가 활발한 세션에 대한 가져오기는 `busy`로 답하고 나중에 재시도한다. 실행 중인 세션의 상태를
  뒤에서 바꾸지 않는다.
- Blob은 1 MiB 연속 청크(`BLOB_CHUNK_BYTES`)로 움직이고, 공개 전에 SHA-256과 크기를 검증한다.
  청크는 `.blob-staging/<sha256>/<offset>.chunk`와 `size` 매니페스트로 준비하므로, 중단된 전송은
  첫 빠진 오프셋에서 재개한다. 같은 오프셋에 같은 바이트는 멱등이고, 다른 바이트는 `conflict`,
  내구 접두부를 넘는 오프셋은 `missing_offset`이다. blob 참조는 바이트가 검증되기 전에 가져오지
  않는다.
- 정규 레코드 자체가 전송에 인라인으로 싣기 너무 크면 페이지는 그것을 논리 위치의 `record_refs`에
  올리고, 바이트는 `RecordBlobChunk` 값으로 `.record-transfer/`를 통해 움직인다. 이것은 전송
  메커니즘일 뿐이고 저장된 로그는 다시 쓰지 않는다.

### 하네스 API 표면(프로토콜 1.12)

저장소는 하네스 API로 접근한다. 주 버전은 `1`에 머물고 부 버전이 11에서 12로 올랐다. 클라이언트는
핸드셰이크를 협상한 뒤 무엇이든 보내기 전에 필요한 능력을 확인한다.

| 능력 | 게이트하는 것 |
| --- | --- |
| `session_event_store_v1` | `list_session_heads`, `select_session_head` |
| `session_replication_v1` | `export_session_events`, `import_session_events`, `export_session_blob`, `import_session_blob`, `get_session_blob_status`, 그리고 레코드 blob 메서드 |
| `session_history_cursor_v1` | `get_history_page` |
| `structured_compaction_v1` | `compacted` 레코드의 구조 요약 필드 읽기. 이 능력으로 게이트되는 메서드는 없다 |

네 가지 모두 브리지 자체가 광고하며(`crates/harness-api-server/src/lib.rs`의
`BRIDGE_CAPABILITIES`), 데몬에서 탐지하지 않는다. 이 빌드의 저장소와 압축 엔진이 무엇을 내는지
설명하는 값이다. 앞의 셋은 SDK 메서드도 게이트하므로, 능력 없이 그중 하나를 보내는 클라이언트는 그
이름을 담은 `unsupported`를 받는다. 넷째는 어떤 메서드도 게이트하지 않는다. 클라이언트가
`compacted` 페이로드를 해석하기 전에 읽을 수 있는 신호이고, 그래서 그것 때문에 거부되는 SDK 호출은
없다.

새 요청은 `ListSessionHeads`, `SelectSessionHead`, `GetHistoryPage`, `ExportSessionEvents`,
`ImportSessionEvents`, `ExportSessionBlob`, `ImportSessionBlob`, `GetSessionBlobStatus`, 그리고 레코드
전체 쌍인 `ExportSessionRecordBlob` / `ImportSessionRecordBlob`과 `GetSessionRecordBlobStatus`다.
응답은 `SessionHeads`, `SessionHistoryPage`, `SessionEventPage`, `SessionImportResult`,
`SessionBlobChunk`, `SessionBlobStatus`, `SessionRecordBlobStatus`다.

`GetHistory`는 뜻이 그대로다. 보이는 트랜스크립트 전체다. `GetHistoryPage`는 새로 생긴 한도 있는
읽기이고, `head_event_id`(선택된 헤드가 기본), 불투명 이어가기인 `before_event_id`, 기본 50에 상한
200인 `limit`을 받는다. 페이지 안에서 메시지는 오래된 것부터 돌아오고, 고정 헤드 커서는 선택이
바뀐 뒤에도 같은 이력을 계속 가리킨다.

옛 클라이언트도 계속 동작한다. 새 요청을 요구하지 않고, 기존 `ErrorCode` 값
(`unsupported_version`, `unknown_request`, `unknown_session`, `invalid_request`, `internal`)은
그대로이며, 더 세밀한 원인은 옛 클라이언트가 그냥 무시하는 선택적 `details` 객체(`store_code`,
`retryable`, `accepted_ids`, `retry_cursor`, `conflict_heads`)로 간다. `store_code` 값은
`crates/session-store/src/error.rs`의 `StoreErrorCode`에서 온다(예: `hash_conflict`,
`missing_dependency`, `conflict_heads`, `busy`,
`invalid_cursor`, `record_too_large`, `migration_required`). `unknown_session`은 존재하지 않는
세션에만 쓴다. `busy`와 I/O 실패는
`internal`로 매핑하고, 재시도가 도움이 될 때 `retryable`을 설정한다.

상태는 과거 PID가 아니라 현재 호스트 런타임에서 조립한다. 저장된 PID는 레거시 보조 데이터이고,
어떤 리더도 그것을 라이브 상태로 되살리지 않는다.

### 레거시 배치와 마이그레이션

옛 빌드가 쓴 세션은 `sessions/<id>.json`과 선택적 `sessions/<id>.journal.jsonl`로 존재하고,
`sessions/<id>.bak`이 복구 가능한 스냅샷이다. 두 배치는 열거 규칙 하나로 찾고(`reader.rs`), 배치는
세션마다 보고한다.

마이그레이션은 오프라인이고 비파괴적이다(`legacy.rs`).

- 레거시 바이트를 현재 상태로 재생한다. `.bak` 복구와 저널 꼬리를 포함하고, 알 수 없는 메타데이터와
  보조 데이터는 보존한다.
- 소스를 전후로 해시하고, 살아 있는 레거시 writer는 거부하며, 준비 사본이 새 로그와 소스 해시
  매니페스트를 나른다. 동기화와 검증이 끝난 뒤에만 공개한다.
- 옛 파일, SQLite 데이터베이스, WAL, SHM, `sdk-archive.json`, 할 일 상태, 편집 통계는 삭제하거나
  고치지 않는다.
- 같은 소스를 다시 마이그레이션하면 결정적 무동작이다(결정적 origin id와 이벤트 id). 다른 소스
  해시는 `source_conflict`다.
- 새 로그가 검증되면 그것을 먼저 읽는다. 아니면 레거시 원본을 읽고 비파괴 마이그레이션을 제안한다.
- 옛 SQLite 인덱스에만 있는 표시 메타데이터는 명시적이고 읽기 전용이며 옵트인인 경로로 가져온다.
  `session migrate --apply --legacy-index`가 일관된 트랜잭션 하나 안에서 보호된 준비 사본을 읽는다.
  그 경로는 `legacy-session-index-import` 기능이 있는 빌드에만 있다. 기본 빌드는 데이터베이스를
  열지 않고 `migration_required`로 거부한다. 정상 동작은 데이터베이스를 열지 않으며, 표시
  메타데이터를 가져오지 않은 데이터베이스는 제목이 조용히 사라진 목록 대신 같은
  `migration_required` 진단을 낸다.

#### 세션이 아닌 SQLite는 범위 밖

SQLite는 세션 저장소의 일부가 아니다. **레거시** 최근 세션 인덱스로만 남고,
`legacy-session-index-import` 카고 기능과 `--legacy-index` 플래그로 접근한다. 그 밖의 SQLite
상태(할 일, 편집 통계, 아카이브, 텔레메트리)는 이벤트 저장소가 읽거나 쓰거나 변환하지 않으며,
저장소의 정상 읽기·쓰기 경로는 데이터베이스 핸들을 링크하지 않는다.

### 운영자 명령

`session inspect`는 배치, 이벤트 수, 헤드, 선택, 메시지 수, 제목, 아카이브 상태, 찢긴 꼬리 바이트,
손상을 읽기 전용으로 보고한다. `session doctor`는 모든 원본(또는 하나)을 검증하고 고치지 않는다.
`session migrate`는 `--apply`가 없으면 예행 실행이고, `--from`은 다른 HOME을 고치지 않고 읽는다.
`session export` / `import`는 묶음을 옮긴다. `export-legacy`는 다운그레이드 전에 현재 투영을 다른
HOME의 레거시 배치로 다시 쓴다. `recover-to`는 검증된 접두부를 새 루트로 건져 내고 원본을 덮어쓰지
않는다. `archive` / `restore`는 `ArchiveChanged` 레코드를 덧붙인다. 보존 정책은 세션을 숨기고
삭제하지 않는다. 하위 명령 묶음은 `session`이고 `sessions`가 별칭이다.

### 한계

| 한계 | 값 | 소유 |
| --- | --- | --- |
| 레코드 바이트 | 외부화 전 8 MiB | `schema.rs::MAX_RECORD_BYTES` |
| 세션 id | 160자 | `schema.rs::MAX_SESSION_ID_LEN` |
| 복원 항목 | 압축당 10개 | `schema.rs::MAX_RESTORATION_ENTRIES` |
| 페이지 이벤트 / 알려진 id | 각각 256 | `replication.rs` |
| 페이지 전송 바이트 | 16 MiB, 이스케이프 봉투 포함 | `replication.rs::MAX_WIRE_BYTES` |
| blob 청크 | 1 MiB | `blob_exchange.rs::BLOB_CHUNK_BYTES` |
| 이력 페이지 | 기본 50, 최대 200 | `GetHistoryPage` |
| 카탈로그 인덱스 재작성 | 죽은 바이트가 살아 있는 바이트의 2배 초과, 또는 64 MiB 초과 | `catalog.rs::needs_compaction` |
| 새 세션의 표식-로그 창 | 표식은 첫 로그 줄보다 먼저 내구해진다. 그 사이에 프로세스가 죽으면 표식만 남고, 다음 목록 조회는 그 세션을 `busy`로 본다. 세션을 다시 열어 저장하면 풀린다. 로그를 먼저 내구하게 하려면 append마다 순서를 뒤집어야 하므로 이 창은 현재 구조의 한계로 남긴다 | `writer.rs::dirty`, `catalog.rs::list` |

## 영속 런타임 상태 저널

이 문서는 현재 트리에 구현된 세션별 내구 상태 저널을 설명합니다. 대화 원본(세션 로그)은
건드리지 않고, 재시작 뒤에도 살아남아야 하는 **실행 상태**만 따로 기록합니다.

### 왜 따로 기록하는가

대화 로그는 "무슨 말이 오갔는가"를 담습니다. 재시작 복구에 필요한 것은 다른 질문입니다.
그 입력을 접수했다고 클라이언트에 답했는가, 실제로 실행을 시작했는가, 끝났는가, 결과를
모르는가. 이 구분이 없으면 재시작 뒤에 같은 입력을 다시 보내거나, 이미 실행된 입력을
조용히 잃습니다.

### 위치

`crates/base/src/durable_runtime/`가 소유합니다. `mod.rs`, `record.rs`(레코드와 상태),
`replay.rs`(스캔·적용·정리), `store.rs`(파일·fsync·압축), `tests.rs`입니다.

### 레코드 종류

`RecordKind`는 열여섯 가지입니다. 각 전이는 `Payload` 하나로 표현됩니다.

| 묶음 | 종류 |
| --- | --- |
| 입력 | `input.accepted`, `input.started`, `input.committed`, `input.cancelled`, `input.returned`, `input.in_doubt` |
| 실행 | `run.started`, `run.stopped` |
| 제어 | `control.applying`, `control.terminal` |
| 감시 | `monitor.upsert`, `monitor.checkpoint`, `monitor.stopped` |
| 깨우기 | `wake.pending`, `wake.acked` |
| 기타 | `checkpoint` |

`input.accepted`가 입력 본문을 한 번 보관하고, 이후 전이는 id만 참조합니다.

### 불변식

- 한 전이는 순번(seq) 하나를 갖습니다. sync가 끝나기 전에는 어떤 상태도 공개되지 않습니다.
- 두 프로세스가 같은 세션에 동시에 append해도 순번이 유실되거나 충돌하지 않습니다.
- 압축(compact) 전후의 `RuntimeState`는 같습니다. 보존 대상 항목은 하나도 잃지 않습니다.
- 대화 그래프의 정규 바이트는 이 저널 때문에 바뀌지 않습니다.
- 저널을 열 때 새로 만든 폴더 단계마다 그 부모 폴더를 flush합니다(`store.rs::ensure_private_dir`).
  첫 레코드는 저널 폴더를 flush해 `runtime.jsonl` 항목을 확정하지만, 폴더 자신의 항목이 부모에
  확정되지 않으면 정전 뒤 폴더와 함께 확정된 저널을 잃기 때문입니다. 이미 있는 폴더는 flush하지
  않으므로 append마다 비용이 늘지 않습니다. 시험은 `durable_runtime/directory_tests.rs`에 있습니다.

### 한도

`JournalLimits`의 기본값입니다(`store.rs`).

| 상수 | 값 | 의미 |
| --- | --- | --- |
| `MAX_JOURNAL_BYTES` | 32 MiB | 저널 파일 상한 |
| `LIFECYCLE_RESERVE_BYTES` | 1 MiB | 접수(admission)가 들어갈 수 없는 예비 영역. 수명주기 행은 들어갈 수 있습니다 |
| `COMPACT_AFTER_BYTES` | 4 MiB | 이 크기를 넘으면 압축 |
| `COMPACT_AFTER_RECORDS` | 4,096 | 이 레코드 수를 넘으면 압축 |
| `MAX_LIVE_INPUTS` | 1,000 | 살아 있는 입력 상한 |
| `MAX_LIVE_INPUT_BYTES` | 16 MiB | 살아 있는 입력 본문 합계 상한 |
| `MAX_INPUT_PAYLOAD_BYTES` | 1 MiB | 입력 하나의 본문 상한 |
| `TERMINAL_RUN_CAP` | 256 | 종료된 실행 기록 보존 수 |
| `TERMINAL_RECEIPT_CAP` | 1,024 | 종료된 제어 영수증 보존 수 |

### 복구

`RuntimeState`는 입력·입력 영수증·실행·제어·감시·깨우기 여섯 표를 들고 있습니다.
저널 계층이 **표현**하는 것과, 그 표를 큐에 반영하는 **집행**은 나뉩니다. 아래 규칙 중
앞의 셋은 저널 계층(`crates/base/src/durable_runtime/`)이 검사하고,
큐 반영은 `crates/app-core`가 맡으며 `durable_input` 테스트가 그것을 검증합니다.

- `accepted`로만 남은 입력은 **대기(pending)** 로 복원합니다. 같은 입력 id를 두 번 복원해도
  큐에는 하나만 들어갑니다.
- `started`인데 종료 행이 없는 입력은 **불확실(in_doubt)** 로 봉인합니다. 자동으로 다시
  주입하지 않습니다. 실행 여부를 모르는 일을 성공으로도 실패로도 적지 않습니다.
- `started`만 남은 실행은 `interrupted_unknown`입니다. 완료도 panic도 아닙니다
  (`crates/agent-runtime/src/turn_state.rs`의 `StopCause::InterruptedUnknown`).
- `input.in_doubt`는 `started` 상태에서만 봉인할 수 있습니다. 다른 단계에서 오면 전이
  위반으로 거절합니다.
- 손상된 꼬리(torn tail)는 구분해서 보고합니다(`RecoveryReport`). 레코드가 아예 없는 파일과
  손상된 파일은 다른 사건입니다.

### 저널이 없던 세션

기존 `<session>.json` 큐는 한 번만 가져옵니다. 결정적인 input id 매핑을 기록하고 두 번째
기동에서는 다시 가져오지 않습니다. 정상 전이 뒤에는 legacy JSON을 fsync로 미러링합니다.
미러 실패는 새 저널의 접수를 되돌리지 않습니다. 대신 복구 경고로 드러나고
`export-legacy`의 거절 사유가 됩니다.

### 이 저널이 보장하지 않는 것

- 외부 부수 효과의 exactly-once. 도구가 두 번 실행되는 것을 막지 않습니다.
- 다중 기기 사이의 감시 공유.
- 운영 규모의 처리량.
- provider 호출의 속도.

### 장애 주입

store 인스턴스별 fsync 실패 훅(`JournalIoPoint`)은 `test-support`에서만 켜집니다. 테스트가
가짜 fsync 성공을 내구성 증거로 쓰지 않도록, OS 호출 순서와 재시작 뒤 결과를 함께 남깁니다.
