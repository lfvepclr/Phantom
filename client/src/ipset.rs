//! Kernel `ipset` bookkeeping for the kernel-split gateway.
//!
//! The gateway decides *which* destinations enter the tunnel and the kernel
//! enforces it (see [`crate::gateway`]). What the kernel cannot know on its own
//! is which IPs belong to a whitelisted domain — that knowledge arrives with
//! the DNS answers, so this module turns them into set entries.
//!
//! Two properties matter for a router:
//!
//! * **one fork per second, not one per answer.** A busy LAN resolves dozens of
//!   whitelisted names a second; shelling out to `ipset` for each of them would
//!   cost more CPU than the relay this design is trying to remove. Answers are
//!   buffered and flushed as a single `ipset restore` batch.
//! * **entries expire.** A CDN address that stops being handed out should stop
//!   pulling traffic into the tunnel, so every line carries a timeout and the
//!   in-process bookkeeping prunes on the same schedule.

use std::collections::{HashMap, HashSet};
use std::net::Ipv4Addr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::io::AsyncWriteExt;

/// How long answers are coalesced before one `ipset restore` runs.
const FLUSH_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Default)]
struct State {
    /// Answers waiting for the next flush.
    pending: HashSet<Ipv4Addr>,
    /// Entries already pushed, with the moment they were (re)armed.
    known: HashMap<Ipv4Addr, Instant>,
}

/// Publishes whitelisted destinations into a kernel ipset.
pub struct IpsetPublisher {
    name: String,
    ttl: Duration,
    /// Entries the gateway seeded itself (published CIDRs, permanent). They
    /// live in the kernel but never pass through [`IpsetPublisher::add_ips`],
    /// so the UI would otherwise under-report the set size by exactly them.
    seeded: u64,
    state: Mutex<State>,
    entries: AtomicU64,
    /// Set entries observed in the kernel (approximate: what we pushed minus
    /// what has expired locally).
    pushed: AtomicU64,
}

impl IpsetPublisher {
    /// Create the publisher and start its flush task.
    pub fn spawn(name: String, ttl_secs: u32, seeded: u64) -> Arc<Self> {
        let publisher = Arc::new(Self {
            name,
            ttl: Duration::from_secs(u64::from(ttl_secs)),
            seeded,
            state: Mutex::new(State::default()),
            entries: AtomicU64::new(0),
            pushed: AtomicU64::new(0),
        });
        let task = Arc::clone(&publisher);
        tokio::spawn(async move { task.flush_loop().await });
        publisher
    }

    /// Number of destinations currently believed to be in the set.
    pub fn entries(&self) -> u64 {
        self.seeded + self.entries.load(Ordering::Relaxed)
    }

    /// Total entries pushed since start (monotonic; handy for "is it learning?").
    pub fn pushed_total(&self) -> u64 {
        self.pushed.load(Ordering::Relaxed)
    }

    /// Queue destination addresses learned from a whitelisted DNS answer.
    pub fn add_ips(&self, ips: &[Ipv4Addr]) {
        if ips.is_empty() {
            return;
        }
        let now = Instant::now();
        let mut state = match self.state.lock() {
            Ok(s) => s,
            Err(poisoned) => poisoned.into_inner(),
        };
        for ip in ips {
            // Re-arming an expired entry is the common case for long sessions.
            match state.known.get(ip) {
                Some(stamp) if now.duration_since(*stamp) < self.ttl => continue,
                _ => {}
            }
            state.pending.insert(*ip);
        }
    }

