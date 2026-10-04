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

/// Name of the kernel ipset holding whitelisted destinations.
pub const DEFAULT_IPSET_NAME: &str = "phantom_proxy";

/// Firewall mark applied to traffic that must enter the tunnel.
pub const DEFAULT_FWMARK: u32 = 0x1;

/// How long an ipset entry learned from a DNS answer stays valid.
///
/// Long enough that a CDN node the app resolved once keeps working for the
/// whole session, short enough that a recycled address does not keep pulling
/// unrelated traffic into the tunnel. 30 min matches the DNS reverse-cache
/// retention, and sits inside the 10 min – 24 h band the design calls for.
pub const IPSET_ENTRY_TTL_SECS: u32 = 30 * 60;

/// How LAN traffic reaches the tunnel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GatewayMode {
    /// Only destinations the kernel knows about (whitelist ipset) enter the
    /// TUN; everything else stays on the kernel fast path, where Broadcom's
    /// Runner/Flow Cache can offload it. This is the default because relaying
    /// *all* LAN traffic through user space caps the whole house at roughly
    /// what one CPU core can copy (~100–150 Mbps on a BCM4912).
    #[default]
    KernelSplit,
    /// Every forwarded packet enters the TUN and is judged in user space.
    /// Slower, but it also catches destinations that were dialled by IP
    /// without ever asking this router's DNS. Kept as the fallback for
    /// firmware without `ipset`/`xt_set`.
    TunRelay,
}

impl GatewayMode {
    pub fn as_str(self) -> &'static str {
        match self {
            GatewayMode::KernelSplit => "kernel-split",
            GatewayMode::TunRelay => "relay",
        }
    }

    /// Accepts the CLI spellings plus a couple of friendly aliases.
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "kernel-split" | "kernel" | "split" | "ipset" => Some(GatewayMode::KernelSplit),
            "relay" | "tun-relay" | "userspace" | "user-space" => Some(GatewayMode::TunRelay),
            _ => None,
        }
    }
}

impl std::fmt::Display for GatewayMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

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
    /// Payload piped to the command's stdin (used to seed the ipset in one
    /// `ipset restore` call instead of one fork per entry).
    pub stdin: Option<String>,
    /// When set, nothing is spawned: the value is written to this procfs/sysfs
    /// path. Needed because the firmware this runs on has **no `sysctl`
    /// binary** — `sysctl -w net.ipv4.conf.tun0.rp_filter=0` fails silently and
    /// the tunnel then drops every reply it injects (reverse-path filter).
    pub write_path: Option<String>,
    pub write_value: Option<String>,
}

impl GatewayCommand {
    fn new(line: &str, tolerate_failure: bool) -> Self {
        let mut parts = line.split_whitespace().map(str::to_string);
        let program = parts.next().unwrap_or_default();
        Self {
            program,
            args: parts.collect(),
            tolerate_failure,
            stdin: None,
            write_path: None,
            write_value: None,
        }
    }

    fn required_with_stdin(line: &str, payload: String) -> Self {
        let mut cmd = Self::new(line, false);
        cmd.stdin = Some(payload);
        cmd
    }

