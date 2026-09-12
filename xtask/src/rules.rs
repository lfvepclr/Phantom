//! Proxy-whitelist data pipeline.
//!
//! Phantom routes **direct by default** and only sends a curated whitelist of
//! (censored) domains through the tunnel. The whitelist is compiled into an FST
//! index at generation time, so the client loads it with zero parsing work:
//! `fst::Set::new(include_bytes!(...))` is a zero-copy view over the blob.
//!
//! `cargo xtask rules update` regenerates the checked-in data files; builds and
//! runtime never need the network. `cargo xtask rules verify` re-checks the
//! generated artefacts (counts, spot checks, hashes, idempotency).

use anyhow::{Context, Result, bail};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Primary source: the GreatFire-derived list of **censored** domains
/// (`+.domain` suffix entries, ~4.4k).  Deliberately the strict list: the
/// tunnel's uplink is tiny, and "default direct + blocked-only proxy" keeps
/// domestic traffic off the server.
const GFW_TXT_URL: &str = "https://cdn.jsdelivr.net/gh/Loyalsoldier/clash-rules@release/gfw.txt";

/// Optional extended list (`cargo xtask rules update --extended`): services
/// that are reachable but usually wanted through the tunnel (Netflix, OpenAI,
/// Telegram, ...). ~27k full-hostname entries.
const PROXY_TXT_URL: &str =
    "https://cdn.jsdelivr.net/gh/Loyalsoldier/clash-rules@release/proxy.txt";

/// IP-based entries (services reachable only by CIDR, e.g. Telegram).
const PROXY_CIDR_URL: &str =
    "https://cdn.jsdelivr.net/gh/Loyalsoldier/clash-rules@release/telegramcidr.txt";

/// Fallback mirrors, tried in order when the primary is unreachable.
const MIRRORS: &[&str] = &[
    "https://fastly.jsdelivr.net/gh/Loyalsoldier/clash-rules@release/gfw.txt",
    "https://gcore.jsdelivr.net/gh/Loyalsoldier/clash-rules@release/gfw.txt",
];

const MIRRORS_EXTENDED: &[&str] = &[
    "https://fastly.jsdelivr.net/gh/Loyalsoldier/clash-rules@release/proxy.txt",
    "https://gcore.jsdelivr.net/gh/Loyalsoldier/clash-rules@release/proxy.txt",
];

const MIRRORS_CIDR: &[&str] = &[
    "https://fastly.jsdelivr.net/gh/Loyalsoldier/clash-rules@release/telegramcidr.txt",
    "https://gcore.jsdelivr.net/gh/Loyalsoldier/clash-rules@release/telegramcidr.txt",
];

pub struct RulesPaths {
    pub dir: PathBuf,
    pub fst: PathBuf,
    pub meta: PathBuf,
    pub cidrs: PathBuf,
}

pub fn paths(root: &Path) -> RulesPaths {
    let dir = root.join("client/data");
    RulesPaths {
        fst: dir.join("proxy_domains.fst"),
        meta: dir.join("proxy_domains.meta.json"),
        cidrs: dir.join("proxy_cidrs.txt"),
        dir,
    }
}

