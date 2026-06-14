// swift-tools-version: 5.9
import PackageDescription

// Phantom macOS SwiftUI menu-bar client.
//
// Two executable targets:
//   - PhantomMac       — the menu-bar app (links the Rust cdylib via -l phantom_client)
//   - PhantomMacBuilder — bundler that packages PhantomMac into Phantom.app
//
// Pattern borrowed from qoder/mytime: SPM as a sub-project (sibling of Cargo workspace),
// a Swift bundler target, and scripts/build-mac.sh to orchestrate cargo + swift build.
let package = Package(
    name: "PhantomMac",
    platforms: [.macOS(.v13)],
    products: [
        .executable(name: "PhantomMac", targets: ["PhantomMac"]),
        .executable(name: "PhantomMacBuilder", targets: ["PhantomMacBuilder"]),
    ],
    targets: [
        .executableTarget(
            name: "PhantomMac",
            path: "Sources/PhantomMac",
            resources: [
                // Bundle MenuBarIcon.png (+ @2x) into PhantomMac_PhantomMac.bundle
                // so MenuBarExtra can load it via Bundle.module.
                .process("Resources"),
            ],
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
        .executableTarget(
            name: "PhantomMacBuilder",
            path: "Sources/PhantomMacBuilder",
        ),
    ]
)