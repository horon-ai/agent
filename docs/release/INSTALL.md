# 릴리스 설치

설치 스크립트는 릴리스 자산을 내려받은 뒤 압축을 직접 풀지 않고 에이전트 실행 파일의 `install-release`에 넘긴다. 검증, 압축 해제, 변경 불가 버전 디렉터리 생성, 채널 활성화를 한 경로에서 처리하려는 것이다 (`src/cli/install_release.rs`, `crates/app-core/src/update.rs` `install_release_from_directory`).

```bash
horon-agent install-release --assets <디렉터리> --agent-version <버전>
```

| 플래그 | 뜻 |
| --- | --- |
| `--assets <DIR>` | 릴리스 자산이 든 실제 디렉터리. `SHA256SUMS`와 플랫폼 자산이 있어야 한다. |
| `--agent-version <VERSION>` | 자산이 속한 릴리스 버전. 변경 불가 버전 디렉터리 이름이 된다. |
| `--source-binary <PATH>` | 내려받은 자산 대신 이 로컬 빌드 실행 파일을 설치한다. |

## 검증

`crates/app-core/src/update/release_asset.rs`가 자산을 스테이징 단계에서 확인한다.

- 자산 디렉터리는 심볼릭 링크가 아닌 실제 디렉터리여야 하고, `SHA256SUMS`를 반드시 읽는다. 파일이 없거나 일반 파일이 아니면 실패한다.
- `--source-binary`를 주지 않으면 플랫폼 자산의 SHA-256을 `SHA256SUMS`와 대조한다. 값이 다르거나 해시 형식이 틀리면 아무것도 설치하지 않는다.
- `--source-binary`를 주면 로컬에서 컴파일한 실행 파일을 소스로 신뢰하고 게시된 바이트와 비교하지 않는다. `SHA256SUMS`는 이때도 필수다.
- 버전 문자열은 영숫자와 `.`, `-`, `+`만 허용한다. 경로 구분자나 `..`가 들어간 이름은 거부한다.
- 압축 자산은 일반 파일과 디렉터리만 푼다. 심볼릭 링크, 하드 링크, 절대 경로, `..` 탈출, 중복 경로를 거부하고 파일 하나당 512 MiB, 전체 2 GiB 예산을 넘기면 실패한다. 압축 안에는 최상위 플랫폼 실행 파일이 정확히 하나 있어야 한다.
- 이미 같은 버전 디렉터리가 있으면 설치된 실행 파일의 SHA-256을 먼저 비교한다. 바이트가 같으면 그 버전을 재사용하고, 다르면 `the installed agent version already contains different bytes`로 거부하며 기존 파일을 덮어쓰지 않는다.

검증이 끝나면 `stable`, `current`, 런처 링크와 업데이트 메타데이터를 함께 바꾼다. 중간 단계가 실패하면 롤백하므로 이전 설치가 그대로 남는다 (`crates/app-core/src/update/activation.rs`). 명령은 네트워크에서 아무것도 내려받지 않는다. 자산을 준비하는 일은 호출자 몫이다.

## 설치 스크립트와의 관계

`scripts/install/install.sh`와 `scripts/install/install.ps1`은 릴리스 자산과 `SHA256SUMS`를 임시 디렉터리에 받은 뒤 이 명령을 호출한다. `scripts/install/install_from_source.sh`는 같은 명령에 `--source-binary`를 붙여 로컬 빌드를 설치한다. 그래서 셸과 PowerShell이 서로 다른 설치 레이아웃을 만들지 않고, 실패하면 스크립트도 이전 설치를 건드리지 않는다.

Windows 설치 경로와 서명 절차는 [`WINDOWS.md`](WINDOWS.md)에 있다.

## Linux 서버에서 실행

