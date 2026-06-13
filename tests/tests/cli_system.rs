use std::fs;
use std::net::{Ipv4Addr, SocketAddr, TcpListener};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::Duration;

use phantom_core::{parse_phantom_uri, KeyPair};

/// Resolve the phantom CLI binary path.
/// Looks in target/debug/phantom relative to the workspace root.
fn phantom_bin() -> String {
    // CARGO_MANIFEST_DIR points to phantom-e2e/, go up one level to workspace.
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR")
        .unwrap_or_else(|_| ".".to_string());
    let workspace = Path::new(&manifest_dir).parent().unwrap();
    let bin_path = workspace.join("target").join("debug").join("phantom");
    bin_path.to_string_lossy().to_string()
}

/// Pick an OS-assigned free port by binding to 0 and reading the port back.
/// The std listener is dropped immediately; the port may briefly remain in
/// TIME_WAIT, so callers should sleep or rebind with SO_REUSEADDR.
fn pick_free_port() -> u16 {
    let l = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    l.local_addr().unwrap().port()
}

/// Spawn `phantom` with the given args, running it inside `cwd`. Stdout and
/// stderr are captured.
fn spawn_phantom_with_cwd(args: &[&str], cwd: &Path) -> Child {
    Command::new(phantom_bin())
        .current_dir(cwd)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn phantom")
}

/// Wait for a file to exist, polling up to `timeout` total. Returns true if
/// the file appears within the budget.
fn wait_for_file(path: &Path, timeout: Duration) -> bool {
    let start = std::time::Instant::now();
    while start.elapsed() < timeout {
        if path.exists() {
            return true;
        }
        thread::sleep(Duration::from_millis(50));
    }
    false
}

/// `phantom --version` should print something containing "phantom".
#[test]
fn cli_version() {
    let bin = phantom_bin();
    let result = Command::new(&bin)
        .arg("--version")
        .output()
        .expect("Failed to execute phantom binary");

    let stdout = String::from_utf8_lossy(&result.stdout);
    let stderr = String::from_utf8_lossy(&result.stderr);

    assert!(
        result.status.success(),
        "phantom --version exited with {:?}\nstdout: {}\nstderr: {}",
        result.status.code(),
        stdout,
        stderr,
    );

    let combined = format!("{}{}", stdout, stderr);
    assert!(
        combined.to_lowercase().contains("phantom"),
        "Expected 'phantom' in version output, got: {}",
        combined,
    );
}

/// `phantom keygen` must be removed; clap should reject the subcommand.
#[test]
fn cli_keygen_subcommand_removed() {
    let bin = phantom_bin();
    let result = Command::new(&bin)
        .args(["keygen"])
        .output()
        .expect("Failed to execute phantom binary");

    let stdout = String::from_utf8_lossy(&result.stdout);
    let stderr = String::from_utf8_lossy(&result.stderr);

    assert!(
        !result.status.success(),
        "`phantom keygen` should fail after the bootstrap refactor.\nstdout: {}\nstderr: {}",
        stdout,
        stderr,
    );

    let combined = format!("{}{}", stdout, stderr);
    assert!(
        combined.contains("unrecognized subcommand")
            || combined.contains("unrecognized")
            || combined.contains("invalid subcommand"),
        "Expected clap 'unrecognized subcommand' message, got: {}",
        combined,
    );
}

