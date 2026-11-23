package dev.veloxdesk.ui

import android.Manifest
import android.app.Activity
import android.content.Context
import android.content.pm.PackageManager
import android.media.projection.MediaProjectionManager
import android.os.Build
import androidx.activity.compose.BackHandler
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.material3.SnackbarHostState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableLongStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.platform.LocalContext
import androidx.core.content.ContextCompat
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleEventObserver
import androidx.lifecycle.compose.LocalLifecycleOwner
import dev.veloxdesk.BuildConfig
import dev.veloxdesk.R
import dev.veloxdesk.bridge.HostPasswordMode
import dev.veloxdesk.bridge.VeloxNative
import dev.veloxdesk.data.AppPreferences
import dev.veloxdesk.data.AppSettings
import dev.veloxdesk.data.ReleaseInfo
import dev.veloxdesk.data.UpdateCheckResult
import dev.veloxdesk.data.UpdateChecker
import dev.veloxdesk.host.HostController
import dev.veloxdesk.host.HostStartConfig
import dev.veloxdesk.host.VeloxAccessibilityService
import dev.veloxdesk.ui.i18n.LocalLocalizedResources
import dev.veloxdesk.ui.i18n.ProvideAppLocale
import dev.veloxdesk.ui.theme.VeloxDeskTheme
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

private enum class HomeRoute {
    CONNECT,
    SETTINGS,
}