    /// A procfs/sysfs write, expressed as a plan step so it is testable.
    fn write(path: &str, value: &str) -> Self {
        Self {
            program: String::new(),
            args: Vec::new(),
            tolerate_failure: true,
            stdin: None,
            write_path: Some(path.to_string()),
            write_value: Some(value.to_string()),
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
        if let (Some(path), Some(value)) = (&self.write_path, &self.write_value) {
            format!("write {}={}", path, value)
        } else if self.args.is_empty() {
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
    /// How much of the LAN's traffic is handed to the TUN.
    pub mode: GatewayMode,
    /// ipset that carries the whitelisted destinations (kernel-split mode).
    pub ipset_name: String,
    /// Firewall mark applied to tunnel-bound packets (kernel-split mode).
    pub fwmark: u32,
    /// IPv4 CIDRs seeded into the ipset at startup. Built from the compiled
    /// whitelist data, so services that are only reachable by IP (Telegram,
    /// and Google/YouTube via its published ranges) work before any DNS query
    /// has been observed.
    pub ipset_seed_cidrs: Vec<String>,
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
            mode: GatewayMode::default(),
            ipset_name: DEFAULT_IPSET_NAME.to_string(),
            fwmark: DEFAULT_FWMARK,
            ipset_seed_cidrs: crate::whitelist::builtin_ipv4_cidrs(),
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

        // Forwarding must be on.
        cmds.push(GatewayCommand::write(
            "/proc/sys/net/ipv4/ip_forward",
            "1",
        ));
        // Reverse-path filtering must not drop the tunnel's replies: they
        // arrive on the TUN with a public source address, so a strict check
        // (which would send them out the WAN) rejects them. The kernel uses
        // `max(all, <iface>)`, so clearing the interface alone is not enough.
        //
        // Written straight to /proc/sys on purpose: this firmware ships **no
        // `sysctl` binary**, and the old `sysctl -w` step failed silently —
        // every DNS answer the tunnel injected was dropped, which looked like
        // "the plugin broke DNS". `all=2` is the loose mode (source must be
        // routable somewhere), which keeps the WAN's anti-spoofing meaningful
        // while allowing the asymmetric tunnel path.
        cmds.push(GatewayCommand::write(
            &format!("/proc/sys/net/ipv4/conf/{}/rp_filter", self.tun_name),
            "0",
        ));
        cmds.push(GatewayCommand::write(
            "/proc/sys/net/ipv4/conf/all/rp_filter",
            "2",
        ));

        // Default route for the tunnel lives in its own table.
        cmds.push(GatewayCommand::best_effort(&format!(
            "ip route flush table {}",
            self.table_id
        )));
        cmds.push(GatewayCommand::required(&format!(
            "ip route add default dev {} table {}",
            self.tun_name, self.table_id
        )));

        match self.mode {
            GatewayMode::TunRelay => self.plan_relay(&mut cmds),
            GatewayMode::KernelSplit => self.plan_kernel_split(&mut cmds),
        }

        Ok(cmds)
    }

    /// Legacy mode: every message from the LAN enters the TUN and is judged in
    /// user space. Correct everywhere, but it puts the whole house behind one
    /// user-space relay (and keeps the hardware flow cache from offloading).
    fn plan_relay(&self, cmds: &mut Vec<GatewayCommand>) {
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
    }

    /// Default mode: mark only whitelisted destinations into the tunnel.
    ///
    /// The kernel does the split *before* the packet reaches the TUN, so a
    /// direct flow never leaves the forwarding path the hardware accelerates:
    ///
    /// ```text
    ///   LAN ──▶ mangle PREROUTING ──▶ ip rule fwmark 0x1 ──▶ table 200 ──▶ phantom0 ──▶ (user space)
    ///                    │
    ///                    └── not marked ──▶ main table ──▶ WAN (hardware fast path)
    /// ```
    fn plan_kernel_split(&self, cmds: &mut Vec<GatewayCommand>) {
        let name = &self.ipset_name;
        let ttl = IPSET_ENTRY_TTL_SECS;

        // `-exist` keeps a restart idempotent; the entry timeout means a stale
        // address stops pulling traffic into the tunnel on its own.
        cmds.push(GatewayCommand::required(&format!(
            "ipset create {} hash:net family inet timeout {} -exist",
            name, ttl
        )));

        if !self.ipset_seed_cidrs.is_empty() {
            let mut payload = String::new();
            for cidr in &self.ipset_seed_cidrs {
                // `timeout 0` = permanent. The published ranges (Telegram,
                // Google/YouTube) do not change minute to minute, and letting
                // them expire after 30 minutes would silently break exactly
                // the apps they exist for (a player dialling a googlevideo IP
                // it resolved earlier).
                payload.push_str(&format!("add {} {} timeout 0 -exist\n", name, cidr));
            }
            // `-!` ignores per-line errors so one odd range cannot abort the
            // whole seed; a genuinely broken ipset still fails on `create`.
            cmds.push(GatewayCommand::required_with_stdin(
                "ipset -! restore",
                payload,
            ));
        }

        // Marked packets take the tunnel table; everything else stays in main.
        cmds.push(GatewayCommand::required(&format!(
            "ip rule add fwmark {} lookup {} priority {}",
            self.fwmark, self.table_id, TUNNEL_RULE_PRIORITY
        )));

        for iface in &self.lan_interfaces {
            cmds.push(GatewayCommand::best_effort(&format!(
                "iptables -I FORWARD -i {} -o {} -j ACCEPT",
                iface, self.tun_name
            )));
            cmds.push(GatewayCommand::best_effort(&format!(
                "iptables -I FORWARD -i {} -o {} -j ACCEPT",
                self.tun_name, iface
            )));

            if self.lan_dns_hijack {
                // DNS is the learning channel: every query has to reach the
                // TUN proxy, otherwise whitelisted domains never make it into
                // the ipset and their connections stay on the direct path.
                for proto in ["udp", "tcp"] {
                    cmds.push(GatewayCommand::required(&format!(
                        "iptables -t mangle -I PREROUTING -i {} -p {} --dport 53 -j MARK --set-mark {}",
                        iface, proto, self.fwmark
                    )));
                    cmds.push(GatewayCommand::best_effort(&format!(
                        "iptables -t nat -I PREROUTING -i {} -p {} --dport 53 -j DNAT --to-destination {}:53",
                        iface, proto, self.dns_sentinel
                    )));
                }
            }

            cmds.push(GatewayCommand::required(&format!(
                "iptables -t mangle -I PREROUTING -i {} -m set --match-set {} dst -j MARK --set-mark {}",
                iface, name, self.fwmark
            )));
        }
    }

    /// Build the ordered list of commands that removes the gateway.
    ///
    /// Every step is best-effort: teardown runs on the error path too, where
    /// some rules may never have been installed.
    pub fn teardown_plan(&self) -> Vec<GatewayCommand> {
        let mut cmds = Vec::new();

        // Both modes' rules are removed: switching modes or upgrading must not
        // leave the previous split behind. Everything here is best-effort.
        for iface in &self.lan_interfaces {
            if self.lan_dns_hijack {
                for proto in ["udp", "tcp"] {
                    cmds.push(GatewayCommand::best_effort(&format!(
                        "iptables -t nat -D PREROUTING -i {} -p {} --dport 53 -j DNAT --to-destination {}:53",
                        iface, proto, self.dns_sentinel
                    )));
                    cmds.push(GatewayCommand::best_effort(&format!(
                        "iptables -t mangle -D PREROUTING -i {} -p {} --dport 53 -j MARK --set-mark {}",
                        iface, proto, self.fwmark
                    )));
                }
            }
            cmds.push(GatewayCommand::best_effort(&format!(
                "iptables -t mangle -D PREROUTING -i {} -m set --match-set {} dst -j MARK --set-mark {}",
                iface, self.ipset_name, self.fwmark
            )));
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
            "ip rule del fwmark {} lookup {} priority {}",
            self.fwmark, self.table_id, TUNNEL_RULE_PRIORITY
        )));
        cmds.push(GatewayCommand::best_effort(&format!(
            "ip route flush table {}",
            self.table_id
        )));
        cmds.push(GatewayCommand::best_effort(&format!(
            "ipset destroy {}",
            self.ipset_name
        )));

        cmds
    }
}

