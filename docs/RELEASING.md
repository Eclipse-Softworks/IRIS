# Release, Signing & Package Publishing Guide

This document describes the automated IRIS release pipeline defined in
[`.github/workflows/release.yml`](../.github/workflows/release.yml), how code signing
and notarization work across Windows and macOS, and how package manager feeds are updated.

## 1. End-to-End Release Flow

Pushing an annotated semver tag triggers the full release matrix:

```bash
git tag -a v1.0.0-rc3 -m "IRIS v1.0.0-rc3"
git push origin v1.0.0-rc3
```

1. **Cross-platform builds (`build`)** — Builds `iris` (with `IRIS_GENERATE_PREBUILT=1` on native runners) and `iris_ui_host` across 6 targets:
   - `x86_64-pc-windows-msvc`, `aarch64-pc-windows-msvc`
   - `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`
   - `x86_64-apple-darwin`, `aarch64-apple-darwin`
   Each target produces both a **full archive** (`bin/` + `LLVM` toolchain + `ucrt64` sysroot on Windows) and a **`-lightweight` archive** (`bin/` + `iris_ui_host` without LLVM).
2. **Native platform installers (`installers`)** — Packages `.deb`, `.rpm`, `.pkg`, `.dmg`, Windows portable `.zip`, and Windows Inno Setup `.exe`.
3. **VS Code extension (`extension`)** — Bundles `vscode-iris/iris-lang-*.vsix`.
4. **GitHub Release (`release`)** — Computes `SHA256SUMS.txt`, generates SLSA build-provenance attestations, and publishes all archives and installers.
5. **Post-release automation** (runs automatically when the corresponding secrets are present):
   - `publish-extension` -> VS Code Marketplace & Open VSX
   - `publish-package-managers` -> Homebrew tap (`Eclipse-Softworks/homebrew-tap`), Scoop bucket (`Eclipse-Softworks/scoop-bucket`), Winget (`EclipseSoftworks.IRIS`), plus generated AUR `PKGBUILD`/`.SRCINFO`
   - `publish-linux-repos` -> Signed static APT & YUM/DNF repository trees (`packaging/linux-repo/build-repos.sh`)

---

## 2. GitHub Actions Secrets Reference

Unsigned / unconfigured optional steps skip cleanly without failing a tag release:

| Platform / Channel | Required Secrets | Notes |
|---|---|---|
| **Windows Azure Trusted Signing** | `AZURE_TRUSTED_SIGNING_ENDPOINT`, `AZURE_TRUSTED_SIGNING_ACCOUNT`, `AZURE_TRUSTED_SIGNING_PROFILE`, `AZURE_CLIENT_ID`, `AZURE_TENANT_ID`, `AZURE_CLIENT_SECRET` (optional `AZURE_TRUSTED_SIGNING_DLIB`) | Preferred on Windows; signs `iris.exe`, `iris_ui_host.dll`, and Inno Setup `.exe` via `installer/windows/sign_windows.ps1`. |
| **Windows PFX Authenticode** | `WINDOWS_CERT_PFX_BASE64`, `WINDOWS_CERT_PASSWORD` | Fallback if Azure Trusted Signing secrets are unset. |
| **macOS Developer ID & Notarization** | `APPLE_CERT_P12_BASE64`, `APPLE_CERT_PASSWORD`, `APPLE_SIGNING_IDENTITY`, `APPLE_INSTALLER_IDENTITY`, `APPLE_NOTARY_KEY_BASE64`, `APPLE_NOTARY_KEY_ID`, `APPLE_NOTARY_ISSUER_ID` (or `APPLE_ID` / `APPLE_APP_PASSWORD` / `APPLE_TEAM_ID`) | Signs Mach-O binaries + `libiris_ui_host.dylib`, signs/staples `.pkg` & `.dmg` via `installer/macos/sign_and_notarize.sh`. |
| **Homebrew Tap** | `HOMEBREW_TAP_TOKEN` | Personal Access Token with push access to `Eclipse-Softworks/homebrew-tap`. |
| **Scoop Bucket** | `SCOOP_BUCKET_TOKEN` | Personal Access Token with push access to `Eclipse-Softworks/scoop-bucket`. |
| **Winget** | `WINGET_TOKEN` | GitHub PAT for `vedantmgoyal9/winget-releaser` PRs against `microsoft/winget-pkgs`. |
| **VS Code Marketplace** | `VSCE_PAT` | Azure DevOps PAT for publisher `iris-lang`. |
| **Open VSX** | `OVSX_PAT` | Access token for `ovsx publish`. |
| **Linux APT / RPM GPG** | `GPG_PRIVATE_KEY`, `GPG_KEY_ID`, `GPG_PASSPHRASE` (optional) | Signs `dists/stable/Release.gpg`, `InRelease`, and `repomd.xml.asc`. |

---

## 3. Generating Manifests & Testing Signing Locally

### Package-manager manifests

```bash
python3 packaging/generate_manifests.py \
  --version 1.0.0-rc3 \
  --sums release-files/SHA256SUMS.txt \
  --out dist-manifests \
  --check
```

Use `--placeholder` instead of `--sums` when validating manifest syntax offline prior to a GitHub release.

### Static APT & RPM repositories

```bash
bash packaging/linux-repo/build-repos.sh ./release-files ./linux-repo-out
```

### Windows Authenticode signing dry-run

```powershell
powershell -ExecutionPolicy Bypass -File installer\windows\sign_windows.ps1 -Files target\release\iris.exe
```

Pass `-RequireSigning` in strict release pipelines to fail instead of skipping when credentials are absent.

### macOS signing & notarization

```bash
bash installer/macos/sign_and_notarize.sh sign-binaries ./staging
bash installer/macos/sign_and_notarize.sh notarize-installer installer/dist/IRIS-1.0.0-rc3-macos-arm64.pkg
```

---

## 4. Clean-Machine Install Smoke Verification

The workflow [`.github/workflows/install-smoke.yml`](../.github/workflows/install-smoke.yml) verifies that a fresh user machine with **no preinstalled Rust, LLVM, GCC, or MSYS2** can install and compile natively:

- **Windows (`windows-clean-path`)**: runs `installer/install.ps1`, strips `PATH` down to only `<prefix>\bin;C:\Windows\System32;C:\Windows`, then runs `iris doctor`, `iris completions powershell`, `iris build`, and `iris bundle`.
- **Linux (`linux-clean-container`)**: tests `installer/install.sh` + `.deb` inside a bare `debian:bookworm-slim` container and `.rpm` inside `fedora:40`.
- **macOS (`macos-clean-path`)**: runs `installer/install.sh`, strips `PATH` to `<prefix>/bin:/usr/bin:/bin`, then verifies `iris doctor`, `iris completions zsh`, `iris build`, and `iris bundle --format app`.
