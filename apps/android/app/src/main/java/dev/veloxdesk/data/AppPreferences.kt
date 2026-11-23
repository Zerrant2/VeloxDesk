package dev.veloxdesk.data

import android.content.Context
import android.content.SharedPreferences
import dev.veloxdesk.bridge.BridgeConstants
import dev.veloxdesk.bridge.HostPasswordMode
import dev.veloxdesk.bridge.HostSavedTrustedPeer
import dev.veloxdesk.bridge.ScaleMode
import dev.veloxdesk.bridge.TouchMode
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import org.json.JSONArray
import org.json.JSONObject
import java.util.Locale

enum class AppLanguage(val code: String, val shortLabel: String, val nativeName: String) {
    EN("en", "EN", "English"),
    RU("ru", "RU", "Русский");

    companion object {
        fun fromCode(code: String?): AppLanguage =
            entries.firstOrNull { it.code.equals(code, ignoreCase = true) } ?: EN

        fun defaultForDevice(locale: Locale = Locale.getDefault()): AppLanguage =
            if (locale.language.equals("ru", ignoreCase = true)) RU else EN
    }
}

enum class AppThemeMode(val code: String) {
    SYSTEM("system"),
    DARK("dark"),
    LIGHT("light");

    fun next(): AppThemeMode = when (this) {
        SYSTEM -> DARK
        DARK -> LIGHT
        LIGHT -> SYSTEM
    }

    companion object {
        fun fromCode(code: String?): AppThemeMode =
            entries.firstOrNull { it.code.equals(code, ignoreCase = true) } ?: SYSTEM
    }
}

enum class BitratePreset(val kbps: Int, val mbps: Int) {
    LOW(5_000, 5),
    BALANCED(20_000, 20),
    HIGH(50_000, 50),
    ULTRA(100_000, 100);

    companion object {
        fun nearest(kbps: Int): BitratePreset =
            entries.minByOrNull { kotlin.math.abs(it.kbps - kbps) } ?: BALANCED
    }
}

data class StreamProfile(
    val bitrateKbps: Int,
    val fpsCap: Int,
    val scaleMode: ScaleMode,
    val touchMode: TouchMode,
    val audioEnabled: Boolean,
    val clipboardEnabled: Boolean,
    val showHudOnConnect: Boolean,
) {
    companion object {
        fun defaultDesktop(): StreamProfile = StreamProfile(
            bitrateKbps = BitratePreset.BALANCED.kbps,
            fpsCap = 60,
            scaleMode = ScaleMode.FIT,
            touchMode = TouchMode.TRACKPAD,
            audioEnabled = true,
            clipboardEnabled = true,
            showHudOnConnect = false,
        )

        fun defaultGaming(): StreamProfile = StreamProfile(
            bitrateKbps = BitratePreset.HIGH.kbps,
            fpsCap = 120,
            scaleMode = ScaleMode.FIT,
            touchMode = TouchMode.TRACKPAD,
            audioEnabled = true,
            clipboardEnabled = true,
            showHudOnConnect = true,
        )
    }
}

data class RecentConnection(
    val target: String,
    val label: String,
    val mode: Int,
    val lastUsedEpochMs: Long,
)

data class AppSettings(
    val language: AppLanguage = AppLanguage.EN,
    val themeMode: AppThemeMode = AppThemeMode.SYSTEM,
    val rendezvousServer: String = "",
    val relayOnly: Boolean = false,
    val lastMode: Int = BridgeConstants.MODE_DESKTOP,
    val desktopProfile: StreamProfile = StreamProfile.defaultDesktop(),
    val gamingProfile: StreamProfile = StreamProfile.defaultGaming(),
    val recentConnections: List<RecentConnection> = emptyList(),
    val hostPasswordMode: Int = HostPasswordMode.ONE_TIME.id,
    val hostPermanentPasswordHash: String = "",
    val hostRequireAccept: Boolean = true,
    val hostTrustedPeers: List<HostSavedTrustedPeer> = emptyList(),
) {
    fun profileForMode(mode: Int): StreamProfile =
        if (mode == BridgeConstants.MODE_GAMING) gamingProfile else desktopProfile

    val hasHostPermanentPassword: Boolean
        get() = hostPermanentPasswordHash.trim().startsWith("\$argon2id\$")

    val effectiveHostOtpEnabled: Boolean
        get() = hostPasswordMode != HostPasswordMode.PERMANENT.id || !hasHostPermanentPassword

    val effectiveHostPermanentHash: String
        get() = if (hostPasswordMode == HostPasswordMode.ONE_TIME.id || !hasHostPermanentPassword) {
            ""
        } else {
            hostPermanentPasswordHash.trim()
        }
}

