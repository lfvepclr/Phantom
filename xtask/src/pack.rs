//! Server packaging, verification, deployment and speed-test orchestration.
//!
//! Design rules (see the deployment plan):
//!   * The server host never compiles anything — it only receives a tarball.
//!   * The host toolchain is not mutated: by default the binary is produced in
//!     a pinned container (deploy/Containerfile, Alpine 3.18 base); `--no-container`
//!     falls back to a rustup cross build for machines without a container engine.
//!   * Every bundle is verifiable offline (no blocked-in-China target needed)
//!     through tests/e2e/minihttpd.rs used as a controllable origin.

use anyhow::{Context, Result, bail};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// A build/deploy platform plus the Rust target triple it maps to.
#[derive(Debug, Clone, Copy)]
pub struct TargetSpec {
    pub platform: &'static str,
    /// Short architecture name used in bundle/file names.
    pub arch: &'static str,
    pub triple: &'static str,
    /// Docker/Podman `TARGETARCH` value.
    pub container_arch: &'static str,
}

impl TargetSpec {
    pub fn binary_path(&self, root: &Path) -> PathBuf {
        root.join("target")
            .join(self.triple)
            .join("release")
            .join("phantom")
    }
}

/// Map `--platform linux/amd64` (or a bare `amd64`) to a target spec.
pub fn resolve_platform(platform: &str) -> Result<TargetSpec> {
    let normalized = platform.trim().trim_start_matches("linux/");
    match normalized {
        "amd64" | "x86_64" | "x86-64" => Ok(TargetSpec {
            platform: "linux/amd64",
            arch: "amd64",
            triple: "x86_64-unknown-linux-musl",
            container_arch: "amd64",
        }),
        "arm64" | "aarch64" => Ok(TargetSpec {
            platform: "linux/arm64",
            arch: "arm64",
            triple: "aarch64-unknown-linux-musl",
            container_arch: "arm64",
        }),
        other => bail!(
            "unsupported platform '{}' (expected linux/amd64 or linux/arm64)",
            other
        ),
    }
}

/// Locate a container engine. `auto` prefers docker then podman; `none` always
/// returns None; an explicit name is validated and reported if missing.
pub fn detect_engine(requested: &str) -> Result<Option<String>> {
    fn have(engine: &str) -> bool {
        Command::new(engine)
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    match requested {
        "none" | "host" => Ok(None),
        "auto" => {
            for engine in ["docker", "podman"] {
                if have(engine) {
                    return Ok(Some(engine.to_string()));
                }
            }
            Ok(None)
        }
        other => {
            if have(other) {
                Ok(Some(other.to_string()))
            } else {
                bail!("container engine '{}' not found in PATH", other)
            }
        }
    }
}

fn run(cmd: &mut Command, label: &str) -> Result<()> {
    println!();
    println!("{:=<60}", format!("  {} ", label));
    let status = cmd
        .status()
        .with_context(|| format!("failed to execute: {:?}", cmd))?;
    if !status.success() {
        bail!("{} failed with exit code {:?}", label, status.code());
    }
    Ok(())
}

fn have(tool: &str) -> bool {
    Command::new(tool)
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Ensure `<triple>` is installed, downloading it from RsProxy when missing.
fn ensure_rust_target(triple: &str) -> Result<()> {
    let installed = Command::new("rustup")
        .args(["target", "list", "--installed"])
        .output()
        .ok()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .any(|l| l.trim() == triple)
        })
        .unwrap_or(false);

    if installed {
        return Ok(());
    }

    // rsproxy.cn mirrors the rustup distribution; static.rust-lang.org takes
    // ~12s per request from here and occasionally stalls.
    let mut cmd = Command::new("rustup");
    cmd.args(["target", "add", triple])
        .env(
            "RUSTUP_DIST_SERVER",
            env::var("RUSTUP_DIST_SERVER").unwrap_or_else(|_| "https://rsproxy.cn".to_string()),
        )
        .env(
            "RUSTUP_UPDATE_ROOT",
            env::var("RUSTUP_UPDATE_ROOT")
                .unwrap_or_else(|_| "https://rsproxy.cn/rustup".to_string()),
        );
    run(&mut cmd, &format!("rustup target add {}", triple))
}

/// Build the `phantom` CLI for `spec` on the host (rust-lld cross link).
pub fn build_on_host(root: &Path, spec: &TargetSpec) -> Result<PathBuf> {
    ensure_rust_target(spec.triple)?;

    let mut cmd = Command::new("cargo");
    cmd.arg("build")
        .arg("--release")
        .arg("-p")
        .arg("phantom-cli")
        .arg("--target")
        .arg(spec.triple)
        // Symbols roughly halve the binary; flash/SD/VM space is tight and the
        // shipped binary never needs a symbol table.
        .env("CARGO_PROFILE_RELEASE_STRIP", "symbols")
        .current_dir(root);
    run(
        &mut cmd,
        &format!("Build phantom ({} host cross)", spec.triple),
    )?;

    let bin = spec.binary_path(root);
    if !bin.exists() {
        bail!("binary not found after build: {}", bin.display());
    }
    Ok(bin)
}

