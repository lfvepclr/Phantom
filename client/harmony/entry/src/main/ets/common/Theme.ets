/*
 * Single source of truth for the app's colours, radii and spacing.
 *
 * Earlier versions hard-coded `#007DFF` / `#999999` at every call site, which
 * made the pages drift apart and left status colours inconsistent between the
 * dashboard and the detail sheet. Everything visual now comes from here, so a
 * change of palette (or a future dark-mode variant) is one edit.
 *
 * The values follow the HarmonyOS light palette: a single brand blue for
 * actions, four semantic status colours, and a light page canvas so that white
 * cards read as separate surfaces.
 */

export class Theme {
  // Brand / actions
  static readonly brand: string = '#0A59F7';
  static readonly onBrand: string = '#FFFFFF';
  /** Tinted brand surface, used for selected pills and chips. */
  static readonly brandSoft: string = '#E8F0FE';

  // Status
  static readonly success: string = '#1FA84C';
  static readonly successSoft: string = '#E6F5EB';
  static readonly warning: string = '#E8A33D';
  static readonly warningSoft: string = '#FDF3E3';
  static readonly danger: string = '#E84026';
  static readonly dangerSoft: string = '#FCEAE7';
  static readonly neutral: string = '#6B7785';
  static readonly neutralSoft: string = '#EFF1F4';

  // Surfaces and text
  static readonly canvas: string = '#F4F5F7';
  static readonly surface: string = '#FFFFFF';
  static readonly surfaceAlt: string = '#F1F3F5';
  static readonly divider: string = '#14000000';
  static readonly textPrimary: string = '#182431';
  static readonly textSecondary: string = '#6B7785';
  static readonly textTertiary: string = '#9AA4AF';
  static readonly codeSurface: string = '#F7F8FA';

  // Geometry
  static readonly radiusCard: number = 16;
  static readonly radiusPill: number = 20;
  static readonly radiusChip: number = 12;
  static readonly cardPadding: number = 16;
  static readonly pagePadding: number = 16;
  static readonly gap: number = 12;
  static readonly iconSize: number = 20;
}
