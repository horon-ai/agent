# 크레이트 소유권과 모듈화 경계

언어: [English](CRATES.md) | **한국어**

> 이 문서는 "TUI"를 언급한다. `horon-agent` 패키지에는 터미널 UI가 없고 의존성 그래프에도 TUI 크레이트가 없다. 대화형 터미널 클라이언트는 `horon-agent-sdk`를 쓰는 별도 `horon-tui` 패키지이자 실행 파일이다. 경계 판단 기준을 설명하려고 이름을 남겼다.
이 문서는 공유 크레이트를 아무거나 쌓아 두는 곳으로 만들지 않으면서 `horon-agent`의 모듈성을 유지하기 위한 목표 구조를 정의한다. 실용적인 판단 기준을 담았다. 타입, 헬퍼, 동작을 루트 크레이트 밖으로 옮길지 정할 때 이 문서를 쓴다.

## 목표

주된 목표는 루트 크레이트의 재컴파일 범위를 줄여 일반 개발 빌드와 selfdev 빌드를 빠르게 만드는 것이다. 구조가 깨끗한 것은 이 컴파일 시간 목표를 뒷받침하기 때문에 가치가 있다.

- 안정적인 DTO와 프로토콜에 안전한 상태를 작은 크레이트로 옮겨서, 루트 동작을 바꿔도 그 계약이 재컴파일되지 않고 계약을 바꾸면 관련된 의존 대상만 재컴파일되게 한다.
- 의존성이 가벼운 크레이트는 가볍게 유지해서 빨리 컴파일되고, 큰 런타임, TUI, 제공자 의존 그래프를 관계없는 빌드로 끌어오지 않게 한다.
- 루트 전용 동작, 저장소, 프로세스, TUI, 서버, 제공자 런타임 로직은 의존성 확산을 늘리지 않고 전체 의존 경계를 옮길 수 있을 때까지 루트 크레이트에 둔다.
- 순환 의존과 `horon-agent-core`의 넓은 재내보내기를 통한 숨은 결합을 피한다.
- 모든 호출 지점을 의도적으로 고치는 경우가 아니면 마이그레이션 중에도 serde 호환성과 루트 재내보내기를 유지한다.
- 성공은 컴파일 영향으로 측정한다. 루트 수정이 줄고, 루트가 소유한 DTO가 줄고, 의존성 확산이 작아지고, 흔한 변경 뒤 `cargo check --profile selfdev`와 재빌드가 빨라지는지를 본다.

## 플랫폼 어댑터

OS별 시스템 호출은 `crates/platform*` 다섯 크레이트에만 있다. core 크레이트(`base`, `session-store`, `provider-core`, `app-core`)는 `horon_agent_platform`의 자유 함수만 부르고 자신이 어느 OS에서 컴파일되는지 모른다.

```text
crates/base · session-store · provider-core · app-core
   │  horon_agent_platform::{sync_file, sync_dir, tls_client_config, spawn_detached, ...}
   ▼
crates/platform           cfg로 Current를 고르고 모듈 fs·ipc·paths·power·process·signal·term·watch를 노출
   ▼
crates/platform-macos | platform-linux | platform-windows   PlatformAdapter 구현, 빌드마다 하나만 컴파일
   ▼
crates/platform-api       PlatformAdapter 트레이트, 공유 타입, 로그 싱크
```

| 크레이트 | 패키지 | 소유 |
| --- | --- | --- |
| `crates/platform-api` | `horon-agent-platform-api` | `PlatformAdapter` 트레이트와 공유 타입(`SyncMode`, `SleepAssertion`, `SleepInhibitor`, `FileStamp`, `FileIds`, `StdinState`, `ReadyChild`), 그리고 `log` 싱크. 트레이트가 `rustls::ClientConfig`를 반환하므로 의존성은 `rustls`뿐이다. |
| `crates/platform` | `horon-agent-platform` | `cfg`로 `pub type Current`를 정하고 자유 함수로 노출한다. TLS 설정을 프로세스당 한 번 만들어 `Arc`로 돌려준다. |
| `crates/platform-macos` | `horon-agent-platform-macos` | macOS 구현 |
| `crates/platform-linux` | `horon-agent-platform-linux` | Linux와 나머지 Unix 구현. FreeBSD도 이 크레이트가 맡는다. |
| `crates/platform-windows` | `horon-agent-platform-windows` | Windows 구현 |

선택 규칙은 컴파일 시점에 정해진다. `target_os = "macos"`는 macOS, `windows`는 Windows, `all(unix, not(target_os = "macos"))`는 Linux다. `crates/platform`은 이 선택을 대상별 의존성으로 선언하므로 최종 바이너리에는 어댑터 하나만 들어간다. macOS와 Windows 크레이트는 자기 대상이 아니면 `#![cfg(...)]`로 빈 라이브러리가 되고, Linux 크레이트는 POSIX 코드라 Unix 대상에서 컴파일되며 선택도 Unix에서만 된다. macOS와 Linux는 POSIX 코드(`unix.rs`, `unix_fs.rs`, `unix_term.rs`, `signal.rs`, `ipc.rs`)를 `#[path]`로 공유하고 다른 부분만 각자 구현한다.

트레이트 메서드는 연관 함수이고 상태를 들지 않는다. 호출자는 컴파일된 `Current`에 대해 호출하므로 이 경로에 `dyn` 디스패치가 없다.

`crates/platform`의 모듈은 이런 경계를 나른다.

| 모듈 | 하는 일 |
| --- | --- |
| `fs` | 개인 디렉터리 생성, 안전한 파일 열기와 검증, 캐시 stamp, 백업 회전 |
| `ipc` | Unix 소켓과 Windows 이름 있는 파이프를 같은 타입 이름으로 노출 |
| `paths` | OS별 설정·런타임 디렉터리 규칙 |
| `power` | 작업 중 잠자기 방지 정책과 OS 어서션 |
| `process` | 프로세스 그룹 신호, 자식 배치, 준비 파이프, 진단 |
| `signal` | 신호 해석, 프로세스 신원, 종료 요청 |
| `term` | 표준 입력 상태와 터미널 판정 |
| `watch` | 파일 시스템 변경 알림 |

어댑터는 로그를 `horon_agent_platform_api::log` 싱크로 보낸다. `horon-agent-logging`은 애플리케이션 기반에 의존하고 기반은 어댑터에 의존하므로, 어댑터가 로깅 크레이트를 직접 부르면 의존 순환이 된다. 로깅이 초기화할 때 자기 함수를 싱크로 설치하므로 메시지는 같은 로그 파일에 같은 등급으로 남는다.

