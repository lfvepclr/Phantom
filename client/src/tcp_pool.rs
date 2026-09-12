//! Pool of pre-handshaked TCP tunnel sessions.
//!
//! On the TCP transport every tunnelled flow used to pay `connect(server)` +
//! Noise handshake before it could even send its SYN: two round trips, ≈80 ms to
//! a Hong Kong node, and a browser opening a dozen connections per page pays it
//! over and over. The QUIC path never had that cost — streams ride a single
//! Noise-authenticated connection — so this pool gives TCP the same head start
//! by keeping a small number of *idle, authenticated* sessions ready.
//!
//! Half-open safety rules (a session that is no longer usable must never be
//! handed to a flow, and a session that dies must not surface as a failed user
//! connection):
//!
//! * `take` **removes** the session, so one session can never be used twice;
//! * a session is only handed out while younger than [`MAX_IDLE_AGE`]; older
//!   ones are dropped instead of reused;
//! * every session stores the network epoch it was born in, and a session from
//!   an older epoch is dropped (its source address no longer exists);
//! * the caller falls back to a fresh connect if the SYN on a pooled session
//!   fails, so a session that died while idle costs the flow the handshake it
//!   was meant to skip — never an error;
//! * refills are single-flight per server and pause briefly after
//!   [`TcpSessionPool::clear`], so a network change cannot be answered with a
//!   burst of doomed handshakes.
//!
//! There is no library for this: the pooled object is a Noise session over the
//! project's own frame protocol, so the lifecycle mirrors [`crate::quic_pool`],
//! which solves the same problem for QUIC.

use std::collections::{HashSet, VecDeque};
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use phantom_core::crypto::{NoiseInitiator, SessionReader, SessionWriter, split_after_handshake};
use phantom_core::transport::Transport;
use phantom_core::transport::tcp::TcpTransport;
use phantom_core::{CipherPreference, Result, ServerEntry};
use tokio::io::{ReadHalf, WriteHalf};
use tokio::net::TcpStream;
use tokio::sync::Mutex;

use crate::socks5::{decode_public_key, resolve_offer};

/// Sessions kept ready for the next flows.
const TARGET_IDLE: usize = 2;

/// Hard cap on held sessions, so a burst of flows cannot pile up handshakes.
const MAX_IDLE: usize = 4;

/// How long an unused session may wait for a flow.
///
/// Deliberately short: the pool exists to cover the gap between the resources of
/// one page load, not to keep connections open for minutes. A session older than
/// this is dropped rather than risk handing out one that a NAT already forgot.
const MAX_IDLE_AGE: Duration = Duration::from_secs(8);

/// Handshake timeout for a refill. `None` means "no timeout" (the transport's
/// own connect timeout still applies).
const REFILL_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Delay before a refill starts, so the flow that just consumed a session gets
/// the link to itself first.
const REFILL_DELAY: Duration = Duration::from_millis(150);

/// Quiet period after [`TcpSessionPool::clear`]: the radio just changed, and
/// hammering it with handshakes before it settles only wastes them.
const SETTLE_AFTER_CLEAR: Duration = Duration::from_millis(1500);

/// One authenticated session that has not carried a flow yet.
pub struct PooledSession {
    pub reader: SessionReader<ReadHalf<TcpStream>>,
    pub writer: SessionWriter<WriteHalf<TcpStream>>,
    /// Server address the session was opened to (pool key).
    server: String,
    /// Cipher preference it was negotiated with (pool key).
    cipher: CipherPreference,
    /// When the handshake finished; used for [`MAX_IDLE_AGE`].
    born_at: Instant,
    /// Network epoch at handshake time; see [`crate::tun::network_epoch`].
    epoch: u64,
}

impl PooledSession {
    /// Milliseconds this session has been waiting for a flow.
    pub fn idle_ms(&self) -> u128 {
        self.born_at.elapsed().as_millis()
    }
}