/// `phantom server` (no flags) should:
/// - generate ./server.key with mode 0600
/// - write ./server.toml with a `phantom://` URI quick-link comment
/// - the URI's public key must match server.key
/// - server.config (legacy file) must NOT be created
#[test]
fn cli_server_auto_generates_key_and_uri() {
    // Use a workspace temp dir to avoid /tmp permission issues in sandboxes.
    let dir = workspace_tmp("phantom_cli_auto");
    fs::create_dir_all(&dir).unwrap();
    // Use a non-default port so we never conflict with other tests / 443.
    let port = pick_free_port() + 1000;
    let port_arg = port.to_string();

    let mut child = spawn_phantom_with_cwd(
        &["server", "--port", &port_arg, "--public-host", "127.0.0.1"],
        &dir,
    );

    // Wait up to 5s for server.toml to appear.
    let toml_path = dir.join("server.toml");
    assert!(
        wait_for_file(&toml_path, Duration::from_secs(5)),
        "server.toml was not created within 5s",
    );

    // Stop the server.
    let _ = child.kill();
    let _ = child.wait();

    // Verify key file with 0600 perms.
    let key_path = dir.join("server.key");
    assert!(key_path.exists(), "server.key missing at {}", key_path.display());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(&key_path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "expected mode 0600, got {:o}", mode);
    }

    // Legacy server.config must NOT exist after the refactor.
    let legacy_config = dir.join("server.config");
    assert!(
        !legacy_config.exists(),
        "legacy server.config should not be created, but found at {}",
        legacy_config.display()
    );

    // Verify server.toml shape.
    let toml_body = fs::read_to_string(&toml_path).unwrap();
    assert!(
        toml_body.contains("bind = \"0.0.0.0:") && toml_body.contains(&port.to_string()),
        "server.toml must reflect the chosen bind, got:\n{}",
        toml_body,
    );
    assert!(toml_body.contains("cipher = \""));
    assert!(toml_body.contains("protocol = \""));
    assert!(
        toml_body.contains("[[allowed_clients]]") || toml_body.contains("OPEN mode"),
        "server.toml must mention whitelist section, got:\n{}",
        toml_body,
    );

    // Extract the URI quick-link comment.
    let uri = extract_uri_from_server_toml(&toml_path);
    assert!(uri.starts_with("phantom://"), "URI does not start with phantom://: {}", uri);

    // Parse and verify the URI's public key matches the local key file.
    let entry = parse_phantom_uri(&uri).expect("server.toml URI is invalid");
    let loaded = KeyPair::load_secret_from_file(key_path.to_str().unwrap())
        .expect("failed to load server.key");
    assert_eq!(
        loaded.public_key_base64(),
        entry.public_key,
        "URI public key does not match server.key",
    );

    // URI's port must match the requested port (no fallback needed).
    let addr: SocketAddr = entry.address.parse().expect("URI has invalid address");
    assert_eq!(addr.port(), port);
    assert_eq!(addr.ip().to_string(), "127.0.0.1");

    let _ = fs::remove_dir_all(&dir);
}

/// When the requested port is already in use, the server should fall back to
/// the next port automatically and write the actual port to server.toml's
/// bind/URI quick-link.
///
/// NOTE: This test is tricky because tokio's TcpListener sets SO_REUSEADDR
/// by default, which would let it bind on top of the same port. We work
/// around this by spawning a child process that holds the port via `nc` (or
/// python's socket if nc is unavailable). If neither tool is available, the
/// test is silently skipped.
#[test]
fn cli_server_auto_port_fallback() {
    let dir = workspace_tmp("phantom_cli_port_fallback");
    fs::create_dir_all(&dir).unwrap();

    // Pick a free port.
    let occupied_port = pick_free_port();
    // Let the OS release the port from our probing bind.
    thread::sleep(Duration::from_millis(100));

    // Spawn a long-lived child that holds the port open. We try `nc -l`
    // first; if that is not on PATH, fall back to a python one-liner.
    let occupier = spawn_port_holder(occupied_port);
    if occupier.is_none() {
        eprintln!("skipped: no `nc` or `python3` available to occupy the port");
        let _ = fs::remove_dir_all(&dir);
        return;
    }
    let _occupier = occupier.unwrap();
    // Give the holder a moment to bind.
    thread::sleep(Duration::from_millis(200));

    let port_str = occupied_port.to_string();
    let mut child = spawn_phantom_with_cwd(
        &["server", "--port", &port_str, "--public-host", "127.0.0.1"],
        &dir,
    );

    let toml_path = dir.join("server.toml");
    assert!(
        wait_for_file(&toml_path, Duration::from_secs(5)),
        "server.toml was not created within 5s",
    );

    let _ = child.kill();
    let _ = child.wait();

    let uri = extract_uri_from_server_toml(&toml_path);
    let entry = parse_phantom_uri(&uri).unwrap();
    let addr: SocketAddr = entry.address.parse().unwrap();
    assert!(
        addr.port() > occupied_port,
        "expected fallback to a higher port ({}), got {}",
        occupied_port,
        addr.port(),
    );

    let _ = fs::remove_dir_all(&dir);
}

