import Foundation

// PhantomMacBuilder — bundles the SPM-built PhantomMac binary + the Rust cdylib
// into a Mac .app bundle, ad-hoc codesigns it (hardened runtime), and
// finally packs everything into a `.dmg` for distribution.
//
// Pattern adapted from qoder/mytime's DMGBuilderExec (same author style):
// SPM as the Swift toolchain, a separate executable target for bundling,
// then codesign as the last step. We skip the AppleScript window layout
// (no custom icon yet) but keep the UDRW → UDZO pipeline.

@main
struct PhantomMacBuilder {
    static let appName = "Phantom"
    static let bundleId = "co.phantom.macos"
    static let version = "0.1.0"
    static let minimumSystemVersion = "13.0"

    @discardableResult
    static func shell(_ exe: String, _ args: [String], silent: Bool = false) throws -> Int32 {
        let p = Process()
        p.executableURL = URL(fileURLWithPath: exe)
        p.arguments = args
        if silent {
            p.standardOutput = FileHandle.nullDevice
            p.standardError = FileHandle.nullDevice
        }
        try p.run()
        p.waitUntilExit()
        return p.terminationStatus
    }

    /// Read a Mach-O dylib's LC_ID_DYLIB (install name) via `otool -D`.
    /// Returns the trimmed name string, or an empty string if the
    /// Mach-O doesn't have a dylib id (e.g. it's an executable).
    static func readInstallName(_ path: String) -> String {
        let p = Process()
        p.executableURL = URL(fileURLWithPath: "/usr/bin/otool")
        p.arguments = ["-D", path]
        let pipe = Pipe()
        p.standardOutput = pipe
        p.standardError = Pipe()
        do {
            try p.run()
        } catch {
            return ""
        }
        p.waitUntilExit()
        let data = pipe.fileHandleForReading.readDataToEndOfFile()
        let raw = String(data: data, encoding: .utf8) ?? ""
        for line in raw.split(separator: "\n") {
            let trimmed = line.trimmingCharacters(in: .whitespaces)
            if trimmed.isEmpty || trimmed.hasSuffix(":") { continue }
            return trimmed
        }
        return ""
    }

    /// Build a complete, Gatekeeper-friendly Info.plist. Required keys:
    ///   * CFBundleExecutable        — without this, macOS shows
    ///                                 "Phantom.app may be damaged or incomplete"
    ///   * CFBundleIdentifier
    ///   * CFBundleName / CFBundlePackageType / CFBundleShortVersionString
    ///   * CFBundleInfoDictionaryVersion
    ///   * LSMinimumSystemVersion    — required when the bundle's deployment
    ///                                 target is older than the host
    ///   * NSPrincipalClass          — required for GUI apps
    ///   * NSHighResolutionCapable   — Retina rendering
    ///
    /// If an `Info.plist` is provided at the package root, any keys it
    /// defines override the defaults (so operators can add e.g.
    /// `CFBundleIconFile`, `LSUIElement`).
    static func buildInfoPlist(cwd: String) -> String {
        // Hard-coded defaults: every key macOS expects a GUI bundle to have.
        let defaults: [String: String] = [
            "CFBundleDevelopmentRegion": "en",
            "CFBundleExecutable": appName,
            "CFBundleIdentifier": bundleId,
            "CFBundleInfoDictionaryVersion": "6.0",
            "CFBundleName": appName,
            "CFBundlePackageType": "APPL",
            "CFBundleShortVersionString": version,
            "CFBundleVersion": version,
            "LSMinimumSystemVersion": minimumSystemVersion,
            "NSHighResolutionCapable": "<true/>",
            "NSPrincipalClass": "NSApplication",
        ]

        // Overlay with the file at the package root, if any.
        var overlay: [String: String] = [:]
        let plistPath = "\(cwd)/Info.plist"
        if let raw = try? String(contentsOfFile: plistPath, encoding: .utf8) {
            overlay = parsePlistTopLevelKeys(raw)
        }

        // Build the final plist: defaults first, then overlay overrides.
        var keys = defaults.keys.sorted()
        for k in overlay.keys where !keys.contains(k) {
            keys.append(k)
        }
        keys.sort()

        var lines: [String] = [
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>",
            "<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">",
            "<plist version=\"1.0\">",
            "<dict>",
        ]
        for key in keys {
            let value = overlay[key] ?? defaults[key]!
            lines.append("    <key>\(key)</key>")
            if value == "<true/>" {
                lines.append("    <true/>")
            } else {
                lines.append("    <string>\(value)</string>")
            }
        }
        lines += ["</dict>", "</plist>", ""]
        return lines.joined(separator: "\n")
    }

