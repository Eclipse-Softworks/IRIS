#!/usr/bin/env python3
"""Generate Homebrew, Scoop, Winget, and AUR manifests for an IRIS release."""

from __future__ import annotations

import argparse
import json
import re
from pathlib import Path

REPO = "Eclipse-Softworks/IRIS"
PLACEHOLDER_SHA = "0" * 64


def parse_sha256sums(path: Path) -> dict[str, str]:
    sums: dict[str, str] = {}
    for line in path.read_text(encoding="utf-8").splitlines():
        m = re.match(r"^\s*([a-fA-F0-9]{64})\s+\*?(\S+)\s*$", line)
        if m:
            sums[Path(m.group(2)).name] = m.group(1).lower()
    return sums


def sha_for(sums: dict[str, str], filename: str, placeholder: bool) -> str:
    if filename in sums:
        return sums[filename]
    if placeholder:
        return PLACEHOLDER_SHA
    raise KeyError(f"Checksum not found in SHA256SUMS.txt for {filename}")


def render_homebrew(version: str, tag: str, sums: dict[str, str], placeholder: bool) -> str:
    ver = version.lstrip("v")
    a_mac_arm = f"iris-{tag}-aarch64-apple-darwin.tar.gz"
    a_mac_x64 = f"iris-{tag}-x86_64-apple-darwin.tar.gz"
    a_lin_arm = f"iris-{tag}-aarch64-unknown-linux-gnu.tar.gz"
    a_lin_x64 = f"iris-{tag}-x86_64-unknown-linux-gnu.tar.gz"

    return f'''# typed: false
# frozen_string_literal: true

class Iris < Formula
  desc "Compiled systems, AI/ML, and UI programming language (compiler, LSP, DAP, REPL)"
  homepage "https://github.com/{REPO}"
  version "{ver}"
  license "Apache-2.0"

  on_macos do
    on_arm do
      url "https://github.com/{REPO}/releases/download/{tag}/{a_mac_arm}"
      sha256 "{sha_for(sums, a_mac_arm, placeholder)}"
    end
    on_intel do
      url "https://github.com/{REPO}/releases/download/{tag}/{a_mac_x64}"
      sha256 "{sha_for(sums, a_mac_x64, placeholder)}"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/{REPO}/releases/download/{tag}/{a_lin_arm}"
      sha256 "{sha_for(sums, a_lin_arm, placeholder)}"
    end
    on_intel do
      url "https://github.com/{REPO}/releases/download/{tag}/{a_lin_x64}"
      sha256 "{sha_for(sums, a_lin_x64, placeholder)}"
    end
  end

  def install
    bin.install "bin/iris"
    ui_libs = Dir["bin/libiris_ui_host.*"]
    lib.install ui_libs unless ui_libs.empty?
    if Dir.exist?("toolchain")
      (prefix/"toolchain").install Dir["toolchain/*"]
    end
    (pkgshare).install "LLVM_VERSION" if File.exist?("LLVM_VERSION")
    (pkgshare).install "share/iris/iris.png" if File.exist?("share/iris/iris.png")
    (pkgshare/"examples").install Dir["examples/*"] if Dir.exist?("examples")
    generate_completions_from_executable(bin/"iris", "completions")
  end

  test do
    assert_match "iris ", shell_output("#{{bin}}/iris --version")
    (testpath/"hello.iris").write <<~EOS
      def main() -> i64 {{
        return 0
      }}
    EOS
    system bin/"iris", "check", testpath/"hello.iris"
  end
end
'''


