//! Property-based equivalence test: the optimized `RuleEngine` (AC + iptrie +
//! RegexSet) must produce the same routing action as a faithful
//! "reference" re-implementation of the original (HashMap / Vec iteration
//! based) engine for every (rules, query) pair we can throw at it.
//!
//! This is the safety net for the A+B+C optimization campaign: any future
//! refactor that breaks semantic equivalence will fail here before reaching
//! production traffic.

use ipnet::Ipv4Net;
use phantom_client::RuleEngine;
use phantom_core::{ClientRule, RuleAction, RulePattern, RulesConfig};
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use regex::Regex;
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr};

// ---------------------------------------------------------------------------
// Reference implementation (mirrors the pre-A+B+C behaviour line by line).
// Deliberately kept in this test file so it never accidentally diverges from
// the production engine.
// ---------------------------------------------------------------------------

#[derive(Default)]
struct RefEngine {
    domain_full: HashMap<String, RuleAction>,
    domain_suffix: HashMap<String, RefSuffixTrie>,
    domain_keyword: Vec<(String, RuleAction)>,
    domain_regex: Vec<(Regex, RuleAction)>,
    ip_cidr_v4: Vec<(Ipv4Net, RuleAction)>,
    ports: HashMap<u16, RuleAction>,
    final_action: RuleAction,
}

struct RefSuffixTrie {
    children: HashMap<String, RefSuffixTrie>,
    action: Option<RuleAction>,
}

impl RefSuffixTrie {
    fn new() -> Self {
        Self {
            children: HashMap::new(),
            action: None,
        }
    }

    fn insert(&mut self, suffix: &str, action: RuleAction) {
        let labels: Vec<&str> = suffix.split('.').rev().collect();
        let mut node = self;
        for label in labels {
            let label = label.to_lowercase();
            node = node.children.entry(label).or_insert_with(Self::new);
        }
        node.action = Some(action);
    }

