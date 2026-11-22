#!/usr/bin/env bash
# Fetches the pinned libjxl sources and only the nested submodules the build needs
# (highway, brotli, skcms), all shallow.
set -euo pipefail
cd "$(dirname "$0")/.."
git submodule update --init --depth 1 third_party/libjxl
git -C third_party/libjxl submodule update --init --depth 1 \
  third_party/highway third_party/brotli third_party/skcms
