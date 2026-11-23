package dev.veloxdesk.ui

import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.rounded.ArrowBack
import androidx.compose.material.icons.rounded.DeleteOutline
import androidx.compose.material.icons.rounded.Devices
import androidx.compose.material.icons.rounded.RestartAlt
import androidx.compose.material.icons.rounded.Visibility
import androidx.compose.material.icons.rounded.VisibilityOff
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
import androidx.compose.material3.Switch
import androidx.compose.material3.SwitchDefaults
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.text.input.VisualTransformation
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import dev.veloxdesk.R
import dev.veloxdesk.bridge.BridgeConstants
import dev.veloxdesk.bridge.HostPasswordMode
import dev.veloxdesk.bridge.HostSavedTrustedPeer
import dev.veloxdesk.bridge.ScaleMode
import dev.veloxdesk.bridge.TouchMode
import dev.veloxdesk.data.AppLanguage
import dev.veloxdesk.data.AppPreferences
import dev.veloxdesk.data.AppSettings
import dev.veloxdesk.data.AppThemeMode
import dev.veloxdesk.data.BitratePreset
import dev.veloxdesk.data.StreamMode
import dev.veloxdesk.data.StreamProfile
import dev.veloxdesk.ui.i18n.str
import dev.veloxdesk.ui.theme.VeloxTheme

private const val MIN_PERMANENT_PASSWORD_LENGTH = 6

