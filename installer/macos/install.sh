#!/usr/bin/env bash
# ──────────────────────────────────────────────────────────────────────────
# IRIS Language Installer for macOS
# ──────────────────────────────────────────────────────────────────────────
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/Eclipse-Softworks/IRIS/main/installer/macos/install.sh | bash
#   # or
#   bash install.sh
#
# Installs iris, libiris_ui_host.dylib, and bundled LLVM toolchain into ~/.iris.
# ──────────────────────────────────────────────────────────────────────────
set -euo pipefail

REPO="Eclipse-Softworks/IRIS"
VERSION="${IRIS_VERSION:-}"
INSTALL_DIR="${IRIS_INSTALL_DIR:-$HOME/.iris}"
BIN_DIR="$INSTALL_DIR/bin"
SHARE_DIR="$INSTALL_DIR/share/iris"

# ── Colours ───────────────────────────────────────────────────────────────
RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[0;33m'
CYAN='\033[0;36m'; BOLD='\033[1m'; RESET='\033[0m'

banner() {
    echo ""
    echo -e "${CYAN}  ██╗██████╗ ██╗███████╗${RESET}"
    echo -e "${CYAN}  ██║██╔══██╗██║██╔════╝${RESET}"
    echo -e "${CYAN}  ██║██████╔╝██║███████╗${RESET}"
    echo -e "${CYAN}  ██║██╔══██╗██║╚════██║${RESET}"
    echo -e "${CYAN}  ██║██║  ██║██║███████║${RESET}"
    echo -e "${CYAN}  ╚═╝╚═╝  ╚═╝╚═╝╚══════╝${RESET}"
    echo ""
    echo -e "  ${BOLD}IRIS Language Installer  v${VERSION}${RESET}"
    echo -e "  Intermediate Representation for Intelligent Systems"
    echo ""
}

step()  { echo -e "  ${YELLOW}-->${RESET} $1"; }
ok()    { echo -e "  ${GREEN}[OK]${RESET} $1"; }
info()  { echo -e "  ${CYAN}[i]${RESET}  $1"; }
warn()  { echo -e "  ${YELLOW}[!]${RESET}  $1"; }
err()   { echo -e "  ${RED}[X]${RESET}  $1"; }

# ── Detect architecture ──────────────────────────────────────────────────
detect_arch() {
    local arch
    arch="$(uname -m)"
    case "$arch" in
        x86_64|amd64)  echo "x86_64-apple-darwin" ;;
        aarch64|arm64) echo "aarch64-apple-darwin" ;;
        *)             err "Unsupported architecture: $arch"; exit 1 ;;
    esac
}

# ── Locate or download bundle root ────────────────────────────────────────
find_iris_bundle_root() {
    local script_path="${BASH_SOURCE[0]:-}"
    if [[ -n "$script_path" && -f "$script_path" ]]; then
        local script_dir
        script_dir="$(cd "$(dirname "$script_path")" && pwd)"
        if [[ -f "$script_dir/iris" || -f "$script_dir/bin/iris" ]]; then
            echo "$script_dir"
            return
        fi
        if [[ -f "$script_dir/../../target/release/iris" ]]; then
            echo "$script_dir/../.."
            return
        fi
    fi

    local target tag archive url sums_url tmp
    target="$(detect_arch)"
    if [[ -z "$VERSION" ]]; then
        step "Resolving latest IRIS release..."
        VERSION="$(curl -fsSL -H "Accept: application/vnd.github+json" "https://api.github.com/repos/${REPO}/releases/latest" \
            | sed -n 's/.*"tag_name":[[:space:]]*"\([^"]*\)".*/\1/p' | head -1)"
    fi
    if [[ -z "$VERSION" ]]; then
        err "Could not resolve latest release tag from GitHub (set IRIS_VERSION=v1.0.0-rc3)."
        exit 1
    fi
    tag="v${VERSION#v}"
    archive="iris-${tag}-${target}.tar.gz"
    url="https://github.com/${REPO}/releases/download/${tag}/${archive}"
    sums_url="https://github.com/${REPO}/releases/download/${tag}/SHA256SUMS.txt"

    step "Downloading IRIS ${tag} for ${target}..."
    tmp="$(mktemp -d)"
    curl -fSL "$url" -o "$tmp/$archive"
    curl -fsSL "$sums_url" -o "$tmp/SHA256SUMS.txt" 2>/dev/null || true

    if [[ -f "$tmp/SHA256SUMS.txt" ]] && command -v shasum &>/dev/null; then
        local expected actual
        expected="$(grep -F "  ${archive}" "$tmp/SHA256SUMS.txt" 2>/dev/null | awk '{print $1}' | head -1 || true)"
        if [[ -n "$expected" ]]; then
            actual="$(shasum -a 256 "$tmp/$archive" | awk '{print $1}')"
            if [[ "$actual" != "$expected" ]]; then
                err "SHA-256 checksum mismatch for ${archive}"
                exit 1
            fi
            ok "Verified SHA-256 checksum for ${archive}"
        fi
    fi

    mkdir -p "$tmp/unpack"
    tar xzf "$tmp/$archive" -C "$tmp/unpack"
    echo "$tmp/unpack"
}