@Composable
fun VeloxApp(
    preferences: AppPreferences,
    sessionViewModel: SessionViewModel,
) {
    val settings by preferences.settings.collectAsState()
    val sessionState by sessionViewModel.uiState.collectAsState()
    val deviceIdentity by sessionViewModel.deviceIdentity.collectAsState()
    val hostState by HostController.uiState.collectAsState()
    val pendingApproval by HostController.pendingApproval.collectAsState()

    VeloxDeskTheme(themeMode = settings.themeMode) {
        ProvideAppLocale(language = settings.language) {
            val context = LocalContext.current
            val lifecycleOwner = LocalLifecycleOwner.current
            val localizedResources = LocalLocalizedResources.current
            val scope = rememberCoroutineScope()
            val snackbarHostState = remember { SnackbarHostState() }

            var homeRoute by rememberSaveable { mutableStateOf(HomeRoute.CONNECT) }
            var availableUpdate by remember { mutableStateOf<ReleaseInfo?>(null) }
            var checkingUpdate by remember { mutableStateOf(false) }
            var isHashingHostPassword by remember { mutableStateOf(false) }
            var pwHashSeq by remember { mutableLongStateOf(0L) }

            fun showSnackbar(message: String) {
                scope.launch {
                    snackbarHostState.currentSnackbarData?.dismiss()
                    snackbarHostState.showSnackbar(message)
                }
            }

            fun pushHostAuthUpdate(current: AppSettings) {
                HostController.updateAuth(
                    passwordMode = HostPasswordMode.fromId(current.hostPasswordMode),
                    permanentHashPhc = current.effectiveHostPermanentHash,
                    requireAccept = current.hostRequireAccept,
                )
            }

            // Bind process-wide AppPreferences singleton to HostController so newly remembered
            // trusted peers persist even if MainActivity is destroyed while hosting in background.
            LaunchedEffect(preferences) {
                HostController.onTrustedPeerAdded = preferences::upsertHostTrustedPeer
            }

            // Refresh LAN/Tailscale IPs, Accessibility status, and persisted prefs on foreground resume
            DisposableEffect(lifecycleOwner, context, preferences) {
                val observer = LifecycleEventObserver { _, event ->
                    if (event == Lifecycle.Event.ON_RESUME) {
                        preferences.reload()
                        HostController.refreshStatus(context)
                    }
                }
                lifecycleOwner.lifecycle.addObserver(observer)
                onDispose {
                    lifecycleOwner.lifecycle.removeObserver(observer)
                }
            }

            // MediaProjection screen capture consent launcher
            val screenCaptureLauncher = rememberLauncherForActivityResult(
                contract = ActivityResultContracts.StartActivityForResult(),
            ) { result ->
                val data = result.data
                if (result.resultCode == Activity.RESULT_OK && data != null) {
                    val latest = preferences.settings.value
                    HostController.startSharing(
                        context = context,
                        resultCode = result.resultCode,
                        projectionData = data,
                        config = HostStartConfig(
                            rendezvousAddr = latest.rendezvousServer,
                            deviceName = Build.MODEL ?: "Android",
                            fps = 30,
                            passwordMode = HostPasswordMode.fromId(latest.hostPasswordMode),
                            permanentHashPhc = latest.effectiveHostPermanentHash,
                            requireAccept = latest.hostRequireAccept,
                            trustedPeers = latest.hostTrustedPeers,
                        ),
                    )
                } else {
                    showSnackbar(localizedResources.getString(R.string.host_capture_denied))
                }
            }

            fun launchScreenCapturePrompt() {
                val mpm = context.getSystemService(Context.MEDIA_PROJECTION_SERVICE) as? MediaProjectionManager
                if (mpm != null) {
                    screenCaptureLauncher.launch(mpm.createScreenCaptureIntent())
                }
            }

            // Request POST_NOTIFICATIONS on Android 13+ before starting the foreground service
            val notifPermissionLauncher = rememberLauncherForActivityResult(
                contract = ActivityResultContracts.RequestPermission(),
            ) {
                launchScreenCapturePrompt()
            }

            fun requestStartSharing() {
                if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU &&
                    ContextCompat.checkSelfPermission(
                        context,
                        Manifest.permission.POST_NOTIFICATIONS,
                    ) != PackageManager.PERMISSION_GRANTED
                ) {
                    notifPermissionLauncher.launch(Manifest.permission.POST_NOTIFICATIONS)
                } else {
                    launchScreenCapturePrompt()
                }
            }

            fun runUpdateCheck(silentIfUpToDate: Boolean) {
                if (checkingUpdate) return
                scope.launch {
                    checkingUpdate = true
                    when (val res = UpdateChecker.checkLatestRelease(BuildConfig.VERSION_NAME)) {
                        is UpdateCheckResult.UpdateAvailable -> {
                            availableUpdate = res.release
                        }
                        is UpdateCheckResult.UpToDate -> {
                            availableUpdate = null
                            if (!silentIfUpToDate) {
                                snackbarHostState.currentSnackbarData?.dismiss()
                                snackbarHostState.showSnackbar(
                                    localizedResources.getString(R.string.update_up_to_date, res.latestVersion),
                                )
                            }
                        }
                        is UpdateCheckResult.Error -> {
                            if (!silentIfUpToDate) {
                                snackbarHostState.currentSnackbarData?.dismiss()
                                snackbarHostState.showSnackbar(
                                    localizedResources.getString(R.string.update_check_failed, res.message),
                                )
                            }
                        }
                    }
                    checkingUpdate = false
                }
            }

            // Cold-start initialization: sanitize corrupt PHC, refresh host status, check updates
            LaunchedEffect(Unit) {
                val current = preferences.settings.value
                if (VeloxNative.isLoaded &&
                    current.hostPermanentPasswordHash.isNotBlank() &&
                    !VeloxNative.nativeIsValidPhcHash(current.hostPermanentPasswordHash)
                ) {
                    preferences.clearHostPermanentPasswordHash()
                }
                HostController.refreshStatus(context)
                runUpdateCheck(silentIfUpToDate = true)
            }

            if (sessionState.isActive) {
                SessionScreen(
                    state = sessionState,
                    onAttachSurface = sessionViewModel::attachSurface,
                    onDetachSurface = sessionViewModel::detachSurface,
                    onViewSizeChanged = sessionViewModel::onVideoViewSizeChanged,
                    onTouchEvent = sessionViewModel::onTouchEvent,
                    onSubmitPassword = sessionViewModel::submitAuthPassword,
                    onCancelAuth = sessionViewModel::cancelAuth,
                    onDisconnect = sessionViewModel::disconnect,
                    onToggleTouchMode = sessionViewModel::toggleTouchMode,
                    onCycleScaleMode = sessionViewModel::cycleScaleMode,
                    onResetZoom = sessionViewModel::resetZoom,
                    onToggleAudio = sessionViewModel::toggleAudio,
                    onToggleClipboard = sessionViewModel::toggleClipboard,
                    onToggleHud = sessionViewModel::toggleHud,
                    onSelectBitrate = sessionViewModel::setMaxBitrate,
                    onSwitchDisplay = sessionViewModel::switchDisplay,
                    onSpecialKeyPressed = sessionViewModel::onSpecialKeyPressed,
                    onSendCtrlAltDel = sessionViewModel::sendCtrlAltDel,
                    onSendSoftText = sessionViewModel::sendSoftKeyboardText,
                    onSendSoftBackspace = sessionViewModel::sendSoftBackspace,
                    onSendSoftEnter = sessionViewModel::sendSoftEnter,
                )
            } else {
                when (homeRoute) {
                    HomeRoute.SETTINGS -> {
                        BackHandler { homeRoute = HomeRoute.CONNECT }
                        SettingsScreen(
                            settings = settings,
                            isHashingHostPassword = isHashingHostPassword,
                            snackbarHostState = snackbarHostState,
                            onBack = { homeRoute = HomeRoute.CONNECT },
                            onLanguageChange = preferences::setLanguage,
                            onThemeModeChange = preferences::setThemeMode,
                            onRendezvousChange = preferences::setRendezvousServer,
                            onRelayOnlyChange = preferences::setRelayOnly,
                            onUpdateProfile = preferences::updateProfile,
                            onHostPasswordModeChange = { mode ->
                                preferences.setHostPasswordMode(mode)
                                pushHostAuthUpdate(preferences.settings.value)
                            },
                            onSaveHostPermanentPassword = { plaintext ->
                                val seq = ++pwHashSeq
                                isHashingHostPassword = true
                                scope.launch {
                                    val phc = withContext(Dispatchers.Default) {
                                        if (VeloxNative.isLoaded) {
                                            runCatching { VeloxNative.nativeHashPassword(plaintext) }
                                                .getOrDefault("")
                                        } else {
                                            ""
                                        }
                                    }
                                    if (seq != pwHashSeq) return@launch
                                    isHashingHostPassword = false
                                    if (phc.isNotBlank() && VeloxNative.nativeIsValidPhcHash(phc)) {
                                        preferences.setHostPermanentPasswordHash(phc)
                                        pushHostAuthUpdate(preferences.settings.value)
                                        showSnackbar(
                                            localizedResources.getString(R.string.settings_host_perm_pw_saved),
                                        )
                                    } else {
                                        showSnackbar(
                                            localizedResources.getString(R.string.settings_host_perm_pw_hash_failed),
                                        )
                                    }
                                }
                            },
                            onClearHostPermanentPassword = {
                                pwHashSeq++
                                isHashingHostPassword = false
                                preferences.clearHostPermanentPasswordHash()
                                pushHostAuthUpdate(preferences.settings.value)
                                showSnackbar(
                                    localizedResources.getString(R.string.settings_host_perm_pw_cleared),
                                )
                            },
                            onHostRequireAcceptChange = { requireAccept ->
                                preferences.setHostRequireAccept(requireAccept)
                                pushHostAuthUpdate(preferences.settings.value)
                            },
                            onRemoveHostTrustedPeer = { pubKeyHex ->
                                preferences.removeHostTrustedPeer(pubKeyHex)
                                HostController.removeTrustedPeer(pubKeyHex)
                                showSnackbar(
                                    localizedResources.getString(R.string.settings_host_trusted_revoked),
                                )
                            },
                        )
                    }
                    HomeRoute.CONNECT -> {
                        ConnectScreen(
                            settings = settings,
                            hostState = hostState,
                            ownDeviceId = deviceIdentity.ownDeviceId,
                            ownFingerprint = deviceIdentity.ownFingerprint,
                            buildDisplay = deviceIdentity.buildDisplay,
                            nativeLoaded = sessionViewModel.nativeLoaded,
                            nativeLoadError = sessionViewModel.nativeLoadError,
                            availableUpdate = availableUpdate,
                            checkingUpdate = checkingUpdate,
                            lastEndedReason = sessionState.lastEndedReason,
                            lastEndedWasError = sessionState.lastEndedWasError,
                            onDismissEndedReason = sessionViewModel::clearEndedBanner,
                            onLanguageChange = preferences::setLanguage,
                            onCycleTheme = { preferences.setThemeMode(settings.themeMode.next()) },
                            onOpenSettings = { homeRoute = HomeRoute.SETTINGS },
                            onStartSharing = ::requestStartSharing,
                            onStopSharing = { HostController.stopSharing(context) },
                            onRotateOtp = HostController::rotateOtp,
                            onDismissHostError = HostController::clearError,
                            onEnableAccessibility = { VeloxAccessibilityService.openSettings(context) },
                            onModeSelected = preferences::setLastMode,
                            onConnect = { target, mode ->
                                preferences.setLastMode(mode)
                                val profile = settings.profileForMode(mode)
                                sessionViewModel.connect(
                                    target = target,
                                    mode = mode,
                                    rendezvousServer = settings.rendezvousServer,
                                    relayOnly = settings.relayOnly,
                                    profile = profile,
                                    onConnectedHost = { connectedTarget, hostName, connectedMode ->
                                        preferences.recordRecentConnection(
                                            target = connectedTarget,
                                            label = hostName,
                                            mode = connectedMode,
                                        )
                                    },
                                )
                            },
                            onRemoveRecent = preferences::removeRecentConnection,
                            onCheckUpdates = { runUpdateCheck(silentIfUpToDate = false) },
                            snackbarHostState = snackbarHostState,
                        )
                    }
                }
            }

            // Global incoming Android Host connection approval dialog
            val approval = pendingApproval
            if (approval != null) {
                HostAcceptDialog(
                    request = approval,
                    onAccept = { rememberPeer, allowInput, allowClipboard, allowAudio ->
                        HostController.respondApproval(
                            requestId = approval.requestId,
                            accepted = true,
                            rememberPeer = rememberPeer,
                            allowInput = allowInput,
                            allowClipboard = allowClipboard,
                            allowAudio = allowAudio,
                        )
                    },
                    onReject = {
                        HostController.respondApproval(
                            requestId = approval.requestId,
                            accepted = false,
                            rememberPeer = false,
                        )
                    },
                )
            }
        }
    }
}
