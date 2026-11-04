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
