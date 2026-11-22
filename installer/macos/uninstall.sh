#!/usr/bin/env bash
# ──────────────────────────────────────────────────────────────────────────
# IRIS Language Uninstaller for macOS
# ──────────────────────────────────────────────────────────────────────────
set -euo pipefail

RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[0;33m'
CYAN='\033[0;36m'; BOLD='\033[1m'; RESET='\033[0m'

INSTALL_DIR="${IRIS_INSTALL_DIR:-$HOME/.iris}"
BIN_DIR="$INSTALL_DIR/bin"

banner() {
    echo ""
    echo -e "  ${BOLD}IRIS Language Uninstaller (macOS)${RESET}"
    echo -e "  Intermediate Representation for Intelligent Systems"
    echo ""
}

step()  { echo -e "  ${YELLOW}-->${RESET} $1"; }
ok()    { echo -e "  ${GREEN}[OK]${RESET} $1"; }
info()  { echo -e "  ${CYAN}[i]${RESET}  $1"; }
warn()  { echo -e "  ${YELLOW}[!]${RESET}  $1"; }

confirm() {
    local prompt="$1"
    echo -ne "  $prompt [y/N] "
    read -r answer
    [[ "$answer" =~ ^[yY]$ ]]
}

banner

echo -e "  This will uninstall IRIS from your machine."
echo -e "  Install directory: ${INSTALL_DIR}"
echo ""

if ! confirm "Proceed with uninstallation?"; then
    echo ""
    info "Uninstall cancelled."
    exit 0
fi

echo ""

# ── Step 1: Remove from shell profiles ────────────────────────────────────
step "Removing IRIS from shell profiles..."
profiles=("$HOME/.zshrc" "$HOME/.zprofile" "$HOME/.bashrc" "$HOME/.bash_profile" "$HOME/.profile")
for profile in "${profiles[@]}"; do
    if [[ -f "$profile" ]]; then
        if grep -qF "$BIN_DIR" "$profile" 2>/dev/null; then
            # macOS sed requires -i '' (no backup extension)
            sed -i '' "/# IRIS Language/d" "$profile"
            sed -i '' "\|${BIN_DIR}|d" "$profile"
            ok "Cleaned $(basename "$profile")"
        fi
    fi
done

# ── Step 2: Remove VSCode extension ──────────────────────────────────────
step "Removing VSCode extension..."
if command -v code &>/dev/null; then
    extensions="$(code --list-extensions 2>/dev/null | grep -i iris || true)"
    if [[ -n "$extensions" ]]; then
        while IFS= read -r ext; do
            code --uninstall-extension "$ext" 2>/dev/null && ok "Removed extension: $ext" || \
                warn "Could not remove extension: $ext"
        done <<< "$extensions"
    else
        info "No IRIS VSCode extension found."
    fi
else
    info "VSCode not detected — skipping extension removal."
fi

# ── Step 3: Remove .pkg receipt (if installed via .pkg) ───────────────────
for pkg_id in "dev.eclipsesoftworks.iris" "dev.moon9t.iris"; do
    if pkgutil --pkgs 2>/dev/null | grep -q "$pkg_id"; then
        step "Removing .pkg receipt ($pkg_id)..."
        sudo pkgutil --forget "$pkg_id" 2>/dev/null && ok "Removed .pkg receipt." || \
            warn "Could not remove .pkg receipt. Try: sudo pkgutil --forget $pkg_id"
    fi
done

# ── Step 4: Remove install directory ──────────────────────────────────────
echo ""
if [[ -d "$INSTALL_DIR" ]]; then
    if confirm "Delete ${INSTALL_DIR} and all its contents?"; then
        rm -rf "$INSTALL_DIR"
        ok "Removed: $INSTALL_DIR"
    else
        info "Skipped directory removal. Files remain at: $INSTALL_DIR"
    fi
else
    info "Directory not found (already removed): $INSTALL_DIR"
fi

# ── Step 5: Remove /usr/local/bin symlink (if present from .pkg) ──────────
if [[ -L "/usr/local/bin/iris" ]]; then
    step "Removing /usr/local/bin/iris symlink..."
    sudo rm -f "/usr/local/bin/iris" 2>/dev/null && ok "Removed symlink." || \
        warn "Could not remove symlink. Try: sudo rm /usr/local/bin/iris"
fi

# ── Done ──────────────────────────────────────────────────────────────────
echo ""
echo -e "  ${GREEN}════════════════════════════════════════════════════════════${RESET}"
echo -e "  ${GREEN}  IRIS has been uninstalled.${RESET}"
echo -e "  ${GREEN}════════════════════════════════════════════════════════════${RESET}"
echo ""
echo -e "  ${YELLOW}Open a new terminal for PATH changes to take effect.${RESET}"
echo ""