/// Spawn a child process that holds `port` open in listen mode. Tries `nc`
/// first, then `python3 -c "import socket; ..."`. Returns `None` if neither
/// tool is available.
fn spawn_port_holder(port: u16) -> Option<Child> {
    // Try `nc -l <port>` (BSD nc on macOS supports `-l`).
    if let Ok(child) = Command::new("nc")
        .args(["-l", &port.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        return Some(child);
    }
    // Fall back to python3.
    let code = format!(
        "import socket; s=socket.socket(); s.bind(('127.0.0.1', {port})); s.listen(); \
         import time; time.sleep(60)"
    );
    if let Ok(child) = Command::new("python3")
        .args(["-c", &code])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        return Some(child);
    }
    None
}

/// A second `phantom server` invocation in the same dir must reuse the
/// existing key (its public key must not change).
#[test]
fn cli_server_auto_reuses_existing_key() {
    let dir = workspace_tmp("phantom_cli_reuse");
    fs::create_dir_all(&dir).unwrap();

    // First run: generate.
    let port_a = pick_free_port() + 2000;
    let port_a_str = port_a.to_string();
    let mut first = spawn_phantom_with_cwd(
        &["server", "--port", &port_a_str, "--public-host", "127.0.0.1"],
        &dir,
    );
    let toml_path = dir.join("server.toml");
    assert!(wait_for_file(&toml_path, Duration::from_secs(5)));
    let _ = first.kill();
    let _ = first.wait();

    let key_path = dir.join("server.key");
    let first_pub = KeyPair::load_secret_from_file(key_path.to_str().unwrap())
        .unwrap()
        .public_key_base64();

    // Give the OS a moment to release the port from the first run.
    thread::sleep(Duration::from_millis(200));

    // Second run: reuse.
    let port_b = pick_free_port() + 3000;
    let port_b_str = port_b.to_string();
    let mut second = spawn_phantom_with_cwd(
        &["server", "--port", &port_b_str, "--public-host", "127.0.0.1"],
        &dir,
    );
    assert!(wait_for_file(&toml_path, Duration::from_secs(5)));
    let _ = second.kill();
    let _ = second.wait();

    let second_pub = KeyPair::load_secret_from_file(key_path.to_str().unwrap())
        .unwrap()
        .public_key_base64();

    assert_eq!(
        first_pub, second_pub,
        "second invocation must reuse the existing key",
    );

    let _ = fs::remove_dir_all(&dir);
}

/// Extract the quick-link `phantom://` URI emitted by bootstrap as a
/// comment inside the generated `server.toml`. The URI lives on the line
/// prefixed with `#   ` under the "Quick link URI" header.
fn extract_uri_from_server_toml(path: &Path) -> String {
    let body = fs::read_to_string(path).expect("failed to read server.toml");
    for line in body.lines() {
        let stripped = line.trim_start_matches('#').trim_start();
        if stripped.starts_with("phantom://") {
            return stripped.to_string();
        }
    }
    panic!(
        "no phantom:// URI comment found in {}. file content:\n{}",
        path.display(),
        body
    );
}

/// Helper: produce a unique temp directory inside the workspace's target/
/// folder. Using `std::env::temp_dir()` is avoided because the test sandbox
/// may refuse writes to the system temp dir.
fn workspace_tmp(prefix: &str) -> std::path::PathBuf {
    let workspace = Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap())
        .parent()
        .unwrap()
        .to_path_buf();
    let dir = workspace
        .join("target")
        .join("tmp")
        .join(format!("{}_{}", prefix, std::process::id()));
    let _ = fs::create_dir_all(dir.parent().unwrap());
    dir
}
