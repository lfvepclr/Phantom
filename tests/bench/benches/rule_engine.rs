//! Rule engine baseline benchmark (current implementation).
//!
//! Measures the per-query latency of [`RuleEngine`] under various rule-set sizes
//! and pattern types. Use this as the baseline before optimizing — when an
//! alternative (daachorse, iptrie, RegexSet, …) is introduced, the same
//! scenarios can be re-run for an apples-to-apples comparison.
//!
//! Run with:
//!
//! ```bash
//! cargo bench -p phantom-bench --bench rule_engine
//! ```
//!
//! All rule data is generated programmatically so the benchmark is reproducible
//! and free of large external fixtures. The pattern vocabulary reflects common
//! Chinese / international smart-tunnel rule sets.

use divan::Bencher;
use phantom_client::rules::RuleEngine;
use phantom_core::{ClientRule, RuleAction, RulePattern, RulesConfig};
use std::net::{IpAddr, Ipv4Addr};

// ---------------------------------------------------------------------------
// Vocabulary — patterns we feed into the engine and queries we run against it
// ---------------------------------------------------------------------------

const DOMAIN_SUFFIX_BASE: &[&str] = &[
    "cn",
    "com.cn",
    "org.cn",
    "net.cn",
    "edu.cn",
    "gov.cn",
    "com",
    "net",
    "org",
    "io",
    "dev",
    "app",
    "co",
    "me",
    "xyz",
    "top",
    "vip",
    "club",
    "info",
    "biz",
    "google.com",
    "youtube.com",
    "facebook.com",
    "twitter.com",
    "instagram.com",
    "baidu.com",
    "qq.com",
    "taobao.com",
    "jd.com",
    "weibo.com",
    "bilibili.com",
    "zhihu.com",
    "douyin.com",
    "github.com",
    "microsoft.com",
    "apple.com",
    "amazon.com",
    "cloudflare.com",
    "wikipedia.org",
];

const DOMAIN_KEYWORD_BASE: &[&str] = &[
    "google",
    "baidu",
    "facebook",
    "twitter",
    "instagram",
    "youtube",
    "tiktok",
    "amazon",
    "microsoft",
    "apple",
    "github",
    "gitlab",
    "bitbucket",
    "stackoverflow",
    "reddit",
    "twitch",
    "discord",
    "telegram",
    "signal",
    "whatsapp",
    "ads",
    "tracker",
    "adnxs",
    "doubleclick",
    "googlesyndication",
    "scorecardresearch",
    "crashlytics",
    "appsflyer",
    "umeng",
    "tencent",
    "alibaba",
    "bytedance",
    "ant",
    "meituan",
    "didi",
    "ctrip",
    "wechat",
    "alipay",
];

const DOMAIN_REGEX_BASE: &[&str] = &[
    r".*\.cn$",
    r".*\.com\.cn$",
    r".*\bads?\b.*",
    r".*\btrack(er|ing)?\b.*",
    r"^[a-z0-9-]+\.googlevideo\.com$",
    r".*\.doubleclick\.net$",
    r".*\.googlesyndication\.com$",
    r".*\.scorecardresearch\.com$",
    r".*\.crashlytics\.com$",
    r".*\.umeng\.com$",
];

const IP_CIDR_V4_BASE: &[&str] = &[
    "10.0.0.0/8",
    "172.16.0.0/12",
    "192.168.0.0/16",
    "100.64.0.0/10",
    "127.0.0.0/8",
    "169.254.0.0/16",
    "224.0.0.0/4",
    "240.0.0.0/4",
    "8.8.8.0/24",
    "1.1.1.0/24",
    "9.9.9.0/24",
    "114.114.114.0/24",
    "223.5.5.0/24",
    "119.29.29.0/24",
    "180.76.76.0/24",
    "202.96.0.0/12",
    "203.208.0.0/12",
    "210.0.0.0/8",
    "218.0.0.0/8",
    "222.0.0.0/8",
];

const PORTS: &[u16] = &[
    22, 53, 80, 123, 135, 137, 138, 139, 161, 389, 443, 445, 465, 500, 514, 587, 636, 873, 902,
    989, 990, 993, 995, 1080, 1194, 1433, 1521, 1701, 1723, 1812, 1900, 2049, 2082, 2083, 2086,
    2087, 2095, 2096, 2181, 2375, 2376, 3000, 3306, 3389, 4500, 4848, 5000, 5060, 5432, 5601, 5672,
    5900, 5984, 6379, 6443, 7001, 7474, 8000, 8080, 8081, 8443, 8500, 8888, 9000, 9092, 9200, 9418,
    11211, 15672, 26379, 27017, 50070,
];

const QUERY_DOMAINS: &[&str] = &[
    "google.com",
    "www.google.com",
    "ads.google.com",
    "tracker.example.com",
    "sub.deep.nested.example.org",
    "baidu.com",
    "www.baidu.com",
    "youtube.com",
    "github.com",
    "raw.githubusercontent.com",
    "wikipedia.org",
    "en.wikipedia.org",
    "shop.taobao.com",
    "mall.jd.com",
    "api.bilibili.com",
    "foo.bar.cn",
    "weibo.com",
    "doubleclick.net",
    "scorecardresearch.com",
    "telegram.org",
];