# ── Add to shell profile ─────────────────────────────────────────────────
add_to_path() {
    local line="export PATH=\"$BIN_DIR:\$PATH\""
    # macOS default shell is zsh since Catalina
    local profiles=("$HOME/.zshrc" "$HOME/.zprofile" "$HOME/.bashrc" "$HOME/.bash_profile")

    for profile in "${profiles[@]}"; do
        if [[ -f "$profile" ]]; then
            if ! grep -qF "$BIN_DIR" "$profile" 2>/dev/null; then
                echo "" >> "$profile"
                echo "# IRIS Language" >> "$profile"
                echo "$line" >> "$profile"
                ok "Added to PATH in $(basename "$profile")"
            else
                info "PATH already configured in $(basename "$profile")"
            fi
        fi
    done

    # If no profile exists yet, create .zshrc (macOS default)
    local found=false
    for profile in "${profiles[@]}"; do
        if [[ -f "$profile" ]]; then found=true; break; fi
    done
    if [[ "$found" == "false" ]]; then
        echo "# IRIS Language" > "$HOME/.zshrc"
        echo "$line" >> "$HOME/.zshrc"
        ok "Created ~/.zshrc with PATH entry"
    fi

    export PATH="$BIN_DIR:$PATH"
}

# ── Install stdlib ────────────────────────────────────────────────────────
install_stdlib() {
    local script_dir
    script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
    local stdlib_src=""

    if [[ -d "$script_dir/stdlib" ]]; then
        stdlib_src="$script_dir/stdlib"
    elif [[ -d "$script_dir/../../stdlib" ]]; then
        stdlib_src="$script_dir/../../stdlib"
    fi

    if [[ -n "$stdlib_src" ]]; then
        mkdir -p "$INSTALL_DIR/stdlib"
        cp -R "$stdlib_src"/* "$INSTALL_DIR/stdlib/" 2>/dev/null || true
        ok "Installed stdlib to $INSTALL_DIR/stdlib"
    fi
}

# ── Install examples ─────────────────────────────────────────────────────
install_examples() {
    local script_dir
    script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
    local examples_src=""

    if [[ -d "$script_dir/examples" ]]; then
        examples_src="$script_dir/examples"
    elif [[ -d "$script_dir/../../examples" ]]; then
        examples_src="$script_dir/../../examples"
    fi

    if [[ -n "$examples_src" ]]; then
        mkdir -p "$INSTALL_DIR/examples"
        cp -R "$examples_src"/* "$INSTALL_DIR/examples/" 2>/dev/null || true
        ok "Installed examples to $INSTALL_DIR/examples"
    fi
}

# ── Install toolchain (bundled LLVM for native compilation) ──────────────
install_toolchain() {
    local script_dir
    script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
    local tc_src=""

    if [[ -d "$script_dir/toolchain" ]]; then
        tc_src="$script_dir/toolchain"
    elif [[ -d "$script_dir/../../toolchain" ]]; then
        tc_src="$script_dir/../../toolchain"
    fi

    if [[ -n "$tc_src" ]]; then
        mkdir -p "$INSTALL_DIR/toolchain"
        cp -R "$tc_src"/* "$INSTALL_DIR/toolchain/" 2>/dev/null || true
        ok "Installed toolchain to $INSTALL_DIR/toolchain"
    else
        info "No bundled toolchain found — iris build will use system clang."
    fi
}

# ── VSCode extension ─────────────────────────────────────────────────────
install_vscode_extension() {
    local script_dir
    script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

    local vsix
    vsix="$(find "$script_dir" -maxdepth 2 -name 'iris-lang-*.vsix' 2>/dev/null | head -1)"

    if [[ -z "$vsix" ]]; then
        vsix="$(find "$script_dir/../.." -maxdepth 2 -name 'iris-lang-*.vsix' 2>/dev/null | head -1)"
    fi

    if [[ -z "$vsix" ]]; then
        info "No .vsix found — skipping VSCode extension."
        return
    fi

    if command -v code &>/dev/null; then
        step "Installing VSCode extension..."
        code --install-extension "$vsix" --force 2>/dev/null && ok "VSCode extension installed." || \
            warn "Could not install VSCode extension automatically."
    else
        info "VSCode not detected — skipping extension install."
    fi
}

# ── Check clang (macOS ships with Apple Clang via Xcode CLT) ─────────────
check_clang() {
    if command -v clang &>/dev/null; then
        local ver
        ver="$(clang --version 2>/dev/null | head -1)"
        ok "clang detected: $ver"
    else
        info "clang not found. Install Xcode Command Line Tools:"
        info "  xcode-select --install"
    fi
}

# ══════════════════════════════════════════════════════════════════════════
# Main
# ══════════════════════════════════════════════════════════════════════════
banner

# Step 1: Locate or download bundle
step "Locating IRIS bundle..."
SRC_ROOT="$(find_iris_bundle_root)"
ok "Bundle root: $SRC_ROOT"

# Step 2: Create install directory
step "Creating install directory: $BIN_DIR"
mkdir -p "$BIN_DIR" "$SHARE_DIR"
ok "Directory ready."

# Step 3: Install iris + libiris_ui_host.dylib
IRIS_BIN=""
for cand in "$SRC_ROOT/bin/iris" "$SRC_ROOT/iris" "$SRC_ROOT/target/release/iris"; do
    if [[ -f "$cand" ]]; then IRIS_BIN="$cand"; break; fi
done
if [[ -z "$IRIS_BIN" ]]; then
    err "iris binary not found under $SRC_ROOT"
    exit 1
fi
step "Installing iris -> $BIN_DIR/iris"
cp "$IRIS_BIN" "$BIN_DIR/iris"
chmod 755 "$BIN_DIR/iris"
ok "Binary installed."

for cand in "$SRC_ROOT/bin/libiris_ui_host.dylib" "$SRC_ROOT/libiris_ui_host.dylib" "$SRC_ROOT/target/release/libiris_ui_host.dylib"; do
    if [[ -f "$cand" ]]; then
        cp "$cand" "$BIN_DIR/libiris_ui_host.dylib"
        chmod 755 "$BIN_DIR/libiris_ui_host.dylib"
        ok "UI host installed -> $BIN_DIR/libiris_ui_host.dylib"
        break
    fi
done

if [[ -d "$SRC_ROOT/toolchain" ]]; then
    rm -rf "$INSTALL_DIR/toolchain"
    mkdir -p "$INSTALL_DIR/toolchain"
    cp -R "$SRC_ROOT/toolchain/." "$INSTALL_DIR/toolchain/"
    ok "Installed bundled LLVM toolchain -> $INSTALL_DIR/toolchain"
fi
for meta in LLVM_VERSION LICENSE README.md; do
    if [[ -f "$SRC_ROOT/$meta" ]]; then cp "$SRC_ROOT/$meta" "$SHARE_DIR/"; fi
done

# Strip Gatekeeper quarantine from extracted binaries/dylibs/toolchain
xattr -cr "$INSTALL_DIR" 2>/dev/null || true

# Step 4: Install examples
step "Installing examples..."
install_examples

# Step 5: Add to PATH
step "Configuring PATH..."
add_to_path

# Step 6: VSCode extension
install_vscode_extension

# Step 7: Verify via doctor
echo ""
step "Running iris doctor..."
"$BIN_DIR/iris" doctor || warn "Run \`iris doctor --fix\` if any optional component needs setup."

# Done
echo ""
echo -e "  ${GREEN}════════════════════════════════════════════════════════════${RESET}"
echo -e "  ${GREEN}  IRIS installed successfully!${RESET}"
echo -e "  ${GREEN}════════════════════════════════════════════════════════════${RESET}"
echo ""
echo -e "  Quick Start:"
echo -e "    ${CYAN}iris --version${RESET}                    # verify install"
echo -e "    ${CYAN}iris run hello.iris${RESET}               # run a program"
echo -e "    ${CYAN}iris build hello.iris -o hello${RESET}    # compile native binary"
echo -e "    ${CYAN}iris repl${RESET}                         # interactive REPL"
echo ""
echo -e "  ${YELLOW}NOTE: Open a new terminal for PATH changes to take effect.${RESET}"
echo ""
