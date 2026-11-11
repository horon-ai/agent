# npm으로 `horon` 설치

언어: [English](NPM.md) | **한국어**

`@horon-ai/horon`은 CLI의 npm 배포본이다. 작은 런처 하나와, 플랫폼마다 `horon-agent`·`horon-tui` 실행 파일을 담은 패키지 하나로 이루어진다. 런처는 실행 중인 컴퓨터에 맞는 패키지의 실행 파일을 띄우고, npm은 다른 의존성과 마찬가지로 설치된 버전을 고정한다. 릴리스 태그를 고르는 일과 릴리스 자산을 내려받는 설치 스크립트는 [`INSTALL.ko.md`](INSTALL.ko.md)에 있다.

## 패키지 구성

일곱 이름은 래퍼 하나와 플랫폼 패키지 여섯으로 나뉜다. 래퍼에는 실행 파일이 없다.

| 패키지 | 내용 |
| --- | --- |
| `@horon-ai/horon` | 런처 `bin/horon.js`와 README. `bin`이 `horon`과 `hrn`을 모두 이 파일에 연결한다. |
| `@horon-ai/horon-linux-x64` | Linux x86_64용 `bin/horon-agent`와 `bin/horon-tui`. |
| `@horon-ai/horon-linux-arm64` | Linux aarch64용 같은 두 파일. |
| `@horon-ai/horon-darwin-x64` | macOS Intel용 같은 두 파일. |
| `@horon-ai/horon-darwin-arm64` | macOS Apple Silicon용 같은 두 파일. |
| `@horon-ai/horon-win32-x64` | Windows x86_64용 `bin/horon-agent.exe`와 `bin/horon-tui.exe`. |
| `@horon-ai/horon-win32-arm64` | Windows ARM64용 같은 두 파일. |

npm은 `optionalDependencies`로 플랫폼 패키지를 고르므로, 전역 설치가 컴퓨터에 맞는 하나만 내려받는다. 다음 세 가지가 이 묶음을 안전하게 게시할 수 있게 한다.

- 래퍼의 플랫폼 고정값은 범위가 아니라 정확한 버전이다. 게시하는 버전과 고정값이 다르면 `scripts/npm/release_npm_version.py check-packages`가 실행을 실패시키므로, 래퍼가 그 버전에 없는 플랫폼 패키지를 가리키는 일이 생기지 않는다.
- 어느 패키지도 설치 스크립트를 선언하지 않는다. 생성된 매니페스트와 `npm/horon/package.json`에 `preinstall`, `install`, `postinstall`이 없고, 같은 검사가 의존성을 선언한 플랫폼 패키지도 거부한다.
- Node 라이브러리가 아니다. `npm/horon/bin/horon.js`는 `process.platform`과 `process.arch`를 읽어 맞는 패키지의 디렉터리를 찾고, 넘겨받은 인자와 환경을 그대로 붙여 `bin/horon-agent`를 `spawnSync`와 `stdio: "inherit"`로 띄운다. 실행 중에는 자식이 터미널을 소유하고, 인터럽트가 CLI에 닿도록 런처는 그동안 `SIGINT`와 `SIGTERM`을 무시한다.

런처는 자식 환경에 `HORON_AGENT_NO_AUTO_UPDATE=1`을 넣는다. npm 패키지가 두 실행 파일의 한 릴리스를 함께 고정하는데 에이전트 자체 업데이터는 다른 빌드로 바꾸므로, npm 설치본은 npm으로 갱신한다.

## 설치와 실행

```bash
npm i -g @horon-ai/horon
horon --version
npx @horon-ai/horon --help
```

`npm i -g`는 래퍼와 맞는 플랫폼 패키지를 설치한다. `npx`는 전역 설치 없이 임시 설치본에서 같은 런처를 실행한다. 전역 설치 뒤에 인자 없이 `horon`을 실행하면 터미널 클라이언트가 열리고, 나머지 하위 명령은 모두 헤드리스 에이전트로 간다. `hrn`은 같은 런처의 짧은 이름이다.

플랫폼 패키지가 없으면 고장 난 설치가 아니라 다시 설치할 일이다. 런처는 `horon:` 한 줄을 출력하고 종료 코드 1로 끝난다.

| 상황 | 런처가 출력하는 내용 |
| --- | --- |
| npm을 `--omit=optional`로 실행했거나 패키지가 불완전하게 설치됨 | `horon: the platform package @horon-ai/horon-darwin-arm64 is not installed. reinstall with optional dependencies enabled: npm install -g --include=optional @horon-ai/horon` |
| 여섯 플랫폼에 없는 환경 | `horon: <platform>-<arch> is not a supported platform.` 뒤에 지원하는 여섯 키와 같은 재설치 안내 |

