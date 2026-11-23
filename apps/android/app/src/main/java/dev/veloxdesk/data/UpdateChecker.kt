package dev.veloxdesk.data

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import org.json.JSONObject
import java.io.IOException
import java.net.HttpURLConnection
import java.net.URL

data class SemVer(
    val major: Long,
    val minor: Long,
    val patch: Long,
    val prerelease: String? = null,
)

data class ReleaseInfo(
    val tagName: String,
    val version: String,
    val htmlUrl: String,
    val apkUrl: String?,
    val apkName: String?,
    val notes: String,
)

sealed interface UpdateCheckResult {
    data class UpToDate(val currentVersion: String, val latestVersion: String) : UpdateCheckResult
    data class UpdateAvailable(val release: ReleaseInfo) : UpdateCheckResult
    data class Error(val message: String) : UpdateCheckResult
}

/**
 * Pure-Kotlin GitHub Releases update checker with strict SemVer 2.0.0 comparison
 * and an exact GitHub HTTPS host allowlist (matching `apps/client/src/updater.rs`).
 */
object UpdateChecker {
    const val RELEASES_LATEST_URL = "https://api.github.com/repos/Zerrant2/VeloxDesk/releases/latest"
    private const val TIMEOUT_MS = 10_000
    private const val MAX_REDIRECTS = 5
    private const val MAX_JSON_BYTES = 1024 * 1024

    val ALLOWED_GITHUB_HOSTS: Set<String> = setOf(
        "github.com",
        "api.github.com",
        "objects.githubusercontent.com",
        "release-assets.githubusercontent.com",
        "github-releases.githubusercontent.com",
    )

    fun parseSemVer(raw: String): SemVer? {
        val trimmed = raw.trim()
        val withoutV = when {
            trimmed.startsWith('v') || trimmed.startsWith('V') -> trimmed.substring(1)
            else -> trimmed
        }
        val withoutBuild = withoutV.substringBefore('+')
        val dashIdx = withoutBuild.indexOf('-')
        val core: String
        val pre: String?
        if (dashIdx >= 0) {
            core = withoutBuild.substring(0, dashIdx)
            val prePart = withoutBuild.substring(dashIdx + 1)
            if (prePart.isEmpty()) return null
            pre = prePart
        } else {
            core = withoutBuild
            pre = null
        }

        val parts = core.split('.')
        if (parts.size != 3) return null
        val major = parseUint(parts[0]) ?: return null
        val minor = parseUint(parts[1]) ?: return null
        val patch = parseUint(parts[2]) ?: return null
        return SemVer(major, minor, patch, pre)
    }

    private fun parseUint(s: String): Long? {
        if (s.isEmpty() || !s.all { it in '0'..'9' }) return null
        return s.toLongOrNull()
    }

    private fun comparePrerelease(a: String, b: String): Int {
        val aParts = a.split('.')
        val bParts = b.split('.')
        val len = maxOf(aParts.size, bParts.size)
        for (i in 0 until len) {
            if (i >= aParts.size) return -1
            if (i >= bParts.size) return 1
            val pa = aParts[i]
            val pb = bParts[i]
            val na = parseUint(pa)
            val nb = parseUint(pb)
            val cmp = when {
                na != null && nb != null -> na.compareTo(nb)
                na != null && nb == null -> -1 // numeric < non-numeric per SemVer 2.0.0
                na == null && nb != null -> 1
                else -> pa.compareTo(pb)
            }
            if (cmp != 0) return cmp
        }
        return 0
    }

    /**
     * Returns `true` iff [candidate] is strictly newer than [current] per SemVer 2.0.0.
     */
    fun isNewerVersion(current: String, candidate: String): Boolean {
        val cur = parseSemVer(current) ?: return false
        val cand = parseSemVer(candidate) ?: return false
        if (cand.major != cur.major) return cand.major > cur.major
        if (cand.minor != cur.minor) return cand.minor > cur.minor
        if (cand.patch != cur.patch) return cand.patch > cur.patch
        return when {
            cur.prerelease == null && cand.prerelease == null -> false
            cur.prerelease != null && cand.prerelease == null -> true  // prerelease < stable
            cur.prerelease == null && cand.prerelease != null -> false // stable > prerelease
            else -> comparePrerelease(cand.prerelease!!, cur.prerelease!!) > 0
        }
    }

    /**
     * Strictly validates that [url] is `https://` on an exact allowed GitHub host,
     * port 443 only (or default), with no userinfo (`@`), whitespace, or control chars.
     */
    fun isAllowedGitHubUrl(url: String): Boolean {
        if (url.isEmpty()) return false
        if (url.any { it.isWhitespace() || it.isISOControl() }) return false
        if (!url.startsWith("https://")) return false

        val afterScheme = url.substring("https://".length)
        val authEnd = afterScheme.indexOfAny(charArrayOf('/', '?', '#')).let {
            if (it == -1) afterScheme.length else it
        }
        val authority = afterScheme.substring(0, authEnd)
        if (authority.isEmpty() || authority.contains('@')) return false

        val host: String
        val port: Int?
        if (authority.startsWith('[')) {
            // IPv6 literals are never in ALLOWED_GITHUB_HOSTS
            return false
        } else {
            val colonIdx = authority.lastIndexOf(':')
            if (colonIdx >= 0) {
                host = authority.substring(0, colonIdx)
                val portStr = authority.substring(colonIdx + 1)
                if (host.isEmpty() || host.contains(':') || !portStr.all { it in '0'..'9' }) {
                    return false
                }
                port = portStr.toIntOrNull() ?: return false
            } else {
                host = authority
                port = null
            }
        }

        if (port != null && port != 443) return false
        return ALLOWED_GITHUB_HOSTS.contains(host.lowercase())
    }

