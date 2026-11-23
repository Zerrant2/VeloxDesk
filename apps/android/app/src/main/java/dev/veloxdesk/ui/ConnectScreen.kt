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
import androidx.compose.material.icons.rounded.Clear
import androidx.compose.material.icons.rounded.Close
import androidx.compose.material.icons.rounded.Computer
import androidx.compose.material.icons.rounded.ContentCopy
import androidx.compose.material.icons.rounded.DeleteOutline
import androidx.compose.material.icons.rounded.DesktopWindows
import androidx.compose.material.icons.rounded.Download
import androidx.compose.material.icons.rounded.History
import androidx.compose.material.icons.rounded.PlayArrow
import androidx.compose.material.icons.rounded.Refresh
import androidx.compose.material.icons.rounded.Settings
import androidx.compose.material.icons.rounded.SportsEsports
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
import dev.veloxdesk.data.AppLanguage
import dev.veloxdesk.data.AppPreferences
import dev.veloxdesk.data.AppSettings
import dev.veloxdesk.data.RecentConnection
import dev.veloxdesk.data.ReleaseInfo
import dev.veloxdesk.data.UpdateChecker
import dev.veloxdesk.ui.i18n.str
import dev.veloxdesk.ui.theme.VeloxColors
import kotlinx.coroutines.launch