### OS `cfg`는 어댑터 안에만 둔다

OS 기본 동작 때문에 core 크레이트에 `#[cfg(unix)]`, `#[cfg(windows)]`, `#[cfg(target_os = ...)]` 분기를 새로 넣지 않는다. 필요한 동작은 `crates/platform`의 함수로 부르고, 그 함수가 없으면 `PlatformAdapter` 트레이트를 넓힌다.

이 규칙은 `scripts/ci/check_platform_cfg.py`가 지킨다.

- `#[cfg(`, `#[cfg_attr(`, `#![cfg(`, `#![cfg_attr(`, `cfg!(` 가운데 `unix`, `windows`, `target_os`, `target_family`를 언급하는 줄을 센다. 테스트 모듈과 주석 처리된 줄도 센다.
- 플랫폼 크레이트 다섯 개 밖에서 남은 줄은 `scripts/ci/platform_cfg_allowlist.json`에 파일별 개수와 사유로 기록한다. 새 줄은 `path:line: text`로 지목되며 게이트가 실패한다.
- 개수가 줄어도 `--update`로 예산을 내릴 때까지 실패한다. 래칫이므로 게이트가 스스로 느슨해지지 않는다.
- `--update`는 개수만 다시 쓰고 사유를 만들지 않는다. 새로 들어간 파일의 사유는 `TODO: classify`이고, 사유를 적을 때까지 게이트가 계속 실패한다.
- `--report <경로>`는 남은 줄을 `path<TAB>count<TAB>reason`으로 쓴다.

허용 목록에 남는 줄은 기능 자체가 한 OS에만 있는 제품 코드다. Windows 콘솔 처리, macOS 알림, OpenSSH ControlMaster처럼 어댑터로 옮길 시스템 호출이 아니라 그 OS에만 존재하는 표면이다. 남길 때는 왜 어댑터의 기본 요소가 아닌지 사유에 적는다.

지금 목록에 남은 자리는 이런 것들이다. Linux 전용 Claude Code 인수는 `crates/base/src/claude_live.rs`가 pidfd로 대상 프로세스를 지목하고 시그널을 보낸다. 자원 지표는 `crates/app-core/src/perf.rs`가 Linux `/proc`, macOS `sysctlbyname`·`getloadavg`, Windows `GlobalMemoryStatusEx`를 각각 읽고, `crates/app-core/src/overnight.rs`는 Linux의 `statvfs`와 프로세스·세션 훅을 쓴다. Windows 콘솔의 VT 처리(`crates/core/src/console.rs`)와 glibc 할당자 진단(`crates/base/src/process_memory.rs`, 어댑터 안으로 옮긴 `crates/platform-linux/src/allocator.rs`)도 같은 자리에 있다. 어댑터가 나르는 것은 공통 동작이고, 이 줄들은 한 OS에서만 존재하는 측정·진단 수단이므로 제품 `cfg`로 남기고 사유를 `scripts/ci/platform_cfg_allowlist.json`에 적는다.

### TLS 신뢰 집합

HTTPS 클라이언트가 신뢰하는 루트는 webpki 내장 루트와 OS 루트의 합집합이고, 어느 OS에서도 이 합집합보다 좁지 않다.

| OS | 구현 |
| --- | --- |
| macOS | `rustls-platform-verifier` 0.7의 `Verifier::new_with_extra_roots`에 `webpki-root-certs`의 `TLS_SERVER_ROOT_CERTS`를 넘긴다. 키체인 인증서를 시작할 때 한꺼번에 읽지 않는다. |
| Windows | 같은 검증기와 같은 루트 집합을 쓴다. |
| Linux와 나머지 Unix | `rustls-native-certs`로 OS 저장소를 프로세스당 한 번 읽고 `webpki-roots`의 루트와 함께 `RootCertStore`에 넣는다. 파싱할 수 없는 인증서는 건너뛰므로 잘못된 파일 하나가 나머지 루트를 잃게 하지 않는다. 저장소가 없으면 webpki 루트만으로 동작한다. |

- ALPN은 `h2`, `http/1.1` 순서다.
- 암호화 제공자는 aws-lc-rs를 명시한다.
- `horon_agent_platform::tls_client_config()`가 `LazyLock`으로 한 번만 만들고 같은 `Arc`를 돌려준다. 클라이언트가 여러 개여도 OS 검증기 준비는 프로세스당 한 번이고, `tls_config_builds()`가 그 횟수를 노출한다.
- 구축에 실패하는 경로는 정해진 합집합의 부분집합인 webpki 루트만 쓰므로 받아들이는 대상을 넓히지 않는다.
- 모든 reqwest 선언은 `rustls-tls-manual-roots`를 쓰고, 클라이언트는 `.use_preconfigured_tls(tls_client_config().as_ref().clone())`로 만든다. 어댑터를 거치지 않은 클라이언트는 HTTPS에 실패하므로 TLS 설정 경로가 둘로 갈리지 않는다.
- 테스트는 `tls_client_config_with_extra_roots`로 OS가 신뢰하고 webpki에는 없는 루트가 통하는지 확인한다. 실제 신뢰 저장소는 건드리지 않는다.

### 저장 내구성은 core 정책이고 flush 호출만 OS에 맡긴다

무엇을 확정할지는 OS와 무관한 core가 정하고, 어떤 시스템 호출로 확정할지만 어댑터가 고른다. `SyncMode`는 보장 수준의 이름이지 특정 호출의 이름이 아니다.

| 모드 | 뜻 | macOS | Linux | Windows |
| --- | --- | --- | --- | --- |
| `Full`(기본) | 정전에도 남는 가장 강한 보장 | `F_FULLFSYNC`(std `sync_all`), 드라이브 쓰기 캐시까지 flush | `fdatasync`(std `sync_data`) | `FlushFileBuffers`(std `sync_all`) |
| `Fast` | OS 크래시는 견디는 가장 싼 flush | `fsync(2)` | 더 싼 호출이 없어 `Full`과 같다 | 더 싼 호출이 없어 `Full`과 같다 |

원본은 `sync_file`과 `sync_dir`로 확정한다.

- 이벤트 로그 append, 원본 표식(`event_origin.rs`), 아카이브
- 런타임 저널(`durable_runtime/store.rs`)과 durable input
- 블롭
- 원본에서 다시 만들 수 없는 로컬 상태: `local-head.json`, `local-head-explicit.json`, `local-explicit-source.json`, `local-runtime.json`

