#!/usr/bin/env bash
# fetch_vendor.sh — Download vendored dependencies required to build Numenor.
#
# The vendor/ directory is excluded from the repository (see .gitignore) because
# llama.cpp and the LLVM libc++ headers together exceed 300 MB. Run this script
# once after cloning to fetch them.
#
# Usage: ./tools/fetch_vendor.sh

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
VENDOR_DIR="${REPO_ROOT}/vendor"

echo "[aeglos] Fetching vendor dependencies into ${VENDOR_DIR}/"
mkdir -p "${VENDOR_DIR}"

# ── llama.cpp ──────────────────────────────────────────────────────────────
LLAMA_DIR="${VENDOR_DIR}/llama.cpp"
if [[ -d "${LLAMA_DIR}/.git" ]]; then
    echo "[aeglos] llama.cpp already present — skipping."
else
    echo "[aeglos] Cloning llama.cpp (depth=1)..."
    git clone --depth 1 https://github.com/ggml-org/llama.cpp "${LLAMA_DIR}"
    echo "[aeglos] llama.cpp fetched."
fi

# ── LLVM libc++ headers ────────────────────────────────────────────────────
# Only the libc++ include tree is required; the full LLVM project (~2 GB) is
# not needed. A sparse checkout retrieves only libcxx/include.
LLVM_DIR="${VENDOR_DIR}/llvm-project"
if [[ -d "${LLVM_DIR}/.git" ]]; then
    echo "[aeglos] llvm-project already present — skipping."
else
    echo "[aeglos] Cloning llvm-project (sparse: libcxx/include only)..."
    git clone \
        --depth 1 \
        --filter=blob:none \
        --sparse \
        https://github.com/llvm/llvm-project "${LLVM_DIR}"
    cd "${LLVM_DIR}"
    git sparse-checkout set libcxx/include
    cd "${REPO_ROOT}"
    echo "[aeglos] llvm-project libc++ headers fetched."
fi

echo ""
echo "[aeglos] Vendor dependencies ready. You can now run:"
echo "         make build"
echo "         ./tools/build.sh build"
