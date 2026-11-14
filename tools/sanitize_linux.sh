#!/usr/bin/env bash
# Run the AddressSanitizer CI job (.github/workflows/ci.yml, job `sanitize`)
# in a Linux container, from any host with Docker.
#
#     tools/sanitize_linux.sh                      # differential seeds 1..61
#     IRIS_DIFF_SEEDS=1..201 tools/sanitize_linux.sh
#     tools/sanitize_linux.sh corpus               # the .iris corpus as well
#
# The first run builds a local image, iris-sanitize:24.04-2 (Ubuntu 24.04, the
# CI runner's clang, lld and sanitizer runtimes, stable Rust); later runs
# reuse it. The source is mounted read-only and only what the build and the
# tests need is copied in
# (the crate, its workspace members, tests, tools, examples, projects),
# without binaries, so nothing the container builds lands in this checkout.
# Cargo's registry and the Linux target directory live in named volumes and
# are reused between runs. Build jobs are capped (IRIS_SANITIZE_JOBS, default 4).
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SEEDS="${IRIS_DIFF_SEEDS:-1..61}"
JOBS="${IRIS_SANITIZE_JOBS:-4}"
CORPUS="${1:-}"
IMAGE=iris-sanitize:24.04-2

# Git Bash on Windows rewrites /paths in arguments; the container needs them as written.
export MSYS_NO_PATHCONV=1

if ! docker image inspect "$IMAGE" >/dev/null 2>&1; then
  echo "building $IMAGE (once)"
  docker build -t "$IMAGE" - <<'DOCKERFILE'
FROM ubuntu:24.04
ENV DEBIAN_FRONTEND=noninteractive PATH=/root/.cargo/bin:$PATH
RUN apt-get update -yq \
 && apt-get install -yq --no-install-recommends build-essential clang lld curl ca-certificates rsync \
 && rm -rf /var/lib/apt/lists/*
RUN curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain stable
# The sanitizer runtimes (libclang_rt.asan*), which `clang` only recommends.
RUN apt-get update -yq \
 && apt-get install -yq --no-install-recommends libclang-rt-dev \
 && rm -rf /var/lib/apt/lists/*
DOCKERFILE
fi

docker run --rm \
  -v "$ROOT:/src:ro" \
  -v iris-sanitize-registry:/root/.cargo/registry \
  -v iris-sanitize-target:/work/target \
  -e IRIS_SANITIZE=address \
  -e IRIS_CLANG=/usr/bin/clang \
  -e IRIS_NO_INTERP_FALLBACK=1 \
  -e IRIS_DIFF_SEEDS="$SEEDS" \
  -e CARGO_BUILD_JOBS="$JOBS" \
  -e CORPUS="$CORPUS" \
  "$IMAGE" bash -euo pipefail -c '
    mkdir -p /work
    cd /src
    rsync -a --delete \
      --exclude target --exclude node_modules --exclude ".git" \
      --exclude "*.dll" --exclude "*.exe" --exclude "*.lib" --exclude "*.pdb" \
      --exclude "*.vsix" --exclude "*.so" --exclude "*.dylib" \
      Cargo.toml Cargo.lock build.rs LLVM_VERSION src tests tools examples projects crates stdlib benches \
      /work/
    cd /work
    echo "copied $(du -sh /work --exclude=target | cut -f1) of source"
    cargo test --test differential -- --nocapture
    if [ "$CORPUS" = corpus ]; then
      cargo test --test iris_corpus the_two_backends_agree -- --nocapture
    fi
  '