    /// Lightweight top-level key/value extraction from a plist XML.
    /// We don't try to be a general plist parser — only string keys at the
    /// top level are recognised, which is enough for Info.plist overrides.
    static func parsePlistTopLevelKeys(_ xml: String) -> [String: String] {
        var result: [String: String] = [:]
        // Match <key>NAME</key> ... <string>VALUE</string> or <true/>
        let pattern = #"<key>([^<]+)</key>\s*<string>([^<]*)</string>"#
        guard let regex = try? NSRegularExpression(pattern: pattern) else {
            return result
        }
        let range = NSRange(xml.startIndex..., in: xml)
        regex.enumerateMatches(in: xml, range: range) { match, _, _ in
            guard let m = match,
                  let keyRange = Range(m.range(at: 1), in: xml),
                  let valRange = Range(m.range(at: 2), in: xml) else { return }
            result[String(xml[keyRange])] = String(xml[valRange])
        }
        // Also handle <key>NAME</key> <true/> (e.g. LSUIElement, NSHighResolutionCapable)
        let boolPattern = #"<key>([^<]+)</key>\s*<true/>"#
        if let boolRegex = try? NSRegularExpression(pattern: boolPattern) {
            boolRegex.enumerateMatches(in: xml, range: range) { match, _, _ in
                guard let m = match,
                      let keyRange = Range(m.range(at: 1), in: xml) else { return }
                result[String(xml[keyRange])] = "<true/>"
            }
        }
        return result
    }