def render_scoop(version: str, tag: str, sums: dict[str, str], placeholder: bool) -> str:
    ver = version.lstrip("v")
    win_x64 = f"iris-{tag}-x86_64-pc-windows-msvc.zip"
    win_arm64 = f"iris-{tag}-aarch64-pc-windows-msvc.zip"

    manifest = {
        "version": ver,
        "description": "IRIS: A compiled systems, AI/ML, and UI programming language with bundled LLVM toolchain",
        "homepage": f"https://github.com/{REPO}",
        "license": "Apache-2.0",
        "architecture": {
            "64bit": {
                "url": f"https://github.com/{REPO}/releases/download/{tag}/{win_x64}",
                "hash": sha_for(sums, win_x64, placeholder),
            },
            "arm64": {
                "url": f"https://github.com/{REPO}/releases/download/{tag}/{win_arm64}",
                "hash": sha_for(sums, win_arm64, placeholder),
            },
        },
        "bin": ["bin\\iris.exe"],
        "post_install": [
            "& \"$dir\\bin\\iris.exe\" completions powershell | Set-Content -Path \"$dir\\share\\iris\\completions\\_iris.ps1\" -Encoding UTF8"
        ],
        "checkver": {
            "github": f"https://github.com/{REPO}",
        },
        "autoupdate": {
            "architecture": {
                "64bit": {
                    "url": f"https://github.com/{REPO}/releases/download/v$version/iris-v$version-x86_64-pc-windows-msvc.zip",
                },
                "arm64": {
                    "url": f"https://github.com/{REPO}/releases/download/v$version/iris-v$version-aarch64-pc-windows-msvc.zip",
                },
            },
            "hash": {
                "url": f"https://github.com/{REPO}/releases/download/v$version/SHA256SUMS.txt",
            },
        },
    }
    return json.dumps(manifest, indent=2) + "\n"


def render_winget(version: str, tag: str, sums: dict[str, str], placeholder: bool) -> dict[str, str]:
    ver = version.lstrip("v")
    setup_exe = f"IRIS-{ver}-windows-x64-setup.exe"
    setup_sha = sha_for(sums, setup_exe, placeholder).upper()

    version_yaml = f"""# yaml-language-server: $schema=https://aka.ms/winget-manifest.version.1.6.0.schema.json
PackageIdentifier: EclipseSoftworks.IRIS
PackageVersion: {ver}
DefaultLocale: en-US
ManifestType: version
ManifestVersion: 1.6.0
"""

    installer_yaml = f"""# yaml-language-server: $schema=https://aka.ms/winget-manifest.installer.1.6.0.schema.json
PackageIdentifier: EclipseSoftworks.IRIS
PackageVersion: {ver}
InstallerLocale: en-US
InstallerType: inno
Scope: user
InstallModes:
  - interactive
  - silent
  - silentWithProgress
UpgradeBehavior: install
Commands:
  - iris
FileExtensions:
  - iris
Installers:
  - Architecture: x64
    InstallerUrl: https://github.com/{REPO}/releases/download/{tag}/{setup_exe}
    InstallerSha256: {setup_sha}
ManifestType: installer
ManifestVersion: 1.6.0
"""

    locale_yaml = f"""# yaml-language-server: $schema=https://aka.ms/winget-manifest.defaultLocale.1.6.0.schema.json
PackageIdentifier: EclipseSoftworks.IRIS
PackageVersion: {ver}
PackageLocale: en-US
Publisher: Eclipse Softworks
PublisherUrl: https://github.com/{REPO}
PublisherSupportUrl: https://github.com/{REPO}/issues
PackageName: IRIS Language
PackageUrl: https://github.com/{REPO}
License: Apache-2.0
LicenseUrl: https://github.com/{REPO}/blob/main/LICENSE
ShortDescription: Compiled systems, AI/ML, and UI programming language with bundled LLVM toolchain, LSP, DAP, and REPL.
Tags:
  - programming-language
  - compiler
  - llvm
  - machine-learning
  - systems
ReleaseNotesUrl: https://github.com/{REPO}/releases/tag/{tag}
ManifestType: defaultLocale
ManifestVersion: 1.6.0
"""
    return {
        "EclipseSoftworks.IRIS.yaml": version_yaml,
        "EclipseSoftworks.IRIS.installer.yaml": installer_yaml,
        "EclipseSoftworks.IRIS.locale.en-US.yaml": locale_yaml,
    }