릴리스 바이너리는 glibc, libm, libgcc_s만 있으면 뜬다. OpenSSL과 다른 패키지는 필요 없다. TLS는 전부 rustls로 처리하고 OpenSSL은 의존성 그래프에 없다 (`cargo tree -p horon-agent -i openssl-sys --target all`이 아무것도 찾지 않는다). IMAP 메일 알림을 보내는 `crates/notify-email`도 `imap`의 `rustls-tls` 기능으로 rustls를 쓴다.

### 인증서

모델 HTTPS 호출은 reqwest가 내장한 webpki 루트를 쓴다. 그래서 `ca-certificates` 없이도 `serve`, `run`, `api-bridge`가 동작한다. IMAP 메일 알림 경로만 시스템 CA 저장소를 읽는다. 메일 알림을 쓰는 서버는 `ca-certificates` 패키지를 설치한다. rustls는 TLS 1.2와 1.3만 받으므로 TLS 1.0·1.1만 지원하는 IMAP 서버에는 연결하지 못한다.

### 준비

TTY, 화면, `XDG_RUNTIME_DIR`이 필요 없다. `XDG_RUNTIME_DIR`이 없으면 `HORON_AGENT_RUNTIME_DIR`을 먼저 보고, 그것도 없으면 임시 디렉터리 아래 `horon-agent-<사용자 구분자>`를 쓴다 (`crates/harness-api/src/sockets.rs:16-31`). 사용자 구분자는 `UID`, `USER` 순으로 읽어 영숫자와 `-`, `_`만 남기고 64자로 자른다. 남는 글자가 없으면 `user`다.

provider 프로파일을 `config.toml`에 둔다. 위치는 `HORON_AGENT_HOME`이 있으면 `$HORON_AGENT_HOME/config.toml`, 없으면 `~/.horon-agent/config.toml`이다. 키와 파일 위치는 [`../provider/MODEL.md`](../provider/MODEL.md)에 있다.

```toml
[provider]
default_provider = "my-gw"

[providers.my-gw]
type = "openai-compatible"
base_url = "https://llm.example.com/v1"
api_key_env = "MY_GW_API_KEY"
default_model = "my-model"
```

| 하려는 일 | 명령 |
| --- | --- |
| 한 번 묻고 끝내기 | `horon-agent --no-update run "..."` |
| 데몬으로 상주 | `horon-agent serve` |
| SDK 소켓 열기 | `horon-agent api-bridge` |

TypeScript·Python SDK의 `launch()`는 Linux에서 자기 인스턴스를 직접 띄운다. 미리 `serve`를 실행해 둘 필요가 없다 (`sdk/typescript/README.md`).

### 릴리스 자산

| 자산 | 빌드 |
| --- | --- |
| `horon-agent-linux-x86_64` | CentOS 7 / manylinux2014 컨테이너에서 glibc 2.17 기준으로 빌드한다 (`scripts/release/build_linux_compat.sh`). |
| `horon-agent-linux-aarch64` | `ubuntu-24.04-arm`에서 빌드한다 (`.github/workflows/release.yml`). |

아카이브에는 래퍼와 `<artifact>.bin`만 들어 있고 OpenSSL 라이브러리는 더 이상 들어가지 않는다. `scripts/install/install.sh`는 예전 아카이브도 설치하도록 `libssl.so*` 멤버를 계속 받는다.

### CI 가드

`scripts/ci/check_linux_bare_runtime.sh <binary>`가 바이너리를 `debian:trixie-slim`에서 돌린다. 컨테이너에서 OpenSSL 라이브러리와 CA 저장소를 지우고, `ldd` 결과가 glibc 계열과 libgcc 허용 목록 안에 있는지 확인한다. 이어서 `--version`과 모의 프로바이더를 상대로 `run` 한 턴을 실행한다. CI `build` 작업의 Linux 항목이 release 프로필 빌드 뒤에 이 스크립트를 돌린다. 이 단계는 GitHub 호스팅 러너에서만 돈다 (`CI_SELF_HOSTED`를 켜면 macOS 항목만 남는다). Docker가 있으면 개발자 기계에서도 같은 명령을 돌릴 수 있다.
