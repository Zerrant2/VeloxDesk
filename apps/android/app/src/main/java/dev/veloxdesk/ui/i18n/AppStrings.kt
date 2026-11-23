package dev.veloxdesk.ui.i18n

import android.content.res.Configuration
import android.content.res.Resources
import androidx.annotation.StringRes
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.ReadOnlyComposable
import androidx.compose.runtime.remember
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.platform.LocalContext
import dev.veloxdesk.data.AppLanguage
import java.util.Locale

val LocalAppLanguage = staticCompositionLocalOf { AppLanguage.EN }

val LocalLocalizedResources = staticCompositionLocalOf<Resources> {
    error("LocalLocalizedResources not provided")
}

/**
 * Provides reactive locale-specific [Resources] backed by `createConfigurationContext(Locale)`
 * so toggling EN <-> RU immediately updates every screen without recreating the Activity
 * or dropping an active remote session.
 */
@Composable
fun ProvideAppLocale(
    language: AppLanguage,
    content: @Composable () -> Unit,
) {
    val baseContext = LocalContext.current
    val localizedResources = remember(language, baseContext) {
        val locale = Locale(language.code)
        val config = Configuration(baseContext.resources.configuration)
        config.setLocale(locale)
        baseContext.createConfigurationContext(config).resources
    }
    CompositionLocalProvider(
        LocalAppLanguage provides language,
        LocalLocalizedResources provides localizedResources,
        content = content,
    )
}

@Composable
@ReadOnlyComposable
fun str(@StringRes id: Int): String = LocalLocalizedResources.current.getString(id)

@Composable
@ReadOnlyComposable
fun str(@StringRes id: Int, vararg formatArgs: Any): String =
    LocalLocalizedResources.current.getString(id, *formatArgs)