def render_aur(version: str, tag: str, sums: dict[str, str], placeholder: bool) -> tuple[str, str]:
    ver = version.lstrip("v")
    arch_ver = ver.replace("-", "_")
    x64_asset = f"iris-{tag}-x86_64-unknown-linux-gnu.tar.gz"
    arm_asset = f"iris-{tag}-aarch64-unknown-linux-gnu.tar.gz"
    sha_x64 = sha_for(sums, x64_asset, placeholder)
    sha_arm = sha_for(sums, arm_asset, placeholder)

    pkgbuild = f'''# Maintainer: Eclipse Softworks <dev@eclipsesoftworks.io>
pkgname=iris-bin
pkgver={arch_ver}
_tagver={ver}
pkgrel=1
pkgdesc="IRIS programming language compiler, bundled LLVM toolchain, LSP, DAP, and REPL"
arch=('x86_64' 'aarch64')
url="https://github.com/{REPO}"
license=('Apache-2.0')
depends=('glibc')
provides=('iris')
conflicts=('iris')
options=('!strip')
source_x86_64=("https://github.com/{REPO}/releases/download/v${{_tagver}}/iris-v${{_tagver}}-x86_64-unknown-linux-gnu.tar.gz")
source_aarch64=("https://github.com/{REPO}/releases/download/v${{_tagver}}/iris-v${{_tagver}}-aarch64-unknown-linux-gnu.tar.gz")
sha256sums_x86_64=('{sha_x64}')
sha256sums_aarch64=('{sha_arm}')

package() {{
  install -Dm755 "${{srcdir}}/bin/iris" "${{pkgdir}}/usr/bin/iris"
  if [[ -f "${{srcdir}}/bin/libiris_ui_host.so" ]]; then
    install -Dm755 "${{srcdir}}/bin/libiris_ui_host.so" "${{pkgdir}}/usr/share/iris/libiris_ui_host.so"
  fi
  if [[ -d "${{srcdir}}/toolchain" ]]; then
    mkdir -p "${{pkgdir}}/usr/share/iris/toolchain"
    cp -a "${{srcdir}}/toolchain/." "${{pkgdir}}/usr/share/iris/toolchain/"
  fi
  if [[ -f "${{srcdir}}/LLVM_VERSION" ]]; then
    install -Dm644 "${{srcdir}}/LLVM_VERSION" "${{pkgdir}}/usr/share/iris/LLVM_VERSION"
  fi
  if [[ -f "${{srcdir}}/share/iris/iris.png" ]]; then
    install -Dm644 "${{srcdir}}/share/iris/iris.png" "${{pkgdir}}/usr/share/iris/iris.png"
    install -Dm644 "${{srcdir}}/share/iris/iris.png" "${{pkgdir}}/usr/share/icons/hicolor/256x256/apps/iris.png"
  fi
  install -Dm644 "${{srcdir}}/LICENSE" "${{pkgdir}}/usr/share/licenses/${{pkgname}}/LICENSE"

  "${{pkgdir}}/usr/bin/iris" completions bash | install -Dm644 /dev/stdin "${{pkgdir}}/usr/share/bash-completion/completions/iris" || true
  "${{pkgdir}}/usr/bin/iris" completions zsh  | install -Dm644 /dev/stdin "${{pkgdir}}/usr/share/zsh/site-functions/_iris" || true
  "${{pkgdir}}/usr/bin/iris" completions fish | install -Dm644 /dev/stdin "${{pkgdir}}/usr/share/fish/vendor_completions.d/iris.fish" || true
}}
'''

    srcinfo = f"""pkgbase = iris-bin
\tpkgdesc = IRIS programming language compiler, bundled LLVM toolchain, LSP, DAP, and REPL
\tpkgver = {arch_ver}
\tpkgrel = 1
\turl = https://github.com/{REPO}
\tarch = x86_64
\tarch = aarch64
\tlicense = Apache-2.0
\tdepends = glibc
\tprovides = iris
\tconflicts = iris
\toptions = !strip
\tsource_x86_64 = https://github.com/{REPO}/releases/download/v{ver}/{x64_asset}
\tsha256sums_x86_64 = {sha_x64}
\tsource_aarch64 = https://github.com/{REPO}/releases/download/v{ver}/{arm_asset}
\tsha256sums_aarch64 = {sha_arm}

pkgname = iris-bin
"""
    return pkgbuild, srcinfo