    static func main() async throws {
        let fm = FileManager.default
        // SPM runs the executable with CWD set to the package root (where
        // Package.swift lives), so all paths below are relative to client/mac/.
        let cwd = fm.currentDirectoryPath

        print("🔨 swift build -c release ...")
        let buildStatus = try shell("/usr/bin/swift", ["build", "-c", "release"])
        guard buildStatus == 0 else {
            print("❌ swift build failed")
            exit(1)
        }

        let buildDir = "\(cwd)/.build/release"
        let appPath = "\(cwd)/Phantom.app"
        let contents = "\(appPath)/Contents"
        let macosDir = "\(contents)/MacOS"
        let fwDir = "\(contents)/Frameworks"
        let distDir = "\(cwd)/dist"
        let dmgPath = "\(distDir)/Phantom.dmg"
        let rwDmgPath = "\(cwd)/.build/Phantom-rw.dmg"
        let stagingDir = "\(cwd)/.build/dmg-root"

        // 1. Wipe & recreate the bundle skeleton
        try? fm.removeItem(atPath: appPath)
        try? fm.removeItem(atPath: stagingDir)
        try? fm.removeItem(atPath: rwDmgPath)
        if fm.fileExists(atPath: dmgPath) {
            try? fm.removeItem(atPath: dmgPath)
        }
        try fm.createDirectory(atPath: macosDir, withIntermediateDirectories: true)
        try fm.createDirectory(atPath: fwDir, withIntermediateDirectories: true)
        try fm.createDirectory(atPath: distDir, withIntermediateDirectories: true)

        // 2. Copy the SPM-built executable into Contents/MacOS/
        let exeSrc = "\(buildDir)/PhantomMac"
        guard fm.fileExists(atPath: exeSrc) else {
            print("❌ SPM did not produce \(exeSrc)")
            exit(1)
        }
        let exeDst = "\(macosDir)/PhantomMac"
        try fm.copyItem(atPath: exeSrc, toPath: exeDst)
        print("✅ Copied SPM executable into Contents/MacOS/PhantomMac")

        // 3. Copy the Rust dylib into Contents/Frameworks/ and rewrite its
        // install_name to `@rpath/libphantom_client.dylib`.
        let dylibSrc = "\(cwd)/PhantomLibs/libphantom_client.dylib"
        guard fm.fileExists(atPath: dylibSrc) else {
            print("❌ PhantomLibs/libphantom_client.dylib missing — run scripts/build-mac.sh first")
            exit(1)
        }
        let dylibDst = "\(fwDir)/libphantom_client.dylib"
        let oldInstallName = readInstallName(dylibSrc)
        print("   dylib current install_name: \(oldInstallName)")
        try fm.copyItem(atPath: dylibSrc, toPath: dylibDst)
        let newInstallName = "@rpath/libphantom_client.dylib"
        let idStatus = try shell(
            "/usr/bin/install_name_tool",
            ["-id", newInstallName, dylibDst]
        )
        guard idStatus == 0 else {
            print("❌ Failed to rewrite dylib install_name")
            exit(1)
        }
        print("✅ Copied Rust dylib into Contents/Frameworks/ (install_name → \(newInstallName))")

        // 4. Write a complete Info.plist (merged from defaults + package-root
        // overlay). Without CFBundleExecutable / CFBundlePackageType /
        // NSPrincipalClass macOS shows "may be damaged or incomplete".
        let plistXML = buildInfoPlist(cwd: cwd)
        try plistXML.write(toFile: "\(contents)/Info.plist", atomically: true, encoding: .utf8)
        print("✅ Wrote Info.plist (Gatekeeper-complete)")

        // 5. Rewrite PhantomMac's LC_LOAD_DYLIB so the binary resolves the
        // dylib via rpath (`@executable_path/../Frameworks`) instead of the
        // cargo-embedded absolute path.
        if !oldInstallName.isEmpty, oldInstallName != newInstallName {
            let changeStatus = try shell(
                "/usr/bin/install_name_tool",
                ["-change", oldInstallName, newInstallName, exeDst]
            )
            guard changeStatus == 0 else {
                print("❌ Failed to rewrite PhantomMac LC_LOAD_DYLIB")
                exit(1)
            }
            print("✅ Rewrote PhantomMac LC_LOAD_DYLIB: \(oldInstallName) → \(newInstallName)")
        }

        // 6. Ad-hoc codesign with hardened runtime. Sign nested code first
        // (dylib before host) — `codesign --deep` does this automatically
        // but we keep the explicit calls so the order is obvious.
        let signDylib = try shell(
            "/usr/bin/codesign",
            [
                "--force", "--sign", "-",
                "--options", "runtime",
                "\(fwDir)/libphantom_client.dylib",
            ]
        )
        guard signDylib == 0 else {
            print("❌ Failed to codesign dylib")
            exit(1)
        }
        let signApp = try shell(
            "/usr/bin/codesign",
            [
                "--force", "--deep", "--sign", "-",
                "--options", "runtime",
                appPath,
            ]
        )
        guard signApp == 0 else {
            print("❌ Failed to codesign .app")
            exit(1)
        }
        print("✅ Ad-hoc codesigned with hardened runtime")

        // 7. Verify the .app is well-formed.
        let verifyStatus = try shell("/usr/bin/codesign", ["--verify", "--strict", "--deep", appPath])
        guard verifyStatus == 0 else {
            print("❌ codesign --verify failed")
            exit(1)
        }
        let plutilStatus = try shell("/usr/bin/plutil", ["-lint", "\(contents)/Info.plist"])
        guard plutilStatus == 0 else {
            print("❌ plutil -lint failed on Info.plist")
            exit(1)
        }

        print("")
        print("✅ Built Phantom.app")
        print("   Path  : \(appPath)")
        print("   Launch: sudo open \(appPath)  # sudo required for TUN device")

        // 8. Stage the .app + Applications symlink for DMG creation.
        print("")
        print("💿 Building Phantom.dmg ...")
        try fm.createDirectory(atPath: stagingDir, withIntermediateDirectories: true)
        try fm.copyItem(atPath: appPath, toPath: "\(stagingDir)/Phantom.app")
        try fm.createSymbolicLink(
            atPath: "\(stagingDir)/Applications",
            withDestinationPath: "/Applications"
        )

        // 9. Create a read-write DMG (UDRW / HFS+). AppleScript window
        // styling is skipped — we have no custom icon yet, so a plain
        // Finder view is fine.
        let createStatus = try shell(
            "/usr/bin/hdiutil",
            [
                "create",
                "-volname", appName,
                "-srcfolder", stagingDir,
                "-fs", "HFS+",
                "-format", "UDRW",
                "-ov",
                rwDmgPath,
            ],
            silent: true
        )
        guard createStatus == 0 else {
            print("❌ Failed to create writable DMG")
            exit(1)
        }

        // 10. Convert to compressed UDZO. zlib-level=9 → smallest file.
        let convertStatus = try shell(
            "/usr/bin/hdiutil",
            [
                "convert", rwDmgPath,
                "-format", "UDZO",
                "-imagekey", "zlib-level=9",
                "-ov",
                "-o", dmgPath,
            ],
            silent: true
        )
        guard convertStatus == 0 else {
            print("❌ Failed to convert DMG to UDZO")
            exit(1)
        }

        // 11. Ad-hoc sign the DMG (so the dmg itself survives Gatekeeper
        // checks and `open Phantom.dmg` works without a quarantine prompt).
        let signDmg = try shell(
            "/usr/bin/codesign",
            ["--force", "--sign", "-", dmgPath]
        )
        guard signDmg == 0 else {
            print("❌ Failed to codesign DMG")
            exit(1)
        }

        // 12. Cleanup temporary DMG artefacts.
        try? fm.removeItem(atPath: rwDmgPath)
        try? fm.removeItem(atPath: stagingDir)

        print("✅ DMG created: \(dmgPath)")
        print("   Mount  : open \(dmgPath)  # then drag Phantom.app into /Applications")
    }
}
