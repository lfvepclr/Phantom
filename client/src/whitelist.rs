//! Proxy whitelist + routing decision (Phantom is **direct by default**).
//!
//! Only destinations on the whitelist take the tunnel; everything else is
//! connected directly. The built-in whitelist is the censored-domain list
//! compiled by `cargo xtask rules update` into an FST, which loads as a
//! zero-copy view over the embedded bytes (no parse, no per-entry allocation).
//!
//! DNS note: whitelisted (proxied) targets are never resolved locally — the
//! hostname travels to the server, which is what keeps the system resolver's
//! cache free of poisoned answers for censored domains.

use crate::rules::RuleEngine;
use ipnet::IpNet;
use phantom_core::{ClientConfig, PhantomError, ProxyMode, Result, RuleAction, RulesConfig};
use std::collections::HashSet;
use std::net::IpAddr;
use std::sync::Arc;

/// Compiled by `cargo xtask rules update` (~4.4k censored domains, ~37 KiB).
static BUILTIN_FST: &[u8] = include_bytes!("../data/proxy_domains.fst");
/// CIDR entries for services only reachable by IP (e.g. Telegram).
static BUILTIN_CIDRS: &str = include_str!("../data/proxy_cidrs.txt");

pub struct ProxyWhitelist {
    domains: Option<fst::Set<&'static [u8]>>,
    /// User-supplied suffixes (config file / macOS UI), matched alongside the
    /// built-in index.
    extra: HashSet<String>,
    cidrs: Vec<IpNet>,
}

impl ProxyWhitelist {
    /// Built-in list only.
    pub fn builtin() -> Self {
        let domains = fst::Set::new(BUILTIN_FST).ok();
        let cidrs = BUILTIN_CIDRS
            .lines()
            .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
            .filter_map(|l| l.trim().parse::<IpNet>().ok())
            .collect();
        Self {
            domains,
            extra: HashSet::new(),
            cidrs,
        }
    }

    /// Build from the client config: honours `builtin_proxy_whitelist` and
    /// folds in the operator's extra domains.
    pub fn from_config(rules: &RulesConfig, extra_domains: &[String]) -> Self {
        let mut wl = if rules.builtin_proxy_whitelist {
            Self::builtin()
        } else {
            Self {
                domains: None,
                extra: HashSet::new(),
                cidrs: Vec::new(),
            }
        };
        for d in extra_domains {
            wl.add_domain(d);
        }
        for d in load_user_domains() {
            wl.add_domain(&d);
        }
        wl
    }

    pub fn add_domain(&mut self, domain: &str) {
        let d = normalise_domain(domain);
        if !d.is_empty() {
            self.extra.insert(d);
        }
    }

    pub fn is_empty(&self) -> bool {
        self.domains.is_none() && self.extra.is_empty() && self.cidrs.is_empty()
    }

    /// Suffix match: `v.youku.com` probes `v.youku.com`, `youku.com`, `com`.
    pub fn is_proxied_domain(&self, domain: &str) -> bool {
        let lowered;
        let domain = if domain.bytes().any(|b| b.is_ascii_uppercase()) {
            lowered = domain.to_ascii_lowercase();
            lowered.as_str()
        } else {
            domain
        };
        let domain = domain.trim_end_matches('.');
        if domain.is_empty() {
            return false;
        }

        let mut rest = domain;
        loop {
            if self.extra.contains(rest) {
                return true;
            }
            if let Some(set) = &self.domains {
                if set.contains(rest.as_bytes()) {
                    return true;
                }
            }
            match rest.find('.') {
                Some(idx) => rest = &rest[idx + 1..],
                None => return false,
            }
        }
    }

    /// IP-based entries (services that are blocked by address, not by name).
    pub fn is_proxied_ip(&self, ip: IpAddr) -> bool {
        self.cidrs.iter().any(|net| net.contains(&ip))
    }
}

fn normalise_domain(domain: &str) -> String {
    domain
        .trim()
        .trim_start_matches(['+', '*', '|'])
        .trim_start_matches('.')
        .trim_end_matches('.')
        .to_ascii_lowercase()
}

/// Extra domains from `~/.config/phantom/proxy_domains.txt` (one per line,
/// `#` comments allowed). Missing file is not an error.
pub fn load_user_domains() -> Vec<String> {
    let path = match user_domains_path() {
        Some(p) => p,
        None => return Vec::new(),
    };
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(_) => return Vec::new(),
    };
    text.lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| l.to_string())
        .collect()
}