더티 표식(`writer.rs`의 `.catalog.dirty/<id>`)은 원본이 아니라 재생성 힌트다. `writer.rs::dirty`는
빈 파일을 원본 append보다 먼저 만들되 flush하지 않는다. 낡은 파생을 막는 것은 아래의 `Stamp` 대조이고,
전원 손실로 표식을 잃어도 늘어난 로그와 낡은 스탬프가 남아 다음 조회가 원본에서 다시 만든다. 근거와
고정 테스트는 [`../protocol/SESSION_STORE.ko.md`](../protocol/SESSION_STORE.ko.md)에 있다.

한 묶음 커밋은 로그 flush를 한 번만 낸다. `BlockingWriter::batched` 안의 append는 원본 바이트를 다 쓴 뒤 flush를 미루고, 묶음 끝의 `flush_pending_log`가 그 한 번을 낸다. `local-head.json` 같은 선택 상태는 그 flush가 성공한 뒤에만 쓴다. flush가 실패하면 `batched`는 오류를 돌려주고 확정된 선택 상태는 남기지 않으므로, 같은 이벤트를 다시 보내면 로그에 한 번만 들어간다. 블롭 flush는 블롭마다 그대로다.

파생은 임시 파일에 쓰고 rename만 한다. `cache_io::publish_derived`와 `event_store::publish_local_derived`가 그 경로이고 fsync하지 않는다.

- 파생 대상: `checkpoint.json`, `sparse-index.jsonl`, `summary.json`, 카탈로그 행(`.catalog.index.jsonl`), `facade-cache.json`, 더티 표식 삭제

크래시 뒤에 rename 결과가 남지 않았으면 다음 로드가 원본에서 다시 만든다. 파생을 믿는 조건은 `Stamp`다. device, inode, 길이, mtime, ctime이 원본 이벤트 로그와 맞아야 하고, 저장한 값은 sha256으로 한 번 더 검사한다(`cache_io::decode`). 맞지 않으면 캐시를 버리고 원본에서 다시 만든다. `.catalog.dirty` 표식이 있는 세션의 카탈로그 행도 같은 이유로 신뢰하지 않는다.

`[storage] sync`가 이 모드를 고르고 `HORON_AGENT_STORAGE_SYNC`가 프로세스 하나에 대해 덮어쓴다. 모르는 값은 `full`로 떨어지고 프로세스당 한 번만 경고한다. 기본값은 `full`이고 맥 기본값을 `fast`로 바꾸지 않는다.

#### 커밋 I/O는 고정 크기 writer 풀이 맡는다

세션 수명 커밋 I/O의 스레드 소유는 `crates/base/src/session/event_pool.rs`가 맡고,
`crates/session-store`는 여전히 `BlockingWriter`라는 블로킹 경계만 노출한다. 풀은
`clamp(available_parallelism, 2, 4)`개의 워커를 프로세스당 한 번 만들고, 세션을 `registry_key` 해시로
워커 하나에 고정 배정한다. `BlockingWriter`는 `Send`이지만 `Sync`가 아니므로 워커의 세션 맵이 값을
소유하고, 그 항목이 사라질 때 `.writer.lock`이 풀린다.

계약은 이전 구조와 같다. 한 세션의 job은 제출 순서대로 실행되고 `call()`은 응답 랑데부로 기다리며,
마지막 파사드의 drop은 앞선 job을 drain한 뒤 `Close` 응답을 받고 락을 놓는다. job panic은
`catch_unwind`가 그 세션 항목만 제거하므로 같은 워커의 다른 세션은 계속 커밋한다. 워커 스레드에서
자기 writer를 다시 부르는 재진입은 교착 대신 오류다. 대가는 머리 막힘이다. 큰 세션의 저장이 그
워커를 오래 붙잡으면 같은 워커에 배정된 다른 세션의 저장이 그만큼 늦어지고, 다른 워커의 세션은
영향받지 않는다. 세션당 스레드와 RSS 기울기는 이 맞바꿈을 재는 측정이다.

## 소유 규칙

### 타입 크레이트는 안정적인 데이터 계약을 소유한다

`*-types` 크레이트에는 다음이 들어가야 한다.

- 여러 크레이트나 프로토콜 계층이 쓰는 순수 데이터 구조.
- 직렬화 형태와 데이터 계약에 묶인 작은 순수 헬퍼 메서드.
- 파일 시스템, 네트워크, 프로세스, TUI, 제공자 클라이언트, 전역 상태, 저장소 접근이 없어야 한다.
- 의존성은 serde, chrono, 필요할 때 다른 타입 크레이트로 제한한다.

예: `horon-agent-session-types`, `horon-agent-background-types`, `horon-agent-todo-policy`.

### 도메인 동작 모듈은 루트 런타임 동작을 소유한다

루트 모듈은 다음이 필요하면 동작을 계속 소유한다.

- `crate::storage`, `crate::config`, `crate::logging`, `crate::server` 또는 프로세스 실행.
- 제공자 HTTP 클라이언트와 인증 관리자.
- Tokio 런타임, 백그라운드 작업, 채널, 전역 캐시, 파일 잠금, PID 레지스트리.

터미널 렌더링과 crossterm/ratatui 상태는 루트의 관심사가 아니다. 대화형 클라이언트는 별도 `horon-tui` 패키지이며 `horon-agent-sdk`를 통해 데몬에 닿는다.

타입의 고유 메서드가 이런 API를 필요로 하면 타입을 루트에 두거나, 동작과 의존성을 함께 도메인 크레이트로 옮긴다. 구조체만 옮겨서 루트에 규칙에 어긋나는 고유 구현을 강제해서는 안 된다.

### `horon-agent-core`는 정말 공유하는 기본 요소를 위한 곳이다

`horon-agent-core`에는 다음이 들어가야 한다.

- 아직 뚜렷한 도메인 크레이트가 없는 도메인 사이의 기본 요소.
- 여러 크레이트가 쓰는 아주 작고 의존성이 가벼운 헬퍼.
- 새 도메인 타입 크레이트를 만들기에는 이른 경우에만 두는 임시 DTO 보관.

`horon-agent-core`가 추출한 DTO를 끝없이 쌓아 두면 안 된다. 덩어리가 커지면 목적에 맞는 도메인 크레이트로 나눈다.

### 컴파일 속도 판단 규칙

루트 크레이트 변경량이나 의존성 확산을 줄이는 분할을 우선한다. 새 크레이트가 의존성을 늘리거나 재빌드 확산을 키우거나 크레이트 사이 수정을 자주 강제한다면, 파일이 깔끔해 보이게 하려고 분할하지 않는다. 좋은 분할은 다음 컴파일 이점 가운데 하나 이상을 가진다.

