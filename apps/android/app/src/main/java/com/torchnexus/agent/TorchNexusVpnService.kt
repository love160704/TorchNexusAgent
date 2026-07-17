package com.torchnexus.agent

import android.content.Intent
import android.net.VpnService
import android.os.Handler
import android.os.Looper
import android.util.Log
import com.torchnexus.agent.rust.MobileEngine
import com.torchnexus.agent.rust.MobileEngineException

/** Android VPN entry point. The UniFFI bridge will own the TUN forwarding lifecycle. */
class TorchNexusVpnService : VpnService() {
    private var tun: android.os.ParcelFileDescriptor? = null
    private val engine = MobileEngine()
    private val mainHandler = Handler(Looper.getMainLooper())
    private var engineStarted = false
    private var stopping = false
    private var pendingStart: PendingStart? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        if (intent?.action == ACTION_STOP) {
            running = false
            pendingStart = null
            if (!stopping) stopEngineAsync()
            return START_STICKY
        }
        val configYaml = intent?.getStringExtra(EXTRA_CONFIG_YAML) ?: return START_NOT_STICKY
        if (stopping) {
            pendingStart = PendingStart(configYaml, startId)
            return START_STICKY
        }
        return startEngine(configYaml, startId)
    }

    private fun startEngine(configYaml: String, startId: Int): Int {
        // startService can deliver a new command while the existing VPN is still active.
        // The native engine is process-global and rejects a second start.
        if (engineStarted || tun != null) return START_STICKY
        val descriptor = Builder()
            .setSession("TorchNexus Agent")
            .setMtu(MTU)
            .addAddress("198.18.0.2", 32)
            .addRoute("0.0.0.0", 0)
            .apply {
                // The agent owns both tun2proxy and the SOCKS5 outbound sockets.
                // Routing its UID back into this TUN would recursively proxy every
                // connection until both the device and LAN SOCKS clients time out.
                vpnExcludedApplications(packageName).forEach(::addDisallowedApplication)
            }
            .establish() ?: return START_NOT_STICKY
        tun = descriptor
        try {
            engine.start(
                tunFd = descriptor.detachFd(),
                closeTunFd = true,
                configYaml = configYaml,
                mtu = MTU.toUShort(),
                packetInformation = false,
            )
            engineStarted = true
            running = true
        } catch (error: MobileEngineException) {
            // A prior shutdown can briefly retain the SOCKS listener. Never let that
            // transient state crash the application process.
            Log.e(TAG, "Unable to start VPN engine", error)
            running = false
            tun?.close()
            tun = null
            stopSelf(startId)
            return START_NOT_STICKY
        }
        return START_STICKY
    }

    override fun onDestroy() {
        running = false
        pendingStart = null
        if (!stopping) stopEngineAsync()
        tun?.close()
        tun = null
        super.onDestroy()
    }

    override fun onRevoke() {
        running = false
        pendingStart = null
        stopSelf()
        super.onRevoke()
    }

    private fun stopEngineAsync() {
        if (!engineStarted) return
        // tun2proxy cleanup can wait for in-flight native work. The Android main
        // thread must return immediately so the UI can render the switch as off.
        engineStarted = false
        stopping = true
        Thread(
            {
                try {
                    engine.stop()
                } catch (_: MobileEngineException.NotRunning) {
                    // The native engine may already have stopped while Android tears down the service.
                } finally {
                    mainHandler.post {
                        stopping = false
                        tun?.close()
                        tun = null
                        val nextStart = pendingStart
                        pendingStart = null
                        if (nextStart == null) stopSelf()
                        else startEngine(nextStart.configYaml, nextStart.startId)
                    }
                }
            },
            "TorchNexusVpnStop",
        ).start()
    }

    companion object {
        const val EXTRA_CONFIG_YAML = "com.torchnexus.agent.CONFIG_YAML"
        private const val ACTION_STOP = "com.torchnexus.agent.action.STOP"
        private const val MTU = 1500
        private const val TAG = "TorchNexusVpn"
        @Volatile private var running = false

        fun isRunning(): Boolean = running

        fun stop(context: android.content.Context): Boolean {
            // A VpnService remains bound by Android even without a start request, so
            // stopService alone cannot release its TUN interface. Ask the service to
            // close its native engine and TUN from inside its own lifecycle first.
            running = false
            return context.startService(stopIntent(context)) != null
        }

        fun intent(context: android.content.Context, configYaml: String): Intent =
            Intent(context, TorchNexusVpnService::class.java).putExtra(EXTRA_CONFIG_YAML, configYaml)

        fun stopIntent(context: android.content.Context): Intent =
            Intent(context, TorchNexusVpnService::class.java).setAction(ACTION_STOP)
    }

    private data class PendingStart(val configYaml: String, val startId: Int)
}

internal fun vpnExcludedApplications(hostPackageName: String): List<String> =
    listOf(hostPackageName)