    private fun resolveRedirectUrl(baseUrl: String, location: String): String? {
        val loc = location.trim()
        if (loc.startsWith("https://") || loc.startsWith("http://")) return loc
        if (loc.startsWith("//")) return "https:$loc"
        if (loc.startsWith('/')) {
            val schemeEnd = baseUrl.indexOf("://")
            if (schemeEnd < 0) return null
            val afterScheme = baseUrl.substring(schemeEnd + 3)
            val authEnd = afterScheme.indexOfAny(charArrayOf('/', '?', '#')).let {
                if (it == -1) afterScheme.length else it
            }
            val origin = baseUrl.substring(0, schemeEnd + 3 + authEnd)
            return "$origin$loc"
        }
        return null
    }

    /**
     * Parses a GitHub `/releases/latest` JSON payload into [ReleaseInfo].
     * Prefers `veloxdesk-android.apk` (case-insensitive), falling back to the first `.apk` asset.
     */
    fun parseReleaseJson(jsonText: String): ReleaseInfo? {
        val root = try {
            JSONObject(jsonText)
        } catch (_: Throwable) {
            return null
        }

        val tagName = root.optString("tag_name", "").trim()
        if (tagName.isEmpty()) return null
        val parsed = parseSemVer(tagName) ?: return null
        val version = buildString {
            append("${parsed.major}.${parsed.minor}.${parsed.patch}")
            if (parsed.prerelease != null) {
                append('-').append(parsed.prerelease)
            }
        }

        val rawHtmlUrl = root.optString("html_url", "").trim()
        val htmlUrl = if (isAllowedGitHubUrl(rawHtmlUrl)) {
            rawHtmlUrl
        } else {
            "https://github.com/Zerrant2/VeloxDesk/releases/tag/$tagName"
        }
        if (!isAllowedGitHubUrl(htmlUrl)) return null

        val notes = root.optString("body", "").trim()

        var preferredApk: Pair<String, String>? = null
        var fallbackApk: Pair<String, String>? = null

        val assets = root.optJSONArray("assets")
        if (assets != null) {
            for (i in 0 until assets.length()) {
                val asset = assets.optJSONObject(i) ?: continue
                val name = asset.optString("name", "").trim()
                val downloadUrl = asset.optString("browser_download_url", "").trim()
                if (!name.endsWith(".apk", ignoreCase = true)) continue
                if (!isAllowedGitHubUrl(downloadUrl)) continue
                if (name.equals("veloxdesk-android.apk", ignoreCase = true)) {
                    preferredApk = name to downloadUrl
                    break
                }
                if (fallbackApk == null) {
                    fallbackApk = name to downloadUrl
                }
            }
        }

        val selected = preferredApk ?: fallbackApk
        return ReleaseInfo(
            tagName = tagName,
            version = version,
            htmlUrl = htmlUrl,
            apkUrl = selected?.second,
            apkName = selected?.first,
            notes = notes,
        )
    }

    suspend fun checkLatestRelease(currentVersion: String): UpdateCheckResult = withContext(Dispatchers.IO) {
        try {
            val body = fetchAllowedHttpsJson(RELEASES_LATEST_URL, "VeloxDesk-Android/$currentVersion")
            val release = parseReleaseJson(body)
                ?: return@withContext UpdateCheckResult.Error("Invalid GitHub release metadata")
            if (isNewerVersion(currentVersion, release.tagName)) {
                UpdateCheckResult.UpdateAvailable(release)
            } else {
                UpdateCheckResult.UpToDate(currentVersion = currentVersion, latestVersion = release.version)
            }
        } catch (e: Throwable) {
            UpdateCheckResult.Error(e.message ?: e.javaClass.simpleName)
        }
    }

    private fun fetchAllowedHttpsJson(initialUrl: String, userAgent: String): String {
        var currentUrl = initialUrl
        for (hop in 0..MAX_REDIRECTS) {
            if (!isAllowedGitHubUrl(currentUrl)) {
                throw IOException("Blocked disallowed update URL host")
            }
            val conn = (URL(currentUrl).openConnection() as HttpURLConnection).apply {
                instanceFollowRedirects = false
                requestMethod = "GET"
                connectTimeout = TIMEOUT_MS
                readTimeout = TIMEOUT_MS
                setRequestProperty("Accept", "application/vnd.github+json")
                setRequestProperty("User-Agent", userAgent)
            }
            try {
                val code = conn.responseCode
                if (code in 301..308) {
                    val location = conn.getHeaderField("Location")
                        ?: throw IOException("Redirect without Location header")
                    currentUrl = resolveRedirectUrl(currentUrl, location)
                        ?: throw IOException("Unsupported redirect URL")
                    if (hop == MAX_REDIRECTS) {
                        throw IOException("Too many redirects")
                    }
                    continue
                }
                if (code !in 200..299) {
                    throw IOException("HTTP $code")
                }
                conn.inputStream.use { stream ->
                    val buf = ByteArray(8192)
                    val out = java.io.ByteArrayOutputStream()
                    var total = 0
                    while (true) {
                        val read = stream.read(buf)
                        if (read <= 0) break
                        total += read
                        if (total > MAX_JSON_BYTES) {
                            throw IOException("Response too large")
                        }
                        out.write(buf, 0, read)
                    }
                    return out.toString(Charsets.UTF_8.name())
                }
            } finally {
                conn.disconnect()
            }
        }
        throw IOException("Too many redirects")
    }
}
