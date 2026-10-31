//! User-configured DNS servers for reqwest.
//!
//! When the daemon hands the plugin a `NetworkConfig` with `dns` servers,
//! name resolution goes to those servers only (UDP, with TCP-grade UDP
//! framing — a truncated answer surfaces as an error). The system resolver is
//! not consulted, so a working server list is the user's responsibility and
//! failures surface as download errors instead of silent fallbacks.

use std::io;
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use aether_plugin_sdk::{IpMode, NetworkConfig};
use hickory_resolver::config::{ConnectionConfig, NameServerConfig, ResolverConfig, ResolverOpts};
use hickory_resolver::net::runtime::TokioRuntimeProvider;
use hickory_resolver::TokioResolver;
use reqwest::dns::{Addrs, Name, Resolve, Resolving};

/// Build the reqwest resolver for a validated `NetworkConfig`.
pub fn custom_resolver(network: &NetworkConfig) -> Result<CustomResolver, String> {
    let servers = network.dns_addrs()?;
    if servers.is_empty() {
        return Err("empty DNS server list".into());
    }
    // 0.26 splits the nameserver address from its per-protocol connection
    // config; keep the user's port (custom DNS servers often run on 5353).
    let name_servers = servers
        .into_iter()
        .map(|server| {
            let mut connection = ConnectionConfig::udp();
            connection.port = server.port();
            NameServerConfig::new(server.ip(), true, vec![connection])
        })
        .collect();
    let config = ResolverConfig::from_name_servers(name_servers);
    let mut options = ResolverOpts::default();
    options.timeout = Duration::from_secs(2);
    options.attempts = 2;
    let resolver = TokioResolver::builder_with_config(config, TokioRuntimeProvider::default())
        .with_options(options)
        .build()
        .map_err(|error| format!("dns resolver: {error}"))?;
    Ok(CustomResolver {
        resolver,
        ip_mode: network.ip_mode,
    })
}

/// Resolve via the configured servers, keeping only the address families the
/// user asked for. The IP-mode filter here is what makes "仅 IPv4" work with
/// dual-stack hosts: a bound socket can never reach the other family.
pub struct CustomResolver {
    resolver: TokioResolver,
    ip_mode: Option<IpMode>,
}

impl Resolve for CustomResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let resolver = self.resolver.clone();
        let ip_mode = self.ip_mode;
        let host = name.as_str().to_string();
        Box::pin(async move {
            let failure = |message: String| {
                Box::new(io::Error::new(io::ErrorKind::Other, message))
                    as Box<dyn std::error::Error + Send + Sync>
            };
            let lookup = resolver
                .lookup_ip(host.clone())
                .await
                .map_err(|error| failure(format!("dns lookup for {host}: {error}")))?;
            let addrs: Vec<SocketAddr> = lookup
                .iter()
                .filter(|ip| family_allowed(*ip, ip_mode))
                .map(|ip| SocketAddr::new(ip, 0))
                .collect();
            if addrs.is_empty() {
                return Err(failure(format!(
                    "dns lookup for {host}: no addresses match the configured IP mode"
                )));
            }
            let addrs: Addrs = Box::new(addrs.into_iter());
            Ok(addrs)
        })
    }
}

fn family_allowed(ip: IpAddr, ip_mode: Option<IpMode>) -> bool {
    match ip_mode {
        Some(IpMode::Ipv4Only) => ip.is_ipv4(),
        Some(IpMode::Ipv6Only) => ip.is_ipv6(),
        None => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aether_plugin_sdk::NetworkConfig;

    fn network(dns: &[&str], ip_mode: Option<IpMode>) -> NetworkConfig {
        NetworkConfig {
            user_agent: None,
            ip_mode,
            dns: Some(dns.iter().map(|server| server.to_string()).collect()),
            ..NetworkConfig::default()
        }
    }

    #[test]
    fn builds_a_resolver_from_valid_servers() {
        // Construction is the observable contract: valid servers build, the
        // value is an object-safe `Resolve` handed straight to reqwest.
        assert!(custom_resolver(&network(&["127.0.0.1"], None)).is_ok());
        assert!(custom_resolver(&network(&["223.5.5.5", "[::1]:5353"], None)).is_ok());
    }

    #[test]
    fn rejects_invalid_server_lists() {
        assert!(custom_resolver(&NetworkConfig::default()).is_err());
        assert!(custom_resolver(&network(&["not-an-ip"], None)).is_err());
    }

    #[test]
    fn family_filter_matches_the_mode() {
        let v4 = IpAddr::from([127, 0, 0, 1]);
        let v6 = IpAddr::from([0x2606, 0x4700, 0, 0, 0, 0, 0, 0x1111]);
        assert!(family_allowed(v4, None));
        assert!(family_allowed(v6, None));
        assert!(family_allowed(v4, Some(IpMode::Ipv4Only)));
        assert!(!family_allowed(v6, Some(IpMode::Ipv4Only)));
        assert!(family_allowed(v6, Some(IpMode::Ipv6Only)));
        assert!(!family_allowed(v4, Some(IpMode::Ipv6Only)));
    }
}
