#!/usr/bin/env bash
set -euo pipefail

command -v uniffi-bindgen >/dev/null || { echo 'Install uniffi-bindgen 0.32.0 first.'; exit 1; }
for target in aarch64-apple-ios aarch64-apple-ios-sim; do
  cargo build -p torchnexus-mobile-engine --release --target "$target"
done
uniffi-bindgen generate target/aarch64-apple-ios/release/libtorchnexus_mobile_engine.a --language swift --out-dir apps/ios/Generated
xcodebuild -create-xcframework \
  -library target/aarch64-apple-ios/release/libtorchnexus_mobile_engine.a \
  -library target/aarch64-apple-ios-sim/release/libtorchnexus_mobile_engine.a \
  -output apps/ios/Generated/TorchNexusMobile.xcframework