/**
 * SharedPreferences-backed persistence for VeloxDesk Android settings, profiles, host security, and recent hosts.
 */
class AppPreferences(context: Context) {
    private val prefs: SharedPreferences =
        context.applicationContext.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)

    private val _settings = MutableStateFlow(loadSettings())
    val settings: StateFlow<AppSettings> = _settings.asStateFlow()

    private fun loadSettings(): AppSettings {
        val defaultLang = AppLanguage.defaultForDevice()
        val langCode = prefs.getString(KEY_LANGUAGE, null)
        val language = if (langCode != null) AppLanguage.fromCode(langCode) else defaultLang
        val themeMode = AppThemeMode.fromCode(prefs.getString(KEY_THEME_MODE, null))

        val rendezvous = prefs.getString(KEY_RENDEZVOUS_SERVER, "")?.trim().orEmpty()
        val relayOnly = prefs.getBoolean(KEY_RELAY_ONLY, false)
        val lastMode = prefs.getInt(KEY_LAST_MODE, BridgeConstants.MODE_DESKTOP)
            .let { if (it == BridgeConstants.MODE_GAMING) BridgeConstants.MODE_GAMING else BridgeConstants.MODE_DESKTOP }

        val desktop = loadProfile(PREFIX_DESKTOP, StreamProfile.defaultDesktop())
        val gaming = loadProfile(PREFIX_GAMING, StreamProfile.defaultGaming())
        val recentJson = prefs.getString(KEY_RECENT_CONNECTIONS, "[]").orEmpty()
        val recent = deserializeRecentConnections(recentJson)

        val hostPwMode = HostPasswordMode.fromId(prefs.getInt(KEY_HOST_PASSWORD_MODE, HostPasswordMode.ONE_TIME.id)).id
        val hostPermHash = prefs.getString(KEY_HOST_PERM_HASH, "").orEmpty().trim()
        val hostRequireAccept = prefs.getBoolean(KEY_HOST_REQUIRE_ACCEPT, true)
        val hostTrustedJson = prefs.getString(KEY_HOST_TRUSTED_PEERS, "[]").orEmpty()
        val hostTrusted = HostSavedTrustedPeer.listFromJson(hostTrustedJson)

        return AppSettings(
            language = language,
            themeMode = themeMode,
            rendezvousServer = rendezvous,
            relayOnly = relayOnly,
            lastMode = lastMode,
            desktopProfile = desktop,
            gamingProfile = gaming,
            recentConnections = recent,
            hostPasswordMode = hostPwMode,
            hostPermanentPasswordHash = hostPermHash,
            hostRequireAccept = hostRequireAccept,
            hostTrustedPeers = hostTrusted,
        )
    }

    private fun loadProfile(prefix: String, defaults: StreamProfile): StreamProfile {
        val rawFps = prefs.getInt("${prefix}fps_cap", defaults.fpsCap)
        val fpsCap = if (rawFps >= 120) 120 else 60
        return StreamProfile(
            bitrateKbps = prefs.getInt("${prefix}bitrate_kbps", defaults.bitrateKbps)
                .coerceIn(1_000, 200_000),
            fpsCap = fpsCap,
            scaleMode = ScaleMode.fromId(prefs.getInt("${prefix}scale_mode", defaults.scaleMode.id)),
            touchMode = TouchMode.fromId(prefs.getInt("${prefix}touch_mode", defaults.touchMode.id)),
            audioEnabled = prefs.getBoolean("${prefix}audio_enabled", defaults.audioEnabled),
            clipboardEnabled = prefs.getBoolean("${prefix}clipboard_enabled", defaults.clipboardEnabled),
            showHudOnConnect = prefs.getBoolean("${prefix}show_hud", defaults.showHudOnConnect),
        )
    }

    private fun saveProfile(editor: SharedPreferences.Editor, prefix: String, profile: StreamProfile) {
        editor.putInt("${prefix}bitrate_kbps", profile.bitrateKbps)
        editor.putInt("${prefix}fps_cap", profile.fpsCap)
        editor.putInt("${prefix}scale_mode", profile.scaleMode.id)
        editor.putInt("${prefix}touch_mode", profile.touchMode.id)
        editor.putBoolean("${prefix}audio_enabled", profile.audioEnabled)
        editor.putBoolean("${prefix}clipboard_enabled", profile.clipboardEnabled)
        editor.putBoolean("${prefix}show_hud", profile.showHudOnConnect)
    }

    fun setLanguage(language: AppLanguage) {
        prefs.edit().putString(KEY_LANGUAGE, language.code).apply()
        _settings.value = _settings.value.copy(language = language)
    }

    fun setThemeMode(themeMode: AppThemeMode) {
        prefs.edit().putString(KEY_THEME_MODE, themeMode.code).apply()
        _settings.value = _settings.value.copy(themeMode = themeMode)
    }

    fun setRendezvousServer(server: String) {
        val cleaned = server.trim()
        prefs.edit().putString(KEY_RENDEZVOUS_SERVER, cleaned).apply()
        _settings.value = _settings.value.copy(rendezvousServer = cleaned)
    }

    fun setRelayOnly(relayOnly: Boolean) {
        prefs.edit().putBoolean(KEY_RELAY_ONLY, relayOnly).apply()
        _settings.value = _settings.value.copy(relayOnly = relayOnly)
    }

    fun setLastMode(mode: Int) {
        val normalized = if (mode == BridgeConstants.MODE_GAMING) {
            BridgeConstants.MODE_GAMING
        } else {
            BridgeConstants.MODE_DESKTOP
        }
        prefs.edit().putInt(KEY_LAST_MODE, normalized).apply()
        _settings.value = _settings.value.copy(lastMode = normalized)
    }

    fun updateProfile(mode: Int, transform: (StreamProfile) -> StreamProfile) {
        val isGaming = mode == BridgeConstants.MODE_GAMING
        val current = _settings.value
        val updated = transform(if (isGaming) current.gamingProfile else current.desktopProfile)
        val prefix = if (isGaming) PREFIX_GAMING else PREFIX_DESKTOP
        prefs.edit().also { saveProfile(it, prefix, updated) }.apply()
        _settings.value = if (isGaming) {
            current.copy(gamingProfile = updated)
        } else {
            current.copy(desktopProfile = updated)
        }
    }

    fun setHostPasswordMode(mode: Int) {
        val normalized = HostPasswordMode.fromId(mode).id
        prefs.edit().putInt(KEY_HOST_PASSWORD_MODE, normalized).apply()
        _settings.value = _settings.value.copy(hostPasswordMode = normalized)
    }

    fun setHostPermanentPasswordHash(phcHash: String) {
        val cleaned = phcHash.trim()
        prefs.edit().putString(KEY_HOST_PERM_HASH, cleaned).apply()
        _settings.value = _settings.value.copy(hostPermanentPasswordHash = cleaned)
    }

    fun clearHostPermanentPasswordHash() {
        prefs.edit().remove(KEY_HOST_PERM_HASH).apply()
        _settings.value = _settings.value.copy(hostPermanentPasswordHash = "")
    }

    fun setHostRequireAccept(requireAccept: Boolean) {
        prefs.edit().putBoolean(KEY_HOST_REQUIRE_ACCEPT, requireAccept).apply()
        _settings.value = _settings.value.copy(hostRequireAccept = requireAccept)
    }

    @Synchronized
    fun reload() {
        _settings.value = loadSettings()
    }

    @Synchronized
    fun upsertHostTrustedPeer(peer: HostSavedTrustedPeer) {
        val updated = upsertTrustedPeer(_settings.value.hostTrustedPeers, peer)
        prefs.edit().putString(KEY_HOST_TRUSTED_PEERS, HostSavedTrustedPeer.listToJson(updated)).apply()
        _settings.value = _settings.value.copy(hostTrustedPeers = updated)
    }

    @Synchronized
    fun removeHostTrustedPeer(publicKeyHex: String): Boolean {
        val before = _settings.value.hostTrustedPeers
        val updated = removeTrustedPeer(before, publicKeyHex)
        if (updated.size == before.size) return false
        prefs.edit().putString(KEY_HOST_TRUSTED_PEERS, HostSavedTrustedPeer.listToJson(updated)).apply()
        _settings.value = _settings.value.copy(hostTrustedPeers = updated)
        return true
    }

    fun recordRecentConnection(target: String, label: String, mode: Int, nowEpochMs: Long = System.currentTimeMillis()) {
        val normalizedTarget = normalizeTarget(target)
        if (normalizedTarget.isEmpty()) return
        val entry = RecentConnection(
            target = normalizedTarget,
            label = label.trim().ifEmpty { formatDisplayTarget(normalizedTarget) },
            mode = mode,
            lastUsedEpochMs = nowEpochMs,
        )
        val updated = upsertRecentConnection(_settings.value.recentConnections, entry, MAX_RECENT)
        prefs.edit().putString(KEY_RECENT_CONNECTIONS, serializeRecentConnections(updated)).apply()
        _settings.value = _settings.value.copy(recentConnections = updated)
    }

    fun removeRecentConnection(target: String) {
        val normalizedTarget = normalizeTarget(target)
        val updated = _settings.value.recentConnections.filterNot {
            normalizeTarget(it.target).equals(normalizedTarget, ignoreCase = true)
        }
        prefs.edit().putString(KEY_RECENT_CONNECTIONS, serializeRecentConnections(updated)).apply()
        _settings.value = _settings.value.copy(recentConnections = updated)
    }

    companion object {
        private const val PREFS_NAME = "veloxdesk_prefs"
        private const val KEY_LANGUAGE = "language"
        private const val KEY_THEME_MODE = "theme_mode"
        private const val KEY_RENDEZVOUS_SERVER = "rendezvous_server"
        private const val KEY_RELAY_ONLY = "relay_only"
        private const val KEY_LAST_MODE = "last_mode"
        private const val KEY_RECENT_CONNECTIONS = "recent_connections_json"
        private const val KEY_HOST_PASSWORD_MODE = "host_password_mode"
        private const val KEY_HOST_PERM_HASH = "host_permanent_password_hash"
        private const val KEY_HOST_REQUIRE_ACCEPT = "host_require_accept"
        private const val KEY_HOST_TRUSTED_PEERS = "host_trusted_peers_json"
        private const val PREFIX_DESKTOP = "profile_desktop_"
        private const val PREFIX_GAMING = "profile_gaming_"
        const val MAX_RECENT = 12

        fun upsertTrustedPeer(
            existing: List<HostSavedTrustedPeer>,
            peer: HostSavedTrustedPeer,
        ): List<HostSavedTrustedPeer> {
            val pkNorm = peer.publicKeyHex.trim().lowercase(Locale.US)
            if (pkNorm.isEmpty()) return existing
            val cleaned = peer.copy(publicKeyHex = pkNorm)
            val filtered = existing.filterNot { it.publicKeyHex.equals(pkNorm, ignoreCase = true) }
            return listOf(cleaned) + filtered
        }

        fun removeTrustedPeer(
            existing: List<HostSavedTrustedPeer>,
            publicKeyHex: String,
        ): List<HostSavedTrustedPeer> {
            val pkNorm = publicKeyHex.trim()
            return existing.filterNot { it.publicKeyHex.equals(pkNorm, ignoreCase = true) }
        }

        /**
         * Formats a 9-digit VeloxDesk Device ID as `XXX XXX XXX`.
         * Non-9-digit inputs are returned trimmed unchanged.
         */
        fun formatDeviceId(raw: String): String {
            val trimmed = raw.trim()
            val digits = trimmed.filter { !it.isWhitespace() && it != '-' }
            if (digits.length == 9 && digits.all { it in '0'..'9' }) {
                return "${digits.substring(0, 3)} ${digits.substring(3, 6)} ${digits.substring(6, 9)}"
            }
            return trimmed
        }

        /**
         * Returns `true` when [raw] represents a 9-digit VeloxDesk Device ID (spaces/dashes allowed).
         */
        fun isNineDigitDeviceId(raw: String): Boolean {
            val trimmed = raw.trim()
            if (trimmed.isEmpty()) return false
            if (trimmed.any { it !in '0'..'9' && it != ' ' && it != '-' }) return false
            val digits = trimmed.filter { it in '0'..'9' }
            return digits.length == 9
        }

        /**
         * Canonicalizes a connection target (9-digit IDs become 9 contiguous digits; hosts are trimmed).
         */
        fun normalizeTarget(raw: String): String {
            val trimmed = raw.trim()
            if (isNineDigitDeviceId(trimmed)) {
                return trimmed.filter { it in '0'..'9' }
            }
            return trimmed
        }

        fun formatDisplayTarget(target: String): String =
            if (isNineDigitDeviceId(target)) formatDeviceId(target) else target.trim()

        /**
         * Validates that [raw] is either a 9-digit Device ID, an IPv4/IPv6 address (optional `:port`),
         * or a syntactically valid hostname (optional `:port`).
         */
        fun isValidTarget(raw: String): Boolean {
            val trimmed = raw.trim()
            if (trimmed.isEmpty()) return false
            if (trimmed.any { it.isISOControl() }) return false
            if (isNineDigitDeviceId(trimmed)) return true
            if (trimmed.any { it.isWhitespace() }) return false

            // Bracketed IPv6: [addr] or [addr]:port
            if (trimmed.startsWith('[')) {
                val closeIdx = trimmed.indexOf(']')
                if (closeIdx <= 1) return false
                val ipv6 = trimmed.substring(1, closeIdx)
                if (!isValidIpv6Literal(ipv6)) return false
                val tail = trimmed.substring(closeIdx + 1)
                if (tail.isEmpty()) return true
                if (!tail.startsWith(':')) return false
                return isValidPort(tail.substring(1))
            }

            // Bare IPv6 (contains 2+ colons, no brackets)
            val colonCount = trimmed.count { it == ':' }
            if (colonCount >= 2) {
                return isValidIpv6Literal(trimmed)
            }

            val host: String
            if (colonCount == 1) {
                val idx = trimmed.lastIndexOf(':')
                host = trimmed.substring(0, idx)
                val portStr = trimmed.substring(idx + 1)
                if (!isValidPort(portStr)) return false
            } else {
                host = trimmed
            }

            if (host.isEmpty()) return false
            // If all numeric + dots, strictly validate IPv4 octets
            if (host.all { it in '0'..'9' || it == '.' }) {
                return isValidIpv4(host)
            }
            return isValidHostname(host)
        }

        private fun isValidPort(portStr: String): Boolean {
            if (portStr.isEmpty() || !portStr.all { it in '0'..'9' }) return false
            val port = portStr.toIntOrNull() ?: return false
            return port in 1..65535
        }

        private fun isValidIpv4(host: String): Boolean {
            val parts = host.split('.')
            if (parts.size != 4) return false
            return parts.all { part ->
                part.isNotEmpty() && part.length <= 3 &&
                    part.all { it in '0'..'9' } &&
                    (part.toIntOrNull() ?: -1) in 0..255
            }
        }

        private fun isValidIpv6Literal(addr: String): Boolean {
            if (addr.isEmpty() || !addr.contains(':')) return false
            if (addr.indexOf("::") != addr.lastIndexOf("::")) return false
            if (!addr.all { it in '0'..'9' || it in 'a'..'f' || it in 'A'..'F' || it == ':' || it == '.' }) {
                return false
            }
            return true
        }

        private fun isValidHostname(host: String): Boolean {
            if (host.length > 253) return false
            val labels = host.split('.')
            return labels.all { label ->
                label.isNotEmpty() &&
                    label.length <= 63 &&
                    !label.startsWith('-') &&
                    !label.endsWith('-') &&
                    label.all { it in 'a'..'z' || it in 'A'..'Z' || it in '0'..'9' || it == '-' }
            }
        }

        fun upsertRecentConnection(
            existing: List<RecentConnection>,
            entry: RecentConnection,
            maxCount: Int = MAX_RECENT,
        ): List<RecentConnection> {
            val norm = normalizeTarget(entry.target)
            val filtered = existing.filterNot {
                normalizeTarget(it.target).equals(norm, ignoreCase = true)
            }
            return (listOf(entry.copy(target = norm)) + filtered).take(maxCount.coerceAtLeast(1))
        }

        fun serializeRecentConnections(list: List<RecentConnection>): String {
            val arr = JSONArray()
            for (item in list) {
                val obj = JSONObject()
                obj.put("target", item.target)
                obj.put("label", item.label)
                obj.put("mode", item.mode)
                obj.put("lastUsedEpochMs", item.lastUsedEpochMs)
                arr.put(obj)
            }
            return arr.toString()
        }

        fun deserializeRecentConnections(json: String): List<RecentConnection> {
            if (json.isBlank()) return emptyList()
            return try {
                val arr = JSONArray(json)
                buildList {
                    for (i in 0 until arr.length()) {
                        val obj = arr.optJSONObject(i) ?: continue
                        val target = obj.optString("target", "").trim()
                        if (target.isEmpty()) continue
                        val label = obj.optString("label", "").trim().ifEmpty { formatDisplayTarget(target) }
                        val mode = obj.optInt("mode", BridgeConstants.MODE_DESKTOP)
                        val ts = obj.optLong("lastUsedEpochMs", 0L)
                        add(RecentConnection(target = target, label = label, mode = mode, lastUsedEpochMs = ts))
                    }
                }.take(MAX_RECENT)
            } catch (_: Throwable) {
                emptyList()
            }
        }
    }
}
