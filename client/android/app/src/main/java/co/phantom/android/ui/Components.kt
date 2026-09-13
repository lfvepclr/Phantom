package co.phantom.android.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

/*
 * Small shared pieces of the dashboard.
 *
 * Deliberately plain: the HarmonyOS client uses cards, pills and 12–16 dp
 * radii, and matching those numbers is what makes the two apps look like one
 * product rather than two ports.
 */

/** Surface plate used by every section. */
@Composable
fun PhantomCard(
    modifier: Modifier = Modifier,
    onClick: (() -> Unit)? = null,
    content: @Composable ColumnScope.() -> Unit,
) {
    val colors = LocalPhantomColors.current
    Column(
        modifier = modifier
            .fillMaxWidth()
            .clip(RoundedCornerShape(16.dp))
            .background(colors.surface)
            .then(if (onClick != null) Modifier.clickable { onClick() } else Modifier)
            .padding(16.dp),
        verticalArrangement = Arrangement.spacedBy(8.dp),
        content = content,
    )
}

/** Status chip; colour encodes the state and the text names it. */
@Composable
fun StatusPill(label: String, foreground: Color, background: Color) {
    Text(
        text = label,
        color = foreground,
        fontSize = 12.sp,
        modifier = Modifier
            .clip(RoundedCornerShape(20.dp))
            .background(background)
            .padding(horizontal = 10.dp, vertical = 4.dp),
    )
}

/** `地址  1.2.3.4:443` row used by the details sheet. */
@Composable
fun KeyValueRow(label: String, value: String, mono: Boolean = false) {
    val colors = LocalPhantomColors.current
    Row(modifier = Modifier.fillMaxWidth(), verticalAlignment = Alignment.Top) {
        Text(
            text = label,
            color = colors.textSecondary,
            fontSize = 13.sp,
            modifier = Modifier.width(96.dp),
        )
        Text(
            text = value,
            color = colors.textPrimary,
            fontSize = 13.sp,
            fontFamily = if (mono) FontFamily.Monospace else FontFamily.Default,
            modifier = Modifier.weight(1f),
        )
    }
}

@Composable
fun SectionLabel(text: String, modifier: Modifier = Modifier) {
    Text(
        text = text,
        color = LocalPhantomColors.current.textSecondary,
        fontSize = 13.sp,
        fontWeight = FontWeight.Medium,
        modifier = modifier,
    )
}

/** Hairline separator sized to the card content, not the screen. */
@Composable
fun PhantomDivider() {
    Spacer(
        modifier = Modifier
            .fillMaxWidth()
            .padding(vertical = 2.dp)
            .height(1.dp)
            .background(LocalPhantomColors.current.divider),
    )
}
