# iOS 使用指南

iOS 的系统 Wi-Fi“手动代理”无法保证游戏流量会经过 Agent，因此不建议使用它。推荐在电脑运行桌面 Agent，或在另一台 Android 设备运行 Android Agent；再使用 Shadowrocket 的 SOCKS5 全局代理将 iPhone 或 iPad 的游戏流量转发到该 Agent 设备。

官网：[https://torchnexus.cc](https://torchnexus.cc)

## 准备工作

1. 选择一台作为上游的 Agent 设备：
   - **Windows 电脑**：按[桌面 Agent 指南](../README.md#桌面-agent)完成配置并启动 CLI，并按该指南添加 hosts 映射。
   - **另一台 Android 设备**：安装 Android Agent，按[Android 使用指南](android.md)启用“局域网 SOCKS5 代理”，并记下应用显示的局域网地址和端口。
2. 确认 iPhone/iPad 与上游 Agent 设备连接同一个 Wi-Fi，且路由器没有开启 AP/客户端隔离。
3. 若上游是 Windows 电脑，在电脑执行 `ipconfig`，记录当前网卡的 IPv4 地址，例如 `192.168.1.23`。
4. 在 iOS 设备上自行合法取得并安装 Shadowrocket。可参考以下可选信息来源，但请自行核验其安全性、地区可用性与 Apple 账号授权状态：

   - [https://free.iosapp.icu/](https://free.iosapp.icu/)
   - [https://idfree.top/](https://idfree.top/)

> 不要共享 Apple ID、密码、双重认证验证码或任何支付信息。文档不提供共享账号或绕过应用商店获取应用的步骤；请使用你有权使用的账号和应用副本。

## 在 Shadowrocket 中添加 Agent 代理

1. 打开 Shadowrocket，新增一个 **SOCKS5** 节点。
2. 服务器填写上游 Agent 设备的局域网 IPv4 地址；Windows 示例为 `192.168.1.23`，Android 使用 Agent 应用显示的地址。
3. 端口填写上游 Agent 的 SOCKS5 端口，默认是 `1080`。
4. 用户名和密码填写上游 Agent 配置中的 SOCKS5 凭据。
5. 保存节点并选中它。
6. 将代理模式设为“全局代理”（或等效的全局/TUN 模式），然后启用连接；在 iOS 的系统弹窗中允许 VPN 配置。
7. 保持 Shadowrocket 连接状态，再启动《火炬之光：无限》。

## 确认连接

游戏启动后，上游 Agent 应出现连接日志或采集记录。游戏连接断开后，数据会自动上传；随后可在 [官网](https://torchnexus.cc) 登录查看。

若未产生记录：

1. 再次确认 Shadowrocket 选中的是刚创建的 SOCKS5 节点，并已真正连接。
2. 确认使用的是“全局代理”或可覆盖游戏流量的 TUN 模式，而非只代理网页流量的规则模式。
3. 核对上游设备的局域网地址、端口、用户名和密码；Windows 电脑 IP 在切换 Wi-Fi 或重启路由器后可能变化。
4. 确认上游 Agent 未退出，且上游设备已允许 SOCKS5 端口的局域网连接。
5. 检查 iPhone/iPad 和上游设备是否仍在同一局域网，且没有 AP/客户端隔离。

## 安全提示

- 只连接自己或可信网络中的 Agent，不要使用陌生人提供的代理节点。
- 不要在公共 Wi-Fi 中暴露无密码的 SOCKS5 服务。
- 如不再使用，关闭 Shadowrocket 的连接和上游 Agent，避免其他流量继续通过该代理。
