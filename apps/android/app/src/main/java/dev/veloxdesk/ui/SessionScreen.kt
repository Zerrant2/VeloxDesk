package dev.veloxdesk.ui

import android.app.Activity
import android.view.Surface
import android.view.SurfaceHolder
import android.view.SurfaceView
import android.view.WindowManager
import androidx.activity.compose.BackHandler
import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.animation.slideInVertically
import androidx.compose.animation.slideOutVertically
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.requiredHeight
import androidx.compose.foundation.layout.requiredWidth
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.rounded.VolumeOff
import androidx.compose.material.icons.automirrored.rounded.VolumeUp
import androidx.compose.material.icons.rounded.AspectRatio
import androidx.compose.material.icons.rounded.CallEnd
import androidx.compose.material.icons.rounded.ContentPaste
import androidx.compose.material.icons.rounded.ExpandLess
import androidx.compose.material.icons.rounded.ExpandMore
import androidx.compose.material.icons.rounded.Keyboard
import androidx.compose.material.icons.rounded.KeyboardCommandKey
import androidx.compose.material.icons.rounded.Lock
import androidx.compose.material.icons.rounded.Monitor
import androidx.compose.material.icons.rounded.Mouse
import androidx.compose.material.icons.rounded.Speed
import androidx.compose.material.icons.rounded.TouchApp
import androidx.compose.material.icons.rounded.Visibility
import androidx.compose.material.icons.rounded.VisibilityOff
import androidx.compose.material.icons.rounded.ZoomOutMap
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.OutlinedTextFieldDefaults
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.ExperimentalComposeUiApi
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.clipToBounds
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.input.pointer.pointerInteropFilter
import androidx.compose.ui.layout.onSizeChanged
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.LocalSoftwareKeyboardController
import androidx.compose.ui.text.TextRange
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardCapitalization
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.text.input.TextFieldValue
import androidx.compose.ui.text.input.VisualTransformation
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.compose.ui.viewinterop.AndroidView
import androidx.compose.ui.window.DialogProperties
import androidx.core.view.WindowCompat
import androidx.core.view.WindowInsetsCompat
import androidx.core.view.WindowInsetsControllerCompat
import dev.veloxdesk.R
import dev.veloxdesk.bridge.AuthPromptData
import dev.veloxdesk.bridge.BridgeConstants
import dev.veloxdesk.bridge.HudStats
import dev.veloxdesk.bridge.ScaleMode
import dev.veloxdesk.bridge.TouchMode
import dev.veloxdesk.data.AppPreferences
import dev.veloxdesk.data.BitratePreset
import dev.veloxdesk.input.KeyMapper
import dev.veloxdesk.input.SpecialKeyDef
import dev.veloxdesk.ui.i18n.str
import dev.veloxdesk.ui.theme.VeloxColors
import dev.veloxdesk.ui.theme.VeloxTheme
import java.util.Locale

/**
 * Normalizes password/OTP input when auto-uppercase OTP mode is enabled:
 * strips whitespace and converts ASCII lowercase letters to uppercase.
 * Leaves input completely untouched when [uppercaseMode] is `false`.
 */
internal fun transformPasswordInput(input: String, uppercaseMode: Boolean): String {
    if (!uppercaseMode) return input
    val sb = StringBuilder(input.length)
    for (ch in input) {
        if (ch.isWhitespace()) continue
        sb.append(if (ch in 'a'..'z') ch.uppercaseChar() else ch)
    }
    return sb.toString()
}