/// An installed gateway. Dropping it reverts every change.
pub struct Gateway {
    config: GatewayConfig,
    /// Mode actually in force. Kernel-split falls back to the relay when the
    /// firmware cannot provide `ipset` or the `set` match.
    effective_mode: GatewayMode,
    installed: bool,
    /// Set when the requested kernel-split install had to fall back, so the
    /// runtime can surface "why is this slower than expected?" without log
    /// spelunking.
    fallback_reason: Option<String>,
}

impl Gateway {
    /// Install the gateway plumbing.
    ///
    /// A pre-emptive teardown runs first so a previous crashed run cannot leave
    /// duplicate `ip rule` entries behind.
    pub fn install(config: GatewayConfig) -> Result<Self> {
        let requested = config.mode;
        match Self::try_install(config.clone()) {
            Ok(gateway) => Ok(gateway),
            Err(e) if requested == GatewayMode::KernelSplit => {
                // Kernel split needs `ipset` plus the `set` iptables match.
                // Older or cut-down firmware may have neither; the relay mode
                // works everywhere, so degrade instead of refusing to start.
                tracing::warn!(
                    "kernel-split gateway unavailable ({}); falling back to user-space relay mode",
                    e
                );
                let mut relay = config;
                relay.mode = GatewayMode::TunRelay;
                let mut gateway = Self::try_install(relay)?;
                gateway.fallback_reason = Some(e.to_string());
                Ok(gateway)
            }
            Err(e) => Err(e),
        }
    }

    fn try_install(config: GatewayConfig) -> Result<Self> {
        let plan = config.plan()?;

        let mut gateway = Self {
            effective_mode: config.mode,
            config,
            fallback_reason: None,
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
            "Gateway installed: mode={} dev={} table={} lan={:?} dns_hijack={} ipset={}",
            gateway.config.mode,
            gateway.config.tun_name,
            gateway.config.table_id,
            gateway.config.lan_interfaces,
            gateway.config.lan_dns_hijack,
            gateway.config.ipset_name
        );
        gateway.verify_rp_filter();
        Ok(gateway)
    }

