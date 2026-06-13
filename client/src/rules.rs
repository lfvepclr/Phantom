//! High-performance routing rule engine for Smart / Split-Tunnel mode.
//!
//! Supports:
//! - Domain: full, suffix, keyword, regex
//! - IP-CIDR: IPv4 and IPv6 longest-prefix match
//! - Port: exact match
//! - GEOIP: country code lookup (optional feature `geoip`)
//!
//! Design goals: O(1) or O(domain-depth) query, no per-packet allocation.
//!
//! Hot-path data structures:
//! - `DomainKeyword`  : [`daachorse::DoubleArrayAhoCorasick`] (single-pass AC scan).
//! - `IpCidr`         : [`iptrie::LCTrieMap`] (level-compressed patricia trie, O(prefix-bits) lookup).
//! - `DomainSuffix`   : in-house radix-style trie keyed by reversed labels.
//! - `DomainFull`/`Port` : `HashMap` for O(1) exact match.

use daachorse::{DoubleArrayAhoCorasick, DoubleArrayAhoCorasickBuilder};
use ipnet::{Ipv4Net, Ipv6Net};
use iptrie::{Ipv4Prefix, Ipv6Prefix, LCTrieMap, RTrieMap};
use phantom_core::{PhantomError, Result, RuleAction, RulePattern, RulesConfig};
use regex::RegexSet;
use std::collections::HashMap;
use std::net::IpAddr;

/// Compiled rule engine built from config.
pub struct RuleEngine {
    domain_full: HashMap<String, RuleAction>,
    domain_suffix: DomainSuffixTrie,
    /// `None` when the rule set contains no `DomainKeyword` rules — the AC
    /// automaton is built only when we actually have patterns to match.
    domain_keyword: Option<DoubleArrayAhoCorasick<RuleAction>>,
    domain_regex: Option<RegexSet>,
    /// Per-pattern action, indexed by position in the `RegexSet`. Length
    /// equals the number of `DomainRegex` rules (0 when `domain_regex` is
    /// `None`).
    regex_actions: Vec<RuleAction>,
    /// LC-trie for IPv4 longest-prefix match. The trie stores
    /// `Option<RuleAction>` so the root can mean "no match" (i.e. fall through
    /// to port/geoip/final) and only the user-configured prefixes return a
    /// concrete action. The trie itself is `None` when there are no CIDR
    /// rules, so the hot path can skip the lookup entirely.
    ip_cidr_v4: Option<LCTrieMap<Ipv4Prefix, Option<RuleAction>>>,
    /// LC-trie for IPv6 longest-prefix match. See `ip_cidr_v4` for semantics.
    ip_cidr_v6: Option<LCTrieMap<Ipv6Prefix, Option<RuleAction>>>,
    ports: HashMap<u16, RuleAction>,
    geoip_rules: HashMap<String, RuleAction>,
    #[cfg(feature = "geoip")]
    geoip: Option<maxminddb::Reader<Vec<u8>>>,
    final_action: RuleAction,
}