fn user_domains_path() -> Option<std::path::PathBuf> {
    if let Ok(explicit) = std::env::var("PHANTOM_PROXY_DOMAINS") {
        if !explicit.is_empty() {
            return Some(std::path::PathBuf::from(explicit));
        }
    }
    let home = std::env::var("HOME").ok()?;
    Some(
        std::path::Path::new(&home)
            .join(".config/phantom")
            .join("proxy_domains.txt"),
    )
}

/// Why a routing decision was made — surfaced in logs and metrics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteReason {
    /// `mode = proxy` / `mode = direct` shortcut.
    Mode,
    /// A user rule matched.
    User,
    /// The built-in or user-supplied whitelist matched.
    Whitelist,
    /// Nothing matched: the configured `final_action` applies.
    Final,
}

impl RouteReason {
    pub fn as_str(self) -> &'static str {
        match self {
            RouteReason::Mode => "mode",
            RouteReason::User => "user",
            RouteReason::Whitelist => "whitelist",
            RouteReason::Final => "final",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct RouteDecision {
    pub action: RuleAction,
    pub reason: RouteReason,
}

impl RouteDecision {
    pub fn is_direct(&self) -> bool {
        matches!(self.action, RuleAction::Direct)
    }

    /// Whether a failed *direct* connection may be retried through the tunnel.
    ///
    /// Only the "nothing matched, so send it outside" verdict qualifies: an
    /// explicit `mode = direct` is a user instruction, and a user rule that
    /// says Direct must not be second-guessed either. Everything else keeps the
    /// plain meaning of the decision.
    pub fn allows_tunnel_fallback(&self, mode: ProxyMode) -> bool {
        self.action == RuleAction::Direct
            && self.reason == RouteReason::Final
            && mode != ProxyMode::Direct
    }
}

/// The single source of truth for "direct or tunnel?".
///
/// Order: mode shortcut → user rules → proxy whitelist → `final_action`
/// (which defaults to Direct, i.e. Phantom only tunnels what needs it).
pub fn decide(
    mode: ProxyMode,
    engine: Option<&RuleEngine>,
    whitelist: Option<&ProxyWhitelist>,
    domain: Option<&str>,
    ip: Option<IpAddr>,
    port: u16,
) -> RouteDecision {
    match mode {
        ProxyMode::Proxy => {
            return RouteDecision {
                action: RuleAction::Proxy,
                reason: RouteReason::Mode,
            };
        }
        ProxyMode::Direct => {
            return RouteDecision {
                action: RuleAction::Direct,
                reason: RouteReason::Mode,
            };
        }
        ProxyMode::Smart | ProxyMode::Auto => {}
    }

    if let Some(engine) = engine {
        if let Some(action) = engine.query_rule(domain, ip, Some(port)) {
            return RouteDecision {
                action,
                reason: RouteReason::User,
            };
        }
    }

    if let Some(wl) = whitelist {
        if let Some(d) = domain {
            if wl.is_proxied_domain(d) {
                return RouteDecision {
                    action: RuleAction::Proxy,
                    reason: RouteReason::Whitelist,
                };
            }
        }
        if let Some(addr) = ip {
            if wl.is_proxied_ip(addr) {
                return RouteDecision {
                    action: RuleAction::Proxy,
                    reason: RouteReason::Whitelist,
                };
            }
        }
    }

    RouteDecision {
        action: engine
            .map(|e| e.final_action())
            .unwrap_or(RuleAction::Direct),
        reason: RouteReason::Final,
    }
}

/// Build the whitelist for a client config (built-in FST + user entries).
pub fn build_for_config(rules: &RulesConfig, extra_domains: &[String]) -> Result<ProxyWhitelist> {
    if !rules.builtin_proxy_whitelist {
        tracing::info!("Built-in proxy whitelist disabled by config");
    }
    let wl = ProxyWhitelist::from_config(rules, extra_domains);
    if wl.domains.is_none() && wl.extra.is_empty() {
        tracing::warn!("Proxy whitelist is empty: Smart mode will not tunnel anything");
        return Err(PhantomError::Config(
            "proxy whitelist unavailable".to_string(),
        ));
    }
    Ok(wl)
}

/// Routing state for one client process: mode + user rule engine + whitelist.
pub struct Router {
    mode: ProxyMode,
    engine: Option<RuleEngine>,
    whitelist: Arc<ProxyWhitelist>,
}

impl Router {
    pub fn new(cfg: &ClientConfig, extra: &[String]) -> Self {
        let engine = match RuleEngine::from_config(&cfg.rules) {
            Ok(e) => Some(e),
            Err(e) => {
                tracing::warn!("Rule engine init failed ({}); using whitelist only", e);
                None
            }
        };
        Self {
            mode: cfg.client.mode,
            engine,
            whitelist: Arc::new(ProxyWhitelist::from_config(&cfg.rules, extra)),
        }
    }

    pub fn decide(&self, domain: Option<&str>, ip: Option<IpAddr>, port: u16) -> RouteDecision {
        decide(
            self.mode,
            self.engine.as_ref(),
            Some(self.whitelist.as_ref()),
            domain,
            ip,
            port,
        )
    }

    pub fn whitelist(&self) -> Arc<ProxyWhitelist> {
        Arc::clone(&self.whitelist)
    }
}

/// Extra whitelist entries injected by an embedding UI (the macOS app's
/// "分流白名单" editor). Replaceable: the UI can edit the list and restart the
/// tunnel without relaunching the app.
static EXTRA_DOMAINS: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

pub fn preset_extra_domains(domains: Vec<String>) {
    if let Ok(mut guard) = EXTRA_DOMAINS.lock() {
        *guard = domains;
    }
}

fn extra_domains() -> Vec<String> {
    EXTRA_DOMAINS.lock().map(|g| g.clone()).unwrap_or_default()
}

/// Process-wide routing state.
///
/// A client process serves one configuration (macOS app, CLI, Android/HarmonyOS
/// VPN service), so keeping one router avoids rebuilding the rule engine and the
/// whitelist per connection. `rebuild` lets a UI apply edited whitelist entries
/// on the next tunnel start.
static ROUTER: std::sync::RwLock<Option<Arc<Router>>> = std::sync::RwLock::new(None);

pub fn shared(cfg: &ClientConfig) -> Arc<Router> {
    if let Ok(guard) = ROUTER.read() {
        if let Some(router) = guard.as_ref() {
            return Arc::clone(router);
        }
    }
    let router = Arc::new(Router::new(cfg, &extra_domains()));
    if let Ok(mut guard) = ROUTER.write() {
        *guard = Some(Arc::clone(&router));
    }
    router
}

/// Rebuild the routing state from `cfg` + the current user whitelist entries.
pub fn rebuild(cfg: &ClientConfig) {
    let router = Arc::new(Router::new(cfg, &extra_domains()));
    if let Ok(mut guard) = ROUTER.write() {
        *guard = Some(router);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use phantom_core::ClientConfig;

    fn engine(cfg: &RulesConfig) -> RuleEngine {
        RuleEngine::from_config(cfg).expect("engine")
    }

    #[test]
    fn builtin_whitelist_matches_censored_domains_only() {
        let wl = ProxyWhitelist::builtin();
        assert!(
            wl.is_proxied_domain("www.google.com"),
            "google through tunnel"
        );
        assert!(wl.is_proxied_domain("youtube.com"));
        assert!(wl.is_proxied_domain("WWW.GOOGLE.COM"), "case-insensitive");
        assert!(!wl.is_proxied_domain("v.youku.com"), "youku stays direct");
        assert!(!wl.is_proxied_domain("www.baidu.com"));
        assert!(
            wl.is_proxied_ip("91.108.56.1".parse().unwrap()),
            "telegram CIDR"
        );
        assert!(!wl.is_proxied_ip("1.1.1.1".parse().unwrap()));
    }

    #[test]
    fn smart_mode_defaults_to_direct_and_proxies_whitelist() {
        let cfg = ClientConfig::default();
        let wl = ProxyWhitelist::builtin();
        let eng = engine(&cfg.rules);

        let d = decide(
            ProxyMode::Smart,
            Some(&eng),
            Some(&wl),
            Some("v.youku.com"),
            None,
            443,
        );
        assert_eq!(d.action, RuleAction::Direct);
        assert_eq!(d.reason, RouteReason::Final);

        let d = decide(
            ProxyMode::Smart,
            Some(&eng),
            Some(&wl),
            Some("www.google.com"),
            None,
            443,
        );
        assert_eq!(d.action, RuleAction::Proxy);
        assert_eq!(d.reason, RouteReason::Whitelist);
    }

    #[test]
    fn user_rules_and_modes_win_over_whitelist() {
        let mut cfg = ClientConfig::default();
        cfg.rules.rules.push(phantom_core::ClientRule {
            pattern: phantom_core::RulePattern::DomainSuffix {
                value: "youku.com".to_string(),
            },
            action: RuleAction::Proxy,
        });
        let eng = engine(&cfg.rules);
        let wl = ProxyWhitelist::builtin();

        let d = decide(
            ProxyMode::Smart,
            Some(&eng),
            Some(&wl),
            Some("v.youku.com"),
            None,
            443,
        );
        assert_eq!(d.action, RuleAction::Proxy, "explicit rule wins");
        assert_eq!(d.reason, RouteReason::User);

        let d = decide(
            ProxyMode::Proxy,
            Some(&eng),
            Some(&wl),
            Some("v.youku.com"),
            None,
            443,
        );
        assert_eq!(d.action, RuleAction::Proxy);
        assert_eq!(d.reason, RouteReason::Mode);

        let d = decide(
            ProxyMode::Direct,
            Some(&eng),
            Some(&wl),
            Some("www.google.com"),
            None,
            443,
        );
        assert_eq!(d.action, RuleAction::Direct);
    }

    #[test]
    fn final_action_proxy_restores_full_tunnel() {
        let mut cfg = ClientConfig::default();
        cfg.rules.final_action = RuleAction::Proxy;
        let eng = engine(&cfg.rules);
        let wl = ProxyWhitelist::builtin();
        let d = decide(
            ProxyMode::Smart,
            Some(&eng),
            Some(&wl),
            Some("v.youku.com"),
            None,
            443,
        );
        assert_eq!(d.action, RuleAction::Proxy);
        assert_eq!(d.reason, RouteReason::Final);
    }

    #[test]
    fn user_supplied_domains_are_proxied() {
        let cfg = ClientConfig::default();
        let eng = engine(&cfg.rules);
        let wl = ProxyWhitelist::from_config(&cfg.rules, &["example.com".to_string()]);
        let d = decide(
            ProxyMode::Smart,
            Some(&eng),
            Some(&wl),
            Some("api.example.com"),
            None,
            443,
        );
        assert_eq!(d.action, RuleAction::Proxy);
        assert_eq!(d.reason, RouteReason::Whitelist);
    }

    /// A Google IP the app resolved on its own lands on the `final` (Direct)
    /// verdict with an empty DNS cache. That guess is the one case allowed to
    /// be retried through the tunnel when the direct connect fails.
    #[test]
    fn only_the_final_verdict_may_fall_back_to_the_tunnel() {
        let cfg = ClientConfig::default();
        let eng = engine(&cfg.rules);
        let wl = ProxyWhitelist::builtin();

        let unknown_ip = decide(ProxyMode::Smart, Some(&eng), Some(&wl), None, None, 443);
        assert_eq!(unknown_ip.action, RuleAction::Direct);
        assert_eq!(unknown_ip.reason, RouteReason::Final);
        assert!(unknown_ip.allows_tunnel_fallback(ProxyMode::Smart));

        // Explicit "everything direct" is an instruction, not a guess.
        let forced = decide(ProxyMode::Direct, Some(&eng), Some(&wl), None, None, 443);
        assert!(!forced.allows_tunnel_fallback(ProxyMode::Direct));

        // User rules stay authoritative.
        let mut cfg_rules = ClientConfig::default();
        cfg_rules.rules.rules.push(phantom_core::ClientRule {
            action: RuleAction::Direct,
            pattern: phantom_core::RulePattern::DomainFull {
                value: "internal.example".to_string(),
            },
        });
        let eng2 = engine(&cfg_rules.rules);
        let user = decide(
            ProxyMode::Smart,
            Some(&eng2),
            Some(&wl),
            Some("internal.example"),
            None,
            443,
        );
        assert_eq!(user.reason, RouteReason::User);
        assert!(!user.allows_tunnel_fallback(ProxyMode::Smart));

        // Whitelisted traffic is already a Proxy verdict.
        let whitelisted = decide(
            ProxyMode::Smart,
            Some(&eng),
            Some(&wl),
            Some("www.google.com"),
            None,
            443,
        );
        assert!(!whitelisted.allows_tunnel_fallback(ProxyMode::Smart));
    }
}