    /// A strict reverse-path filter on the TUN drops every reply the tunnel
    /// injects, which presents as "the plugin broke DNS and browsing". The
    /// write above is best-effort (procfs can be read-only), so check what the
    /// kernel actually ended up with and say so.
    fn verify_rp_filter(&self) {
        let read = |path: String| {
            std::fs::read_to_string(path)
                .ok()
                .and_then(|v| v.trim().parse::<i32>().ok())
        };
        let dev = read(format!(
            "/proc/sys/net/ipv4/conf/{}/rp_filter",
            self.config.tun_name
        ));
        let all = read("/proc/sys/net/ipv4/conf/all/rp_filter".to_string());
        if let (Some(dev), Some(all)) = (dev, all) {
            // The kernel validates against max(all, iface): -1 means "unset".
            if dev.max(all) == 1 {
                tracing::warn!(
                    "gateway: reverse-path filter is still strict ({}={}, all={}); replies injected into \
                     {} will be dropped — tunnel DNS and proxied traffic will look broken",
                    self.config.tun_name,
                    dev,
                    all,
                    self.config.tun_name
                );
            }
        }
    }

    /// Mode actually in force (after any fallback).
    pub fn effective_mode(&self) -> GatewayMode {
        self.effective_mode
    }

    /// Name of the whitelist ipset, when the kernel-split mode is in force.
    pub fn ipset_name(&self) -> Option<&str> {
        match self.effective_mode {
            GatewayMode::KernelSplit => Some(self.config.ipset_name.as_str()),
            GatewayMode::TunRelay => None,
        }
    }

    /// How many destinations the gateway seeded into the ipset itself (the
    /// published CIDR list). The publisher adds its own learned entries on top.
    pub fn ipset_seed_entries(&self) -> u64 {
        self.config.ipset_seed_cidrs.len() as u64
    }

    /// Populated when kernel-split was requested but relay had to be used.
    pub fn fallback_reason(&self) -> Option<&str> {
        self.fallback_reason.as_deref()
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
    if let (Some(path), Some(value)) = (&cmd.write_path, &cmd.write_value) {
        return match std::fs::write(path, format!("{}\n", value)) {
            Ok(()) => Ok(()),
            Err(e) => {
                if cmd.tolerate_failure {
                    tracing::debug!("gateway: cannot write {}: {}", path, e);
                    Ok(())
                } else {
                    Err(PhantomError::Config(format!(
                        "cannot write {}: {}",
                        path, e
                    )))
                }
            }
        };
    }
    let output = match &cmd.stdin {
        None => Command::new(&cmd.program).args(&cmd.args).output(),
        Some(payload) => {
            use std::io::Write;
            use std::process::Stdio;
            Command::new(&cmd.program)
                .args(&cmd.args)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .and_then(|mut child| {
                    if let Some(mut stdin) = child.stdin.take() {
                        stdin.write_all(payload.as_bytes())?;
                    }
                    child.wait_with_output()
                })
        }
    };
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

    /// The legacy mode used to be the only one; several tests pin its exact
    /// rule set, so they ask for it explicitly.
    fn relay_config() -> GatewayConfig {
        GatewayConfig {
            mode: GatewayMode::TunRelay,
            ..GatewayConfig::default()
        }
    }

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
        let lines = plan_lines(&relay_config());
        // `iif`-based selection is what keeps router-local tunnel sockets from
        // looping back into the TUN.
        assert!(lines.contains(&"ip rule add iif br0 lookup 200 priority 9050".to_string()));
        assert!(lines.iter().all(|l| !l.contains("ip rule add from")));
    }

    #[test]
    fn bypass_rules_sort_before_the_tunnel_rule() {
        let lines = plan_lines(&relay_config());
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
            ..relay_config()
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
        // /proc/sys 直写：本机固件没有 sysctl 二进制，用 sysctl -w 会静默失败，
        // 结果是隧道注入的回包全被反向路径校验丢掉（表现为"插件把 DNS 弄坏了"）。
        assert!(
            lines.contains(&"write /proc/sys/net/ipv4/conf/phantomX/rp_filter=0".to_string()),
            "the TUN's rp_filter must be off or tunnel replies are dropped: {lines:#?}"
        );
        // 内核按 max(all, iface) 判定，只清 iface 不够。
        assert!(
            lines.contains(&"write /proc/sys/net/ipv4/conf/all/rp_filter=2".to_string()),
            "all.rp_filter must be relaxed to loose mode as well"
        );
        assert!(
            lines.iter().all(|l| !l.contains("sysctl")),
            "sysctl is not present on the target firmware; write /proc/sys directly"
        );
    }

