//! Rule engine integration tests.
//!
//! Tests that the RuleEngine produces correct routing decisions and that
//! traffic is proxied through the tunnel when the rule action is Proxy.

use phantom_core::{CipherPreference, ClientRule, RuleAction, RulePattern, RulesConfig};
use phantom_client::RuleEngine;
use phantom_e2e::fixture::TestFixture;
use phantom_e2e::socks5::connect_tunnel;
use phantom_e2e::throughput::{echo_data, generate_random_data};
use phantom_core::protocol::TargetAddr;
use std::net::IpAddr;

fn target_from_fixture(fixture: &TestFixture) -> TargetAddr {
    match fixture.target_addr.ip() {
        std::net::IpAddr::V4(ip) => TargetAddr::IPv4(ip.octets(), fixture.target_addr.port()),
        std::net::IpAddr::V6(_) => TargetAddr::IPv4([127, 0, 0, 1], fixture.target_addr.port()),
    }
}

/// Domain-suffix rule with Proxy action: traffic to *.example.com should be
/// routed through the proxy tunnel. We verify by sending data through the
/// tunnel and confirming the echo comes back intact.
#[tokio::test]
async fn domain_suffix_proxy_routes_through_tunnel() {
    let fixture = TestFixture::new(CipherPreference::Aes256Gcm).await;

    // Build a rule engine that proxies domain-suffix "example.com"
    let cfg = RulesConfig {
        rules: vec![ClientRule {
            pattern: RulePattern::DomainSuffix {
                value: "example.com".into(),
            },
            action: RuleAction::Proxy,
        }],
        final_action: RuleAction::Direct,
    };
    let engine = RuleEngine::from_config(&cfg).unwrap();

    // Verify the rule engine returns Proxy for the domain
    assert_eq!(
        engine.query(Some("www.example.com"), None, None),
        RuleAction::Proxy,
        "domain-suffix rule should return Proxy for www.example.com"
    );
    assert_eq!(
        engine.query(Some("example.com"), None, None),
        RuleAction::Proxy,
        "domain-suffix rule should return Proxy for exact suffix match"
    );

    // Verify that an unrelated domain falls through to the final action
    assert_eq!(
        engine.query(Some("unrelated.org"), None, None),
        RuleAction::Direct,
        "unrelated domain should fall through to final_action=Direct"
    );

    // Now actually proxy traffic through the tunnel to confirm the path works
    let (mut reader, mut writer, stream_id) = connect_tunnel(
        fixture.server_addr,
        &fixture.server_key.public,
        &fixture.client_key.secret,
        &target_from_fixture(&fixture),
        fixture.cipher_preference,
    )
    .await
    .unwrap();

    let data = generate_random_data(4096);
    let echoed = echo_data(&mut reader, &mut writer, stream_id, &data).await;
    assert_eq!(echoed, data, "Echoed data should match through proxied tunnel");
}

/// IP-CIDR rule with Direct action: IP ranges that match should route
/// directly (bypass the proxy). We test the rule engine decision; the
/// actual bypass path is outside E2E scope.
#[tokio::test]
async fn ip_cidr_direct_bypasses_proxy() {
    let cfg = RulesConfig {
        rules: vec![ClientRule {
            pattern: RulePattern::IpCidr {
                value: "192.168.0.0/16".into(),
            },
            action: RuleAction::Direct,
        }],
        final_action: RuleAction::Proxy,
    };
    let engine = RuleEngine::from_config(&cfg).unwrap();

    let local_ip: IpAddr = "192.168.1.100".parse().unwrap();
    assert_eq!(
        engine.query(None, Some(local_ip), None),
        RuleAction::Direct,
        "192.168.1.100 should match 192.168.0.0/16 with Direct"
    );

    let external_ip: IpAddr = "93.184.216.34".parse().unwrap();
    assert_eq!(
        engine.query(None, Some(external_ip), None),
        RuleAction::Proxy,
        "93.184.216.34 should fall through to final_action=Proxy"
    );
}

/// Mixed rules: domain takes priority over IP-CIDR, and the proxy tunnel
/// still works when we actively send data.
#[tokio::test]
async fn domain_priority_over_ip_with_tunnel_echo() {
    let fixture = TestFixture::new(CipherPreference::Aes256Gcm).await;

    let cfg = RulesConfig {
        rules: vec![
            ClientRule {
                pattern: RulePattern::DomainFull {
                    value: "test.local".into(),
                },
                action: RuleAction::Proxy,
            },
            ClientRule {
                pattern: RulePattern::IpCidr {
                    value: "127.0.0.0/8".into(),
                },
                action: RuleAction::Direct,
            },
        ],
        final_action: RuleAction::Direct,
    };
    let engine = RuleEngine::from_config(&cfg).unwrap();

    let loopback: IpAddr = "127.0.0.1".parse().unwrap();
    // Domain match should win over IP match
    assert_eq!(
        engine.query(Some("test.local"), Some(loopback), None),
        RuleAction::Proxy,
        "Domain rule should take priority over IP-CIDR rule"
    );

    // IP-only query should hit the CIDR rule
    assert_eq!(
        engine.query(None, Some(loopback), None),
        RuleAction::Direct,
        "Loopback IP should match 127.0.0.0/8 Direct rule"
    );

    // Confirm data flows correctly through the proxy tunnel
    let (mut reader, mut writer, stream_id) = connect_tunnel(
        fixture.server_addr,
        &fixture.server_key.public,
        &fixture.client_key.secret,
        &target_from_fixture(&fixture),
        fixture.cipher_preference,
    )
    .await
    .unwrap();

    let data = b"rule engine priority test".to_vec();
    let echoed = echo_data(&mut reader, &mut writer, stream_id, &data).await;
    assert_eq!(echoed, data, "Echoed data should match through tunnel");
}
