package dev.veloxdesk.host

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.content.res.Configuration
import android.media.projection.MediaProjectionManager
import android.os.Build
import android.os.IBinder
import android.util.Log
import androidx.core.app.NotificationCompat
import dev.veloxdesk.MainActivity
import dev.veloxdesk.R
import dev.veloxdesk.bridge.HostApprovalRequest
import dev.veloxdesk.data.AppLanguage
import dev.veloxdesk.data.AppPreferences
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.flow.collectLatest
import kotlinx.coroutines.launch
import java.util.Locale

/**
 * Android 14/15 (`targetSdk 35`) compliant `mediaProjection` foreground service hosting
 * screen capture + Rust QUIC host engine while sharing the phone screen.
 *
 * Also observes [HostController.pendingApproval] to raise a high-priority heads-up notification
 * bringing [MainActivity] to the foreground when an incoming approval request arrives while the
 * app is backgrounded.
 */
class VeloxHostService : Service() {
    private val serviceScope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)
    private var approvalWatcherJob: Job? = null

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        when (intent?.action) {
            ACTION_STOP_SHARING -> {
                cancelApprovalNotification()
                HostController.teardownEngine()
                stopSelf()
                return START_NOT_STICKY
            }

            ACTION_START_SHARING -> {
                ensureNotificationChannels()
                val notification = buildSharingNotification()
                // Android 14/15 requires calling startForeground(..., MEDIA_PROJECTION) BEFORE getMediaProjection().
                if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
                    startForeground(
                        NOTIFICATION_ID,
                        notification,
                        ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PROJECTION,
                    )
                } else {
                    startForeground(NOTIFICATION_ID, notification)
                }

                startApprovalObserver()

                val pending = HostController.consumePendingStart()
                if (pending == null) {
                    Log.w(TAG, "VeloxHostService started without pending projection intent")
                    stopSelf()
                    return START_NOT_STICKY
                }

                val mpm = getSystemService(Context.MEDIA_PROJECTION_SERVICE) as? MediaProjectionManager
                val projection = try {
                    mpm?.getMediaProjection(pending.resultCode, pending.projectionData)
                } catch (t: Throwable) {
                    Log.e(TAG, "getMediaProjection failed: ${t.message}", t)
                    null
                }
                if (projection == null) {
                    HostController.failStart("Screen capture permission denied or unavailable")
                    stopSelf()
                    return START_NOT_STICKY
                }

                val started = HostController.startEngineWithProjection(this, projection, pending.config)
                if (!started) {
                    stopSelf()
                }
                return START_NOT_STICKY
            }

            else -> {
                stopSelf()
                return START_NOT_STICKY
            }
        }
    }

    override fun onConfigurationChanged(newConfig: Configuration) {
        super.onConfigurationChanged(newConfig)
        HostController.onDisplayMetricsChanged(this)
    }

    override fun onDestroy() {
        approvalWatcherJob?.cancel()
        approvalWatcherJob = null
        serviceScope.cancel()
        cancelApprovalNotification()
        HostController.teardownEngine()
        super.onDestroy()
    }

    private fun startApprovalObserver() {
        approvalWatcherJob?.cancel()
        approvalWatcherJob = serviceScope.launch {
            HostController.pendingApproval.collectLatest { req ->
                if (req != null) {
                    showApprovalNotification(req)
                } else {
                    cancelApprovalNotification()
                }
            }
        }
    }

    private fun isRussian(): Boolean {
        return try {
            AppPreferences(this).settings.value.language == AppLanguage.RU
        } catch (_: Throwable) {
            Locale.getDefault().language.equals("ru", ignoreCase = true)
        }
    }

    private fun ensureNotificationChannels() {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            val nm = getSystemService(Context.NOTIFICATION_SERVICE) as? NotificationManager ?: return
            val isRu = isRussian()
            val sharingName = if (isRu) "Трансляция экрана VeloxDesk" else "VeloxDesk Screen Sharing"
            val sharingChannel = NotificationChannel(CHANNEL_ID, sharingName, NotificationManager.IMPORTANCE_LOW).apply {
                setShowBadge(false)
            }
            nm.createNotificationChannel(sharingChannel)

            val approvalName = if (isRu) "Запросы подключения VeloxDesk" else "VeloxDesk Connection Requests"
            val approvalChannel = NotificationChannel(
                APPROVAL_CHANNEL_ID,
                approvalName,
                NotificationManager.IMPORTANCE_HIGH,
            ).apply {
                setShowBadge(true)
            }
            nm.createNotificationChannel(approvalChannel)
        }
    }

    private fun buildSharingNotification(): Notification {
        val isRu = isRussian()
        val title = if (isRu) "VeloxDesk — доступ к экрану активен" else "VeloxDesk — Screen Sharing Active"
        val body = if (isRu) {
            "Устройство принимает удалённые подключения."
        } else {
            "This device is accepting remote connections."
        }
        val stopLabel = if (isRu) "Остановить" else "Stop Sharing"

        val openPending = buildOpenAppPendingIntent(requestCode = 0)

        val stopIntent = Intent(this, VeloxHostService::class.java).apply {
            action = ACTION_STOP_SHARING
        }
        val stopPending = PendingIntent.getService(
            this,
            1,
            stopIntent,
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )

        return NotificationCompat.Builder(this, CHANNEL_ID)
            .setSmallIcon(R.mipmap.ic_launcher)
            .setContentTitle(title)
            .setContentText(body)
            .setOngoing(true)
            .setPriority(NotificationCompat.PRIORITY_LOW)
            .setContentIntent(openPending)
            .addAction(0, stopLabel, stopPending)
            .build()
    }

    private fun showApprovalNotification(req: HostApprovalRequest) {
        val nm = getSystemService(Context.NOTIFICATION_SERVICE) as? NotificationManager ?: return
        ensureNotificationChannels()
        val isRu = isRussian()
        val caller = req.peerName.ifBlank { req.peerDeviceId }
        val title = if (isRu) "Запрос подключения к устройству" else "Incoming Connection Request"
        val body = if (isRu) {
            "$caller (${req.peerDeviceId}) хочет подключиться. Нажмите, чтобы подтвердить."
        } else {
            "$caller (${req.peerDeviceId}) wants to connect. Tap to approve or reject."
        }

        val openPending = buildOpenAppPendingIntent(requestCode = 2)
        val notification = NotificationCompat.Builder(this, APPROVAL_CHANNEL_ID)
            .setSmallIcon(R.mipmap.ic_launcher)
            .setContentTitle(title)
            .setContentText(body)
            .setStyle(NotificationCompat.BigTextStyle().bigText(body))
            .setPriority(NotificationCompat.PRIORITY_HIGH)
            .setCategory(NotificationCompat.CATEGORY_CALL)
            .setAutoCancel(true)
            .setContentIntent(openPending)
            .build()

        try {
            nm.notify(APPROVAL_NOTIFICATION_ID, notification)
        } catch (t: Throwable) {
            Log.w(TAG, "Failed posting approval notification: ${t.message}")
        }
    }

    private fun cancelApprovalNotification() {
        val nm = getSystemService(Context.NOTIFICATION_SERVICE) as? NotificationManager ?: return
        try {
            nm.cancel(APPROVAL_NOTIFICATION_ID)
        } catch (_: Throwable) {
        }
    }

    private fun buildOpenAppPendingIntent(requestCode: Int): PendingIntent {
        val openIntent = Intent(this, MainActivity::class.java).apply {
            addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_SINGLE_TOP or Intent.FLAG_ACTIVITY_CLEAR_TOP)
        }
        return PendingIntent.getActivity(
            this,
            requestCode,
            openIntent,
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
    }

    companion object {
        private const val TAG = "VeloxHostService"
        private const val CHANNEL_ID = "veloxdesk_host_sharing"
        private const val APPROVAL_CHANNEL_ID = "veloxdesk_host_approval"
        private const val NOTIFICATION_ID = 4101
        private const val APPROVAL_NOTIFICATION_ID = 4102
        const val ACTION_START_SHARING = "dev.veloxdesk.host.ACTION_START_SHARING"
        const val ACTION_STOP_SHARING = "dev.veloxdesk.host.ACTION_STOP_SHARING"
    }
}