@OptIn(ExperimentalComposeUiApi::class)
@Composable
fun SessionScreen(
    state: SessionUiState,
    onAttachSurface: (Surface) -> Unit,
    onDetachSurface: () -> Unit,
    onViewSizeChanged: (Int, Int) -> Unit,
    onTouchEvent: (android.view.MotionEvent) -> Boolean,
    onSubmitPassword: (String) -> Unit,
    onCancelAuth: () -> Unit,
    onDisconnect: () -> Unit,
    onToggleTouchMode: () -> Unit,
    onCycleScaleMode: () -> Unit,
    onResetZoom: () -> Unit,
    onToggleAudio: () -> Unit,
    onToggleClipboard: () -> Unit,
    onToggleHud: () -> Unit,
    onSelectBitrate: (Int) -> Unit,
    onSwitchDisplay: (Int) -> Unit,
    onSpecialKeyPressed: (SpecialKeyDef) -> Unit,
    onSendCtrlAltDel: () -> Unit,
    onSendSoftText: (String) -> Unit,
    onSendSoftBackspace: () -> Unit,
    onSendSoftEnter: () -> Unit,
) {
    val context = LocalContext.current
    val keyboardController = LocalSoftwareKeyboardController.current
    val imeFocusRequester = remember { FocusRequester() }

    var toolbarExpanded by remember { mutableStateOf(false) }
    var specialKeysVisible by remember { mutableStateOf(false) }
    var functionKeysExpanded by remember { mutableStateOf(false) }
    var softKeyboardActive by remember { mutableStateOf(false) }
    var showDisconnectConfirm by remember { mutableStateOf(false) }

    // Immersive fullscreen + keep screen on during remote session
    DisposableEffect(context) {
        val window = (context as? Activity)?.window
        val controller = window?.let { WindowCompat.getInsetsController(it, it.decorView) }
        window?.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
        controller?.apply {
            systemBarsBehavior = WindowInsetsControllerCompat.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE
            hide(WindowInsetsCompat.Type.systemBars())
        }
        onDispose {
            window?.clearFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
            controller?.show(WindowInsetsCompat.Type.systemBars())
        }
    }

    LaunchedEffect(softKeyboardActive) {
        if (softKeyboardActive) {
            runCatching { imeFocusRequester.requestFocus() }
            keyboardController?.show()
        } else {
            keyboardController?.hide()
        }
    }

    BackHandler(enabled = true) {
        when {
            state.authPrompt != null -> onCancelAuth()
            toolbarExpanded -> toolbarExpanded = false
            specialKeysVisible -> specialKeysVisible = false
            else -> showDisconnectConfirm = true
        }
    }

    Box(
        modifier = Modifier
            .fillMaxSize()
            .background(Color.Black),
    ) {
        // Hidden soft-keyboard IME bridge
        SoftKeyboardBridge(
            focusRequester = imeFocusRequester,
            onTextCommitted = onSendSoftText,
            onBackspace = onSendSoftBackspace,
            onEnter = onSendSoftEnter,
        )

        // Fullscreen video viewport + touch handler
        BoxWithConstraints(
            contentAlignment = Alignment.Center,
            modifier = Modifier
                .fillMaxSize()
                .clipToBounds()
                .onSizeChanged { size -> onViewSizeChanged(size.width, size.height) }
                .pointerInteropFilter { event -> onTouchEvent(event) },
        ) {
            val density = LocalDensity.current
            val hostW = state.videoWidth.coerceAtLeast(1)
            val hostH = state.videoHeight.coerceAtLeast(1)
            val viewW = constraints.maxWidth.coerceAtLeast(1)
            val viewH = constraints.maxHeight.coerceAtLeast(1)

            val (surfaceWidthPx, surfaceHeightPx) = when (state.scaleMode) {
                ScaleMode.STRETCH -> viewW to viewH
                ScaleMode.ORIGINAL -> hostW to hostH
                ScaleMode.FIT -> {
                    val hostAspect = hostW.toFloat() / hostH.toFloat()
                    val viewAspect = viewW.toFloat() / viewH.toFloat()
                    if (hostAspect > viewAspect) {
                        viewW to (viewW / hostAspect).toInt().coerceAtLeast(1)
                    } else {
                        (viewH * hostAspect).toInt().coerceAtLeast(1) to viewH
                    }
                }
            }

            val surfaceW = with(density) { surfaceWidthPx.toDp() }
            val surfaceH = with(density) { surfaceHeightPx.toDp() }

            AndroidView(
                factory = { ctx ->
                    SurfaceView(ctx).apply {
                        setZOrderMediaOverlay(false)
                        holder.addCallback(
                            object : SurfaceHolder.Callback {
                                override fun surfaceCreated(holder: SurfaceHolder) {
                                    onAttachSurface(holder.surface)
                                }

                                override fun surfaceChanged(
                                    holder: SurfaceHolder,
                                    format: Int,
                                    width: Int,
                                    height: Int,
                                ) {
                                    onAttachSurface(holder.surface)
                                }

                                override fun surfaceDestroyed(holder: SurfaceHolder) {
                                    onDetachSurface()
                                }
                            },
                        )
                    }
                },
                modifier = Modifier
                    .requiredWidth(surfaceW)
                    .requiredHeight(surfaceH)
                    .graphicsLayer {
                        scaleX = state.viewport.zoom
                        scaleY = state.viewport.zoom
                        translationX = state.viewport.panX
                        translationY = state.viewport.panY
                    },
            )

            // Hardware cursor / trackpad pointer overlay aligned 1:1 with the video surface
            if (state.hasFirstVideoFrame) {
                CursorOverlayCanvas(
                    cursor = state.cursor,
                    touchMode = state.touchMode,
                    videoWidth = state.videoWidth,
                    videoHeight = state.videoHeight,
                    scaleMode = state.scaleMode,
                    viewport = state.viewport,
                    modifier = Modifier.fillMaxSize(),
                )
            }
        }

        // Connecting / waiting status overlay (hidden once first frame arrives)
        AnimatedVisibility(
            visible = !state.hasFirstVideoFrame && state.authPrompt == null,
            enter = fadeIn(),
            exit = fadeOut(),
            modifier = Modifier.align(Alignment.Center),
        ) {
            ConnectingStatusOverlay(
                state = state,
                onCancel = onDisconnect,
            )
        }

        // Compact translucent HUD (top-start)
        if (state.showHud && state.hasFirstVideoFrame) {
            SessionHudOverlay(
                stats = state.hudStats,
                width = state.videoWidth,
                height = state.videoHeight,
                decoderHardware = state.decoderHardware,
                modifier = Modifier
                    .align(Alignment.TopStart)
                    .padding(10.dp),
            )
        }

        // Top-center floating collapsible toolbar handle & panel
        Column(
            horizontalAlignment = Alignment.CenterHorizontally,
            modifier = Modifier
                .align(Alignment.TopCenter)
                .padding(top = 8.dp, start = 12.dp, end = 12.dp),
        ) {
            ToolbarPillHandle(
                hostLabel = state.sessionInfo?.hostName?.ifBlank { null }
                    ?: AppPreferences.formatDisplayTarget(state.target),
                connected = state.statusCode == BridgeConstants.STATUS_CONNECTED,
                expanded = toolbarExpanded,
                onClick = { toolbarExpanded = !toolbarExpanded },
            )

            AnimatedVisibility(
                visible = toolbarExpanded,
                enter = fadeIn() + slideInVertically { -it / 2 },
                exit = fadeOut() + slideOutVertically { -it / 2 },
            ) {
                SessionControlPanel(
                    state = state,
                    softKeyboardActive = softKeyboardActive,
                    specialKeysVisible = specialKeysVisible,
                    onToggleTouchMode = onToggleTouchMode,
                    onToggleSoftKeyboard = {
                        softKeyboardActive = !softKeyboardActive
                        if (softKeyboardActive) toolbarExpanded = false
                    },
                    onToggleSpecialKeys = {
                        specialKeysVisible = !specialKeysVisible
                        if (specialKeysVisible) toolbarExpanded = false
                    },
                    onCycleScaleMode = onCycleScaleMode,
                    onResetZoom = onResetZoom,
                    onToggleAudio = onToggleAudio,
                    onToggleClipboard = onToggleClipboard,
                    onToggleHud = onToggleHud,
                    onSelectBitrate = onSelectBitrate,
                    onSwitchDisplay = onSwitchDisplay,
                    onDisconnect = {
                        toolbarExpanded = false
                        showDisconnectConfirm = true
                    },
                )
            }
        }

        // Slide-up Special Keys Bar at bottom edge
        AnimatedVisibility(
            visible = specialKeysVisible,
            enter = slideInVertically { it } + fadeIn(),
            exit = slideOutVertically { it } + fadeOut(),
            modifier = Modifier
                .align(Alignment.BottomCenter)
                .imePadding()
                .fillMaxWidth(),
        ) {
            SpecialKeysBar(
                stickyModifiers = state.stickyModifiers,
                functionKeysExpanded = functionKeysExpanded,
                onToggleFunctionKeys = { functionKeysExpanded = !functionKeysExpanded },
                onKeyPressed = onSpecialKeyPressed,
                onSendCtrlAltDel = onSendCtrlAltDel,
            )
        }

        // Auth Password / OTP Dialog
        val authPrompt = state.authPrompt
        if (authPrompt != null) {
            AuthPasswordDialog(
                prompt = authPrompt,
                onSubmit = onSubmitPassword,
                onCancel = onCancelAuth,
            )
        }

        // Back-press / Disconnect confirmation dialog
        if (showDisconnectConfirm) {
            val colors = VeloxTheme.colors
            AlertDialog(
                onDismissRequest = { showDisconnectConfirm = false },
                containerColor = colors.surface,
                titleContentColor = colors.textPrimary,
                textContentColor = colors.textSecondary,
                title = { Text(str(R.string.disconnect_confirm_title)) },
                text = {
                    val host = state.sessionInfo?.hostName?.ifBlank { null }
                        ?: AppPreferences.formatDisplayTarget(state.target)
                    Text(str(R.string.disconnect_confirm_body, host))
                },
                confirmButton = {
                    Button(
                        onClick = {
                            showDisconnectConfirm = false
                            onDisconnect()
                        },
                        colors = ButtonDefaults.buttonColors(
                            containerColor = colors.danger,
                            contentColor = Color.White,
                        ),
                    ) {
                        Text(str(R.string.btn_disconnect))
                    }
                },
                dismissButton = {
                    TextButton(onClick = { showDisconnectConfirm = false }) {
                        Text(str(R.string.btn_cancel), color = colors.textSecondary)
                    }
                },
            )
        }
    }
}

