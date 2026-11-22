#!/usr/bin/env bash
# ──────────────────────────────────────────────────────────────────────────
# sign_and_notarize.sh — macOS code signing and Apple notarization helper
# ──────────────────────────────────────────────────────────────────────────
# Usage:
#   sign_and_notarize.sh sign-binaries <dir> [--require-signing]
#   sign_and_notarize.sh notarize-installer <artifact.{pkg,dmg}> [--require-signing]
#
# Required env secrets when active:
#   APPLE_CERT_P12_BASE64, APPLE_CERT_PASSWORD, APPLE_SIGNING_IDENTITY
#   For .pkg signing: APPLE_INSTALLER_IDENTITY (defaults to APPLE_SIGNING_IDENTITY if omitted)
#   For notarization (either API key or Apple ID):
#     APPLE_NOTARY_KEY_BASE64, APPLE_NOTARY_KEY_ID, APPLE_NOTARY_ISSUER_ID
#     or: APPLE_ID, APPLE_APP_PASSWORD, APPLE_TEAM_ID
# ──────────────────────────────────────────────────────────────────────────
set -euo pipefail

if [[ $# -lt 2 ]]; then
    echo "Usage: $0 <sign-binaries|notarize-installer> <path> [--require-signing]" >&2
    exit 1
fi

MODE="$1"
TARGET="$2"
REQUIRE=false
if [[ "${3:-}" == "--require-signing" ]]; then
    REQUIRE=true
fi

if [[ -z "${APPLE_CERT_P12_BASE64:-}" || -z "${APPLE_SIGNING_IDENTITY:-}" ]]; then
    if [[ "$REQUIRE" == "true" ]]; then
        echo "ERROR: Apple signing requested (--require-signing), but APPLE_CERT_P12_BASE64 / APPLE_SIGNING_IDENTITY is unset." >&2
        exit 1
    fi
    echo "[skip] Apple signing credentials not configured (leaving artifact unsigned)."
    exit 0
fi

WORK_DIR="$(mktemp -d)"
KEYCHAIN="$WORK_DIR/iris-build.keychain-db"
KEYCHAIN_PW="$(openssl rand -hex 24)"
DEFAULT_KC=""

cleanup() {
    if [[ -n "$DEFAULT_KC" ]]; then
        security list-keychains -d user -s "$DEFAULT_KC" >/dev/null 2>&1 || true
    fi
    security delete-keychain "$KEYCHAIN" >/dev/null 2>&1 || true
    rm -rf "$WORK_DIR"
}
trap cleanup EXIT

DEFAULT_KC="$(security default-keychain -d user | tr -d ' "')"
echo "$APPLE_CERT_P12_BASE64" | base64 --decode > "$WORK_DIR/cert.p12"
security create-keychain -p "$KEYCHAIN_PW" "$KEYCHAIN"
security set-keychain-settings -lut 21600 "$KEYCHAIN"
security unlock-keychain -p "$KEYCHAIN_PW" "$KEYCHAIN"
security import "$WORK_DIR/cert.p12" -k "$KEYCHAIN" -P "${APPLE_CERT_PASSWORD:-}" -T /usr/bin/codesign -T /usr/bin/productsign
security set-key-partition-list -S apple-tool:,apple:,codesign: -s -k "$KEYCHAIN_PW" "$KEYCHAIN"
security list-keychains -d user -s "$KEYCHAIN" "$DEFAULT_KC"

notarize_and_staple() {
    local artifact="$1"
    if [[ -n "${APPLE_NOTARY_KEY_BASE64:-}" && -n "${APPLE_NOTARY_KEY_ID:-}" && -n "${APPLE_NOTARY_ISSUER_ID:-}" ]]; then
        echo "$APPLE_NOTARY_KEY_BASE64" | base64 --decode > "$WORK_DIR/AuthKey.p8"
        xcrun notarytool submit "$artifact" \
            --key "$WORK_DIR/AuthKey.p8" \
            --key-id "$APPLE_NOTARY_KEY_ID" \
            --issuer "$APPLE_NOTARY_ISSUER_ID" \
            --wait
    elif [[ -n "${APPLE_ID:-}" && -n "${APPLE_APP_PASSWORD:-}" && -n "${APPLE_TEAM_ID:-}" ]]; then
        xcrun notarytool submit "$artifact" \
            --apple-id "$APPLE_ID" \
            --password "$APPLE_APP_PASSWORD" \
            --team-id "$APPLE_TEAM_ID" \
            --wait
    else
        echo "[skip] Notarization credentials not set — signed only ($artifact)."
        return 0
    fi
    xcrun stapler staple "$artifact"
    echo "[ok] Notarized and stapled: $artifact"
}

case "$MODE" in
    sign-binaries)
        if [[ ! -d "$TARGET" ]]; then
            echo "Directory not found: $TARGET" >&2
            exit 1
        fi
        while IFS= read -r f; do
            if file "$f" | grep -q "Mach-O"; then
                codesign --force --options runtime --timestamp --keychain "$KEYCHAIN" --sign "$APPLE_SIGNING_IDENTITY" "$f"
                codesign --verify --strict "$f"
            fi
        done < <(find "$TARGET" -type f)
        echo "[ok] Signed all Mach-O binaries/libraries under $TARGET"
        ;;
    notarize-installer)
        if [[ ! -f "$TARGET" ]]; then
            echo "Artifact not found: $TARGET" >&2
            exit 1
        fi
        case "$TARGET" in
            *.pkg)
                INSTALLER_ID="${APPLE_INSTALLER_IDENTITY:-$APPLE_SIGNING_IDENTITY}"
                SIGNED_PKG="$WORK_DIR/$(basename "$TARGET")"
                productsign --keychain "$KEYCHAIN" --sign "$INSTALLER_ID" "$TARGET" "$SIGNED_PKG"
                mv "$SIGNED_PKG" "$TARGET"
                notarize_and_staple "$TARGET"
                ;;
            *.dmg)
                codesign --force --timestamp --keychain "$KEYCHAIN" --sign "$APPLE_SIGNING_IDENTITY" "$TARGET"
                notarize_and_staple "$TARGET"
                ;;
            *)
                echo "Unsupported artifact type for notarization: $TARGET" >&2
                exit 1
                ;;
        esac
        ;;
    *)
        echo "Unknown mode: $MODE" >&2
        exit 1
        ;;
esac
