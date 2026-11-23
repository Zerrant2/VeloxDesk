package dev.veloxdesk

import android.os.Bundle
import android.view.KeyEvent
import android.view.MotionEvent
import androidx.activity.ComponentActivity
import androidx.activity.SystemBarStyle
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.activity.viewModels
import dev.veloxdesk.ui.SessionViewModel
import dev.veloxdesk.ui.VeloxApp

class MainActivity : ComponentActivity() {
    private val sessionViewModel: SessionViewModel by viewModels()

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge(
            statusBarStyle = SystemBarStyle.dark(android.graphics.Color.TRANSPARENT),
            navigationBarStyle = SystemBarStyle.dark(android.graphics.Color.TRANSPARENT),
        )

        val prefs = (application as VeloxApplication).preferences
        setContent {
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