@Composable
private fun SoftKeyboardBridge(
    focusRequester: FocusRequester,
    onTextCommitted: (String) -> Unit,
    onBackspace: () -> Unit,
    onEnter: () -> Unit,
) {
    val sentinel = " "
    var tfv by remember { mutableStateOf(TextFieldValue(sentinel, TextRange(1))) }

    BasicTextField(
        value = tfv,
        onValueChange = { updated ->
            val text = updated.text
            when {
                text.length < sentinel.length -> {
                    onBackspace()
                }
                text.length > sentinel.length -> {
                    val added = if (text.startsWith(sentinel)) {
                        text.substring(sentinel.length)
                    } else if (text.endsWith(sentinel)) {
                        text.substring(0, text.length - sentinel.length)
                    } else {
                        text
                    }
                    if (added.isNotEmpty()) {
                        val lines = added.split('\n')
                        lines.forEachIndexed { idx, part ->
                            if (part.isNotEmpty()) onTextCommitted(part)
                            if (idx < lines.lastIndex) onEnter()
                        }
                    }
                }
            }
            tfv = TextFieldValue(sentinel, TextRange(sentinel.length))
        },
        keyboardOptions = KeyboardOptions(
            autoCorrectEnabled = false,
            imeAction = ImeAction.Send,
        ),
        keyboardActions = KeyboardActions(
            onSend = { onEnter() },
            onDone = { onEnter() },
        ),
        modifier = Modifier
            .size(1.dp)
            .alpha(0f)
            .focusRequester(focusRequester),
    )
}