## 버전이 정해지는 방식

릴리스의 npm 버전은 릴리스 태그에서 `v`를 뺀 값이다. `scripts/npm/release_npm_version.py tag-version`은 `vMAJOR.MINOR.PATCH`만 받으므로, 프리릴리스나 빌드 접미사가 붙은 태그는 아무것도 내려받기 전에 거부된다. `dispatch` 명령은 수동 실행에 같은 규칙을 적용한다.

`npm/horon/package.json`은 git에서 `0.0.0`을 유지하고 올리지 않는다. `scripts/npm/prepare_cli_packages.sh <out-dir> <assets-dir> <version>`가 매니페스트와 런처를 스테이징 디렉터리로 복사하고, 그 복사본에 릴리스 버전과 플랫폼 고정값 여섯을 쓴다. 그래서 추적되는 파일은 항상 자리 표시 상태를 설명하고, 게시되는 패키지는 담고 있는 자산의 릴리스 버전을 단다.

## 게시 워크플로

`.github/workflows/publish-npm.yml`이 레지스트리로 가는 유일한 경로다. 세 진입점이 이 파일에 닿고, 그중 하나만 게시할 수 있다.

| 진입점 | 실행 시점 | 하는 일 |
| --- | --- | --- |
| `workflow_run` | `main`의 "Release Tag" 실행이 끝났을 때 | 릴리스한 커밋을 가리키는 태그를 찾고, 공개 릴리스인지 확인한 뒤 태그 버전으로 게시한다 |
| `workflow_dispatch` | 운영자가 릴리스 태그를, 또는 `package_only` Release 실행과 버전을 지정했을 때 | 기본은 드라이런이고, 스위치가 켜져 있으면 `dry_run=false`가 게시한다 |
| `pull_request` | `npm/`, `scripts/npm/`, 워크플로 파일이 바뀌었을 때 | 가짜 자산을 묶고 같은 드라이런 흐름을 돌린다 |

워크플로는 npm 11.5.1 이상을 요구한다. provenance 서명과 trusted publishing에 이 버전이 필요하다. 계획 잡은 `contents: read`를 들고 끝난 실행의 잡을 읽으며, `id-token: write`는 게시 잡만 받는다. 이 권한이 주는 단명 자격 증명을 npm이 한 번짜리 게시 허가로 바꾼다. `cancel-in-progress: false`인 동시성 그룹은 두 번째 실행을 취소하지 않고 줄 세우므로, 뒤 실행이 레지스트리를 다시 읽고 이미 올라간 것은 건너뛴다.

다음 조건이 모두 성립할 때만 게시 실행이 된다. 이벤트가 pull request가 아니고, 계획 잡이 `mode=publish`로 정했고, 저장소 변수 `NPM_PUBLISH`가 정확히 `true`이다. 자동 경로는 여기에 더해 `horon-ai/agent`의 `main`에서 성공한 "Release Tag" 실행, 그 실행의 헤드 커밋을 가리키는 태그, 초안이 아닌 공개 릴리스를 요구한다.

두 흐름 모두 `scripts/npm/release_npm_version.py publish-order`가 출력하는 순서대로 플랫폼 패키지 여섯을 먼저, 래퍼를 마지막에 게시한다. 래퍼가 선택적 의존성을 이름으로 찾기 때문이다. 실제 경로는 게시 전에 `npm view <name>@<version> version`으로 그 버전이 이미 있는지 묻고, 답이 오면 건너뛰므로 재실행이 일부만 올라간 묶음을 실패 없이 마무리한다. 실행 끝에는 모든 `name@version`을 최대 10분 동안 확인한 뒤 래퍼의 `optionalDependencies`를 출력한다. 그래서 성공한 실행 자체가 일곱이 함께 올라갔다는 증거가 된다.

드라이런은 패키지를 `npm pack`으로 묶은 뒤 `npm publish <tarball> --access public --provenance --dry-run`을 돌리고, 실행 전후의 레지스트리를 비교한다. pull request 경로는 여기에 가짜 버전 `0.0.1`을 쓴다. npm 11.5.1은 이미 레지스트리에 있는 버전의 드라이런 게시를 거부하고, 일곱 이름 모두 `0.0.0` 자리 패키지를 들고 있으며, 프리릴리스 버전이라면 `--tag`를 따로 줘야 하기 때문이다.

중간에 멈춘 릴리스를 다시 시작하려면 같은 태그로 다시 디스패치한다. 이미 있는 플랫폼 패키지와 래퍼를 건너뛰므로 게시 잡은 여러 번 실행해도 같은 결과에 이른다.

