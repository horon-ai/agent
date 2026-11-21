# Rust 패키지

언어: [English](PACKAGES.md) | **한국어**

사용자 설치 패키지는 Rust 실행 파일 하나와 선언 파일로 이루어진다. 확장은
`extension.toml`을 싣고, 플러그인은 `extension.toml` 하나와 자원 파일을 묶은
`plugin.toml`을 싣는다. 호스트는 설치된 실행 파일을 자식 프로세스로 띄우고 줄 단위
JSON-RPC 계약 하나로 대화한다. 패키지는 도구 호출, 실행형 명령과 자신이 선언한 세션
사건을 받는다. 작성 SDK와 예제는 배포 에이전트 실행 파일 `horon-agent`의 의존성이
아니다. 작성자가 엔진 내부 ABI 없이 패키지를 만들고 포장하고 시험하도록 존재한다.

## 상태

`dev`에서는 설치 명령으로 두 패키지 유형을 등록할 수 있고 수신함 동기화 함수가 완성된 폴더를 처리한다. 자식 프로세스 감독기는 Linux와 macOS에서 설치 사본을 실행한다. 수신함 자동 동기화와 세션 연결은 후속 이슈에서 다룬다.

| 영역 | 상태 | 검사 |
| --- | --- | --- |
| 선언 스키마와 네이티브 대상 검사 | 구현됨 | `cargo test -p horon-agent-base --test package_manifest` |
| 설치 사본과 카탈로그 | 구현됨 | `cargo test -p horon-agent-base --test package_catalog` |
| 통신 계약, 프레이밍, 기한 | 구현됨 | `cargo test -p horon-agent-app-core --lib extensions::wire::tests` |
| 작성 SDK: 프레이밍, 교섭, 취소 | 구현됨 | `cargo test -p horon-agent-extension-sdk` |
| 두 레이아웃의 예제 실행 | 구현됨 | `cargo test -p horon-package-example --test extension_layouts` |
| `horon package install`과 수신함 동기화 함수 | 구현됨 | `cargo test --locked --test package_cli` |
| Linux와 macOS의 자식 프로세스 감독 | 구현됨 | `cargo test --locked -p horon-agent-base --test package_spawn` |
| Windows의 자식 프로세스 감독과 세션 연결 | 예정 | 같은 실행 경로의 후속 변경 |

## 두 패키지 유형

두 유형은 Rust 바이너리 하나를 실행하고 같은 도구·명령·사건 이름을 선언한다.
플러그인은 호스트가 세션에 공급하는 자원 파일을 더한다.

| 유형 | 루트 선언 | 싣는 것 |
| --- | --- | --- |
| 확장 | `extension.toml` | 실행 파일만 |
| 플러그인 | `plugin.toml` | 묶인 `extension.toml` 하나, 자원 파일, 실행 파일 |

## 작성 SDK

`horon-agent-extension-sdk`는 피어 계약 크레이트, `serde`, `serde_json`, `sha2`,
`toml`에만 의존한다. 에이전트 런타임, `app-core`, 터미널 클라이언트에 의존하지 않고
게시되지 않는다. 패키지가 엔진 옆에 있는 동안에는 경로 의존성으로 더한다.

```toml
[dependencies]
horon-agent-extension-sdk = { path = "../agent/crates/extension-sdk" }
serde_json = "1"
```

프로그램은 핸들러 하나와 호출 하나다. `main`은 호스트가 첫 인자로 넘긴 패키지 루트를
읽고, 선언을 적재하고, 코드와 선언이 같은 표면을 이름 짓는지 확인한 뒤 stdio로
서비스한다. 예제의 도구, 실행형 명령, 사건 관찰자는
[src/main.rs](../../examples/packages/example/src/main.rs)에 있다.

```rust
use std::process::ExitCode;

fn main() -> ExitCode {
    let root = std::env::args_os().nth(1).expect("the host passes the package root");
    let declaration = Declaration::load(root).expect("the installed declaration");
    declaration.check(&Example).expect("the code matches the declaration");
    serve_stdio(Example, declaration).expect("the host closed the stream");
    ExitCode::SUCCESS
}
```

## 선언과 해시

선언은 패키지의 정체성이다. id, 버전, 표시 이름, 프로토콜, 바이너리를 싣는 네이티브
대상, 도구·명령·사건 이름을 담는다. 설치기가 한 번 검증하고, 호스트는 시동할 때마다
자식이 보고한 해시를 설치 기록과 대조한다. 해시는 `extension.toml` 원본 바이트의
SHA-256을 소문자 16진수로 쓴 값이므로, 단독 확장과 같은 바이트를 묶은 플러그인은 같은
값을 보고한다.

```toml
schema_version = 1
id = "hello"
version = "0.1.0"
name = "Hello package example"
protocol = "1.0"
events = ["turn.start", "turn.end"]

[targets]
aarch64-apple-darwin = "bin/aarch64-apple-darwin/horon-package-example"
```