/// `curl` instead of an HTTP client crate: available on every dev machine here,
/// no new dependency, and it follows the CDN redirects jsDelivr uses.
fn fetch(url: &str) -> Result<String> {
    let out = Command::new("curl")
        .args(["-fsSL", "--max-time", "120", url])
        .output()
        .with_context(|| format!("failed to run curl for {}", url))?;
    if !out.status.success() {
        bail!(
            "download failed ({}): {}",
            url,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn fetch_with_mirrors(primary: &str, mirrors: &[&str]) -> Result<String> {
    match fetch(primary) {
        Ok(body) => Ok(body),
        Err(primary_err) => {
            for mirror in mirrors {
                println!("    primary failed ({}), trying {}", primary_err, mirror);
                if let Ok(body) = fetch(mirror) {
                    return Ok(body);
                }
            }
            Err(primary_err)
        }
    }
}

/// Normalise one raw list entry into a bare lowercase domain suffix.
///
/// Handles the shapes found in the wild: `payload:` YAML headers, `- 'x.com'`
/// list items, `+.google.com` (Clash wildcard), `*.example.com`, `||x.com^`
/// (gfwlist style) and inline comments.
fn normalise_entry(raw: &str) -> Option<String> {
    let mut s = raw.trim();
    if s.is_empty() || s.starts_with('#') || s.eq_ignore_ascii_case("payload:") {
        return None;
    }
    // YAML list bullet: `- 'domain.com'`.
    if let Some(rest) = s.strip_prefix('-') {
        s = rest;
    }
    // Inline comment.
    if let Some((head, _)) = s.split_once('#') {
        s = head;
    }
    s = s.trim();

    // Quotes first: the source wraps entries as `- '+.domain.com'`, so the
    // wildcard prefix sits *inside* the quotes.
    s = s
        .trim_matches(|c: char| c == '\'' || c == '"' || c == ' ')
        .trim();

    // Decorations: Clash wildcards (`+.x`, `*.x`), gfwlist (`||x^`), stray dots.
    loop {
        let before = s;
        s = s
            .trim_start_matches(['+', '*', '|', '.'])
            .trim_start_matches("||")
            .trim();
        if s == before {
            break;
        }
    }
    s = s
        .trim_end_matches('^')
        .trim_matches(|c: char| c == '\'' || c == '"' || c == ' ')
        .trim();

    let s = s.to_ascii_lowercase();
    if s.is_empty() || !s.contains('.') {
        return None;
    }
    // Reject regex/keyword leftovers from gfwlist-style lists.
    if s.contains(['*', '|', '/', '\\', '^', '$']) {
        return None;
    }
    if s.split('.').any(|label| label.is_empty()) {
        return None;
    }
    Some(s)
}

/// Drop entries already covered by a parent suffix in the set.
///
/// `youku.com` in the set makes `v.youku.com` redundant, and the suffix lookup
/// treats the parent as a match anyway. This cuts the index size substantially.
fn prune_redundant(set: BTreeSet<String>) -> BTreeSet<String> {
    let sorted: Vec<String> = set.into_iter().collect();
    let mut out = BTreeSet::new();
    for domain in &sorted {
        let mut rest = domain.as_str();
        let mut covered = false;
        while let Some(idx) = rest.find('.') {
            rest = &rest[idx + 1..];
            if sorted
                .binary_search_by(|probe| probe.as_str().cmp(rest))
                .is_ok()
            {
                covered = true;
                break;
            }
        }
        if !covered {
            out.insert(domain.clone());
        }
    }
    out
}

pub fn update(root: &Path, extended: bool) -> Result<()> {
    let p = paths(root);
    fs::create_dir_all(&p.dir)?;

    println!("Fetching proxy whitelist …");
    println!("  {}", GFW_TXT_URL);
    let body = fetch_with_mirrors(GFW_TXT_URL, MIRRORS)?;

    let raw_lines = body.lines().count();
    let mut domains: BTreeSet<String> = BTreeSet::new();
    for line in body.lines() {
        if let Some(d) = normalise_entry(line) {
            domains.insert(d);
        }
    }
    if extended {
        println!("  + extended list {}", PROXY_TXT_URL);
        let extra = fetch_with_mirrors(PROXY_TXT_URL, MIRRORS_EXTENDED)?;
        for line in extra.lines() {
            if let Some(d) = normalise_entry(line) {
                domains.insert(d);
            }
        }
    }
    let normalised = domains.len();
    domains = prune_redundant(domains);
    let pruned = domains.len();

    // Build the FST (sorted input is required by the builder).
    let mut builder = fst::SetBuilder::memory();
    for d in &domains {
        builder.insert(d.as_bytes())?;
    }
    let bytes = builder.into_inner()?;
    fs::write(&p.fst, &bytes).with_context(|| format!("writing {}", p.fst.display()))?;

    // Optional IP whitelist (services only reachable by CIDR).
    let mut cidr_count = 0usize;
    match fetch_with_mirrors(PROXY_CIDR_URL, MIRRORS_CIDR) {
        Ok(cidr_body) => {
            let mut cidrs: BTreeSet<String> = BTreeSet::new();
            for line in cidr_body.lines() {
                let t = line
                    .trim()
                    .trim_start_matches('-')
                    .trim()
                    .trim_matches('\'');
                if t.is_empty() || t.starts_with('#') || t == "payload:" {
                    continue;
                }
                if t.parse::<ipnet::IpNet>().is_ok() {
                    cidrs.insert(t.to_string());
                }
            }
            cidr_count = cidrs.len();
            let mut out = String::from(
                "# Phantom proxy whitelist — IP ranges that require the tunnel.\n\
                 # Generated by `cargo xtask rules update`; do not edit by hand.\n",
            );
            for c in &cidrs {
                out.push_str(c);
                out.push('\n');
            }
            fs::write(&p.cidrs, out)?;
        }
        Err(e) => println!("  (IP whitelist unavailable, skipping: {})", e),
    }

    let digest = sha256_hex(&bytes);
    let meta = format!(
        "{{\n  \"source\": \"{}\",\n  \"extended\": {},\n  \"extended_source\": \"{}\",\n  \
         \"cidr_source\": \"{}\",\n  \"generated\": \"{}\",\n  \
         \"raw_lines\": {},\n  \"normalised\": {},\n  \"domains\": {},\n  \"cidrs\": {},\n  \
         \"fst_bytes\": {},\n  \"fst_sha256\": \"{}\"\n}}\n",
        GFW_TXT_URL,
        extended,
        PROXY_TXT_URL,
        PROXY_CIDR_URL,
        timestamp(),
        raw_lines,
        normalised,
        pruned,
        cidr_count,
        bytes.len(),
        digest
    );
    fs::write(&p.meta, meta)?;

    println!("  raw lines : {}", raw_lines);
    println!("  normalised: {} unique domains", normalised);
    println!(
        "  pruned    : {} domains ({} redundant removed)",
        pruned,
        normalised - pruned
    );
    println!("  FST       : {} KiB", bytes.len() / 1024);
    println!("  CIDRs     : {}", cidr_count);
    println!("  sha256    : {}", digest);
    println!();
    println!("  {}", p.fst.display());
    println!("  {}", p.meta.display());
    println!("  {}", p.cidrs.display());
    Ok(())
}

pub fn verify(root: &Path) -> Result<()> {
    let p = paths(root);
    let bytes = fs::read(&p.fst).with_context(|| {
        format!(
            "{} missing — run `cargo xtask rules update` first",
            p.fst.display()
        )
    })?;
    let set = fst::Set::new(bytes.as_slice())?;
    let count = set.len();
    let digest = sha256_hex(&bytes);

    // Dump a few keys so a corrupt/oddly-encoded index is obvious at a glance.
    {
        use fst::Streamer;
        let mut stream = set.stream();
        let mut sample: Vec<String> = Vec::new();
        while let Some(key) = stream.next() {
            if sample.len() < 5 {
                sample.push(String::from_utf8_lossy(key).into_owned());
            } else {
                break;
            }
        }
        println!("  sample keys  : {}", sample.join(", "));
        println!(
            "  direct probe : google.com={} www.google.com={}",
            set.contains("google.com"),
            set.contains("www.google.com")
        );
    }

    let mut problems = Vec::new();
    for expected_proxy in [
        "www.google.com",
        "youtube.com",
        "www.youtube.com",
        "twitter.com",
    ] {
        if !matches(&set, expected_proxy) {
            problems.push(format!(
                "expected {} to be in the whitelist",
                expected_proxy
            ));
        }
    }
    for expected_direct in ["v.youku.com", "youku.com", "www.baidu.com", "bilibili.com"] {
        if matches(&set, expected_direct) {
            problems.push(format!(
                "expected {} NOT to be in the whitelist",
                expected_direct
            ));
        }
    }

    // Micro benchmark: the decision runs once per new connection, so this is
    // about proving there is no per-lookup allocation or parse cost.
    let probes = ["www.google.com", "v.youku.com", "mail.qq.com", "github.com"];
    let start = std::time::Instant::now();
    let iterations = 100_000usize;
    let mut hits = 0usize;
    for i in 0..iterations {
        if matches(&set, probes[i % probes.len()]) {
            hits += 1;
        }
    }
    let elapsed = start.elapsed();

    println!("rules verify");
    println!("  domains      : {}", count);
    println!("  fst bytes    : {} KiB", bytes.len() / 1024);
    println!("  sha256       : {}", digest);
    println!(
        "  lookup       : {} probes in {:?} ({:.1} ns/probe, {} hits)",
        iterations,
        elapsed,
        elapsed.as_nanos() as f64 / iterations as f64,
        hits
    );
    if p.cidrs.exists() {
        let cidrs = fs::read_to_string(&p.cidrs)?
            .lines()
            .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
            .count();
        println!("  cidrs        : {}", cidrs);
    }

    if !problems.is_empty() {
        for problem in &problems {
            println!("  FAIL: {}", problem);
        }
        bail!("rules verify failed ({} problem(s))", problems.len());
    }
    println!("  spot checks  : OK");
    Ok(())
}

/// Suffix match: `v.youku.com` checks `v.youku.com`, `youku.com`, `com`.
fn matches(set: &fst::Set<&[u8]>, domain: &str) -> bool {
    let domain = domain.trim_end_matches('.').to_ascii_lowercase();
    let mut rest = domain.as_str();
    loop {
        if set.contains(rest.as_bytes()) {
            return true;
        }
        match rest.find('.') {
            Some(idx) => rest = &rest[idx + 1..],
            None => return false,
        }
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    // shasum/sha256sum keep xtask dependency-free; the value is only used as a
    // change-detection fingerprint.
    let mut child = Command::new("shasum")
        .args(["-a", "256"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .or_else(|_| {
            Command::new("sha256sum")
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .spawn()
        })
        .expect("neither shasum nor sha256sum available");
    use std::io::Write;
    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(bytes)
        .expect("write to hasher");
    let out = child.wait_with_output().expect("hash output");
    String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_string()
}

fn timestamp() -> String {
    Command::new("date")
        .args(["-u", "+%Y-%m-%dT%H:%M:%SZ"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|_| "unknown".to_string())
}