- 흔한 루트 동작 수정이 더 이상 안정적인 타입 정의를 건드리지 않는다.
- 타입만 바꾼 변경을 작은 타입 크레이트와 관련 의존 대상만 컴파일해서 확인할 수 있다.
- 무거운 의존성이 DTO 크레이트에 들어가지 않는다.
- 여러 하위 크레이트가 루트 크레이트에 의존하지 않고 작은 계약을 쓸 수 있다.

### 재내보내기 정책

마이그레이션 중에는 다음 순서를 따른다.

1. 타입을 대상 크레이트로 옮긴다.
2. 호출 지점을 유지하려고 기존 루트 경로를 `pub use ...`로 남긴다.
3. 좁은 테스트와 재적재를 검증한다.
4. 나중에 하위 크레이트가 도메인 크레이트에 직접 의존할 수 있게 된 뒤에만 불필요해진 루트 재내보내기를 지운다.

## 이동 점검 목록

타입이나 순수 헬퍼를 옮길 때마다 이 점검 목록을 쓴다. 이동이 간단하지 않으면 PR이나 커밋 설명에 복사해 둔다.

1. 대상을 분류한다.
   - [ ] 루트 런타임 동작이 아니라 안정적인 데이터 계약이나 순수 헬퍼인가?
   - [ ] 고유 메서드가 있는가?
   - [ ] 그 메서드가 저장소, 네트워크 클라이언트, TUI 상태, 프로세스 관리, 전역 같은 루트 전용 API를 필요로 하는가?
   - [ ] 동작도 함께 옮겨야 한다면 확산을 늘리지 않고 전체 의존 경계를 옮길 수 있는가?
2. 호환성을 확인한다.
   - [ ] serde 표현이 그대로인가?
   - [ ] 기본값, 건너뛰기, 이름 변경, 열거형 판별값이 유지되는가?
   - [ ] 모든 필드 가시성이 여전히 적절한가?
   - [ ] 루트가 호환용 재내보내기를 유지할 수 있는가?
3. 크레이트 상태를 확인한다.
   - [ ] 대상 크레이트에 필요한 의존성 정책이 이미 있는가?
   - [ ] 새 의존성이 타입 크레이트에 맞는 라이브러리, 보통 `serde`, `serde_json`, `chrono` 또는 형제 타입 크레이트로 제한되는가?
   - [ ] 대상 크레이트가 여전히 순환 의존이 없는가?
   - [ ] `cargo metadata`와 `cargo check`가 루트, TUI, 제공자, 저장소, 서버, 프로세스 의존성을 타입 크레이트로 끌어오지 않았는가?
4. 검증한다.
   - [ ] 옮긴 타입을 덮는 좁은 테스트 필터가 있는가?
   - [ ] `cargo check --profile selfdev -p <type-crate> -p horon-agent --bin horon-agent`가 통과했는가?
   - [ ] 관련된 좁은 루트 테스트가 통과했는가?
   - [ ] `cargo fmt`가 통과했는가?
   - [ ] 깨끗하게 커밋된 HEAD에서 재적재가 통과했는가?

## 의존 경계 가드

타입 크레이트 의존성을 추가하거나 바꾼 뒤에는 이 가드를 실행한다.

```sh
python3 scripts/ci/check_dependency_boundaries.py
```

가드는 `horon-agent-*-types` 크레이트가 `horon-agent`, `horon-agent-core`, 제공자 크레이트, TUI 크레이트, 프로토콜·런타임 크레이트, 데스크톱·모바일 크레이트처럼 루트나 런타임 비중이 큰 내부 크레이트에 직접 의존하는 것을 막는다. 타입 크레이트는 외부 경량 라이브러리와 다른 타입 크레이트에 의존할 수 있다. 새 내부 의존성이 필요하면 먼저 그것 자체가 타입 크레이트가 되어야 하는지 정한다.

## 증가 래칫

검토된 결정으로만 올릴 수 있는 세 수치를 게이트 하나가 고정한다.

```sh
python3 scripts/ci/check_growth_ratchets.py
python3 scripts/ci/check_growth_ratchets.py --list
```

| 래칫 | 세는 대상 | 상한 파일 |
| --- | --- | --- |
| `Cargo.lock` 패키지 | `Cargo.lock`의 `[[package]]` 항목. 같은 크레이트의 두 버전은 두 번 센다 | `scripts/ci/lockfile_packages.ratchet.json` |
| 내부 레거시 표시자 | 제외 하위 트리 밖에서 `legacy`, `deprecated`, `compat_alias`, `back.?compat`에 맞는 Rust 줄 | `scripts/ci/internal_legacy_markers.ratchet.json` |
| 문자열 결과 | 시그니처가 `Result<_, String>`을 돌려주는 Rust 줄. 문자열 오류에는 분기할 타입이 없어 호출자가 원인을 구분하지 못한다 | `scripts/ci/result_string.ratchet.json` |

래칫 파일은 이름이 `.ratchet.json`으로 끝나거나, 최상위 객체가 `counter`와 `ceiling`을 선언한 `scripts/ci/*.json` 파일이다. 그래서 나중에 추가할 래칫(모델 이름 분기)은 파일 하나만 더하면 되고 워크플로나 `check_all.sh`를 고치지 않는다. `.ratchet.json`인데 두 키를 잃은 파일은 건너뛰지 않고 실패한다. `counter.kind`는 `cargo_lock_packages` 또는 `regex_lines`이고, 후자는 `pattern`, `roots`, `extensions`, `exclude`를 받는다. 옆에 있는 게이트별 예산 파일(`panic_budget.json`, `code_size_budget.json`)은 두 키를 선언하지 않으므로 각자 스크립트가 계속 소유한다.

레거시 표시자 래칫은 외부 계약을 구현하는 하위 트리를 파일에 사유와 함께 제외한다. `crates/harness-api-server`(하네스 API v1 브리지와 변환 계층), `crates/harness-api`(공개된 v1 표면과 스키마 스냅숏), `crates/session-store`(v0.1.x가 쓴 세션을 읽는 저장소 이관·재생 리더)다. 존재하지 않는 경로를 제외하면 실패하므로 낡은 항목이 구멍을 넓히지 못한다. 나머지는 내부 잔여물이며 줄어들 수는 있어도 늘어날 수는 없다.

