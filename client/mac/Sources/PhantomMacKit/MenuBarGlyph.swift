import AppKit

/// The menu bar icon, drawn as a **template** image.
///
/// Two bugs are fixed by construction here:
///
/// * The previous label was a SwiftUI `Shape` filled with a state colour.
///   macOS renders menu bar extras as templates, so the colour was discarded
///   and the glyph came out as an unreadable dark blob that never changed.
///   A template `NSImage` drawn with alpha only is what the system expects, and
///   it adapts to light/dark menu bars automatically.
/// * Because colour cannot be trusted in the menu bar, the four states are told
///   apart by **silhouette** instead: outline (idle), outline + filled hem
///   (connecting), solid (running), solid + exclamation badge (error).
public enum MenuBarGlyph {
    /// Canvas size in points; 18pt matches the other extras in the bar.
    public static let canvasSize = NSSize(width: 18, height: 18)

    /// - Parameter needsAttention: draws a small dot beside the ghost, used on
    ///   a fresh install where nothing is configured yet. The popover cannot be
    ///   opened programmatically, so the icon itself has to carry the hint that
    ///   there is something to do here.
    public static func image(
        for state: PhantomState,
        needsAttention: Bool = false,
        size: NSSize = canvasSize
    ) -> NSImage {
        let image = NSImage(size: size, flipped: false) { rect in
            draw(state: state, in: rect)
            if needsAttention {
                drawAttentionDot(in: rect)
            }
            return true
        }
        image.isTemplate = true
        return image
    }

    /// A single solid dot in the bottom-right corner.
    ///
    /// Deliberately *not* the exclamation badge: that one means "error", and
    /// "you have not configured anything yet" is not an error.
    private static func drawAttentionDot(in rect: NSRect) {
        let radius = rect.width * 0.13
        let centre = NSPoint(x: rect.maxX - radius - 0.5, y: rect.minY + radius + 0.5)
        NSColor.black.setFill()
        NSBezierPath(
            ovalIn: NSRect(
                x: centre.x - radius,
                y: centre.y - radius,
                width: radius * 2,
                height: radius * 2
            )
        ).fill()
    }

    /// Ghost silhouette in `rect`: semicircular head, straight flanks and a
    /// three-scallop hem, with two round eyes.
    ///
    /// `includeEyes` is separated out because the caller punches them with
    /// `.clear` when filling and draws them solid when stroking.
    public static func ghostPath(in rect: NSRect, includeEyes: Bool = true) -> NSBezierPath {
        let width = rect.width
        let height = rect.height
        // A ghost is taller than it is wide: with a square canvas the head
        // (half the width) plus the hem must share the height, so the lobes get
        // 30% of it and the straight flanks take what is left. Without this the
        // scallops collapse into a bowl — the "bucket" the first draft drew.
        let radius = width * 0.5
        let lobeDepth = min(height * 0.30, height - radius - 2)
        let hemY = rect.minY + lobeDepth
        let path = NSBezierPath()

        // Head: left-mid → top → right-mid.
        //
        // AppKit measures angles counter-clockwise from +x, so 180° → 0° runs
        // through 270° (the *bottom*). `clockwise: true` is what actually
        // sweeps over the top; without it the head comes out mirrored and the
        // silhouette collapses into a bowl.
        path.move(to: NSPoint(x: rect.minX, y: rect.maxY - radius))
        path.appendArc(
            withCenter: NSPoint(x: rect.minX + radius, y: rect.maxY - radius),
            radius: radius,
            startAngle: 180,
            endAngle: 0,
            clockwise: true
        )
        path.line(to: NSPoint(x: rect.maxX, y: hemY))

        // Three rounded lobes, right → left, hanging below the hem line.
        let lobe = width / 3
        var x = rect.maxX
        for _ in 0..<3 {
            let next = max(rect.minX, x - lobe)
            path.curve(
                to: NSPoint(x: next, y: hemY),
                controlPoint1: NSPoint(x: x - lobe * 0.28, y: hemY - lobeDepth * 0.9),
                controlPoint2: NSPoint(x: next + lobe * 0.28, y: hemY - lobeDepth * 0.9)
            )
            x = next
        }
        path.line(to: NSPoint(x: rect.minX, y: rect.maxY - radius))
        path.close()

        if includeEyes {
            let eyeRadius = width * 0.085
            let eyeY = rect.midY + height * 0.06
            for dx in [rect.minX + width * 0.34, rect.minX + width * 0.66] {
                path.appendOval(
                    in: NSRect(
                        x: dx - eyeRadius,
                        y: eyeY - eyeRadius,
                        width: eyeRadius * 2,
                        height: eyeRadius * 2
                    )
                )
            }
        }
        return path
    }

