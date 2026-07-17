package com.torchnexus.agent

import android.content.Context
import androidx.security.crypto.EncryptedSharedPreferences
import androidx.security.crypto.MasterKey
import java.net.Inet4Address
import java.net.NetworkInterface

data class AgentSettings(
    val socksPort: String = "1080",
    val socksUsername: String = "",
    val socksPassword: String = "",
    val httpEnabled: Boolean = true,
    val httpPort: String = "1081",
    val httpUsername: String = "",
    val httpPassword: String = "",
    val uploadEndpoint: String = "https://torchnexus.cc/api/v1/app/tcp-batches",
    val uploadUsername: String = "",
    val uploadPassword: String = "",
) {
    fun validatedPort(): Int = socksPort.toIntOrNull()?.takeIf { it in 1..65535 }
        ?: throw IllegalArgumentException("SOCKS5 端口必须在 1 到 65535 之间")

    fun validatedHttpPort(): Int = httpPort.toIntOrNull()?.takeIf { it in 1..65535 }
        ?: throw IllegalArgumentException("HTTP 代理端口必须在 1 到 65535 之间")

    fun toYaml(filesDir: String): String {
        val socksPort = validatedPort()
        val httpPort = if (httpEnabled) validatedHttpPort() else httpPort.toIntOrNull()?.takeIf { it in 1..65535 } ?: 1081
        val socksAuth = proxyAuth(socksUsername, socksPassword, "SOCKS5")
        val httpAuth = proxyAuth(httpUsername, httpPassword, "HTTP 代理")
        val uploadEnabled = uploadEndpoint.isNotBlank()
        if (uploadEnabled) {
            require(uploadUsername.isNotBlank()) { "上传用户名不能为空" }
            require(uploadPassword.isNotBlank()) { "上传密码不能为空" }
        }
        return """
listen:
  socks5:
    enabled: true
    bind: "0.0.0.0:$socksPort"
$socksAuth
  http:
    enabled: $httpEnabled
    bind: "0.0.0.0:$httpPort"
$httpAuth
  tcp: []
capture:
  targets:
    - ip: "60.205.202.26"
      ports: [1002]
  save_dir: ${yamlString("$filesDir/captures")}
  save_uncaptured_sessions: false
upload:
  enabled: $uploadEnabled
  endpoint: ${yamlString(uploadEndpoint.ifBlank { "https://example.invalid/upload" })}
  basic_auth:
    username: ${yamlString(uploadUsername)}
    password: ${yamlString(uploadPassword)}
  auto_package_on_disconnect: true
  upload_interval_seconds: 60
  retry: { max_attempts: 5, base_delay_seconds: 3 }
storage: { flush_each_chunk: true }
log: { level: "info" }
        """.trimIndent()
    }

    private fun proxyAuth(username: String, password: String, displayName: String): String = when {
        username.isBlank() && password.isBlank() -> ""
        username.isBlank() -> throw IllegalArgumentException("配置 $displayName 密码时必须同时填写用户名")
        password.isBlank() -> throw IllegalArgumentException("配置 $displayName 用户名时必须同时填写密码")
        else -> """
auth:
  username: ${yamlString(username)}
  password: ${yamlString(password)}
        """.trimIndent().prependIndent("    ")
    }

    private fun yamlString(value: String): String =
        "\"${value.replace("\\", "\\\\").replace("\"", "\\\"").replace("\n", "")}\""
}

class AgentSettingsRepository(context: Context) {
    private val preferences = EncryptedSharedPreferences.create(
        context,
        "agent_settings",
        MasterKey.Builder(context).setKeyScheme(MasterKey.KeyScheme.AES256_GCM).build(),
        EncryptedSharedPreferences.PrefKeyEncryptionScheme.AES256_SIV,
        EncryptedSharedPreferences.PrefValueEncryptionScheme.AES256_GCM,
    )

    fun load(): AgentSettings {
        val socksUsername = preferences.getString("socks_username", "")!!
        val socksPassword = preferences.getString("socks_password", "")!!
        return AgentSettings(
            socksPort = preferences.getString("socks_port", "1080")!!,
            // Migrate the former mobile default to optional/no-auth mode.
            socksUsername = if (socksUsername == "torchnexus" && socksPassword.isBlank()) "" else socksUsername,
            socksPassword = socksPassword,
            httpEnabled = preferences.getBoolean("http_enabled", true),
            httpPort = preferences.getString("http_port", "1081")!!,
            httpUsername = preferences.getString("http_username", "")!!,
            httpPassword = preferences.getString("http_password", "")!!,
            uploadEndpoint = preferences.getString(
                "upload_endpoint",
                "https://torchnexus.cc/api/v1/app/tcp-batches",
            )!!,
            uploadUsername = preferences.getString("upload_username", "")!!,
            uploadPassword = preferences.getString("upload_password", "")!!,
        )
    }

    fun save(settings: AgentSettings) {
        preferences.edit()
            .putString("socks_port", settings.socksPort)
            .putString("socks_username", settings.socksUsername)
            .putString("socks_password", settings.socksPassword)
            .putBoolean("http_enabled", settings.httpEnabled)
            .putString("http_port", settings.httpPort)
            .putString("http_username", settings.httpUsername)
            .putString("http_password", settings.httpPassword)
            .putString("upload_endpoint", settings.uploadEndpoint)
            .putString("upload_username", settings.uploadUsername)
            .putString("upload_password", settings.uploadPassword)
            .apply()
    }
}

fun localProxyAddress(port: String, fallbackPort: Int = 1080): String {
    val resolvedPort = port.toIntOrNull()?.takeIf { it in 1..65535 } ?: fallbackPort
    val interfaces = NetworkInterface.getNetworkInterfaces() ?: return "未检测到局域网地址:$resolvedPort"
    while (interfaces.hasMoreElements()) {
        val networkInterface = interfaces.nextElement()
        if (!networkInterface.isUp || networkInterface.isLoopback) continue
        val addresses = networkInterface.inetAddresses
        while (addresses.hasMoreElements()) {
            val address = addresses.nextElement()
            if (address is Inet4Address && address.isSiteLocalAddress) {
                return "${address.hostAddress}:$resolvedPort"
            }
        }
    }
    return "未检测到局域网地址:$resolvedPort"
}
