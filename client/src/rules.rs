//! High-performance routing rule engine for Smart / Split-Tunnel mode.
//!
//! Supports:
//! - Domain: full, suffix, keyword, regex
//! - IP-CIDR: IPv4 and IPv6 longest-prefix match
//! - Port: exact match
//! - GEOIP: country code lookup (optional feature `geoip`)
//!
//! Design goals: O(1) or O(domain-depth) query, no per-packet allocation.

use ipnet::{Ipv4Net, Ipv6Net};
use phantom_core::{PhantomError, Result, RuleAction, RulePattern, RulesConfig};
use regex::Regex;
use std::collections::HashMap;
use std::net::IpAddr;

/// Compiled rule engine built from config.
pub struct RuleEngine {
    domain_full: HashMap<String, RuleAction>,
    domain_suffix: DomainSuffixTrie,
    domain_keyword: Vec<(String, RuleAction)>,
    domain_regex: Vec<(Regex, RuleAction)>,
    ip_cidr_v4: Vec<(Ipv4Net, RuleAction)>,
    ip_cidr_v6: Vec<(Ipv6Net, RuleAction)>,
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
        let mut domain_keyword = Vec::new();
        let mut domain_regex = Vec::new();
        let mut ip_cidr_v4 = Vec::new();
        let mut ip_cidr_v6 = Vec::new();
        let mut ports = HashMap::new();
        let mut geoip_rules = HashMap::new();

        for rule in &cfg.rules {
            match &rule.pattern {
                RulePattern::DomainFull { value } => {
                    domain_full.insert(value.to_lowercase(), rule.action);
                }
                RulePattern::DomainSuffix { value } => {
                    domain_suffix.insert(value, rule.action);
                }
                RulePattern::DomainKeyword { value } => {
                    domain_keyword.push((value.to_lowercase(), rule.action));
                }
                RulePattern::DomainRegex { value } => {
                    let re = Regex::new(value)
                        .map_err(|e| PhantomError::Config(format!("Invalid domain regex: {}", e)))?;
                    domain_regex.push((re, rule.action));
                }
                RulePattern::IpCidr { value } => {
                    if let Ok(net) = value.parse::<Ipv4Net>() {
                        ip_cidr_v4.push((net, rule.action));
                    } else if let Ok(net) = value.parse::<Ipv6Net>() {
                        ip_cidr_v6.push((net, rule.action));
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

        // Sort CIDR lists so longest prefix (most specific) is checked first.
        ip_cidr_v4.sort_by_key(|(net, _)| std::cmp::Reverse(net.prefix_len()));
        ip_cidr_v6.sort_by_key(|(net, _)| std::cmp::Reverse(net.prefix_len()));

        #[cfg(feature = "geoip")]
        let geoip = None;

        Ok(Self {
            domain_full,
            domain_suffix,
            domain_keyword,
            domain_regex,
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
            for (kw, action) in &self.domain_keyword {
                if d.contains(kw) {
                    return *action;
                }
            }
            for (re, action) in &self.domain_regex {
                if re.is_match(&d) {
                    return *action;
                }
            }
        }

        // 2. IP-CIDR rules.
        if let Some(addr) = ip {
            match addr {
                IpAddr::V4(v4) => {
                    for (net, action) in &self.ip_cidr_v4 {
                        if net.contains(&v4) {
                            return *action;
                        }
                    }
                }
                IpAddr::V6(v6) => {
                    for (net, action) in &self.ip_cidr_v6 {
                        if net.contains(&v6) {
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

    fn make_cfg(rules: Vec<(RulePattern, RuleAction)>) -> RulesConfig {
        RulesConfig {
            rules: rules
                .into_iter()
                .map(|(pattern, action)| ClientRule { pattern, action })
                .collect(),
            final_action: RuleAction::Proxy,
        }
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
}