    fn query(&self, domain: &str) -> Option<RuleAction> {
        let labels: Vec<&str> = domain.split('.').rev().collect();
        let mut node = self;
        let mut last_action = node.action;
        for label in labels {
            match node.children.get(label) {
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

impl RefEngine {
    fn from_config(cfg: &RulesConfig) -> Self {
        let mut out = Self {
            final_action: cfg.final_action,
            ..Self::default()
        };
        for rule in &cfg.rules {
            match &rule.pattern {
                RulePattern::DomainFull { value } => {
                    out.domain_full.insert(value.to_lowercase(), rule.action);
                }
                RulePattern::DomainSuffix { value } => {
                    out.domain_suffix
                        .entry(String::new()) // single root placeholder
                        .or_insert_with(RefSuffixTrie::new)
                        .insert(value, rule.action);
                }
                RulePattern::DomainKeyword { value } => {
                    out.domain_keyword.push((value.to_lowercase(), rule.action));
                }
                RulePattern::DomainRegex { value } => {
                    if let Ok(re) = Regex::new(value) {
                        out.domain_regex.push((re, rule.action));
                    }
                }
                RulePattern::IpCidr { value } => {
                    if let Ok(net) = value.parse::<Ipv4Net>() {
                        out.ip_cidr_v4.push((net, rule.action));
                    }
                }
                RulePattern::Port { value } => {
                    out.ports.insert(*value, rule.action);
                }
                _ => {
                    // GeoIp / Final are not exercised by the fuzz generator.
                }
            }
        }
        // Match production: longest prefix first.
        out.ip_cidr_v4
            .sort_by_key(|(net, _)| std::cmp::Reverse(net.prefix_len()));
        out
    }

    fn query(&self, domain: Option<&str>, ip: Option<IpAddr>, port: Option<u16>) -> RuleAction {
        if let Some(d) = domain {
            let d = d.to_lowercase();
            if let Some(action) = self.domain_full.get(&d) {
                return *action;
            }
            // The reference model has a single root trie under the empty key.
            if let Some(root) = self.domain_suffix.get("") {
                if let Some(action) = root.query(&d) {
                    return action;
                }
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
        if let Some(addr) = ip {
            if let IpAddr::V4(v4) = addr {
                for (net, action) in &self.ip_cidr_v4 {
                    if net.contains(&v4) {
                        return *action;
                    }
                }
            }
        }
        if let Some(p) = port {
            if let Some(action) = self.ports.get(&p) {
                return *action;
            }
        }
        self.final_action
    }
}

// ---------------------------------------------------------------------------
// Random rule + query generators
// ---------------------------------------------------------------------------

const DOMAIN_TLDS: &[&str] = &["com", "net", "org", "io", "cn", "app", "dev"];
const DOMAIN_NAMES: &[&str] = &[
    "google",
    "baidu",
    "facebook",
    "twitter",
    "github",
    "apple",
    "amazon",
    "microsoft",
    "youtube",
    "tiktok",
    "wechat",
    "taobao",
    "jd",
    "bilibili",
    "zhihu",
    "douyin",
    "tencent",
    "alibaba",
    "bytedance",
    "meituan",
    "didi",
    "ctrip",
    "example",
    "wikipedia",
    "reddit",
];
const KEYWORD_STEMS: &[&str] = &[
    "ad",
    "ads",
    "tracker",
    "tracking",
    "metric",
    "telemetry",
    "crash",
    "analytics",
    "doubleclick",
    "scorecard",
    "googlesyndication",
    "adnxs",
    "facebook",
    "google",
    "baidu",
    "qq",
    "wechat",
    "alipay",
    "meituan",
    "tiktok",
    "bytedance",
];
const IPV4_RANGES: &[&str] = &[
    "10.0.0.0/8",
    "172.16.0.0/12",
    "192.168.0.0/16",
    "8.8.8.0/24",
    "1.1.1.0/24",
    "9.9.9.0/24",
    "114.114.114.0/24",
    "223.5.5.0/24",
    "119.29.29.0/24",
    "202.96.0.0/12",
    "203.208.0.0/12",
    "180.76.76.0/24",
    "100.64.0.0/10",
    "127.0.0.0/8",
];
const REGEX_TEMPLATES: &[&str] = &[
    r".*\.cn$",
    r"^.*google.*$",
    r".*tracker.*",
    r"^api\..*",
    r".*\.doubleclick\.net$",
    r".*\bads?\b.*",
];

fn gen_domain(rng: &mut StdRng, depth: usize) -> String {
    let name = DOMAIN_NAMES[rng.gen_range(0..DOMAIN_NAMES.len())];
    let tld = DOMAIN_TLDS[rng.gen_range(0..DOMAIN_TLDS.len())];
    let mut s = format!("{name}.{tld}");
    for _ in 1..depth {
        let prefix = DOMAIN_NAMES[rng.gen_range(0..DOMAIN_NAMES.len())];
        s = format!("{prefix}.{s}");
    }
    s
}

fn gen_ruleset(rng: &mut StdRng) -> RulesConfig {
    let n: usize = rng.gen_range(0..50);
    let mut rules = Vec::with_capacity(n);
    for _ in 0..n {
        let action = match rng.gen_range(0..3) {
            0 => RuleAction::Direct,
            1 => RuleAction::Proxy,
            _ => RuleAction::Reject,
        };
        let pattern = match rng.gen_range(0..6) {
            0 => RulePattern::DomainFull {
                value: gen_domain(rng, 1),
            },
            1 => RulePattern::DomainSuffix {
                value: gen_domain(rng, 1),
            },
            2 => {
                // Append index to guarantee uniqueness — daachorse rejects
                // duplicate patterns and the production engine must too.
                let stem = KEYWORD_STEMS[rng.gen_range(0..KEYWORD_STEMS.len())];
                let value = format!("{stem}_{}", rules.len());
                RulePattern::DomainKeyword { value }
            }
            3 => {
                // A small whitelist of patterns that always compile.
                let value = REGEX_TEMPLATES[rng.gen_range(0..REGEX_TEMPLATES.len())].to_string();
                RulePattern::DomainRegex { value }
            }
            4 => RulePattern::IpCidr {
                value: IPV4_RANGES[rng.gen_range(0..IPV4_RANGES.len())].to_string(),
            },
            _ => RulePattern::Port {
                value: [22, 53, 80, 443, 3306, 8080][rng.gen_range(0..6)],
            },
        };
        rules.push(ClientRule { pattern, action });
    }
    let final_action = match rng.gen_range(0..3) {
        0 => RuleAction::Direct,
        1 => RuleAction::Proxy,
        _ => RuleAction::Reject,
    };
    RulesConfig {
        rules,
        final_action,
    }
}

fn gen_query_domain(rng: &mut StdRng) -> String {
    // Bias: roughly half queries are "well-formed" subdomain chains; the
    // other half are short or odd-shaped so they exercise the no-match path.
    if rng.gen_bool(0.5) {
        let depth: usize = rng.gen_range(1..4);
        gen_domain(rng, depth)
    } else {
        let name_idx: usize = rng.gen_range(0..DOMAIN_NAMES.len());
        DOMAIN_NAMES[name_idx].to_string()
    }
}

fn gen_query_ip(rng: &mut StdRng) -> IpAddr {
    IpAddr::V4(Ipv4Addr::new(
        rng.gen_range(0..=255),
        rng.gen_range(0..=255),
        rng.gen_range(0..=255),
        rng.gen_range(0..=255),
    ))
}

fn gen_query_port(rng: &mut StdRng) -> u16 {
    [22, 53, 80, 443, 3306, 8080, 1, 65535][rng.gen_range(0..8)]
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Run the production engine and the in-file reference engine against the
/// same (rules, query) inputs and assert the actions are identical.
///
/// Three randomized regimes are exercised: domain-only, IP-only, and the
/// realistic "domain + IP + port" packet shape.
#[test]
fn fuzz_equivalence_random_rules_and_queries() {
    let mut rng = StdRng::seed_from_u64(0xA1B2_C3D4);
    let iterations: usize = 200;
    let queries_per_cfg: usize = 32;

    let mut total = 0usize;
    for i in 0..iterations {
        let cfg = gen_ruleset(&mut rng);
        let prod = RuleEngine::from_config(&cfg).expect("production engine build");
        let reference = RefEngine::from_config(&cfg);

        for _ in 0..queries_per_cfg {
            // Three query shapes per iteration; each tested independently so
            // a mismatch can be pinned to a specific code path.
            let domain = gen_query_domain(&mut rng);
            let ip = gen_query_ip(&mut rng);
            let port = gen_query_port(&mut rng);

            // (1) domain-only
            let p1 = prod.query(Some(&domain), None, None);
            let r1 = reference.query(Some(&domain), None, None);
            assert_eq!(
                p1, r1,
                "iteration {i} domain-only mismatch on {domain:?}: prod={p1:?} ref={r1:?}"
            );

            // (2) ip-only
            let p2 = prod.query(None, Some(ip), None);
            let r2 = reference.query(None, Some(ip), None);
            assert_eq!(
                p2, r2,
                "iteration {i} ip-only mismatch on {ip}: prod={p2:?} ref={r2:?}"
            );

            // (3) realistic
            let p3 = prod.query(Some(&domain), Some(ip), Some(port));
            let r3 = reference.query(Some(&domain), Some(ip), Some(port));
            assert_eq!(
                p3, r3,
                "iteration {i} realistic mismatch on (d={domain:?}, ip={ip}, port={port}): prod={p3:?} ref={r3:?}\n  cfg: final_action={:?}, rules={:#?}",
                cfg.final_action, cfg.rules
            );

            total += 3;
        }
    }
    assert_eq!(total, iterations * queries_per_cfg * 3);
}

/// Boundary cases that specifically stress the AC automaton, RegexSet, and
/// LC-trie. These are the rules that historically differed in priority
/// (multiple matches, longest-prefix IP, etc.).
#[test]
fn fuzz_priority_edge_cases() {
    let mut rng = StdRng::seed_from_u64(0xDEAD_BEEF);

    // Build a ruleset with overlapping suffix + keyword + regex patterns
    // for the same domain.
    for i in 0..50 {
        let mut rules = Vec::new();
        let base = gen_domain(&mut rng, 1);
        rules.push(ClientRule {
            pattern: RulePattern::DomainFull {
                value: base.clone(),
            },
            action: RuleAction::Direct,
        });
        rules.push(ClientRule {
            pattern: RulePattern::DomainSuffix {
                value: base.clone(),
            },
            action: RuleAction::Proxy,
        });
        rules.push(ClientRule {
            pattern: RulePattern::DomainKeyword {
                value: format!("kw_{i}"),
            },
            action: RuleAction::Reject,
        });
        rules.push(ClientRule {
            pattern: RulePattern::DomainRegex {
                value: r".*\.com$".to_string(),
            },
            action: RuleAction::Direct,
        });
        // Overlapping CIDRs: a /24 and a /16 sharing the same address.
        rules.push(ClientRule {
            pattern: RulePattern::IpCidr {
                value: "10.0.0.0/16".to_string(),
            },
            action: RuleAction::Direct,
        });
        rules.push(ClientRule {
            pattern: RulePattern::IpCidr {
                value: "10.0.1.0/24".to_string(),
            },
            action: RuleAction::Reject,
        });
        let cfg = RulesConfig {
            rules,
            final_action: RuleAction::Proxy,
        };
        let prod = RuleEngine::from_config(&cfg).unwrap();
        let reference = RefEngine::from_config(&cfg);

        let queries: Vec<(String, IpAddr)> = vec![
            (base.clone(), "10.0.1.5".parse().unwrap()),
            (format!("sub.{base}"), "10.0.2.5".parse().unwrap()),
            (format!("kw_{i}.example.org"), "8.8.8.8".parse().unwrap()),
            ("unrelated-name.io".to_string(), "1.1.1.1".parse().unwrap()),
        ];
        for (d, ip) in &queries {
            let p = prod.query(Some(d), Some(*ip), None);
            let r = reference.query(Some(d), Some(*ip), None);
            assert_eq!(
                p, r,
                "edge-case iter {i} on (d={d:?}, ip={ip}): prod={p:?} ref={r:?}"
            );
        }
    }
}

/// Property: an empty rule set always returns `final_action` regardless of
/// the input. Verifies the LC-trie root + AC + RegexSet default paths all
/// short-circuit correctly.
#[test]
fn empty_ruleset_returns_final_action() {
    for action in [RuleAction::Direct, RuleAction::Proxy, RuleAction::Reject] {
        let cfg = RulesConfig {
            rules: vec![],
            final_action: action,
        };
        let prod = RuleEngine::from_config(&cfg).unwrap();
        let reference = RefEngine::from_config(&cfg);
        for d in ["", "a", "sub.example.com", "X".repeat(64).as_str()] {
            assert_eq!(prod.query(Some(d), None, None), action);
            assert_eq!(reference.query(Some(d), None, None), action);
        }
        for ip_str in ["0.0.0.0", "255.255.255.255", "127.0.0.1"] {
            let ip: IpAddr = ip_str.parse().unwrap();
            assert_eq!(prod.query(None, Some(ip), None), action);
            assert_eq!(reference.query(None, Some(ip), None), action);
        }
    }
}
