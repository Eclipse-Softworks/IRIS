#!/usr/bin/env bash
set -euo pipefail

# Bundle non-core runtime shared libraries (e.g. libicu*.so.70, libxml2,
# libtinfo, libzstd, libz3) required by upstream LLVM 23 Linux binaries
# (built on Ubuntu 22.04) and rewrite ELF RPATHs to $ORIGIN/../lib / $ORIGIN
# so bundled clang and ld.lld run self-contained on Ubuntu 24.04 and Fedora 40+.
#
# Usage: bundle_linux_llvm_deps.sh <llvm_dir> [x86_64|aarch64]

LLVM_DIR="${1:?usage: bundle_linux_llvm_deps.sh <llvm_dir> [arch]}"
ARCH="${2:-$(uname -m)}"

LLVM_ABS="$(cd "$LLVM_DIR" && pwd)"
mkdir -p "$LLVM_ABS/lib"

if ! command -v patchelf >/dev/null 2>&1; then
    sudo apt-get update -qq
    sudo apt-get install -y -qq patchelf >/dev/null
fi

case "$ARCH" in
    x86_64|amd64)
        docker run --rm -i \
            -v "$LLVM_ABS:/llvm" \
            -e HOST_UID="$(id -u)" \
            -e HOST_GID="$(id -g)" \
            ubuntu:22.04 \
            bash -euo pipefail <<'DOCKER_X64'
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq
apt-get install -y -qq --no-install-recommends \
    libxml2 libicu70 libtinfo6 libncurses6 libzstd1 zlib1g \
    liblzma5 libedit2 libffi8 libz3-4 >/dev/null

is_core() {
    case "$1" in
        linux-vdso.so*|ld-linux*.so*|libc.so*|libm.so*|libpthread.so*|\
        libdl.so*|librt.so*|libresolv.so*|libutil.so*|libnsl.so*|libanl.so*|\
        libgcc_s.so*|libstdc++.so*)
            return 0 ;;
        *)
            return 1 ;;
    esac
}

