//! Linux gateway plumbing for router deployments (e.g. ASUS RT-AX86U Pro).
//!
//! On a router the TUN device alone is not enough: the kernel has to be told to
//! push *forwarded* LAN traffic into it. This module installs that plumbing and
//! tears it down again on exit.
//!
//! Design notes:
//! - Routing is selected by **incoming interface** (`ip rule iif br0`) rather
//!   than by source subnet. Router-local traffic — including Phantom's own
//!   tunnel sockets and dnsmasq — has no `iif`, so it never matches and can
//!   never loop back into the TUN. This removes the need to special-case the
//!   server IP or the WAN gateway.
//! - Bypass CIDRs are expressed as higher-priority rules that fall back to the
//!   `main` table, keeping LAN↔LAN and LAN→router traffic intact.
//! - Command construction is separated from execution ([`GatewayConfig::plan`]
//!   / [`GatewayConfig::teardown_plan`]) so the exact rule set is unit-testable
//!   without root or a live interface.

use phantom_core::{PhantomError, Result};
use std::net::Ipv4Addr;
use std::process::Command;

/// RFC1918 + loopback + link-local + multicast + broadcast.
///
/// Traffic to these destinations must keep using the router's `main` table,
/// otherwise LAN clients lose access to each other and to the router itself.
pub const DEFAULT_BYPASS_CIDRS: &[&str] = &[
    "0.0.0.0/8",
    "10.0.0.0/8",
    "127.0.0.0/8",
    "169.254.0.0/16",
    "172.16.0.0/12",
    "192.168.0.0/16",
    "224.0.0.0/4",
    "240.0.0.0/4",
];

/// Default routing table id used for the tunnel default route.
pub const DEFAULT_TABLE_ID: u32 = 200;

/// Priority of the bypass rules (must sort before the catch-all rule).
const BYPASS_RULE_PRIORITY: u32 = 9040;
/// Priority of the catch-all "send LAN traffic to the tunnel" rule.
const TUNNEL_RULE_PRIORITY: u32 = 9050;

/// A single external command in a gateway plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayCommand {
    pub program: String,
    pub args: Vec<String>,
    /// When true a non-zero exit status is logged but does not abort the plan.
    /// Used for idempotent add/delete steps that legitimately fail when the
    /// rule is already present / already gone.
    pub tolerate_failure: bool,
}

impl GatewayCommand {
    fn new(line: &str, tolerate_failure: bool) -> Self {
        let mut parts = line.split_whitespace().map(str::to_string);
        let program = parts.next().unwrap_or_default();
        Self {
            program,
            args: parts.collect(),
            tolerate_failure,
        }
    }

    fn required(line: &str) -> Self {
        Self::new(line, false)
    }

    fn best_effort(line: &str) -> Self {
        Self::new(line, true)
    }

    /// Render back to a shell-ish string, for logging and test assertions.
    pub fn display(&self) -> String {
        if self.args.is_empty() {
            self.program.clone()
        } else {
            format!("{} {}", self.program, self.args.join(" "))
        }
    }
}

/// Gateway settings for a Linux router.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayConfig {
    /// TUN interface the default route points at.
    pub tun_name: String,
    /// TUN local address, used as the DNS hijack target.
    pub tun_addr: Ipv4Addr,
    /// LAN-side interfaces whose *forwarded* traffic should be tunnelled.
    pub lan_interfaces: Vec<String>,
    /// Destinations that keep using the `main` routing table.
    pub bypass_cidrs: Vec<String>,
    /// Routing table id holding the tunnel default route.
    pub table_id: u32,
    /// Redirect LAN port-53 traffic into the tunnel so the TUN DNS hijack sees
    /// it. Without this, LAN clients resolve via the router's dnsmasq and
    /// domain-based rules never match.
    pub lan_dns_hijack: bool,
    /// Resolver LAN DNS queries are rewritten to when `lan_dns_hijack` is on.
    /// It only has to be a routable address outside the bypass set — the query
    /// is intercepted by the TUN DNS proxy before it ever leaves the router.
    pub dns_sentinel: Ipv4Addr,
}