@OptIn(ExperimentalLayoutApi::class)
@Composable
fun SettingsScreen(
    settings: AppSettings,
    isHashingHostPassword: Boolean,
    snackbarHostState: SnackbarHostState,
    onBack: () -> Unit,
    onLanguageChange: (AppLanguage) -> Unit,
    onThemeModeChange: (AppThemeMode) -> Unit,
    onRendezvousChange: (String) -> Unit,
    onRelayOnlyChange: (Boolean) -> Unit,
    onUpdateProfile: (mode: Int, (StreamProfile) -> StreamProfile) -> Unit,
    onResetProfileDefaults: (StreamMode) -> Unit,
    onHostPasswordModeChange: (Int) -> Unit,
    onSaveHostPermanentPassword: (String) -> Unit,
    onClearHostPermanentPassword: () -> Unit,
    onHostRequireAcceptChange: (Boolean) -> Unit,
    onHostAllowInputChange: (Boolean) -> Unit,
    onHostAllowClipboardChange: (Boolean) -> Unit,
    onHostAllowAudioChange: (Boolean) -> Unit,
    onHostAllowChatChange: (Boolean) -> Unit,
    onRemoveHostTrustedPeer: (String) -> Unit,
) {
    val colors = VeloxTheme.colors
    var editingMode by rememberSaveable { mutableIntStateOf(settings.lastMode) }
    val profile = settings.profileForMode(editingMode)

    // Plaintext password draft is intentionally volatile (never rememberSaveable).
    var passwordDraft by remember { mutableStateOf("") }
    var showPassword by remember { mutableStateOf(false) }
    var passwordMinLenError by remember { mutableStateOf(false) }

    fun submitPermanentPassword() {
        if (isHashingHostPassword) return
        if (passwordDraft.length < MIN_PERMANENT_PASSWORD_LENGTH) {
            passwordMinLenError = true
            return
        }
        val pw = passwordDraft
        passwordDraft = ""
        showPassword = false
        passwordMinLenError = false
        onSaveHostPermanentPassword(pw)
    }

    Scaffold(
        containerColor = colors.background,
        snackbarHost = { SnackbarHost(snackbarHostState) },
        topBar = {
            Surface(
                color = colors.background,
                modifier = Modifier
                    .fillMaxWidth()
                    .statusBarsPadding(),
            ) {
                Row(
                    modifier = Modifier
                        .fillMaxWidth()
                        .padding(horizontal = 8.dp, vertical = 8.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    IconButton(onClick = onBack) {
                        Icon(
                            imageVector = Icons.AutoMirrored.Rounded.ArrowBack,
                            contentDescription = str(R.string.btn_back),
                            tint = colors.textPrimary,
                        )
                    }
                    Spacer(modifier = Modifier.width(4.dp))
                    Text(
                        text = str(R.string.settings_title),
                        style = MaterialTheme.typography.titleLarge,
                        color = colors.textPrimary,
                    )
                }
            }
        },
    ) { innerPadding ->
        Column(
            modifier = Modifier
                .fillMaxSize()
                .padding(innerPadding)
                .verticalScroll(rememberScrollState())
                .padding(horizontal = 16.dp, vertical = 10.dp),
            verticalArrangement = Arrangement.spacedBy(14.dp),
        ) {
            // General (Language & Theme)
            SettingsSectionCard(title = str(R.string.settings_section_general)) {
                Text(
                    text = str(R.string.settings_language),
                    style = MaterialTheme.typography.labelMedium,
                    color = colors.textSecondary,
                )
                Spacer(modifier = Modifier.height(6.dp))
                Row(horizontalArrangement = Arrangement.spacedBy(10.dp)) {
                    AppLanguage.entries.forEach { lang ->
                        SettingChip(
                            selected = settings.language == lang,
                            label = lang.nativeName,
                            onClick = { onLanguageChange(lang) },
                            modifier = Modifier.weight(1f),
                        )
                    }
                }

                Spacer(modifier = Modifier.height(14.dp))
                Text(
                    text = str(R.string.settings_theme),
                    style = MaterialTheme.typography.labelMedium,
                    color = colors.textSecondary,
                )
                Spacer(modifier = Modifier.height(6.dp))
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    listOf(
                        AppThemeMode.SYSTEM to str(R.string.theme_system),
                        AppThemeMode.DARK to str(R.string.theme_dark),
                        AppThemeMode.LIGHT to str(R.string.theme_light),
                    ).forEach { (mode, label) ->
                        SettingChip(
                            selected = settings.themeMode == mode,
                            label = label,
                            onClick = { onThemeModeChange(mode) },
                            modifier = Modifier.weight(1f),
                        )
                    }
                }
            }

            // Host Access & Security (Android Host Mode)
            SettingsSectionCard(title = str(R.string.settings_section_host_security)) {
                Text(
                    text = str(R.string.settings_host_pw_mode),
                    style = MaterialTheme.typography.labelMedium,
                    color = colors.textSecondary,
                )
                Spacer(modifier = Modifier.height(6.dp))
                FlowRow(
                    horizontalArrangement = Arrangement.spacedBy(8.dp),
                    verticalArrangement = Arrangement.spacedBy(6.dp),
                ) {
                    listOf(
                        HostPasswordMode.ONE_TIME.id to str(R.string.host_pw_mode_otp),
                        HostPasswordMode.PERMANENT.id to str(R.string.host_pw_mode_permanent),
                        HostPasswordMode.BOTH.id to str(R.string.host_pw_mode_both),
                    ).forEach { (modeId, label) ->
                        SettingChip(
                            selected = settings.hostPasswordMode == modeId,
                            label = label,
                            onClick = { onHostPasswordModeChange(modeId) },
                        )
                    }
                }

                if (settings.hostPasswordMode == HostPasswordMode.PERMANENT.id &&
                    !settings.hasHostPermanentPassword
                ) {
                    Spacer(modifier = Modifier.height(8.dp))
                    Surface(
                        color = colors.warningContainer,
                        shape = RoundedCornerShape(10.dp),
                        border = BorderStroke(1.dp, colors.warning.copy(alpha = 0.45f)),
                        modifier = Modifier.fillMaxWidth(),
                    ) {
                        Text(
                            text = str(R.string.host_pw_permanent_fallback_hint),
                            style = MaterialTheme.typography.bodySmall,
                            color = colors.warning,
                            modifier = Modifier.padding(horizontal = 10.dp, vertical = 8.dp),
                        )
                    }
                }

                Spacer(modifier = Modifier.height(14.dp))
                Row(
                    modifier = Modifier.fillMaxWidth(),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Text(
                        text = str(R.string.settings_host_perm_pw_label),
                        style = MaterialTheme.typography.labelMedium,
                        color = colors.textSecondary,
                        modifier = Modifier.weight(1f),
                    )
                    Surface(
                        color = if (settings.hasHostPermanentPassword) colors.successContainer else colors.background,
                        shape = RoundedCornerShape(999.dp),
                        border = BorderStroke(
                            1.dp,
                            if (settings.hasHostPermanentPassword) {
                                colors.success.copy(alpha = 0.45f)
                            } else {
                                colors.border
                            },
                        ),
                    ) {
                        Text(
                            text = if (settings.hasHostPermanentPassword) {
                                str(R.string.settings_host_perm_status_set)
                            } else {
                                str(R.string.settings_host_perm_status_unset)
                            },
                            style = MaterialTheme.typography.labelSmall,
                            fontWeight = FontWeight.SemiBold,
                            color = if (settings.hasHostPermanentPassword) colors.success else colors.textMuted,
                            modifier = Modifier.padding(horizontal = 8.dp, vertical = 3.dp),
                        )
                    }
                }
                Spacer(modifier = Modifier.height(6.dp))

                OutlinedTextField(
                    value = passwordDraft,
                    onValueChange = {
                        passwordDraft = it
                        if (passwordMinLenError && it.length >= MIN_PERMANENT_PASSWORD_LENGTH) {
                            passwordMinLenError = false
                        }
                    },
                    singleLine = true,
                    enabled = !isHashingHostPassword,
                    isError = passwordMinLenError,
                    placeholder = {
                        Text(
                            text = str(R.string.settings_host_perm_pw_placeholder),
                            color = colors.textMuted,
                        )
                    },
                    visualTransformation = if (showPassword) {
                        VisualTransformation.None
                    } else {
                        PasswordVisualTransformation()
                    },
                    trailingIcon = {
                        IconButton(onClick = { showPassword = !showPassword }) {
                            Icon(
                                imageVector = if (showPassword) {
                                    Icons.Rounded.VisibilityOff
                                } else {
                                    Icons.Rounded.Visibility
                                },
                                contentDescription = if (showPassword) {
                                    str(R.string.auth_hide_password)
                                } else {
                                    str(R.string.auth_show_password)
                                },
                                tint = colors.textSecondary,
                            )
                        }
                    },
                    supportingText = if (passwordMinLenError) {
                        {
                            Text(
                                text = str(R.string.settings_host_perm_pw_min_len),
                                color = colors.danger,
                            )
                        }
                    } else {
                        null
                    },
                    keyboardOptions = KeyboardOptions(
                        keyboardType = KeyboardType.Password,
                        autoCorrectEnabled = false,
                        imeAction = ImeAction.Done,
                    ),
                    keyboardActions = KeyboardActions(onDone = { submitPermanentPassword() }),
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

                Spacer(modifier = Modifier.height(8.dp))
                Row(
                    horizontalArrangement = Arrangement.spacedBy(10.dp),
                    modifier = Modifier.fillMaxWidth(),
                ) {
                    Button(
                        onClick = ::submitPermanentPassword,
                        enabled = !isHashingHostPassword && passwordDraft.isNotEmpty(),
                        colors = ButtonDefaults.buttonColors(
                            containerColor = colors.primary,
                            contentColor = colors.onPrimary,
                        ),
                        shape = RoundedCornerShape(10.dp),
                        modifier = Modifier.weight(1f),
                    ) {
                        if (isHashingHostPassword) {
                            CircularProgressIndicator(
                                modifier = Modifier.size(16.dp),
                                strokeWidth = 2.dp,
                                color = colors.onPrimary,
                            )
                            Spacer(modifier = Modifier.width(8.dp))
                        }
                        Text(text = str(R.string.btn_save), fontWeight = FontWeight.SemiBold)
                    }
                    if (settings.hasHostPermanentPassword) {
                        OutlinedButton(
                            onClick = {
                                passwordDraft = ""
                                showPassword = false
                                passwordMinLenError = false
                                onClearHostPermanentPassword()
                            },
                            border = BorderStroke(1.dp, colors.danger.copy(alpha = 0.6f)),
                            shape = RoundedCornerShape(10.dp),
                        ) {
                            Text(text = str(R.string.btn_clear), color = colors.danger)
                        }
                    }
                }

                Spacer(modifier = Modifier.height(12.dp))
                HorizontalDivider(color = colors.border.copy(alpha = 0.6f))
                Spacer(modifier = Modifier.height(6.dp))

                SettingsToggleRow(
                    title = str(R.string.settings_host_require_accept),
                    subtitle = str(R.string.settings_host_require_accept_desc),
                    checked = settings.hostRequireAccept,
                    onCheckedChange = onHostRequireAcceptChange,
                )

                Spacer(modifier = Modifier.height(10.dp))
                HorizontalDivider(color = colors.border.copy(alpha = 0.6f))
                Spacer(modifier = Modifier.height(10.dp))

                Text(
                    text = str(R.string.settings_host_default_perms_title),
                    style = MaterialTheme.typography.labelMedium,
                    color = colors.textSecondary,
                )
                Spacer(modifier = Modifier.height(4.dp))
                SettingsToggleRow(
                    title = str(R.string.settings_host_allow_input),
                    subtitle = str(R.string.settings_host_allow_input_desc),
                    checked = settings.hostAllowInput,
                    onCheckedChange = onHostAllowInputChange,
                )
                SettingsToggleRow(
                    title = str(R.string.settings_host_allow_clipboard),
                    subtitle = str(R.string.settings_host_allow_clipboard_desc),
                    checked = settings.hostAllowClipboard,
                    onCheckedChange = onHostAllowClipboardChange,
                )
                SettingsToggleRow(
                    title = str(R.string.settings_host_allow_audio),
                    subtitle = str(R.string.settings_host_allow_audio_desc),
                    checked = settings.hostAllowAudio,
                    onCheckedChange = onHostAllowAudioChange,
                )
                SettingsToggleRow(
                    title = str(R.string.settings_host_allow_chat),
                    subtitle = str(R.string.settings_host_allow_chat_desc),
                    checked = settings.hostAllowChat,
                    onCheckedChange = onHostAllowChatChange,
                )

                Spacer(modifier = Modifier.height(10.dp))
                HorizontalDivider(color = colors.border.copy(alpha = 0.6f))
                Spacer(modifier = Modifier.height(10.dp))

                Text(
                    text = str(R.string.settings_host_trusted_title),
                    style = MaterialTheme.typography.labelMedium,
                    color = colors.textSecondary,
                )
                Spacer(modifier = Modifier.height(6.dp))

                if (settings.hostTrustedPeers.isEmpty()) {
                    Text(
                        text = str(R.string.settings_host_trusted_empty),
                        style = MaterialTheme.typography.bodySmall,
                        color = colors.textMuted,
                    )
                } else {
                    settings.hostTrustedPeers.forEachIndexed { idx, peer ->
                        if (idx > 0) {
                            HorizontalDivider(color = colors.border.copy(alpha = 0.45f))
                        }
                        TrustedPeerRow(
                            peer = peer,
                            onRevoke = { onRemoveHostTrustedPeer(peer.publicKeyHex) },
                        )
                    }
                }
            }

            // Network & P2P
            SettingsSectionCard(title = str(R.string.settings_section_network)) {
                Text(
                    text = str(R.string.settings_rendezvous_label),
                    style = MaterialTheme.typography.labelMedium,
                    color = colors.textSecondary,
                )
                Spacer(modifier = Modifier.height(6.dp))
                OutlinedTextField(
                    value = settings.rendezvousServer,
                    onValueChange = onRendezvousChange,
                    singleLine = true,
                    placeholder = {
                        Text(
                            text = str(R.string.settings_rendezvous_placeholder),
                            color = colors.textMuted,
                        )
                    },
                    supportingText = {
                        Text(
                            text = str(R.string.settings_rendezvous_hint),
                            color = colors.textMuted,
                        )
                    },
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
                Spacer(modifier = Modifier.height(8.dp))
                SettingsToggleRow(
                    title = str(R.string.settings_relay_only),
                    subtitle = str(R.string.settings_relay_only_desc),
                    checked = settings.relayOnly,
                    onCheckedChange = onRelayOnlyChange,
                )
            }

            // Independent Desktop / Gaming Stream Profile
            SettingsSectionCard(title = str(R.string.settings_section_profile)) {
                Row(horizontalArrangement = Arrangement.spacedBy(10.dp)) {
                    SettingChip(
                        selected = editingMode == BridgeConstants.MODE_DESKTOP,
                        label = str(R.string.mode_desktop),
                        onClick = { editingMode = BridgeConstants.MODE_DESKTOP },
                        modifier = Modifier.weight(1f),
                    )
                    SettingChip(
                        selected = editingMode == BridgeConstants.MODE_GAMING,
                        label = str(R.string.mode_gaming),
                        onClick = { editingMode = BridgeConstants.MODE_GAMING },
                        modifier = Modifier.weight(1f),
                    )
                }

                Spacer(modifier = Modifier.height(14.dp))
                Text(
                    text = str(R.string.settings_quality),
                    style = MaterialTheme.typography.labelMedium,
                    color = colors.textSecondary,
                )
                Spacer(modifier = Modifier.height(6.dp))
                FlowRow(
                    horizontalArrangement = Arrangement.spacedBy(8.dp),
                    verticalArrangement = Arrangement.spacedBy(6.dp),
                ) {
                    val presets = listOf(
                        BitratePreset.LOW to str(R.string.quality_low),
                        BitratePreset.BALANCED to str(R.string.quality_balanced),
                        BitratePreset.HIGH to str(R.string.quality_high),
                        BitratePreset.ULTRA to str(R.string.quality_ultra),
                    )
                    presets.forEach { (preset, label) ->
                        SettingChip(
                            selected = BitratePreset.nearest(profile.bitrateKbps) == preset,
                            label = label,
                            onClick = {
                                onUpdateProfile(editingMode) { it.copy(bitrateKbps = preset.kbps) }
                            },
                        )
                    }
                }

                Spacer(modifier = Modifier.height(14.dp))
                Text(
                    text = str(R.string.settings_fps_cap),
                    style = MaterialTheme.typography.labelMedium,
                    color = colors.textSecondary,
                )
                Spacer(modifier = Modifier.height(6.dp))
                Row(horizontalArrangement = Arrangement.spacedBy(10.dp)) {
                    SettingChip(
                        selected = profile.fpsCap <= 60,
                        label = str(R.string.fps_60),
                        onClick = { onUpdateProfile(editingMode) { it.copy(fpsCap = 60) } },
                        modifier = Modifier.weight(1f),
                    )
                    SettingChip(
                        selected = profile.fpsCap >= 120,
                        label = str(R.string.fps_120),
                        onClick = { onUpdateProfile(editingMode) { it.copy(fpsCap = 120) } },
                        modifier = Modifier.weight(1f),
                    )
                }

                Spacer(modifier = Modifier.height(14.dp))
                Text(
                    text = str(R.string.settings_scaling),
                    style = MaterialTheme.typography.labelMedium,
                    color = colors.textSecondary,
                )
                Spacer(modifier = Modifier.height(6.dp))
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    listOf(
                        ScaleMode.FIT to str(R.string.scale_fit),
                        ScaleMode.STRETCH to str(R.string.scale_stretch),
                        ScaleMode.ORIGINAL to str(R.string.scale_original),
                    ).forEach { (mode, label) ->
                        SettingChip(
                            selected = profile.scaleMode == mode,
                            label = label,
                            onClick = { onUpdateProfile(editingMode) { it.copy(scaleMode = mode) } },
                            modifier = Modifier.weight(1f),
                        )
                    }
                }

                Spacer(modifier = Modifier.height(14.dp))
                Text(
                    text = str(R.string.settings_touch_mode),
                    style = MaterialTheme.typography.labelMedium,
                    color = colors.textSecondary,
                )
                Spacer(modifier = Modifier.height(6.dp))
                Row(horizontalArrangement = Arrangement.spacedBy(10.dp)) {
                    SettingChip(
                        selected = profile.touchMode == TouchMode.TRACKPAD,
                        label = str(R.string.touch_trackpad),
                        onClick = { onUpdateProfile(editingMode) { it.copy(touchMode = TouchMode.TRACKPAD) } },
                        modifier = Modifier.weight(1f),
                    )
                    SettingChip(
                        selected = profile.touchMode == TouchMode.DIRECT,
                        label = str(R.string.touch_direct),
                        onClick = { onUpdateProfile(editingMode) { it.copy(touchMode = TouchMode.DIRECT) } },
                        modifier = Modifier.weight(1f),
                    )
                }

                Spacer(modifier = Modifier.height(12.dp))
                HorizontalDivider(color = colors.border.copy(alpha = 0.6f))
                Spacer(modifier = Modifier.height(8.dp))

                SettingsToggleRow(
                    title = str(R.string.settings_yuv444),
                    subtitle = str(R.string.settings_yuv444_desc),
                    checked = profile.preferYuv444,
                    onCheckedChange = { enabled ->
                        onUpdateProfile(editingMode) { it.copy(preferYuv444 = enabled) }
                    },
                )
                SettingsToggleRow(
                    title = str(R.string.settings_clipboard),
                    subtitle = str(R.string.settings_clipboard_desc),
                    checked = profile.clipboardEnabled,
                    onCheckedChange = { enabled ->
                        onUpdateProfile(editingMode) { it.copy(clipboardEnabled = enabled) }
                    },
                )
                SettingsToggleRow(
                    title = str(R.string.settings_show_hud),
                    subtitle = str(R.string.settings_show_hud_desc),
                    checked = profile.showHudOnConnect,
                    onCheckedChange = { enabled ->
                        onUpdateProfile(editingMode) { it.copy(showHudOnConnect = enabled) }
                    },
                )

                Spacer(modifier = Modifier.height(10.dp))
                HorizontalDivider(color = colors.border.copy(alpha = 0.6f))
                Spacer(modifier = Modifier.height(10.dp))

                Text(
                    text = str(R.string.settings_section_audio_voice),
                    style = MaterialTheme.typography.labelMedium,
                    color = colors.primary,
                    fontWeight = FontWeight.SemiBold,
                )
                Spacer(modifier = Modifier.height(4.dp))

                SettingsToggleRow(
                    title = str(R.string.settings_audio),
                    subtitle = str(R.string.settings_audio_desc),
                    checked = profile.audioEnabled,
                    onCheckedChange = { enabled ->
                        onUpdateProfile(editingMode) { it.copy(audioEnabled = enabled) }
                    },
                )
                SettingsToggleRow(
                    title = str(R.string.settings_mic),
                    subtitle = str(R.string.settings_mic_desc),
                    checked = profile.micEnabled,
                    onCheckedChange = { enabled ->
                        onUpdateProfile(editingMode) { it.copy(micEnabled = enabled) }
                    },
                )
                SettingsToggleRow(
                    title = str(R.string.settings_aec),
                    subtitle = str(R.string.settings_aec_desc),
                    checked = profile.echoCancellation,
                    onCheckedChange = { enabled ->
                        onUpdateProfile(editingMode) { it.copy(echoCancellation = enabled) }
                    },
                )
                SettingsToggleRow(
                    title = str(R.string.settings_noise_suppression),
                    subtitle = str(R.string.settings_noise_suppression_desc),
                    checked = profile.noiseSuppression,
                    onCheckedChange = { enabled ->
                        onUpdateProfile(editingMode) { it.copy(noiseSuppression = enabled) }
                    },
                )

                Spacer(modifier = Modifier.height(10.dp))
                Text(
                    text = str(R.string.settings_audio_frame_latency),
                    style = MaterialTheme.typography.labelMedium,
                    color = colors.textSecondary,
                )
                Spacer(modifier = Modifier.height(6.dp))
                FlowRow(
                    horizontalArrangement = Arrangement.spacedBy(8.dp),
                    verticalArrangement = Arrangement.spacedBy(6.dp),
                ) {
                    val autoLabel = if (editingMode == BridgeConstants.MODE_GAMING) {
                        str(R.string.audio_latency_auto_gaming)
                    } else {
                        str(R.string.audio_latency_auto_desktop)
                    }
                    val latencies = listOf(
                        StreamProfile.AUDIO_FRAME_LATENCY_AUTO to autoLabel,
                        2 to str(R.string.audio_latency_2_5ms),
                        5 to str(R.string.audio_latency_5ms),
                        10 to str(R.string.audio_latency_10ms),
                        20 to str(R.string.audio_latency_20ms),
                    )
                    latencies.forEach { (ms, label) ->
                        SettingChip(
                            selected = profile.audioFrameLatencyMs == ms,
                            label = label,
                            onClick = {
                                onUpdateProfile(editingMode) { it.copy(audioFrameLatencyMs = ms) }
                            },
                        )
                    }
                }

                Spacer(modifier = Modifier.height(14.dp))
                HorizontalDivider(color = colors.border.copy(alpha = 0.6f))
                Spacer(modifier = Modifier.height(12.dp))

                OutlinedButton(
                    onClick = { onResetProfileDefaults(StreamMode.fromId(editingMode)) },
                    border = BorderStroke(1.dp, colors.border),
                    shape = RoundedCornerShape(10.dp),
                    modifier = Modifier.fillMaxWidth(),
                ) {
                    Icon(
                        imageVector = Icons.Rounded.RestartAlt,
                        contentDescription = null,
                        tint = colors.textPrimary,
                        modifier = Modifier.size(18.dp),
                    )
                    Spacer(modifier = Modifier.width(8.dp))
                    Text(
                        text = str(R.string.btn_reset_profile_defaults),
                        color = colors.textPrimary,
                        fontWeight = FontWeight.Medium,
                    )
                }
            }

            Spacer(modifier = Modifier.height(20.dp))
        }
    }
}

@Composable
private fun TrustedPeerRow(
    peer: HostSavedTrustedPeer,
    onRevoke: () -> Unit,
) {
    val colors = VeloxTheme.colors
    val peerName = peer.name.trim().ifEmpty { str(R.string.host_peer_unknown) }
    val formattedId = AppPreferences.formatDeviceId(peer.deviceId)
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .padding(vertical = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Icon(
            imageVector = Icons.Rounded.Devices,
            contentDescription = null,
            tint = colors.primary,
            modifier = Modifier.size(20.dp),
        )
        Spacer(modifier = Modifier.width(10.dp))
        Column(modifier = Modifier.weight(1f)) {
            Text(
                text = peerName,
                style = MaterialTheme.typography.bodyMedium,
                fontWeight = FontWeight.Medium,
                color = colors.textPrimary,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
            val subtitle = buildString {
                if (formattedId.isNotEmpty()) append(formattedId)
                if (peer.fingerprint.isNotBlank()) {
                    if (isNotEmpty()) append(" · ")
                    append(peer.fingerprint)
                }
            }
            if (subtitle.isNotEmpty()) {
                Text(
                    text = subtitle,
                    style = MaterialTheme.typography.labelSmall,
                    fontFamily = FontFamily.Monospace,
                    color = colors.textSecondary,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
            }
        }
        IconButton(onClick = onRevoke) {
            Icon(
                imageVector = Icons.Rounded.DeleteOutline,
                contentDescription = str(R.string.btn_revoke),
                tint = colors.danger,
                modifier = Modifier.size(19.dp),
            )
        }
    }
}

@Composable
private fun SettingsSectionCard(
    title: String,
    content: @Composable () -> Unit,
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
                text = title,
                style = MaterialTheme.typography.titleMedium,
                color = colors.primary,
                fontWeight = FontWeight.SemiBold,
            )
            Spacer(modifier = Modifier.height(12.dp))
            content()
        }
    }
}

