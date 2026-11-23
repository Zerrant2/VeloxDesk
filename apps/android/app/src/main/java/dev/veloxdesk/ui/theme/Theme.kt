package dev.veloxdesk.ui.theme

import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Typography
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.Immutable
import androidx.compose.runtime.ReadOnlyComposable
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.sp
import dev.veloxdesk.data.AppThemeMode

/**
 * Legacy dark palette constants kept for dark-only video viewport overlays (e.g. HUD / floating
 * toolbar on top of black video frames) and backwards compatibility.
 */
object VeloxColors {
    val Background = Color(0xFF0F172A)
    val Surface = Color(0xFF1E293B)
    val SurfaceElevated = Color(0xFF273549)
    val SurfaceOverlay = Color(0xCC0F172A)
    val Border = Color(0xFF334155)
    val Primary = Color(0xFF38BDF8)
    val PrimaryDark = Color(0xFF0284C7)
    val OnPrimary = Color(0xFF082F49)
    val Emerald = Color(0xFF10B981)
    val Amber = Color(0xFFF59E0B)
    val Danger = Color(0xFFEF4444)
    val DangerContainer = Color(0xFF450A0A)
    val TextPrimary = Color(0xFFF8FAFC)
    val TextSecondary = Color(0xFF94A3B8)
    val TextMuted = Color(0xFF64748B)
}

/**
 * Theme-adaptive semantic colors that guarantee crisp WCAG contrast in both Dark and Light themes.
 */
@Immutable
data class VeloxSemanticColors(
    val isDark: Boolean,
    val background: Color,
    val surface: Color,
    val surfaceElevated: Color,
    val surfaceOverlay: Color,
    val border: Color,
    val primary: Color,
    val onPrimary: Color,
    val primaryBannerBg: Color,
    val success: Color,
    val successContainer: Color,
    val warning: Color,
    val warningContainer: Color,
    val danger: Color,
    val dangerContainer: Color,
    val textPrimary: Color,
    val textSecondary: Color,
    val textMuted: Color,
)

private val DarkSemanticColors = VeloxSemanticColors(
    isDark = true,
    background = Color(0xFF0F172A),
    surface = Color(0xFF1E293B),
    surfaceElevated = Color(0xFF273549),
    surfaceOverlay = Color(0xCC0F172A),
    border = Color(0xFF334155),
    primary = Color(0xFF38BDF8),
    onPrimary = Color(0xFF082F49),
    primaryBannerBg = Color(0xA60C4A6E),
    success = Color(0xFF10B981),
    successContainer = Color(0xFF064E3B),
    warning = Color(0xFFF59E0B),
    warningContainer = Color(0x26F59E0B),
    danger = Color(0xFFEF4444),
    dangerContainer = Color(0xFF450A0A),
    textPrimary = Color(0xFFF8FAFC),
    textSecondary = Color(0xFF94A3B8),
    textMuted = Color(0xFF64748B),
)

private val LightSemanticColors = VeloxSemanticColors(
    isDark = false,
    background = Color(0xFFF1F5F9),
    surface = Color(0xFFFFFFFF),
    surfaceElevated = Color(0xFFE2E8F0),
    surfaceOverlay = Color(0xE60F172A),
    border = Color(0xFFCBD5E1),
    primary = Color(0xFF0284C7),
    onPrimary = Color(0xFFFFFFFF),
    primaryBannerBg = Color(0xFFE0F2FE),
    success = Color(0xFF047857),
    successContainer = Color(0xFFD1FAE5),
    warning = Color(0xFFB45309),
    warningContainer = Color(0xFFFEF3C7),
    danger = Color(0xFFDC2626),
    dangerContainer = Color(0xFFFEE2E2),
    textPrimary = Color(0xFF0F172A),
    textSecondary = Color(0xFF475569),
    textMuted = Color(0xFF64748B),
)

val LocalVeloxColors = staticCompositionLocalOf { DarkSemanticColors }

object VeloxTheme {
    val colors: VeloxSemanticColors
        @Composable
        @ReadOnlyComposable
        get() = LocalVeloxColors.current
}