```bash
gh workflow run publish-npm.yml -f release_tag=vX.Y.Z -f dry_run=false
```

## 첫 릴리스 전 한 번만 하는 준비

이 단계는 한 번만 하며, npm 조직 소유권과 저장소 설정이 필요하므로 보통 릴리스 절차에 들지 않는다.

1. 일곱 이름이 각각 Trusted Publisher를 달려면 먼저 레지스트리에 있어야 하는데, npm은 게시하지 않고는 이름을 가져올 방법이 없다. 읽기만 하는 `npm view @horon-ai/horon-linux-x64 version`으로 이미 있는 이름을 확인하고, 답이 오는 이름에는 자리 패키지가 필요 없다. 아직 없는 이름은 `scripts/npm/prepare_placeholders.sh <out-dir>`가 이름마다 폴더를 만들고, 그 안에 `0.0.0` 매니페스트와 설명, 저장소, 한 줄 README를 쓴다. 설치 스크립트도, `bin`도, 실행 파일도 넣지 않으며 실행할 명령만 출력한다. 그 폴더는 2단계 인증을 걸고 직접 게시한다. 스크립트 자체는 게시하지 않는다.

   ```bash
   bash scripts/npm/prepare_placeholders.sh /tmp/horon-placeholders
   npm publish /tmp/horon-placeholders/horon-linux-x64 --access public
   ```

2. 일곱 패키지 각각에 Trusted Publisher를 등록한다. npm → 패키지 → Settings → Trusted Publisher → GitHub Actions에서 저장소 `horon-ai/agent`, 워크플로 `publish-npm.yml`, 환경은 비워 둔다. 같은 게시자에게 `npm publish`도 허용해야 한다. 2026-09-03 이후에 만든 패키지 설정은 기본이 단계적 게시 전용이므로, 게시 권한을 켜지 않으면 단계적 승인 흐름을 쓴다.
3. 게시자는 첫 릴리스 직전에 등록한다. 등록한 뒤 2일 안에 OIDC 게시가 성공하지 않으면 설정이 만료된다.
4. 저장소 변수 `NPM_PUBLISH`를 `true`로 둔다. 그 전에는 `dry_run=false`인 디스패치까지 모든 경로가 드라이런으로 남는다.
5. 첫 게시가 성공한 뒤 Publishing access를 "Require two-factor authentication and disallow tokens"로 바꾼다. 워크플로 어디에도 npm 토큰을 저장하지 않고, 게시 잡은 GitHub OIDC로만 인증한다.

## provenance 확인

게시 잡이 `--provenance`를 붙이므로 npm은 각 tarball을 만든 워크플로 실행과 커밋에 서명된 진술로 묶어 기록한다. 레지스트리에서 다시 읽는다.

```bash
npm view @horon-ai/horon --json | jq '.dist.attestations'
```

플랫폼 패키지의 같은 필드에는 그 패키지의 증명이 들어 있다.

## TypeScript SDK

TypeScript SDK는 `@horon-ai/agent-sdk`와 런타임 패키지 여섯 `@horon-ai/agent-<os>-<arch>`를 따로 게시한다. 버전은 같은 태그에서 오고, `Release Tag` 트리거와 `NPM_PUBLISH` 스위치도 같다. 파일은 `.github/workflows/publish-typescript-sdk.yml`로 나뉘어 있는데, npm이 최상위 워크플로 파일 이름을 신뢰하기 때문이다. SDK의 일곱 패키지는 `publish-typescript-sdk.yml`을 Trusted Publisher로 등록한다. 절차는 `sdk/typescript/RELEASING.md`에 있으므로 여기서 되풀이하지 않는다.

## 알려진 한계

- `workflow_run`은 항상 기본 브랜치에 있는 워크플로 파일을 실행하고 `workflow_dispatch`도 그 파일이 기본 브랜치에 있어야 하므로, 두 경로는 이 워크플로가 `main`에 올라간 뒤에야 동작한다. pull request에서는 가짜 자산 경로가 게시 없이 패키징만 확인한다.
- `package_only` Release 실행은 드라이런에만 쓸 수 있다. 그 자산은 공개 릴리스가 아니어서, 디스패치 검증이 `run_id`와 함께 온 `dry_run=false`를 거부한다.
- 게시 뒤 레지스트리가 늦게 반영되는 경우는 최대 10분을 기다리는 확인 루프가 처리한다. 그래도 답이 없는 패키지가 있으면 실행이 실패하고, 같은 태그로 다시 디스패치할 수 있다.
