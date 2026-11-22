#!/usr/bin/env bash
# ──────────────────────────────────────────────────────────────────────────
# IRIS Language Installer for Linux & macOS
# ──────────────────────────────────────────────────────────────────────────
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/Eclipse-Softworks/IRIS/main/installer/install.sh | bash
#   bash install.sh [--version v1.0.0-rc3] [--dir ~/.iris] [--lightweight] [--no-modify-path] [--no-vscode] [-y]
# ──────────────────────────────────────────────────────────────────────────
set -euo pipefail

REPO="Eclipse-Softworks/IRIS"
REQUESTED_VERSION="${IRIS_VERSION:-}"
INSTALL_DIR="${IRIS_INSTALL_DIR:-$HOME/.iris}"
LIGHTWEIGHT=false
MODIFY_PATH=true
INSTALL_VSCODE=true
YES=false

while [[ $# -gt 0 ]]; do
    case "$1" in
        --version)        REQUESTED_VERSION="$2"; shift 2 ;;
        --dir|--prefix)   INSTALL_DIR="$2";       shift 2 ;;
        --lightweight)    LIGHTWEIGHT=true;       shift ;;
        --no-modify-path) MODIFY_PATH=false;      shift ;;
        --no-vscode)      INSTALL_VSCODE=false;   shift ;;
        -y|--yes)         YES=true;               shift ;;
        -h|--help)
            echo "IRIS Language Installer"
            echo "Usage: install.sh [--version TAG] [--dir PREFIX] [--lightweight] [--no-modify-path] [--no-vscode] [--yes]"
            exit 0
            ;;
        *) echo "Unknown option: $1" >&2; exit 1 ;;
    esac
done

BIN_DIR="$INSTALL_DIR/bin"
SHARE_DIR="$INSTALL_DIR/share/iris"
RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[0;33m'
CYAN='\033[0;36m'; BOLD='\033[1m'; RESET='\033[0m'

banner() {
    echo ""
    echo -e "  ${BOLD}IRIS Language Installer${RESET}"
    echo -e "  Relocatable compiler, LLVM toolchain & UI host -> ${INSTALL_DIR}"
    echo ""
}
step() { echo -e "  ${YELLOW}-->${RESET} $1"; }
ok()   { echo -e "  ${GREEN}[OK]${RESET} $1"; }
info() { echo -e "  ${CYAN}[i]${RESET}  $1"; }
warn() { echo -e "  ${YELLOW}[!]${RESET}  $1"; }
err()  { echo -e "  ${RED}[X]${RESET}  $1"; }

detect_target() {
    local os arch
    os="$(uname -s)"
    arch="$(uname -m)"
    case "$os" in
        Linux)
            case "$arch" in
                x86_64|amd64)  echo "x86_64-unknown-linux-gnu" ;;
                aarch64|arm64) echo "aarch64-unknown-linux-gnu" ;;
                *) err "Unsupported Linux architecture: $arch"; exit 1 ;;
            esac
            ;;
        Darwin)
            case "$arch" in
                x86_64|amd64)  echo "x86_64-apple-darwin" ;;
                aarch64|arm64) echo "aarch64-apple-darwin" ;;
                *) err "Unsupported macOS architecture: $arch"; exit 1 ;;
            esac
            ;;
        *) err "Unsupported OS: $os (use install.ps1 on Windows)"; exit 1 ;;
    esac
}

http_get() {
    local url="$1" dest="$2"
    if command -v curl &>/dev/null; then
        curl -fsSL "$url" -o "$dest"
    elif command -v wget &>/dev/null; then
        wget -q "$url" -O "$dest"
    else
        err "Neither curl nor wget found."
        exit 1
    fi
}

http_api() {
    local url="$1"
    if command -v curl &>/dev/null; then
        curl -fsSL -H "Accept: application/vnd.github+json" -H "User-Agent: iris-installer" "$url"
    else
        wget -qO- "$url"
    fi
}

verify_sha256() {
    local file="$1" sums="$2" name="$3"
    local expected actual
    expected="$(grep -F "  ${name}" "$sums" 2>/dev/null | awk '{print $1}' | head -1 || true)"
    if [[ -z "$expected" ]]; then
        expected="$(grep -F "${name}" "$sums" 2>/dev/null | awk '{print $1}' | head -1 || true)"
    fi
    [[ -n "$expected" ]] || return 0
    if command -v sha256sum &>/dev/null; then
        actual="$(sha256sum "$file" | awk '{print $1}')"
    elif command -v shasum &>/dev/null; then
        actual="$(shasum -a 256 "$file" | awk '{print $1}')"
    else
        warn "No sha256sum/shasum tool found — skipping digest verification."
        return 0
    fi
    if [[ "$actual" != "$expected" ]]; then
        err "SHA-256 verification failed for $name (expected $expected, got $actual)"
        exit 1
    fi
    ok "Verified SHA-256 checksum for $name"
}

