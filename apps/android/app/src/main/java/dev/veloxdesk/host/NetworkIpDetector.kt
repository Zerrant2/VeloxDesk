package dev.veloxdesk.host

import java.net.Inet4Address
import java.net.NetworkInterface

enum class IpKind {
    TAILSCALE,
    LAN,
    OTHER,
}

data class DetectedIps(
    val lanIps: List<String> = emptyList(),
    val tailscaleIp: String? = null,
)

/**
 * Pure IPv4 address detector separating local LAN addresses (RFC 1918) from Tailscale
 * CGNAT addresses (`100.64.0.0/10`) for display on the Android Host card.
 */
object NetworkIpDetector {
    fun classifyIpv4(ip: String): IpKind {
        val parts = ip.split('.')
        if (parts.size != 4) return IpKind.OTHER
        val o0 = parts[0].toIntOrNull() ?: return IpKind.OTHER
        val o1 = parts[1].toIntOrNull() ?: return IpKind.OTHER
        val o2 = parts[2].toIntOrNull() ?: return IpKind.OTHER
        val o3 = parts[3].toIntOrNull() ?: return IpKind.OTHER
        if (o0 !in 0..255 || o1 !in 0..255 || o2 !in 0..255 || o3 !in 0..255) return IpKind.OTHER

        // Tailscale uses CGNAT 100.64.0.0/10 (100.64.0.0 - 100.127.255.255).
        if (o0 == 100 && o1 in 64..127) {
            return IpKind.TAILSCALE
        }
        // RFC 1918 private LAN ranges.
        if (o0 == 192 && o1 == 168) return IpKind.LAN
        if (o0 == 10) return IpKind.LAN
        if (o0 == 172 && o1 in 16..31) return IpKind.LAN
        return IpKind.OTHER
    }

    internal fun partitionIps(candidates: Iterable<String>): DetectedIps {
        val lan = LinkedHashSet<String>()
        var tailscale: String? = null
        for (raw in candidates) {
            val ip = raw.trim()
            if (ip.isEmpty() || ip.startsWith("127.") || ip == "0.0.0.0") continue
            when (classifyIpv4(ip)) {
                IpKind.TAILSCALE -> if (tailscale == null) tailscale = ip
                IpKind.LAN -> lan.add(ip)
                IpKind.OTHER -> {}
            }
        }
        val sortedLan = lan.sortedWith(
            compareBy<String>(
                {
                    when {
                        it.startsWith("192.168.") -> 0
                        it.startsWith("10.") -> 1
                        else -> 2
                    }
                },
                { it },
            ),
        )
        return DetectedIps(lanIps = sortedLan, tailscaleIp = tailscale)
    }

    fun detectLocalAndTailscaleIps(): DetectedIps {
        return try {
            val found = mutableListOf<String>()
            val ifaces = NetworkInterface.getNetworkInterfaces() ?: return DetectedIps()
            while (ifaces.hasMoreElements()) {
                val iface = ifaces.nextElement()
                val isUp = try {
                    iface.isUp && !iface.isLoopback
                } catch (_: Throwable) {
                    false
                }
                if (!isUp) continue
                val addrs = iface.inetAddresses
                while (addrs.hasMoreElements()) {
                    val addr = addrs.nextElement()
                    if (addr is Inet4Address && !addr.isLoopbackAddress && !addr.isLinkLocalAddress) {
                        addr.hostAddress?.let(found::add)
                    }
                }
            }
            partitionIps(found)
        } catch (_: Throwable) {
            DetectedIps()
        }
    }
}