코드와 선언이 어긋난 패키지는 시작하지 않는다. SDK는 핸들러가 없는 선언 도구와 선언에
없는 핸들러를 거부하고, 명령과 관찰 사건에도 같은 규칙을 적용한다. 그래서 실패는 선언이
약속하지 않은 호출에 답하는 대신 어긋난 이름을 알려 준다.

## 통신 계약

모든 방향이 JSON-RPC 2.0이고 UTF-8 객체 하나가 한 줄이다. 표준 출력은 프레임만
나르고, 진단은 표준 오류로 간다. 자식이 요청에 답하고 관찰을 보고하며, 호스트는 답을
쓰지 않는다.

| 메서드 | 방향 | 응답 |
| --- | --- | --- |
| `initialize` | 호스트에서 자식 | 프로토콜 범위, 기능, 선언 해시 |
| `tool/call` | 호스트에서 자식 | 도구의 JSON 값 |
| `command/invoke` | 호스트에서 자식 | 실행형 명령의 JSON 값 |
| `observer/notify` | 양방향 | 없음. id를 싣지 않는다 |
| `cancel` | 호스트에서 자식 | 없음. 취소된 호출은 아무것도 답하지 않는다 |
| `shutdown` | 호스트에서 자식 | 빈 결과 하나 뒤 자식이 종료한다 |

`initialize`는 자식이 상태로 답하는 유일한 요청이다. 제안은 패키지 id와 유형을
이름 짓고, 응답은 양쪽이 공유하는 버전, 양쪽이 제안한 기능과 위의 해시를 보고한다.

```json
{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocol":{"min":"1.0","max":"1.0"},"capabilities":["tool/call","command/invoke","observer/notify"],"package_id":"hello","kind":"extension"}}
```

거부는 JSON-RPC 코드를 쓴다. 호스트가 추측 대신 한 줄을 기록하기 위해서다. 공유하는
프로토콜 버전이 없으면 `-32001`, `initialize`가 끝나기 전 요청이면 `-32002`, 깨진
프레임이면 `-32600`, 선언한 모양이 아닌 인자면 `-32602`, 교섭하지 않은 메서드나
이름이면 `-32601`이다.

## 취소

`cancel` 알림은 요청 id 하나를 이름 짓는다. SDK가 프레임을 자기 스레드에서 읽으므로
핸들러가 도는 동안 토큰이 바뀐다. 긴 호출은 `Context::is_cancelled`를 확인하고 일찍
돌아온다. 그러면 서버는 그 id에 아무것도 보내지 않는다. 호스트는 이미 기다리기를
그쳤고 늦은 답은 등록되지 않은 프레임이 되기 때문이다. 아직 시작하지 않은 호출에 대한
취소는 그 호출을 시작하지 않게 한다. 호출이 도는 동안 스트림이 끝나면 그 호출을
취소하고 뒤의 호출은 시작하지 않으므로, 핸들러가 닫힌 연결이 전할 수 없는 취소를
기다리지 않는다.

## 레이아웃

예제는 바이너리 하나를 두 번 포장하고, 두 레이아웃이 같은 `extension.toml` 바이트를
실어 해시 하나가 둘을 덮는다.

```text
package/                     플러그인 루트
  plugin.toml                묶인 확장을 이름 짓는다
  extension/extension.toml   단독 선언과 바이트 단위로 같다
  extension/bin/<target>/    실행 파일
  resources/                 skills, commands, mcp, rules, prompts
```

단독 레이아웃은 `extension.toml`과 자체 `bin/<target>/`을 담은 디렉터리다. 설치는
선언한 파일만 복사하고 모든 경로를 패키지 루트 기준으로 검사하며 링크를 거부한다.

## 대상

지원 대상 키 여섯 개가 릴리스 대상이다. 바이너리를 싣는 대상마다 한 항목을 선언한다.
설치기는 빠진 산출물, fat Mach-O, 틀린 CPU, 실행 권한이 없는 파일, `.exe` 접미사가
없는 Windows 바이너리를 거부한다.

| 대상 키 | 플랫폼 |
| --- | --- |
| `aarch64-apple-darwin` | Apple 실리콘의 macOS |
| `x86_64-apple-darwin` | Intel의 macOS |
| `aarch64-unknown-linux-gnu` | arm64의 Linux |
| `x86_64-unknown-linux-gnu` | x86-64의 Linux |
| `aarch64-pc-windows-msvc` | arm64의 Windows |
| `x86_64-pc-windows-msvc` | x86-64의 Windows |

저장소 루트에서 배포 대상에 맞춰 예제를 빌드하고 선언 파일과 실행파일을 같은 패키지에 넣는다.

```sh
cargo build --locked --release -p horon-package-example --target aarch64-apple-darwin
mkdir -p package/bin/aarch64-apple-darwin
cp examples/packages/example/layouts/extension.toml package/
cp target/aarch64-apple-darwin/release/horon-package-example package/bin/aarch64-apple-darwin/
```

