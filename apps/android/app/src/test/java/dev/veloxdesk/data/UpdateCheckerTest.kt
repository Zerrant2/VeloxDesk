package dev.veloxdesk.data

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class UpdateCheckerTest {
    @Test
    fun parseSemVer_validAndInvalid() {
        assertEquals(SemVer(0, 3, 0, null), UpdateChecker.parseSemVer("v0.3.0"))
        assertEquals(SemVer(1, 2, 3, "beta.1"), UpdateChecker.parseSemVer("V1.2.3-beta.1+build.42"))
        assertEquals(SemVer(2, 0, 10, null), UpdateChecker.parseSemVer("  2.0.10  "))

        assertNull(UpdateChecker.parseSemVer(""))
        assertNull(UpdateChecker.parseSemVer("0.3"))
        assertNull(UpdateChecker.parseSemVer("0.3.0.1"))
        assertNull(UpdateChecker.parseSemVer("0.3.0-"))
        assertNull(UpdateChecker.parseSemVer("v0.3.x"))
    }

    @Test
    fun isNewerVersion_semverPrecedence() {
        assertTrue(UpdateChecker.isNewerVersion("0.3.0", "0.3.1"))
        assertTrue(UpdateChecker.isNewerVersion("0.3.0", "v0.4.0"))
        assertTrue(UpdateChecker.isNewerVersion("0.3.0", "1.0.0"))
        assertFalse(UpdateChecker.isNewerVersion("0.3.0", "0.3.0"))
        assertFalse(UpdateChecker.isNewerVersion("0.3.0", "v0.3.0+sha123"))
        assertFalse(UpdateChecker.isNewerVersion("0.3.1", "0.3.0"))

        // Prerelease < stable
        assertTrue(UpdateChecker.isNewerVersion("0.3.0-rc.1", "0.3.0"))
        assertFalse(UpdateChecker.isNewerVersion("0.3.0", "0.3.0-rc.1"))

        // Prerelease numeric & lexical ordering
        assertTrue(UpdateChecker.isNewerVersion("0.3.0-alpha.2", "0.3.0-alpha.10"))
        assertTrue(UpdateChecker.isNewerVersion("0.3.0-alpha.1", "0.3.0-beta.1"))
        assertTrue(UpdateChecker.isNewerVersion("0.3.0-1", "0.3.0-alpha"))
        assertFalse(UpdateChecker.isNewerVersion("0.3.0-alpha", "0.3.0-1"))
        assertTrue(UpdateChecker.isNewerVersion("0.3.0-rc", "0.3.0-rc.1"))

        // Invalid inputs never claim an update
        assertFalse(UpdateChecker.isNewerVersion("invalid", "0.4.0"))
        assertFalse(UpdateChecker.isNewerVersion("0.3.0", "invalid"))
    }

    @Test
    fun isAllowedGitHubUrl_acceptsOnlyExactHttpsAllowedHosts() {
        assertTrue(UpdateChecker.isAllowedGitHubUrl("https://api.github.com/repos/Zerrant2/VeloxDesk/releases/latest"))
        assertTrue(UpdateChecker.isAllowedGitHubUrl("https://github.com/Zerrant2/VeloxDesk/releases/download/v0.4.0/veloxdesk-android.apk"))
        assertTrue(UpdateChecker.isAllowedGitHubUrl("https://objects.githubusercontent.com/github-production-release-asset/123"))
        assertTrue(UpdateChecker.isAllowedGitHubUrl("https://release-assets.githubusercontent.com/abc.apk"))
        assertTrue(UpdateChecker.isAllowedGitHubUrl("https://github-releases.githubusercontent.com/abc.apk"))
        assertTrue(UpdateChecker.isAllowedGitHubUrl("https://GITHUB.COM:443/Zerrant2/VeloxDesk"))

        // Reject HTTP, evil hosts, userinfo, non-443 ports, whitespace/control chars
        assertFalse(UpdateChecker.isAllowedGitHubUrl("http://github.com/Zerrant2/VeloxDesk"))
        assertFalse(UpdateChecker.isAllowedGitHubUrl("https://evil.com/veloxdesk-android.apk"))
        assertFalse(UpdateChecker.isAllowedGitHubUrl("https://github.com.evil.com/veloxdesk-android.apk"))
        assertFalse(UpdateChecker.isAllowedGitHubUrl("https://sub.github.com/veloxdesk-android.apk"))
        assertFalse(UpdateChecker.isAllowedGitHubUrl("https://github.com@evil.com/veloxdesk-android.apk"))
        assertFalse(UpdateChecker.isAllowedGitHubUrl("https://user:pass@github.com/Zerrant2/VeloxDesk"))
        assertFalse(UpdateChecker.isAllowedGitHubUrl("https://github.com:8443/Zerrant2/VeloxDesk"))
        assertFalse(UpdateChecker.isAllowedGitHubUrl("https://github.com/Zerrant2/VeloxDesk "))
        assertFalse(UpdateChecker.isAllowedGitHubUrl("https://github.com/Zerrant2/\nVeloxDesk"))
        assertFalse(UpdateChecker.isAllowedGitHubUrl("https://[::1]/Zerrant2/VeloxDesk"))
        assertFalse(UpdateChecker.isAllowedGitHubUrl(""))
    }

    @Test
    fun parseReleaseJson_prefersNamedAndroidApkAndFiltersEvilAssets() {
        val json = """
            {
              "tag_name": "v0.4.0",
              "html_url": "https://github.com/Zerrant2/VeloxDesk/releases/tag/v0.4.0",
              "body": "Android release notes",
              "assets": [
                {
                  "name": "veloxdesk.exe",
                  "browser_download_url": "https://github.com/Zerrant2/VeloxDesk/releases/download/v0.4.0/veloxdesk.exe"
                },
                {
                  "name": "evil.apk",
                  "browser_download_url": "https://evil.example.com/evil.apk"
                },
                {
                  "name": "other-viewer.apk",
                  "browser_download_url": "https://github.com/Zerrant2/VeloxDesk/releases/download/v0.4.0/other-viewer.apk"
                },
                {
                  "name": "veloxdesk-android.apk",
                  "browser_download_url": "https://github.com/Zerrant2/VeloxDesk/releases/download/v0.4.0/veloxdesk-android.apk"
                }
              ]
            }
        """.trimIndent()

        val release = UpdateChecker.parseReleaseJson(json)
        assertNotNull(release)
        assertEquals("v0.4.0", release!!.tagName)
        assertEquals("0.4.0", release.version)
        assertEquals("veloxdesk-android.apk", release.apkName)
        assertEquals(
            "https://github.com/Zerrant2/VeloxDesk/releases/download/v0.4.0/veloxdesk-android.apk",
            release.apkUrl,
        )
        assertEquals("Android release notes", release.notes)
    }

    @Test
    fun parseReleaseJson_fallsBackToAnyAllowedApk() {
        val json = """
            {
              "tag_name": "v0.4.1",
              "html_url": "https://github.com/Zerrant2/VeloxDesk/releases/tag/v0.4.1",
              "body": "",
              "assets": [
                {
                  "name": "malicious.apk",
                  "browser_download_url": "http://github.com/Zerrant2/VeloxDesk/releases/download/v0.4.1/malicious.apk"
                },
                {
                  "name": "VeloxDesk-0.4.1-arm64.apk",
                  "browser_download_url": "https://objects.githubusercontent.com/VeloxDesk-0.4.1-arm64.apk"
                }
              ]
            }
        """.trimIndent()

        val release = UpdateChecker.parseReleaseJson(json)
        assertNotNull(release)
        assertEquals("VeloxDesk-0.4.1-arm64.apk", release!!.apkName)
        assertEquals("https://objects.githubusercontent.com/VeloxDesk-0.4.1-arm64.apk", release.apkUrl)
    }
}