/// Build inside the pinned container image and extract the binary.
pub fn build_in_container(
    root: &Path,
    spec: &TargetSpec,
    engine: &str,
    runtime_image: bool,
) -> Result<PathBuf> {
    let stage_dir = root.join("dist").join(format!("stage-{}", spec.arch));
    if stage_dir.exists() {
        fs::remove_dir_all(&stage_dir)
            .with_context(|| format!("failed to clear {}", stage_dir.display()))?;
    }
    fs::create_dir_all(&stage_dir)?;

    let mut cmd = Command::new(engine);
    cmd.arg("build")
        .arg("-f")
        .arg("deploy/Containerfile")
        .arg("--target")
        .arg("artifact")
        .arg("--build-arg")
        .arg(format!("TARGETARCH={}", spec.container_arch))
        .arg("-o")
        .arg(format!("type=local,dest={}", stage_dir.display()))
        .current_dir(root);
    run(
        &mut cmd,
        &format!(
            "Container build phantom ({}, engine={})",
            spec.platform, engine
        ),
    )?;

    let bin = stage_dir.join("phantom");
    if !bin.exists() {
        bail!(
            "container build produced no binary at {} (engine output layout?)",
            bin.display()
        );
    }

    if runtime_image {
        let tag = format!("phantom-server:{}", env!("CARGO_PKG_VERSION"));
        let mut cmd = Command::new(engine);
        cmd.arg("build")
            .arg("-f")
            .arg("deploy/Containerfile")
            .arg("--target")
            .arg("runtime")
            .arg("--build-arg")
            .arg(format!("TARGETARCH={}", spec.container_arch))
            .arg("-t")
            .arg(&tag)
            .current_dir(root);
        run(&mut cmd, &format!("Container image {} ({})", tag, engine))?;
    }

    Ok(bin)
}

/// Static-link sanity checks that work both on macOS (`file`) and Linux.
pub fn check_static(bin: &Path, spec: &TargetSpec) -> Result<()> {
    let out = Command::new("file").arg(bin).output();
    match out {
        Ok(o) => {
            let desc = String::from_utf8_lossy(&o.stdout).trim().to_string();
            println!("  file: {}", desc);
            if !desc.contains("ELF") {
                bail!("{} is not an ELF binary — wrong target?", bin.display());
            }
            if !desc.contains("statically linked") && !desc.contains("static-pie") {
                bail!(
                    "{} is not statically linked (expected a musl static build)",
                    bin.display()
                );
            }
            let expected_machine = match spec.arch {
                "amd64" => "x86-64",
                "arm64" => "aarch64",
                _ => "",
            };
            if !expected_machine.is_empty() && !desc.contains(expected_machine) {
                bail!(
                    "{} does not look like {} (expected '{}' in `file` output)",
                    bin.display(),
                    spec.platform,
                    expected_machine
                );
            }
        }
        Err(_) => println!("  (file(1) unavailable — skipping static-link check)"),
    }

    let size = fs::metadata(bin)?.len() as f64 / (1024.0 * 1024.0);
    println!("  size: {:.1} MiB", size);
    Ok(())
}

fn sha256_file(path: &Path) -> Result<String> {
    // macOS ships shasum; coreutils hosts ship sha256sum.
    let (tool, args): (&str, Vec<&str>) = if have("shasum") {
        ("shasum", vec!["-a", "256"])
    } else {
        ("sha256sum", vec![])
    };
    let out = Command::new(tool)
        .args(&args)
        .arg(path)
        .output()
        .with_context(|| format!("failed to run {}", tool))?;
    if !out.status.success() {
        bail!("{} failed for {}", tool, path.display());
    }
    let text = String::from_utf8_lossy(&out.stdout);
    text.split_whitespace()
        .next()
        .map(|s| s.to_string())
        .context("empty hash output")
}

