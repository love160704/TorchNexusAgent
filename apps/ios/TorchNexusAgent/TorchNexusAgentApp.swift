import NetworkExtension
import SwiftUI

@main
struct TorchNexusAgentApp: App {
    var body: some Scene {
        WindowGroup { ContentView() }
    }
}

struct ContentView: View {
    @AppStorage("socksPort") private var socksPort = "1080"
    @AppStorage("socksUsername") private var socksUsername = ""
    @AppStorage("socksPassword") private var socksPassword = ""
    @AppStorage("httpEnabled") private var httpEnabled = true
    @AppStorage("httpPort") private var httpPort = "1081"
    @AppStorage("httpUsername") private var httpUsername = ""
    @AppStorage("httpPassword") private var httpPassword = ""
    @StateObject private var vpn = VPNController()

    var body: some View {
        NavigationStack {
            Form {
                Section("局域网 SOCKS5 代理") {
                    TextField("端口", text: $socksPort).keyboardType(.numberPad)
                    TextField("用户名（留空则不认证）", text: $socksUsername)
                    SecureField("密码", text: $socksPassword)
                }

                Section("局域网 HTTP 代理") {
                    Toggle("启用 HTTP 代理", isOn: $httpEnabled)
                    if httpEnabled {
                        TextField("端口", text: $httpPort).keyboardType(.numberPad)
                        TextField("用户名（留空则不认证）", text: $httpUsername)
                        SecureField("密码", text: $httpPassword)
                    }
                }

                Section {
                    Button(vpn.isRunning ? "停止 VPN" : "保存设置并启用 VPN") {
                        if vpn.isRunning {
                            vpn.stop()
                        } else {
                            startVpn()
                        }
                    }
                    .disabled(vpn.isWorking)
                } footer: {
                    Text("HTTP 代理供局域网其他设备使用；VPN 的 TUN 流量由 Rust 引擎创建的私有 loopback SOCKS5 转发。")
                }
            }
            .navigationTitle("TorchNexus Agent")
            .alert("无法更新 VPN", isPresented: Binding(
                get: { vpn.errorMessage != nil },
                set: { if !$0 { vpn.errorMessage = nil } }
            )) {
                Button("确定", role: .cancel) { vpn.errorMessage = nil }
            } message: {
                Text(vpn.errorMessage ?? "")
            }
        }
    }

    private func configurationYaml() -> String {
        let socksAuth = proxyAuthYaml(username: socksUsername, password: socksPassword)
        let httpAuth = proxyAuthYaml(username: httpUsername, password: httpPassword)
        return """
        listen:
          socks5:
            enabled: true
            bind: "0.0.0.0:\(validatedPort(socksPort, fallback: 1080))"
        \(socksAuth)  http:
            enabled: \(httpEnabled)
            bind: "0.0.0.0:\(validatedPort(httpPort, fallback: 1081))"
        \(httpAuth)  tcp: []
        capture:
          targets:
            - ip: "60.205.202.26"
              ports: [1002]
          save_dir: "./captures"
          save_uncaptured_sessions: false
        upload:
          enabled: false
          endpoint: "https://torchnexus.cc/api/v1/app/tcp-batches"
          basic_auth: { username: "", password: "" }
          auto_package_on_disconnect: true
          upload_interval_seconds: 60
          retry: { max_attempts: 5, base_delay_seconds: 3 }
        storage: { flush_each_chunk: true }
        log: { level: "info" }
        """
    }

    private func validatedPort(_ value: String, fallback: Int) -> Int {
        guard let port = Int(value), (1...65535).contains(port) else { return fallback }
        return port
    }

    private func startVpn() {
        guard validCredentials(username: socksUsername, password: socksPassword, label: "SOCKS5") else { return }
        guard !httpEnabled || validCredentials(username: httpUsername, password: httpPassword, label: "HTTP 代理") else { return }
        vpn.start(configurationYaml: configurationYaml())
    }

    private func validCredentials(username: String, password: String, label: String) -> Bool {
        guard username.isEmpty == password.isEmpty else {
            vpn.errorMessage = "\(label) 用户名和密码必须同时填写。"
            return false
        }
        return true
    }

    private func proxyAuthYaml(username: String, password: String) -> String {
        guard !username.isEmpty || !password.isEmpty else { return "" }
        return "    auth:\n      username: \(yamlString(username))\n      password: \(yamlString(password))\n"
    }

    private func yamlString(_ value: String) -> String {
        "\"\(value.replacingOccurrences(of: "\\", with: "\\\\").replacingOccurrences(of: "\"", with: "\\\"").replacingOccurrences(of: "\n", with: ""))\""
    }
}

@MainActor
final class VPNController: ObservableObject {
    @Published private(set) var isRunning = false
    @Published private(set) var isWorking = false
    @Published var errorMessage: String?

    private var manager: NETunnelProviderManager?

    func start(configurationYaml: String) {
        isWorking = true
        Task {
            do {
                let manager = try await loadManager()
                let tunnelProtocol = NETunnelProviderProtocol()
                tunnelProtocol.providerBundleIdentifier = "com.torchnexus.agent.tunnel"
                tunnelProtocol.serverAddress = "TorchNexus Agent"
                tunnelProtocol.providerConfiguration = ["torchnexusConfigYaml": configurationYaml]
                manager.protocolConfiguration = tunnelProtocol
                manager.isEnabled = true
                try await save(manager)
                try await reload(manager)
                try manager.connection.startVPNTunnel()
                self.manager = manager
                isRunning = true
            } catch {
                errorMessage = error.localizedDescription
            }
            isWorking = false
        }
    }

    func stop() {
        isWorking = true
        Task {
            do {
                let manager = self.manager ?? (try await loadManager())
                manager.connection.stopVPNTunnel()
                self.manager = manager
                isRunning = false
            } catch {
                errorMessage = error.localizedDescription
            }
            isWorking = false
        }
    }

    private func loadManager() async throws -> NETunnelProviderManager {
        let managers = try await withCheckedThrowingContinuation { continuation in
            NETunnelProviderManager.loadAllFromPreferences { managers, error in
                if let error { continuation.resume(throwing: error) }
                else { continuation.resume(returning: managers ?? []) }
            }
        }
        return managers.first ?? NETunnelProviderManager()
    }

    private func save(_ manager: NETunnelProviderManager) async throws {
        try await withCheckedThrowingContinuation { continuation in
            manager.saveToPreferences { error in
                if let error { continuation.resume(throwing: error) }
                else { continuation.resume() }
            }
        }
    }

    private func reload(_ manager: NETunnelProviderManager) async throws {
        try await withCheckedThrowingContinuation { continuation in
            manager.loadFromPreferences { error in
                if let error { continuation.resume(throwing: error) }
                else { continuation.resume() }
            }
        }
    }
}
