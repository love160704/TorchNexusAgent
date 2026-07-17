#!/usr/bin/env bash
set -euo pipefail

for target in aarch64-apple-ios aarch64-apple-ios-sim; do
  cargo build -p torchnexus-mobile-engine --release --target "$target"
done
cargo run --locked -p torchnexus-core --features uniffi-bindgen --bin uniffi-bindgen -- generate target/aarch64-apple-ios/release/libtorchnexus_mobile_engine.a --language swift --out-dir apps/ios/Generated
xcodebuild -create-xcframework \
  -library target/aarch64-apple-ios/release/libtorchnexus_mobile_engine.a \
  -library target/aarch64-apple-ios-sim/release/libtorchnexus_mobile_engine.a \
  -output apps/ios/Generated/TorchNexusMobile.xcframework
