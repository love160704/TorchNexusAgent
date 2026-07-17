# TorchNexus Agent — 协作指南

## 基本约定

- 所有面向用户的文档、代码注释和协作回复使用中文。
- 修改前先确认目标平台与数据流：桌面 CLI、Android VPN、iOS Tunnel/外部 SOCKS5 客户端三者的配置并不完全相同。
- 不提交真实账号、密码、签名证书、Apple ID、钱包信息或其他密钥；本地配置使用 `config.local.yaml`，不要覆盖 `config.example.yaml` 中的示例值。
- 提交信息使用中文，建议格式：`<类型>(<范围>): <描述>`，例如 `docs(ios): 补充 SOCKS5 配置说明`。

## 项目概览

这是一个 Rust workspace，用于转发《火炬之光：无限》游戏连接、采集会话数据并上传至 TorchNexus 服务端。主要交付物包括：

- Windows 桌面 CLI：`torchnexus-agent`。
- Android 应用：在设备上创建 VPN，并可向局域网提供 SOCKS5/HTTP 代理。
- iOS 应用与 Packet Tunnel；用户也可通过 Shadowrocket 接入 Windows 或 Android Agent 的 SOCKS5 代理。

官方服务端上传地址为：`https://torchnexus.cc/api/v1/app/tcp-batches`。

## 目录职责

```text
crates/
  app-cli/          桌面 CLI 入口与命令行子命令
  core/             公共领域模型与配置基础
  proxy-tcp/        游戏 TCP 转发与采集
  proxy-socks5/     SOCKS5 代理
  proxy-http/       HTTP CONNECT 代理
  storage-support/  本地会话与归档存储
  uploader/         自动打包和上传重试
  runtime/          运行时组装
  mobile-engine/    Android/iOS 共用的 UniFFI 移动引擎
apps/
  android/          Android Gradle 工程与 Kotlin 宿主
  ios/              iOS/XcodeGen 工程与 Packet Tunnel 扩展
docs/               面向用户的 Android、iOS 配置说明
scripts/mobile/     Android 与 iOS 构建脚本
config.example.yaml 桌面 CLI 配置模板
README.md           产品总览、桌面使用与打包说明
```

## 常用命令

```bash
# Rust 格式检查、静态检查与测试
cargo fmt --all -- --check
cargo check --workspace
cargo test --workspace

# Windows CLI 发布构建（Windows 环境）
cargo build --release -p torchnexus-agent --target x86_64-pc-windows-msvc

# Android APK（Windows）
.\scripts\mobile\build-android.ps1 -Target arm64-v8a -BuildType debug -AssembleApk

# iOS 移动引擎（macOS）
bash scripts/mobile/build-ios.sh
```

Android 构建依赖 Android SDK、NDK、Java 21、Rust、`cargo-ndk 2.6.0` 与 `uniffi-bindgen 0.32.0`。iOS 构建还需要 macOS、Xcode、XcodeGen 和具有 Packet Tunnel 权限的有效签名团队。

## 修改规则

### 代理与上传

- SOCKS5/HTTP 监听在局域网使用时必须支持认证；不要引导用户将代理端口暴露到公网。
- 变更上传配置时，保持 Android 默认地址、iOS 主应用和 iOS Tunnel 扩展使用相同的官方 HTTPS 地址。
- 变更会话生命周期、断线打包或上传重试行为时，优先补充对应 crate 的测试。

### 移动端

- Android 修改 Kotlin 与 Rust FFI 边界时，同时检查生成绑定和 `apps/android/app/src/main/jniLibs` 的构建流程。
- iOS 修改主应用与 `TorchNexusTunnel` 之间的配置传递时，同时检查两个 target 的签名、Bundle ID 与 Packet Tunnel 能力。
- 用户文档中的 iOS 路径以“Shadowrocket 通过 SOCKS5 接入上游 Agent”为准；不建议使用 iOS Wi-Fi 手动 HTTP 代理承载游戏流量。

### 文档

- 用户配置指南只放在 `docs/android.md` 与 `docs/ios.md`；`apps/` 目录保留工程文件。
- 改动 CLI 参数、默认端口、上传地址、设备配置方式或打包产物位置时，必须同步更新 `README.md` 和相应的 `docs/` 指南。
- 文档不要包含接口细节、实现代码、真实凭据或共享 Apple ID 的操作方式。

## 验证要求

- Rust 逻辑修改：至少运行受影响 crate 的测试；跨 crate 修改运行 `cargo test --workspace`。
- Android/iOS 修改：在具备相应工具链时执行对应构建；若环境不具备，应明确说明未执行的验证。
- 仅文档修改：检查所有本地 Markdown 链接与命令、端口、默认上传地址是否一致。
