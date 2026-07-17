# Android 使用指南

Android Agent 会在手机上创建本地 VPN，将游戏流量交给 Agent 处理并自动上传记录。适用于 Android 10 及以上版本。

官网：[https://torchnexus.cc](https://torchnexus.cc)

## 安装前准备

1. 从可信发布渠道取得名为 `torchnexus-agent-arm64-release.apk` 的 APK；安装未知来源应用前，请核对发布者和文件来源。
2. 准备 TorchNexus 用户中心提供的上传用户名和密码。
3. 首次启用时，系统会显示 VPN 连接授权提示；这是 Android 的正常安全机制。

## 配置步骤

1. 安装并打开 TorchNexus Agent。
2. 在“上传接口认证”中填写服务端提供的上传用户名和上传密码。
3. “上传服务器地址”使用默认值 `https://torchnexus.cc/api/v1/app/tcp-batches`；如不确定，请不要改动。
4. 选择“保存设置”。每次修改配置后都应再次保存。
5. 打开“TorchNexus Agent”开关，在系统弹窗中允许 VPN 连接。
6. 看到开关处于开启状态后，启动《火炬之光：无限》并正常游戏。

游戏连接结束后，应用会自动整理并上传记录。登录 [TorchNexus 官网](https://torchnexus.cc) 可查看结果。

## 可选：向同一局域网设备提供代理

Android Agent 还可以为局域网中的其他设备提供 SOCKS5 和 HTTP 代理。通常只需使用本机时，不需要修改此部分。

如需让另一台设备接入：

1. 在“局域网 SOCKS5 代理”中设置端口、用户名和密码；建议始终设置随机且唯一的密码。
2. 应用会显示“外部设备请配置”的局域网地址；在另一台设备中填写该地址、端口及相同凭据。
3. 只在可信局域网启用此功能，且不要把代理端口暴露到公网。

HTTP 代理仅用于需要 HTTP 代理的局域网设备；它不是 Android 自身使用 VPN 的必要设置。

## 使用 Windows CLI 的 SOCKS5 代理

如果你希望 Android 游戏流量由 Windows CLI 处理，而不是在手机运行 Android Agent，请使用支持 SOCKS5 **全局代理**或 TUN 模式的 Android 代理客户端。

1. 在 Windows 电脑上按[桌面 Agent 指南](../README.md#桌面-agent)启动 CLI，并按要求添加 hosts 映射。
2. 确认 Android 手机与电脑处于同一个 Wi-Fi；在电脑执行 `ipconfig`，记下当前网卡的 IPv4 地址，例如 `192.168.1.23`。
3. 在 Android 代理客户端中新增 SOCKS5 节点：服务器填写电脑 IPv4 地址，端口填写 Windows CLI 的 SOCKS5 端口（默认 `1080`），用户名和密码填写 CLI 配置中的 SOCKS5 凭据。
4. 将代理客户端切换为“全局代理”或 TUN 模式，并在系统弹窗中允许 VPN 连接。
5. 保持代理连接，再启动游戏。电脑 CLI 出现连接日志并产生记录，即表示已接入。

Android 系统 Wi-Fi 的“手动代理”通常只支持 HTTP 代理，不能直接填写 SOCKS5。因此这里必须使用支持 SOCKS5 的代理客户端，而不是 Android 的 Wi-Fi 手动代理设置。

## 使用检查与排错

- 无法打开 VPN：确认已保存完整的上传账号和密码，然后重新授权 VPN；如设备有其他 VPN，请先断开或按系统提示处理冲突。
- 官网暂未显示记录：保持网络连接，等待游戏连接结束后再刷新；确认上传账号、密码和服务器地址由服务端提供且填写无误。
- 无法让其他设备接入：确认两台设备处于同一局域网、未开启客户端隔离，并核对显示的地址、端口与认证信息。
- 请勿关闭 Agent 的 VPN 后再启动游戏；关闭后流量不会经过 Agent。
