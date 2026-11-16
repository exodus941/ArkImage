#!/bin/bash
# Builds rlottie as a universal static library in ~/Library/Caches/rlottie/build,
# where build.rs finds it. Needs cmake (brew install cmake). Its NEON path calls
# 32-bit ARM assembly that does not build for arm64, so __ARM_NEON__ is undefined.
set -euo pipefail
D="$HOME/Library/Caches/rlottie"
[ -f "$D/build/librlottie.a" ] && exit 0
[ -d "$D/src" ] || git clone -q --depth 1 https://github.com/Samsung/rlottie.git "$D/src"
cmake -S "$D/src" -B "$D/build" -DCMAKE_BUILD_TYPE=Release -DBUILD_SHARED_LIBS=OFF \
  -DLOTTIE_MODULE=OFF -DLOTTIE_TEST=OFF -DCMAKE_OSX_ARCHITECTURES="arm64;x86_64" \
  -DCMAKE_CXX_FLAGS="-U__ARM_NEON__" \
  -DCMAKE_POLICY_VERSION_MINIMUM=3.5 >/dev/null
cmake --build "$D/build" -j8 --target rlottie