impl Default for GatewayConfig {
    fn default() -> Self {
        Self {
            tun_name: crate::tun::DEFAULT_TUN_NAME.to_string(),
            tun_addr: Ipv4Addr::new(10, 7, 0, 1),
            lan_interfaces: vec!["br0".to_string()],
            bypass_cidrs: DEFAULT_BYPASS_CIDRS.iter().map(|s| s.to_string()).collect(),
            table_id: DEFAULT_TABLE_ID,
            lan_dns_hijack: true,
            dns_sentinel: Ipv4Addr::new(8, 8, 8, 8),
        }
    }
}

impl GatewayConfig {
    fn validate(&self) -> Result<()> {
        if self.tun_name.is_empty() {
            return Err(PhantomError::Config(
                "gateway: TUN interface name must not be empty".to_string(),
            ));
        }
        if self.lan_interfaces.is_empty() {
            return Err(PhantomError::Config(
                "gateway: at least one --lan-interface is required".to_string(),
            ));
        }
        if self.table_id == 0
            || self.table_id == 253
            || self.table_id == 254
            || self.table_id == 255
        {
            return Err(PhantomError::Config(format!(
                "gateway: routing table {} is reserved (default/main/local)",
                self.table_id
            )));
        }
        Ok(())
    }

    /// Build the ordered list of commands that installs the gateway.
    pub fn plan(&self) -> Result<Vec<GatewayCommand>> {
        self.validate()?;
        let mut cmds = Vec::new();

        // Forwarding must be on, and reverse-path filtering off for the TUN:
        // replies arrive on an interface the kernel would not have chosen.
        cmds.push(GatewayCommand::best_effort(
            "sysctl -w net.ipv4.ip_forward=1",
        ));
        cmds.push(GatewayCommand::best_effort(&format!(
            "sysctl -w net.ipv4.conf.{}.rp_filter=0",
            self.tun_name
        )));

        // Default route for the tunnel lives in its own table.
        cmds.push(GatewayCommand::best_effort(&format!(
            "ip route flush table {}",
            self.table_id
        )));
        cmds.push(GatewayCommand::required(&format!(
            "ip route add default dev {} table {}",
            self.tun_name, self.table_id
        )));

        for iface in &self.lan_interfaces {
            // Bypass first, so private destinations resolve via `main`.
            for cidr in &self.bypass_cidrs {
                cmds.push(GatewayCommand::required(&format!(
                    "ip rule add iif {} to {} lookup main priority {}",
                    iface, cidr, BYPASS_RULE_PRIORITY
                )));
            }
            cmds.push(GatewayCommand::required(&format!(
                "ip rule add iif {} lookup {} priority {}",
                iface, self.table_id, TUNNEL_RULE_PRIORITY
            )));

            cmds.push(GatewayCommand::best_effort(&format!(
                "iptables -I FORWARD -i {} -o {} -j ACCEPT",
                iface, self.tun_name
            )));
            cmds.push(GatewayCommand::best_effort(&format!(
                "iptables -I FORWARD -i {} -o {} -j ACCEPT",
                self.tun_name, iface
            )));

            if self.lan_dns_hijack {
                for proto in ["udp", "tcp"] {
                    cmds.push(GatewayCommand::best_effort(&format!(
                        "iptables -t nat -I PREROUTING -i {} -p {} --dport 53 -j DNAT --to-destination {}:53",
                        iface, proto, self.dns_sentinel
                    )));
                }
            }
        }

        Ok(cmds)
    }

    /// Build the ordered list of commands that removes the gateway.
    ///
    /// Every step is best-effort: teardown runs on the error path too, where
    /// some rules may never have been installed.
    pub fn teardown_plan(&self) -> Vec<GatewayCommand> {
        let mut cmds = Vec::new();

        for iface in &self.lan_interfaces {
            if self.lan_dns_hijack {
                for proto in ["udp", "tcp"] {
                    cmds.push(GatewayCommand::best_effort(&format!(
                        "iptables -t nat -D PREROUTING -i {} -p {} --dport 53 -j DNAT --to-destination {}:53",
                        iface, proto, self.dns_sentinel
                    )));
                }
            }
            cmds.push(GatewayCommand::best_effort(&format!(
                "iptables -D FORWARD -i {} -o {} -j ACCEPT",
                self.tun_name, iface
            )));
            cmds.push(GatewayCommand::best_effort(&format!(
                "iptables -D FORWARD -i {} -o {} -j ACCEPT",
                iface, self.tun_name
            )));

            cmds.push(GatewayCommand::best_effort(&format!(
                "ip rule del iif {} lookup {} priority {}",
                iface, self.table_id, TUNNEL_RULE_PRIORITY
            )));
            for cidr in &self.bypass_cidrs {
                cmds.push(GatewayCommand::best_effort(&format!(
                    "ip rule del iif {} to {} lookup main priority {}",
                    iface, cidr, BYPASS_RULE_PRIORITY
                )));
            }
        }

        cmds.push(GatewayCommand::best_effort(&format!(
            "ip route flush table {}",
            self.table_id
        )));

        cmds
    }
}

