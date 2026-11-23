package dev.veloxdesk.ui

import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.rounded.Chat
import androidx.compose.material.icons.automirrored.rounded.Send
import androidx.compose.material.icons.rounded.Close
import androidx.compose.material.icons.rounded.Done
import androidx.compose.material.icons.rounded.DoneAll
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.IconButtonDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.OutlinedTextFieldDefaults
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import dev.veloxdesk.R
import dev.veloxdesk.bridge.ChatMessageItem
import dev.veloxdesk.bridge.ChatState
import dev.veloxdesk.ui.i18n.str
import dev.veloxdesk.ui.theme.VeloxColors
import dev.veloxdesk.ui.theme.VeloxTheme

/**
 * Small pill badge displaying unread chat message count (`1..99+`).
 */
@Composable
fun UnreadCountBadge(
    count: Int,
    modifier: Modifier = Modifier,
) {
    if (count <= 0) return
    val label = if (count > 99) "99+" else count.toString()
    Box(
        contentAlignment = Alignment.Center,
        modifier = modifier
            .clip(RoundedCornerShape(999.dp))
            .background(VeloxColors.Danger)
            .padding(horizontal = 5.dp, vertical = 1.dp),
    ) {
        Text(
            text = label,
            color = Color.White,
            fontSize = 10.sp,
            fontWeight = FontWeight.Bold,
            lineHeight = 12.sp,
        )
    }
}

/**
 * Reusable in-session chat panel content (header + auto-scrolling message list + input bar).
 *
 * Supports both the dark translucent overlay palette (`useDarkOverlayPalette = true`, used inside
 * [SessionScreen]) and the adaptive [VeloxTheme.colors] palette (`useDarkOverlayPalette = false`,
 * used for Android Host chat dialog on [ConnectScreen]).
 */