/// Assemble `dist/phantom-server-<version>-linux-<arch>.tar.gz` from a binary.
pub fn assemble_bundle(root: &Path, spec: &TargetSpec, bin: &Path) -> Result<PathBuf> {
    let version = env!("CARGO_PKG_VERSION");
    let name = format!("phantom-server-{}-linux-{}", version, spec.arch);
    let pkg_dir = root.join("dist").join(&name);
    if pkg_dir.exists() {
        fs::remove_dir_all(&pkg_dir)?;
    }
    fs::create_dir_all(&pkg_dir)?;

    // 1. the binary
    fs::copy(bin, pkg_dir.join("phantom"))
        .with_context(|| format!("failed to copy {}", bin.display()))?;

    // 2. server-side installer assets (no compiler, no network)
    for asset in ["install.sh", "phantom.initd", "phantom.confd"] {
        let src = root.join("deploy/alpine").join(asset);
        fs::copy(&src, pkg_dir.join(asset))
            .with_context(|| format!("failed to copy {}", src.display()))?;
    }

    // 3. systemd unit for non-Alpine hosts
    let unit = root.join("deploy/phantom.service");
    if unit.exists() {
        fs::copy(&unit, pkg_dir.join("phantom.service"))?;
    }

    // 4. bundle README
    let readme = root.join("deploy/alpine/README.md");
    if readme.exists() {
        fs::copy(&readme, pkg_dir.join("README.md"))?;
    }

    // 5. checksums (over every shipped file except the checksum file itself)
    let mut lines = String::new();
    let mut entries: Vec<_> = fs::read_dir(&pkg_dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n != "SHA256SUMS")
        .collect();
    entries.sort();
    for entry in &entries {
        lines.push_str(&format!(
            "{}  {}\n",
            sha256_file(&pkg_dir.join(entry))?,
            entry
        ));
    }
    fs::write(pkg_dir.join("SHA256SUMS"), lines)?;

    // 6. tarball + detached checksum
    let tarball = root.join("dist").join(format!("{}.tar.gz", name));
    let _ = fs::remove_file(&tarball);
    let mut cmd = Command::new("tar");
    cmd.arg("czf")
        .arg(&tarball)
        .arg("-C")
        .arg(root.join("dist"))
        .arg(&name);
    run(&mut cmd, "tar czf bundle")?;

    let digest = sha256_file(&tarball)?;
    let sum_path = root.join("dist").join(format!("{}.tar.gz.sha256", name));
    fs::write(&sum_path, format!("{}  {}.tar.gz\n", digest, name))?;

    println!();
    println!("  Bundle:  {}", tarball.display());
    println!("  SHA256:  {}", digest);
    Ok(tarball)
}

/// `scripts/deploy-server.sh` wrapper: upload + install + report the URI.
/// Offline end-to-end verification: runs the packaged binary inside the
/// production Alpine release together with a controllable local origin
/// (scripts/verify-server.sh). No external/blocked target is involved.
pub fn verify(root: &Path, tarball: &Path, spec: &TargetSpec, engine: &str) -> Result<()> {
    let script = root.join("scripts/verify-server.sh");
    if !script.exists() {
        bail!("{} not found", script.display());
    }
    let pkg_dir = PathBuf::from(tarball.to_string_lossy().trim_end_matches(".tar.gz"));
    if !pkg_dir.exists() {
        bail!("package directory not found: {}", pkg_dir.display());
    }

    let mut cmd = Command::new("bash");
    cmd.arg(&script)
        .arg(&pkg_dir)
        .arg(spec.arch)
        .arg(engine)
        .current_dir(root);
    run(
        &mut cmd,
        "Offline end-to-end verification (production Alpine release)",
    )
}

pub fn deploy(
    root: &Path,
    tarball: &Path,
    host: &str,
    port: u16,
    proto: &str,
    public_host: &str,
) -> Result<String> {
    let script = root.join("scripts/deploy-server.sh");
    if !script.exists() {
        bail!("{} not found", script.display());
    }
    let mut cmd = Command::new("bash");
    cmd.arg(&script)
        .arg(tarball)
        .arg(host)
        .arg(port.to_string())
        .arg(proto)
        .arg(public_host)
        .current_dir(root);
    run(&mut cmd, &format!("Deploy to {}", host))?;

    // Echo the URI so the operator can paste it straight into the client.
    let out = Command::new("ssh")
        .arg("-o")
        .arg("BatchMode=yes")
        .arg(host)
        .arg("sed -n 's|^#[[:space:]]*\\(phantom://.*\\)$|\\1|p' /var/lib/phantom/server.toml | head -1")
        .output()
        .context("failed to read the server URI over ssh")?;
    let uri = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if uri.is_empty() {
        bail!("deployed, but no phantom:// URI was found on {}", host);
    }
    println!();
    println!("  URI: {}", uri);
    Ok(uri)
}

/// `scripts/speedtest.sh` wrapper.
pub fn speedtest(
    root: &Path,
    uri: Option<&str>,
    rounds: u32,
    origin: &str,
    check_unblock: bool,
    socks_port: u16,
    loopback: bool,
    vps_host: Option<&str>,
) -> Result<()> {
    let script = root.join("scripts/speedtest.sh");
    if !script.exists() {
        bail!("{} not found", script.display());
    }
    let mut cmd = Command::new("bash");
    cmd.arg(&script)
        .arg("--rounds")
        .arg(rounds.to_string())
        .arg("--origin")
        .arg(origin)
        .arg("--socks-port")
        .arg(socks_port.to_string())
        .current_dir(root);
    match uri {
        Some(u) => {
            cmd.arg("--uri").arg(u);
        }
        None => {
            cmd.arg("--loopback");
        }
    }
    if check_unblock {
        cmd.arg("--check-unblock");
    }
    if loopback {
        cmd.arg("--loopback");
    }
    if let Some(host) = vps_host {
        cmd.arg("--vps-host").arg(host);
    }
    run(&mut cmd, "Speed test")
}
