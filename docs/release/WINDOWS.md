# Windows support

Language: **English** | [한국어](WINDOWS.ko.md)

HoronAgent treats Windows as a fully supported platform. The Windows implementation uses native named pipes, Windows process management and PowerShell installation.

## Support status

| Area | Status |
|---|---|
| Windows 11 x64 | Supported and verified directly |
| Windows 11 ARM64 | Release builds and automated install checks run for it |
| PowerShell install script | Verified in Windows CI |
| Native IPC and process lifecycle | Covered by Windows-targeted tests and end-to-end tests |
| `horon update` | Supported, with SHA-256 verification |
| `horon install-release` | The path the install script hands release asset verification and channel activation to |
| Distribution assets | `.exe` and `.tar.gz` assets for x64 and ARM64 |
| Authenticode signing | The pipeline is ready; the Azure setup below has to be done once |

The install script requires PowerShell 5.1 or later. Intel and AMD Windows machines get the x64 build by default. ARM64 Windows picks the ARM64 build automatically.

## Installation

Open PowerShell and run:

```powershell
irm https://raw.githubusercontent.com/horon-ai/agent/main/scripts/install/install.ps1 | iex
```

The install script proceeds in this order:

1. Detect x64 or ARM64.
2. Download the matching asset from the official GitHub release.
3. Verify it against the release's `SHA256SUMS` file.
4. Hand the downloaded asset directory and version to `horon install-release --assets <directory> --agent-version <version>`. The executable verifies the assets again and creates the immutable version directory, `stable`, `current` and the launcher links under `%LOCALAPPDATA%\horon-agent`.
5. Add `%LOCALAPPDATA%\horon-agent\bin` to the user `PATH`.

The install script never unpacks the release archive itself. `install-release` in step 4 handles verification and channel activation on one path, and a failure leaves the previous installation in place. Flags and verification rules are in [`INSTALL.md`](INSTALL.md).

Installing Alacritty is optional and no longer happens automatically. To request it explicitly, run:

```powershell
$script = [scriptblock]::Create((irm https://raw.githubusercontent.com/horon-ai/agent/main/scripts/install/install.ps1))
& $script -ConfigureAlacritty
```

HoronAgent may also offer this option interactively after startup.

When the release has no matching prebuilt Windows asset, the install script does not silently start a long build; it prints a message the user can act on and fails. To explicitly allow a source build, run:

```powershell
$script = [scriptblock]::Create((irm https://raw.githubusercontent.com/horon-ai/agent/main/scripts/install/install.ps1))
& $script -BuildFromSource
```

A source build needs Git, Rust and Visual Studio 2022 Build Tools with the **Desktop development with C++** workload installed.

### Install locations

- Launcher: `%LOCALAPPDATA%\horon-agent\bin\horon-agent.exe`
- Stable binary: `%LOCALAPPDATA%\horon-agent\builds\stable\horon-agent.exe`
- Versioned binary: `%LOCALAPPDATA%\horon-agent\builds\versions\<version>\horon-agent.exe`
- User data and settings: `%USERPROFILE%\.horon`

### Verifying an installation

```powershell
horon --version
Get-Command horon
Get-FileHash (Get-Command horon).Source -Algorithm SHA256
```

