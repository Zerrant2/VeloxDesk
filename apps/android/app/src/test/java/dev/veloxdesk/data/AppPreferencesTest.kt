package dev.veloxdesk.data

import android.content.SharedPreferences
import dev.veloxdesk.bridge.BridgeConstants
import dev.veloxdesk.bridge.HostPasswordMode
import dev.veloxdesk.bridge.HostSavedTrustedPeer
import dev.veloxdesk.bridge.ScaleMode
import dev.veloxdesk.bridge.TouchMode
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class AppPreferencesTest {
    @Test
    fun formatDeviceId_formatsNineDigitsAsGroupedTriplets() {
        assertEquals("123 456 789", AppPreferences.formatDeviceId("123456789"))
        assertEquals("987 654 321", AppPreferences.formatDeviceId(" 987 654 321 "))
        assertEquals("001 002 003", AppPreferences.formatDeviceId("001-002-003"))
        assertEquals("192.168.1.5", AppPreferences.formatDeviceId("  192.168.1.5 "))
        assertEquals("12345", AppPreferences.formatDeviceId("12345"))
    }

    @Test
    fun targetValidation_acceptsValidAndRejectsInvalidTargets() {
        // 9-digit Device IDs
        assertTrue(AppPreferences.isNineDigitDeviceId("123456789"))
        assertTrue(AppPreferences.isNineDigitDeviceId("123 456 789"))
        assertTrue(AppPreferences.isValidTarget("123456789"))
        assertTrue(AppPreferences.isValidTarget("123 456 789"))
        assertEquals("123456789", AppPreferences.normalizeTarget("123 456 789"))

        // IPv4 & IPv4:port
        assertTrue(AppPreferences.isValidTarget("192.168.1.10"))
        assertTrue(AppPreferences.isValidTarget("100.64.0.1:21118"))
        assertFalse(AppPreferences.isValidTarget("192.168.1.999"))
        assertFalse(AppPreferences.isValidTarget("192.168.1.10:0"))
        assertFalse(AppPreferences.isValidTarget("192.168.1.10:70000"))
        assertFalse(AppPreferences.isValidTarget("192.168.1.10:abc"))

        // IPv6 & [IPv6]:port
        assertTrue(AppPreferences.isValidTarget("::1"))
        assertTrue(AppPreferences.isValidTarget("[::1]:21118"))
        assertTrue(AppPreferences.isValidTarget("[2001:db8::1]:21118"))
        assertFalse(AppPreferences.isValidTarget("[::1]:0"))

        // Hostnames
        assertTrue(AppPreferences.isValidTarget("gaming-pc.local"))
        assertTrue(AppPreferences.isValidTarget("workstation.ts.net:21118"))
        assertFalse(AppPreferences.isValidTarget(""))
        assertFalse(AppPreferences.isValidTarget("   "))
        assertFalse(AppPreferences.isValidTarget("host name with spaces"))
        assertFalse(AppPreferences.isValidTarget("bad\nhost"))
        assertFalse(AppPreferences.isValidTarget("-leading-dash.local"))
    }

    @Test
    fun streamProfileDefaults_matchDesktopAndGamingSpec() {
        val desktop = StreamProfile.defaultDesktop()
        assertEquals(20_000, desktop.bitrateKbps)
        assertEquals(60, desktop.fpsCap)
        assertEquals(ScaleMode.FIT, desktop.scaleMode)
        assertEquals(TouchMode.TRACKPAD, desktop.touchMode)
        assertTrue(desktop.audioEnabled)
        assertTrue(desktop.clipboardEnabled)
        assertFalse(desktop.showHudOnConnect)
        assertTrue(desktop.preferYuv444)
        assertFalse(desktop.micEnabled)
        assertTrue(desktop.echoCancellation)
        assertTrue(desktop.noiseSuppression)
        assertEquals(StreamProfile.AUDIO_FRAME_LATENCY_AUTO, desktop.audioFrameLatencyMs)
        assertEquals(10_000, desktop.effectiveAudioFrameUs(StreamMode.DESKTOP))
        assertEquals(0, desktop.protocolAudioFrameUs())

        val gaming = StreamProfile.defaultGaming()
        assertEquals(50_000, gaming.bitrateKbps)
        assertEquals(120, gaming.fpsCap)
        assertEquals(ScaleMode.FIT, gaming.scaleMode)
        assertEquals(TouchMode.TRACKPAD, gaming.touchMode)
        assertTrue(gaming.audioEnabled)
        assertTrue(gaming.clipboardEnabled)
        assertTrue(gaming.showHudOnConnect)
        assertFalse(gaming.preferYuv444)
        assertFalse(gaming.micEnabled)
        assertTrue(gaming.echoCancellation)
        assertTrue(gaming.noiseSuppression)
        assertEquals(StreamProfile.AUDIO_FRAME_LATENCY_AUTO, gaming.audioFrameLatencyMs)
        assertEquals(5_000, gaming.effectiveAudioFrameUs(StreamMode.GAMING))
        assertEquals(0, gaming.protocolAudioFrameUs())
    }

    @Test
    fun audioFrameLatency_normalizationAndWireMicroseconds() {
        assertEquals(0, StreamProfile.normalizeAudioFrameLatencyMs(0))
        assertEquals(2, StreamProfile.normalizeAudioFrameLatencyMs(2))
        assertEquals(5, StreamProfile.normalizeAudioFrameLatencyMs(5))
        assertEquals(10, StreamProfile.normalizeAudioFrameLatencyMs(10))
        assertEquals(20, StreamProfile.normalizeAudioFrameLatencyMs(20))
        assertEquals(0, StreamProfile.normalizeAudioFrameLatencyMs(-1))
        assertEquals(0, StreamProfile.normalizeAudioFrameLatencyMs(7))
        assertEquals(0, StreamProfile.normalizeAudioFrameLatencyMs(99))

        val base = StreamProfile.defaultDesktop()
        assertEquals(2_500, base.copy(audioFrameLatencyMs = 2).protocolAudioFrameUs())
        assertEquals(5_000, base.copy(audioFrameLatencyMs = 5).protocolAudioFrameUs())
        assertEquals(10_000, base.copy(audioFrameLatencyMs = 10).protocolAudioFrameUs())
        assertEquals(20_000, base.copy(audioFrameLatencyMs = 20).protocolAudioFrameUs())
        assertEquals(2_500, base.copy(audioFrameLatencyMs = 2).effectiveAudioFrameUs(BridgeConstants.MODE_DESKTOP))
    }

    @Test
    fun resetProfileDefaults_andNewAudioAndHostPrefsRoundTripPersistence() {
        val backing = InMemorySharedPreferences()
        val prefs = AppPreferences(backing)

        // Verify fresh defaults
        val initial = prefs.settings.value
        assertEquals(StreamProfile.defaultDesktop(), initial.desktopProfile)
        assertEquals(StreamProfile.defaultGaming(), initial.gamingProfile)
        assertTrue(initial.hostAllowInput)
        assertTrue(initial.hostAllowClipboard)
        assertTrue(initial.hostAllowAudio)
        assertTrue(initial.hostAllowChat)

        // Customize both profiles and host default permissions
        prefs.updateProfile(BridgeConstants.MODE_DESKTOP) {
            it.copy(
                bitrateKbps = BitratePreset.LOW.kbps,
                fpsCap = 120,
                showHudOnConnect = true,
                preferYuv444 = false,
                micEnabled = true,
                echoCancellation = false,
                noiseSuppression = false,
                audioFrameLatencyMs = 20,
            )
        }
        prefs.updateProfile(BridgeConstants.MODE_GAMING) {
            it.copy(
                bitrateKbps = BitratePreset.ULTRA.kbps,
                fpsCap = 60,
                showHudOnConnect = false,
                preferYuv444 = true,
                micEnabled = true,
                echoCancellation = false,
                noiseSuppression = true,
                audioFrameLatencyMs = 2,
            )
        }
        prefs.setHostAllowInput(false)
        prefs.setHostAllowClipboard(false)
        prefs.setHostAllowAudio(false)
        prefs.setHostAllowChat(false)

        // Reload from backing SharedPreferences and verify complete roundtrip
        val reloaded = AppPreferences(backing).settings.value
        assertEquals(BitratePreset.LOW.kbps, reloaded.desktopProfile.bitrateKbps)
        assertEquals(120, reloaded.desktopProfile.fpsCap)
        assertTrue(reloaded.desktopProfile.showHudOnConnect)
        assertFalse(reloaded.desktopProfile.preferYuv444)
        assertTrue(reloaded.desktopProfile.micEnabled)
        assertFalse(reloaded.desktopProfile.echoCancellation)
        assertFalse(reloaded.desktopProfile.noiseSuppression)
        assertEquals(20, reloaded.desktopProfile.audioFrameLatencyMs)

        assertEquals(BitratePreset.ULTRA.kbps, reloaded.gamingProfile.bitrateKbps)
        assertEquals(60, reloaded.gamingProfile.fpsCap)
        assertFalse(reloaded.gamingProfile.showHudOnConnect)
        assertTrue(reloaded.gamingProfile.preferYuv444)
        assertTrue(reloaded.gamingProfile.micEnabled)
        assertFalse(reloaded.gamingProfile.echoCancellation)
        assertTrue(reloaded.gamingProfile.noiseSuppression)
        assertEquals(2, reloaded.gamingProfile.audioFrameLatencyMs)

        assertFalse(reloaded.hostAllowInput)
        assertFalse(reloaded.hostAllowClipboard)
        assertFalse(reloaded.hostAllowAudio)
        assertFalse(reloaded.hostAllowChat)

        // Reset Desktop defaults -> Gaming remains customized
        prefs.resetProfileDefaults(StreamMode.DESKTOP)
        val afterDesktopReset = AppPreferences(backing).settings.value
        assertEquals(StreamProfile.defaultDesktop(), afterDesktopReset.desktopProfile)
        assertEquals(BitratePreset.ULTRA.kbps, afterDesktopReset.gamingProfile.bitrateKbps)
        assertEquals(2, afterDesktopReset.gamingProfile.audioFrameLatencyMs)

        // Reset Gaming defaults -> Gaming restored to defaults
        prefs.resetProfileDefaults(StreamMode.GAMING)
        val afterGamingReset = AppPreferences(backing).settings.value
        assertEquals(StreamProfile.defaultGaming(), afterGamingReset.gamingProfile)
    }

    @Test
    fun recentConnections_dedupesCapsAtTwelveAndRoundTripsJson() {
        var list = emptyList<RecentConnection>()
        for (i in 1..15) {
            list = AppPreferences.upsertRecentConnection(
                existing = list,
                entry = RecentConnection(
                    target = "192.168.1.$i",
                    label = "PC-$i",
                    mode = if (i % 2 == 0) BridgeConstants.MODE_GAMING else BridgeConstants.MODE_DESKTOP,
                    lastUsedEpochMs = i * 1000L,
                ),
            )
        }
        assertEquals(AppPreferences.MAX_RECENT, list.size)
        assertEquals("192.168.1.15", list.first().target)

        // Re-adding an existing target moves it to the top without duplicating
        list = AppPreferences.upsertRecentConnection(
            existing = list,
            entry = RecentConnection("192.168.1.10", "Gaming Rig", BridgeConstants.MODE_GAMING, 99999L),
        )
        assertEquals(AppPreferences.MAX_RECENT, list.size)
        assertEquals("192.168.1.10", list.first().target)
        assertEquals("Gaming Rig", list.first().label)
        assertEquals(1, list.count { it.target == "192.168.1.10" })

        val json = AppPreferences.serializeRecentConnections(list)
        val restored = AppPreferences.deserializeRecentConnections(json)
        assertEquals(list, restored)

        assertTrue(AppPreferences.deserializeRecentConnections("not-json").isEmpty())
    }

    @Test
    fun themeMode_fromCodeAndCycleOrder() {
        assertEquals(AppThemeMode.SYSTEM, AppThemeMode.fromCode(null))
        assertEquals(AppThemeMode.SYSTEM, AppThemeMode.fromCode("unknown"))
        assertEquals(AppThemeMode.SYSTEM, AppThemeMode.fromCode("system"))
        assertEquals(AppThemeMode.DARK, AppThemeMode.fromCode("DARK"))
        assertEquals(AppThemeMode.LIGHT, AppThemeMode.fromCode("Light"))

        assertEquals(AppThemeMode.DARK, AppThemeMode.SYSTEM.next())
        assertEquals(AppThemeMode.LIGHT, AppThemeMode.DARK.next())
        assertEquals(AppThemeMode.SYSTEM, AppThemeMode.LIGHT.next())
    }

    @Test
    fun hostSecuritySettings_enforcePasswordModeInvariants() {
        val samplePhc = "\$argon2id\$v=19\$m=19456,t=2,p=1\$c2FsdA\$aGFzaA"

        // Default: ONE_TIME with no hash -> OTP enabled, effective hash empty
        val defaults = AppSettings()
        assertFalse(defaults.hasHostPermanentPassword)
        assertTrue(defaults.effectiveHostOtpEnabled)
        assertEquals("", defaults.effectiveHostPermanentHash)

        // ONE_TIME with saved PHC -> PHC preserved on disk, but NOT exposed to live host
        val oneTimeWithHash = defaults.copy(
            hostPasswordMode = HostPasswordMode.ONE_TIME.id,
            hostPermanentPasswordHash = samplePhc,
        )
        assertTrue(oneTimeWithHash.hasHostPermanentPassword)
        assertTrue(oneTimeWithHash.effectiveHostOtpEnabled)
        assertEquals("", oneTimeWithHash.effectiveHostPermanentHash)

        // PERMANENT without valid PHC -> falls back to OTP enabled, empty hash
        val permWithoutHash = defaults.copy(
            hostPasswordMode = HostPasswordMode.PERMANENT.id,
            hostPermanentPasswordHash = "not-a-valid-phc",
        )
        assertFalse(permWithoutHash.hasHostPermanentPassword)
        assertTrue(permWithoutHash.effectiveHostOtpEnabled)
        assertEquals("", permWithoutHash.effectiveHostPermanentHash)

        // PERMANENT with valid PHC -> OTP disabled, PHC exposed
        val permWithHash = defaults.copy(
            hostPasswordMode = HostPasswordMode.PERMANENT.id,
            hostPermanentPasswordHash = " $samplePhc ",
        )
        assertTrue(permWithHash.hasHostPermanentPassword)
        assertFalse(permWithHash.effectiveHostOtpEnabled)
        assertEquals(samplePhc, permWithHash.effectiveHostPermanentHash)

        // BOTH with valid PHC -> both OTP and permanent hash active
        val bothWithHash = defaults.copy(
            hostPasswordMode = HostPasswordMode.BOTH.id,
            hostPermanentPasswordHash = samplePhc,
        )
        assertTrue(bothWithHash.hasHostPermanentPassword)
        assertTrue(bothWithHash.effectiveHostOtpEnabled)
        assertEquals(samplePhc, bothWithHash.effectiveHostPermanentHash)
    }

    @Test
    fun hostTrustedPeers_upsertRemoveAndJsonRoundTrip() {
        val peerA = HostSavedTrustedPeer(
            publicKeyHex = "AA11BB22",
            deviceId = "123456789",
            name = "Work Laptop",
            fingerprint = "AB:CD:EF:01",
            allowInput = true,
            allowClipboard = true,
            allowAudio = false,
        )
        val peerB = HostSavedTrustedPeer(
            publicKeyHex = "cc33dd44",
            deviceId = "987654321",
            name = "Home PC",
            fingerprint = "11:22:33:44",
            allowInput = true,
            allowClipboard = false,
            allowAudio = true,
        )

        var list = AppPreferences.upsertTrustedPeer(emptyList(), peerA)
        list = AppPreferences.upsertTrustedPeer(list, peerB)
        assertEquals(2, list.size)
        assertEquals("cc33dd44", list[0].publicKeyHex)
        assertEquals("aa11bb22", list[1].publicKeyHex)

        // Case-insensitive upsert updates existing entry and moves it to front
        list = AppPreferences.upsertTrustedPeer(list, peerA.copy(name = "Workstation"))
        assertEquals(2, list.size)
        assertEquals("aa11bb22", list[0].publicKeyHex)
        assertEquals("Workstation", list[0].name)

        // Blank publicKeyHex is ignored
        val unchanged = AppPreferences.upsertTrustedPeer(list, peerA.copy(publicKeyHex = "   "))
        assertEquals(list, unchanged)

        // JSON round-trip preserves all peer fields
        val json = HostSavedTrustedPeer.listToJson(list)
        val restored = HostSavedTrustedPeer.listFromJson(json)
        assertEquals(list, restored)

        // Case-insensitive removal
        val afterRemove = AppPreferences.removeTrustedPeer(list, "AA11BB22")
        assertEquals(1, afterRemove.size)
        assertEquals("cc33dd44", afterRemove[0].publicKeyHex)
    }

    private class InMemorySharedPreferences : SharedPreferences {
        private val map = LinkedHashMap<String, Any?>()

        override fun getAll(): MutableMap<String, *> = LinkedHashMap(map)

        override fun getString(key: String?, defValue: String?): String? =
            (map[key] as? String) ?: defValue

        @Suppress("UNCHECKED_CAST")
        override fun getStringSet(key: String?, defValues: MutableSet<String>?): MutableSet<String>? =
            (map[key] as? Set<String>)?.toMutableSet() ?: defValues

        override fun getInt(key: String?, defValue: Int): Int =
            (map[key] as? Int) ?: defValue

        override fun getLong(key: String?, defValue: Long): Long =
            (map[key] as? Long) ?: defValue

        override fun getFloat(key: String?, defValue: Float): Float =
            (map[key] as? Float) ?: defValue

        override fun getBoolean(key: String?, defValue: Boolean): Boolean =
            (map[key] as? Boolean) ?: defValue

        override fun contains(key: String?): Boolean = map.containsKey(key)

        override fun edit(): SharedPreferences.Editor = object : SharedPreferences.Editor {
            private val pending = LinkedHashMap<String, Any?>()
            private val removals = LinkedHashSet<String>()
            private var clearAll = false

            override fun putString(key: String?, value: String?): SharedPreferences.Editor = apply {
                if (key != null) {
                    if (value == null) removals.add(key) else pending[key] = value
                }
            }

            override fun putStringSet(key: String?, values: MutableSet<String>?): SharedPreferences.Editor = apply {
                if (key != null) {
                    if (values == null) removals.add(key) else pending[key] = LinkedHashSet(values)
                }
            }

            override fun putInt(key: String?, value: Int): SharedPreferences.Editor = apply {
                if (key != null) pending[key] = value
            }

            override fun putLong(key: String?, value: Long): SharedPreferences.Editor = apply {
                if (key != null) pending[key] = value
            }

            override fun putFloat(key: String?, value: Float): SharedPreferences.Editor = apply {
                if (key != null) pending[key] = value
            }

            override fun putBoolean(key: String?, value: Boolean): SharedPreferences.Editor = apply {
                if (key != null) pending[key] = value
            }

            override fun remove(key: String?): SharedPreferences.Editor = apply {
                if (key != null) {
                    removals.add(key)
                    pending.remove(key)
                }
            }

            override fun clear(): SharedPreferences.Editor = apply {
                clearAll = true
                pending.clear()
                removals.clear()
            }

            override fun commit(): Boolean {
                apply()
                return true
            }

            override fun apply() {
                if (clearAll) map.clear()
                for (r in removals) map.remove(r)
                map.putAll(pending)
            }
        }

        override fun registerOnSharedPreferenceChangeListener(
            listener: SharedPreferences.OnSharedPreferenceChangeListener?,
        ) {}

        override fun unregisterOnSharedPreferenceChangeListener(
            listener: SharedPreferences.OnSharedPreferenceChangeListener?,
        ) {}
    }
}