/// An installed gateway. Dropping it reverts every change.
pub struct Gateway {
    config: GatewayConfig,
    installed: bool,
}

impl Gateway {
    /// Install the gateway plumbing.
    ///
    /// A pre-emptive teardown runs first so a previous crashed run cannot leave
    /// duplicate `ip rule` entries behind.
    pub fn install(config: GatewayConfig) -> Result<Self> {
        let plan = config.plan()?;

        let mut gateway = Self {
            config,
            installed: true,
        };
        gateway.run_plan(&gateway.config.teardown_plan());

        for cmd in &plan {
            if let Err(e) = run_command(cmd) {
                tracing::error!("Gateway install failed at `{}`: {}", cmd.display(), e);
                gateway.uninstall();
                return Err(e);
            }
        }

        tracing::info!(
            "Gateway installed: dev={} table={} lan={:?} dns_hijack={}",
            gateway.config.tun_name,
            gateway.config.table_id,
            gateway.config.lan_interfaces,
            gateway.config.lan_dns_hijack
        );
        Ok(gateway)
    }

    fn run_plan(&self, plan: &[GatewayCommand]) {
        for cmd in plan {
            let _ = run_command(cmd);
        }
    }

    /// Revert every change. Idempotent.
    pub fn uninstall(&mut self) {
        if !self.installed {
            return;
        }
        self.installed = false;
        let plan = self.config.teardown_plan();
        for cmd in &plan {
            let _ = run_command(cmd);
        }
        tracing::info!("Gateway removed: dev={}", self.config.tun_name);
    }
}

impl Drop for Gateway {
    fn drop(&mut self) {
        self.uninstall();
    }
}