증가하면 실패하고, 감소하면 `--update` 실행을 권하는 줄만 출력한다. 관계없는 변경에서 정리한 사람이 이 파일을 고칠 필요가 없고, 게이트가 스스로 느슨해지지도 않는다. 뭔가 다시 늘어나면 옛 상한에서 곧바로 실패한다. 새 의존성은 패키지 상한을 정당하게 올리며, 그 재고정은 의존성을 추가하는 검토 커밋에 들어간다. `--update`는 `ceiling`만 다시 쓰고 설명이나 사유는 건드리지 않는다.

## 테스트 정책

검증에는 좁은 필터를 우선한다. 넓은 필터는 관계없는 상태 기반 테스트, 타이밍에 민감한 테스트, 벤치마크 테스트를 함께 고르는 경우가 많다.

모듈화 중에 관찰한 넓은 필터의 위험:

- `usage`는 순수 사용량 테스트에 더해 앱 표시 테스트까지 고른다.
- `session::`은 세션 영속화를 넘어 라이브 접속 서버 테스트와 피커 동작까지 고른다.
- `ambient`는 ambient 모듈의 영속화·런타임 테스트를 넘어 설정과 스케줄 상태를 다루는 헬퍼 통합 테스트까지 고른다.

도메인 크레이트나 모듈 옆에 정확한 필터를 적어 둔다. 넓은 필터는 주기적인 전체 점검에는 여전히 쓸모가 있지만, 정확한 테스트와 컴파일 검사가 통과하면 DTO만 추출하는 작업을 막아서는 안 된다.

현재 DTO 분할 뒤의 좁은 검증 표:

| 영역 | 빠른 컴파일 검사 | 분할 중 쓴 좁은 루트 테스트 | 비고 |
| --- | --- | --- | --- |
| 사용량 DTO | `cargo check --profile selfdev -p horon-agent-usage-types -p horon-agent --bin horon-agent` | `crates/usage-types`의 단위 테스트와, 그 DTO를 소비하는 정확한 테스트 이름을 우선한다. 맨 `usage` 필터는 표시 계층 테스트까지 고르므로 필수 게이트로 쓰지 않는다. | DTO 크레이트는 보고서와 로컬 카운터 계약을 소유한다. 런타임 조회, 캐시, 표시는 루트에 남는다. |
| 게이트웨이 DTO | `cargo check --profile selfdev -p horon-agent-gateway-types -p horon-agent --bin horon-agent` | 가능하면 정확한 테스트 이름으로 게이트웨이 영속화·인증 테스트를 좁힌다. | 페어링과 토큰의 HTTP·WebSocket 동작은 루트에 남는다. |
| ambient DTO | `cargo check --profile selfdev -p horon-agent-ambient-types -p horon-agent --bin horon-agent` | 스케줄러와 타입 소비자만 대상으로 한다. | ambient DTO 크레이트는 사용 기록만 소유한다. 큐, 런타임, 프롬프트 동작은 루트에 남는다. |
| ambient 동작 모듈 | `cargo check --profile selfdev -p horon-agent-app-core -p horon-agent --bin horon-agent` | `cargo test --profile selfdev -p horon-agent-app-core ambient::ambient_tests --lib`; `cargo test --profile selfdev -p horon-agent-app-core ambient::scheduler::tests --lib`; `cargo test --profile selfdev -p horon-agent-app-core ambient::runner::runner_tests --lib` | 모듈만 고치는 리팩터링에서 맨 `ambient`를 필수 게이트로 쓰지 않는다. 모듈을 넘나드는 설정 상태 테스트까지 고르기 때문이다. |
| 메모리 활동 DTO | `cargo check --profile selfdev -p horon-agent-memory-types -p horon-agent-core -p horon-agent --bin horon-agent` | `cargo test --profile selfdev -p horon-agent-base runtime_memory_log --lib` | `memory::activity`는 현재 어떤 테스트와도 맞지 않으므로 소비자 테스트를 쓴다. |
| 목표·todo·catchup 코어 DTO | `cargo check --profile selfdev -p horon-agent-core -p horon-agent --bin horon-agent` | 동작이 바뀌면 목표·todo·catchup의 정확한 필터를 쓴다. | 지금은 작고 안정적이어서 `horon-agent-core`에 둘 만하다. 변경이 잦아지면 다시 본다. |

## 컴파일 기준선 관찰

> 아래 측정은 2026-04-30 시점 기록이며 현재 상태가 아니다. 표에 나오는 `src/usage.rs`와
> `crates/core/src/usage_types.rs`는 그때의 경로이고 지금 트리에는 없다. 지금 루트 `src/`에는
> `cli/`, `lib.rs`, `main.rs`만 있고, `crates/core/src/`에는 도메인 DTO 파일이 없다.

컴파일 속도 경계 문서 커밋 뒤에 `scripts/dev/cargo.sh check --profile selfdev -p horon-agent --bin horon-agent`로 2026-04-30에 측정했다. mtime을 건드려 재는 거친 벤치마크이며 통계적 연구가 아니다. 그래도 우선순위를 정하기에는 충분하다.

| 시나리오 | 관측 시간 | 해석 |
| --- | ---: | --- |
| 문서만 고친 커밋 직후의 무변경 검사 | ~65.8s | 첫 검사는 환경과 캐시 상태가 좌우할 수 있다. 순수한 무변경 정상 상태가 아니라 예열과 잡음 기준선으로 본다. |
| 루트 동작 모듈 `src/usage.rs`를 건드림 | ~6.25s | 의존성이 이미 빌드되어 있으면 루트 전용 동작 수정은 비교적 싸다. |
| `crates/core/src/usage_types.rs`(그때의 경로이고 지금은 없다)를 건드림 | ~65.35s | `horon-agent-core`를 고치면 넓은 하위 의존 대상이 무효가 된다. 변경이 잦은 도메인 DTO를 `horon-agent-core`에 넣지 않는다. |

시사점: 컴파일 속도 목표는 단순히 "move things out of root"가 아니다. 안정적이고 변경이 드문 계약을 루트 밖으로 옮기는 것은 좋다. 그러나 변경이 잦은 도메인 DTO를 `horon-agent-core`에 많이 넣으면 `horon-agent-core`의 확산이 커서 역효과가 날 수 있다. 바뀔 가능성이 큰 도메인 DTO에는 `horon-agent-usage-types`, `horon-agent-gateway-types`, `horon-agent-ambient-types` 같은 목적별 말단 크레이트를 쓴다.

## `horon-agent-core` 확산 감사

이 시점에 루트 크레이트는 `horon-agent-core`에 직접 의존하는 유일한 Cargo 대상이다. 그러나 루트는 `horon-agent-core` 모듈을 많이 재내보내고, 루트가 재컴파일 비용이 큰 대상이다. 위 기준선에서 `horon-agent-core`를 건드리자 넓은 하위 검사가 무효가 됐다. 따라서 Cargo.toml에 적힌 직접 의존 대상이 지금은 적더라도 `horon-agent-core`는 확산이 큰 크레이트로 다뤄야 한다.

