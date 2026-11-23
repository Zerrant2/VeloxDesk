package dev.veloxdesk.ui

import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.rounded.Shield
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.Checkbox
import androidx.compose.material3.CheckboxDefaults
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.DialogProperties
import dev.veloxdesk.R
import dev.veloxdesk.bridge.HostApprovalRequest
import dev.veloxdesk.data.AppPreferences
import dev.veloxdesk.ui.i18n.str
import dev.veloxdesk.ui.theme.VeloxTheme

/**
 * Modal approval dialog displayed on the Android Host whenever a remote viewer requests access
 * and `hostRequireAccept` is enabled (or the peer is not yet in Trusted Devices).
 */
@Composable
fun HostAcceptDialog(
    request: HostApprovalRequest,
    onAccept: (rememberPeer: Boolean, allowInput: Boolean, allowClipboard: Boolean, allowAudio: Boolean) -> Unit,
    onReject: () -> Unit,
) {
    val colors = VeloxTheme.colors
    var allowInput by remember(request.requestId) { mutableStateOf(true) }
    var allowClipboard by remember(request.requestId) { mutableStateOf(true) }
    var allowAudio by remember(request.requestId) { mutableStateOf(true) }
    var rememberPeer by remember(request.requestId) { mutableStateOf(false) }

    val callerName = request.peerName.trim().ifEmpty { str(R.string.host_peer_unknown) }
    val formattedCallerId = remember(request.peerDeviceId) {
        AppPreferences.formatDeviceId(request.peerDeviceId)
    }

    AlertDialog(
        onDismissRequest = onReject,
        properties = DialogProperties(
            dismissOnBackPress = true,
            dismissOnClickOutside = false,
        ),
        containerColor = colors.surface,
        titleContentColor = colors.textPrimary,
        textContentColor = colors.textPrimary,
        icon = {
            Icon(
                imageVector = Icons.Rounded.Shield,
                contentDescription = null,
                tint = colors.primary,
                modifier = Modifier.size(28.dp),
            )
        },
        title = {
            Text(
                text = str(R.string.host_accept_title),
                style = MaterialTheme.typography.titleLarge,
                color = colors.textPrimary,
            )
        },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                Text(
                    text = callerName,
                    style = MaterialTheme.typography.titleMedium,
                    fontWeight = FontWeight.Bold,
                    color = colors.textPrimary,
                )
                if (formattedCallerId.isNotEmpty()) {
                    Text(
                        text = str(R.string.host_accept_peer_id, formattedCallerId),
                        style = MaterialTheme.typography.bodySmall,
                        color = colors.primary,
                        fontFamily = FontFamily.Monospace,
                    )
                }

                if (request.peerFingerprint.isNotBlank()) {
                    Column(verticalArrangement = Arrangement.spacedBy(3.dp)) {
                        Text(
                            text = str(R.string.host_accept_peer_fp),
                            style = MaterialTheme.typography.labelMedium,
                            color = colors.textSecondary,
                        )
                        Surface(
                            color = colors.background,
                            shape = RoundedCornerShape(8.dp),
                            border = BorderStroke(1.dp, colors.border),
                            modifier = Modifier.fillMaxWidth(),
                        ) {
                            Text(
                                text = request.peerFingerprint,
                                style = MaterialTheme.typography.labelSmall,
                                color = colors.textPrimary,
                                modifier = Modifier.padding(horizontal = 10.dp, vertical = 7.dp),
                            )
                        }
                    }
                }

                Spacer(modifier = Modifier.height(2.dp))
                Text(
                    text = str(R.string.host_accept_permissions),
                    style = MaterialTheme.typography.labelMedium,
                    color = colors.textSecondary,
                )

                PermissionCheckRow(
                    label = str(R.string.host_accept_perm_input),
                    checked = allowInput,
                    onCheckedChange = { allowInput = it },
                )
                PermissionCheckRow(
                    label = str(R.string.host_accept_perm_clipboard),
                    checked = allowClipboard,
                    onCheckedChange = { allowClipboard = it },
                )
                PermissionCheckRow(
                    label = str(R.string.host_accept_perm_audio),
                    checked = allowAudio,
                    onCheckedChange = { allowAudio = it },
                )

                HorizontalDivider(color = colors.border.copy(alpha = 0.6f))

                PermissionCheckRow(
                    label = str(R.string.host_accept_remember),
                    checked = rememberPeer,
                    onCheckedChange = { rememberPeer = it },
                    highlight = true,
                )
            }
        },
        confirmButton = {
            Button(
                onClick = { onAccept(rememberPeer, allowInput, allowClipboard, allowAudio) },
                colors = ButtonDefaults.buttonColors(
                    containerColor = colors.success,
                    contentColor = colors.onPrimary,
                ),
                shape = RoundedCornerShape(10.dp),
            ) {
                Text(text = str(R.string.btn_accept), fontWeight = FontWeight.Bold)
            }
        },
        dismissButton = {
            TextButton(onClick = onReject) {
                Text(text = str(R.string.btn_reject), color = colors.danger, fontWeight = FontWeight.SemiBold)
            }
        },
    )
}

@Composable
private fun PermissionCheckRow(
    label: String,
    checked: Boolean,
    onCheckedChange: (Boolean) -> Unit,
    highlight: Boolean = false,
) {
    val colors = VeloxTheme.colors
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .clip(RoundedCornerShape(8.dp))
            .background(if (highlight && checked) colors.primary.copy(alpha = 0.10f) else colors.surface)
            .clickable { onCheckedChange(!checked) }
            .padding(vertical = 2.dp, horizontal = 2.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Checkbox(
            checked = checked,
            onCheckedChange = onCheckedChange,
            colors = CheckboxDefaults.colors(
                checkedColor = colors.primary,
                uncheckedColor = colors.textSecondary,
                checkmarkColor = colors.onPrimary,
            ),
            modifier = Modifier.size(32.dp),
        )
        Spacer(modifier = Modifier.width(8.dp))
        Text(
            text = label,
            style = MaterialTheme.typography.bodySmall,
            color = if (highlight) colors.textPrimary else colors.textSecondary,
            fontWeight = if (highlight) FontWeight.Medium else FontWeight.Normal,
        )
    }
}