@Composable
private fun SettingChip(
    selected: Boolean,
    label: String,
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
        colors = FilterChipDefaults.filterChipColors(
            selectedContainerColor = colors.primary.copy(alpha = 0.2f),
            selectedLabelColor = colors.primary,
            containerColor = colors.background,
            labelColor = colors.textSecondary,
        ),
        border = FilterChipDefaults.filterChipBorder(
            enabled = true,
            selected = selected,
            borderColor = colors.border,
            selectedBorderColor = colors.primary,
        ),
        shape = RoundedCornerShape(10.dp),
        modifier = modifier,
    )
}

@Composable
private fun SettingsToggleRow(
    title: String,
    subtitle: String,
    checked: Boolean,
    onCheckedChange: (Boolean) -> Unit,
) {
    val colors = VeloxTheme.colors
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .padding(vertical = 6.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Column(modifier = Modifier.weight(1f)) {
            Text(
                text = title,
                style = MaterialTheme.typography.bodyMedium,
                color = colors.textPrimary,
            )
            Text(
                text = subtitle,
                style = MaterialTheme.typography.bodySmall,
                color = colors.textSecondary,
            )
        }
        Spacer(modifier = Modifier.width(12.dp))
        Switch(
            checked = checked,
            onCheckedChange = onCheckedChange,
            colors = SwitchDefaults.colors(
                checkedThumbColor = colors.onPrimary,
                checkedTrackColor = colors.primary,
                uncheckedThumbColor = colors.textSecondary,
                uncheckedTrackColor = colors.background,
                uncheckedBorderColor = colors.border,
            ),
        )
    }
}