관찰한 루트 재내보내기와 사용 경로:

- `catchup` 타입은 `crates/app-core/src/catchup.rs`, `goal`은
  `crates/base/src/goal.rs`, `todo`는 `crates/task-types`에 있다
- `env`, `id`, `stdin_detect`, `util`, `panic_util`은 `crates/core/src/`에 있다

이 감사는 headless 전환 이전의 루트 모듈을 대상으로 했다. 그 모듈은 지금 루트 `src/`에 없고,
루트에는 `cli/`, `lib.rs`, `main.rs`만 남아 있다.

이 감사에서 정한 컴파일 속도 우선순위:

1. `horon-agent-core`에 뭉쳐 있고 바뀔 가능성이 큰 도메인 DTO를 목적별 말단 크레이트로 옮긴다.
2. 안정적인 일반 유틸리티는 `horon-agent-core`에 둔다.
3. 아주 안정적이거나 임시 보관인 경우가 아니면 `horon-agent-core`에 새 도메인 DTO를 더하지 않는다.

| 모듈 | 현재 내용 | 장기적으로 둘 곳 | 비고 |
| --- | --- | --- | --- |
| `ambient_usage_types` | ambient 스케줄러 사용 기록과 속도 제한 DTO | `horon-agent-ambient-types`로 이동함 | 이동은 끝났고, 그때 두었던 호환용 재내보내기도 지금 루트 `src/lib.rs`에는 없다. |
| `catchup_types` | catchup이 저장하는 상태와 렌더링된 브리프 DTO | `horon-agent-catchup-types` 또는 코어에 유지 | 작고 변경이 드물다. catchup이 커질 때만 나눈다. |
| `copilot_usage_types` | 로컬 Copilot 사용량 카운터 | `horon-agent-usage-types`로 이동함 | 이동은 끝났고, 그때 두었던 호환용 재내보내기도 지금 루트 `src/lib.rs`에는 없다. |
| `gateway_types` | 페어링된 기기와 페어링 코드의 영속 기록 | `horon-agent-gateway-types`로 이동함 | 페어링과 토큰 동작은 루트에 남는다. |
| `goal_types` | 목표 상태, 마일스톤, 상태, 업데이트 | `horon-agent-goal-types` 또는 `horon-agent-task-types` | 도메인이 더 크다. 목표와 도구 작업이 늘면 나눌 만하다. |
| `memory_types` | 메모리 활동 DTO | `horon-agent-memory-types`로 이동함 | 메모리는 자체 타입 크레이트를 둘 만큼 도메인 비중이 있다. |
| `todo_types` | todo 항목 DTO | `horon-agent-task-types`, `horon-agent-todo-types`, 또는 코어 | 아주 작다. 목표·catchup 작업 상태 크레이트에 합칠 수 있다. |
| `usage_types` | 제공자 사용량 보고서 DTO | `horon-agent-usage-types`로 이동함 | 런타임 조회, 캐시, 표시는 루트에 남는다. |
| `env` | 환경 변수 헬퍼 | 코어에 유지 | 일반 유틸리티라 도메인 크레이트가 필요 없다. |
| `id` | ID 헬퍼 | 코어에 유지 | 일반 유틸리티다. |
| `panic_util` | 패닉 형식화 헬퍼 | 코어에 유지 | 일반 런타임 유틸리티다. |
| `stdin_detect` | stdin 감지 헬퍼 | 코어에 유지 | 일반 플랫폼·런타임 유틸리티다. |
| `util` | 기타 유틸리티 | 나중에 감사한다 | 무엇이든 모으는 곳이 되면 안 된다. |

## 목표 도메인 타입 크레이트

완료했거나 가치가 큰 도메인 타입 분할:

1. `horon-agent-usage-types`
   - `usage_types`
   - `copilot_usage_types`
   - 루트 형식화·런타임 헬퍼와 분리되는 시점에 순수 계정 사용량 DTO

2. `horon-agent-gateway-types`
   - `gateway_types`
   - 설정이 `GatewayConfig`를 소유할지 정한 뒤 필요하면 `GatewayConfig`
   - 모바일 크레이트가 필요로 하면 모바일 게이트웨이의 프로토콜 안전 DTO

3. `horon-agent-ambient-types`
   - `ambient_usage_types`
   - ambient 상태·요청·결과 DTO. 다만 루트 전용 `AmbientState::load/save/record_cycle` 메서드를 루트 자유 함수나 영속 계층으로 분리한 뒤에만 옮긴다.

4. `horon-agent-memory-types`
   - `memory_types`
   - 서버, TUI, 도구에서 함께 쓰는 메모리 프로토콜·활동 DTO

5. 선택 사항인 작업 상태 크레이트
   - `goal_types`
   - `todo_types`
   - 제품 모델이 묶기를 원하면 `catchup_types`

## 큰 모듈 리팩터링 대상

단순한 DTO 이동이 아니다. 동작 경계를 먼저 리팩터링한다.

### 세션 모듈

headless 전환 이전 `src/session.rs`의 책임은 지금 `crates/session-store`와
`crates/session-types`에 나뉘어 있다. 아래는 그 위에서 남은 분할 목표다.

- 메타데이터와 세션 모델
- 영속화와 저널 재생
- 시작 스텁과 원격 시작 스냅샷
- 메모리 프로파일링과 캐시 귀속
- 렌더링은 기존 `session/render.rs`에 있다
- 충돌 복구는 기존 `session/crash.rs`에 있다

### 앰비언트 모듈

headless 전환 이전 `src/ambient.rs`의 책임은 지금 `crates/app-core/src/ambient/`에
있다. 아래는 그 위에서 남은 분할 목표다.

- 보이는 사이클 컨텍스트 입출력
- 상태 영속화
- 지시 영속화
- 스케줄 큐와 잠금
- 프롬프트 구성
- 관리자와 런타임 조율

load/save/record 동작을 구조체에서 분리하기 전에는 `AmbientState`를 DTO로 옮기지 않는다.

### 사용량 모듈

headless 전환 이전 `src/usage.rs`의 책임은 지금 `crates/base/src/usage.rs`와
`crates/base/src/usage/`에 있고, DTO는 `horon-agent-usage-types`가 소유한다.

- API 조회 제공자
- 제공자 응답 파싱
- 로컬 캐시와 동기화
- 표시 형식화
- 계정 선택과 안내
- 공개 보고서 DTO는 `horon-agent-usage-types`에 둔다

