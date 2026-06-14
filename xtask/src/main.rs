//! Phantom xtask — unified build orchestrator.
//!
//! Usage:
//!   cargo xtask build [all|server|cli|mac|android|harmony] [--release|--debug]
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
        /// Target(s) to build: all, server, cli, mac, android, harmony
        target: Vec<String>,
        /// Build in release mode (default)
        #[arg(long, default_value_t = true)]
        release: bool,
        /// Build in debug mode
        #[arg(long)]
        debug: bool,
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
    let android_target_ok = Command::new("rustup")
        .args(["target", "list", "--installed"])
        .output()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .any(|l| l.trim() == "aarch64-linux-android")
        })
        .unwrap_or(false);
    deps.push(DepStatus {
        name: "Rust aarch64-linux-android",
        installed: android_target_ok,
        hint: "Install: rustup target add aarch64-linux-android",
    });

    // HarmonyOS target
    let ohos_target_ok = Command::new("rustup")
        .args(["target", "list", "--installed"])
        .output()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .any(|l| l.trim() == "aarch64-unknown-linux-ohos")
        })
        .unwrap_or(false);
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
        "harmony" => deps
            .iter()
            .find(|d| d.name == "Rust aarch64-unknown-linux-ohos")
            .unwrap()
            .installed,
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
    let script = root.join("scripts/build-harmony.sh");
    if !script.exists() {
        bail!("scripts/build-harmony.sh not found");
    }
    let mut cmd = Command::new("bash");
    cmd.arg(&script);
    if !release {
        cmd.env("BUILD_MODE", "debug");
    }
    cmd.current_dir(&root);
    run_cmd(&mut cmd, "Build HarmonyOS .so")
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

    // HarmonyOS entry libs (built .so)
    let harmony_libs = root.join("client/harmony/entry/src/main/libs");
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
        Commands::Build { target, debug, .. } => {
            let release = !debug;
            let targets = if target.is_empty() || target.contains(&"all".to_string()) {
                vec!["server", "cli", "mac", "android", "harmony"]
            } else {
                target.iter().map(|s| s.as_str()).collect()
            };

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
                    "mac" => build_mac(release)?,
                    "android" => build_android(release)?,
                    "harmony" => build_harmony(release)?,
                    other => bail!("Unknown target: {}. Valid: all, server, cli, mac, android, harmony", other),
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