Compare the hash with `SHA256SUMS` in the matching [GitHub release](https://github.com/horon-ai/agent/releases/latest).

Once Authenticode signing is on, this command must report `Valid`:

```powershell
Get-AuthenticodeSignature (Get-Command horon).Source | Format-List Status,StatusMessage,SignerCertificate
```

## Microsoft Defender and SmartScreen

Two different Windows warnings are often confused.

- **Microsoft Defender SmartScreen** shows a message such as "Windows protected your PC" when a downloaded application is unsigned or has not accumulated enough publisher reputation. An Authenticode signature with a trusted timestamping certificate is the main remedy. A new publisher identity only accumulates reputation over time.
- **Microsoft Defender Antivirus** reports a named threat or suspicious behaviour. A signature helps establish provenance, but a heuristic false positive has to be submitted to Microsoft with the exact signed file and its SHA-256 hash.

Never tell users to turn Defender off, to add an exclusion or to work around a named malware detection. Check the release URL, the checksum and the Authenticode signature first. If a correctly signed official build is still detected, submit it to the [Microsoft Security Intelligence file submission portal](https://www.microsoft.com/wdsi/filesubmission) as a software developer false positive.

### Heuristic triggers already reduced

The Windows setup is deliberately designed to avoid unnecessary suspicious behaviour.

- Release downloads are verified against `SHA256SUMS`.
- The optional terminal setup requires explicit consent.
- The old hidden VBScript startup trampoline is gone.
- The global launch hotkey is not created during installation. Startup shortcuts and hotkey scripts left by earlier installations are cleaned up by the uninstaller.
- Release binaries are built on GitHub-hosted Windows runners and verified before distribution.

## Turning on Authenticode signing

The release workflow supports [Azure Artifact Signing](https://azure.microsoft.com/products/artifact-signing) through GitHub OIDC. The certificate private key stays in Microsoft's managed signing service instead of being exported into GitHub secrets.

This is a one-time setup for the owner, and it may require Azure billing and organization or identity verification.

1. Create an Artifact Signing account and a public trust certificate profile.
2. Create a Microsoft Entra application or managed identity with a federated credential for the `horon-ai/agent` GitHub Actions.
3. Grant the **Artifact Signing Certificate Profile Signer** role on the certificate profile.
4. Add these GitHub Actions secrets:
   - `AZURE_CLIENT_ID`
   - `AZURE_TENANT_ID`
   - `AZURE_SUBSCRIPTION_ID`
5. Add these GitHub Actions variables:
   - `WINDOWS_SIGNING_ENDPOINT`, for example `https://eus.codesigning.azure.net/`
   - `WINDOWS_SIGNING_ACCOUNT`
   - `WINDOWS_SIGNING_CERTIFICATE_PROFILE`
6. Push a test tag and check that the `Sign and publish Windows assets` job signs both executables and that `Get-AuthenticodeSignature` returns `Valid`.
7. To make signing mandatory, set `WINDOWS_SIGNING_REQUIRED` to `true`. When it is not `true` and Azure is not configured, the signing step leaves a `::notice::` and is skipped, unsigned Windows assets are published, and the release notes say they are unsigned. When it is `true` and the configuration is missing, the job fails and names the missing secrets and variables. Official Windows releases ship with it set to `true`.

The workflow applies the SHA-256 Authenticode signature and an RFC 3161 timestamp before packaging, checksum generation and release upload. Both the x64 and ARM64 executables are signed on the x64 Windows signing runner, which supports both.

Until signing is enforced and a public release carries a valid signature, do not describe the Defender and SmartScreen work as finished.

## Release approval checklist

Check this for every release that changes Windows behaviour.

- [ ] The Windows x64 CI build and targeted tests pass.
- [ ] The Windows lifecycle end-to-end tests pass.
- [ ] The x64 and ARM64 install script checks pass.
- [ ] Both `.exe` files carry a valid timestamped Authenticode signature.
- [ ] `SHA256SUMS` lists both Windows executables and archives.
- [ ] Install, run, update and uninstall all succeed on a clean Windows 11 machine.
- [ ] Defender Antivirus reports no named detection for the signed release.
- [ ] SmartScreen identifies the expected publisher. A low-reputation warning is tracked separately from a malware detection.
- [ ] The website's Windows button points at a distributed asset and has no preview or work-in-progress wording.
- [ ] The release notes record any important Windows fix or limitation.

## Continuous integration

Windows is covered by:

- `.github/workflows/ci.yml`: release build, test compilation, platform-targeted tests, runtime smoke tests, lifecycle end-to-end tests, install script checks, PowerShell syntax check.
- `.github/workflows/windows-smoke.yml`: manually run x64 and ARM64 smoke verification.
- `.github/workflows/release.yml`: x64 and ARM64 builds (a separate job per architecture), the managed signing the Windows distribution needs, signature verification, packaging and checksums. Each platform job uploads its own assets to the draft release without waiting for the others. The final `Finalize release` job publishes only when every platform asset is present and leaves the release as a draft when one is missing. A failed ARM64 build does not stop the x64 signing and upload.

## Architecture notes

Unix domain sockets are replaced by Windows named pipes in `crates/platform-windows/src/ipc.rs`, and `crates/transport` exposes the same types on both platforms. The platform-specific parts of filesystem, process, update and swap behaviour are selected at compile time with `#[cfg(windows)]`, so Windows support does not add runtime branches to the Unix build.

Session records go to the same `crates/session-store` event log as on Unix. Windows has neither `O_NOFOLLOW` nor permission bits, so lock files and source files are checked after opening by confirming they are regular files and that their creation time matches the opened handle, while owner-only permissions come from the inherited ACL on `%USERPROFILE%\.horon`. Directory entries themselves are not flushed. On Windows a directory cannot be opened for `FlushFileBuffers` and NTFS journals directory metadata itself, so the durability boundary is `sync_all` on the file itself.

## Reporting a Windows problem

Include the following in a GitHub issue:

- Windows edition, version and architecture
- The HoronAgent version from `horon --version`
- How it was installed
- The terminal and the PowerShell version (`$PSVersionTable.PSVersion`)
- The exact Defender or SmartScreen message
- The Defender threat name, if one was shown
- The SHA-256 from `Get-FileHash`
- The Authenticode status from `Get-AuthenticodeSignature`

Do not post private settings, credentials, session records or `.horon` authentication files.