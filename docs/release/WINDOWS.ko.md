# Windows 지원

언어: [English](WINDOWS.md) | **한국어**

HoronAgent는 Windows를 정식 지원 플랫폼으로 다룬다. Windows 구현은 네이티브 명명된 파이프, Windows 프로세스 관리, PowerShell 설치를 쓴다.

## 지원 현황

| 영역 | 상태 |
|---|---|
| Windows 11 x64 | 지원하며 직접 검증했다 |
| Windows 11 ARM64 | 배포 빌드와 자동 설치 검사를 수행한다 |
| PowerShell 설치 스크립트 | Windows CI에서 검증했다 |
| 네이티브 IPC와 프로세스 수명 주기 | Windows 대상 테스트와 종단 테스트로 덮는다 |
| `horon update` | SHA-256 검증과 함께 지원한다 |
| `horon-agent install-release` | 설치 스크립트가 릴리스 자산 검증과 채널 활성화를 맡기는 경로다 |
| 배포 자산 | x64와 ARM64용 `.exe` 및 `.tar.gz` 자산 |
| Authenticode 서명 | 배포 파이프라인은 준비됐고, 아래 Azure 설정을 한 번 해 두어야 한다 |

설치 스크립트는 PowerShell 5.1 이상을 요구한다. Intel과 AMD Windows 컴퓨터에는 x64 빌드가 기본이다. ARM64 Windows에서는 ARM64 빌드를 자동으로 고른다.

## 설치

PowerShell을 열고 실행한다.

```powershell
irm https://horon-agent.sh/install.ps1 | iex
```

설치 스크립트는 다음 순서로 움직인다.

1. x64인지 ARM64인지 판별한다.
2. 공식 GitHub 릴리스에서 맞는 자산을 내려받는다.
3. 릴리스의 `SHA256SUMS` 파일과 대조해 검증한다.
4. 내려받은 자산 디렉터리와 버전을 `horon-agent install-release --assets <디렉터리> --agent-version <버전>`에 넘긴다. 실행 파일이 자산을 다시 검증하고 `%LOCALAPPDATA%\horon-agent` 아래에 변경 불가 버전 디렉터리와 `stable`, `current`, 런처 링크를 만든다.
5. `%LOCALAPPDATA%\horon-agent\bin`을 사용자 `PATH`에 추가한다.

설치 스크립트는 릴리스 압축을 직접 풀지 않는다. 4단계의 `install-release`가 검증과 채널 활성화를 한 경로에서 처리하고, 실패하면 이전 설치를 그대로 둔다. 플래그와 검증 규칙은 [`INSTALL.ko.md`](INSTALL.ko.md)에 있다.

Alacritty 설치는 선택 사항이고 더 이상 자동으로 설치하지 않는다. 명시적으로 요청하려면 다음을 실행한다.

```powershell
$script = [scriptblock]::Create((irm https://horon-agent.sh/install.ps1))
& $script -ConfigureAlacritty
```

HoronAgent는 실행 후에 이 옵션을 대화형으로 제안할 수도 있다.

릴리스에 맞는 Windows 사전 빌드 자산이 없으면 설치 스크립트는 오래 걸리는 빌드를 조용히 시작하지 않고, 사용자가 조치할 수 있는 메시지를 내고 실패한다. 소스 빌드를 명시적으로 허용하려면 다음을 실행한다.

```powershell
$script = [scriptblock]::Create((irm https://horon-agent.sh/install.ps1))
& $script -BuildFromSource
```

소스 빌드에는 Git, Rust, **Desktop development with C++** 워크로드가 설치된 Visual Studio 2022 Build Tools가 필요하다.

### 설치 경로

- 런처: `%LOCALAPPDATA%\horon-agent\bin\horon-agent.exe`
- 안정 바이너리: `%LOCALAPPDATA%\horon-agent\builds\stable\horon-agent.exe`
- 버전별 바이너리: `%LOCALAPPDATA%\horon-agent\builds\versions\<version>\horon-agent.exe`
- 사용자 데이터와 설정: `%USERPROFILE%\.horon`

### 설치 검증

```powershell
horon --version
Get-Command horon-agent
Get-FileHash (Get-Command horon-agent).Source -Algorithm SHA256
```