fn run_command(cmd: &GatewayCommand) -> Result<()> {
    tracing::debug!("gateway: {}", cmd.display());
    let output = Command::new(&cmd.program).args(&cmd.args).output();
    match output {
        Ok(out) if out.status.success() => Ok(()),
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
            if cmd.tolerate_failure {
                tracing::debug!("gateway: `{}` returned non-zero: {}", cmd.display(), stderr);
                Ok(())
            } else {
                Err(PhantomError::Config(format!(
                    "`{}` failed: {}",
                    cmd.display(),
                    stderr
                )))
            }
        }
        Err(e) => {
            if cmd.tolerate_failure {
                tracing::debug!("gateway: `{}` not executable: {}", cmd.display(), e);
                Ok(())
            } else {
                Err(PhantomError::Config(format!(
                    "`{}` could not be executed: {} (is iproute2 installed?)",
                    cmd.display(),
                    e
                )))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan_lines(config: &GatewayConfig) -> Vec<String> {
        config
            .plan()
            .unwrap()
            .iter()
            .map(GatewayCommand::display)
            .collect()
    }

    #[test]
    fn plan_adds_default_route_in_dedicated_table() {
        let lines = plan_lines(&GatewayConfig::default());
        assert!(lines.contains(&"ip route add default dev phantom0 table 200".to_string()));
    }

    #[test]
    fn plan_selects_traffic_by_incoming_interface() {
        let lines = plan_lines(&GatewayConfig::default());
        // `iif`-based selection is what keeps router-local tunnel sockets from
        // looping back into the TUN.
        assert!(lines.contains(&"ip rule add iif br0 lookup 200 priority 9050".to_string()));
        assert!(lines.iter().all(|l| !l.contains("ip rule add from")));
    }

    #[test]
    fn bypass_rules_sort_before_the_tunnel_rule() {
        let lines = plan_lines(&GatewayConfig::default());
        let bypass = lines
            .iter()
            .position(|l| l.contains("to 192.168.0.0/16 lookup main"))
            .expect("bypass rule missing");
        let tunnel = lines
            .iter()
            .position(|l| l == "ip rule add iif br0 lookup 200 priority 9050")
            .expect("tunnel rule missing");
        assert!(bypass < tunnel);
        assert!(BYPASS_RULE_PRIORITY < TUNNEL_RULE_PRIORITY);
    }

    #[test]
    fn plan_covers_every_lan_interface() {
        let config = GatewayConfig {
            lan_interfaces: vec!["br0".into(), "br1".into()],
            ..GatewayConfig::default()
        };
        let lines = plan_lines(&config);
        assert!(lines.contains(&"ip rule add iif br0 lookup 200 priority 9050".to_string()));
        assert!(lines.contains(&"ip rule add iif br1 lookup 200 priority 9050".to_string()));
    }

    #[test]
    fn dns_hijack_can_be_disabled() {
        let with = plan_lines(&GatewayConfig::default());
        assert!(with.iter().any(|l| l.contains("--dport 53 -j DNAT")));

        let without = plan_lines(&GatewayConfig {
            lan_dns_hijack: false,
            ..GatewayConfig::default()
        });
        assert!(without.iter().all(|l| !l.contains("DNAT")));
    }

    #[test]
    fn teardown_mirrors_every_added_rule() {
        let config = GatewayConfig::default();
        let teardown: Vec<String> = config
            .teardown_plan()
            .iter()
            .map(GatewayCommand::display)
            .collect();

        for added in plan_lines(&config) {
            // sysctl / flush steps have no delete counterpart.
            if !added.contains(" add ") && !added.contains(" -I ") {
                continue;
            }
            let removed = added.replace(" add ", " del ").replace(" -I ", " -D ");
            assert!(
                teardown.contains(&removed),
                "teardown missing `{}`",
                removed
            );
        }
    }

    #[test]
    fn teardown_is_entirely_best_effort() {
        // Teardown also runs on the install error path, where some rules were
        // never created: a hard failure there would mask the real error.
        assert!(
            GatewayConfig::default()
                .teardown_plan()
                .iter()
                .all(|c| c.tolerate_failure)
        );
    }

    #[test]
    fn reserved_routing_tables_are_rejected() {
        for table in [0, 253, 254, 255] {
            let config = GatewayConfig {
                table_id: table,
                ..GatewayConfig::default()
            };
            assert!(config.plan().is_err(), "table {} should be rejected", table);
        }
    }

    #[test]
    fn empty_lan_interface_list_is_rejected() {
        let config = GatewayConfig {
            lan_interfaces: Vec::new(),
            ..GatewayConfig::default()
        };
        assert!(config.plan().is_err());
    }

    #[test]
    fn empty_tun_name_is_rejected() {
        let config = GatewayConfig {
            tun_name: String::new(),
            ..GatewayConfig::default()
        };
        assert!(config.plan().is_err());
    }

    #[test]
    fn rp_filter_is_disabled_for_the_tun_device() {
        let config = GatewayConfig {
            tun_name: "phantomX".into(),
            ..GatewayConfig::default()
        };
        let lines = plan_lines(&config);
        assert!(
            lines.contains(&"sysctl -w net.ipv4.conf.phantomX.rp_filter=0".to_string()),
            "rp_filter must be off or tunnel replies are dropped"
        );
    }

    #[test]
    fn default_bypass_set_covers_rfc1918_and_loopback() {
        let lines = plan_lines(&GatewayConfig::default());
        for cidr in [
            "10.0.0.0/8",
            "172.16.0.0/12",
            "192.168.0.0/16",
            "127.0.0.0/8",
        ] {
            assert!(
                lines.iter().any(|l| l.contains(cidr)),
                "bypass set missing {}",
                cidr
            );
        }
    }

    #[test]
    fn command_parsing_splits_program_and_args() {
        let cmd = GatewayCommand::required("ip route add default dev phantom0 table 200");
        assert_eq!(cmd.program, "ip");
        assert_eq!(cmd.args.len(), 7);
        assert!(!cmd.tolerate_failure);
        assert_eq!(cmd.display(), "ip route add default dev phantom0 table 200");
    }
}
