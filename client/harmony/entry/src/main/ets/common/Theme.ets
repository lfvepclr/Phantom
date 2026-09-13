/*
 * Single source of truth for the app's colours, radii and spacing.
 *
 * The colours are *resource references* (`$r('app.color.…')`), not literals:
 * the platform resolves them against `resources/base` or `resources/dark`
 * depending on the current colour mode, which is what makes a mode change
 * repaint every page without any page subscribing to anything. Writing the
 * hex values here instead would mean hand-threading "is it dark?" through 100+
 * call sites and still getting the ones inside `@Builder` functions wrong.
 *
 * `ThemeMode` (below) is the user's three-way choice; `applyThemeMode` pushes
 * it into the ability's colour mode, and from then on the resource qualifier
 * does the work.
 */

import { common, ConfigurationConstant } from '@kit.AbilityKit';

export class Theme {
  // Brand / actions
  static readonly brand: ResourceColor = $r('app.color.brand');
  static readonly onBrand: ResourceColor = $r('app.color.onBrand');
  /** Tinted brand surface, used for selected pills and chips. */
  static readonly brandSoft: ResourceColor = $r('app.color.brandSoft');

  // Status
  static readonly success: ResourceColor = $r('app.color.success');
  static readonly successSoft: ResourceColor = $r('app.color.successSoft');
  static readonly warning: ResourceColor = $r('app.color.warning');
  static readonly warningSoft: ResourceColor = $r('app.color.warningSoft');
  static readonly danger: ResourceColor = $r('app.color.danger');
  static readonly dangerSoft: ResourceColor = $r('app.color.dangerSoft');
  static readonly neutral: ResourceColor = $r('app.color.neutral');
  static readonly neutralSoft: ResourceColor = $r('app.color.neutralSoft');

  // Surfaces and text
  static readonly canvas: ResourceColor = $r('app.color.canvas');
  static readonly surface: ResourceColor = $r('app.color.surface');
  static readonly surfaceAlt: ResourceColor = $r('app.color.surfaceAlt');
  static readonly divider: ResourceColor = $r('app.color.divider');
  static readonly textPrimary: ResourceColor = $r('app.color.textPrimary');
  static readonly textSecondary: ResourceColor = $r('app.color.textSecondary');
  static readonly textTertiary: ResourceColor = $r('app.color.textTertiary');
  static readonly codeSurface: ResourceColor = $r('app.color.codeSurface');

  // Geometry
  static readonly radiusCard: number = 16;
  static readonly radiusPill: number = 20;
  static readonly radiusChip: number = 12;
  static readonly cardPadding: number = 16;
  static readonly pagePadding: number = 16;
  static readonly gap: number = 12;
  static readonly iconSize: number = 20;

  /** The scan page's camera overlay is dark by design and never follows the theme. */
  static readonly overlayScrim: ResourceColor = '#66000000';
  static readonly overlayText: ResourceColor = '#FFFFFF';
}

/**
 * The user's three-way appearance choice, persisted as `themeMode`.
 *
 * `system` is the default: an OS-wide dark schedule is a decision the user
 * already made, and overriding it silently is the kind of thing that makes an
 * app feel foreign on the platform.
 */
export class ThemeMode {
  static readonly SYSTEM: string = 'system';
  static readonly LIGHT: string = 'light';
  static readonly DARK: string = 'dark';

  static readonly OPTIONS: string[] = ['system', 'light', 'dark'];

  static label(key: string): string {
    switch (key) {
      case ThemeMode.DARK: return '深色';
      case ThemeMode.LIGHT: return '浅色';
      default: return '跟随系统';
    }
  }
}

/**
 * Push a `themeMode` value into the ability's colour mode.
 *
 * `COLOR_MODE_NOT_SET` hands control back to the system, which is exactly what
 * "跟随系统" means — passing LIGHT or DARK there would freeze the app in that
 * mode even after the user's dark schedule kicks in.
 */
export function applyThemeMode(context: common.UIAbilityContext, mode: string): void {
  const colorMode: ConfigurationConstant.ColorMode =
    mode === ThemeMode.DARK
      ? ConfigurationConstant.ColorMode.COLOR_MODE_DARK
      : mode === ThemeMode.LIGHT
        ? ConfigurationConstant.ColorMode.COLOR_MODE_LIGHT
        : ConfigurationConstant.ColorMode.COLOR_MODE_NOT_SET;
  try {
    context.getApplicationContext().setColorMode(colorMode);
  } catch (e) {
    console.warn('[Phantom] setColorMode failed: ' + JSON.stringify(e));
  }
}