const QUERY_IPS_V4: &[IpAddr] = &[
    IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)),
    IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1)),
    IpAddr::V4(Ipv4Addr::new(114, 114, 114, 114)),
    IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1)),
    IpAddr::V4(Ipv4Addr::new(10, 0, 1, 5)),
    IpAddr::V4(Ipv4Addr::new(202, 96, 209, 5)),
    IpAddr::V4(Ipv4Addr::new(218, 85, 152, 99)),
];

const QUERY_PORTS: &[u16] = &[22, 53, 80, 443, 3306, 5432, 6379, 8080, 8443];

// ---------------------------------------------------------------------------
// Rule-set builders
// ---------------------------------------------------------------------------

/// Build N suffix rules by cycling the base vocabulary.
fn build_suffix_rules(n: usize) -> Vec<ClientRule> {
    let mut rules = Vec::with_capacity(n);
    for i in 0..n {
        let value = DOMAIN_SUFFIX_BASE[i % DOMAIN_SUFFIX_BASE.len()];
        // Alternate action to keep behavior varied.
        let action = if i % 3 == 0 {
            RuleAction::Direct
        } else {
            RuleAction::Proxy
        };
        rules.push(ClientRule {
            pattern: RulePattern::DomainSuffix {
                value: value.to_string(),
            },
            action,
        });
    }
    rules
}

fn build_keyword_rules(n: usize) -> Vec<ClientRule> {
    let mut rules = Vec::with_capacity(n);
    for i in 0..n {
        // Each keyword must be unique — daachorse rejects duplicate patterns,
        // and this also mirrors production rule sets where each line is a
        // distinct pattern. We append the index to a base keyword stem.
        let stem = DOMAIN_KEYWORD_BASE[i % DOMAIN_KEYWORD_BASE.len()];
        let value = if i < DOMAIN_KEYWORD_BASE.len() {
            stem.to_string()
        } else {
            format!("{}_{}", stem, i)
        };
        let action = if i % 2 == 0 {
            RuleAction::Proxy
        } else {
            RuleAction::Direct
        };
        rules.push(ClientRule {
            pattern: RulePattern::DomainKeyword { value },
            action,
        });
    }
    rules
}

fn build_regex_rules(n: usize) -> Vec<ClientRule> {
    let mut rules = Vec::with_capacity(n);
    for i in 0..n {
        let value = DOMAIN_REGEX_BASE[i % DOMAIN_REGEX_BASE.len()];
        let action = if i % 2 == 0 {
            RuleAction::Reject
        } else {
            RuleAction::Direct
        };
        rules.push(ClientRule {
            pattern: RulePattern::DomainRegex {
                value: value.to_string(),
            },
            action,
        });
    }
    rules
}

fn build_cidr_rules(n: usize) -> Vec<ClientRule> {
    let mut rules = Vec::with_capacity(n);
    for i in 0..n {
        let value = IP_CIDR_V4_BASE[i % IP_CIDR_V4_BASE.len()];
        let action = if i % 2 == 0 {
            RuleAction::Direct
        } else {
            RuleAction::Proxy
        };
        rules.push(ClientRule {
            pattern: RulePattern::IpCidr {
                value: value.to_string(),
            },
            action,
        });
    }
    rules
}

fn build_port_rules(n: usize) -> Vec<ClientRule> {
    let mut rules = Vec::with_capacity(n);
    for i in 0..n {
        let value = PORTS[i % PORTS.len()];
        rules.push(ClientRule {
            pattern: RulePattern::Port { value },
            action: RuleAction::Proxy,
        });
    }
    rules
}

/// Realistic mixed rule set: domain + IP + port in expected proportion.
fn build_mixed_rules(n: usize) -> Vec<ClientRule> {
    let suffix_n = n / 2;
    let keyword_n = n / 4;
    let cidr_n = n / 8;
    let port_n = n - suffix_n - keyword_n - cidr_n;
    let mut rules = Vec::with_capacity(n);
    rules.extend(build_suffix_rules(suffix_n));
    rules.extend(build_keyword_rules(keyword_n));
    rules.extend(build_cidr_rules(cidr_n));
    rules.extend(build_port_rules(port_n));
    rules
}

fn engine(rules: Vec<ClientRule>, final_action: RuleAction) -> RuleEngine {
    let cfg = RulesConfig {
        rules,
        final_action,
    };
    RuleEngine::from_config(&cfg).expect("valid rules")
}

// ---------------------------------------------------------------------------
// Benchmarks
// ---------------------------------------------------------------------------