@Composable
fun ChatPanelContent(
    title: String,
    subtitle: String?,
    messages: List<ChatMessageItem>,
    onSendMessage: (String) -> Unit,
    onClose: () -> Unit,
    modifier: Modifier = Modifier,
    useDarkOverlayPalette: Boolean = true,
) {
    val themeColors = VeloxTheme.colors
    val surfaceBg = if (useDarkOverlayPalette) VeloxColors.Surface.copy(alpha = 0.96f) else themeColors.surface
    val fieldBg = if (useDarkOverlayPalette) VeloxColors.Background else themeColors.background
    val borderColor = if (useDarkOverlayPalette) VeloxColors.Border else themeColors.border
    val primaryColor = if (useDarkOverlayPalette) VeloxColors.Primary else themeColors.primary
    val onPrimaryColor = if (useDarkOverlayPalette) VeloxColors.OnPrimary else themeColors.onPrimary
    val incomingBg = if (useDarkOverlayPalette) VeloxColors.SurfaceElevated else themeColors.surfaceElevated
    val outgoingBg = primaryColor.copy(alpha = if (useDarkOverlayPalette) 0.22f else 0.16f)
    val textPrimary = if (useDarkOverlayPalette) VeloxColors.TextPrimary else themeColors.textPrimary
    val textSecondary = if (useDarkOverlayPalette) VeloxColors.TextSecondary else themeColors.textSecondary
    val textMuted = if (useDarkOverlayPalette) VeloxColors.TextMuted else themeColors.textMuted
    val deliveredColor = if (useDarkOverlayPalette) VeloxColors.Emerald else themeColors.success

    var draft by remember { mutableStateOf("") }
    val listState = rememberLazyListState()

    LaunchedEffect(messages.size) {
        if (messages.isNotEmpty()) {
            listState.animateScrollToItem(messages.lastIndex)
        }
    }

    fun submitDraft() {
        val trimmed = draft.trim()
        if (trimmed.isNotEmpty()) {
            onSendMessage(trimmed)
            draft = ""
        }
    }

    Surface(
        color = surfaceBg,
        shape = RoundedCornerShape(16.dp),
        border = BorderStroke(1.dp, borderColor),
        modifier = modifier,
    ) {
        Column(modifier = Modifier.fillMaxWidth()) {
            // Header
            Row(
                verticalAlignment = Alignment.CenterVertically,
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(start = 14.dp, end = 6.dp, top = 8.dp, bottom = 8.dp),
            ) {
                Icon(
                    imageVector = Icons.AutoMirrored.Rounded.Chat,
                    contentDescription = null,
                    tint = primaryColor,
                    modifier = Modifier.size(18.dp),
                )
                Spacer(modifier = Modifier.width(8.dp))
                Column(modifier = Modifier.weight(1f)) {
                    Text(
                        text = title,
                        style = MaterialTheme.typography.titleMedium,
                        color = textPrimary,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                    if (!subtitle.isNullOrBlank()) {
                        Text(
                            text = subtitle,
                            style = MaterialTheme.typography.labelSmall,
                            color = textSecondary,
                            maxLines = 1,
                            overflow = TextOverflow.Ellipsis,
                        )
                    }
                }
                IconButton(onClick = onClose, modifier = Modifier.size(34.dp)) {
                    Icon(
                        imageVector = Icons.Rounded.Close,
                        contentDescription = str(R.string.btn_close),
                        tint = textSecondary,
                        modifier = Modifier.size(18.dp),
                    )
                }
            }

            HorizontalDivider(color = borderColor.copy(alpha = 0.6f))

            // Messages list
            if (messages.isEmpty()) {
                Box(
                    contentAlignment = Alignment.Center,
                    modifier = Modifier
                        .fillMaxWidth()
                        .weight(1f)
                        .padding(20.dp),
                ) {
                    Text(
                        text = str(R.string.chat_empty),
                        style = MaterialTheme.typography.bodySmall,
                        color = textMuted,
                    )
                }
            } else {
                LazyColumn(
                    state = listState,
                    contentPadding = PaddingValues(horizontal = 12.dp, vertical = 10.dp),
                    verticalArrangement = Arrangement.spacedBy(8.dp),
                    modifier = Modifier
                        .fillMaxWidth()
                        .weight(1f),
                ) {
                    items(
                        items = messages,
                        key = { msg -> "${if (msg.outgoing) "out" else "in"}-${msg.id}-${msg.tsMs}" },
                    ) { msg ->
                        ChatMessageBubble(
                            message = msg,
                            outgoingBg = outgoingBg,
                            incomingBg = incomingBg,
                            borderColor = borderColor,
                            primaryColor = primaryColor,
                            deliveredColor = deliveredColor,
                            textPrimary = textPrimary,
                            textSecondary = textSecondary,
                            textMuted = textMuted,
                        )
                    }
                }
            }

            HorizontalDivider(color = borderColor.copy(alpha = 0.6f))

            // Input bar
            Row(
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(8.dp),
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(horizontal = 10.dp, vertical = 8.dp),
            ) {
                OutlinedTextField(
                    value = draft,
                    onValueChange = { draft = it },
                    placeholder = {
                        Text(
                            text = str(R.string.chat_input_placeholder),
                            style = MaterialTheme.typography.bodySmall,
                            color = textMuted,
                        )
                    },
                    singleLine = false,
                    maxLines = 3,
                    textStyle = MaterialTheme.typography.bodyMedium.copy(color = textPrimary),
                    keyboardOptions = KeyboardOptions(imeAction = ImeAction.Send),
                    keyboardActions = KeyboardActions(onSend = { submitDraft() }),
                    colors = OutlinedTextFieldDefaults.colors(
                        focusedBorderColor = primaryColor,
                        unfocusedBorderColor = borderColor,
                        focusedContainerColor = fieldBg,
                        unfocusedContainerColor = fieldBg,
                        focusedTextColor = textPrimary,
                        unfocusedTextColor = textPrimary,
                    ),
                    shape = RoundedCornerShape(12.dp),
                    modifier = Modifier.weight(1f),
                )
                IconButton(
                    onClick = ::submitDraft,
                    enabled = draft.isNotBlank(),
                    colors = IconButtonDefaults.iconButtonColors(
                        containerColor = primaryColor,
                        contentColor = onPrimaryColor,
                        disabledContainerColor = borderColor.copy(alpha = 0.4f),
                        disabledContentColor = textMuted,
                    ),
                    modifier = Modifier
                        .size(42.dp)
                        .clip(CircleShape),
                ) {
                    Icon(
                        imageVector = Icons.AutoMirrored.Rounded.Send,
                        contentDescription = str(R.string.chat_send),
                        modifier = Modifier.size(19.dp),
                    )
                }
            }
        }
    }
}

@Composable
private fun ChatMessageBubble(
    message: ChatMessageItem,
    outgoingBg: Color,
    incomingBg: Color,
    borderColor: Color,
    primaryColor: Color,
    deliveredColor: Color,
    textPrimary: Color,
    textSecondary: Color,
    textMuted: Color,
) {
    val timeLabel = remember(message.tsMs) { ChatState.formatTimestamp(message.tsMs) }
    val senderLabel = if (message.outgoing) {
        str(R.string.chat_you)
    } else {
        message.senderName.ifBlank { "Peer" }
    }
    val bubbleShape = RoundedCornerShape(
        topStart = 12.dp,
        topEnd = 12.dp,
        bottomStart = if (message.outgoing) 12.dp else 4.dp,
        bottomEnd = if (message.outgoing) 4.dp else 12.dp,
    )

    Column(
        horizontalAlignment = if (message.outgoing) Alignment.End else Alignment.Start,
        modifier = Modifier.fillMaxWidth(),
    ) {
        Surface(
            color = if (message.outgoing) outgoingBg else incomingBg,
            shape = bubbleShape,
            border = BorderStroke(
                width = 1.dp,
                color = if (message.outgoing) primaryColor.copy(alpha = 0.4f) else borderColor.copy(alpha = 0.6f),
            ),
            modifier = Modifier.widthIn(min = 84.dp, max = 280.dp),
        ) {
            Column(
                modifier = Modifier.padding(horizontal = 10.dp, vertical = 7.dp),
                verticalArrangement = Arrangement.spacedBy(3.dp),
            ) {
                Text(
                    text = senderLabel,
                    style = MaterialTheme.typography.labelSmall,
                    fontWeight = FontWeight.SemiBold,
                    color = if (message.outgoing) primaryColor else textSecondary,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
                Text(
                    text = message.text,
                    style = MaterialTheme.typography.bodyMedium,
                    color = textPrimary,
                )
                Row(
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.End,
                    modifier = Modifier.align(Alignment.End),
                ) {
                    if (timeLabel.isNotEmpty()) {
                        Text(
                            text = timeLabel,
                            style = MaterialTheme.typography.labelSmall,
                            fontSize = 10.sp,
                            color = textMuted,
                        )
                    }
                    if (message.outgoing) {
                        Spacer(modifier = Modifier.width(4.dp))
                        Icon(
                            imageVector = if (message.delivered) Icons.Rounded.DoneAll else Icons.Rounded.Done,
                            contentDescription = if (message.delivered) {
                                str(R.string.chat_delivered)
                            } else {
                                str(R.string.chat_sending)
                            },
                            tint = if (message.delivered) deliveredColor else textMuted,
                            modifier = Modifier.size(13.dp),
                        )
                    }
                }
            }
        }
    }
}

/**
 * Modal chat dialog shown on the Android Host screen when chatting with an active connected viewer.
 */
@Composable
fun HostChatDialog(
    peerName: String,
    messages: List<ChatMessageItem>,
    onSendMessage: (String) -> Unit,
    onDismiss: () -> Unit,
) {
    Dialog(
        onDismissRequest = onDismiss,
        properties = DialogProperties(
            dismissOnBackPress = true,
            dismissOnClickOutside = true,
            usePlatformDefaultWidth = false,
        ),
    ) {
        ChatPanelContent(
            title = str(R.string.chat_host_title, peerName),
            subtitle = null,
            messages = messages,
            onSendMessage = onSendMessage,
            onClose = onDismiss,
            useDarkOverlayPalette = false,
            modifier = Modifier
                .fillMaxWidth(0.94f)
                .widthIn(max = 460.dp)
                .heightIn(min = 340.dp, max = 560.dp)
                .height(460.dp),
        )
    }
}