impl RuleEngine {
    /// Build engine from deserialized config.
    pub fn from_config(cfg: &RulesConfig) -> Result<Self> {
        let mut domain_full = HashMap::new();
        let mut domain_suffix = DomainSuffixTrie::new();
        let mut domain_keyword_pairs: Vec<(String, RuleAction)> = Vec::new();
        let mut regex_pairs: Vec<(String, RuleAction)> = Vec::new();
        let mut ip_cidr_v4_pairs: Vec<(Ipv4Prefix, RuleAction)> = Vec::new();
        let mut ip_cidr_v6_pairs: Vec<(Ipv6Prefix, RuleAction)> = Vec::new();
        let mut ports = HashMap::new();
        let geoip_rules = HashMap::new();

        for rule in &cfg.rules {
            match &rule.pattern {
                RulePattern::DomainFull { value } => {
                    domain_full.insert(value.to_lowercase(), rule.action);
                }
                RulePattern::DomainSuffix { value } => {
                    domain_suffix.insert(value, rule.action);
                }
                RulePattern::DomainKeyword { value } => {
                    domain_keyword_pairs.push((value.to_lowercase(), rule.action));
                }
                RulePattern::DomainRegex { value } => {
                    regex_pairs.push((value.clone(), rule.action));
                }
                RulePattern::IpCidr { value } => {
                    if let Ok(net) = value.parse::<Ipv4Net>() {
                        let prefix = Ipv4Prefix::new(net.network(), net.prefix_len())
                            .map_err(|e| PhantomError::Config(format!("Invalid IPv4 prefix: {}", e)))?;
                        ip_cidr_v4_pairs.push((prefix, rule.action));
                    } else if let Ok(net) = value.parse::<Ipv6Net>() {
                        let prefix = Ipv6Prefix::new(net.network(), net.prefix_len())
                            .map_err(|e| PhantomError::Config(format!("Invalid IPv6 prefix: {}", e)))?;
                        ip_cidr_v6_pairs.push((prefix, rule.action));
                    } else {
                        return Err(PhantomError::Config(format!(
                            "Invalid IP-CIDR: {}",
                            value
                        )));
                    }
                }
                RulePattern::Port { value } => {
                    ports.insert(*value, rule.action);
                }
                #[cfg(feature = "geoip")]
                RulePattern::GeoIp { value } => {
                    geoip_rules.insert(value.to_uppercase(), rule.action);
                }
                RulePattern::Final => {
                    // Final action handled separately.
                }
            }
        }

        // Build a `RegexSet` from all domain-regex patterns. The set is queried
        // in a single pass and reports matching pattern indices in ascending
        // order — `iter().next()` therefore returns the lowest-indexed match,
        // preserving the "first match wins" config-order priority of the
        // previous `Vec<Regex>` implementation.
        let (domain_regex, regex_actions) = if regex_pairs.is_empty() {
            (None, Vec::new())
        } else {
            let pat_strs: Vec<&str> = regex_pairs.iter().map(|(p, _)| p.as_str()).collect();
            // The default 10 MB per-regex size limit is too tight for very
            // large rule sets (e.g. 10k+ patterns with DFA caching); lift it
            // to 256 MB and bump the DFA cache proportionally. The defaults
            // are still applied to individual patterns, so pathological
            // patterns will still be rejected.
            let set = regex::RegexSetBuilder::new(&pat_strs)
                .size_limit(256 * 1024 * 1024)
                .dfa_size_limit(64 * 1024 * 1024)
                .build()
                .map_err(|e| PhantomError::Config(format!("Invalid domain regex: {}", e)))?;
            let actions: Vec<RuleAction> = regex_pairs.iter().map(|(_, a)| *a).collect();
            (Some(set), actions)
        };

        // Build AC automaton for keyword rules. We track insertion order via
        // `LeftmostFirst` so that when several patterns match at the same
        // position the earliest-registered (== config-order) one wins,
        // matching the previous Vec-iteration semantics.
        let domain_keyword = if domain_keyword_pairs.is_empty() {
            None
        } else {
            // `build_with_values` accepts any `Copy` value type (unlike
            // `build`, which insists on `TryFrom<usize>`); perfect for our enum.
            let automaton = DoubleArrayAhoCorasickBuilder::new()
                .match_kind(daachorse::MatchKind::LeftmostFirst)
                .build_with_values(
                    domain_keyword_pairs
                        .iter()
                        .map(|(p, a)| (p.clone(), *a)),
                )
                .map_err(|e| PhantomError::Config(format!("Failed to build keyword automaton: {}", e)))?;
            Some(automaton)
        };

        // Build level-compressed patricia tries for IP-CIDR. The root stores
        // `None` ("no user rule matched"); only the explicitly-inserted
        // prefixes store `Some(action)`. This preserves the old
        // "fall-through-to-port" semantics when the IP doesn't match any
        // configured prefix.
        let mut rtrie_v4 = RTrieMap::with_root(None);
        for (prefix, action) in &ip_cidr_v4_pairs {
            rtrie_v4.insert(*prefix, Some(*action));
        }
        let ip_cidr_v4 = if ip_cidr_v4_pairs.is_empty() {
            None
        } else {
            Some(rtrie_v4.compress())
        };

        let mut rtrie_v6 = RTrieMap::with_root(None);
        for (prefix, action) in &ip_cidr_v6_pairs {
            rtrie_v6.insert(*prefix, Some(*action));
        }
        let ip_cidr_v6 = if ip_cidr_v6_pairs.is_empty() {
            None
        } else {
            Some(rtrie_v6.compress())
        };

        #[cfg(feature = "geoip")]
        let geoip = None;

        Ok(Self {
            domain_full,
            domain_suffix,
            domain_keyword,
            domain_regex,
            regex_actions,
            ip_cidr_v4,
            ip_cidr_v6,
            ports,
            geoip_rules,
            #[cfg(feature = "geoip")]
            geoip,
            final_action: cfg.final_action,
        })
    }

