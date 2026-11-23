package dev.veloxdesk.ui

import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.content.Intent
import android.net.Uri
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.rounded.Chat
import androidx.compose.material.icons.automirrored.rounded.ScreenShare
import androidx.compose.material.icons.automirrored.rounded.StopScreenShare
import androidx.compose.material.icons.rounded.BrightnessAuto
import androidx.compose.material.icons.rounded.CheckCircle
import androidx.compose.material.icons.rounded.Clear
import androidx.compose.material.icons.rounded.Close
import androidx.compose.material.icons.rounded.Computer
import androidx.compose.material.icons.rounded.ContentCopy
import androidx.compose.material.icons.rounded.DarkMode
import androidx.compose.material.icons.rounded.DeleteOutline
import androidx.compose.material.icons.rounded.DesktopWindows
import androidx.compose.material.icons.rounded.Download
import androidx.compose.material.icons.rounded.History
import androidx.compose.material.icons.rounded.LightMode
import androidx.compose.material.icons.rounded.Lock
import androidx.compose.material.icons.rounded.PlayArrow
import androidx.compose.material.icons.rounded.Refresh
import androidx.compose.material.icons.rounded.Settings
import androidx.compose.material.icons.rounded.SportsEsports
import androidx.compose.material.icons.rounded.TouchApp
import androidx.compose.material.icons.rounded.WarningAmber
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.FilterChip
import androidx.compose.material3.FilterChipDefaults
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.OutlinedTextFieldDefaults
import androidx.compose.material3.Scaffold
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.veloxdesk.R
import dev.veloxdesk.bridge.BridgeConstants
import dev.veloxdesk.bridge.HostPasswordMode
import dev.veloxdesk.bridge.HostUiState
import dev.veloxdesk.data.AppLanguage
import dev.veloxdesk.data.AppPreferences
import dev.veloxdesk.data.AppSettings
import dev.veloxdesk.data.AppThemeMode
import dev.veloxdesk.data.RecentConnection
import dev.veloxdesk.data.ReleaseInfo
import dev.veloxdesk.data.UpdateChecker
import dev.veloxdesk.ui.i18n.str
import dev.veloxdesk.ui.theme.VeloxTheme
import kotlinx.coroutines.launch