    async fn flush_loop(&self) {
        let mut ticker = tokio::time::interval(FLUSH_INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            let batch = self.take_batch();
            if batch.is_empty() {
                continue;
            }
            match self.push(&batch).await {
                Ok(()) => {
                    let now = Instant::now();
                    let mut state = match self.state.lock() {
                        Ok(s) => s,
                        Err(poisoned) => poisoned.into_inner(),
                    };
                    for ip in &batch {
                        state.known.insert(*ip, now);
                    }
                    state
                        .known
                        .retain(|_, stamp| now.duration_since(*stamp) < self.ttl);
                    self.entries
                        .store(state.known.len() as u64, Ordering::Relaxed);
                    self.pushed
                        .fetch_add(batch.len() as u64, Ordering::Relaxed);
                }
                Err(e) => {
                    // Put them back: the next tick retries, and a set that is
                    // missing entries must not look like a successful flush.
                    tracing::warn!("ipset {}: restore failed: {}", self.name, e);
                    let mut state = match self.state.lock() {
                        Ok(s) => s,
                        Err(poisoned) => poisoned.into_inner(),
                    };
                    for ip in &batch {
                        state.pending.insert(*ip);
                    }
                }
            }
        }
    }

    fn take_batch(&self) -> Vec<Ipv4Addr> {
        let mut state = match self.state.lock() {
            Ok(s) => s,
            Err(poisoned) => poisoned.into_inner(),
        };
        let mut batch: Vec<Ipv4Addr> = state.pending.drain().collect();
        batch.sort();
        batch
    }

    async fn push(&self, batch: &[Ipv4Addr]) -> std::io::Result<()> {
        let payload = restore_payload(&self.name, batch, self.ttl.as_secs() as u32);
        let mut child = tokio::process::Command::new("ipset")
            .arg("-!")
            .arg("restore")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(payload.as_bytes()).await?;
            stdin.flush().await?;
            drop(stdin);
        }
        let out = child.wait_with_output().await?;
        if out.status.success() {
            self.pushed
                .fetch_add(batch.len() as u64, Ordering::Relaxed);
            Ok(())
        } else {
            Err(std::io::Error::other(
                String::from_utf8_lossy(&out.stderr).trim().to_string(),
            ))
        }
    }
}

/// `ipset restore` script adding every address with the standard timeout.
fn restore_payload(name: &str, batch: &[Ipv4Addr], ttl_secs: u32) -> String {
    let mut out = String::with_capacity(batch.len() * 40);
    for ip in batch {
        out.push_str(&format!("add {} {} timeout {} -exist\n", name, ip, ttl_secs));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restore_payload_uses_timeouts_and_exist() {
        let batch = vec![Ipv4Addr::new(142, 250, 0, 1), Ipv4Addr::new(8, 8, 8, 8)];
        let payload = restore_payload("phantom_proxy", &batch, 1800);
        assert_eq!(
            payload,
            "add phantom_proxy 142.250.0.1 timeout 1800 -exist\n\
             add phantom_proxy 8.8.8.8 timeout 1800 -exist\n"
        );
    }

    #[test]
    fn add_ips_is_idempotent_within_the_ttl() {
        let publisher = IpsetPublisher {
            name: "phantom_proxy".into(),
            ttl: Duration::from_secs(1800),
            seeded: 0,
            state: Mutex::new(State::default()),
            entries: AtomicU64::new(0),
            pushed: AtomicU64::new(0),
        };
        let ip = Ipv4Addr::new(1, 2, 3, 4);
        publisher.add_ips(&[ip]);
        {
            let state = publisher.state.lock().unwrap();
            assert_eq!(state.pending.len(), 1);
            // Pretend the flush succeeded.
        }
        let mut state = publisher.state.lock().unwrap();
        state.pending.clear();
        state.known.insert(ip, Instant::now());
        drop(state);

        publisher.add_ips(&[ip]);
        let state = publisher.state.lock().unwrap();
        assert!(state.pending.is_empty(), "a live entry must not be re-queued");
    }

    #[test]
    fn entries_include_the_seeded_published_ranges() {
        let publisher = IpsetPublisher {
            name: "phantom_proxy".into(),
            ttl: Duration::from_secs(1800),
            seeded: 142,
            state: Mutex::new(State::default()),
            entries: AtomicU64::new(3),
            pushed: AtomicU64::new(3),
        };
        assert_eq!(publisher.entries(), 145);
    }
}