def verify_generated(out_dir: Path, expected_ver: str) -> None:
    scoop_data = json.loads((out_dir / "scoop" / "iris.json").read_text(encoding="utf-8"))
    assert scoop_data["version"] == expected_ver
    assert "bin\\iris.exe" in scoop_data["bin"]
    assert "64bit" in scoop_data["architecture"] and "arm64" in scoop_data["architecture"]

    try:
        import yaml  # type: ignore
    except ImportError:
        yaml = None
    for name in (
        "EclipseSoftworks.IRIS.yaml",
        "EclipseSoftworks.IRIS.installer.yaml",
        "EclipseSoftworks.IRIS.locale.en-US.yaml",
    ):
        txt = (out_dir / "winget" / name).read_text(encoding="utf-8")
        if yaml is not None:
            parsed = yaml.safe_load(txt)
            assert parsed["PackageIdentifier"] == "EclipseSoftworks.IRIS"
            assert parsed["PackageVersion"] == expected_ver
        else:
            assert "PackageIdentifier: EclipseSoftworks.IRIS" in txt

    rb = (out_dir / "homebrew" / "iris.rb").read_text(encoding="utf-8")
    for needle in ("class Iris < Formula", "def install", "generate_completions_from_executable", "test do"):
        assert needle in rb, f"homebrew/iris.rb missing `{needle}`"

    pkgbuild = (out_dir / "aur" / "PKGBUILD").read_text(encoding="utf-8")
    srcinfo = (out_dir / "aur" / ".SRCINFO").read_text(encoding="utf-8")
    m = re.search(r"^pkgver=(\S+)", pkgbuild, re.M)
    assert m and "-" not in m.group(1), f"Invalid Arch pkgver in PKGBUILD: {m.group(1) if m else None}"
    assert "pkgbase = iris-bin" in srcinfo
    assert f"_tagver={expected_ver}" in pkgbuild
    print(f"[PASS] Verified all generated manifests for v{expected_ver} in {out_dir}")


def main() -> int:
    parser = argparse.ArgumentParser(description="Generate package manager manifests for IRIS")
    parser.add_argument("--version", required=True, help="Release version (e.g. 1.0.0-rc3)")
    parser.add_argument("--sums", type=Path, help="Path to SHA256SUMS.txt")
    parser.add_argument("--placeholder", action="store_true", help="Allow zero placeholder SHA-256 hashes")
    parser.add_argument("--out", type=Path, default=Path("packaging"), help="Output directory")
    parser.add_argument("--check", action="store_true", help="Validate generated manifests")
    args = parser.parse_args()

    if not args.sums and not args.placeholder:
        parser.error("Specify either --sums SHA256SUMS.txt or --placeholder")

    ver = args.version.lstrip("v")
    tag = f"v{ver}"
    sums = parse_sha256sums(args.sums) if args.sums else {}

    out = args.out
    for sub in ("homebrew", "scoop", "winget", "aur"):
        (out / sub).mkdir(parents=True, exist_ok=True)

    (out / "homebrew" / "iris.rb").write_text(render_homebrew(ver, tag, sums, args.placeholder), encoding="utf-8")
    (out / "scoop" / "iris.json").write_text(render_scoop(ver, tag, sums, args.placeholder), encoding="utf-8")
    for fname, content in render_winget(ver, tag, sums, args.placeholder).items():
        (out / "winget" / fname).write_text(content, encoding="utf-8")
    pkgbuild, srcinfo = render_aur(ver, tag, sums, args.placeholder)
    (out / "aur" / "PKGBUILD").write_text(pkgbuild, encoding="utf-8")
    (out / "aur" / ".SRCINFO").write_text(srcinfo, encoding="utf-8")

    if args.check:
        verify_generated(out, ver)
    else:
        print(f"Generated manifests for {tag} -> {out}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
