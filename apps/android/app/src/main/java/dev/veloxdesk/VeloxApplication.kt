package dev.veloxdesk

import android.app.Application
import dev.veloxdesk.data.AppPreferences

class VeloxApplication : Application() {
    lateinit var preferences: AppPreferences
        private set

    override fun onCreate() {
        super.onCreate()
        preferences = AppPreferences(this)
    }
}
