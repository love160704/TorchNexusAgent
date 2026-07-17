param(
    [ValidateSet('arm64-v8a', 'armeabi-v7a', 'x86_64')]
    [string]$Target = 'arm64-v8a',
    [ValidateSet('debug', 'release')]
    [string]$BuildType = 'debug',
    [switch]$AssembleApk
)

$ErrorActionPreference = 'Stop'
if (-not $env:ANDROID_HOME) { throw 'ANDROID_HOME must point to the Android SDK.' }
if (-not $env:ANDROID_NDK_HOME) {
    $ndkRoot = Join-Path $env:ANDROID_HOME 'ndk'
    $env:ANDROID_NDK_HOME = Get-ChildItem $ndkRoot -Directory |
        Sort-Object Name -Descending |
        Select-Object -First 1 -ExpandProperty FullName
}
if (-not $env:ANDROID_NDK_HOME) { throw 'ANDROID_NDK_HOME is not set and no NDK was found under ANDROID_HOME\ndk.' }
if (-not (Get-Command uniffi-bindgen -ErrorAction SilentlyContinue)) {
    throw 'Install uniffi-bindgen 0.32.0 before building bindings.'
}

$rustTarget = @{ 'arm64-v8a' = 'aarch64-linux-android'; 'armeabi-v7a' = 'armv7-linux-androideabi'; 'x86_64' = 'x86_64-linux-android' }[$Target]
$ndkBin = Join-Path $env:ANDROID_NDK_HOME 'toolchains\llvm\prebuilt\windows-x86_64\bin'
$llvmAr = Join-Path $ndkBin 'llvm-ar.exe'
if (-not (Test-Path $llvmAr)) { throw "NDK llvm-ar was not found: $llvmAr" }
$cargoTarget = $rustTarget.ToUpperInvariant().Replace('-', '_')
Set-Item -Path ("Env:CARGO_TARGET_{0}_AR" -f $cargoTarget) -Value $llvmAr
Set-Item -Path ("Env:AR_{0}" -f $rustTarget) -Value $llvmAr
Set-Item -Path ("Env:AR_{0}" -f $rustTarget.Replace('-', '_')) -Value $llvmAr

cargo ndk -t $Target -o apps/android/app/src/main/jniLibs build -p torchnexus-mobile-engine --release
$tun2proxyLibrary = Join-Path "apps/android/app/src/main/jniLibs/$Target" 'libtun2proxy.so'
if (Test-Path $tun2proxyLibrary) {
    Remove-Item -LiteralPath $tun2proxyLibrary -Force
}
uniffi-bindgen generate target/$rustTarget/release/libtorchnexus_mobile_engine.so --language kotlin --out-dir apps/android/app/src/main/java --metadata-no-deps --no-format
if ($AssembleApk) {
    $variant = $BuildType.Substring(0, 1).ToUpperInvariant() + $BuildType.Substring(1)
    Push-Location apps/android
    try { .\gradlew.bat ":app:assemble$variant" --no-daemon }
    finally { Pop-Location }
    if ($BuildType -eq 'release') {
        $releaseApk = 'apps/android/app/build/outputs/apk/release/app-release.apk'
        $renamedApk = 'apps/android/app/build/outputs/apk/release/torchnexus-agent-arm64-release.apk'
        if (-not (Test-Path $releaseApk)) { throw "Release APK was not found: $releaseApk" }
        Move-Item -LiteralPath $releaseApk -Destination $renamedApk -Force
    }
}
