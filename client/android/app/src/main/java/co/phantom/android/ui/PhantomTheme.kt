package co.phantom.android.ui

import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.graphics.Color
import co.phantom.android.ThemeMode

/*
 * One palette for the whole app.
 *
 * The values are the same ones `client/harmony/…/common/Theme.ets` uses, so a
 * screenshot from either phone reads the same: a single brand blue for actions,
 * four semantic status colours, and a page canvas that lets white cards read as
 * separate surfaces. Dark mode is a second set of the same names rather than a
 * filter over the light one — a dimmed white card is muddy, not dark.
 */

data class PhantomColors(
    val brand: Color,
    val onBrand: Color,
    val brandSoft: Color,
    val success: Color,
    val successSoft: Color,
    val warning: Color,
    val warningSoft: Color,
    val danger: Color,
    val dangerSoft: Color,
    val neutral: Color,
    val neutralSoft: Color,
    val canvas: Color,
    val surface: Color,
    val surfaceAlt: Color,
    val divider: Color,
    val textPrimary: Color,
    val textSecondary: Color,
    val textTertiary: Color,
    val codeSurface: Color,
)

private val LightColors = PhantomColors(
    brand = Color(0xFF0A59F7),
    onBrand = Color(0xFFFFFFFF),
    brandSoft = Color(0xFFE8F0FE),
    success = Color(0xFF1FA84C),
    successSoft = Color(0xFFE6F5EB),
    warning = Color(0xFFE8A33D),
    warningSoft = Color(0xFFFDF3E3),
    danger = Color(0xFFE84026),
    dangerSoft = Color(0xFFFCEAE7),
    neutral = Color(0xFF6B7785),
    neutralSoft = Color(0xFFEFF1F4),
    canvas = Color(0xFFF4F5F7),
    surface = Color(0xFFFFFFFF),
    surfaceAlt = Color(0xFFF1F3F5),
    divider = Color(0x14000000),
    textPrimary = Color(0xFF182431),
    textSecondary = Color(0xFF6B7785),
    textTertiary = Color(0xFF9AA4AF),
    codeSurface = Color(0xFFF7F8FA),
)

private val DarkColors = PhantomColors(
    // Brighter than the light-mode blue: on a dark canvas the light-mode value
    // loses contrast against the surface it sits on.
    brand = Color(0xFF4C8DFF),
    onBrand = Color(0xFF04121F),
    brandSoft = Color(0xFF16273F),
    success = Color(0xFF43C06C),
    successSoft = Color(0xFF14301E),
    warning = Color(0xFFEFAE4C),
    warningSoft = Color(0xFF34290F),
    danger = Color(0xFFFF6B52),
    dangerSoft = Color(0xFF3A1A15),
    neutral = Color(0xFF9AA4AF),
    neutralSoft = Color(0xFF23272C),
    canvas = Color(0xFF101214),
    surface = Color(0xFF1A1D21),
    surfaceAlt = Color(0xFF23272C),
    divider = Color(0x1FFFFFFF),
    textPrimary = Color(0xFFE8EAED),
    textSecondary = Color(0xFF9AA4AF),
    textTertiary = Color(0xFF6B7785),
    codeSurface = Color(0xFF14171A),
)

val LocalPhantomColors = staticCompositionLocalOf { LightColors }

/** True when the app should paint dark, given the user's three-way choice. */
fun resolveDarkMode(mode: ThemeMode, systemDark: Boolean): Boolean = when (mode) {
    ThemeMode.DARK -> true
    ThemeMode.LIGHT -> false
    ThemeMode.SYSTEM -> systemDark
}

/**
 * The palette for a three-way choice, outside of composition.
 *
 * The status and navigation bars are owned by the window, not by Compose, so
 * they need the same colours without a composition to read them from.
 */
fun phantomColorsFor(mode: ThemeMode, systemDark: Boolean): PhantomColors =
    if (resolveDarkMode(mode, systemDark)) DarkColors else LightColors

@Composable
fun PhantomTheme(
    mode: ThemeMode,
    content: @Composable () -> Unit,
) {
    val dark = resolveDarkMode(mode, isSystemInDarkTheme())
    val colors = phantomColorsFor(mode, dark)
    val scheme = if (dark) {
        darkColorScheme(
            primary = colors.brand,
            onPrimary = colors.onBrand,
            primaryContainer = colors.brandSoft,
            onPrimaryContainer = colors.textPrimary,
            background = colors.canvas,
            onBackground = colors.textPrimary,
            surface = colors.surface,
            onSurface = colors.textPrimary,
            surfaceVariant = colors.surfaceAlt,
            onSurfaceVariant = colors.textSecondary,
            error = colors.danger,
            onError = colors.onBrand,
            errorContainer = colors.dangerSoft,
            onErrorContainer = colors.textPrimary,
        )
    } else {
        lightColorScheme(
            primary = colors.brand,
            onPrimary = colors.onBrand,
            primaryContainer = colors.brandSoft,
            onPrimaryContainer = colors.textPrimary,
            background = colors.canvas,
            onBackground = colors.textPrimary,
            surface = colors.surface,
            onSurface = colors.textPrimary,
            surfaceVariant = colors.surfaceAlt,
            onSurfaceVariant = colors.textSecondary,
            error = colors.danger,
            onError = colors.onBrand,
            errorContainer = colors.dangerSoft,
            onErrorContainer = colors.textPrimary,
        )
    }

    CompositionLocalProvider(LocalPhantomColors provides colors) {
        MaterialTheme(colorScheme = scheme, content = content)
    }
}
