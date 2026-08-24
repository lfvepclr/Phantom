//! Phantom xtask — unified build orchestrator.
//!
//! Usage:
//!   cargo xtask build [all|server|cli|router|mac|android|harmony] [--release|--debug]
//!   cargo xtask check-deps
//!   cargo xtask icons
//!   cargo xtask clean

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

// ── CLI ──────────────────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(name = "xtask", about = "Phantom unified build orchestrator")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Build one or more targets
    Build {
        /// Target(s) to build: all, server, server-arm64, cli, router, router-armv7, mac, android, harmony
        target: Vec<String>,
        /// Build in release mode (default)
        #[arg(long, default_value_t = true)]
        release: bool,
        /// Build in debug mode
        #[arg(long)]
        debug: bool,
        /// Extra cargo features, currently only honoured by `server-arm64`
        /// (e.g. `--features io-uring` enables the io_uring runtime; needs a
        /// ≥5.10 kernel on the target host).
        #[arg(long)]
        features: Vec<String>,
    },
    /// Check dependencies and print status table
    CheckDeps,
    /// Generate platform icons from source appicon.png
    Icons,
    /// Clean all build artifacts
    Clean,
}

// ── Project paths ────────────────────────────────────────────────────────────

fn project_root() -> PathBuf {
    Path::new(&env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".into()))
        .ancestors()
        .nth(1)
        .unwrap()
        .to_path_buf()
}

/// Rust target triple for the router client.
///
/// The ASUS RT-AX86U Pro is an ARMv8 (Broadcom BCM4912) box running Asuswrt /
/// Asuswrt-Merlin. Its glibc is old and lacks dev headers, so the router build
/// is statically linked against musl.
const ROUTER_TARGET: &str = "aarch64-unknown-linux-musl";

/// Rust target triple for legacy 32-bit ARM routers (ARMv7 hard-float,
/// e.g. older Broadcom/Qualcomm boxes). Also statically linked via musl and
/// linked with the toolchain's own rust-lld, so no C toolchain is needed.
const ROUTER_ARMV7_TARGET: &str = "armv7-unknown-linux-musleabihf";

// ── Probe helpers ───────────────────────────────────────────────────────

fn rustup_target_installed(triple: &str) -> bool {
    Command::new("rustup")
        .args(["target", "list", "--installed"])
        .output()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .any(|l| l.trim() == triple)
        })
        .unwrap_or(false)
}

fn router_target_installed() -> bool {
    rustup_target_installed(ROUTER_TARGET)
}

// ── Dependency checking ─────────────────────────────────────────────────────

struct DepStatus {
    name: &'static str,
    installed: bool,
    hint: &'static str,
}

