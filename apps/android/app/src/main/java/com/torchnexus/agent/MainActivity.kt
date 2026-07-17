package com.torchnexus.agent

import android.app.Activity
import android.os.Bundle
import android.widget.Toast
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive

class MainActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        val repository = AgentSettingsRepository(this)
        setContent {
            MaterialTheme {
                var settings by remember { mutableStateOf(repository.load()) }
                var vpnEnabled by remember { mutableStateOf(TorchNexusVpnService.isRunning()) }
                LaunchedEffect(Unit) {
                    while (isActive) {
                        vpnEnabled = TorchNexusVpnService.isRunning()
                        delay(1_000)
                    }
                }
                val vpnPermission = rememberLauncherForActivityResult(
                    ActivityResultContracts.StartActivityForResult(),
                ) { result ->
                    if (result.resultCode == Activity.RESULT_OK) {
                        startAgent(repository)
                        vpnEnabled = true
                    }
                }
                AgentSettingsScreen(
                    settings = settings,
                    vpnEnabled = vpnEnabled,
                    onSettingsChange = { settings = it },
                    onSave = {
                        repository.save(settings)
                        Toast.makeText(this, "设置已保存", Toast.LENGTH_SHORT).show()
                    },
                    onVpnEnabledChange = { enabled ->
                        if (enabled) {
                            try {
                                // VPN must always use the last explicitly saved configuration.
                                repository.load().toYaml(filesDir.absolutePath)
                                val prepareIntent = android.net.VpnService.prepare(this)
                                if (prepareIntent == null) {
                                    startAgent(repository)
                                    vpnEnabled = true
                                } else {
                                    vpnPermission.launch(prepareIntent)
                                }
                            } catch (error: IllegalArgumentException) {
                                Toast.makeText(this, error.message, Toast.LENGTH_LONG).show()
                                vpnEnabled = false
                            }
                        } else {
                            TorchNexusVpnService.stop(this)
                            vpnEnabled = false
                        }
                    },
                )
            }
        }
    }

    private fun startAgent(repository: AgentSettingsRepository) {
        val savedSettings = repository.load()
        startService(TorchNexusVpnService.intent(this, savedSettings.toYaml(filesDir.absolutePath)))
    }
}

@Suppress("LongMethod")
@androidx.compose.runtime.Composable
private fun AgentSettingsScreen(
    settings: AgentSettings,
    vpnEnabled: Boolean,
    onSettingsChange: (AgentSettings) -> Unit,
    onSave: () -> Unit,
    onVpnEnabledChange: (Boolean) -> Unit,
) {
    Column(
        modifier = Modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(PaddingValues(20.dp)),
        verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        Text("上传接口认证", style = MaterialTheme.typography.titleMedium)
        OutlinedTextField(
            value = settings.uploadUsername,
            onValueChange = { onSettingsChange(settings.copy(uploadUsername = it)) },
            modifier = Modifier.fillMaxWidth(), label = { Text("上传用户名") }, singleLine = true,
        )
        SecretField("上传密码", settings.uploadPassword) {
            onSettingsChange(settings.copy(uploadPassword = it))
        }
        Row(
            modifier = Modifier.fillMaxWidth(),
            horizontalArrangement = Arrangement.SpaceBetween,
            verticalAlignment = androidx.compose.ui.Alignment.CenterVertically,
        ) {
            Text("TorchNexus Agent", style = MaterialTheme.typography.headlineSmall)
            Switch(checked = vpnEnabled, onCheckedChange = onVpnEnabledChange)
        }
        Text("局域网 SOCKS5 代理", style = MaterialTheme.typography.titleMedium)
        Text("外部设备请配置：${localProxyAddress(settings.socksPort)}")
        Text("用户名和密码留空则不启用 SOCKS5 认证。", style = MaterialTheme.typography.bodySmall)
        OutlinedTextField(
            value = settings.socksPort,
            onValueChange = { onSettingsChange(settings.copy(socksPort = it)) },
            modifier = Modifier.fillMaxWidth(), label = { Text("SOCKS5 端口") },
            keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Number), singleLine = true,
        )
        OutlinedTextField(
            value = settings.socksUsername,
            onValueChange = { onSettingsChange(settings.copy(socksUsername = it)) },
            modifier = Modifier.fillMaxWidth(), label = { Text("SOCKS5 用户名") }, singleLine = true,
        )
        SecretField("SOCKS5 密码", settings.socksPassword) {
            onSettingsChange(settings.copy(socksPassword = it))
        }
        Row(
            modifier = Modifier.fillMaxWidth(),
            horizontalArrangement = Arrangement.SpaceBetween,
            verticalAlignment = androidx.compose.ui.Alignment.CenterVertically,
        ) {
            Text("局域网 HTTP 代理", style = MaterialTheme.typography.titleMedium)
            Switch(
                checked = settings.httpEnabled,
                onCheckedChange = { onSettingsChange(settings.copy(httpEnabled = it)) },
            )
        }
        Text("外部设备请配置：${localProxyAddress(settings.httpPort, fallbackPort = 1081)}")
        Text("用户名和密码留空则不启用 HTTP 代理认证。", style = MaterialTheme.typography.bodySmall)
        OutlinedTextField(
            value = settings.httpPort,
            onValueChange = { onSettingsChange(settings.copy(httpPort = it)) },
            modifier = Modifier.fillMaxWidth(), label = { Text("HTTP 代理端口") },
            keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Number), singleLine = true,
            enabled = settings.httpEnabled,
        )
        OutlinedTextField(
            value = settings.httpUsername,
            onValueChange = { onSettingsChange(settings.copy(httpUsername = it)) },
            modifier = Modifier.fillMaxWidth(), label = { Text("HTTP 代理用户名") }, singleLine = true,
            enabled = settings.httpEnabled,
        )
        SecretField(
            label = "HTTP 代理密码",
            value = settings.httpPassword,
            enabled = settings.httpEnabled,
        ) {
            onSettingsChange(settings.copy(httpPassword = it))
        }
        Text("上传服务器（留空则仅本地保存）", style = MaterialTheme.typography.titleMedium)
        OutlinedTextField(
            value = settings.uploadEndpoint,
            onValueChange = { onSettingsChange(settings.copy(uploadEndpoint = it)) },
            modifier = Modifier.fillMaxWidth(), label = { Text("上传服务器地址") }, singleLine = true,
        )
        Button(onClick = onSave, modifier = Modifier.fillMaxWidth()) { Text("保存设置") }
    }
}

@androidx.compose.runtime.Composable
private fun SecretField(
    label: String,
    value: String,
    enabled: Boolean = true,
    onValueChange: (String) -> Unit,
) {
    OutlinedTextField(
        value = value,
        onValueChange = onValueChange,
        modifier = Modifier.fillMaxWidth(),
        label = { Text(label) },
        visualTransformation = PasswordVisualTransformation(),
        singleLine = true,
        enabled = enabled,
    )
}