해시를 해당 [GitHub 릴리스](https://github.com/horon-ai/agent/releases/latest)의 `SHA256SUMS`와 비교한다.

Authenticode 서명을 켠 뒤에는 다음 명령이 `Valid`를 보고해야 한다.

```powershell
Get-AuthenticodeSignature (Get-Command horon-agent).Source | Format-List Status,StatusMessage,SignerCertificate
```

## Microsoft Defender와 SmartScreen

Windows 경고 두 가지를 흔히 혼동한다.

- **Microsoft Defender SmartScreen**은 내려받은 응용 프로그램이 서명되지 않았거나 게시자 평판이 충분히 쌓이지 않았을 때 "Windows protected your PC" 같은 메시지를 띄운다. 신뢰할 수 있는 타임스탬프 인증서로 Authenticode 서명을 하는 것이 주된 해결책이다. 새 게시자 신원은 평판이 시간이 지나야 쌓인다.
- **Microsoft Defender Antivirus**는 이름이 붙은 위협이나 의심스러운 동작을 보고한다. 서명은 출처를 밝히는 데 도움이 되지만, 휴리스틱 오탐은 서명된 정확한 파일과 SHA-256 해시를 함께 Microsoft에 제출해야 한다.

사용자에게 Defender를 끄라고 하거나 예외를 추가하라고 하거나 이름이 붙은 악성코드 탐지를 우회하라고 해서는 안 된다. 먼저 릴리스 URL, 체크섬, Authenticode 서명을 확인한다. 올바르게 서명된 공식 빌드가 여전히 탐지되면 [Microsoft Security Intelligence 파일 제출 포털](https://www.microsoft.com/wdsi/filesubmission)에 소프트웨어 개발자 오탐으로 제출한다.

### 이미 적용한 휴리스틱 유발 감소 조치

Windows 설정은 불필요한 의심 동작을 피하도록 일부러 설계했다.

- 릴리스 다운로드를 `SHA256SUMS`와 대조해 검증한다.
- 선택 사항인 Alacritty 설정은 명시적 동의를 받아야 한다.
- 예전의 숨은 VBScript 시작 트램폴린을 삭제했다.
- 전역 실행 단축키는 설치 단계에서 만들지 않는다. 예전 설치본이 남긴 시작 프로그램 바로가기와 단축키 스크립트는 제거 프로그램이 정리한다.
- 릴리스 바이너리는 GitHub가 호스팅하는 Windows 러너에서 빌드하고 배포 전에 검증한다.

## Authenticode 서명 켜기

배포 워크플로는 GitHub OIDC로 [Azure Artifact Signing](https://azure.microsoft.com/products/artifact-signing)을 지원한다. 인증서 개인 키를 GitHub 시크릿으로 내보내지 않고 Microsoft의 관리형 서명 서비스에 두는 방식이다.

소유자가 한 번만 해 두면 되는 설정이고, Azure 결제와 조직 또는 신원 확인이 필요할 수 있다.

1. Artifact Signing 계정과 공개 신뢰 인증서 프로필을 만든다.
2. `horon-ai/agent` GitHub Actions용 연합 자격 증명이 붙은 Microsoft Entra 응용 프로그램 또는 관리 ID를 만든다.
3. 인증서 프로필에 **Artifact Signing Certificate Profile Signer** 역할을 부여한다.
4. 다음 GitHub Actions 시크릿을 추가한다.
   - `AZURE_CLIENT_ID`
   - `AZURE_TENANT_ID`
   - `AZURE_SUBSCRIPTION_ID`
5. 다음 GitHub Actions 변수를 추가한다.
   - `WINDOWS_SIGNING_ENDPOINT`, 예를 들어 `https://eus.codesigning.azure.net/`
   - `WINDOWS_SIGNING_ACCOUNT`
   - `WINDOWS_SIGNING_CERTIFICATE_PROFILE`
6. 테스트 태그를 푸시하고 `Sign and publish Windows assets` 작업이 실행 파일 두 개에 모두 서명하는지, `Get-AuthenticodeSignature`가 `Valid`를 돌려주는지 확인한다.
7. 서명을 필수로 만들려면 `WINDOWS_SIGNING_REQUIRED`를 `true`로 둔다. 이 값이 `true`가 아니고 Azure 설정도 없으면 서명 단계는 `::notice::`를 남기고 건너뛰며, 서명되지 않은 Windows 자산이 올라가고 릴리스 노트에 서명되지 않았다고 적힌다. `true`인데 설정이 빠졌으면 작업이 빠진 시크릿과 변수 이름을 알리며 실패한다. 공식 Windows 릴리스는 `true`로 배포한다.

워크플로는 패키징, 체크섬 생성, 릴리스 업로드 전에 SHA-256 Authenticode 서명과 RFC 3161 타임스탬프를 적용한다. x64와 ARM64 실행 파일 모두 지원되는 x64 Windows 서명 러너에서 서명한다.

서명 강제가 켜져 있고 공개 릴리스에 유효한 서명이 붙기 전에는 Defender와 SmartScreen 대응이 끝났다고 서술하지 않는다.

## 릴리스 승인 체크리스트

Windows 동작이 바뀌는 릴리스마다 확인한다.

- [ ] Windows x64 CI 빌드와 대상 테스트가 통과한다.
- [ ] Windows 수명 주기 종단 테스트가 통과한다.
- [ ] x64와 ARM64 설치 스크립트 검증이 통과한다.
- [ ] `.exe` 파일 두 개에 유효한 타임스탬프 Authenticode 서명이 붙어 있다.
- [ ] `SHA256SUMS`에 Windows 실행 파일과 압축 파일이 모두 들어 있다.
- [ ] 깨끗한 Windows 11 머신에서 설치, 실행, 업데이트, 제거가 모두 성공한다.
- [ ] Defender Antivirus가 서명된 릴리스에서 이름이 붙은 탐지를 보고하지 않는다.
- [ ] SmartScreen이 기대한 게시자를 식별한다. 평판이 낮다는 경고는 악성코드 탐지와 따로 추적한다.
- [ ] 웹사이트의 Windows 버튼이 배포된 자산을 가리키고 미리보기나 작업 중 문구가 없다.
- [ ] 릴리스 노트에 중요한 Windows 수정이나 제약을 적었다.

## 지속적 통합

Windows는 다음으로 덮는다.

- `.github/workflows/ci.yml`: 배포 빌드, 테스트 컴파일, 플랫폼 대상 테스트, 런타임 스모크 테스트, 수명 주기 종단 테스트, 설치 스크립트 검증, PowerShell 문법 검사.
- `.github/workflows/windows-smoke.yml`: 수동으로 실행하는 x64와 ARM64 스모크 검증.
- `.github/workflows/release.yml`: x64와 ARM64 빌드(아키텍처마다 별도 작업), Windows 배포에 필요한 관리형 서명, 서명 검증, 패키징, 체크섬. 각 플랫폼 작업은 서로를 기다리지 않고 자기 자산을 초안 릴리스에 올린다. 마지막 `Finalize release` 작업은 모든 플랫폼 자산이 갖춰졌을 때만 릴리스를 공개하고, 하나라도 빠지면 초안으로 남긴다. ARM64 빌드가 실패해도 x64 서명과 업로드는 진행된다.

## 아키텍처 메모

Unix 도메인 소켓은 `crates/platform-windows/src/ipc.rs`의 Windows 명명된 파이프로 대체되고, `crates/transport`가 두 플랫폼에 같은 타입을 노출한다. 파일 시스템, 프로세스, 업데이트, 교체 동작 중 플랫폼별 부분은 `#[cfg(windows)]`로 컴파일 시점에 선택하므로, Windows 지원 때문에 Unix 빌드에 런타임 분기가 늘지 않는다.

세션 기록은 Unix와 같은 `crates/session-store` 이벤트 로그에 남는다. Windows에는 `O_NOFOLLOW`와 권한 비트가 없으므로, 잠금 파일과 원본 파일은 연 뒤에 일반 파일인지와 생성 시각이 열린 핸들과 같은지로 확인하고 소유자 전용 권한은 `%USERPROFILE%\.horon`의 상속 ACL이 맡는다. 디렉터리 항목 자체는 flush하지 않는다. Windows에서는 디렉터리를 `FlushFileBuffers`용으로 열 수 없고 NTFS가 디렉터리 메타데이터를 직접 저널링하므로, 내구성 경계는 파일 자체의 `sync_all`이다.

## Windows 문제 보고

GitHub 이슈에 다음을 적는다.

- Windows 에디션, 버전, 아키텍처
- `horon --version`으로 확인한 HoronAgent 버전
- 설치 방법
- 터미널과 PowerShell 버전(`$PSVersionTable.PSVersion`)
- Defender 또는 SmartScreen 메시지 원문
- 표시됐다면 Defender 위협 이름
- `Get-FileHash`로 구한 SHA-256
- `Get-AuthenticodeSignature`로 확인한 Authenticode 상태

비공개 설정, 자격 증명, 세션 기록, `.horon` 인증 파일은 올리지 않는다.
