// swift-tools-version: 6.2
import PackageDescription

// Phantom macOS SwiftUI menu-bar client.
//
// One library plus two executables:
//   - PhantomMacKit    — pure logic (URI parsing, log view, whitelist, probes,
//                        menu-bar glyph). No UI state, so it is unit-testable.
//   - PhantomMac       — the menu-bar app (links the Rust cdylib via -l phantom_client)
//   - PhantomMacBuilder — bundler that packages PhantomMac into Phantom.app
//
// Pattern borrowed from qoder/mytime: SPM as a sub-project (sibling of Cargo workspace),
// a Swift bundler target, and scripts/build-mac.sh to orchestrate cargo + swift build.
let package = Package(
    name: "PhantomMac",
    platforms: [.macOS(.v26)],
    products: [
        .executable(name: "PhantomMac", targets: ["PhantomMac"]),
        .executable(name: "PhantomMacBuilder", targets: ["PhantomMacBuilder"]),
    ],
    targets: [
        .target(
            name: "PhantomMacKit",
            path: "Sources/PhantomMacKit"
        ),
        .executableTarget(
            name: "PhantomMac",
            dependencies: ["PhantomMacKit"],
            path: "Sources/PhantomMac",
            linkerSettings: [
                // Link client/mac/PhantomLibs/libphantom_client.dylib (cargo output
                // is copied there by scripts/build-mac.sh).
                //
                // rpath lets the bundled .app/Contents/MacOS/PhantomMac find its
                // dylib at runtime via Frameworks/, regardless of launch context.
                .unsafeFlags([
                    "-L", ".build/lib",
                    "-l", "phantom_client",
                    "-Xlinker", "-rpath",
                    "-Xlinker", "@executable_path/../Frameworks",
                ])
            ]
        ),
        .testTarget(
            name: "PhantomMacKitTests",
            dependencies: ["PhantomMacKit"],
            path: "Tests/PhantomMacKitTests"
        ),
        .executableTarget(
            name: "PhantomMacBuilder",
            path: "Sources/PhantomMacBuilder",
        ),
    ]
)
