import SwiftUI
import PhantomMacKit

/// Colours, radii and spacing.
///
/// One place for the visual language so the header, the cards and the panels
/// cannot drift apart — the same reasoning as `common/Theme.ets` on the
/// HarmonyOS client, which this window mirrors.
enum Theme {
    static let cardRadius: CGFloat = 14
    static let chipRadius: CGFloat = 10
    static let cardPadding: CGFloat = 14
    static let pagePadding: CGFloat = 16
    static let gap: CGFloat = 12
    static let logMinHeight: CGFloat = 220

    static func accent(_ state: PhantomState) -> Color {
        Color(nsColor: state.accent)
    }

    static func accentSoft(_ state: PhantomState) -> Color {
        Color(nsColor: state.accent).opacity(0.14)
    }

    /// Card surface: a subtle plate that reads as a separate layer on both
    /// light and dark windows.
    static var surface: Color { Color(nsColor: .controlBackgroundColor) }
    static var canvas: Color { Color(nsColor: .windowBackgroundColor) }
}

/// Card container used by every section of the window.
struct Card<Content: View>: View {
    var padding: CGFloat = Theme.cardPadding
    @ViewBuilder var content: Content

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            content
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(padding)
        .background(
            RoundedRectangle(cornerRadius: Theme.cardRadius, style: .continuous)
                .fill(Theme.surface)
        )
        .overlay(
            RoundedRectangle(cornerRadius: Theme.cardRadius, style: .continuous)
                .strokeBorder(Color.secondary.opacity(0.15), lineWidth: 0.5)
        )
    }
}

/// Small monospaced label used for secondary facts.
struct Caption: View {
    let text: String

    var body: some View {
        Text(text)
            .font(.system(size: 11))
            .foregroundStyle(.secondary)
            .lineLimit(1)
            .truncationMode(.middle)
    }
}

/// One `label: value` row, as used by the connection-info card.
struct InfoRow: View {
    let label: String
    let value: String
    var monospaced = false

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 10) {
            Text(label)
                .font(.system(size: 12))
                .foregroundStyle(.secondary)
                .frame(width: 84, alignment: .leading)
            Text(value)
                .font(.system(size: 12, design: monospaced ? .monospaced : .default))
                .textSelection(.enabled)
                .lineLimit(1)
                .truncationMode(.middle)
            Spacer(minLength: 0)
        }
    }
}