@Composable
private fun ConnectingStatusOverlay(
    state: SessionUiState,
    onCancel: () -> Unit,
) {
    val statusText = when (state.statusCode) {
        BridgeConstants.STATUS_CONNECTING ->
            str(R.string.status_connecting, AppPreferences.formatDisplayTarget(state.target))
        BridgeConstants.STATUS_WAITING_FOR_HOST -> str(R.string.status_waiting_host)
        BridgeConstants.STATUS_AUTHENTICATING -> str(R.string.status_authenticating)
        BridgeConstants.STATUS_WAITING_FOR_ACCEPT -> str(R.string.status_waiting_accept)
        BridgeConstants.STATUS_CONNECTED -> str(R.string.status_waiting_video)
        BridgeConstants.STATUS_RECONNECTING -> str(R.string.status_reconnecting)
        else -> str(R.string.status_connecting, AppPreferences.formatDisplayTarget(state.target))
    }

    Surface(
        color = VeloxColors.Surface.copy(alpha = 0.94f),
        shape = RoundedCornerShape(18.dp),
        border = BorderStroke(1.dp, VeloxColors.Border),
        modifier = Modifier
            .padding(24.dp)
            .widthIn(min = 240.dp, max = 360.dp),
    ) {
        Column(
            horizontalAlignment = Alignment.CenterHorizontally,
            modifier = Modifier.padding(24.dp),
        ) {
            CircularProgressIndicator(
                color = VeloxColors.Primary,
                strokeWidth = 3.dp,
                modifier = Modifier.size(36.dp),
            )
            Spacer(modifier = Modifier.height(16.dp))
            Text(
                text = statusText,
                style = MaterialTheme.typography.titleMedium,
                color = VeloxColors.TextPrimary,
            )
            if (state.statusDetail.isNotBlank()) {
                Spacer(modifier = Modifier.height(4.dp))
                Text(
                    text = state.statusDetail,
                    style = MaterialTheme.typography.bodySmall,
                    color = VeloxColors.TextSecondary,
                )
            }
            Spacer(modifier = Modifier.height(18.dp))
            OutlinedButton(
                onClick = onCancel,
                border = BorderStroke(1.dp, VeloxColors.Border),
                shape = RoundedCornerShape(10.dp),
            ) {
                Text(str(R.string.btn_cancel), color = VeloxColors.TextPrimary)
            }
        }
    }
}