    /// Load a MaxMind DB for GEOIP lookups (optional).
    #[cfg(feature = "geoip")]
    pub fn load_geoip(&mut self, path: &str) -> Result<()> {
        let reader = maxminddb::Reader::open_readfile(path)
            .map_err(|e| PhantomError::Config(format!("Failed to open GeoIP DB: {}", e)))?;
        self.geoip = Some(reader);
        Ok(())
    }

    /// Query the engine for a routing decision.
    ///
    /// At least one of `domain` or `ip` should be provided.  Rules are evaluated
    /// in config order (already encoded into separate structures).  Priority:
    /// 1. Domain full > suffix > keyword > regex
    /// 2. IP-CIDR (longest prefix first)
    /// 3. Port
    /// 4. GEOIP
    /// 5. Final action
    pub fn query(&self, domain: Option<&str>, ip: Option<IpAddr>, port: Option<u16>) -> RuleAction {
        // 1. Domain rules (most specific first).
        if let Some(d) = domain {
            let d = d.to_lowercase();
            if let Some(action) = self.domain_full.get(&d) {
                return *action;
            }
            if let Some(action) = self.domain_suffix.query(&d) {
                return action;
            }
            if let Some(ac) = &self.domain_keyword {
                // `leftmost_find_iter` yields non-overlapping leftmost
                // matches; with `MatchKind::LeftmostFirst` the first hit at
                // each position is the earliest-registered pattern. Take the
                // very first occurrence, which is the earliest pattern in
                // the global match order — equivalent to scanning a Vec in
                // order.
                if let Some(m) = ac.leftmost_find_iter(&d).next() {
                    return m.value();
                }
            }
            if let Some(re_set) = &self.domain_regex {
                // `matches().iter()` yields matching pattern indices in
                // ascending order (== config order). The first one is the
                // earliest-registered pattern, matching the previous
                // `Vec<Regex>` greedy semantics.
                if let Some(idx) = re_set.matches(&d).iter().next() {
                    return self.regex_actions[idx];
                }
            }
        }

        // 2. IP-CIDR rules (longest prefix match). Only return when the IP
        // actually matches a user-configured prefix; on no match we fall
        // through to port / geoip / final just like the legacy engine did.
        if let Some(addr) = ip {
            match addr {
                IpAddr::V4(v4) => {
                    if let Some(trie) = &self.ip_cidr_v4 {
                        let key = Ipv4Prefix::new(v4, 32).expect("/32 always valid");
                        let (_, opt_action) = trie.lookup(&key);
                        if let Some(action) = opt_action {
                            return *action;
                        }
                    }
                }
                IpAddr::V6(v6) => {
                    if let Some(trie) = &self.ip_cidr_v6 {
                        let key = Ipv6Prefix::new(v6, 128).expect("/128 always valid");
                        let (_, opt_action) = trie.lookup(&key);
                        if let Some(action) = opt_action {
                            return *action;
                        }
                    }
                }
            }
        }

        // 3. Port rules.
        if let Some(p) = port {
            if let Some(action) = self.ports.get(&p) {
                return *action;
            }
        }

        // 4. GEOIP (optional, requires feature).
        #[cfg(feature = "geoip")]
        if let (Some(addr), Some(reader)) = (ip, &self.geoip) {
            if !self.geoip_rules.is_empty() {
                if let Ok(country) = reader.lookup::<maxminddb::geoip2::Country>(addr) {
                    if let Some(iso) = country.country.and_then(|c| c.iso_code) {
                        if let Some(action) = self.geoip_rules.get(&iso.to_uppercase()) {
                            return *action;
                        }
                    }
                }
            }
        }

        // 4b. GEOIP rules present but no GeoIP DB loaded — skip.
        #[cfg(not(feature = "geoip"))]
        if !self.geoip_rules.is_empty() {
            tracing::trace!("GEOIP rules configured but geoip feature disabled");
        }

        self.final_action
    }
}

// ---------------------------------------------------------------------------
// Domain Suffix Trie
// ---------------------------------------------------------------------------

struct DomainSuffixTrie {
    nodes: HashMap<String, DomainSuffixTrie>,
    action: Option<RuleAction>,
}

