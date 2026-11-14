# 릴리스 설치

언어: [English](INSTALL.md) | **한국어**

설치 스크립트는 릴리스 자산을 내려받은 뒤 압축을 직접 풀지 않고 에이전트 실행 파일의 `install-release`에 넘긴다. 검증, 압축 해제, 변경 불가 버전 디렉터리 생성, 채널 활성화를 한 경로에서 처리하려는 것이다 (`src/cli/install_release.rs`, `crates/app-core/src/update.rs` `install_release_from_directory`).

```bash
horon install-release --assets <디렉터리> --agent-version <버전>
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
- 압축 자산은 일반 파일과 디렉터리만 푼다. 심볼릭 링크, 하드 링크, 절대 경로, `..` 탈출, 중복 경로를 거부하고 파일 하나당 512 MiB, 전체 2 GiB 예산을 넘기면 실패한다. 압축 안에는 최상위 플랫폼 실행 파일이 정확히 하나 있어야 하고, 배포하는 모든 압축에는 최상위 `LICENSE`도 들어 있으며 이 파일은 실행 파일 옆에 문서로 설치된다. 그 밖의 멤버는 거부한다.
- 이미 같은 버전 디렉터리가 있으면 설치된 실행 파일의 SHA-256을 먼저 비교한다. 바이트가 같으면 그 버전을 재사용하고, 다르면 `the installed agent version already contains different bytes`로 거부하며 기존 파일을 덮어쓰지 않는다.

검증이 끝나면 `stable`, `current`, 런처 링크와 업데이트 메타데이터를 함께 바꾼다. 중간 단계가 실패하면 롤백하므로 이전 설치가 그대로 남는다 (`crates/app-core/src/update/activation.rs`). 명령은 네트워크에서 아무것도 내려받지 않는다. 자산을 준비하는 일은 호출자 몫이다.

## 설치 스크립트와의 관계

`scripts/install/install.sh`와 `scripts/install/install.ps1`은 릴리스 자산과 `SHA256SUMS`를 임시 디렉터리에 받은 뒤 이 명령을 호출한다. `scripts/install/install_from_source.sh`는 같은 명령에 `--source-binary`를 붙여 로컬 빌드를 설치한다. 그래서 셸과 PowerShell이 서로 다른 설치 레이아웃을 만들지 않고, 실패하면 스크립트도 이전 설치를 건드리지 않는다.

Windows 설치 경로와 서명 절차는 [`WINDOWS.ko.md`](WINDOWS.ko.md)에 있다.

## npm에서 설치 (아직 게시되지 않음)

npm 배포본은 만들어지고 시험되지만 게시된 적이 없다. 레지스트리에는 일곱 이름을 선점하는 `0.0.0` 자리표시 패키지만 있고 그 패키지에는 실행 파일이 없다. 그래서 지금 `npm i -g @horon-ai/horon`을 실행하면 런타임을 찾지 못하는 런처가 설치된다. 게시가 켜지기 전까지는 위의 설치 스크립트, 컨테이너 이미지, 소스 체크아웃 가운데 하나를 쓴다.

게시가 켜지면 npm의 `@horon-ai/horon`은 릴리스 자산을 내려받지 않고 같은 두 실행 파일을 설치한다. 작은 런처 하나와 같은 버전으로 고정된 플랫폼 패키지 하나이고, npm이 `optionalDependencies`로 플랫폼 패키지를 골라 전역 설치가 컴퓨터에 맞는 하나만 받는다.

```bash
npm i -g @horon-ai/horon
npx @horon-ai/horon --help
```

npm 설치본은 에이전트 자체 업데이터가 아니라 npm으로 갱신한다. 패키지가 두 실행 파일의 한 릴리스를 함께 고정하므로 런처가 시작하는 실행 파일에 `HORON_AGENT_NO_AUTO_UPDATE=1`을 넣기 때문이다. 패키지 구성, 플랫폼 패키지가 없을 때 나오는 오류, 버전 규칙, 게시 절차는 [`NPM.ko.md`](NPM.ko.md)에 있다.

## 선택 사항인 터미널 클라이언트

릴리스는 대화형 클라이언트도 에이전트 옆의 별도 압축 파일로 배포한다. 이름은 `horon-tui-<접미사>.tar.gz`이고 접미사는 에이전트 자산과 같은 플랫폼 접미사다(`horon-tui-linux-x86_64.tar.gz`, `horon-tui-macos-aarch64.tar.gz`, `horon-tui-windows-x86_64.tar.gz` 등). `release_asset.rs`가 에이전트 자산 이름에서 이 이름을 만들며, 접미사가 둘을 잇는다.

`scripts/install/install.sh`와 `scripts/install/install.ps1`은 이 압축 파일을 에이전트 자산과 같은 임시 디렉터리로 받아 `SHA256SUMS`와 대조한 뒤 그 디렉터리를 `install-release`에 넘긴다. 검증을 마친 클라이언트는 `builds/versions/<version>/`의 `horon-agent` 옆에 놓이고 각 압축이 담은 `LICENSE`도 같은 곳에 설치되므로, 한 버전 디렉터리가 두 실행 파일과 그 실행 파일을 규율하는 라이선스를 들고 활성화가 함께 바꾼다. 클라이언트 설치 방법은 [`../runtime/TUI.ko.md`](../runtime/TUI.ko.md)에 있다.

클라이언트는 모든 단계에서 선택 사항이고, 검증하지 못한 사본은 에이전트 설치를 막지 않는다.

| 상황 | 결과 |
| --- | --- |
| 릴리스가 그 플랫폼용 압축 파일을 배포하지 않음 | 한 줄을 남기고 `horon-agent`만 설치한다 |
| `SHA256SUMS`에 그 압축 파일 항목이 없음 | 한 줄을 남기고 `horon-agent`만 설치한다 |
| 다이제스트가 다름 | 한 줄을 남기고 `horon-agent`만 설치한다 |
| `--source-binary`, 또는 FreeBSD | 배포된 클라이언트를 넣지 않는다. FreeBSD는 `horon-agent`만 배포한다 |
| 버전 디렉터리가 이미 있음 | 파일이 없을 때만 클라이언트를 더한다. 그 밖에는 버전 디렉터리를 바꾸지 않는다 |

`scripts/install/uninstall.sh`와 `scripts/install/uninstall.ps1`은 설치 디렉터리의 `horon-tui` 런처를 다른 대상과 함께 목록에 올리고 지운다. 예전 설치가 남긴 런처도 포함한다.

## Linux 서버에서 실행

릴리스 바이너리는 glibc, libm, libgcc_s만 있으면 뜬다. OpenSSL과 다른 패키지는 필요 없다. TLS는 전부 rustls로 처리하고 OpenSSL은 의존성 그래프에 없다 (`cargo tree -p horon-agent -i openssl-sys --target all`이 아무것도 찾지 않는다).

### 인증서

모델 HTTPS 호출이 신뢰하는 루트는 바이너리에 내장한 webpki 루트와 OS가 신뢰하는 루트의 합집합이다. 그래서 `ca-certificates`가 없어도 `serve`, `run`, `api-bridge`가 동작하고, OS 저장소에 설치한 사설 CA도 신뢰한다.

- macOS와 Windows는 `rustls-platform-verifier`로 OS 검증기를 쓴다. 키체인이나 인증서 저장소의 인증서를 프로세스 시작 때 전부 읽지 않는다.
- Linux와 나머지 Unix는 `rustls-native-certs`로 OS 저장소(`/etc/ssl/certs` 등)를 프로세스당 한 번 읽어 webpki 루트와 합친다. 저장소가 없는 이미지에서도 webpki 루트만으로 연결된다.
- 설정은 프로세스당 한 번만 만들고 모든 HTTPS 클라이언트가 공유한다. ALPN은 `h2`와 `http/1.1`을 제시한다.

신뢰 집합을 좁히거나 검증을 끄는 설정은 없다. 사내 CA를 쓰려면 그 CA를 OS 저장소에 넣는다. 구현은 [`../architecture/CRATES.ko.md`](../architecture/CRATES.ko.md)의 "TLS 신뢰 집합"에 적혀 있다.

### 준비

TTY, 화면, `XDG_RUNTIME_DIR`이 필요 없다. 소켓 디렉터리는 `HORON_AGENT_RUNTIME_DIR`, `XDG_RUNTIME_DIR`, 플랫폼별 사용자 런타임 디렉터리 순으로 정하고, 셋 다 없으면 임시 디렉터리 아래 `horon-agent-<사용자 구분자>`를 쓴다 (`crates/harness-api/src/sockets.rs:16-31`). 사용자 구분자는 `UID`, `USER` 순으로 읽어 영숫자와 `-`, `_`만 남기고 64자로 자른다. 남는 글자가 없으면 `user`다.

provider 프로파일을 `config.toml`에 둔다. 위치는 `HORON_HOME`이 있으면 `$HORON_HOME/config.toml`, 없으면 `~/.horon/config.toml`이다. 키와 파일 위치는 [`../provider/MODEL.ko.md`](../provider/MODEL.ko.md)에 있다.

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
| 한 번 묻고 끝내기 | `horon --no-update run "..."` |
| 데몬으로 상주 | `horon serve` |
| SDK 소켓 열기 | `horon api-bridge` |

TypeScript·Python SDK의 `launch()`는 Linux에서 자기 인스턴스를 직접 띄운다. 미리 `serve`를 실행해 둘 필요가 없다 (`packages/sdk/typescript/README.md`).

### 릴리스 자산

| 자산 | 빌드 |
| --- | --- |
| `horon-agent-linux-x86_64` | CentOS 7 / manylinux2014 컨테이너에서 glibc 2.17 기준으로 빌드한다 (`scripts/release/build_linux_compat.sh`). |
| `horon-agent-linux-aarch64` | `ubuntu-24.04-arm`에서 빌드한다 (`.github/workflows/release.yml`). |

아카이브에는 래퍼, `<artifact>.bin`, `LICENSE`가 들어 있고 OpenSSL 라이브러리는 더 이상 들어가지 않는다. `scripts/install/install.sh`는 예전 아카이브도 설치하도록 `libssl.so*` 멤버를 계속 받는다.

### CI 가드

`scripts/ci/check_linux_bare_runtime.sh <binary>`가 바이너리를 `debian:trixie-slim`에서 돌린다. 컨테이너에서 OpenSSL 라이브러리와 CA 저장소를 지우고, `ldd` 결과가 glibc 계열과 libgcc 허용 목록 안에 있는지 확인한다. 이어서 `--version`과 모의 프로바이더를 상대로 `run` 한 턴을 실행한다. CI `build` 작업의 Linux 항목이 release 프로필 빌드 뒤에 이 스크립트를 돌린다. 이 단계는 GitHub 호스팅 러너에서만 돈다 (`CI_SELF_HOSTED`를 켜면 macOS 항목만 남는다). Docker가 있으면 개발자 기계에서도 같은 명령을 돌릴 수 있다.

## 릴리스 번호와 자동 발행

루트 `Cargo.toml`의 `[package].version`이 버전의 단일 출처다. 이 값은 릴리스 라인(MAJOR.MINOR)과 그 라인의 가장 낮은 patch를 정하고, 이후 patch 번호는 태그가 매긴다. 버전을 올리는 커밋은 만들지 않는다.

| 단계 | 하는 일 |
| --- | --- |
| main push | `ci.yml`이 그 커밋을 검사한다. |
| CI 성공 | `release-tag.yml`이 `scripts/release/next_version.py`로 다음 태그를 고른다. 같은 MAJOR.MINOR의 가장 높은 태그에 patch를 1 더하고, 그 라인에 태그가 없으면 `Cargo.toml` 버전을 그대로 쓴다. 이미 태그된 커밋과 최신 릴리스보다 오래된 커밋은 건너뛴다. |
| 태그 생성 | 같은 실행이 그 커밋에 태그를 만들고 `release.yml`을 `workflow_call`로 부른다. 이후 검사(정확한 커밋의 CI 성공, 드래프트, 플랫폼 빌드, `SHA256SUMS`, 공개, 설치 확인)는 태그를 직접 push할 때와 같다. |
| 이미지 게시 | 공개된 릴리스의 Linux 자산을 `SHA256SUMS`로 확인한 뒤 컨테이너 이미지로 묶어 올린다. |

`prepare_source.py`는 태그가 `Cargo.toml`과 MAJOR.MINOR가 같고 patch가 그 이상일 때만 빌드를 허락한다. 새 MINOR나 MAJOR를 내려면 `Cargo.toml`과 `Cargo.lock`의 루트 버전만 올려 main에 병합한다. 저장소 변수 `RELEASE_AUTO`를 `false`로 두면 태그를 만들지 않는다. `release-tag.yml`을 수동 실행하면 만들 태그만 보고하고 아무것도 만들지 않는다.

업데이트 확인은 릴리스 버전이 현재 버전보다 높을 때만 갱신한다. 그래서 0.98.x로 직접 빌드한 설치본은 0.1.x 릴리스를 새 버전으로 보지 않는다. 이런 설치본은 `scripts/install/install.sh`로 한 번 다시 설치한다.

## 컨테이너 이미지

릴리스마다 `ghcr.io/horon-ai/agent`에 `linux/amd64`, `linux/arm64` 이미지를 `<버전>`, `<MAJOR.MINOR>`, `latest` 태그로 올린다. GitHub Packages는 cargo와 PyPI 레지스트리를 지원하지 않고, SDK는 npm과 PyPI로 내보낼 예정이므로 GitHub Packages에는 헤드리스 데몬을 바로 띄우는 컨테이너만 올린다. 이미지는 다시 컴파일하지 않고 릴리스 자산을 그대로 넣는다 (`scripts/release/Containerfile`).

```bash
docker run --rm ghcr.io/horon-ai/agent:latest --version
```

## 런처 별칭 (`horon`, `hrn`)

`horon-agent` 런처와 같은 디렉터리에 `horon`과 `hrn`도 만든다. 이 별칭은 v0.1.3부터 설치된다. 문서와 예시는 `horon`을 쓰고, `hrn`은 더 짧은 형태다. 셋 다 같은 채널 실행 파일을 가리키며, 유닉스에서는 심볼릭 링크, Windows에서는 복사본이다 (`crates/build-support/src/paths.rs` `update_launcher_symlink`). 업데이트 때마다 런처와 함께 바뀐다. Termux에서는 `install.sh`가 별칭도 `LD_PRELOAD`를 지우는 래퍼로 연결한다.

- 같은 이름의 다른 프로그램은 덮어쓰지 않는다. 유닉스는 `horon-agent`를 가리키는 심볼릭 링크만, Windows는 이전 런처와 바이트가 같은 복사본만 우리 것으로 보고 교체한다. 그 밖의 파일이 있으면 별칭 없이 설치가 끝난다.
- `uninstall.sh`와 `uninstall.ps1`도 같은 기준으로 별칭만 지운다.
- 도움말과 `--version`은 어느 이름으로 실행해도 `horon-agent`로 표시한다(`#[command(name = "horon-agent")]`).
- 데이터 디렉터리는 `~/.horon`(`HORON_HOME`)이고 프로젝트 안 설정은 `.horon/`이다. v0.1.1까지의 릴리스는 `~/.horon-agent`를 썼고, 지금 코드는 그 경로를 읽지 않는다.
