pub mod dns;
pub mod failover;
#[cfg(target_os = "linux")]
pub mod gateway;
pub mod hello;
pub mod http_proxy;
pub mod net_tune;
pub mod platform;
pub mod quic_pool;
pub mod rules;
pub mod socks5;
pub mod stats;
pub mod tcp_pool;
pub mod tun;
pub mod tun_trace;
pub mod tunnel;
pub mod udp_relay;
pub mod whitelist;

/// Append a line to the opt-in TUN trace (`tun_trace::set_path`). Formats
/// nothing while tracing is off.
#[macro_export]
macro_rules! tun_trace {
    ($($arg:tt)*) => {
        $crate::tun_trace::log(format_args!($($arg)*))
    };
}

pub use rules::RuleEngine;
pub use stats::TrafficStats;
pub use tunnel::{PhantomClient, TunRuntimeOptions};
pub use whitelist::{ProxyWhitelist, RouteDecision, RouteReason};
