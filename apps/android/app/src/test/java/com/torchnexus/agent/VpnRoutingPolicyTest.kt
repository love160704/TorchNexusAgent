package com.torchnexus.agent

import org.junit.Assert.assertEquals
import org.junit.Test

class VpnRoutingPolicyTest {
    @Test
    fun `host application bypasses its own full-tunnel route`() {
        assertEquals(
            listOf("com.torchnexus.agent"),
            vpnExcludedApplications("com.torchnexus.agent"),
        )
    }
}