/// End-to-end `query()` against a domain-only rule set of varying size.
#[divan::bench(args = [10, 100, 1000, 10_000])]
fn query_domain_mixed(bencher: Bencher, n: usize) {
    let engine = engine(build_mixed_rules(n), RuleAction::Proxy);
    bencher.bench_local(|| {
        let mut acc = 0u8;
        for d in QUERY_DOMAINS {
            let action = engine.query(Some(d), None, None);
            acc = acc.wrapping_add(action as u8);
        }
        acc
    });
}

/// Suffix-only rule set.
#[divan::bench(args = [10, 100, 1000, 10_000])]
fn query_suffix_only(bencher: Bencher, n: usize) {
    let engine = engine(build_suffix_rules(n), RuleAction::Proxy);
    bencher.bench_local(|| {
        let mut acc = 0u8;
        for d in QUERY_DOMAINS {
            acc = acc.wrapping_add(engine.query(Some(d), None, None) as u8);
        }
        acc
    });
}

/// Keyword-only rule set (the worst-case today).
#[divan::bench(args = [10, 100, 1000, 10_000])]
fn query_keyword_only(bencher: Bencher, n: usize) {
    let engine = engine(build_keyword_rules(n), RuleAction::Proxy);
    bencher.bench_local(|| {
        let mut acc = 0u8;
        for d in QUERY_DOMAINS {
            acc = acc.wrapping_add(engine.query(Some(d), None, None) as u8);
        }
        acc
    });
}

/// Regex-only rule set (the second-worst case today).
#[divan::bench(args = [10, 100, 1000, 10_000])]
fn query_regex_only(bencher: Bencher, n: usize) {
    let engine = engine(build_regex_rules(n), RuleAction::Proxy);
    bencher.bench_local(|| {
        let mut acc = 0u8;
        for d in QUERY_DOMAINS {
            acc = acc.wrapping_add(engine.query(Some(d), None, None) as u8);
        }
        acc
    });
}

/// CIDR-only rule set, queried with a mix of public and private IPs.
#[divan::bench(args = [10, 100, 1000, 10_000])]
fn query_cidr_only(bencher: Bencher, n: usize) {
    let engine = engine(build_cidr_rules(n), RuleAction::Proxy);
    bencher.bench_local(|| {
        let mut acc = 0u8;
        for ip in QUERY_IPS_V4 {
            acc = acc.wrapping_add(engine.query(None, Some(*ip), None) as u8);
        }
        acc
    });
}

/// Port-only rule set.
#[divan::bench(args = [10, 100, 1000, 10_000])]
fn query_port_only(bencher: Bencher, n: usize) {
    let engine = engine(build_port_rules(n), RuleAction::Proxy);
    bencher.bench_local(|| {
        let mut acc = 0u8;
        for p in QUERY_PORTS {
            acc = acc.wrapping_add(engine.query(None, None, Some(*p)) as u8);
        }
        acc
    });
}

/// Most realistic scenario: domain + IP + port queried per packet.
#[divan::bench(args = [10, 100, 1000, 10_000])]
fn query_realistic(bencher: Bencher, n: usize) {
    let engine = engine(build_mixed_rules(n), RuleAction::Proxy);
    bencher.bench_local(|| {
        let mut acc = 0u8;
        // Each iteration simulates one packet that has both a domain and an IP.
        for (d, ip) in QUERY_DOMAINS.iter().zip(QUERY_IPS_V4.iter().cycle()) {
            acc = acc.wrapping_add(engine.query(Some(d), Some(*ip), Some(443)) as u8);
        }
        acc
    });
}

/// Worst-case for mixed rules: domain intentionally *misses* the suffix trie
/// so the query has to descend into keyword and regex paths, exercising the
/// AC automaton in real conditions.
#[divan::bench(args = [10, 100, 1000, 10_000])]
fn query_mixed_no_suffix_hit(bencher: Bencher, n: usize) {
    let engine = engine(build_mixed_rules(n), RuleAction::Proxy);
    // Random-looking domains that won't match any suffix rule.
    let probe_domains = [
        "randomsite12345.xyz",
        "unmatched-domain-name.io",
        "fallback-bucket.app",
        "no-rule-here.dev",
        "cold-path-host.net",
        "no-match-foo.bar",
    ];
    bencher.bench_local(|| {
        let mut acc = 0u8;
        for (d, ip) in probe_domains
            .iter()
            .cycle()
            .take(20)
            .zip(QUERY_IPS_V4.iter().cycle())
        {
            acc = acc.wrapping_add(engine.query(Some(d), Some(*ip), Some(443)) as u8);
        }
        acc
    });
}

/// Cold-path: building a `RuleEngine` from a fully-populated config.
#[divan::bench(args = [10, 100, 1000, 10_000])]
fn build_engine_mixed(bencher: Bencher, n: usize) {
    let rules = build_mixed_rules(n);
    let cfg = RulesConfig {
        rules,
        final_action: RuleAction::Proxy,
    };
    bencher.bench_local(|| {
        // Clone is cheap relative to the parse, but we want to measure parse.
        let cfg = cfg.clone();
        RuleEngine::from_config(&cfg).unwrap()
    });
}

fn main() {
    divan::main();
}