find_local_root() {
    local script_path="${BASH_SOURCE[0]:-}"
    [[ -n "$script_path" && -f "$script_path" ]] || return 1
    local d
    d="$(cd "$(dirname "$script_path")" && pwd)"
    if [[ -f "$d/iris" || -f "$d/bin/iris" ]]; then
        echo "$d"
        return 0
    fi
    if [[ -f "$d/../target/release/iris" ]]; then
        echo "$d/.."
        return 0
    fi
    return 1
}

stage_tree() {
    local src="$1"
    mkdir -p "$BIN_DIR" "$SHARE_DIR"

    # Binary
    if [[ -f "$src/bin/iris" ]]; then
        cp "$src/bin/iris" "$BIN_DIR/iris"
    elif [[ -f "$src/iris" ]]; then
        cp "$src/iris" "$BIN_DIR/iris"
    elif [[ -f "$src/target/release/iris" ]]; then
        cp "$src/target/release/iris" "$BIN_DIR/iris"
    else
        err "Could not find iris binary in $src"
        exit 1
    fi
    chmod 755 "$BIN_DIR/iris"
    ok "Installed iris -> $BIN_DIR/iris"

    # UI Host library
    local ui_name="libiris_ui_host.so"
    if [[ "$(uname -s)" == "Darwin" ]]; then
        ui_name="libiris_ui_host.dylib"
    fi
    for cand in "$src/bin/$ui_name" "$src/$ui_name" "$src/lib/$ui_name" "$src/target/release/$ui_name"; do
        if [[ -f "$cand" ]]; then
            cp "$cand" "$BIN_DIR/$ui_name"
            chmod 755 "$BIN_DIR/$ui_name"
            ok "Installed $ui_name -> $BIN_DIR/$ui_name"
            break
        fi
    done

    # Bundled LLVM/lld toolchain (optional if --lightweight)
    if [[ "$LIGHTWEIGHT" == "false" && -d "$src/toolchain" ]]; then
        rm -rf "$INSTALL_DIR/toolchain"
        mkdir -p "$INSTALL_DIR/toolchain"
        cp -RP "$src/toolchain/." "$INSTALL_DIR/toolchain/"
        ok "Installed bundled LLVM toolchain -> $INSTALL_DIR/toolchain"
    fi

    # Metadata, logo icon & VSIX
    for meta in LLVM_VERSION LICENSE README.md; do
        if [[ -f "$src/$meta" ]]; then
            cp "$src/$meta" "$SHARE_DIR/"
        elif [[ -f "$src/share/iris/$meta" ]]; then
            cp "$src/share/iris/$meta" "$SHARE_DIR/"
        fi
    done
    for icand in "$src/share/iris/iris.png" "$src/iris.png" "$src/installer/icon.png" "$src/logo/iris-logo.png"; do
        if [[ -f "$icand" ]]; then
            cp "$icand" "$SHARE_DIR/iris.png"
            if [[ "$(uname -s)" == "Linux" ]]; then
                local xdg_icon_dir="${XDG_DATA_HOME:-$HOME/.local/share}/icons/hicolor/256x256/apps"
                mkdir -p "$xdg_icon_dir" 2>/dev/null || true
                cp "$icand" "$xdg_icon_dir/iris.png" 2>/dev/null || true
            fi
            break
        fi
    done
    local vsix=""
    vsix="$(find "$src" -maxdepth 3 -name 'iris-lang-*.vsix' 2>/dev/null | sort -r | head -1 || true)"
    if [[ -n "$vsix" ]]; then
        cp "$vsix" "$SHARE_DIR/"
    fi

    # Remove macOS Gatekeeper quarantine from extracted binaries/libraries
    if [[ "$(uname -s)" == "Darwin" ]] && command -v xattr &>/dev/null; then
        xattr -cr "$INSTALL_DIR" 2>/dev/null || true
    fi
}

configure_path() {
    [[ "$MODIFY_PATH" == "true" ]] || { info "Skipped shell PATH configuration (--no-modify-path)."; return; }
    local line="export PATH=\"$BIN_DIR:\$PATH\""
    local profiles=("$HOME/.zshrc" "$HOME/.zprofile" "$HOME/.bashrc" "$HOME/.profile")
    local touched=false

    for profile in "${profiles[@]}"; do
        if [[ -f "$profile" ]]; then
            if ! grep -qF "$BIN_DIR" "$profile" 2>/dev/null; then
                printf '\n# IRIS Language\n%s\n' "$line" >> "$profile"
                ok "Added PATH entry to $(basename "$profile")"
            else
                info "PATH already present in $(basename "$profile")"
            fi
            touched=true
        fi
    done

    if [[ "$touched" == "false" && "$(uname -s)" == "Darwin" ]]; then
        printf '# IRIS Language\n%s\n' "$line" > "$HOME/.zshrc"
        ok "Created ~/.zshrc with IRIS PATH entry"
    fi

    # Fish shell support
    local fish_cfg="$HOME/.config/fish/config.fish"
    if [[ -f "$fish_cfg" ]] && ! grep -qF "$BIN_DIR" "$fish_cfg" 2>/dev/null; then
        printf '\n# IRIS Language\nfish_add_path "%s"\n' "$BIN_DIR" >> "$fish_cfg"
        ok "Added PATH entry to config.fish"
    fi

    export PATH="$BIN_DIR:$PATH"
}