### 게이트웨이 모듈

headless 전환 이전 `src/gateway.rs`의 책임은 지금 `crates/base/src/gateway.rs`와
`crates/base/src/gateway/`에 있고, DTO는 `horon-agent-gateway-types`가
소유한다.

- 레지스트리 영속화
- 페어링과 토큰 인증
- HTTP 라우트 처리
- WebSocket 인증과 추출
- WebSocket 릴레이
- 공개 게이트웨이 DTO는 `horon-agent-gateway-types`에 둔다

## 하네스 경계 불변식

크레이트를 옮기거나 도구 표면을 넓힐 때 지켜야 하는 경계다. 항목마다 그 경계를 실제로 소유한 파일을 적었다. 여기 적은 경로는 이번 변경 시점에 실존하는 것만 골랐고, 위의 `## 컴파일 기준선 관찰` 표는 측정 당시 기록이므로 그대로 둔다.

### 턴 상태는 루프 밖에 있고 에이전트를 모른다

- `turn_state`는 `Agent`를 참조하지 않는다. 세션, 클라이언트 이벤트, 디스크 어느 것도 만지지 않고 턴 카운터와 종료 사유만 들고 있다: `crates/agent-runtime/src/turn_state.rs`의 `TurnState`, `TurnOutcome`, `TurnStateError`.
- 이 크레이트의 의존성은 `thiserror`와 `tokio`의 `sync` 기능뿐이다. 여기에 저장소, 제공자, 프로토콜 타입이 들어오면 경계가 깨진 것이므로, 턴 상태를 확장할 때는 원시 타입으로 표현할 수 있는 것만 넣는다.
- 두 턴 루프(`crates/app-core/src/agent/turn_loops.rs`, `crates/app-core/src/agent/turn_streaming_mpsc.rs`)는 지역 변수 대신 이 타입에 전이를 기록한다.

### 자동 계속 예산은 턴 상태가 소유하고 상한이 있다

- 자동 계속 예산은 `TurnState`가 들고 있으며 새 턴마다 0으로 돌아간다
  (`crates/agent-runtime/src/turn_state_tests.rs:20-26`).
- 상한은 2회다(`crates/agent-runtime/src/turn_recovery.rs:31`).
- 예기치 않은 정지의 자동 계속 판정은 두 턴 루프(순차·스트리밍)에서 같은 함수를 쓴다
  (`crates/app-core/src/agent/turn_loops.rs:1392-1396`,
  `crates/app-core/src/agent/interrupts.rs:679-683`).

### 큐 소비는 메모리와 디스크를 함께 커밋한다

- `SoftInterruptQueue`는 디스크를 만지지 않는다. 큐 타입은 `crates/agent-runtime/src/lib.rs`가 소유하고, 디스크 스냅샷은 `crates/app-core/src/agent/interrupts.rs`가 소유한다. 이 분리 때문에 큐 크레이트에 파일 시스템 의존성을 넣지 않는다.
- 큐는 하나의 `Arc<Mutex<QueueInner>>`를 감싼 뉴타입이고 `QueueInner`가 `pending`과 `in_flight`를 함께 든다. 클론은 같은 세션의 큐를 공유하고, 전이는 한 번의 락 안에서 두 벡터를 함께 고치므로 중간 상태가 관측되지 않는다: `crates/agent-runtime/src/lib.rs`의 `transfer_to_in_flight`, `commit`, `restore`, `pending_snapshot`.
- 턴 루프는 턴 하나에 `Option<InFlightGuard>` 하나를 만들어 턴 수명 동안 들고 있고, 주입은 `pending`에서 `in_flight`로 옮기기만 한다. 가드를 새로 만들지 않으므로 헬퍼가 반환해도 복원이 돌지 않는다: `crates/app-core/src/agent/interrupts.rs`의 `InFlightGuard`와 `crates/app-core/src/agent/turn_loops.rs`, `crates/app-core/src/agent/turn_streaming_mpsc.rs`의 가드 소유.
- 정상 종료의 커밋 순서는 디스크가 먼저다. `soft_interrupt_store::overwrite`로 대기 스냅샷을 덮어쓰고 그 호출이 `Ok`일 때만 `queue.commit()`이 `in_flight`를 비운다. 디스크 쓰기가 실패하면 `in_flight`를 그대로 두므로 메시지가 사라지지 않고, 다음 복원에서 한 번 더 주입되는 쪽이 손실보다 낫다: `crates/app-core/src/agent/interrupts.rs`의 `commit_soft_interrupt_turn`, `crates/base/src/soft_interrupt_store.rs`의 `overwrite`.
- 실패, 취소, panic으로 끝난 턴은 `in_flight`를 `pending` 앞으로 되돌리고 디스크 사본은 그대로 둔다. 동시에 그 턴이 전사에 붙인 메시지만 골라 제거해 재시도가 같은 메시지를 정확히 한 번 주입하게 한다: `crates/app-core/src/agent/interrupts.rs`의 `restore_soft_interrupt_turn`, `roll_back_soft_interrupt_injections`, `settle_soft_interrupt_turn`.
- 복원은 파괴적인 `take`가 아니라 `soft_interrupt_store::load`로 읽는다. 커밋 전에 크래시해도 디스크 사본이 남아 있어야 하기 때문이다: `crates/base/src/soft_interrupt_store.rs`의 `load`와 `crates/app-core/src/agent/interrupts.rs`의 `restore_persisted_soft_interrupts`.

### 비활성 도구는 세 목록 어디에도 나오지 않는다

- 등록은 되어 있지만 아직 활성화되지 않은 도구는 `definitions*`, `tool_names`, `closest_tool_names` 어디에도 나오지 않는다. 세 표면이 같은 비활성 집합을 본다: `crates/app-core/src/tool/mod.rs`의 `inactive_lazy_tools`, `register_inactive`, `advertised_tool_names`, 그리고 제안 목록을 만드는 `crates/app-core/src/tool/name_suggestions.rs`의 `closest_tool_names`.
- 활성기는 레지스트리 락을 쥔 채 부르지 않는다. `execute`는 `resolve_registered_tool`에서 가드를 모두 놓고 활성기를 부른 뒤 맵을 다시 읽는다. 락을 쥔 채 부르면 활성기가 도구를 등록할 때 교착이 난다.
- 활성기는 `Registry`의 클론과 `WeakRegistry` 업그레이드에서 공유된다. 클론마다 새로 만들면 같은 세션 안에서 지연 활성이 갈린다.

### 미드스트림 규칙과 외부 규칙 가져오기는 기본 꺼짐이다

