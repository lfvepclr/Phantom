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
use phantom_core::{
    ClientConfig, ClientRule, PhantomError, ProxyMode, Result, RuleAction, RulePattern, RulesConfig,
};
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

/// The token every client pane greps for to hide direct traffic.
///
/// Capitalisation is part of the contract: macOS, HarmonyOS and Android all
/// match this exact string, so it is spelled out here once instead of being
/// reconstructed (wrongly) in three languages.
pub const ROUTE_DIRECT_MARKER: &str = "-> Direct (";

/// The one and only shape of a routing breadcrumb.
///
/// Every transport — TUN, SOCKS5 CONNECT and the HTTP proxy — writes its
/// verdict through this function. When they each formatted their own line the
/// SOCKS5/HTTP paths emitted `-> DIRECT (` while the client panes matched
/// `-> Direct (`, so "tunnel only" silently showed every direct flow on macOS
/// (whose system proxy rides the SOCKS5/HTTP path rather than the TUN one).
pub fn route_log_line(
    target: impl std::fmt::Display,
    action: RuleAction,
    reason: impl std::fmt::Display,
) -> String {
    format!("route {} -> {:?} ({})", target, action, reason)
}

/// The same line for an intercepted DNS query, which carries the resolved
/// addresses after the verdict.
pub fn dns_route_log_line(
    domain: &str,
    route: impl std::fmt::Display,
    action: RuleAction,
    answers: &str,
) -> String {
    format!(
        "{} {}",
        route_log_line(format_args!("{}:53", domain), action, format_args!("dns {}", route)),
        answers
    )
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
        let rules = with_user_rules(&cfg.rules);
        let engine = match RuleEngine::from_config(&rules) {
            Ok(e) => Some(e),
            Err(e) => {
                tracing::warn!("Rule engine init failed ({}); using whitelist only", e);
                None
            }
        };
        Self {
            mode: cfg.client.mode,
            engine,
            whitelist: Arc::new(ProxyWhitelist::from_config(&rules, extra)),
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

// ---------------------------------------------------------------------------
// User rule editor (Android / HarmonyOS "分流白名单")
// ---------------------------------------------------------------------------

/// Wire format accepted by [`set_user_rules`].
///
/// One rule per line, `kind:value`, where `kind` is one of:
///
/// ```text
/// domain:www.google.com     exact host
/// suffix:google.com         google.com and every *.google.com
/// keyword:youtube           substring match on the host
/// regex:^.*\.doubleclick\.net$
/// cidr:91.108.4.0/22        whole network (the UI folds IP ranges into these)
/// ```
///
/// A line with no recognised prefix is taken as a bare domain, and a leading
/// `*.` promotes it to a suffix — the plain format the desktop editor already
/// writes, so one string can be pasted between clients.
///
/// Every user rule means *proxy*: this editor extends the proxy whitelist, it
/// does not override it with direct routes.
pub const USER_RULE_FORMAT_HELP: &str =
    "domain:/suffix:/keyword:/regex:/cidr: per line, or a bare domain";

/// Parse [`USER_RULE_FORMAT_HELP`]-formatted text, dropping what cannot be used.
///
/// Invalid entries are skipped (and logged) rather than failing the whole set:
/// one mistyped regex must not cost the user every other rule. Regexes are
/// compiled here for exactly that reason — [`RuleEngine::from_config`] treats a
/// bad pattern as a configuration error and would abandon *all* rules.
pub fn parse_user_rules(text: &str) -> Vec<ClientRule> {
    let mut rules = Vec::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (kind, value) = match line.split_once(':') {
            Some((head, tail))
                if ["domain", "suffix", "keyword", "regex", "cidr"].contains(&head.trim()) =>
            {
                (head.trim(), tail.trim())
            }
            // Bare domain: the desktop editor's format.
            _ => match line.strip_prefix("*.") {
                Some(rest) => ("suffix", rest),
                None => ("domain", line),
            },
        };
        if value.is_empty() {
            tracing::warn!("Dropping empty user rule: {line}");
            continue;
        }
        let pattern = match kind {
            "domain" => RulePattern::DomainFull {
                value: value.to_lowercase(),
            },
            "suffix" => RulePattern::DomainSuffix {
                value: value.to_lowercase(),
            },
            "keyword" => RulePattern::DomainKeyword {
                value: value.to_lowercase(),
            },
            "regex" => {
                if let Err(e) = regex::Regex::new(value) {
                    tracing::warn!("Dropping invalid user regex {value:?}: {e}");
                    continue;
                }
                RulePattern::DomainRegex {
                    value: value.to_string(),
                }
            }
            "cidr" => match value.parse::<IpNet>() {
                Ok(_) => RulePattern::IpCidr {
                    value: value.to_string(),
                },
                Err(e) => {
                    tracing::warn!("Dropping invalid user CIDR {value:?}: {e}");
                    continue;
                }
            },
            other => {
                tracing::warn!("Dropping user rule with unknown kind {other:?}");
                continue;
            }
        };
        rules.push(ClientRule {
            pattern,
            action: RuleAction::Proxy,
        });
    }
    rules
}

/// User rules injected by an embedding UI, on top of `ClientConfig.rules`.
///
/// Rules ride this global rather than the `phantom://` URI on purpose: the URI
/// is scanned, shared and stored as one opaque string, and a rule list inside it
/// would leak into QR codes and connection history. It is also what keeps the
/// desktop editor, Android and HarmonyOS on one wire format.
static USER_RULES: std::sync::Mutex<Vec<ClientRule>> = std::sync::Mutex::new(Vec::new());

/// Replace the user rules with the ones parsed from `text`.
pub fn set_user_rules(text: &str) {
    let rules = parse_user_rules(text);
    tracing::info!("User rules: {} entr(ies) accepted", rules.len());
    if let Ok(mut guard) = USER_RULES.lock() {
        *guard = rules;
    }
}

fn user_rules() -> Vec<ClientRule> {
    USER_RULES.lock().map(|g| g.clone()).unwrap_or_default()
}

/// `cfg.rules` with the UI's rules appended.
///
/// User rules are appended rather than replacing, and [`crate::rules`]' priority
/// order puts them ahead of `final`, so a user entry wins over the built-in
/// default without disturbing anything the config itself asked for.
fn with_user_rules(rules: &RulesConfig) -> RulesConfig {
    let user = user_rules();
    if user.is_empty() {
        return rules.clone();
    }
    let mut merged = rules.clone();
    merged.rules.extend(user);
    merged
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

    /// The client panes ("仅隧道" on macOS, `showDirectLogs` on HarmonyOS and
    /// Android) filter on this exact spelling, so the shared formatter is the
    /// contract: keep both verdicts — and therefore the marker — pinned.
    #[test]
    fn route_log_line_is_the_shape_the_client_panes_match() {
        let proxied = route_log_line("www.google.com:443", RuleAction::Proxy, "whitelist");
        assert_eq!(proxied, "route www.google.com:443 -> Proxy (whitelist)");

        let direct = route_log_line("v.youku.com:443", RuleAction::Direct, "final");
        assert_eq!(direct, "route v.youku.com:443 -> Direct (final)");
        assert!(direct.contains(ROUTE_DIRECT_MARKER));
        assert!(!proxied.contains(ROUTE_DIRECT_MARKER));

        // A proxied flow whose *reason* mentions a failed direct attempt must
        // not be mistaken for direct traffic once it is spelled this way.
        let retried = route_log_line(
            "142.250.0.1:443",
            RuleAction::Proxy,
            "direct connect timed out; retrying through the tunnel",
        );
        assert!(retried.starts_with("route 142.250.0.1:443 -> Proxy ("));

        // Intercepted DNS carries the resolved addresses after the verdict.
        let dns = dns_route_log_line("www.baidu.com", "local", RuleAction::Direct, "1.2.3.4");
        assert_eq!(dns, "route www.baidu.com:53 -> Direct (dns local) 1.2.3.4");
    }
}
