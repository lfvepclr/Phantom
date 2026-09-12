import AppKit
import XCTest
@testable import PhantomMacKit

/// The menu bar icon used to render as an unidentifiable dark blob. These tests
/// pin down what "visible" and "distinguishable" mean in pixels, so a future
/// refactor cannot silently bring the blob back.
///
/// Set `PHANTOM_GLYPH_DUMP=/some/dir` to also write one PNG per state for
/// eyeballing (see client/mac/README.md).
final class MenuBarGlyphTests: XCTestCase {
    private let states: [(String, PhantomState)] = [
        ("idle", .idle),
        ("connecting", .connecting),
        ("running", .running),
        ("error", .error("boom")),
    ]

    func testEveryStateDrawsSomething() {
        for (name, state) in states {
            let coverage = MenuBarGlyph.alphaCoverage(of: MenuBarGlyph.image(for: state))
            XCTAssertGreaterThan(coverage, 0.03, "\(name) 几乎不可见（coverage=\(coverage)）")
        }
    }

    func testStatesLookDifferent() {
        let coverages = states.map { MenuBarGlyph.alphaCoverage(of: MenuBarGlyph.image(for: $0.1)) }
        // Outline < outline+hem < solid, and the error badge adds ink on top.
        XCTAssertGreaterThan(coverages[1], coverages[0], "连接中应比未连接更实")
        XCTAssertGreaterThan(coverages[2], coverages[1], "已连接应为实心")
        XCTAssertGreaterThan(coverages[3], coverages[1], "错误态应带角标")
        XCTAssertNotEqual(coverages[0], coverages[2])
    }

    func testImageIsATemplate() {
        let image = MenuBarGlyph.image(for: .running)
        XCTAssertTrue(image.isTemplate, "菜单栏图标必须是模板图，否则深浅色下会变成黑块")
        XCTAssertEqual(image.size.width, MenuBarGlyph.canvasSize.width)
    }

    func testGlyphStaysInsideTheCanvas() {
        let path = MenuBarGlyph.ghostPath(in: NSRect(x: 0, y: 0, width: 18, height: 18))
        let bounds = path.bounds
        XCTAssertGreaterThanOrEqual(bounds.minX, 0)
        XCTAssertGreaterThanOrEqual(bounds.minY, 0)
        XCTAssertLessThanOrEqual(bounds.maxX, 18.0001)
        XCTAssertLessThanOrEqual(bounds.maxY, 18.0001)
        XCTAssertGreaterThan(bounds.height, 12, "幽灵轮廓太小，17pt 菜单栏里看不清")
    }

    func testOptionalPngDump() throws {
        guard let directory = ProcessInfo.processInfo.environment["PHANTOM_GLYPH_DUMP"] else {
            throw XCTSkip("设置 PHANTOM_GLYPH_DUMP=<dir> 时导出各状态 PNG，便于人工确认")
        }
        try FileManager.default.createDirectory(
            atPath: directory, withIntermediateDirectories: true
        )
        for (name, state) in states {
            let image = MenuBarGlyph.image(for: state)
            guard let tiff = image.tiffRepresentation,
                  let bitmap = NSBitmapImageRep(data: tiff),
                  let png = bitmap.representation(using: .png, properties: [:]) else {
                XCTFail("无法渲染 \(name)")
                continue
            }
            try png.write(to: URL(fileURLWithPath: directory).appendingPathComponent("\(name).png"))
        }
    }
}