    #[test]
    fn write_steps_are_rendered_and_tolerated() {
        let cmd = GatewayCommand::write("/proc/sys/net/ipv4/ip_forward", "1");
        assert_eq!(cmd.display(), "write /proc/sys/net/ipv4/ip_forward=1");
        assert!(cmd.tolerate_failure, "a read-only procfs must not fail startup");
    }

    #[test]
    fn default_bypass_set_covers_rfc1918_and_loopback() {
        let lines = plan_lines(&relay_config());
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

    // ---- kernel-split (default) ------------------------------------------

    #[test]
    fn default_mode_is_kernel_split() {
        assert_eq!(GatewayConfig::default().mode, GatewayMode::KernelSplit);
    }

    #[test]
    fn kernel_split_marks_only_whitelisted_destinations() {
        let lines = plan_lines(&GatewayConfig::default());
        assert!(
            lines.iter().any(|l| l
                == "iptables -t mangle -I PREROUTING -i br0 -m set --match-set phantom_proxy dst -j MARK --set-mark 1"),
            "whitelist match rule missing: {lines:#?}"
        );
        assert!(lines.contains(&"ip rule add fwmark 1 lookup 200 priority 9050".to_string()));
        // The catch-all that made every packet visit user space must be gone.
        assert!(
            lines.iter().all(|l| !l.contains("ip rule add iif br0 lookup 200")),
            "kernel-split must not route every LAN packet into the TUN"
        );
        // …and the private-range bypass is unnecessary once selection is by mark.
        assert!(lines.iter().all(|l| !l.contains("lookup main priority 9040")));
    }

    #[test]
    fn kernel_split_keeps_dns_on_the_tunnel_path() {
        // DNS is how whitelisted domains become ipset entries; if queries took
        // the direct path the domain would never be learned.
        let lines = plan_lines(&GatewayConfig::default());
        for proto in ["udp", "tcp"] {
            assert!(
                lines.iter().any(|l| l.contains(&format!(
                    "-i br0 -p {proto} --dport 53 -j MARK --set-mark 1"
                ))),
                "DNS mark rule missing for {proto}"
            );
        }
    }

    #[test]
    fn kernel_split_creates_and_seeds_the_ipset_in_one_call() {
        let plan = GatewayConfig::default().plan().unwrap();
        assert!(plan
            .iter()
            .any(|c| c.display().contains("ipset create phantom_proxy hash:net family inet")));

        let restore = plan
            .iter()
            .find(|c| c.display() == "ipset -! restore")
            .expect("ipset restore step missing");
        let payload = restore.stdin.as_deref().expect("seed payload missing");
        assert!(payload.contains("add phantom_proxy "));
        // 种子必须是永久条目：它们不在 DNS 学习路径上，一旦过期就再没人补回来，
        // 而它们存在的意义恰恰是"玩家直接对着 googlevideo IP 建连"。
        assert!(
            payload.lines().all(|l| l.contains("timeout 0")),
            "seeded published ranges must not expire"
        );
        assert!(payload.lines().count() > 10, "built-in CIDRs were not seeded");
    }

    #[test]
    fn teardown_destroys_the_ipset_and_both_modes_rules() {
        let config = GatewayConfig::default();
        let teardown: Vec<String> = config
            .teardown_plan()
            .iter()
            .map(GatewayCommand::display)
            .collect();
        assert!(teardown.contains(&"ipset destroy phantom_proxy".to_string()));
        assert!(teardown.iter().any(|l| l.starts_with("ip rule del fwmark 1 ")));
        assert!(teardown
            .iter()
            .any(|l| l.contains("-m set --match-set phantom_proxy dst -j MARK")));
        // Relay-mode leftovers are cleaned too, so switching modes is safe.
        assert!(teardown.contains(&"ip rule del iif br0 lookup 200 priority 9050".to_string()));
    }

    #[test]
    fn mode_parsing_accepts_cli_and_alias_spellings() {
        assert_eq!(GatewayMode::parse("kernel-split"), Some(GatewayMode::KernelSplit));
        assert_eq!(GatewayMode::parse("KERNEL"), Some(GatewayMode::KernelSplit));
        assert_eq!(GatewayMode::parse("relay"), Some(GatewayMode::TunRelay));
        assert_eq!(GatewayMode::parse("userspace"), Some(GatewayMode::TunRelay));
        assert_eq!(GatewayMode::parse("nonsense"), None);
    }
}
