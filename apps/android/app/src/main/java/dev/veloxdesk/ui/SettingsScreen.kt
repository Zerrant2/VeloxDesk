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
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.rounded.ArrowBack
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.FilterChip
import androidx.compose.material3.FilterChipDefaults
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.OutlinedTextFieldDefaults
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Surface
import androidx.compose.material3.Switch
import androidx.compose.material3.SwitchDefaults
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import dev.veloxdesk.R
import dev.veloxdesk.bridge.BridgeConstants
import dev.veloxdesk.bridge.ScaleMode
import dev.veloxdesk.bridge.TouchMode
import dev.veloxdesk.data.AppLanguage
import dev.veloxdesk.data.AppSettings
import dev.veloxdesk.data.BitratePreset
import dev.veloxdesk.data.StreamProfile
import dev.veloxdesk.ui.i18n.str
import dev.veloxdesk.ui.theme.VeloxColors

@OptIn(ExperimentalLayoutApi::class)
@Composable
fun SettingsScreen(
    settings: AppSettings,
    onBack: () -> Unit,
    onLanguageChange: (AppLanguage) -> Unit,
    onRendezvousChange: (String) -> Unit,
    onRelayOnlyChange: (Boolean) -> Unit,
    onUpdateProfile: (mode: Int, (StreamProfile) -> StreamProfile) -> Unit,
) {
    var editingMode by rememberSaveable { mutableIntStateOf(settings.lastMode) }
    val profile = settings.profileForMode(editingMode)

    Scaffold(
        containerColor = VeloxColors.Background,
        topBar = {
            Surface(
                color = VeloxColors.Background,
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
                            tint = VeloxColors.TextPrimary,
                        )
                    }
                    Spacer(modifier = Modifier.width(4.dp))
                    Text(
                        text = str(R.string.settings_title),
                        style = MaterialTheme.typography.titleLarge,
                        color = VeloxColors.TextPrimary,
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
            // General / Language
            SettingsSectionCard(title = str(R.string.settings_section_general)) {
                Text(
                    text = str(R.string.settings_language),
                    style = MaterialTheme.typography.labelMedium,
                    color = VeloxColors.TextSecondary,
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
            }

            // Network & P2P
            SettingsSectionCard(title = str(R.string.settings_section_network)) {
                Text(
                    text = str(R.string.settings_rendezvous_label),
                    style = MaterialTheme.typography.labelMedium,
                    color = VeloxColors.TextSecondary,
                )
                Spacer(modifier = Modifier.height(6.dp))
                OutlinedTextField(
                    value = settings.rendezvousServer,
                    onValueChange = onRendezvousChange,
                    singleLine = true,
                    placeholder = {
                        Text(
                            text = str(R.string.settings_rendezvous_placeholder),
                            color = VeloxColors.TextMuted,
                        )
                    },
                    supportingText = {
                        Text(
                            text = str(R.string.settings_rendezvous_hint),
                            color = VeloxColors.TextMuted,
                        )
                    },
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
                    color = VeloxColors.TextSecondary,
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
                    color = VeloxColors.TextSecondary,
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
                    color = VeloxColors.TextSecondary,
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
                    color = VeloxColors.TextSecondary,
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
                HorizontalDivider(color = VeloxColors.Border.copy(alpha = 0.6f))
                Spacer(modifier = Modifier.height(8.dp))

                SettingsToggleRow(
                    title = str(R.string.settings_audio),
                    subtitle = str(R.string.settings_audio_desc),
                    checked = profile.audioEnabled,
                    onCheckedChange = { enabled ->
                        onUpdateProfile(editingMode) { it.copy(audioEnabled = enabled) }
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
            }

            Spacer(modifier = Modifier.height(20.dp))
        }
    }
}

@Composable
private fun SettingsSectionCard(
    title: String,
    content: @Composable () -> Unit,
) {
    Card(
        colors = CardDefaults.cardColors(containerColor = VeloxColors.Surface),
        border = BorderStroke(1.dp, VeloxColors.Border),
        shape = RoundedCornerShape(16.dp),
        modifier = Modifier.fillMaxWidth(),
    ) {
        Column(modifier = Modifier.padding(16.dp)) {
            Text(
                text = title,
                style = MaterialTheme.typography.titleMedium,
                color = VeloxColors.Primary,
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
    FilterChip(
        selected = selected,
        onClick = onClick,
        label = { Text(text = label, maxLines = 1) },
        colors = FilterChipDefaults.filterChipColors(
            selectedContainerColor = VeloxColors.Primary.copy(alpha = 0.2f),
            selectedLabelColor = VeloxColors.Primary,
            containerColor = VeloxColors.Background,
            labelColor = VeloxColors.TextSecondary,
        ),
        border = FilterChipDefaults.filterChipBorder(
            enabled = true,
            selected = selected,
            borderColor = VeloxColors.Border,
            selectedBorderColor = VeloxColors.Primary,
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
                color = VeloxColors.TextPrimary,
            )
            Text(
                text = subtitle,
                style = MaterialTheme.typography.bodySmall,
                color = VeloxColors.TextSecondary,
            )
        }
        Spacer(modifier = Modifier.width(12.dp))
        Switch(
            checked = checked,
            onCheckedChange = onCheckedChange,
            colors = SwitchDefaults.colors(
                checkedThumbColor = VeloxColors.OnPrimary,
                checkedTrackColor = VeloxColors.Primary,
                uncheckedThumbColor = VeloxColors.TextSecondary,
                uncheckedTrackColor = VeloxColors.Background,
                uncheckedBorderColor = VeloxColors.Border,
            ),
        )
    }
}