같은 빌드로 플러그인을 만들려면 단독 확장 디렉터리를 `extension/` 아래에 넣고 플러그인 선언과 자원 파일을 추가한다.

```sh
mkdir -p plugin
cp -R package plugin/extension
cp examples/packages/example/layouts/plugin.toml plugin/
cp -R examples/packages/example/layouts/resources plugin/
```

설치기는 호스트 대상만이 아니라 선언한 모든 대상을 검사하므로, 준비한 패키지는 위 표의
모든 행에 실행 파일을 담아야 한다. 여섯 대상을 모두 빌드해 넣거나, 설치 전에
`[targets]`를 실제로 배포하는 대상만 남기십시오. 해시는 설치하는 선언에서 다시
계산된다.

## 서명과 공증

패키지는 사용자 계정의 파일과 네트워크 권한으로 실행된다. macOS에서는 Developer ID와 강화 런타임으로 실행파일에 서명한다. 디스크 이미지를 공증에 제출하고 상태가 `Accepted`로 바뀌면 이미지에 공증 티켓을 붙인다. [Apple 공증 안내](https://developer.apple.com/documentation/security/customizing-the-notarization-workflow)를 참고한다.

```sh
codesign --force --options runtime --timestamp --sign "Developer ID Application: Example (TEAMID)" package/bin/aarch64-apple-darwin/horon-package-example
hdiutil create -srcfolder package -format UDZO package.dmg
xcrun notarytool submit package.dmg --keychain-profile horon --wait
xcrun stapler staple package.dmg
```

Windows에서는 포장 전에 실행 파일에 Authenticode로 서명한다(`signtool sign /fd
SHA256`). Linux 패키지는 서명하지 않고 카탈로그의 SHA-256이 무결성 검사다. 에이전트
자체의 릴리스 절차는 [설치 안내](../release/INSTALL.ko.md)에 있다. 패키지는 그 릴리스가
아니라 작성자가 서명하고 배포한다.

## 설치와 관리

`horon package install <dir>`과 수신함 동기화 함수는 같은 카탈로그 설치 경로를 쓴다. 카탈로그는 패키지를 검증하고 복사해 등록한다. 설치는 사용자 계정의 권한으로 패키지를 실행하는 데 동의하는 절차다. 카탈로그는 무결성을 확인할 SHA-256 값을 기록한다. 수신함 자동 처리와 열린 세션 반영은 후속 이슈에서 다룬다.

| 명령 | 효과 |
| --- | --- |
| `horon package install <dir> [--project <root>] [--replace]` | 검증하고 복사하고 등록한다 |
| `horon package list [--kind extension\|plugin]` | 출처, 버전, 범위, 유형, 상태를 보여 준다 |
| `horon package verify <id>` | 기록된 바이트를 다시 검사한다 |
| `horon package disable <id>` | 사본을 남기고 적재를 멈춘다 |
| `horon package remove <id>` | 기록을 철회하고 사용자가 보관한 원본을 남긴다 |

## 설치된 실행파일 시작

실행기는 설치 사본의 참조를 잡고 시작 직전에 기록된 실행파일을 검사한다. 자식은 설치 사본을 작업 디렉터리로 쓰고 패키지 루트의 절대 경로를 첫 인자로 받는다. Linux와 macOS에서 실행기는 기존 환경 변수를 비운 뒤 아래 값을 전달한다.

| 변수 | 값 |
| --- | --- |
| `HORON_PACKAGE_ROOT`, `HORON_PACKAGE_ID`, `HORON_PACKAGE_KIND` | 설치 루트, 패키지 ID, 패키지 유형 |
| `HORON_PACKAGE_STATE`, `HOME` | 소유자만 접근하는 패키지 상태 디렉터리 |
| `TMPDIR` | 패키지 상태 디렉터리 아래 `tmp` |
| `PATH`, `LANG`, `LC_ALL`, `LC_CTYPE`, `TZ` | 부모에 값이 있으면 상속 |

공급자 키와 나머지 부모 환경 변수는 전달하지 않는다. 자식은 사용자 계정의 파일과 네트워크 권한으로 실행된다.

## 예제

`examples/packages/example`이 예제다. 도구(`greet`), 실행형 명령(`hello`),
`turn.start`와 `turn.end` 관찰자, 플러그인 자원과 두 레이아웃을 담는다. 시험은 실제
파이프 위에서 레이아웃마다 빌드된 바이너리를 시작한다.

```sh
cargo test --locked -p horon-package-example --test extension_layouts
cargo run --locked -p horon-package-example -- package
```

## 한계

| 한계 | 값 |
| --- | --- |
| 줄바꿈을 포함한 프레임 하나 | 1 MiB |
| 선언 파일 | 256 KiB |
| 양방향 대기 프레임 | 128 |
| 호스트가 기다리는 요청 | 64 |
| 호출 하나가 쌓을 수 있는 관찰 보고 | 128 |
