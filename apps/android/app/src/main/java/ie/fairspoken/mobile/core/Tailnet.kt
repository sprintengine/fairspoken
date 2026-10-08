package ie.fairspoken.mobile.core

import android.content.Context
import android.net.ConnectivityManager
import android.net.NetworkCapabilities
import java.net.Inet4Address
import java.net.Inet6Address
import java.net.InetAddress

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
                .firstOrNull { it is Inet4Address && isTailnetAddress(it) }
                ?.hostAddress
                ?: continue
            val domain = link.domains.orEmpty().split(' ', ',')
                .map { it.trim().trimEnd('.') }
                .firstOrNull { it.endsWith(".ts.net") }
            return TailnetStatus(true, address, domain)
        }
        return TailnetStatus(false, null, null)
    }

    /** 100.64.0.0/10 and fd7a:115c:a1e0::/48, the ranges Tailscale hands out. */
    fun isTailnetAddress(address: InetAddress): Boolean {
        val b = address.address.map { it.toInt() and 0xFF }
        return when (address) {
            is Inet4Address -> b[0] == 100 && b[1] in 64..127
            is Inet6Address -> b.take(6) == listOf(0xfd, 0x7a, 0x11, 0x5c, 0xa1, 0xe0)
            else -> false
        }
    }
}