private val VeloxDarkColorScheme = darkColorScheme(
    primary = DarkSemanticColors.primary,
    onPrimary = DarkSemanticColors.onPrimary,
    primaryContainer = Color(0xFF0C4A6E),
    onPrimaryContainer = Color(0xFFE0F2FE),
    secondary = DarkSemanticColors.success,
    onSecondary = Color(0xFF022C22),
    secondaryContainer = DarkSemanticColors.successContainer,
    onSecondaryContainer = Color(0xFFD1FAE5),
    tertiary = DarkSemanticColors.warning,
    background = DarkSemanticColors.background,
    onBackground = DarkSemanticColors.textPrimary,
    surface = DarkSemanticColors.surface,
    onSurface = DarkSemanticColors.textPrimary,
    surfaceVariant = DarkSemanticColors.surfaceElevated,
    onSurfaceVariant = DarkSemanticColors.textSecondary,
    outline = DarkSemanticColors.border,
    outlineVariant = Color(0xFF1E293B),
    error = DarkSemanticColors.danger,
    onError = Color(0xFFFFFFFF),
    errorContainer = DarkSemanticColors.dangerContainer,
    onErrorContainer = Color(0xFFFECACA),
)

private val VeloxLightColorScheme = lightColorScheme(
    primary = LightSemanticColors.primary,
    onPrimary = LightSemanticColors.onPrimary,
    primaryContainer = Color(0xFFE0F2FE),
    onPrimaryContainer = Color(0xFF0C4A6E),
    secondary = LightSemanticColors.success,
    onSecondary = Color(0xFFFFFFFF),
    secondaryContainer = LightSemanticColors.successContainer,
    onSecondaryContainer = Color(0xFF064E3B),
    tertiary = LightSemanticColors.warning,
    onTertiary = Color(0xFFFFFFFF),
    tertiaryContainer = LightSemanticColors.warningContainer,
    onTertiaryContainer = Color(0xFF78350F),
    background = LightSemanticColors.background,
    onBackground = LightSemanticColors.textPrimary,
    surface = LightSemanticColors.surface,
    onSurface = LightSemanticColors.textPrimary,
    surfaceVariant = LightSemanticColors.surfaceElevated,
    onSurfaceVariant = LightSemanticColors.textSecondary,
    outline = LightSemanticColors.border,
    outlineVariant = Color(0xFFE2E8F0),
    error = LightSemanticColors.danger,
    onError = Color(0xFFFFFFFF),
    errorContainer = LightSemanticColors.dangerContainer,
    onErrorContainer = Color(0xFF7F1D1D),
)

private val VeloxTypography = Typography(
    headlineSmall = TextStyle(
        fontWeight = FontWeight.Bold,
        fontSize = 22.sp,
        lineHeight = 28.sp,
        letterSpacing = (-0.2).sp,
    ),
    titleLarge = TextStyle(
        fontWeight = FontWeight.SemiBold,
        fontSize = 18.sp,
        lineHeight = 24.sp,
    ),
    titleMedium = TextStyle(
        fontWeight = FontWeight.SemiBold,
        fontSize = 15.sp,
        lineHeight = 20.sp,
    ),
    bodyMedium = TextStyle(
        fontWeight = FontWeight.Normal,
        fontSize = 14.sp,
        lineHeight = 20.sp,
    ),
    bodySmall = TextStyle(
        fontWeight = FontWeight.Normal,
        fontSize = 12.sp,
        lineHeight = 16.sp,
    ),
    labelMedium = TextStyle(
        fontWeight = FontWeight.Medium,
        fontSize = 12.sp,
        lineHeight = 16.sp,
    ),
    labelSmall = TextStyle(
        fontFamily = FontFamily.Monospace,
        fontWeight = FontWeight.Medium,
        fontSize = 11.sp,
        lineHeight = 14.sp,
    ),
)

@Composable
fun resolveDarkTheme(themeMode: AppThemeMode): Boolean = when (themeMode) {
    AppThemeMode.SYSTEM -> isSystemInDarkTheme()
    AppThemeMode.DARK -> true
    AppThemeMode.LIGHT -> false
}

@Composable
fun VeloxDeskTheme(
    themeMode: AppThemeMode = AppThemeMode.SYSTEM,
    content: @Composable () -> Unit,
) {
    val darkTheme = resolveDarkTheme(themeMode)
    val colorScheme = if (darkTheme) VeloxDarkColorScheme else VeloxLightColorScheme
    val semanticColors = if (darkTheme) DarkSemanticColors else LightSemanticColors

    CompositionLocalProvider(LocalVeloxColors provides semanticColors) {
        MaterialTheme(
            colorScheme = colorScheme,
            typography = VeloxTypography,
            content = content,
        )
    }
}