@Composable
fun ConnectScreen(
    settings: AppSettings,
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
    onOpenSettings: () -> Unit,
    onModeSelected: (Int) -> Unit,
    onConnect: (target: String, mode: Int) -> Unit,
    onRemoveRecent: (target: String) -> Unit,
    onCheckUpdates: () -> Unit,
    snackbarHostState: SnackbarHostState,
) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    var targetInput by rememberSaveable { mutableStateOf("") }
    var targetError by remember { mutableStateOf(false) }

    val formattedOwnId = remember(ownDeviceId) {
        if (ownDeviceId.isBlank()) "" else AppPreferences.formatDeviceId(ownDeviceId)
    }
    val needsRendezvous = AppPreferences.isNineDigitDeviceId(targetInput) && settings.rendezvousServer.isBlank()
    val copiedIdMsg = str(R.string.copied_device_id)
    val copiedFpMsg = str(R.string.copied_fingerprint)

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
        containerColor = VeloxColors.Background,
        snackbarHost = { SnackbarHost(snackbarHostState) },
        topBar = {
            ConnectTopBar(
                language = settings.language,
                nativeLoaded = nativeLoaded,
                onLanguageChange = onLanguageChange,
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

            // "This Device" card
            ThisDeviceCard(
                formattedDeviceId = formattedOwnId,
                fingerprint = ownFingerprint,
                onCopyDeviceId = {
                    if (formattedOwnId.isNotEmpty()) {
                        copyToClipboard("VeloxDesk ID", ownDeviceId, copiedIdMsg)
                    }
                },
                onCopyFingerprint = {
                    if (ownFingerprint.isNotEmpty()) {
                        copyToClipboard("VeloxDesk Fingerprint", ownFingerprint, copiedFpMsg)
                    }
                },
            )

            // "Remote PC" card
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
    nativeLoaded: Boolean,
    onLanguageChange: (AppLanguage) -> Unit,
    onOpenSettings: () -> Unit,
) {
    Surface(
        color = VeloxColors.Background,
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
                    .background(VeloxColors.Surface),
                contentAlignment = Alignment.Center,
            ) {
                Icon(
                    imageVector = Icons.Rounded.DesktopWindows,
                    contentDescription = null,
                    tint = VeloxColors.Primary,
                    modifier = Modifier.size(20.dp),
                )
            }
            Spacer(modifier = Modifier.width(10.dp))
            Column(modifier = Modifier.weight(1f)) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Text(
                        text = str(R.string.app_name),
                        style = MaterialTheme.typography.titleLarge,
                        color = VeloxColors.TextPrimary,
                    )
                    Spacer(modifier = Modifier.width(8.dp))
                    Box(
                        modifier = Modifier
                            .size(8.dp)
                            .clip(CircleShape)
                            .background(if (nativeLoaded) VeloxColors.Emerald else VeloxColors.Danger),
                    )
                }
            }

            // EN / RU compact segmented pill
            Surface(
                color = VeloxColors.Surface,
                shape = RoundedCornerShape(999.dp),
                border = BorderStroke(1.dp, VeloxColors.Border),
            ) {
                Row(modifier = Modifier.padding(2.dp)) {
                    AppLanguage.entries.forEach { lang ->
                        val selected = lang == language
                        Box(
                            modifier = Modifier
                                .clip(RoundedCornerShape(999.dp))
                                .background(if (selected) VeloxColors.Primary else Color.Transparent)
                                .clickable { onLanguageChange(lang) }
                                .padding(horizontal = 10.dp, vertical = 5.dp),
                        ) {
                            Text(
                                text = lang.shortLabel,
                                style = MaterialTheme.typography.labelMedium,
                                fontWeight = FontWeight.Bold,
                                color = if (selected) VeloxColors.OnPrimary else VeloxColors.TextSecondary,
                            )
                        }
                    }
                }
            }

            Spacer(modifier = Modifier.width(6.dp))
            IconButton(onClick = onOpenSettings) {
                Icon(
                    imageVector = Icons.Rounded.Settings,
                    contentDescription = str(R.string.btn_settings),
                    tint = VeloxColors.TextPrimary,
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
    Surface(
        color = if (isError) VeloxColors.DangerContainer else VeloxColors.SurfaceElevated,
        shape = RoundedCornerShape(12.dp),
        border = BorderStroke(1.dp, if (isError) VeloxColors.Danger.copy(alpha = 0.5f) else VeloxColors.Border),
        modifier = Modifier.fillMaxWidth(),
    ) {
        Row(
            modifier = Modifier.padding(horizontal = 12.dp, vertical = 10.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Icon(
                imageVector = Icons.Rounded.WarningAmber,
                contentDescription = null,
                tint = if (isError) VeloxColors.Danger else VeloxColors.Amber,
                modifier = Modifier.size(20.dp),
            )
            Spacer(modifier = Modifier.width(10.dp))
            Text(
                text = text,
                style = MaterialTheme.typography.bodySmall,
                color = VeloxColors.TextPrimary,
                modifier = Modifier.weight(1f),
            )
            if (onDismiss != null) {
                IconButton(onClick = onDismiss, modifier = Modifier.size(28.dp)) {
                    Icon(
                        imageVector = Icons.Rounded.Close,
                        contentDescription = str(R.string.btn_close),
                        tint = VeloxColors.TextSecondary,
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
    Surface(
        color = Color(0xFF0C4A6E).copy(alpha = 0.65f),
        shape = RoundedCornerShape(14.dp),
        border = BorderStroke(1.dp, VeloxColors.Primary.copy(alpha = 0.6f)),
        modifier = Modifier.fillMaxWidth(),
    ) {
        Row(
            modifier = Modifier.padding(horizontal = 14.dp, vertical = 10.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Icon(
                imageVector = Icons.Rounded.Download,
                contentDescription = null,
                tint = VeloxColors.Primary,
                modifier = Modifier.size(20.dp),
            )
            Spacer(modifier = Modifier.width(10.dp))
            Column(modifier = Modifier.weight(1f)) {
                Text(
                    text = str(R.string.update_available_title, release.tagName),
                    style = MaterialTheme.typography.titleMedium,
                    color = VeloxColors.TextPrimary,
                )
                if (release.apkName != null) {
                    Text(
                        text = release.apkName,
                        style = MaterialTheme.typography.bodySmall,
                        color = VeloxColors.TextSecondary,
                    )
                }
            }
            TextButton(onClick = onOpenRelease) {
                Text(
                    text = str(R.string.btn_open_release),
                    color = VeloxColors.Primary,
                    fontWeight = FontWeight.Bold,
                )
            }
        }
    }
}

@Composable
private fun ThisDeviceCard(
    formattedDeviceId: String,
    fingerprint: String,
    onCopyDeviceId: () -> Unit,
    onCopyFingerprint: () -> Unit,
) {
    Card(
        colors = CardDefaults.cardColors(containerColor = VeloxColors.Surface),
        border = BorderStroke(1.dp, VeloxColors.Border),
        shape = RoundedCornerShape(16.dp),
        modifier = Modifier.fillMaxWidth(),
    ) {
        Column(modifier = Modifier.padding(16.dp)) {
            Text(
                text = str(R.string.this_device_title),
                style = MaterialTheme.typography.titleMedium,
                color = VeloxColors.TextPrimary,
            )
            Spacer(modifier = Modifier.height(2.dp))
            Text(
                text = str(R.string.this_device_subtitle),
                style = MaterialTheme.typography.bodySmall,
                color = VeloxColors.TextSecondary,
            )
            Spacer(modifier = Modifier.height(12.dp))

            Row(
                modifier = Modifier
                    .fillMaxWidth()
                    .clip(RoundedCornerShape(12.dp))
                    .background(VeloxColors.Background)
                    .padding(horizontal = 14.dp, vertical = 10.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Column(modifier = Modifier.weight(1f)) {
                    Text(
                        text = str(R.string.device_id_label),
                        style = MaterialTheme.typography.labelMedium,
                        color = VeloxColors.TextSecondary,
                    )
                    Text(
                        text = formattedDeviceId.ifEmpty { str(R.string.unavailable) },
                        fontFamily = FontFamily.Monospace,
                        fontWeight = FontWeight.Bold,
                        fontSize = 22.sp,
                        letterSpacing = 1.5.sp,
                        color = VeloxColors.Primary,
                    )
                }
                if (formattedDeviceId.isNotEmpty()) {
                    TextButton(onClick = onCopyDeviceId) {
                        Icon(
                            imageVector = Icons.Rounded.ContentCopy,
                            contentDescription = str(R.string.btn_copy),
                            tint = VeloxColors.Primary,
                            modifier = Modifier.size(16.dp),
                        )
                        Spacer(modifier = Modifier.width(6.dp))
                        Text(str(R.string.btn_copy), color = VeloxColors.Primary)
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
                        color = VeloxColors.TextMuted,
                    )
                    Text(
                        text = fingerprint,
                        style = MaterialTheme.typography.labelSmall,
                        color = VeloxColors.TextSecondary,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                        modifier = Modifier.weight(1f),
                    )
                    Icon(
                        imageVector = Icons.Rounded.ContentCopy,
                        contentDescription = str(R.string.btn_copy),
                        tint = VeloxColors.TextMuted,
                        modifier = Modifier.size(14.dp),
                    )
                }
            }
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
    Card(
        colors = CardDefaults.cardColors(containerColor = VeloxColors.Surface),
        border = BorderStroke(1.dp, VeloxColors.Border),
        shape = RoundedCornerShape(16.dp),
        modifier = Modifier.fillMaxWidth(),
    ) {
        Column(modifier = Modifier.padding(16.dp)) {
            Text(
                text = str(R.string.remote_pc_title),
                style = MaterialTheme.typography.titleMedium,
                color = VeloxColors.TextPrimary,
            )
            Spacer(modifier = Modifier.height(2.dp))
            Text(
                text = str(R.string.remote_pc_subtitle),
                style = MaterialTheme.typography.bodySmall,
                color = VeloxColors.TextSecondary,
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
                        color = VeloxColors.TextMuted,
                    )
                },
                trailingIcon = {
                    if (targetInput.isNotEmpty()) {
                        IconButton(onClick = { onTargetChange("") }) {
                            Icon(
                                imageVector = Icons.Rounded.Clear,
                                contentDescription = null,
                                tint = VeloxColors.TextSecondary,
                            )
                        }
                    }
                },
                supportingText = if (targetError) {
                    { Text(str(R.string.err_invalid_target), color = VeloxColors.Danger) }
                } else {
                    null
                },
                keyboardOptions = KeyboardOptions(imeAction = ImeAction.Go),
                keyboardActions = KeyboardActions(onGo = { onConnect() }),
                colors = OutlinedTextFieldDefaults.colors(
                    focusedBorderColor = VeloxColors.Primary,
                    unfocusedBorderColor = VeloxColors.Border,
                    focusedContainerColor = VeloxColors.Background,
                    unfocusedContainerColor = VeloxColors.Background,
                    focusedTextColor = VeloxColors.TextPrimary,
                    unfocusedTextColor = VeloxColors.TextPrimary,
                ),
                shape = RoundedCornerShape(12.dp),
                modifier = Modifier.fillMaxWidth(),
            )

            if (needsRendezvous) {
                Spacer(modifier = Modifier.height(8.dp))
                Surface(
                    color = VeloxColors.Amber.copy(alpha = 0.12f),
                    shape = RoundedCornerShape(10.dp),
                    border = BorderStroke(1.dp, VeloxColors.Amber.copy(alpha = 0.4f)),
                    modifier = Modifier.fillMaxWidth(),
                ) {
                    Row(
                        modifier = Modifier.padding(horizontal = 10.dp, vertical = 6.dp),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        Text(
                            text = str(R.string.rendezvous_missing_hint),
                            style = MaterialTheme.typography.bodySmall,
                            color = VeloxColors.Amber,
                            modifier = Modifier.weight(1f),
                        )
                        TextButton(onClick = onOpenSettings) {
                            Text(str(R.string.btn_configure), color = VeloxColors.Primary)
                        }
                    }
                }
            }

            Spacer(modifier = Modifier.height(12.dp))
            Text(
                text = str(R.string.session_mode_label),
                style = MaterialTheme.typography.labelMedium,
                color = VeloxColors.TextSecondary,
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
                    containerColor = VeloxColors.Primary,
                    contentColor = VeloxColors.OnPrimary,
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
            selectedContainerColor = VeloxColors.Primary.copy(alpha = 0.18f),
            selectedLabelColor = VeloxColors.Primary,
            selectedLeadingIconColor = VeloxColors.Primary,
            containerColor = VeloxColors.Background,
            labelColor = VeloxColors.TextSecondary,
            iconColor = VeloxColors.TextSecondary,
        ),
        border = FilterChipDefaults.filterChipBorder(
            enabled = true,
            selected = selected,
            borderColor = VeloxColors.Border,
            selectedBorderColor = VeloxColors.Primary,
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
    Card(
        colors = CardDefaults.cardColors(containerColor = VeloxColors.Surface),
        border = BorderStroke(1.dp, VeloxColors.Border),
        shape = RoundedCornerShape(16.dp),
        modifier = Modifier.fillMaxWidth(),
    ) {
        Column(modifier = Modifier.padding(16.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Icon(
                    imageVector = Icons.Rounded.History,
                    contentDescription = null,
                    tint = VeloxColors.TextSecondary,
                    modifier = Modifier.size(18.dp),
                )
                Spacer(modifier = Modifier.width(8.dp))
                Text(
                    text = str(R.string.recent_connections_title),
                    style = MaterialTheme.typography.titleMedium,
                    color = VeloxColors.TextPrimary,
                )
            }
            Spacer(modifier = Modifier.height(10.dp))

            if (recent.isEmpty()) {
                Text(
                    text = str(R.string.recent_connections_empty),
                    style = MaterialTheme.typography.bodySmall,
                    color = VeloxColors.TextMuted,
                )
            } else {
                recent.forEachIndexed { idx, entry ->
                    if (idx > 0) {
                        HorizontalDivider(color = VeloxColors.Border.copy(alpha = 0.5f))
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
                            tint = VeloxColors.Primary,
                            modifier = Modifier.size(20.dp),
                        )
                        Spacer(modifier = Modifier.width(12.dp))
                        Column(modifier = Modifier.weight(1f)) {
                            Text(
                                text = entry.label,
                                style = MaterialTheme.typography.bodyMedium,
                                fontWeight = FontWeight.Medium,
                                color = VeloxColors.TextPrimary,
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
                                color = VeloxColors.TextSecondary,
                                maxLines = 1,
                                overflow = TextOverflow.Ellipsis,
                            )
                        }
                        IconButton(onClick = { onRemoveRecent(entry.target) }) {
                            Icon(
                                imageVector = Icons.Rounded.DeleteOutline,
                                contentDescription = str(R.string.btn_remove),
                                tint = VeloxColors.TextMuted,
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
    Surface(
        color = VeloxColors.Surface,
        border = BorderStroke(1.dp, VeloxColors.Border),
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
                color = VeloxColors.TextSecondary,
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
                        color = VeloxColors.Primary,
                    )
                } else {
                    Icon(
                        imageVector = Icons.Rounded.Refresh,
                        contentDescription = null,
                        tint = VeloxColors.Primary,
                        modifier = Modifier.size(16.dp),
                    )
                }
                Spacer(modifier = Modifier.width(6.dp))
                Text(
                    text = str(R.string.btn_check_updates),
                    color = VeloxColors.Primary,
                    style = MaterialTheme.typography.labelMedium,
                )
            }
        }
    }
}