install_editor_extension() {
    [[ "$INSTALL_VSCODE" == "true" ]] || return 0
    local vsix=""
    vsix="$(find "$SHARE_DIR" -maxdepth 1 -name 'iris-lang-*.vsix' 2>/dev/null | sort -r | head -1 || true)"
    [[ -n "$vsix" ]] || return 0

    for cli in code code-insiders cursor codium; do
        if command -v "$cli" &>/dev/null; then
            step "Installing VS Code extension via $cli..."
            if "$cli" --install-extension "$vsix" --force >/dev/null 2>&1; then
                ok "Installed $(basename "$vsix") into $cli"
                return 0
            fi
        fi
    done
    info "VS Code / Cursor CLI not found — VSIX saved to $vsix"
}

main() {
    banner
    local local_src=""
    if local_src="$(find_local_root)"; then
        step "Installing from local bundle ($local_src)..."
        stage_tree "$local_src"
    else
        local target tag archive url sums_url tmp
        target="$(detect_target)"
        if [[ -n "$REQUESTED_VERSION" ]]; then
            tag="$REQUESTED_VERSION"
        else
            step "Resolving latest IRIS release..."
            tag="$(http_api "https://api.github.com/repos/${REPO}/releases/latest" \
                | sed -n 's/.*"tag_name":[[:space:]]*"\([^"]*\)".*/\1/p' | head -1)"
        fi
        if [[ -z "$tag" ]]; then
            err "Could not resolve release tag from GitHub. Pass --version vX.Y.Z."
            exit 1
        fi
        if [[ "$LIGHTWEIGHT" == "true" ]]; then
            archive="iris-${tag}-${target}-lightweight.tar.gz"
        else
            archive="iris-${tag}-${target}.tar.gz"
        fi
        url="https://github.com/${REPO}/releases/download/${tag}/${archive}"
        sums_url="https://github.com/${REPO}/releases/download/${tag}/SHA256SUMS.txt"

        tmp="$(mktemp -d)"
        trap 'rm -rf "$tmp"' EXIT

        step "Downloading ${archive} (${tag})..."
        http_get "$url" "$tmp/$archive"

        if http_get "$sums_url" "$tmp/SHA256SUMS.txt" 2>/dev/null; then
            verify_sha256 "$tmp/$archive" "$tmp/SHA256SUMS.txt" "$archive"
        else
            warn "SHA256SUMS.txt not found — continuing without checksum check."
        fi

        mkdir -p "$tmp/unpack"
        tar xzf "$tmp/$archive" -C "$tmp/unpack"
        stage_tree "$tmp/unpack"

        if [[ "$INSTALL_VSCODE" == "true" ]] && ! find "$SHARE_DIR" -maxdepth 1 -name 'iris-lang-*.vsix' | grep -q .; then
            local vsix_url="https://github.com/${REPO}/releases/download/${tag}/iris-lang-${tag#v}.vsix"
            http_get "$vsix_url" "$SHARE_DIR/iris-lang-${tag#v}.vsix" 2>/dev/null || true
        fi
    fi

    configure_path
    install_editor_extension

    step "Installing shell completions..."
    mkdir -p "$SHARE_DIR/completions"
    "$BIN_DIR/iris" completions bash > "$SHARE_DIR/completions/iris.bash" 2>/dev/null || true
    "$BIN_DIR/iris" completions zsh  > "$SHARE_DIR/completions/_iris" 2>/dev/null || true
    "$BIN_DIR/iris" completions fish > "$SHARE_DIR/completions/iris.fish" 2>/dev/null || true
    if [[ -d "$HOME/.config/fish" ]]; then
        mkdir -p "$HOME/.config/fish/completions"
        cp "$SHARE_DIR/completions/iris.fish" "$HOME/.config/fish/completions/iris.fish" 2>/dev/null || true
    fi

    echo ""
    step "Running iris doctor..."
    "$BIN_DIR/iris" doctor || warn "Run \`iris doctor --fix\` if any component needs repair."

    echo ""
    echo -e "  ${GREEN}IRIS installed into ${INSTALL_DIR}${RESET}"
    echo -e "  Restart your shell (or run: ${CYAN}export PATH=\"$BIN_DIR:\$PATH\"${RESET}) and run ${CYAN}iris --version${RESET}."
    echo ""
}

main "$@"
