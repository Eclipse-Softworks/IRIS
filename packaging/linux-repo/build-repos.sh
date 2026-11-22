#!/usr/bin/env bash
# ──────────────────────────────────────────────────────────────────────────
# build-repos.sh — Build static APT (.deb) and YUM/DNF (.rpm) repository trees
# ──────────────────────────────────────────────────────────────────────────
# Usage:
#   bash packaging/linux-repo/build-repos.sh <input_dir_with_deb_and_rpm> <output_repo_dir>
# Optional environment variables:
#   GPG_KEY_ID   — GPG key fingerprint/ID used to sign APT Release & RPM repomd.xml
#   BASE_URL     — Public URL shown on index.html (default: https://eclipse-softworks.github.io/iris-packages)
# ──────────────────────────────────────────────────────────────────────────
set -euo pipefail

if [[ $# -lt 2 ]]; then
    echo "Usage: $0 <input_artifacts_dir> <output_repo_dir>" >&2
    exit 1
fi

INPUT_DIR="$1"
OUT_DIR="$2"
BASE_URL="${BASE_URL:-https://eclipse-softworks.github.io/iris-packages}"
APT_DIR="$OUT_DIR/apt"
RPM_DIR="$OUT_DIR/rpm"

rm -rf "$OUT_DIR"
mkdir -p "$APT_DIR/pool/main" "$RPM_DIR/x86_64" "$RPM_DIR/aarch64"

echo "Collecting .deb and .rpm artifacts from $INPUT_DIR..."
DEB_COUNT=0
while IFS= read -r deb; do
    cp "$deb" "$APT_DIR/pool/main/"
    ((DEB_COUNT++)) || true
done < <(find "$INPUT_DIR" -type f -name '*.deb')

RPM_COUNT=0
while IFS= read -r rpm; do
    case "$(basename "$rpm")" in
        *.aarch64.rpm) cp "$rpm" "$RPM_DIR/aarch64/" ;;
        *)             cp "$rpm" "$RPM_DIR/x86_64/" ;;
    esac
    ((RPM_COUNT++)) || true
done < <(find "$INPUT_DIR" -type f -name '*.rpm')

echo "  Found $DEB_COUNT .deb file(s) and $RPM_COUNT .rpm file(s)."

# ── Build APT metadata ────────────────────────────────────────────────────
for arch in amd64 arm64; do
    BIN_DIR="$APT_DIR/dists/stable/main/binary-$arch"
    mkdir -p "$BIN_DIR"
    if command -v dpkg-scanpackages &>/dev/null; then
        (cd "$APT_DIR" && dpkg-scanpackages --arch "$arch" pool/main /dev/null > "dists/stable/main/binary-${arch}/Packages")
    else
        : > "$BIN_DIR/Packages"
    fi
    gzip -9c "$BIN_DIR/Packages" > "$BIN_DIR/Packages.gz"
done

RELEASE_FILE="$APT_DIR/dists/stable/Release"
if command -v apt-ftparchive &>/dev/null; then
    apt-ftparchive \
        -o APT::FTPArchive::Release::Origin="Eclipse Softworks" \
        -o APT::FTPArchive::Release::Label="IRIS Language" \
        -o APT::FTPArchive::Release::Suite="stable" \
        -o APT::FTPArchive::Release::Codename="stable" \
        -o APT::FTPArchive::Release::Architectures="amd64 arm64" \
        -o APT::FTPArchive::Release::Components="main" \
        release "$APT_DIR/dists/stable" > "$RELEASE_FILE"
else
    {
        echo "Origin: Eclipse Softworks"
        echo "Label: IRIS Language"
        echo "Suite: stable"
        echo "Codename: stable"
        echo "Architectures: amd64 arm64"
        echo "Components: main"
        echo "Date: $(LC_ALL=C date -Ru)"
        echo "SHA256:"
        (cd "$APT_DIR/dists/stable" && find main -type f | sort | while read -r f; do
            sz="$(wc -c < "$f" | tr -d ' ')"
            h="$(sha256sum "$f" | awk '{print $1}')"
            printf " %s %16s %s\n" "$h" "$sz" "$f"
        done)
    } > "$RELEASE_FILE"
fi

if [[ -n "${GPG_KEY_ID:-}" ]] && command -v gpg &>/dev/null; then
    echo "Signing APT metadata with GPG key $GPG_KEY_ID..."
    gpg --batch --yes -u "$GPG_KEY_ID" --armor --detach-sign -o "$APT_DIR/dists/stable/Release.gpg" "$RELEASE_FILE"
    gpg --batch --yes -u "$GPG_KEY_ID" --clearsign -o "$APT_DIR/dists/stable/InRelease" "$RELEASE_FILE"
    gpg --export "$GPG_KEY_ID" > "$APT_DIR/iris-archive-keyring.gpg"
    gpg --armor --export "$GPG_KEY_ID" > "$RPM_DIR/iris-rpm.key"
fi

# ── Build RPM metadata ────────────────────────────────────────────────────
for arch in x86_64 aarch64; do
    if command -v createrepo_c &>/dev/null; then
        createrepo_c "$RPM_DIR/$arch"
    elif command -v createrepo &>/dev/null; then
        createrepo "$RPM_DIR/$arch"
    fi
    if [[ -n "${GPG_KEY_ID:-}" && -f "$RPM_DIR/$arch/repodata/repomd.xml" ]] && command -v gpg &>/dev/null; then
        gpg --batch --yes -u "$GPG_KEY_ID" --detach-sign --armor -o "$RPM_DIR/$arch/repodata/repomd.xml.asc" "$RPM_DIR/$arch/repodata/repomd.xml"
    fi
done

cat > "$OUT_DIR/index.html" << EOF
<!DOCTYPE html>
<html lang="en">
<head><meta charset="utf-8"><title>IRIS Linux Packages</title></head>
<body>
<h1>IRIS Language — APT &amp; RPM Repositories</h1>
<h2>Debian / Ubuntu (APT)</h2>
<pre>
curl -fsSL ${BASE_URL}/apt/iris-archive-keyring.gpg | sudo tee /usr/share/keyrings/iris-archive-keyring.gpg >/dev/null
echo "deb [signed-by=/usr/share/keyrings/iris-archive-keyring.gpg] ${BASE_URL}/apt stable main" | sudo tee /etc/apt/sources.list.d/iris.list
sudo apt-get update &amp;&amp; sudo apt-get install -y iris
</pre>
<h2>Fedora / RHEL (DNF)</h2>
<pre>
sudo tee /etc/yum.repos.d/iris.repo &lt;&lt; 'REPO'
[iris]
name=IRIS Language
baseurl=${BASE_URL}/rpm/\$basearch
enabled=1
gpgcheck=1
gpgkey=${BASE_URL}/rpm/iris-rpm.key
REPO
sudo dnf install -y iris
</pre>
</body>
</html>
EOF

echo "Linux APT & RPM repository built at $OUT_DIR"