- `[[stream_rules]]` 항목은 `[features] stream_rules`가 켜졌을 때만 강제되고 기본값은
  꺼짐이다(`crates/base/src/config/default_file.rs:342-345`,
  `crates/config-types/src/lib.rs:1352-1375`).
- 외부 에이전트 규칙 파일 가져오기도 기본 꺼짐이다
  (`crates/base/src/config.rs`의 `FeatureConfig`).

### 경로 정책은 거부만 하고 셸을 막지 못한다

- 경로 정책은 deny-only이고 `bash`를 막지 못한다. 정책은 거부만 할 수 있고 허용을 강제하지 못하며, `read`, `write`, `edit`, `replace`, `apply_patch`, `ls`, `agentgrep` 실행 전에만 검사한다: `crates/app-core/src/tool/path_policy.rs`.
- 정책 저장소는 `Registry` 필드가 아니다. 세션 키, owner 토큰, `Drop` 정리로 등록하고 해제한다. 그래야 도구 레지스트리의 복제 의미론과 소유가 겹치지 않는다.
- `bash`는 셸을 통해 어떤 경로든 닿으므로 이 훅의 사정권 밖이고, 심볼릭 링크로도 우회된다. 두 한계는 구현 누락이 아니라 이 정책이 감당하지 않는 범위다.

### 세션 확장 상태는 기록 로그 안에서만 는다

- 세션 확장 상태는 세션 기록 로그 안에 두고, `KnownEventKind`의 기존 8종과 `AuxiliaryRecordedPayload`의 기존 4변형을 깨지 않는 방향으로만 확장한다: `crates/session-store/src/schema.rs`.
- 1단계는 기존 `auxiliary_recorded` 종류의 `Legacy { schema_version, value }` 봉투 안에 넣는다. 이 변형은 옛 빌드도 정상적으로 디코드하고 투영에서는 무시된다.
- 2단계는 지원하는 리더가 모두 새 종류를 읽을 수 있게 된 뒤에만 쓴다. 그때 `ALL` 배열 길이, `as_str`과 `parse`, `typed_payload_ok`, `Payload`, `Projection` 필드, 세션 이벤트 형식 표를 같은 커밋에서 함께 고친다. 새 변형을 먼저 쓰고 리더를 나중에 배포하면 옛 빌드가 그 세션을 열지 못한다.
- 확장 상태를 `ProjectedMessage`에 밀어 넣지 않는다. 그러면 모델 대화 기록에 섞여 도구 검증과 압축 경계 계산이 그 항목을 대화로 센다. 별도 필드와 종류별 접근자로 둔다.

### 와이어 버전 상수는 소유 크레이트와 함께 적는다

- 와이어 버전 상수는 한 계약으로 모으고, 각 상수의 소유 크레이트를 이름 옆에 적는다. 지금은 네 상수가 두 크레이트에 흩어져 있고 통합 타입은 아직 코드에 없다.
- `REPLICATION_PROTOCOL_VERSION`은 이름이 같고 값도 같지만 소유가 둘이다. 저장소 쪽 원본 이벤트 교환 계약은 `crates/session-store/src/replication.rs`, 브리지 쪽 같은 이름의 두 번째 정의는 `crates/harness-api/src/replication.rs`다.
- 하네스 API 프레임의 major와 minor는 `API_VERSION_MAJOR`와 `API_VERSION_MINOR`이고 둘 다 `crates/harness-api/src/lib.rs`가 소유한다. 지금 프레임의 `v`에는 major만 실리므로 minor가 올라가도 클라이언트가 알 수 없다. 통합할 때는 major와 minor를 한 쌍으로 싣고, 이름이 같은 두 복제 상수는 `STORE_REPLICATION_VERSION`과 `REPLICATION_VERSION`으로 갈라 값의 일치를 테스트가 단언하게 한다.
- MCP는 이 계약에 넣지 않는다. MCP `protocol_version`은 상대 서버가 정하는 값이라 우리가 올리거나 내릴 수 없다.

### IRC 버스 크레이트는 동기화 원시만 쓴다

`horon-agent-irc-core`(crate 경로 `crates/irc-core/`)는 프로세스 안에서만 도는 메일박스 버스를 소유한다. 서버 상태, 소켓, 설정, 디스크를 만지지 않고 `Mutex`와 `oneshot` 같은 동기화 원시와 `Arc` 공유만 쓴다. 의존성도 `tokio`의 `sync`, `time`, `macros`, `rt` 기능과 `futures`로 제한한다(`crates/irc-core/Cargo.toml`). 런타임을 만들지 않으므로 서버 없이 단위 테스트와 처리량 예제를 돌릴 수 있고, 서버 쪽 연결은 `crates/app-core/src/server/irc/`가 소유한다.

이 배치는 "타입 크레이트에는 전역 상태와 런타임 의존성을 넣지 않는다"는 소유 규칙의 예외다. 버스는 데이터 계약이 아니라 동작을 가진 작은 엔진이고, 그 동작을 서버 크레이트에 두면 서버 없이 검증할 수 없기 때문이다. 예외를 좁게 유지하려고 두 가지를 지킨다.

- 버스 본체(`crates/irc-core/src/bus.rs`)는 전역을 읽지 않는다. 데몬은 `crates/app-core/src/server/irc/mod.rs`의 `install`이 리스너를 bind한 뒤 한 번 설치하고, 데몬 실행이 끝날 때 지운다.
- 전역 접근은 `crates/irc-core/src/global.rs`의 세 함수로만 열고, 세션 제거 정리는 `crates/app-core/src/server/session_forget.rs`가 버스의 `forget`을 호출해 맡는다.

새 기능이 서버 세션 상태, 소켓, 디스크를 필요로 하면 이 크레이트에 넣지 않고 `crates/app-core/src/server/irc/`로 넓힌다. 반대로 순수한 큐·표·순서 규칙은 이 크레이트에 남긴다.

## "충분히 최적"의 정의

다음 조건을 만족하면 구조가 충분히 좋다.

- 각 타입 크레이트에 뚜렷한 도메인과 최소한의 의존성 집합이 있다.
- `horon-agent-core`에는 진짜 기본 요소나 문서화된 임시 보관 모듈만 들어 있다.
- 루트 모듈이 큰 DTO 덩어리, 영속화, 런타임 조율, 렌더링을 한 파일에 더 이상 섞지 않는다.
- 모든 도메인에 좁은 검증 명령이 있다.
- 구조를 바꿀 때마다 selfdev 빌드와 재적재가 깨끗하게 동작한다.