collect_from_ldd() {
    local target="$1"
    LD_LIBRARY_PATH="/llvm/lib:${LD_LIBRARY_PATH:-}" ldd "$target" 2>/dev/null \
        | awk '/=> \//{print $1, $3}' \
        | while read -r soname path; do
            [[ -n "$soname" && -n "$path" && -e "$path" ]] || continue
            if is_core "$soname"; then
                continue
            fi
            case "$path" in
                /llvm/lib/*) continue ;;
            esac
            if [[ ! -e "/llvm/lib/$soname" ]]; then
                cp -L "$path" "/llvm/lib/$soname"
                chmod 0644 "/llvm/lib/$soname"
                echo "  bundled $soname <- $path"
            fi
        done
}

for bin in clang clang++ ld.lld lld llvm-ar; do
    if [[ -x "/llvm/bin/$bin" ]]; then
        collect_from_ldd "/llvm/bin/$bin"
    fi
done

for _pass in 1 2 3; do
    for so in /llvm/lib/*.so*; do
        [[ -f "$so" && ! -L "$so" ]] || continue
        base="$(basename "$so")"
        case "$base" in
            libclang*.so*|libLLVM*.so*) continue ;;
        esac
        collect_from_ldd "$so"
    done
done

for bin in clang ld.lld; do
    if [[ -x "/llvm/bin/$bin" ]]; then
        if LD_LIBRARY_PATH="/llvm/lib" ldd "/llvm/bin/$bin" | grep -q "not found"; then
            echo "ERROR: unresolved shared libs for $bin:" >&2
            LD_LIBRARY_PATH="/llvm/lib" ldd "/llvm/bin/$bin" | grep "not found" >&2
            exit 1
        fi
    fi
done

chown -R "$HOST_UID:$HOST_GID" /llvm/lib
DOCKER_X64
        ;;
    aarch64|arm64)
        docker run --rm -i \
            -v "$LLVM_ABS:/llvm" \
            -e HOST_UID="$(id -u)" \
            -e HOST_GID="$(id -g)" \
            ubuntu:22.04 \
            bash -euo pipefail <<'DOCKER_ARM64'
export DEBIAN_FRONTEND=noninteractive
dpkg --add-architecture arm64
cat > /etc/apt/sources.list <<'APTEOF'
deb [arch=amd64] http://archive.ubuntu.com/ubuntu jammy main universe
deb [arch=amd64] http://archive.ubuntu.com/ubuntu jammy-updates main universe
deb [arch=arm64] http://ports.ubuntu.com/ubuntu-ports jammy main universe
deb [arch=arm64] http://ports.ubuntu.com/ubuntu-ports jammy-updates main universe
APTEOF
apt-get update -qq
apt-get install -y -qq --no-install-recommends binutils python3 >/dev/null
mkdir -p /tmp/debs /tmp/sysroot
cd /tmp/debs
apt-get download \
    libxml2:arm64 libicu70:arm64 libtinfo6:arm64 libncurses6:arm64 \
    libzstd1:arm64 zlib1g:arm64 liblzma5:arm64 libedit2:arm64 \
    libffi8:arm64 libz3-4:arm64 >/dev/null
for deb in /tmp/debs/*.deb; do
    dpkg-deb -x "$deb" /tmp/sysroot
done

python3 - <<'PYEOF'
import os, re, shutil, subprocess

CORE = (
    "ld-linux-", "libc.so", "libm.so", "libpthread.so", "libdl.so",
    "librt.so", "libresolv.so", "libutil.so", "libnsl.so", "libanl.so",
    "libgcc_s.so", "libstdc++.so",
)

def is_core(name: str) -> bool:
    return name.startswith(CORE)

def needed_libs(path: str) -> list[str]:
    out = subprocess.check_output(["readelf", "-d", path], text=True, stderr=subprocess.DEVNULL)
    return re.findall(r"\(NEEDED\)\s+Shared library:\s+\[([^\]]+)\]", out)

lib_dirs = [
    "/llvm/lib",
    "/tmp/sysroot/lib/aarch64-linux-gnu",
    "/tmp/sysroot/usr/lib/aarch64-linux-gnu",
    "/tmp/sysroot/lib",
    "/tmp/sysroot/usr/lib",
]

def find_lib(soname: str):
    for d in lib_dirs:
        p = os.path.join(d, soname)
        if os.path.exists(p):
            return p
    return None

queue = [
    os.path.join("/llvm/bin", b)
    for b in ("clang", "clang++", "ld.lld", "lld", "llvm-ar")
    if os.path.isfile(os.path.join("/llvm/bin", b))
]
seen_elfs = set()
missing = []

while queue:
    elf = queue.pop(0)
    if elf in seen_elfs:
        continue
    seen_elfs.add(elf)
    for soname in needed_libs(elf):
        if is_core(soname):
            continue
        dst = os.path.join("/llvm/lib", soname)
        if os.path.exists(dst):
            if not soname.startswith(("libclang", "libLLVM")):
                queue.append(dst)
            continue
        src = find_lib(soname)
        if not src:
            missing.append((elf, soname))
            continue
        shutil.copy2(os.path.realpath(src), dst)
        os.chmod(dst, 0o644)
        print(f"  bundled {soname} <- {src}")
        queue.append(dst)

if missing:
    for elf, soname in missing:
        print(f"ERROR: missing arm64 shared lib {soname} needed by {elf}")
    raise SystemExit(1)
PYEOF

chown -R "$HOST_UID:$HOST_GID" /llvm/lib
DOCKER_ARM64
        ;;
    *)
        echo "Unsupported architecture for bundle_linux_llvm_deps.sh: $ARCH" >&2
        exit 1
        ;;
esac

for bin in "$LLVM_ABS"/bin/*; do
    [[ -f "$bin" && ! -L "$bin" ]] || continue
    if readelf -h "$bin" >/dev/null 2>&1; then
        patchelf --set-rpath '$ORIGIN/../lib' "$bin"
    fi
done

for so in "$LLVM_ABS"/lib/*.so*; do
    [[ -f "$so" && ! -L "$so" ]] || continue
    base="$(basename "$so")"
    case "$base" in
        libclang*.so*|libLLVM*.so*) continue ;;
    esac
    if readelf -h "$so" >/dev/null 2>&1; then
        patchelf --set-rpath '$ORIGIN' "$so"
    fi
done

if [[ "$ARCH" == "$(uname -m)" ]]; then
    env -u LD_LIBRARY_PATH "$LLVM_ABS/bin/clang" --version
    env -u LD_LIBRARY_PATH "$LLVM_ABS/bin/ld.lld" --version
fi
