# 火炬之光无限收益统计器｜TorchNexus Agent

TorchNexus 是《火炬之光：无限》收益助手。它通过 Agent 自动整理跑图记录、掉落与消耗，并在官网按赛季、角色和区域汇总收益数据。

官网与用户中心：[TorchNexus 火炬之光无限收益统计器](https://torchnexus.cc)

## 收益统计能力

### 自动记录每一次跑图

Agent 会在游戏连接结束后自动整理本局地图记录、拾取列表和消耗列表并上传。无需手动导出文件，刷图过程更容易回顾。

### 清晰统计收益、成本与效率

为掉落和消耗补充初火估值后，系统会汇总总收益、总成本、总净收益、刷图次数、总耗时与时均收益，并标出尚待估值的项目。

### 按赛季、角色和区域回溯

在官网选择赛季与角色，可查询历史地图记录、单局时长和收益明细；区域表现会按净收益汇总，帮助比较不同地图的刷图结果。

### 管理掉落与消耗明细

打开单局跑图详情即可分别查看拾取与消耗。未估值物品可手动设置单价，随后相关收益统计会同步更新。

## 使用流程

1. 按下方设备指南配置 Windows、Android 或 iOS Agent。
2. 启动 Agent 后正常进入游戏并完成跑图。
3. 游戏连接结束后，等待记录自动上传至 TorchNexus。
4. 在官网查看收益、成本与历史记录；对待估值物品补充价格后获得更完整的净收益统计。

## 选择你的设备与配置方式

| 设备 | 推荐方式 | 指南 |
| --- | --- | --- |
| Android 手机 | 安装 Android Agent，在手机上启用 VPN | [Android 使用指南](docs/android.md) |
| iPhone / iPad | 通过 Shadowrocket 的 SOCKS5 全局代理接入桌面或 Android Agent | [iOS 使用指南](docs/ios.md) |
| Windows 电脑 | 运行桌面 Agent，供手机接入或用于本机转发 | 见下文“桌面 Agent” |

> 请只对自己拥有或获授权使用的游戏账号和设备操作，并妥善保管上传账号、代理账号与支付信息。

## 桌面 Agent

### 使用前准备

1. 下载并解压 `torchnexus-agent` 到固定文件夹。
2. 确保电脑与要接入的手机使用同一个局域网；不要开启 Wi-Fi 的 AP/客户端隔离。
3. 准备 TorchNexus 用户中心提供的上传地址、用户名和密码。
4. 允许系统防火墙放行代理端口：SOCKS5 默认 `1080`，HTTP 默认 `1081`。

### 配置与启动

1. 将 `config.example.yaml` 复制为 `config.local.yaml`。
2. 在 `config.local.yaml` 中填写服务端提供的上传地址、用户名和密码；为 SOCKS5 设置仅自己知道的用户名和密码。
3. 保持默认监听地址 `0.0.0.0`，让局域网中的手机可以连接；如改动端口，稍后也要在手机上填写相同端口。
4. 在配置文件所在目录执行：

   ```powershell
   .\torchnexus-agent.exe check-config --config config.local.yaml
   ```

5. 校验成功后启动：

   ```powershell
   .\torchnexus-agent.exe run --config config.local.yaml
   ```

6. 保持窗口运行，再按设备指南配置手机并启动游戏。连接断开后，Agent 会自动整理并上传记录。

### Windows CLI 必要的 hosts 映射

为让游戏连接进入本机 CLI，请按以下顺序操作：

1. 先完成上面的配置校验并启动 CLI；确认 Agent 窗口保持运行。
2. 以“管理员身份”打开记事本，打开文件 `C:\Windows\System32\drivers\etc\hosts`。如看不到该文件，在打开窗口中将文件类型切换为“所有文件”。
3. 在文件末尾新增一行并保存：

   ```text
   127.0.0.1 torchlight-gateway2.xdgtw.cm
   ```

4. 重新启动游戏，使它重新解析域名并连接到本机 CLI。

> 不使用 CLI 时，请删除这行 hosts 映射并保存，以恢复游戏的正常网络连接。只修改这一条指定记录，不要替换 hosts 文件中已有的其他内容。

### 获取电脑局域网地址

在 Windows PowerShell 执行 `ipconfig`，记录当前 Wi-Fi 或有线网卡的“IPv4 地址”，例如 `192.168.1.23`。手机代理服务器应填写这个地址，不能填写 `127.0.0.1`。

### 确认是否正常工作

游戏启动后，Agent 窗口应出现连接日志，`captures` 目录会产生记录。游戏断开后数据将自动上传；随后在 [官网](https://torchnexus.cc) 登录并查看相关统计。

若无法连接，依次检查：电脑和手机是否在同一局域网、手机是否填写了电脑的 IPv4 地址、代理用户名/密码和端口是否一致、Agent 是否仍在运行，以及防火墙是否已放行端口。

## 使用 Epusdt 充值与购买套餐

1. 登录 [TorchNexus 官网](https://torchnexus.cc)，打开“余额充值”。
2. 输入要充值的金额并选择“前往支付”，浏览器会跳转到 Epusdt 收银台。
3. 在收银台选择可用的网络与代币，并使用自己的钱包完成支付。
4. **务必以收银台显示的网络、代币、收款地址和精确金额为准。** 转账前逐项核对；不要跨链、不要少付，也不要向旧订单地址再次付款。
5. 支付完成后返回网站，在“余额充值”的待处理记录中等待状态变为“已到账”。到账通常需要链上确认，请勿因等待而重复创建订单。
6. 打开“套餐权益”，选择套餐并用余额完成购买；权益生效后即可使用。

如支付已完成但长时间未到账，请保留交易哈希、支付时间、订单金额与付款网络，联系服务支持核对。不要向任何人提供钱包助记词、私钥或交易所登录信息。

## 本地自行打包发布

本节面向需要自行制作安装包的发布者，不涉及业务代码。请仅分发经自己签名、可追溯的构建产物。

### Windows 桌面 Agent

准备 Rust 稳定版工具链和 Windows C++ 构建工具后，在项目根目录执行：

```powershell
cargo build --release -p torchnexus-agent --target x86_64-pc-windows-msvc
```

产物为 `target\x86_64-pc-windows-msvc\release\torchnexus-agent.exe`。发布前将它与 `config.example.yaml` 一同放入新文件夹，在另一台 Windows 设备上运行 `check-config` 完成验收。

### Android APK

需要 Android SDK、NDK、Java 21、Rust 稳定版与 `cargo-ndk 4.1.2`。UniFFI 绑定生成器通过 core crate 的本地可选二进制自动编译，无需单独安装。设置 `ANDROID_HOME`（并确保可找到 NDK）后，在 Windows 运行：

```powershell
.\scripts\mobile\build-android.ps1 -Target arm64-v8a -BuildType release -AssembleApk
```

正式发布还需预先配置自己的 Android 签名证书与签名信息；未配置签名时，请只生成 Debug APK 用于内部测试。Release 产物路径为 `apps\android\app\build\outputs\apk\release\torchnexus-agent-arm64-release.apk`。安装到真机后，至少完成一次保存配置、授权 VPN、启动和停止的验收。

更多 Android 安装与使用说明见 [Android 使用指南](docs/android.md)。

### iOS 应用

iOS 打包只能在 macOS 上完成，并且需要有效的 Apple 开发者签名能力及 Packet Tunnel 权限。准备 Xcode、XcodeGen 与 Rust 稳定版后：

```bash
bash scripts/mobile/build-ios.sh
cd apps/ios
xcodegen generate
```

随后用 Xcode 打开生成的 `TorchNexusAgent.xcodeproj`，选择自己的签名团队，为主应用和 Tunnel 扩展配置匹配的签名，再连接真机运行并确认 VPN 可以启停。请勿使用他人的开发者证书或账号签名发布。

## 常见问题

**为什么官网没有新记录？**

确认 Agent 已运行、游戏流量已通过代理或 VPN、上传凭据正确，随后等待游戏连接断开和服务端处理完成。

**能否手动上传某一局？**

不能。Agent 会在连接断开后自动整理，并按配置周期自动重试上传。

**是否可以将代理直接暴露到互联网？**

不建议。仅在受信任局域网中使用，并设置高强度代理密码；不要将 `1080`、`1081` 端口映射到公网。