#[derive(Default)]
struct State {
    idle: VecDeque<PooledSession>,
    /// Pool keys with a refill in flight (single-flight).
    refilling: HashSet<String>,
    /// Set by `clear`, so refills wait for the new network to settle.
    quiet_until: Option<Instant>,
}

/// Counters exposed for tests and log lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PoolStats {
    /// Sessions built by a handshake (cold flows + refills).
    pub created: u64,
    /// Flows served by a session that was already handshaked.
    pub reused: u64,
}

/// Per-server cache of ready-to-use tunnel sessions.
#[derive(Default)]
pub struct TcpSessionPool {
    state: Mutex<State>,
    created: AtomicU64,
    reused: AtomicU64,
}

impl TcpSessionPool {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn stats(&self) -> PoolStats {
        PoolStats {
            created: self.created.load(Ordering::Relaxed),
            reused: self.reused.load(Ordering::Relaxed),
        }
    }

    /// Number of sessions currently waiting for a flow.
    pub async fn idle_count(&self) -> usize {
        let now = Instant::now();
        let epoch = crate::tun::network_epoch();
        let mut state = self.state.lock().await;
        prune(&mut state, now, epoch);
        state.idle.len()
    }

    /// Take a ready session for `server`, if one is fresh.
    ///
    /// Returns `None` when the pool is empty or holds only sessions for another
    /// server/cipher; the caller then establishes one the slow way.
    pub async fn take(
        &self,
        server: &ServerEntry,
        cipher: CipherPreference,
    ) -> Option<PooledSession> {
        let now = Instant::now();
        let epoch = crate::tun::network_epoch();
        let mut state = self.state.lock().await;
        prune(&mut state, now, epoch);
        let session = state
            .idle
            .iter()
            .position(|s| s.server == server.address && s.cipher == cipher)
            .and_then(|at| state.idle.remove(at))?;
        self.reused.fetch_add(1, Ordering::Relaxed);
        Some(session)
    }

    /// Drop every session and pause refills.
    ///
    /// Called when the OS reports a new network: sessions are bound to a source
    /// address that no longer exists, and the ones that come back must be
    /// rebuilt on the new link.
    pub async fn clear(&self) {
        let mut state = self.state.lock().await;
        let dropped = state.idle.len();
        state.idle.clear();
        state.refilling.clear();
        state.quiet_until = Some(Instant::now() + SETTLE_AFTER_CLEAR);
        if dropped > 0 {
            tracing::info!("TCP session pool cleared ({dropped} idle session(s))");
        }
    }

    /// Top the pool back up after a session was handed out.
    ///
    /// Spawned rather than awaited: a flow must never wait for a handshake it
    /// does not need itself. Safe to call on every flow — the bookkeeping makes
    /// concurrent calls collapse into one refill per server.
    pub fn spawn_refill(
        self: &Arc<Self>,
        server: ServerEntry,
        local_secret: [u8; 32],
        cipher: CipherPreference,
    ) {
        let pool = Arc::clone(self);
        tokio::spawn(async move {
            tokio::time::sleep(REFILL_DELAY).await;
            pool.refill(server, local_secret, cipher).await;
        });
    }

    async fn refill(
        &self,
        server: ServerEntry,
        local_secret: [u8; 32],
        cipher: CipherPreference,
    ) {
        let key = pool_key(&server, cipher);
        let now = Instant::now();
        let epoch = crate::tun::network_epoch();
        {
            let mut state = self.state.lock().await;
            prune(&mut state, now, epoch);
            if state.quiet_until.is_some_and(|until| now < until) {
                return;
            }
            let matching = state
                .idle
                .iter()
                .filter(|s| s.server == server.address && s.cipher == cipher)
                .count();
            if matching >= TARGET_IDLE || !state.refilling.insert(key.clone()) {
                return;
            }
        }

        let result = self.connect_session(&server, &local_secret, cipher).await;
        let mut state = self.state.lock().await;
        state.refilling.remove(&key);
        match result {
            Ok(session) => {
                // The network may have changed while the handshake was in
                // flight; such a session is dead on arrival.
                if state.idle.len() < MAX_IDLE && session.epoch == crate::tun::network_epoch() {
                    state.idle.push_back(session);
                    tracing::debug!("TCP session pool refilled ({} idle)", state.idle.len());
                }
            }
            // A failed refill is invisible by design: the next flow simply
            // establishes its own session.
            Err(e) => tracing::debug!("TCP session refill failed: {e}"),
        }
    }

