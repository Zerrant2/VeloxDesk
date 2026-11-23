package dev.veloxdesk

import android.graphics.Color
import android.os.Bundle
import android.view.KeyEvent
import android.view.MotionEvent
import androidx.activity.ComponentActivity
import androidx.activity.SystemBarStyle
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.activity.viewModels
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import dev.veloxdesk.ui.SessionViewModel
import dev.veloxdesk.ui.VeloxApp
import dev.veloxdesk.ui.theme.resolveDarkTheme

class MainActivity : ComponentActivity() {
    private val sessionViewModel: SessionViewModel by viewModels()

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()

        val prefs = (application as VeloxApplication).preferences
        setContent {
            val settings by prefs.settings.collectAsState()
            val darkTheme = resolveDarkTheme(settings.themeMode)

            DisposableEffect(darkTheme) {
                if (darkTheme) {
                    enableEdgeToEdge(
                        statusBarStyle = SystemBarStyle.dark(Color.TRANSPARENT),
                        navigationBarStyle = SystemBarStyle.dark(Color.TRANSPARENT),
                    )
                } else {
                    enableEdgeToEdge(
                        statusBarStyle = SystemBarStyle.light(Color.TRANSPARENT, Color.TRANSPARENT),
                        navigationBarStyle = SystemBarStyle.light(Color.TRANSPARENT, Color.TRANSPARENT),
                    )
                }
                onDispose {}
            }

            VeloxApp(
                preferences = prefs,
                sessionViewModel = sessionViewModel,
            )
        }
    }

    override fun dispatchKeyEvent(event: KeyEvent): Boolean {
        if (sessionViewModel.dispatchHardwareKeyEvent(event)) {
            return true
        }
        return super.dispatchKeyEvent(event)
    }

    override fun dispatchGenericMotionEvent(event: MotionEvent): Boolean {
        if (sessionViewModel.dispatchGenericMotionEvent(event)) {
            return true
        }
        return super.dispatchGenericMotionEvent(event)
    }

    override fun onStop() {
        sessionViewModel.onAppBackgrounded()
        super.onStop()
    }

    override fun onDestroy() {
        if (isFinishing) {
            sessionViewModel.disconnect()
        }
        super.onDestroy()
    }
}
