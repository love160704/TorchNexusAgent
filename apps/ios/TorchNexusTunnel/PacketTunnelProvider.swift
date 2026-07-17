import NetworkExtension
import TorchNexusMobile

final class PacketTunnelProvider: NEPacketTunnelProvider {
    static let configurationKey = "torchnexusConfigYaml"
    private let engine = MobileEngine()

    override func startTunnel(options: [String : NSObject]?) async throws {
        guard let fileDescriptor = packetFlow.value(forKeyPath: "socket.fileDescriptor") as? Int32 else {
            throw NSError(domain: "TorchNexusAgent", code: 1, userInfo: [NSLocalizedDescriptionKey: "Packet Tunnel file descriptor is unavailable"])
        }
        _ = try engine.start(tunFd: fileDescriptor, configYaml: configuredYaml(), mtu: 1500)
    }

    override func stopTunnel(with reason: NEProviderStopReason) async {
        try? engine.stop()
    }

    private func defaultConfig() -> String {
        """
        listen:
          socks5: { enabled: false, bind: "127.0.0.1:0" }
          http: { enabled: false, bind: "127.0.0.1:0" }
          tcp: []
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

    private func configuredYaml() -> String {
        guard
            let tunnelProtocol = protocolConfiguration as? NETunnelProviderProtocol,
            let yaml = tunnelProtocol.providerConfiguration?[Self.configurationKey] as? String,
            !yaml.isEmpty
        else {
            return defaultConfig()
        }
        return yaml
    }
}