impl DomainSuffixTrie {
    fn new() -> Self {
        Self {
            nodes: HashMap::new(),
            action: None,
        }
    }

    fn insert(&mut self, suffix: &str, action: RuleAction) {
        let labels: Vec<&str> = suffix.split('.').rev().collect();
        let mut node = self;
        for label in labels {
            let label = label.to_lowercase();
            node = node.nodes.entry(label).or_insert_with(Self::new);
        }
        node.action = Some(action);
    }

    fn query(&self, domain: &str) -> Option<RuleAction> {
        let labels: Vec<&str> = domain.split('.').rev().collect();
        let mut node = self;
        let mut last_action = node.action;
        for label in labels {
            match node.nodes.get(label) {
                Some(child) => {
                    node = child;
                    if node.action.is_some() {
                        last_action = node.action;
                    }
                }
                None => break,
            }
        }
        last_action
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use phantom_core::ClientRule;
    use std::net::Ipv4Addr;

    fn make_cfg(rules: Vec<(RulePattern, RuleAction)>) -> RulesConfig {
        RulesConfig {
            rules: rules
                .into_iter()
                .map(|(pattern, action)| ClientRule { pattern, action })
                .collect(),
            final_action: RuleAction::Proxy,
        }
    }

    fn make_config(rules: Vec<ClientRule>, final_action: RuleAction) -> RulesConfig {
        RulesConfig { rules, final_action }
    }

    #[test]
    fn domain_full_match() {
        let cfg = make_cfg(vec![(
            RulePattern::DomainFull {
                value: "example.com".into(),
            },
            RuleAction::Direct,
        )]);
        let engine = RuleEngine::from_config(&cfg).unwrap();
        assert_eq!(
            engine.query(Some("example.com"), None, None),
            RuleAction::Direct
        );
        assert_eq!(
            engine.query(Some("sub.example.com"), None, None),
            RuleAction::Proxy
        );
    }

    #[test]
    fn domain_suffix_match() {
        let cfg = make_cfg(vec![(
            RulePattern::DomainSuffix {
                value: "google.com".into(),
            },
            RuleAction::Direct,
        )]);
        let engine = RuleEngine::from_config(&cfg).unwrap();
        assert_eq!(
            engine.query(Some("mail.google.com"), None, None),
            RuleAction::Direct
        );
        assert_eq!(
            engine.query(Some("google.com"), None, None),
            RuleAction::Direct
        );
        assert_eq!(
            engine.query(Some("example.com"), None, None),
            RuleAction::Proxy
        );
    }

    #[test]
    fn domain_keyword_match() {
        let cfg = make_cfg(vec![(
            RulePattern::DomainKeyword {
                value: "google".into(),
            },
            RuleAction::Direct,
        )]);
        let engine = RuleEngine::from_config(&cfg).unwrap();
        assert_eq!(
            engine.query(Some("google.com"), None, None),
            RuleAction::Direct
        );
        assert_eq!(
            engine.query(Some("mail.google.co.jp"), None, None),
            RuleAction::Direct
        );
    }

    #[test]
    fn domain_regex_match() {
        let cfg = make_cfg(vec![(
            RulePattern::DomainRegex {
                value: r"^.*\.cn$".into(),
            },
            RuleAction::Direct,
        )]);
        let engine = RuleEngine::from_config(&cfg).unwrap();
        assert_eq!(
            engine.query(Some("baidu.cn"), None, None),
            RuleAction::Direct
        );
        assert_eq!(
            engine.query(Some("baidu.com"), None, None),
            RuleAction::Proxy
        );
    }

    #[test]
    fn ip_cidr_v4_match() {
        let cfg = make_cfg(vec![(
            RulePattern::IpCidr {
                value: "192.168.0.0/16".into(),
            },
            RuleAction::Direct,
        )]);
        let engine = RuleEngine::from_config(&cfg).unwrap();
        assert_eq!(
            engine.query(None, Some(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1))), None),
            RuleAction::Direct
        );
        assert_eq!(
            engine.query(None, Some(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1))), None),
            RuleAction::Proxy
        );
    }

    #[test]
    fn port_match() {
        let cfg = make_cfg(vec![(
            RulePattern::Port { value: 22 },
            RuleAction::Direct,
        )]);
        let engine = RuleEngine::from_config(&cfg).unwrap();
        assert_eq!(engine.query(None, None, Some(22)), RuleAction::Direct);
        assert_eq!(engine.query(None, None, Some(443)), RuleAction::Proxy);
    }

    #[test]
    fn rule_priority_domain_before_ip() {
        let cfg = make_cfg(vec![
            (
                RulePattern::DomainSuffix {
                    value: "example.com".into(),
                },
                RuleAction::Direct,
            ),
            (
                RulePattern::IpCidr {
                    value: "93.184.216.0/24".into(),
                },
                RuleAction::Reject,
            ),
        ]);
        let engine = RuleEngine::from_config(&cfg).unwrap();
        // Domain match wins over IP match.
        assert_eq!(
            engine.query(
                Some("example.com"),
                Some(IpAddr::V4(Ipv4Addr::new(93, 184, 216, 34))),
                None
            ),
            RuleAction::Direct
        );
    }

    #[test]
    fn empty_rules_final_proxy() {
        let cfg = make_config(vec![], RuleAction::Proxy);
        let engine = RuleEngine::from_config(&cfg).unwrap();
        assert_eq!(engine.query(None, None, None), RuleAction::Proxy);
    }

    #[test]
    fn empty_rules_final_direct() {
        let cfg = make_config(vec![], RuleAction::Direct);
        let engine = RuleEngine::from_config(&cfg).unwrap();
        assert_eq!(engine.query(None, None, None), RuleAction::Direct);
    }

    #[test]
    fn domain_full_case_insensitive() {
        let cfg = make_config(vec![
            ClientRule { pattern: RulePattern::DomainFull { value: "Google.COM".to_string() }, action: RuleAction::Proxy }
        ], RuleAction::Direct);
        let engine = RuleEngine::from_config(&cfg).unwrap();
        assert_eq!(engine.query(Some("google.com"), None, None), RuleAction::Proxy);
        assert_eq!(engine.query(Some("GOOGLE.COM"), None, None), RuleAction::Proxy);
    }

    #[test]
    fn ip_cidr_longest_prefix() {
        let cfg = make_config(vec![
            ClientRule { pattern: RulePattern::IpCidr { value: "10.0.0.0/8".to_string() }, action: RuleAction::Direct },
            ClientRule { pattern: RulePattern::IpCidr { value: "10.0.1.0/24".to_string() }, action: RuleAction::Proxy },
        ], RuleAction::Direct);
        let engine = RuleEngine::from_config(&cfg).unwrap();
        // 10.0.1.5 matches both /8 and /24, /24 is more specific
        assert_eq!(engine.query(None, Some("10.0.1.5".parse().unwrap()), None), RuleAction::Proxy);
        // 10.1.2.3 matches only /8
        assert_eq!(engine.query(None, Some("10.1.2.3".parse().unwrap()), None), RuleAction::Direct);
    }

    #[test]
    fn domain_keyword_proxy_on_match() {
        let cfg = make_config(vec![
            ClientRule { pattern: RulePattern::DomainKeyword { value: "google".to_string() }, action: RuleAction::Proxy }
        ], RuleAction::Direct);
        let engine = RuleEngine::from_config(&cfg).unwrap();
        assert_eq!(engine.query(Some("mail.google.com"), None, None), RuleAction::Proxy);
        assert_eq!(engine.query(Some("example.com"), None, None), RuleAction::Direct);
    }

    #[test]
    fn port_proxy_on_match() {
        let cfg = make_config(vec![
            ClientRule { pattern: RulePattern::Port { value: 443 }, action: RuleAction::Proxy }
        ], RuleAction::Direct);
        let engine = RuleEngine::from_config(&cfg).unwrap();
        assert_eq!(engine.query(None, None, Some(443)), RuleAction::Proxy);
        assert_eq!(engine.query(None, None, Some(80)), RuleAction::Direct);
    }

    #[test]
    fn domain_priority_over_ip() {
        let cfg = make_config(vec![
            ClientRule { pattern: RulePattern::DomainFull { value: "example.com".to_string() }, action: RuleAction::Proxy },
            ClientRule { pattern: RulePattern::IpCidr { value: "1.2.3.0/24".to_string() }, action: RuleAction::Direct },
        ], RuleAction::Direct);
        let engine = RuleEngine::from_config(&cfg).unwrap();
        // Domain match takes priority
        assert_eq!(engine.query(Some("example.com"), Some("1.2.3.4".parse().unwrap()), None), RuleAction::Proxy);
    }
}
