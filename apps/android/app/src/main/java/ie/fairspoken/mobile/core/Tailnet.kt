package ie.fairspoken.mobile.core

import android.content.Context
import android.net.ConnectivityManager
import android.net.NetworkCapabilities
import java.net.Inet4Address

/**
 * What the phone can see of its tailnet. The Tailscale app keeps its peer
 * list to itself, so this is only the VPN's state, the phone's own address
 * and the MagicDNS domain used to expand bare machine names.
 */
data class TailnetStatus(val connected: Boolean, val address: String?, val domain: String?)

object Tailnet {
    @Suppress("DEPRECATION") // allNetworks is the only way to look at a VPN another app owns.
    fun status(context: Context): TailnetStatus {
        val cm = context.getSystemService(ConnectivityManager::class.java)
        for (network in cm.allNetworks) {
            val caps = cm.getNetworkCapabilities(network) ?: continue
            if (!caps.hasTransport(NetworkCapabilities.TRANSPORT_VPN)) continue
            val link = cm.getLinkProperties(network) ?: continue
            val address = link.linkAddresses
                .map { it.address }
                .firstOrNull { it is Inet4Address && isTailscaleAddress(it.hostAddress.orEmpty()) }
                ?.hostAddress
                ?: continue
            val domain = link.domains.orEmpty().split(' ', ',')
                .map { it.trim().trimEnd('.') }
                .firstOrNull { it.endsWith(".ts.net") }
            return TailnetStatus(true, address, domain)
        }
        return TailnetStatus(false, null, null)
    }

    /** 100.64.0.0/10, the CGNAT range Tailscale hands out. */
    private fun isTailscaleAddress(ip: String): Boolean {
        val parts = ip.split('.').mapNotNull { it.toIntOrNull() }
        return parts.size == 4 && parts[0] == 100 && parts[1] in 64..127
    }
}
