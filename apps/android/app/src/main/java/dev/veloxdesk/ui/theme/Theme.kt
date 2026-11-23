package dev.veloxdesk.ui.theme

import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Typography
import androidx.compose.material3.darkColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.sp

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

private val VeloxDarkColorScheme = darkColorScheme(
    primary = VeloxColors.Primary,
    onPrimary = VeloxColors.OnPrimary,
    primaryContainer = Color(0xFF0C4A6E),
    onPrimaryContainer = Color(0xFFE0F2FE),
    secondary = VeloxColors.Emerald,
    onSecondary = Color(0xFF022C22),
    secondaryContainer = Color(0xFF064E3B),
    onSecondaryContainer = Color(0xFFD1FAE5),
    tertiary = VeloxColors.Amber,
    background = VeloxColors.Background,
    onBackground = VeloxColors.TextPrimary,
    surface = VeloxColors.Surface,
    onSurface = VeloxColors.TextPrimary,
    surfaceVariant = VeloxColors.SurfaceElevated,
    onSurfaceVariant = VeloxColors.TextSecondary,
    outline = VeloxColors.Border,
    outlineVariant = Color(0xFF1E293B),
    error = VeloxColors.Danger,
    onError = Color(0xFFFFFFFF),
    errorContainer = VeloxColors.DangerContainer,
    onErrorContainer = Color(0xFFFECACA),
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
fun VeloxDeskTheme(content: @Composable () -> Unit) {
    MaterialTheme(
        colorScheme = VeloxDarkColorScheme,
        typography = VeloxTypography,
        content = content,
    )
}