@Composable
fun ConnectScreen(
    settings: AppSettings,
    hostState: HostUiState,
    ownDeviceId: String,
    ownFingerprint: String,
    buildDisplay: String,
    nativeLoaded: Boolean,
    nativeLoadError: String?,
    availableUpdate: ReleaseInfo?,
    checkingUpdate: Boolean,
    lastEndedReason: String?,
    lastEndedWasError: Boolean,
    onDismissEndedReason: () -> Unit,
    onLanguageChange: (AppLanguage) -> Unit,
    onCycleTheme: () -> Unit,
    onOpenSettings: () -> Unit,
    onStartSharing: () -> Unit,
    onStopSharing: () -> Unit,
    onRotateOtp: () -> Unit,
    onDismissHostError: () -> Unit,
    onEnableAccessibility: () -> Unit,
    onOpenHostChat: () -> Unit,
    onModeSelected: (Int) -> Unit,
    onConnect: (target: String, mode: Int) -> Unit,
    onRemoveRecent: (target: String) -> Unit,
    onCheckUpdates: () -> Unit,
    snackbarHostState: SnackbarHostState,
) {
    val colors = VeloxTheme.colors
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    var targetInput by rememberSaveable { mutableStateOf("") }
    var targetError by remember { mutableStateOf(false) }

    val effectiveDeviceId = hostState.deviceId.ifBlank { ownDeviceId }
    val formattedOwnId = remember(effectiveDeviceId) {
        if (effectiveDeviceId.isBlank()) "" else AppPreferences.formatDeviceId(effectiveDeviceId)
    }
    val needsRendezvous = AppPreferences.isNineDigitDeviceId(targetInput) && settings.rendezvousServer.isBlank()
    val copiedIdMsg = str(R.string.copied_device_id)
    val copiedFpMsg = str(R.string.copied_fingerprint)
    val copiedOtpMsg = str(R.string.copied_otp)
    val copiedIpMsg = str(R.string.copied_ip)

    fun copyToClipboard(label: String, text: String, toastMessage: String) {
        val cm = context.getSystemService(Context.CLIPBOARD_SERVICE) as? ClipboardManager
        cm?.setPrimaryClip(ClipData.newPlainText(label, text))
        scope.launch {
            snackbarHostState.currentSnackbarData?.dismiss()
            snackbarHostState.showSnackbar(toastMessage)
        }
    }

    fun attemptConnect() {
        if (!AppPreferences.isValidTarget(targetInput)) {
            targetError = true
            return
        }
        targetError = false
        onConnect(targetInput.trim(), settings.lastMode)
    }

    Scaffold(
        containerColor = colors.background,
        snackbarHost = { SnackbarHost(snackbarHostState) },
        topBar = {
            ConnectTopBar(
                language = settings.language,
                themeMode = settings.themeMode,
                nativeLoaded = nativeLoaded,
                onLanguageChange = onLanguageChange,
                onCycleTheme = onCycleTheme,
                onOpenSettings = onOpenSettings,
            )
        },
        bottomBar = {
            ConnectBottomBar(
                buildDisplay = buildDisplay,
                checkingUpdate = checkingUpdate,
                onCheckUpdates = onCheckUpdates,
            )
        },
    ) { innerPadding ->
        Column(
            modifier = Modifier
                .fillMaxSize()
                .padding(innerPadding)
                .verticalScroll(rememberScrollState())
                .padding(horizontal = 16.dp, vertical = 12.dp),
            verticalArrangement = Arrangement.spacedBy(14.dp),
        ) {
            if (!nativeLoaded) {
                AlertBanner(
                    text = str(R.string.err_native_unavailable, nativeLoadError ?: "unknown"),
                    isError = true,
                )
            }

            if (lastEndedReason != null) {
                AlertBanner(
                    text = if (lastEndedWasError) {
                        str(R.string.session_closed_error, lastEndedReason)
                    } else {
                        lastEndedReason
                    },
                    isError = lastEndedWasError,
                    onDismiss = onDismissEndedReason,
                )
            }

            if (availableUpdate != null) {
                UpdateAvailableBanner(
                    release = availableUpdate,
                    onOpenRelease = {
                        val url = availableUpdate.apkUrl ?: availableUpdate.htmlUrl
                        if (UpdateChecker.isAllowedGitHubUrl(url)) {
                            runCatching {
                                context.startActivity(Intent(Intent.ACTION_VIEW, Uri.parse(url)))
                            }
                        }
                    },
                )
            }

            // "Share This Device" card (Android Host Mode)
            ShareThisDeviceCard(
                settings = settings,
                hostState = hostState,
                formattedDeviceId = formattedOwnId,
                fingerprint = ownFingerprint,
                nativeLoaded = nativeLoaded,
                onCopyDeviceId = {
                    if (effectiveDeviceId.isNotEmpty()) {
                        copyToClipboard("VeloxDesk ID", effectiveDeviceId, copiedIdMsg)
                    }
                },
                onCopyFingerprint = {
                    if (ownFingerprint.isNotEmpty()) {
                        copyToClipboard("VeloxDesk Fingerprint", ownFingerprint, copiedFpMsg)
                    }
                },
                onCopyIp = { ip ->
                    copyToClipboard("VeloxDesk IP", ip, copiedIpMsg)
                },
                onCopyOtp = { otp ->
                    copyToClipboard("VeloxDesk OTP", otp, copiedOtpMsg)
                },
                onRotateOtp = onRotateOtp,
                onStartSharing = onStartSharing,
                onStopSharing = onStopSharing,
                onDismissHostError = onDismissHostError,
                onEnableAccessibility = onEnableAccessibility,
                onOpenHostChat = onOpenHostChat,
            )

            // "Connect to Remote Device" card
            RemotePcCard(
                targetInput = targetInput,
                onTargetChange = {
                    targetInput = it
                    if (targetError) targetError = false
                },
                targetError = targetError,
                selectedMode = settings.lastMode,
                onModeSelected = onModeSelected,
                needsRendezvous = needsRendezvous,
                nativeLoaded = nativeLoaded,
                onOpenSettings = onOpenSettings,
                onConnect = ::attemptConnect,
            )

            // "Recent Connections" card
            RecentConnectionsCard(
                recent = settings.recentConnections,
                onConnectRecent = { entry ->
                    targetInput = AppPreferences.formatDisplayTarget(entry.target)
                    onModeSelected(entry.mode)
                    onConnect(entry.target, entry.mode)
                },
                onRemoveRecent = onRemoveRecent,
            )

            Spacer(modifier = Modifier.height(8.dp))
        }
    }
}

@Composable
private fun ConnectTopBar(
    language: AppLanguage,
    themeMode: AppThemeMode,
    nativeLoaded: Boolean,
    onLanguageChange: (AppLanguage) -> Unit,
    onCycleTheme: () -> Unit,
    onOpenSettings: () -> Unit,
) {
    val colors = VeloxTheme.colors
    Surface(
        color = colors.background,
        modifier = Modifier
            .fillMaxWidth()
            .statusBarsPadding(),
    ) {
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .padding(horizontal = 16.dp, vertical = 12.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Box(
                modifier = Modifier
                    .size(34.dp)
                    .clip(RoundedCornerShape(10.dp))
                    .background(colors.surface),
                contentAlignment = Alignment.Center,
            ) {
                Icon(
                    imageVector = Icons.Rounded.DesktopWindows,
                    contentDescription = null,
                    tint = colors.primary,
                    modifier = Modifier.size(20.dp),
                )
            }
            Spacer(modifier = Modifier.width(10.dp))
            Column(modifier = Modifier.weight(1f)) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Text(
                        text = str(R.string.app_name),
                        style = MaterialTheme.typography.titleLarge,
                        color = colors.textPrimary,
                    )
                    Spacer(modifier = Modifier.width(8.dp))
                    Box(
                        modifier = Modifier
                            .size(8.dp)
                            .clip(CircleShape)
                            .background(if (nativeLoaded) colors.success else colors.danger),
                    )
                }
            }

            // Quick theme cycle button (System -> Dark -> Light)
            IconButton(onClick = onCycleTheme) {
                Icon(
                    imageVector = when (themeMode) {
                        AppThemeMode.SYSTEM -> Icons.Rounded.BrightnessAuto
                        AppThemeMode.DARK -> Icons.Rounded.DarkMode
                        AppThemeMode.LIGHT -> Icons.Rounded.LightMode
                    },
                    contentDescription = str(R.string.btn_cycle_theme),
                    tint = colors.textPrimary,
                )
            }

            // EN / RU compact segmented pill
            Surface(
                color = colors.surface,
                shape = RoundedCornerShape(999.dp),
                border = BorderStroke(1.dp, colors.border),
            ) {
                Row(modifier = Modifier.padding(2.dp)) {
                    AppLanguage.entries.forEach { lang ->
                        val selected = lang == language
                        Box(
                            modifier = Modifier
                                .clip(RoundedCornerShape(999.dp))
                                .background(if (selected) colors.primary else Color.Transparent)
                                .clickable { onLanguageChange(lang) }
                                .padding(horizontal = 10.dp, vertical = 5.dp),
                        ) {
                            Text(
                                text = lang.shortLabel,
                                style = MaterialTheme.typography.labelMedium,
                                fontWeight = FontWeight.Bold,
                                color = if (selected) colors.onPrimary else colors.textSecondary,
                            )
                        }
                    }
                }
            }

            Spacer(modifier = Modifier.width(4.dp))
            IconButton(onClick = onOpenSettings) {
                Icon(
                    imageVector = Icons.Rounded.Settings,
                    contentDescription = str(R.string.btn_settings),
                    tint = colors.textPrimary,
                )
            }
        }
    }
}

