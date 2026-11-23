package dev.veloxdesk.ui

import androidx.activity.compose.BackHandler
import androidx.compose.material3.SnackbarHostState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import dev.veloxdesk.BuildConfig
import dev.veloxdesk.R
import dev.veloxdesk.data.AppPreferences
import dev.veloxdesk.data.ReleaseInfo
import dev.veloxdesk.data.UpdateCheckResult
import dev.veloxdesk.data.UpdateChecker
import dev.veloxdesk.ui.i18n.LocalLocalizedResources
import dev.veloxdesk.ui.i18n.ProvideAppLocale
import dev.veloxdesk.ui.theme.VeloxDeskTheme
import kotlinx.coroutines.launch

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

    VeloxDeskTheme {
        ProvideAppLocale(language = settings.language) {
            val localizedResources = LocalLocalizedResources.current
            val scope = rememberCoroutineScope()
            val snackbarHostState = remember { SnackbarHostState() }

            var homeRoute by rememberSaveable { mutableStateOf(HomeRoute.CONNECT) }
            var availableUpdate by remember { mutableStateOf<ReleaseInfo?>(null) }
            var checkingUpdate by remember { mutableStateOf(false) }

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

            // Quiet background update check on cold start
            LaunchedEffect(Unit) {
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
                            onBack = { homeRoute = HomeRoute.CONNECT },
                            onLanguageChange = preferences::setLanguage,
                            onRendezvousChange = preferences::setRendezvousServer,
                            onRelayOnlyChange = preferences::setRelayOnly,
                            onUpdateProfile = preferences::updateProfile,
                        )
                    }
                    HomeRoute.CONNECT -> {
                        ConnectScreen(
                            settings = settings,
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
                            onOpenSettings = { homeRoute = HomeRoute.SETTINGS },
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
        }
    }
}