@Composable
private fun SessionHudOverlay(
    stats: HudStats,
    width: Int,
    height: Int,
    decoderHardware: Boolean,
    modifier: Modifier = Modifier,
) {
    val (pathLabel, pathColor) = when (stats.pathKind) {
        BridgeConstants.PATH_DIRECT -> str(R.string.hud_path_direct) to VeloxColors.Emerald
        BridgeConstants.PATH_P2P_HOLE_PUNCH -> str(R.string.hud_path_p2p) to VeloxColors.Primary
        BridgeConstants.PATH_RELAY -> str(R.string.hud_path_relay) to VeloxColors.Amber
        else -> str(R.string.hud_path_unknown) to VeloxColors.TextSecondary
    }
    val decLabel = if (decoderHardware) str(R.string.hud_hw) else str(R.string.hud_sw)
    val decColor = if (decoderHardware) VeloxColors.Emerald else VeloxColors.Amber

    Surface(
        color = VeloxColors.SurfaceOverlay,
        shape = RoundedCornerShape(10.dp),
        border = BorderStroke(1.dp, VeloxColors.Border.copy(alpha = 0.6f)),
        modifier = modifier,
    ) {
        Column(
            modifier = Modifier.padding(horizontal = 10.dp, vertical = 6.dp),
            verticalArrangement = Arrangement.spacedBy(2.dp),
        ) {
            Row(
                horizontalArrangement = Arrangement.spacedBy(6.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                HudBadge(text = pathLabel, color = pathColor)
                HudBadge(text = decLabel, color = decColor)
                if (width > 0 && height > 0) {
                    Text(
                        text = "${width}x${height}",
                        style = MaterialTheme.typography.labelSmall,
                        color = VeloxColors.TextSecondary,
                    )
                }
            }
            Text(
                text = String.format(
                    Locale.US,
                    "%.0f FPS  RTT %.1fms  DEC %.1fms",
                    stats.fps,
                    stats.rttMs,
                    stats.decodeMs,
                ),
                style = MaterialTheme.typography.labelSmall,
                color = VeloxColors.TextPrimary,
            )
            Text(
                text = String.format(
                    Locale.US,
                    "%.1f Mbps  Loss %.1f%%",
                    stats.bitrateKbps / 1000f,
                    stats.lossPct,
                ),
                style = MaterialTheme.typography.labelSmall,
                color = VeloxColors.TextSecondary,
            )
        }
    }
}

@Composable
private fun HudBadge(text: String, color: Color) {
    Box(
        modifier = Modifier
            .clip(RoundedCornerShape(4.dp))
            .background(color.copy(alpha = 0.2f))
            .padding(horizontal = 5.dp, vertical = 1.dp),
    ) {
        Text(
            text = text,
            style = MaterialTheme.typography.labelSmall,
            fontWeight = FontWeight.Bold,
            color = color,
        )
    }
}

@Composable
private fun ToolbarPillHandle(
    hostLabel: String,
    connected: Boolean,
    expanded: Boolean,
    onClick: () -> Unit,
) {
    Surface(
        color = VeloxColors.SurfaceOverlay,
        shape = RoundedCornerShape(999.dp),
        border = BorderStroke(1.dp, VeloxColors.Border.copy(alpha = 0.7f)),
        modifier = Modifier.clickable(onClick = onClick),
    ) {
        Row(
            modifier = Modifier.padding(horizontal = 12.dp, vertical = 6.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Box(
                modifier = Modifier
                    .size(8.dp)
                    .clip(CircleShape)
                    .background(if (connected) VeloxColors.Emerald else VeloxColors.Amber),
            )
            Spacer(modifier = Modifier.width(8.dp))
            Text(
                text = hostLabel,
                style = MaterialTheme.typography.labelMedium,
                color = VeloxColors.TextPrimary,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
                modifier = Modifier.widthIn(max = 160.dp),
            )
            Spacer(modifier = Modifier.width(4.dp))
            Icon(
                imageVector = if (expanded) Icons.Rounded.ExpandLess else Icons.Rounded.ExpandMore,
                contentDescription = str(R.string.toolbar_toggle),
                tint = VeloxColors.TextSecondary,
                modifier = Modifier.size(18.dp),
            )
        }
    }
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun SessionControlPanel(
    state: SessionUiState,
    softKeyboardActive: Boolean,
    specialKeysVisible: Boolean,
    onToggleTouchMode: () -> Unit,
    onToggleSoftKeyboard: () -> Unit,
    onToggleSpecialKeys: () -> Unit,
    onCycleScaleMode: () -> Unit,
    onResetZoom: () -> Unit,
    onToggleAudio: () -> Unit,
    onToggleClipboard: () -> Unit,
    onToggleHud: () -> Unit,
    onSelectBitrate: (Int) -> Unit,
    onSwitchDisplay: (Int) -> Unit,
    onDisconnect: () -> Unit,
) {
    Surface(
        color = VeloxColors.Surface.copy(alpha = 0.95f),
        shape = RoundedCornerShape(16.dp),
        border = BorderStroke(1.dp, VeloxColors.Border),
        modifier = Modifier
            .padding(top = 6.dp)
            .widthIn(max = 520.dp),
    ) {
        Column(
            modifier = Modifier.padding(12.dp),
            verticalArrangement = Arrangement.spacedBy(10.dp),
        ) {
            FlowRow(
                horizontalArrangement = Arrangement.spacedBy(8.dp),
                verticalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                ControlPillButton(
                    icon = if (state.touchMode == TouchMode.TRACKPAD) Icons.Rounded.Mouse else Icons.Rounded.TouchApp,
                    label = if (state.touchMode == TouchMode.TRACKPAD) {
                        str(R.string.touch_trackpad)
                    } else {
                        str(R.string.touch_direct)
                    },
                    active = true,
                    onClick = onToggleTouchMode,
                )
                ControlPillButton(
                    icon = Icons.Rounded.Keyboard,
                    label = str(R.string.toolbar_keyboard),
                    active = softKeyboardActive,
                    onClick = onToggleSoftKeyboard,
                )
                ControlPillButton(
                    icon = Icons.Rounded.KeyboardCommandKey,
                    label = str(R.string.toolbar_special_keys),
                    active = specialKeysVisible,
                    onClick = onToggleSpecialKeys,
                )
                ControlPillButton(
                    icon = Icons.Rounded.AspectRatio,
                    label = when (state.scaleMode) {
                        ScaleMode.FIT -> str(R.string.scale_fit)
                        ScaleMode.STRETCH -> str(R.string.scale_stretch)
                        ScaleMode.ORIGINAL -> str(R.string.scale_original)
                    },
                    active = false,
                    onClick = onCycleScaleMode,
                )
                if (state.viewport.zoom > 1.01f) {
                    ControlPillButton(
                        icon = Icons.Rounded.ZoomOutMap,
                        label = str(R.string.toolbar_reset_zoom),
                        active = true,
                        onClick = onResetZoom,
                    )
                }
                ControlPillButton(
                    icon = if (state.audioEnabled) {
                        Icons.AutoMirrored.Rounded.VolumeUp
                    } else {
                        Icons.AutoMirrored.Rounded.VolumeOff
                    },
                    label = str(R.string.toolbar_audio),
                    active = state.audioEnabled,
                    onClick = onToggleAudio,
                )
                ControlPillButton(
                    icon = Icons.Rounded.ContentPaste,
                    label = str(R.string.toolbar_clipboard),
                    active = state.clipboardEnabled,
                    onClick = onToggleClipboard,
                )
                ControlPillButton(
                    icon = Icons.Rounded.Speed,
                    label = str(R.string.toolbar_hud),
                    active = state.showHud,
                    onClick = onToggleHud,
                )
                ControlPillButton(
                    icon = Icons.Rounded.CallEnd,
                    label = str(R.string.btn_disconnect),
                    active = false,
                    danger = true,
                    onClick = onDisconnect,
                )
            }

            // Display switcher (when host reports > 1 display)
            if (state.displays.size > 1) {
                HorizontalDivider(color = VeloxColors.Border.copy(alpha = 0.6f))
                Row(
                    horizontalArrangement = Arrangement.spacedBy(8.dp),
                    verticalAlignment = Alignment.CenterVertically,
                    modifier = Modifier.horizontalScroll(rememberScrollState()),
                ) {
                    Icon(
                        imageVector = Icons.Rounded.Monitor,
                        contentDescription = null,
                        tint = VeloxColors.TextSecondary,
                        modifier = Modifier.size(16.dp),
                    )
                    state.displays.forEach { disp ->
                        val selected = disp.index == state.activeDisplay
                        CompactChip(
                            label = disp.name.ifBlank { str(R.string.toolbar_display, disp.index + 1) },
                            selected = selected,
                            onClick = { onSwitchDisplay(disp.index) },
                        )
                    }
                }
            }

            // Quick bitrate selector
            HorizontalDivider(color = VeloxColors.Border.copy(alpha = 0.6f))
            Row(
                horizontalArrangement = Arrangement.spacedBy(6.dp),
                verticalAlignment = Alignment.CenterVertically,
                modifier = Modifier.horizontalScroll(rememberScrollState()),
            ) {
                Text(
                    text = "${str(R.string.toolbar_bitrate)}:",
                    style = MaterialTheme.typography.labelSmall,
                    color = VeloxColors.TextSecondary,
                )
                val presets = listOf(
                    BitratePreset.LOW to str(R.string.bitrate_5),
                    BitratePreset.BALANCED to str(R.string.bitrate_20),
                    BitratePreset.HIGH to str(R.string.bitrate_50),
                    BitratePreset.ULTRA to str(R.string.bitrate_100),
                )
                presets.forEach { (preset, label) ->
                    CompactChip(
                        label = label,
                        selected = BitratePreset.nearest(state.bitrateKbps) == preset,
                        onClick = { onSelectBitrate(preset.kbps) },
                    )
                }
            }
        }
    }
}

@Composable
private fun ControlPillButton(
    icon: ImageVector,
    label: String,
    active: Boolean,
    danger: Boolean = false,
    onClick: () -> Unit,
) {
    val bg = when {
        danger -> VeloxColors.Danger.copy(alpha = 0.2f)
        active -> VeloxColors.Primary.copy(alpha = 0.2f)
        else -> VeloxColors.Background
    }
    val border = when {
        danger -> VeloxColors.Danger.copy(alpha = 0.65f)
        active -> VeloxColors.Primary.copy(alpha = 0.7f)
        else -> VeloxColors.Border
    }
    val content = when {
        danger -> VeloxColors.Danger
        active -> VeloxColors.Primary
        else -> VeloxColors.TextPrimary
    }

    Surface(
        color = bg,
        shape = RoundedCornerShape(10.dp),
        border = BorderStroke(1.dp, border),
        modifier = Modifier.clickable(onClick = onClick),
    ) {
        Row(
            modifier = Modifier.padding(horizontal = 10.dp, vertical = 7.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Icon(
                imageVector = icon,
                contentDescription = null,
                tint = content,
                modifier = Modifier.size(16.dp),
            )
            Spacer(modifier = Modifier.width(5.dp))
            Text(
                text = label,
                style = MaterialTheme.typography.labelMedium,
                color = content,
            )
        }
    }
}

@Composable
private fun CompactChip(
    label: String,
    selected: Boolean,
    onClick: () -> Unit,
) {
    Surface(
        color = if (selected) VeloxColors.Primary.copy(alpha = 0.22f) else VeloxColors.Background,
        shape = RoundedCornerShape(8.dp),
        border = BorderStroke(1.dp, if (selected) VeloxColors.Primary else VeloxColors.Border),
        modifier = Modifier.clickable(onClick = onClick),
    ) {
        Text(
            text = label,
            style = MaterialTheme.typography.labelMedium,
            color = if (selected) VeloxColors.Primary else VeloxColors.TextSecondary,
            modifier = Modifier.padding(horizontal = 10.dp, vertical = 5.dp),
        )
    }
}

@Composable
private fun SpecialKeysBar(
    stickyModifiers: Int,
    functionKeysExpanded: Boolean,
    onToggleFunctionKeys: () -> Unit,
    onKeyPressed: (SpecialKeyDef) -> Unit,
    onSendCtrlAltDel: () -> Unit,
) {
    Surface(
        color = VeloxColors.Surface.copy(alpha = 0.94f),
        border = BorderStroke(1.dp, VeloxColors.Border),
        modifier = Modifier.fillMaxWidth(),
    ) {
        Column(
            modifier = Modifier.padding(horizontal = 8.dp, vertical = 6.dp),
            verticalArrangement = Arrangement.spacedBy(6.dp),
        ) {
            if (functionKeysExpanded) {
                Row(
                    horizontalArrangement = Arrangement.spacedBy(6.dp),
                    modifier = Modifier
                        .fillMaxWidth()
                        .horizontalScroll(rememberScrollState()),
                ) {
                    KeyMapper.functionKeys.forEach { key ->
                        SpecialKeyChip(
                            label = key.label,
                            latched = false,
                            onClick = { onKeyPressed(key) },
                        )
                    }
                }
            }

            Row(
                horizontalArrangement = Arrangement.spacedBy(6.dp),
                verticalAlignment = Alignment.CenterVertically,
                modifier = Modifier
                    .fillMaxWidth()
                    .horizontalScroll(rememberScrollState()),
            ) {
                KeyMapper.primarySpecialKeys.forEach { key ->
                    val latched = key.isToggleModifier && (stickyModifiers and key.modifierBit) != 0
                    SpecialKeyChip(
                        label = key.label,
                        latched = latched,
                        onClick = { onKeyPressed(key) },
                    )
                }
                SpecialKeyChip(
                    label = str(R.string.special_keys_fn_toggle),
                    latched = functionKeysExpanded,
                    onClick = onToggleFunctionKeys,
                )
                SpecialKeyChip(
                    label = str(R.string.special_key_cad),
                    latched = false,
                    accentDanger = true,
                    onClick = onSendCtrlAltDel,
                )
            }
        }
    }
}

@Composable
private fun SpecialKeyChip(
    label: String,
    latched: Boolean,
    accentDanger: Boolean = false,
    onClick: () -> Unit,
) {
    val bg = when {
        accentDanger -> VeloxColors.Danger.copy(alpha = 0.2f)
        latched -> VeloxColors.Primary
        else -> VeloxColors.Background
    }
    val border = when {
        accentDanger -> VeloxColors.Danger.copy(alpha = 0.65f)
        latched -> VeloxColors.Primary
        else -> VeloxColors.Border
    }
    val fg = when {
        accentDanger -> VeloxColors.Danger
        latched -> VeloxColors.OnPrimary
        else -> VeloxColors.TextPrimary
    }

    Surface(
        color = bg,
        shape = RoundedCornerShape(8.dp),
        border = BorderStroke(1.dp, border),
        modifier = Modifier.clickable(onClick = onClick),
    ) {
        Text(
            text = label,
            style = MaterialTheme.typography.labelMedium,
            fontWeight = if (latched || accentDanger) FontWeight.Bold else FontWeight.Medium,
            color = fg,
            modifier = Modifier.padding(horizontal = 11.dp, vertical = 7.dp),
        )
    }
}

@Composable
private fun AuthPasswordDialog(
    prompt: AuthPromptData,
    onSubmit: (String) -> Unit,
    onCancel: () -> Unit,
) {
    val colors = VeloxTheme.colors
    // Keep password strictly in transient memory (never rememberSaveable, never logged!)
    var password by remember(prompt) { mutableStateOf("") }
    var showPassword by remember { mutableStateOf(false) }
    var uppercaseMode by remember(prompt) { mutableStateOf(prompt.allowOtp && !prompt.allowPerm) }

    val methodsHint = when {
        prompt.allowPerm && prompt.allowOtp -> str(R.string.auth_hint_both)
        prompt.allowPerm -> str(R.string.auth_hint_perm)
        else -> str(R.string.auth_hint_otp)
    }

    AlertDialog(
        onDismissRequest = {
            password = ""
            onCancel()
        },
        properties = DialogProperties(
            dismissOnBackPress = true,
            dismissOnClickOutside = false,
        ),
        containerColor = colors.surface,
        titleContentColor = colors.textPrimary,
        textContentColor = colors.textSecondary,
        icon = {
            Icon(
                imageVector = Icons.Rounded.Lock,
                contentDescription = null,
                tint = colors.primary,
            )
        },
        title = { Text(str(R.string.auth_title)) },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(10.dp)) {
                if (prompt.hostDeviceId.isNotBlank()) {
                    Text(
                        text = str(R.string.auth_host_id, AppPreferences.formatDeviceId(prompt.hostDeviceId)),
                        style = MaterialTheme.typography.bodyMedium,
                        color = colors.textPrimary,
                        fontWeight = FontWeight.Medium,
                    )
                }
                if (prompt.hostFingerprint.isNotBlank()) {
                    Column(
                        modifier = Modifier
                            .fillMaxWidth()
                            .clip(RoundedCornerShape(10.dp))
                            .background(colors.background)
                            .padding(10.dp),
                    ) {
                        Text(
                            text = str(R.string.auth_fingerprint),
                            style = MaterialTheme.typography.bodySmall,
                            color = colors.textMuted,
                        )
                        Spacer(modifier = Modifier.height(3.dp))
                        Text(
                            text = prompt.hostFingerprint,
                            fontFamily = FontFamily.Monospace,
                            fontSize = 12.sp,
                            color = colors.primary,
                        )
                    }
                }
                Text(
                    text = methodsHint,
                    style = MaterialTheme.typography.bodySmall,
                    color = colors.textSecondary,
                )
                OutlinedTextField(
                    value = password,
                    onValueChange = { raw ->
                        password = transformPasswordInput(raw, uppercaseMode)
                    },
                    singleLine = true,
                    label = { Text(str(R.string.auth_password_label)) },
                    visualTransformation = if (showPassword) {
                        VisualTransformation.None
                    } else {
                        PasswordVisualTransformation()
                    },
                    trailingIcon = {
                        Row(
                            verticalAlignment = Alignment.CenterVertically,
                            horizontalArrangement = Arrangement.spacedBy(2.dp),
                            modifier = Modifier.padding(end = 4.dp),
                        ) {
                            if (prompt.allowOtp) {
                                Surface(
                                    color = if (uppercaseMode) colors.primary.copy(alpha = 0.2f) else Color.Transparent,
                                    shape = RoundedCornerShape(6.dp),
                                    border = BorderStroke(
                                        1.dp,
                                        if (uppercaseMode) colors.primary else colors.border,
                                    ),
                                    modifier = Modifier.clickable {
                                        uppercaseMode = !uppercaseMode
                                        if (uppercaseMode) {
                                            password = transformPasswordInput(password, true)
                                        }
                                    },
                                ) {
                                    Text(
                                        text = str(R.string.auth_caps_chip),
                                        style = MaterialTheme.typography.labelSmall,
                                        fontWeight = FontWeight.Bold,
                                        color = if (uppercaseMode) colors.primary else colors.textSecondary,
                                        modifier = Modifier.padding(horizontal = 6.dp, vertical = 3.dp),
                                    )
                                }
                            }
                            IconButton(onClick = { showPassword = !showPassword }) {
                                Icon(
                                    imageVector = if (showPassword) Icons.Rounded.VisibilityOff else Icons.Rounded.Visibility,
                                    contentDescription = if (showPassword) {
                                        str(R.string.auth_hide_password)
                                    } else {
                                        str(R.string.auth_show_password)
                                    },
                                    tint = colors.textSecondary,
                                )
                            }
                        }
                    },
                    keyboardOptions = KeyboardOptions(
                        capitalization = if (uppercaseMode) {
                            KeyboardCapitalization.Characters
                        } else {
                            KeyboardCapitalization.None
                        },
                        keyboardType = KeyboardType.Password,
                        autoCorrectEnabled = false,
                        imeAction = ImeAction.Done,
                    ),
                    keyboardActions = KeyboardActions(
                        onDone = {
                            if (password.isNotEmpty()) {
                                val pw = password
                                password = ""
                                onSubmit(pw)
                            }
                        },
                    ),
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
            }
        },
        confirmButton = {
            Button(
                onClick = {
                    val pw = password
                    password = ""
                    onSubmit(pw)
                },
                enabled = password.isNotEmpty(),
                colors = ButtonDefaults.buttonColors(
                    containerColor = colors.primary,
                    contentColor = colors.onPrimary,
                ),
                contentPadding = PaddingValues(horizontal = 18.dp, vertical = 10.dp),
            ) {
                Text(str(R.string.btn_connect), fontWeight = FontWeight.Bold)
            }
        },
        dismissButton = {
            TextButton(
                onClick = {
                    password = ""
                    onCancel()
                },
            ) {
                Text(str(R.string.btn_cancel), color = colors.textSecondary)
            }
        },
    )
}