@Composable
private fun AlertBanner(
    text: String,
    isError: Boolean,
    onDismiss: (() -> Unit)? = null,
) {
    val colors = VeloxTheme.colors
    Surface(
        color = if (isError) colors.dangerContainer else colors.surfaceElevated,
        shape = RoundedCornerShape(12.dp),
        border = BorderStroke(1.dp, if (isError) colors.danger.copy(alpha = 0.5f) else colors.border),
        modifier = Modifier.fillMaxWidth(),
    ) {
        Row(
            modifier = Modifier.padding(horizontal = 12.dp, vertical = 10.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Icon(
                imageVector = Icons.Rounded.WarningAmber,
                contentDescription = null,
                tint = if (isError) colors.danger else colors.warning,
                modifier = Modifier.size(20.dp),
            )
            Spacer(modifier = Modifier.width(10.dp))
            Text(
                text = text,
                style = MaterialTheme.typography.bodySmall,
                color = colors.textPrimary,
                modifier = Modifier.weight(1f),
            )
            if (onDismiss != null) {
                IconButton(onClick = onDismiss, modifier = Modifier.size(28.dp)) {
                    Icon(
                        imageVector = Icons.Rounded.Close,
                        contentDescription = str(R.string.btn_close),
                        tint = colors.textSecondary,
                        modifier = Modifier.size(16.dp),
                    )
                }
            }
        }
    }
}

@Composable
private fun UpdateAvailableBanner(
    release: ReleaseInfo,
    onOpenRelease: () -> Unit,
) {
    val colors = VeloxTheme.colors
    Surface(
        color = colors.primaryBannerBg,
        shape = RoundedCornerShape(14.dp),
        border = BorderStroke(1.dp, colors.primary.copy(alpha = 0.6f)),
        modifier = Modifier.fillMaxWidth(),
    ) {
        Row(
            modifier = Modifier.padding(horizontal = 14.dp, vertical = 10.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Icon(
                imageVector = Icons.Rounded.Download,
                contentDescription = null,
                tint = colors.primary,
                modifier = Modifier.size(20.dp),
            )
            Spacer(modifier = Modifier.width(10.dp))
            Column(modifier = Modifier.weight(1f)) {
                Text(
                    text = str(R.string.update_available_title, release.tagName),
                    style = MaterialTheme.typography.titleMedium,
                    color = colors.textPrimary,
                )
                if (release.apkName != null) {
                    Text(
                        text = release.apkName,
                        style = MaterialTheme.typography.bodySmall,
                        color = colors.textSecondary,
                    )
                }
            }
            TextButton(onClick = onOpenRelease) {
                Text(
                    text = str(R.string.btn_open_release),
                    color = colors.primary,
                    fontWeight = FontWeight.Bold,
                )
            }
        }
    }
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun ShareThisDeviceCard(
    settings: AppSettings,
    hostState: HostUiState,
    formattedDeviceId: String,
    fingerprint: String,
    nativeLoaded: Boolean,
    onCopyDeviceId: () -> Unit,
    onCopyFingerprint: () -> Unit,
    onCopyIp: (String) -> Unit,
    onCopyOtp: (String) -> Unit,
    onRotateOtp: () -> Unit,
    onStartSharing: () -> Unit,
    onStopSharing: () -> Unit,
    onDismissHostError: () -> Unit,
    onEnableAccessibility: () -> Unit,
    onOpenHostChat: () -> Unit,
) {
    val colors = VeloxTheme.colors
    val statusText = when {
        hostState.activePeerName != null -> str(R.string.host_status_connected, hostState.activePeerName)
        hostState.isRunning -> str(R.string.host_status_sharing)
        else -> str(R.string.host_status_offline)
    }
    val statusBg = when {
        hostState.activePeerName != null -> colors.successContainer
        hostState.isRunning -> colors.primaryBannerBg
        else -> colors.background
    }
    val statusFg = when {
        hostState.activePeerName != null -> colors.success
        hostState.isRunning -> colors.primary
        else -> colors.textMuted
    }

    Card(
        colors = CardDefaults.cardColors(containerColor = colors.surface),
        border = BorderStroke(
            width = 1.dp,
            color = if (hostState.isRunning) colors.primary.copy(alpha = 0.55f) else colors.border,
        ),
        shape = RoundedCornerShape(16.dp),
        modifier = Modifier.fillMaxWidth(),
    ) {
        Column(modifier = Modifier.padding(16.dp)) {
            Row(
                modifier = Modifier.fillMaxWidth(),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Text(
                    text = str(R.string.this_device_title),
                    style = MaterialTheme.typography.titleMedium,
                    color = colors.textPrimary,
                    modifier = Modifier.weight(1f),
                )
                Surface(
                    color = statusBg,
                    shape = RoundedCornerShape(999.dp),
                    border = BorderStroke(1.dp, statusFg.copy(alpha = 0.45f)),
                ) {
                    Row(
                        modifier = Modifier.padding(horizontal = 10.dp, vertical = 4.dp),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        Box(
                            modifier = Modifier
                                .size(7.dp)
                                .clip(CircleShape)
                                .background(statusFg),
                        )
                        Spacer(modifier = Modifier.width(6.dp))
                        Text(
                            text = statusText,
                            style = MaterialTheme.typography.labelSmall,
                            fontWeight = FontWeight.SemiBold,
                            color = statusFg,
                            maxLines = 1,
                            overflow = TextOverflow.Ellipsis,
                        )
                    }
                }
            }
            Spacer(modifier = Modifier.height(2.dp))
            Text(
                text = str(R.string.this_device_subtitle),
                style = MaterialTheme.typography.bodySmall,
                color = colors.textSecondary,
            )

            if (hostState.error != null) {
                Spacer(modifier = Modifier.height(10.dp))
                AlertBanner(
                    text = hostState.error,
                    isError = true,
                    onDismiss = onDismissHostError,
                )
            }

            Spacer(modifier = Modifier.height(12.dp))

            // 9-digit Device ID row
            Row(
                modifier = Modifier
                    .fillMaxWidth()
                    .clip(RoundedCornerShape(12.dp))
                    .background(colors.background)
                    .padding(horizontal = 14.dp, vertical = 10.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Column(modifier = Modifier.weight(1f)) {
                    Text(
                        text = str(R.string.device_id_label),
                        style = MaterialTheme.typography.labelMedium,
                        color = colors.textSecondary,
                    )
                    Text(
                        text = formattedDeviceId.ifEmpty { str(R.string.unavailable) },
                        fontFamily = FontFamily.Monospace,
                        fontWeight = FontWeight.Bold,
                        fontSize = 22.sp,
                        letterSpacing = 1.5.sp,
                        color = colors.primary,
                    )
                }
                if (formattedDeviceId.isNotEmpty()) {
                    TextButton(onClick = onCopyDeviceId) {
                        Icon(
                            imageVector = Icons.Rounded.ContentCopy,
                            contentDescription = str(R.string.btn_copy),
                            tint = colors.primary,
                            modifier = Modifier.size(16.dp),
                        )
                        Spacer(modifier = Modifier.width(6.dp))
                        Text(str(R.string.btn_copy), color = colors.primary)
                    }
                }
            }

            // Detected LAN & Tailscale IPs
            val hasIps = hostState.lanIps.isNotEmpty() || !hostState.tailscaleIp.isNullOrBlank()
            if (hasIps) {
                Spacer(modifier = Modifier.height(10.dp))
                FlowRow(
                    horizontalArrangement = Arrangement.spacedBy(8.dp),
                    verticalArrangement = Arrangement.spacedBy(8.dp),
                    modifier = Modifier.fillMaxWidth(),
                ) {
                    hostState.lanIps.forEach { ip ->
                        CopyableIpChip(
                            label = str(R.string.host_lan_ip_label),
                            ip = ip,
                            accent = colors.primary,
                            onClick = { onCopyIp(ip) },
                        )
                    }
                    val tsIp = hostState.tailscaleIp
                    if (!tsIp.isNullOrBlank()) {
                        CopyableIpChip(
                            label = str(R.string.host_tailscale_ip_label),
                            ip = tsIp,
                            accent = colors.success,
                            onClick = { onCopyIp(tsIp) },
                        )
                    }
                }
            }

            // Active OTP / Permanent password status when sharing
            if (hostState.isRunning) {
                Spacer(modifier = Modifier.height(10.dp))
                if (settings.effectiveHostOtpEnabled) {
                    Row(
                        modifier = Modifier
                            .fillMaxWidth()
                            .clip(RoundedCornerShape(12.dp))
                            .background(colors.background)
                            .padding(start = 14.dp, end = 6.dp, top = 8.dp, bottom = 8.dp),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        Column(modifier = Modifier.weight(1f)) {
                            Row(verticalAlignment = Alignment.CenterVertically) {
                                Text(
                                    text = str(R.string.host_otp_label),
                                    style = MaterialTheme.typography.labelMedium,
                                    color = colors.textSecondary,
                                )
                                if (settings.hostPasswordMode == HostPasswordMode.BOTH.id &&
                                    settings.hasHostPermanentPassword
                                ) {
                                    Spacer(modifier = Modifier.width(8.dp))
                                    Surface(
                                        color = colors.successContainer,
                                        shape = RoundedCornerShape(999.dp),
                                    ) {
                                        Text(
                                            text = str(R.string.host_plus_perm_badge),
                                            style = MaterialTheme.typography.labelSmall,
                                            color = colors.success,
                                            modifier = Modifier.padding(horizontal = 7.dp, vertical = 2.dp),
                                        )
                                    }
                                }
                            }
                            Text(
                                text = hostState.otp.ifBlank { "—" },
                                fontFamily = FontFamily.Monospace,
                                fontWeight = FontWeight.Bold,
                                fontSize = 20.sp,
                                letterSpacing = 2.sp,
                                color = colors.textPrimary,
                            )
                        }
                        if (hostState.otp.isNotBlank()) {
                            IconButton(onClick = { onCopyOtp(hostState.otp) }) {
                                Icon(
                                    imageVector = Icons.Rounded.ContentCopy,
                                    contentDescription = str(R.string.btn_copy),
                                    tint = colors.primary,
                                    modifier = Modifier.size(18.dp),
                                )
                            }
                        }
                        IconButton(onClick = onRotateOtp) {
                            Icon(
                                imageVector = Icons.Rounded.Refresh,
                                contentDescription = str(R.string.btn_rotate),
                                tint = colors.textSecondary,
                                modifier = Modifier.size(20.dp),
                            )
                        }
                    }
                } else {
                    Surface(
                        color = colors.successContainer,
                        shape = RoundedCornerShape(10.dp),
                        border = BorderStroke(1.dp, colors.success.copy(alpha = 0.45f)),
                        modifier = Modifier.fillMaxWidth(),
                    ) {
                        Row(
                            modifier = Modifier.padding(horizontal = 12.dp, vertical = 9.dp),
                            verticalAlignment = Alignment.CenterVertically,
                        ) {
                            Icon(
                                imageVector = Icons.Rounded.Lock,
                                contentDescription = null,
                                tint = colors.success,
                                modifier = Modifier.size(16.dp),
                            )
                            Spacer(modifier = Modifier.width(8.dp))
                            Text(
                                text = str(R.string.host_perm_active_badge),
                                style = MaterialTheme.typography.labelMedium,
                                fontWeight = FontWeight.SemiBold,
                                color = colors.success,
                            )
                        }
                    }
                }
            }

            if (fingerprint.isNotEmpty()) {
                Spacer(modifier = Modifier.height(8.dp))
                Row(
                    modifier = Modifier
                        .fillMaxWidth()
                        .clip(RoundedCornerShape(10.dp))
                        .clickable { onCopyFingerprint() }
                        .padding(horizontal = 4.dp, vertical = 4.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Text(
                        text = "${str(R.string.fingerprint_label)}: ",
                        style = MaterialTheme.typography.bodySmall,
                        color = colors.textMuted,
                    )
                    Text(
                        text = fingerprint,
                        style = MaterialTheme.typography.labelSmall,
                        color = colors.textSecondary,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                        modifier = Modifier.weight(1f),
                    )
                    Icon(
                        imageVector = Icons.Rounded.ContentCopy,
                        contentDescription = str(R.string.btn_copy),
                        tint = colors.textMuted,
                        modifier = Modifier.size(14.dp),
                    )
                }
            }

            Spacer(modifier = Modifier.height(12.dp))

            // Start / Stop Sharing button
            if (hostState.isRunning) {
                Button(
                    onClick = onStopSharing,
                    shape = RoundedCornerShape(12.dp),
                    colors = ButtonDefaults.buttonColors(
                        containerColor = colors.danger,
                        contentColor = Color.White,
                    ),
                    contentPadding = PaddingValues(vertical = 13.dp),
                    modifier = Modifier.fillMaxWidth(),
                ) {
                    Icon(
                        imageVector = Icons.AutoMirrored.Rounded.StopScreenShare,
                        contentDescription = null,
                        modifier = Modifier.size(20.dp),
                    )
                    Spacer(modifier = Modifier.width(8.dp))
                    Text(
                        text = str(R.string.host_stop_sharing),
                        fontWeight = FontWeight.Bold,
                        fontSize = 15.sp,
                    )
                }

                // Host <-> Viewer Chat button when a remote viewer is connected
                if (hostState.activePeerDeviceId != null) {
                    Spacer(modifier = Modifier.height(8.dp))
                    OutlinedButton(
                        onClick = onOpenHostChat,
                        shape = RoundedCornerShape(12.dp),
                        border = BorderStroke(
                            1.dp,
                            if (hostState.chat.unreadCount > 0) colors.primary else colors.border,
                        ),
                        contentPadding = PaddingValues(vertical = 11.dp, horizontal = 14.dp),
                        modifier = Modifier.fillMaxWidth(),
                    ) {
                        Icon(
                            imageVector = Icons.AutoMirrored.Rounded.Chat,
                            contentDescription = null,
                            tint = colors.primary,
                            modifier = Modifier.size(18.dp),
                        )
                        Spacer(modifier = Modifier.width(8.dp))
                        Text(
                            text = str(R.string.chat_open_button),
                            color = colors.textPrimary,
                            fontWeight = FontWeight.SemiBold,
                        )
                        if (hostState.chat.unreadCount > 0) {
                            Spacer(modifier = Modifier.width(8.dp))
                            UnreadCountBadge(count = hostState.chat.unreadCount)
                        }
                    }
                }
            } else {
                Button(
                    onClick = onStartSharing,
                    enabled = nativeLoaded && !hostState.isStarting,
                    shape = RoundedCornerShape(12.dp),
                    colors = ButtonDefaults.buttonColors(
                        containerColor = colors.primary,
                        contentColor = colors.onPrimary,
                    ),
                    contentPadding = PaddingValues(vertical = 13.dp),
                    modifier = Modifier.fillMaxWidth(),
                ) {
                    if (hostState.isStarting) {
                        CircularProgressIndicator(
                            modifier = Modifier.size(18.dp),
                            strokeWidth = 2.dp,
                            color = colors.onPrimary,
                        )
                    } else {
                        Icon(
                            imageVector = Icons.AutoMirrored.Rounded.ScreenShare,
                            contentDescription = null,
                            modifier = Modifier.size(20.dp),
                        )
                    }
                    Spacer(modifier = Modifier.width(8.dp))
                    Text(
                        text = str(R.string.host_start_sharing),
                        fontWeight = FontWeight.Bold,
                        fontSize = 15.sp,
                    )
                }
            }

            // Accessibility Remote Control status bar
            Spacer(modifier = Modifier.height(10.dp))
            Surface(
                color = if (hostState.accessibilityEnabled) colors.successContainer else colors.warningContainer,
                shape = RoundedCornerShape(10.dp),
                border = BorderStroke(
                    1.dp,
                    if (hostState.accessibilityEnabled) {
                        colors.success.copy(alpha = 0.4f)
                    } else {
                        colors.warning.copy(alpha = 0.45f)
                    },
                ),
                modifier = Modifier.fillMaxWidth(),
            ) {
                Row(
                    modifier = Modifier.padding(horizontal = 10.dp, vertical = 7.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Icon(
                        imageVector = if (hostState.accessibilityEnabled) {
                            Icons.Rounded.CheckCircle
                        } else {
                            Icons.Rounded.TouchApp
                        },
                        contentDescription = null,
                        tint = if (hostState.accessibilityEnabled) colors.success else colors.warning,
                        modifier = Modifier.size(18.dp),
                    )
                    Spacer(modifier = Modifier.width(8.dp))
                    Text(
                        text = if (hostState.accessibilityEnabled) {
                            str(R.string.host_control_enabled)
                        } else {
                            str(R.string.host_control_disabled)
                        },
                        style = MaterialTheme.typography.bodySmall,
                        color = colors.textPrimary,
                        modifier = Modifier.weight(1f),
                    )
                    if (!hostState.accessibilityEnabled) {
                        TextButton(
                            onClick = onEnableAccessibility,
                            contentPadding = PaddingValues(horizontal = 8.dp, vertical = 2.dp),
                        ) {
                            Text(
                                text = str(R.string.host_enable_control),
                                color = colors.primary,
                                fontWeight = FontWeight.Bold,
                                style = MaterialTheme.typography.labelMedium,
                            )
                        }
                    }
                }
            }
        }
    }
}

@Composable
private fun CopyableIpChip(
    label: String,
    ip: String,
    accent: Color,
    onClick: () -> Unit,
) {
    val colors = VeloxTheme.colors
    Surface(
        color = colors.background,
        shape = RoundedCornerShape(10.dp),
        border = BorderStroke(1.dp, colors.border),
        modifier = Modifier
            .clip(RoundedCornerShape(10.dp))
            .clickable(onClick = onClick),
    ) {
        Row(
            modifier = Modifier.padding(horizontal = 10.dp, vertical = 6.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Text(
                text = "$label:",
                style = MaterialTheme.typography.labelSmall,
                color = accent,
                fontWeight = FontWeight.Bold,
            )
            Spacer(modifier = Modifier.width(5.dp))
            Text(
                text = ip,
                style = MaterialTheme.typography.labelSmall,
                fontFamily = FontFamily.Monospace,
                color = colors.textPrimary,
            )
            Spacer(modifier = Modifier.width(5.dp))
            Icon(
                imageVector = Icons.Rounded.ContentCopy,
                contentDescription = str(R.string.btn_copy),
                tint = colors.textMuted,
                modifier = Modifier.size(13.dp),
            )
        }
    }
}

@Composable
private fun RemotePcCard(
    targetInput: String,
    onTargetChange: (String) -> Unit,
    targetError: Boolean,
    selectedMode: Int,
    onModeSelected: (Int) -> Unit,
    needsRendezvous: Boolean,
    nativeLoaded: Boolean,
    onOpenSettings: () -> Unit,
    onConnect: () -> Unit,
) {
    val colors = VeloxTheme.colors
    Card(
        colors = CardDefaults.cardColors(containerColor = colors.surface),
        border = BorderStroke(1.dp, colors.border),
        shape = RoundedCornerShape(16.dp),
        modifier = Modifier.fillMaxWidth(),
    ) {
        Column(modifier = Modifier.padding(16.dp)) {
            Text(
                text = str(R.string.remote_pc_title),
                style = MaterialTheme.typography.titleMedium,
                color = colors.textPrimary,
            )
            Spacer(modifier = Modifier.height(2.dp))
            Text(
                text = str(R.string.remote_pc_subtitle),
                style = MaterialTheme.typography.bodySmall,
                color = colors.textSecondary,
            )
            Spacer(modifier = Modifier.height(12.dp))

            OutlinedTextField(
                value = targetInput,
                onValueChange = onTargetChange,
                singleLine = true,
                isError = targetError,
                placeholder = {
                    Text(
                        text = str(R.string.target_placeholder),
                        color = colors.textMuted,
                    )
                },
                trailingIcon = {
                    if (targetInput.isNotEmpty()) {
                        IconButton(onClick = { onTargetChange("") }) {
                            Icon(
                                imageVector = Icons.Rounded.Clear,
                                contentDescription = null,
                                tint = colors.textSecondary,
                            )
                        }
                    }
                },
                supportingText = if (targetError) {
                    { Text(str(R.string.err_invalid_target), color = colors.danger) }
                } else {
                    null
                },
                keyboardOptions = KeyboardOptions(imeAction = ImeAction.Go),
                keyboardActions = KeyboardActions(onGo = { onConnect() }),
                colors = OutlinedTextFieldDefaults.colors(
                    focusedBorderColor = colors.primary,
                    unfocusedBorderColor = colors.border,
                    focusedContainerColor = colors.background,
                    unfocusedContainerColor = colors.background,
                    focusedTextColor = colors.textPrimary,
                    unfocusedTextColor = colors.textPrimary,
                ),
                shape = RoundedCornerShape(12.dp),
                modifier = Modifier.fillMaxWidth(),
            )

            if (needsRendezvous) {
                Spacer(modifier = Modifier.height(8.dp))
                Surface(
                    color = colors.warningContainer,
                    shape = RoundedCornerShape(10.dp),
                    border = BorderStroke(1.dp, colors.warning.copy(alpha = 0.45f)),
                    modifier = Modifier.fillMaxWidth(),
                ) {
                    Row(
                        modifier = Modifier.padding(horizontal = 10.dp, vertical = 6.dp),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        Text(
                            text = str(R.string.rendezvous_missing_hint),
                            style = MaterialTheme.typography.bodySmall,
                            color = colors.warning,
                            modifier = Modifier.weight(1f),
                        )
                        TextButton(onClick = onOpenSettings) {
                            Text(str(R.string.btn_configure), color = colors.primary)
                        }
                    }
                }
            }

            Spacer(modifier = Modifier.height(12.dp))
            Text(
                text = str(R.string.session_mode_label),
                style = MaterialTheme.typography.labelMedium,
                color = colors.textSecondary,
            )
            Spacer(modifier = Modifier.height(6.dp))

            Row(
                horizontalArrangement = Arrangement.spacedBy(10.dp),
                modifier = Modifier.fillMaxWidth(),
            ) {
                ModeChip(
                    selected = selectedMode == BridgeConstants.MODE_DESKTOP,
                    label = str(R.string.mode_desktop),
                    icon = Icons.Rounded.Computer,
                    onClick = { onModeSelected(BridgeConstants.MODE_DESKTOP) },
                    modifier = Modifier.weight(1f),
                )
                ModeChip(
                    selected = selectedMode == BridgeConstants.MODE_GAMING,
                    label = str(R.string.mode_gaming),
                    icon = Icons.Rounded.SportsEsports,
                    onClick = { onModeSelected(BridgeConstants.MODE_GAMING) },
                    modifier = Modifier.weight(1f),
                )
            }

            Spacer(modifier = Modifier.height(14.dp))
            Button(
                onClick = onConnect,
                enabled = nativeLoaded && targetInput.isNotBlank(),
                shape = RoundedCornerShape(12.dp),
                colors = ButtonDefaults.buttonColors(
                    containerColor = colors.primary,
                    contentColor = colors.onPrimary,
                ),
                contentPadding = PaddingValues(vertical = 14.dp),
                modifier = Modifier.fillMaxWidth(),
            ) {
                Icon(
                    imageVector = Icons.Rounded.PlayArrow,
                    contentDescription = null,
                    modifier = Modifier.size(20.dp),
                )
                Spacer(modifier = Modifier.width(6.dp))
                Text(
                    text = str(R.string.btn_connect),
                    fontWeight = FontWeight.Bold,
                    fontSize = 15.sp,
                )
            }
        }
    }
}

@Composable
private fun ModeChip(
    selected: Boolean,
    label: String,
    icon: androidx.compose.ui.graphics.vector.ImageVector,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val colors = VeloxTheme.colors
    FilterChip(
        selected = selected,
        onClick = onClick,
        label = {
            Text(
                text = label,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
        },
        leadingIcon = {
            Icon(
                imageVector = icon,
                contentDescription = null,
                modifier = Modifier.size(18.dp),
            )
        },
        colors = FilterChipDefaults.filterChipColors(
            selectedContainerColor = colors.primary.copy(alpha = 0.18f),
            selectedLabelColor = colors.primary,
            selectedLeadingIconColor = colors.primary,
            containerColor = colors.background,
            labelColor = colors.textSecondary,
            iconColor = colors.textSecondary,
        ),
        border = FilterChipDefaults.filterChipBorder(
            enabled = true,
            selected = selected,
            borderColor = colors.border,
            selectedBorderColor = colors.primary,
        ),
        shape = RoundedCornerShape(12.dp),
        modifier = modifier.height(44.dp),
    )
}

@Composable
private fun RecentConnectionsCard(
    recent: List<RecentConnection>,
    onConnectRecent: (RecentConnection) -> Unit,
    onRemoveRecent: (String) -> Unit,
) {
    val colors = VeloxTheme.colors
    Card(
        colors = CardDefaults.cardColors(containerColor = colors.surface),
        border = BorderStroke(1.dp, colors.border),
        shape = RoundedCornerShape(16.dp),
        modifier = Modifier.fillMaxWidth(),
    ) {
        Column(modifier = Modifier.padding(16.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Icon(
                    imageVector = Icons.Rounded.History,
                    contentDescription = null,
                    tint = colors.textSecondary,
                    modifier = Modifier.size(18.dp),
                )
                Spacer(modifier = Modifier.width(8.dp))
                Text(
                    text = str(R.string.recent_connections_title),
                    style = MaterialTheme.typography.titleMedium,
                    color = colors.textPrimary,
                )
            }
            Spacer(modifier = Modifier.height(10.dp))

            if (recent.isEmpty()) {
                Text(
                    text = str(R.string.recent_connections_empty),
                    style = MaterialTheme.typography.bodySmall,
                    color = colors.textMuted,
                )
            } else {
                recent.forEachIndexed { idx, entry ->
                    if (idx > 0) {
                        HorizontalDivider(color = colors.border.copy(alpha = 0.5f))
                    }
                    Row(
                        modifier = Modifier
                            .fillMaxWidth()
                            .clickable { onConnectRecent(entry) }
                            .padding(vertical = 10.dp),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        Icon(
                            imageVector = if (entry.mode == BridgeConstants.MODE_GAMING) {
                                Icons.Rounded.SportsEsports
                            } else {
                                Icons.Rounded.Computer
                            },
                            contentDescription = null,
                            tint = colors.primary,
                            modifier = Modifier.size(20.dp),
                        )
                        Spacer(modifier = Modifier.width(12.dp))
                        Column(modifier = Modifier.weight(1f)) {
                            Text(
                                text = entry.label,
                                style = MaterialTheme.typography.bodyMedium,
                                fontWeight = FontWeight.Medium,
                                color = colors.textPrimary,
                                maxLines = 1,
                                overflow = TextOverflow.Ellipsis,
                            )
                            Text(
                                text = "${AppPreferences.formatDisplayTarget(entry.target)} · " +
                                    if (entry.mode == BridgeConstants.MODE_GAMING) {
                                        str(R.string.mode_gaming)
                                    } else {
                                        str(R.string.mode_desktop)
                                    },
                                style = MaterialTheme.typography.bodySmall,
                                color = colors.textSecondary,
                                maxLines = 1,
                                overflow = TextOverflow.Ellipsis,
                            )
                        }
                        IconButton(onClick = { onRemoveRecent(entry.target) }) {
                            Icon(
                                imageVector = Icons.Rounded.DeleteOutline,
                                contentDescription = str(R.string.btn_remove),
                                tint = colors.textMuted,
                                modifier = Modifier.size(18.dp),
                            )
                        }
                    }
                }
            }
        }
    }
}

@Composable
private fun ConnectBottomBar(
    buildDisplay: String,
    checkingUpdate: Boolean,
    onCheckUpdates: () -> Unit,
) {
    val colors = VeloxTheme.colors
    Surface(
        color = colors.surface,
        border = BorderStroke(1.dp, colors.border),
        modifier = Modifier
            .fillMaxWidth()
            .navigationBarsPadding(),
    ) {
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .padding(horizontal = 16.dp, vertical = 8.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Text(
                text = buildDisplay,
                style = MaterialTheme.typography.labelSmall,
                color = colors.textSecondary,
                modifier = Modifier.weight(1f),
            )
            TextButton(
                onClick = onCheckUpdates,
                enabled = !checkingUpdate,
            ) {
                if (checkingUpdate) {
                    CircularProgressIndicator(
                        modifier = Modifier.size(14.dp),
                        strokeWidth = 2.dp,
                        color = colors.primary,
                    )
                } else {
                    Icon(
                        imageVector = Icons.Rounded.Refresh,
                        contentDescription = null,
                        tint = colors.primary,
                        modifier = Modifier.size(16.dp),
                    )
                }
                Spacer(modifier = Modifier.width(6.dp))
                Text(
                    text = str(R.string.btn_check_updates),
                    color = colors.primary,
                    style = MaterialTheme.typography.labelMedium,
                )
            }
        }
    }
}