    // MARK: - Drawing

    private static func draw(state: PhantomState, in rect: NSRect) {
        let inset = rect.insetBy(dx: 0.6, dy: 0.6)
        let context = NSGraphicsContext.current
        let previous = context?.compositingOperation
        defer { if let previous { context?.compositingOperation = previous } }

        switch state {
        case .idle:
            // Pure outline: "installed but doing nothing".
            strokeGhost(in: inset)

        case .connecting:
            // Outline plus a single dot: "working on it" without ever being
            // mistaken for the solid connected glyph at a glance.
            strokeGhost(in: inset)
            NSColor.black.setFill()
            let dotRadius = inset.width * 0.11
            let dotCentre = NSPoint(
                x: inset.midX,
                y: inset.minY + inset.height * 0.33
            )
            NSBezierPath(
                ovalIn: NSRect(
                    x: dotCentre.x - dotRadius,
                    y: dotCentre.y - dotRadius,
                    width: dotRadius * 2,
                    height: dotRadius * 2
                )
            ).fill()

        case .running:
            let body = ghostPath(in: inset, includeEyes: false)
            NSColor.black.setFill()
            body.fill()
            // Eyes are holes, so they read at any menu bar tint.
            NSGraphicsContext.current?.compositingOperation = .clear
            eyesPath(in: inset).fill()

        case .error:
            let body = ghostPath(in: inset, includeEyes: false)
            NSColor.black.setFill()
            body.fill()
            NSGraphicsContext.current?.compositingOperation = .clear
            eyesPath(in: inset).fill()
            // The badge sits in the empty top-right corner. The disc is punched
            // out of the ghost and the "!" is drawn back on top, so the mark
            // stays legible no matter what tint the menu bar gives the template.
            let badgeRadius = inset.width * 0.21
            let badgeCentre = NSPoint(
                x: inset.maxX - badgeRadius * 0.85,
                y: inset.maxY - badgeRadius * 0.95
            )
            NSBezierPath(
                ovalIn: NSRect(
                    x: badgeCentre.x - badgeRadius,
                    y: badgeCentre.y - badgeRadius,
                    width: badgeRadius * 2,
                    height: badgeRadius * 2
                )
            ).fill()
            NSGraphicsContext.current?.compositingOperation = .sourceOver
            NSColor.black.setFill()
            let bar = NSBezierPath(
                roundedRect: NSRect(
                    x: badgeCentre.x - badgeRadius * 0.13,
                    y: badgeCentre.y - badgeRadius * 0.05,
                    width: badgeRadius * 0.26,
                    height: badgeRadius * 0.85
                ),
                xRadius: badgeRadius * 0.13,
                yRadius: badgeRadius * 0.13
            )
            bar.fill()
            NSBezierPath(
                ovalIn: NSRect(
                    x: badgeCentre.x - badgeRadius * 0.15,
                    y: badgeCentre.y - badgeRadius * 0.60,
                    width: badgeRadius * 0.30,
                    height: badgeRadius * 0.30
                )
            ).fill()
        }
    }

    private static func strokeGhost(in rect: NSRect) {
        let body = ghostPath(in: rect)
        body.lineWidth = max(1.1, rect.width * 0.09)
        body.lineJoinStyle = .round
        NSColor.black.setStroke()
        body.stroke()
        NSColor.black.setFill()
        eyesPath(in: rect).fill()
    }

    private static func eyesPath(in rect: NSRect) -> NSBezierPath {
        let path = NSBezierPath()
        let width = rect.width
        let radius = width * 0.5
        let eyeRadius = width * 0.085
        let eyeY = rect.maxY - radius * 0.95
        for dx in [rect.minX + width * 0.34, rect.minX + width * 0.66] {
            path.appendOval(
                in: NSRect(
                    x: dx - eyeRadius,
                    y: eyeY - eyeRadius,
                    width: eyeRadius * 2,
                    height: eyeRadius * 2
                )
            )
        }
        return path
    }

    // MARK: - Test support

    /// Fraction of pixels that carry any alpha, used by the unit tests to prove
    /// each state actually renders something (and something different).
    public static func alphaCoverage(of image: NSImage) -> Double {
        guard let tiff = image.tiffRepresentation,
              let bitmap = NSBitmapImageRep(data: tiff) else {
            return 0
        }
        var covered = 0
        var total = 0
        for y in 0..<bitmap.pixelsHigh {
            for x in 0..<bitmap.pixelsWide {
                total += 1
                if let color = bitmap.colorAt(x: x, y: y), color.alphaComponent > 0.35 {
                    covered += 1
                }
            }
        }
        return total > 0 ? Double(covered) / Double(total) : 0
    }
}