fn check_deps() -> Vec<DepStatus> {
    let root = project_root();
    let mut deps = Vec::new();

    // Rust toolchain
    let rustc_ok = Command::new("rustc").arg("--version").output().is_ok();
    deps.push(DepStatus {
        name: "Rust (rustc)",
        installed: rustc_ok,
        hint: "Install: https://rustup.rs",
    });

    // cargo
    let cargo_ok = Command::new("cargo").arg("--version").output().is_ok();
    deps.push(DepStatus {
        name: "cargo",
        installed: cargo_ok,
        hint: "Part of Rust toolchain",
    });

    // Xcode CLI tools (macOS only)
    let xcode_ok = Command::new("xcrun")
        .args(["--find", "swift"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    deps.push(DepStatus {
        name: "Xcode CLI (swift)",
        installed: xcode_ok,
        hint: "Install: xcode-select --install",
    });

    // Android NDK
    let ndk_home = env::var("ANDROID_NDK_HOME").unwrap_or_default();
    let ndk_ok = if ndk_home.is_empty() {
        let default_ndk = PathBuf::from(env::var("HOME").unwrap_or_default())
            .join("Library/Android/sdk/ndk");
        if default_ndk.exists() {
            // Found NDK at default location
            true
        } else {
            false
        }
    } else {
        Path::new(&ndk_home).exists()
    };
    deps.push(DepStatus {
        name: "Android NDK",
        installed: ndk_ok,
        hint: "Set ANDROID_NDK_HOME or install via Android Studio SDK Manager",
    });

    // Android aarch64 target
    let android_target_ok = rustup_target_installed("aarch64-linux-android");
    deps.push(DepStatus {
        name: "Rust aarch64-linux-android",
        installed: android_target_ok,
        hint: "Install: rustup target add aarch64-linux-android",
    });

    // HarmonyOS target
    let ohos_target_ok = rustup_target_installed("aarch64-unknown-linux-ohos");
    deps.push(DepStatus {
        name: "Rust aarch64-unknown-linux-ohos",
        installed: ohos_target_ok,
        hint: "Install: rustup target add aarch64-unknown-linux-ohos",
    });

    // DevEco Studio (check for ohos clang)
    let deveco_ok = root
        .join(".cargo/config.toml")
        .exists()
        && fs::read_to_string(root.join(".cargo/config.toml"))
            .map(|c| c.contains("aarch64-unknown-linux-ohos-clang"))
            .unwrap_or(false);
    deps.push(DepStatus {
        name: "DevEco Studio / OHOS SDK",
        installed: deveco_ok,
        hint: "Install: https://developer.huawei.com/consumer/cn/deveco-studio/",
    });

    // Java (required by hap-sign-tool for HAP signing)
    let java_ok = Command::new("java")
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    deps.push(DepStatus {
        name: "Java (hap-sign-tool)",
        installed: java_ok,
        hint: "Install JDK 17+ (bundled with DevEco Studio)",
    });

    // Gradle (for Android APK)
    let gradlew = root.join("client/android/gradlew");
    let gradle_ok = gradlew.exists();
    deps.push(DepStatus {
        name: "Gradle (Android gradlew)",
        installed: gradle_ok,
        hint: "Already bundled in client/android/",
    });

    // sips (macOS icon generation)
    let sips_ok = Command::new("sips")
        .args(["--version"])
        .output()
        .is_ok();
    deps.push(DepStatus {
        name: "sips (icon generation)",
        installed: sips_ok,
        hint: "Built-in on macOS",
    });

    // Router targets: statically-linked musl builds.
    //
    // Linking uses `rust-lld` from the Rust toolchain and the dependency tree
    // is pure Rust (no ring, no C shims), so no C cross toolchain is needed.
    deps.push(DepStatus {
        name: "Rust aarch64-unknown-linux-musl",
        installed: router_target_installed(),
        hint: "Install: rustup target add aarch64-unknown-linux-musl",
    });

    // Legacy 32-bit ARM routers (armv7hf), also fully static musl builds.
    deps.push(DepStatus {
        name: "Rust armv7-unknown-linux-musleabihf",
        installed: rustup_target_installed(ROUTER_ARMV7_TARGET),
        hint: "Install: rustup target add armv7-unknown-linux-musleabihf",
    });

    deps
}

fn print_dep_table(deps: &[DepStatus]) {
    println!("{:<35} {:<12} {}", "Dependency", "Status", "Hint");
    println!("{}", "-".repeat(80));
    for dep in deps {
        let status = if dep.installed { "OK" } else { "MISSING" };
        println!("{:<35} {:<12} {}", dep.name, status, dep.hint);
    }
    println!();
}

// ── Build helpers ────────────────────────────────────────────────────────────

fn cargo_cmd() -> Command {
    Command::new("cargo")
}

fn run_cmd(cmd: &mut Command, label: &str) -> Result<()> {
    println!();
    println!("{:=<60}", format!("  {} ", label));
    let status = cmd
        .status()
        .with_context(|| format!("Failed to execute: {:?}", cmd))?;
    if !status.success() {
        bail!("{} failed with exit code {:?}", label, status.code());
    }
    Ok(())
}

fn is_available(target: &str) -> bool {
    let deps = check_deps();
    match target {
        "cli" => deps.iter().find(|d| d.name == "Rust (rustc)").unwrap().installed,
        "server" => deps.iter().find(|d| d.name == "Rust (rustc)").unwrap().installed,
        // The arm64 server is pure Rust (no C shims since ring was dropped),
        // so rust-lld alone suffices — no clang probe needed here.
        "server-arm64" => {
            let rustc = deps.iter().find(|d| d.name == "Rust (rustc)").unwrap().installed;
            let target = deps
                .iter()
                .find(|d| d.name == "Rust aarch64-unknown-linux-musl")
                .unwrap()
                .installed;
            rustc && target
        }
        "router" => {
            deps
                .iter()
                .find(|d| d.name == "Rust aarch64-unknown-linux-musl")
                .unwrap()
                .installed
        }
        // Pure-Rust dependency tree: rust-lld alone links the armv7 build.
        "router-armv7" => {
            let rustc = deps.iter().find(|d| d.name == "Rust (rustc)").unwrap().installed;
            let target = deps
                .iter()
                .find(|d| d.name == "Rust armv7-unknown-linux-musleabihf")
                .unwrap()
                .installed;
            rustc && target
        }
        "mac" => deps
            .iter()
            .find(|d| d.name == "Xcode CLI (swift)")
            .unwrap()
            .installed,
        "android" => {
            let ndk = deps.iter().find(|d| d.name == "Android NDK").unwrap().installed;
            let target = deps
                .iter()
                .find(|d| d.name == "Rust aarch64-linux-android")
                .unwrap()
                .installed;
            ndk && target
        }
        "harmony" => {
            let rust_target = deps
                .iter()
                .find(|d| d.name == "Rust aarch64-unknown-linux-ohos")
                .unwrap()
                .installed;
            let deveco = deps
                .iter()
                .find(|d| d.name == "DevEco Studio / OHOS SDK")
                .unwrap()
                .installed;
            let java = deps
                .iter()
                .find(|d| d.name == "Java (hap-sign-tool)")
                .unwrap()
                .installed;
            rust_target && deveco && java
        }
        _ => false,
    }
}

// ── Build targets ────────────────────────────────────────────────────────────

fn build_cli(release: bool) -> Result<()> {
    let root = project_root();
    let mut cmd = cargo_cmd();
    cmd.arg("build").arg("-p").arg("phantom-cli");
    if release {
        cmd.arg("--release");
    }
    cmd.current_dir(&root);
    run_cmd(&mut cmd, "Build phantom (CLI)")?;

    let profile = if release { "release" } else { "debug" };
    let bin_path = root.join("target").join(profile).join("phantom");
    println!("  Binary: {}", bin_path.display());
    Ok(())
}

fn build_server(release: bool) -> Result<()> {
    let root = project_root();
    let mut cmd = cargo_cmd();
    cmd.arg("build").arg("-p").arg("phantom-server");
    if release {
        cmd.arg("--release");
    }
    cmd.current_dir(&root);
    run_cmd(&mut cmd, "Build phantom-server")?;

    let profile = if release { "release" } else { "debug" };
    let bin_path = root.join("target").join(profile).join("phantom-server");
    println!("  Binary: {}", bin_path.display());
    Ok(())
}

/// Build the statically-linked router client (ASUS RT-AX86U Pro and friends).
///
/// Plain host cross-compile: `rust-lld` ships with the Rust toolchain and the
/// dependency tree is pure Rust (no ring, no C shims), so no musl-gcc, no
/// clang probe and no container are needed (see `.cargo/config.toml`).
fn build_router(release: bool) -> Result<()> {
    let root = project_root();

    if !router_target_installed() {
        bail!(
            "Router build prerequisite missing:\n  - rustup target add {}",
            ROUTER_TARGET
        );
    }

    let mut cmd = cargo_cmd();
    cmd.arg("build")
        .arg("-p")
        .arg("phantom-cli")
        .arg("--target")
        .arg(ROUTER_TARGET);
    if release {
        cmd.arg("--release");
    }
    // Symbols roughly halve the binary; JFFS space on routers is tight.
    cmd.env("CARGO_PROFILE_RELEASE_STRIP", "symbols");
    cmd.current_dir(&root);
    run_cmd(&mut cmd, "Build phantom (router, aarch64 musl static)")?;

    let profile = if release { "release" } else { "debug" };
    let bin_path = root
        .join("target")
        .join(ROUTER_TARGET)
        .join(profile)
        .join("phantom");
    if !bin_path.exists() {
        bail!("Router binary not found: {}", bin_path.display());
    }

    println!();
    println!("  Binary: {}", bin_path.display());
    if let Ok(meta) = fs::metadata(&bin_path) {
        println!("  Size:   {:.1} MiB", meta.len() as f64 / (1024.0 * 1024.0));
    }
    println!("  Deploy: bash deploy/router/install.sh <router-host> \"<phantom:// URI>\"");
    Ok(())
}

/// Build the statically-linked Linux ARM64 server (Ubuntu 24.04 LTS ARM,
/// Ampere / RK3588-class boxes). Shares the `aarch64-unknown-linux-musl`
/// triple with the router client, so the same rust-lld link setup applies;
/// musl static linking also keeps the binary runnable on older glibc hosts.
///
/// `features` are passed through to cargo verbatim — notably `io-uring`,
/// which enables the zero-copy io_uring runtime on ≥5.10 kernels.
fn build_server_arm64(release: bool, features: &[String]) -> Result<()> {
    let root = project_root();
    let mut cmd = cargo_cmd();
    cmd.arg("build")
        .arg("-p")
        .arg("phantom-server")
        .arg("--target")
        .arg(ROUTER_TARGET);
    if release {
        cmd.arg("--release");
    }
    if !features.is_empty() {
        cmd.arg("--features").arg(features.join(","));
    }
    // Symbols roughly halve the binary; flash/SD space on ARM boxes is tight.
    cmd.env("CARGO_PROFILE_RELEASE_STRIP", "symbols");
    cmd.current_dir(&root);
    run_cmd(&mut cmd, "Build phantom-server (linux arm64, musl static)")?;

    let profile = if release { "release" } else { "debug" };
    let bin_path = root
        .join("target")
        .join(ROUTER_TARGET)
        .join(profile)
        .join("phantom-server");
    if !bin_path.exists() {
        bail!("arm64 server binary not found: {}", bin_path.display());
    }

    println!();
    println!("  Binary: {}", bin_path.display());
    if let Ok(meta) = fs::metadata(&bin_path) {
        println!("  Size:   {:.1} MiB", meta.len() as f64 / (1024.0 * 1024.0));
    }
    println!("  Deploy: see deploy/README.md (systemd unit in deploy/phantom.service)");
    Ok(())
}

/// Build the statically-linked client for legacy 32-bit ARMv7 routers.
/// Same pure-Rust story as `server-arm64`: rust-lld links, no clang probe.
fn build_router_armv7(release: bool) -> Result<()> {
    let root = project_root();
    let mut cmd = cargo_cmd();
    cmd.arg("build")
        .arg("-p")
        .arg("phantom-cli")
        .arg("--target")
        .arg(ROUTER_ARMV7_TARGET);
    if release {
        cmd.arg("--release");
    }
    cmd.env("CARGO_PROFILE_RELEASE_STRIP", "symbols");
    cmd.current_dir(&root);
    run_cmd(&mut cmd, "Build phantom (router, armv7 musl static)")?;

    let profile = if release { "release" } else { "debug" };
    let bin_path = root
        .join("target")
        .join(ROUTER_ARMV7_TARGET)
        .join(profile)
        .join("phantom");
    if !bin_path.exists() {
        bail!("armv7 router binary not found: {}", bin_path.display());
    }

    println!();
    println!("  Binary: {}", bin_path.display());
    if let Ok(meta) = fs::metadata(&bin_path) {
        println!("  Size:   {:.1} MiB", meta.len() as f64 / (1024.0 * 1024.0));
    }
    println!("  Deploy: bash deploy/router/install.sh <router-host> \"<phantom:// URI>\"");
    Ok(())
}

fn build_mac(release: bool) -> Result<()> {
    let root = project_root();
    let script = root.join("scripts/build-mac.sh");
    if !script.exists() {
        bail!("scripts/build-mac.sh not found");
    }
    let mut cmd = Command::new("bash");
    cmd.arg(&script);
    if !release {
        cmd.arg("--debug");
    }
    cmd.current_dir(&root);
    run_cmd(&mut cmd, "Build macOS Phantom.app + DMG")
}

fn build_android(release: bool) -> Result<()> {
    let root = project_root();
    let script = root.join("scripts/build-android.sh");
    if !script.exists() {
        bail!("scripts/build-android.sh not found");
    }
    let mut cmd = Command::new("bash");
    cmd.arg(&script);
    if !release {
        cmd.arg("--debug");
    }
    cmd.current_dir(&root);
    run_cmd(&mut cmd, "Build Android .so + APK")
}

fn build_harmony(release: bool) -> Result<()> {
    let root = project_root();
    let harmony_dir = root.join("client/harmony");
    let target = "aarch64-unknown-linux-ohos";
    let profile = if release { "release" } else { "debug" };

    // ── Step 1: Build Rust .so ──
    let mut cmd = cargo_cmd();
    cmd.arg("build")
        .arg("-p")
        .arg("phantom-harmony")
        .arg("--target")
        .arg(target);
    if release {
        cmd.arg("--release");
    }
    cmd.current_dir(&root);
    run_cmd(&mut cmd, "Build phantom-harmony .so")?;

    // ── Step 2: Copy .so to entry/libs/arm64-v8a/ (hvigor native-lib pickup dir) ──
    let so_src = root
        .join("target")
        .join(target)
        .join(profile)
        .join("libphantom_harmony.so");
    if !so_src.exists() {
        bail!("Rust .so not found: {}. Build may have failed.", so_src.display());
    }
    let libs_dir = harmony_dir.join("entry/libs/arm64-v8a");
    fs::create_dir_all(&libs_dir)?;
    let so_dst = libs_dir.join("libphantom_harmony.so");
    fs::copy(&so_src, &so_dst)?;
    println!("  Copied .so -> {}", so_dst.display());

    // ── Step 3: hvigor assembleHap ──
    let deveco_sdk = env::var("DEVECO_SDK_HOME")
        .unwrap_or_else(|_| "/Applications/DevEco-Studio.app/Contents/sdk".to_string());
    let node_home = env::var("NODE_HOME")
        .unwrap_or_else(|_| "/Applications/DevEco-Studio.app/Contents/tools/node".to_string());
    let hvigorw = "/Applications/DevEco-Studio.app/Contents/tools/hvigor/bin/hvigorw";
    if !Path::new(hvigorw).exists() {
        bail!("hvigorw not found at {}. Install DevEco Studio NEXT.", hvigorw);
    }

    let build_mode = if release { "release" } else { "debug" };
    let mut cmd = Command::new("bash");
    cmd.arg(hvigorw)
        .arg("--mode")
        .arg("module")
        .arg("-p")
        .arg("product=default")
        .arg("-p")
        .arg(format!("buildMode={}", build_mode))
        .arg("--no-daemon")
        .arg("assembleHap")
        .env("DEVECO_SDK_HOME", &deveco_sdk)
        .env("NODE_HOME", &node_home)
        .current_dir(&harmony_dir);
    run_cmd(&mut cmd, "hvigor assembleHap")?;

    // ── Step 4: Sign HAP with hap-sign-tool.jar ──
    let hap_sign_tool = Path::new(&deveco_sdk)
        .join("default/openharmony/toolchains/lib/hap-sign-tool.jar");
    if !hap_sign_tool.exists() {
        bail!("hap-sign-tool.jar not found at {}. Check DEVECO_SDK_HOME.", hap_sign_tool.display());
    }

    let unsigned_hap = harmony_dir
        .join("entry/build/default/outputs/default/entry-default-unsigned.hap");
    if !unsigned_hap.exists() {
        bail!("Unsigned HAP not found: {}", unsigned_hap.display());
    }

    let signing_dir = harmony_dir.join("signing");
    let keystore = signing_dir.join("OpenHarmony.p12");
    let app_cert = signing_dir.join("OpenHarmonyAppCertChain.cer");
    let debug_profile = signing_dir.join("OpenHarmonyDebug.p7b");

    for (name, path) in [
        ("keystore", &keystore),
        ("app cert chain", &app_cert),
        ("debug profile", &debug_profile),
    ] {
        if !path.exists() {
            bail!("Signing file '{}' not found: {}", name, path.display());
        }
    }

    let signed_name = if release {
        "entry-default-signed.hap"
    } else {
        "entry-default-debug-signed.hap"
    };
    let signed_hap = harmony_dir.join(signed_name);

    let mut cmd = Command::new("java");
    cmd.arg("-jar")
        .arg(&hap_sign_tool)
        .arg("sign-app")
        .arg("-mode")
        .arg("localSign")
        .arg("-keyAlias")
        .arg("openharmony application release")
        .arg("-keyPwd")
        .arg("123456")
        .arg("-keystoreFile")
        .arg(&keystore)
        .arg("-keystorePwd")
        .arg("123456")
        .arg("-signAlg")
        .arg("SHA256withECDSA")
        .arg("-appCertFile")
        .arg(&app_cert)
        .arg("-profileFile")
        .arg(&debug_profile)
        .arg("-inFile")
        .arg(&unsigned_hap)
        .arg("-outFile")
        .arg(&signed_hap)
        .current_dir(&harmony_dir);
    run_cmd(&mut cmd, "Sign HAP (hap-sign-tool)")?;

    println!();
    println!("  Signed HAP: {}", signed_hap.display());
    println!("  Install:    hdc install {}", signed_hap.display());

    Ok(())
}

// ── Icons ────────────────────────────────────────────────────────────────────

fn generate_icons() -> Result<()> {
    let root = project_root();
    let src = root.join("appicon.png");
    if !src.exists() {
        bail!("Source icon not found: appicon.png");
    }

    println!("Generating icons from {} ...", src.display());

    // macOS Icon.iconset
    let iconset_dir = root.join("client/mac/.build/icon/Icon.iconset");
    fs::create_dir_all(&iconset_dir)?;

    let mac_sizes = [
        (16, "icon_16x16.png"),
        (32, "icon_16x16@2x.png"),
        (32, "icon_32x32.png"),
        (64, "icon_32x32@2x.png"),
        (128, "icon_128x128.png"),
        (256, "icon_128x128@2x.png"),
        (256, "icon_256x256.png"),
        (512, "icon_256x256@2x.png"),
        (512, "icon_512x512.png"),
        (1024, "icon_512x512@2x.png"),
    ];

    for (size, name) in &mac_sizes {
        let out = iconset_dir.join(name);
        let status = Command::new("sips")
            .args(["-z", &size.to_string(), &size.to_string()])
            .arg("-s")
            .arg("format")
            .arg("png")
            .arg(&src)
            .args(["--out", &out.to_string_lossy()])
            .status()?;
        if !status.success() {
            bail!("sips failed for {}", name);
        }
    }

    // Generate .icns
    let icns_path = root.join("client/mac/.build/icon/Icon.icns");
    let status = Command::new("iconutil")
        .args(["-c", "icns"])
        .arg(&iconset_dir)
        .args(["-o", &icns_path.to_string_lossy()])
        .status()?;
    if !status.success() {
        bail!("iconutil failed");
    }
    println!("  macOS icons generated (Icon.iconset + Icon.icns)");

    // Android adaptive icon foreground
    let android_res = root.join("client/android/app/src/main/res");
    let densities = [
        ("mdpi", 108),
        ("hdpi", 162),
        ("xhdpi", 216),
        ("xxhdpi", 324),
        ("xxxhdpi", 432),
    ];

    for (density, size) in &densities {
        let dir = android_res.join(format!("drawable-{}", density));
        fs::create_dir_all(&dir)?;
        let out = dir.join("ic_launcher_foreground.png");
        let status = Command::new("sips")
            .args(["-z", &size.to_string(), &size.to_string()])
            .arg("-s")
            .arg("format")
            .arg("png")
            .arg(&src)
            .args(["--out", &out.to_string_lossy()])
            .status()?;
        if !status.success() {
            bail!("sips failed for Android {} density", density);
        }
    }
    println!("  Android adaptive icon foreground generated (5 densities)");

    // HarmonyOS
    let harmony_icon = root.join("client/harmony/AppScope/resources/base/media/app_icon.png");
    let harmony_start =
        root.join("client/harmony/entry/src/main/resources/base/media/startIcon.png");

    for path in [&harmony_icon, &harmony_start] {
        let status = Command::new("sips")
            .args(["-z", "192", "192"])
            .arg("-s")
            .arg("format")
            .arg("png")
            .arg(&src)
            .args(["--out", &path.to_string_lossy()])
            .status()?;
        if !status.success() {
            bail!("sips failed for HarmonyOS icon");
        }
    }
    println!("  HarmonyOS icons generated (app_icon.png + startIcon.png)");

    Ok(())
}

// ── Clean ────────────────────────────────────────────────────────────────────

fn clean_all() -> Result<()> {
    let root = project_root();

    println!("Cleaning all build artifacts ...");

    // Rust target/
    let target = root.join("target");
    if target.exists() {
        println!("  Removing target/ ...");
        fs::remove_dir_all(&target).with_context(|| "Failed to remove target/")?;
    }

    // macOS .build/
    let mac_build = root.join("client/mac/.build");
    if mac_build.exists() {
        println!("  Removing client/mac/.build/ ...");
        fs::remove_dir_all(&mac_build).with_context(|| "Failed to remove client/mac/.build/")?;
    }

    // Android build/
    for dir in [
        root.join("client/android/build"),
        root.join("client/android/app/build"),
    ] {
        if dir.exists() {
            println!("  Removing {} ...", dir.display());
            fs::remove_dir_all(&dir)?;
        }
    }

    // Android jniLibs (built .so)
    let jni_libs = root.join("client/android/app/src/main/jniLibs");
    if jni_libs.exists() {
        println!("  Removing {} ...", jni_libs.display());
        fs::remove_dir_all(&jni_libs)?;
    }

    // HarmonyOS build/
    for dir in [
        root.join("client/harmony/build"),
        root.join("client/harmony/entry/build"),
        root.join("client/harmony/rust/target"),
    ] {
        if dir.exists() {
            println!("  Removing {} ...", dir.display());
            fs::remove_dir_all(&dir)?;
        }
    }

    // HarmonyOS entry libs (built .so; hvigor pickup dir is module-root libs/)
    let harmony_libs = root.join("client/harmony/entry/libs");
    if harmony_libs.exists() {
        println!("  Removing {} ...", harmony_libs.display());
        fs::remove_dir_all(&harmony_libs)?;
    }

    // Test targets
    for dir in [
        root.join("tests/target"),
        root.join("tests/bench/target"),
    ] {
        if dir.exists() {
            println!("  Removing {} ...", dir.display());
            fs::remove_dir_all(&dir)?;
        }
    }

    println!("  All clean!");
    Ok(())
}

// ── Main ─────────────────────────────────────────────────────────────────────

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Build { target, debug, features, .. } => {
            let release = !debug;
            let targets = if target.is_empty() || target.contains(&"all".to_string()) {
                vec!["server", "server-arm64", "cli", "router", "router-armv7", "mac", "android", "harmony"]
            } else {
                target.iter().map(|s| s.as_str()).collect()
            };
            if !features.is_empty() && !targets.iter().any(|t| *t == "server-arm64") {
                println!("NOTE: --features is currently only honoured by the server-arm64 target");
            }

            let deps = check_deps();
            print_dep_table(&deps);

            let mut built = 0;
            let mut skipped = 0;

            for t in &targets {
                if !is_available(t) {
                    println!("SKIP: {} — missing dependencies (see table above)", t);
                    skipped += 1;
                    continue;
                }
                match *t {
                    "cli" => build_cli(release)?,
                    "server" => build_server(release)?,
                    "server-arm64" => build_server_arm64(release, &features)?,
                    "router" => build_router(release)?,
                    "router-armv7" => build_router_armv7(release)?,
                    "mac" => build_mac(release)?,
                    "android" => build_android(release)?,
                    "harmony" => build_harmony(release)?,
                    other => bail!("Unknown target: {}. Valid: all, server, server-arm64, cli, router, router-armv7, mac, android, harmony", other),
                }
                built += 1;
            }

            println!();
            println!("{:=<60}", "  Build Summary  ");
            println!("  Built: {}, Skipped: {}", built, skipped);
        }
        Commands::CheckDeps => {
            let deps = check_deps();
            print_dep_table(&deps);

            // Auto-install what we can
            let missing: Vec<_> = deps.iter().filter(|d| !d.installed).collect();
            if missing.is_empty() {
                println!("All dependencies satisfied!");
            } else {
                println!("Missing dependencies detected. Attempting auto-install ...");
                for dep in &missing {
                    match dep.name {
                        "Rust aarch64-linux-android" => {
                            println!("  Installing {} ...", dep.name);
                            let status = Command::new("rustup")
                                .args(["target", "add", "aarch64-linux-android"])
                                .status()?;
                            if status.success() {
                                println!("    OK!");
                            } else {
                                println!("    FAILED — install manually: rustup target add aarch64-linux-android");
                            }
                        }
                        "Rust aarch64-unknown-linux-ohos" => {
                            println!("  Installing {} ...", dep.name);
                            let status = Command::new("rustup")
                                .args(["target", "add", "aarch64-unknown-linux-ohos"])
                                .status()?;
                            if status.success() {
                                println!("    OK!");
                            } else {
                                println!("    FAILED — install manually: rustup target add aarch64-unknown-linux-ohos");
                            }
                        }
                        "Rust aarch64-unknown-linux-musl" => {
                            println!("  Installing {} ...", dep.name);
                            let status = Command::new("rustup")
                                .args(["target", "add", ROUTER_TARGET])
                                .status()?;
                            if status.success() {
                                println!("    OK!");
                            } else {
                                println!(
                                    "    FAILED — install manually: rustup target add {}",
                                    ROUTER_TARGET
                                );
                            }
                        }
                        "Xcode CLI (swift)" => {
                            println!("  Cannot auto-install {}. Run: xcode-select --install", dep.name);
                        }
                        "Android NDK" => {
                            println!("  Cannot auto-install {}. Set ANDROID_NDK_HOME or install via Android Studio.", dep.name);
                        }
                        "DevEco Studio / OHOS SDK" => {
                            println!("  Cannot auto-install {}. Download from Huawei Developer.", dep.name);
                        }
                        _ => {
                            println!("  Cannot auto-install {}. {}", dep.name, dep.hint);
                        }
                    }
                }
            }
        }
        Commands::Icons => {
            generate_icons()?;
        }
        Commands::Clean => {
            clean_all()?;
        }
    }

    Ok(())
}