    async fn connect_session(
        &self,
        server: &ServerEntry,
        local_secret: &[u8; 32],
        cipher: CipherPreference,
    ) -> Result<PooledSession> {
        let epoch = crate::tun::network_epoch();
        let addr: SocketAddr = server
            .address
            .parse()
            .map_err(|e| phantom_core::PhantomError::Config(format!(
                "Invalid server address: {e}"
            )))?;
        let transport = TcpTransport::new(REFILL_CONNECT_TIMEOUT);
        let stream = transport.connect(&addr).await?;
        let remote_public = decode_public_key(&server.public_key)?;
        let initiator = NoiseInitiator::new(local_secret, &remote_public, server.decode_psk()?);
        let result = initiator.handshake(stream, &resolve_offer(cipher)).await?;
        let (reader, writer) = split_after_handshake(
            result.stream,
            result.split_keys,
            result.chosen_cipher,
            result.is_initiator,
        );
        tracing::debug!("TCP session pooled (cipher={})", result.chosen_cipher);
        self.created.fetch_add(1, Ordering::Relaxed);
        Ok(PooledSession {
            reader,
            writer,
            server: server.address.clone(),
            cipher,
            born_at: Instant::now(),
            epoch,
        })
    }
}

/// Drop sessions that are too old or from a previous network epoch.
fn prune(state: &mut State, now: Instant, epoch: u64) {
    state
        .idle
        .retain(|s| now.duration_since(s.born_at) < MAX_IDLE_AGE && s.epoch == epoch);
}

fn pool_key(server: &ServerEntry, cipher: CipherPreference) -> String {
    format!("{}|{cipher:?}", server.address)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pool key must distinguish servers and ciphers: a session handshaked with
    /// one preference cannot serve a flow that asked for another (the server
    /// negotiated that cipher for the whole session).
    #[test]
    fn pool_key_covers_server_and_cipher() {
        let mut server = ServerEntry {
            name: "a".into(),
            address: "127.0.0.1:443".into(),
            public_key: String::new(),
            psk: String::new(),
            cipher: CipherPreference::Auto,
            protocol: Default::default(),
        };
        let auto = pool_key(&server, CipherPreference::Auto);
        let aes = pool_key(&server, CipherPreference::Aes256Gcm);
        assert_ne!(auto, aes, "cipher must be part of the key");
        server.address = "127.0.0.1:444".into();
        assert_ne!(auto, pool_key(&server, CipherPreference::Auto));
    }

    /// A quiet pool hands out nothing, and a cleared pool pauses refills: that
    /// is what keeps a Wi-Fi ⇄ cellular switch from resurrecting sessions bound
    /// to the old address.
    #[tokio::test]
    async fn cleared_pool_stays_quiet() {
        let pool = TcpSessionPool::new();
        assert_eq!(pool.idle_count().await, 0);
        pool.clear().await;
        let state = pool.state.lock().await;
        assert!(
            state.quiet_until.is_some_and(|until| until > Instant::now()),
            "clear() must hold refills back for the settle period"
        );
    }

    /// Taking from an empty pool is a miss, not an error: the caller falls back
    /// to a fresh handshake.
    #[tokio::test]
    async fn empty_pool_misses() {
        let pool = TcpSessionPool::new();
        let server = ServerEntry {
            name: "a".into(),
            address: "127.0.0.1:443".into(),
            public_key: String::new(),
            psk: String::new(),
            cipher: CipherPreference::Auto,
            protocol: Default::default(),
        };
        assert!(pool.take(&server, CipherPreference::Auto).await.is_none());
        assert_eq!(pool.stats().reused, 0);
    }
}
