#!/usr/bin/env bash
set -euo pipefail

target="arm64-v8a"
assemble_apk=false
build_type="debug"

usage() {
  echo "Usage: $0 [--target arm64-v8a|armeabi-v7a|x86_64] [--build-type debug|release] [--assemble-apk]" >&2
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --target)
      target="${2:?missing value for --target}"
      shift 2
      ;;
    --build-type)
      build_type="${2:?missing value for --build-type}"
      shift 2
      ;;
    --assemble-apk)
      assemble_apk=true
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      usage
      exit 2
      ;;
  esac
done

case "$target" in
  arm64-v8a) rust_target="aarch64-linux-android" ;;
  armeabi-v7a) rust_target="armv7-linux-androideabi" ;;
  x86_64) rust_target="x86_64-linux-android" ;;
  *) echo "Unsupported Android ABI: $target" >&2; exit 2 ;;
esac

case "$build_type" in
  debug) assemble_task="assembleDebug" ;;
  release) assemble_task="assembleRelease" ;;
  *) echo "Unsupported Android build type: $build_type" >&2; exit 2 ;;
esac

android_home="${ANDROID_HOME:-${ANDROID_SDK_ROOT:-}}"
if [[ -z "$android_home" ]]; then
  echo "ANDROID_HOME or ANDROID_SDK_ROOT must point to the Android SDK." >&2
  exit 1
fi

if [[ -z "${ANDROID_NDK_HOME:-}" ]]; then
  ndk_root="$android_home/ndk"
  [[ -d "$ndk_root" ]] || { echo "No NDK directory found under $ndk_root" >&2; exit 1; }
  ANDROID_NDK_HOME="$(find "$ndk_root" -mindepth 1 -maxdepth 1 -type d -printf '%f\n' | sort -V | tail -n 1)"
  ANDROID_NDK_HOME="$ndk_root/$ANDROID_NDK_HOME"
  export ANDROID_NDK_HOME
fi

ndk_bin="$ANDROID_NDK_HOME/toolchains/llvm/prebuilt/linux-x86_64/bin"
llvm_ar="$ndk_bin/llvm-ar"
[[ -x "$llvm_ar" ]] || { echo "NDK llvm-ar was not found: $llvm_ar" >&2; exit 1; }
command -v cargo-ndk >/dev/null || { echo "Install cargo-ndk before building." >&2; exit 1; }
command -v uniffi-bindgen >/dev/null || { echo "Install uniffi-bindgen 0.32.0 before generating bindings." >&2; exit 1; }

rustup target add "$rust_target"
cargo_target="${rust_target^^}"
cargo_target="${cargo_target//-/_}"
export "CARGO_TARGET_${cargo_target}_AR=$llvm_ar"

cargo ndk -t "$target" -o apps/android/app/src/main/jniLibs build -p torchnexus-mobile-engine --release
uniffi-bindgen generate "target/$rust_target/release/libtorchnexus_mobile_engine.so" \
  --language kotlin --out-dir apps/android/app/src/main/java --metadata-no-deps --no-format

if [[ "$assemble_apk" == true ]]; then
  (cd apps/android && bash ./gradlew ":app:$assemble_task" --no-daemon)
  if [[ "$build_type" == release ]]; then
    release_apk="apps/android/app/build/outputs/apk/release/app-release.apk"
    renamed_apk="apps/android/app/build/outputs/apk/release/torchnexus-agent-arm64-release.apk"
    [[ -f "$release_apk" ]] || { echo "Release APK was not found: $release_apk" >&2; exit 1; }
    mv -f "$release_apk" "$renamed_apk"
  fi
fi
