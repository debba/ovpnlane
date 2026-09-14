use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use anyhow::{Context, Result, bail, ensure};

/// Negotiated addresses belong exclusively to the in-process IP stack.
#[derive(Clone, Debug)]
pub struct Settings {
    pub ipv4: Option<Ipv4Addr>,
    pub ipv6: Option<Ipv6Addr>,
    pub dns: Vec<IpAddr>,
    pub mtu: usize,
}

impl Settings {
    pub fn parse(options: &str, mtu: usize, dns_override: &[IpAddr]) -> Result<Self> {
        let mut result = Self {
            ipv4: None,
            ipv6: None,
            dns: vec![],
            mtu,
        };
        // OpenVPN Core's OptionList::render(0) returns one unnumbered option per line.
        for line in options.lines() {
            let fields: Vec<_> = line.split_whitespace().collect();
            match fields.as_slice() {
                ["ifconfig", address, _peer_or_mask] => {
                    result.ipv4 = Some(address.parse().context("invalid VPN IPv4 address")?);
                }
                ["ifconfig-ipv6", address, _gateway] => {
                    let ip = address.split('/').next().unwrap_or(address);
                    result.ipv6 = Some(ip.parse().context("invalid VPN IPv6 address")?);
                }
                ["dhcp-option", "DNS" | "DNS6", address] => {
                    result
                        .dns
                        .push(address.parse().context("invalid VPN DNS address")?);
                }
                ["dns", "server", _priority, "address", addresses @ ..] => {
                    for address in addresses {
                        result
                            .dns
                            .push(address.parse().context("DNS must use a plain IP address")?);
                    }
                }
                ["tun-mtu", value] => {
                    result.mtu = result.mtu.min(value.parse().context("invalid tun-mtu")?)
                }
                _ => {}
            }
        }
        if !dns_override.is_empty() {
            result.dns = dns_override.to_vec();
        }
        ensure!(
            result.ipv4.is_some() || result.ipv6.is_some(),
            "server did not assign an IP address"
        );
        ensure!(
            (576..=9000).contains(&result.mtu),
            "VPN MTU must be between 576 and 9000"
        );
        ensure!(
            result.ipv6.is_none() || result.mtu >= 1280,
            "IPv6 requires MTU >= 1280"
        );
        for ip in result
            .ipv4
            .map(IpAddr::V4)
            .into_iter()
            .chain(result.ipv6.map(IpAddr::V6))
        {
            ensure!(
                !ip.is_unspecified() && !ip.is_multicast(),
                "invalid assigned VPN address"
            );
        }
        if result
            .dns
            .iter()
            .any(|ip| ip.is_unspecified() || ip.is_multicast())
        {
            bail!("invalid DNS server address");
        }
        // A resolver of a family not assigned by the VPN cannot be reached.
        let (v4, v6) = (result.ipv4.is_some(), result.ipv6.is_some());
        result.dns.retain(|ip| if ip.is_ipv4() { v4 } else { v6 });
        result.dns.dedup();
        Ok(result)
    }

    pub fn supports_family(&self, ip: IpAddr) -> bool {
        match ip {
            IpAddr::V4(_) => self.ipv4.is_some(),
            IpAddr::V6(_) => self.ipv6.is_some(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_subnet_net30_and_dual_stack() {
        for peer in ["255.255.255.0", "10.8.0.1"] {
            let settings = Settings::parse(&format!("topology subnet\nifconfig 10.8.0.2 {peer}\nifconfig-ipv6 fd00::2/64 fd00::1\ndhcp-option DNS 10.8.0.1\ndhcp-option DNS6 fd00::1\ntun-mtu 1400\n"), 1500, &[]).unwrap();
            assert_eq!(settings.ipv4, Some(Ipv4Addr::new(10, 8, 0, 2)));
            assert_eq!(settings.ipv6, Some("fd00::2".parse().unwrap()));
            assert_eq!(settings.dns.len(), 2);
            assert_eq!(settings.mtu, 1400);
        }
    }

    #[test]
    fn rejects_missing_address_and_bad_mtu() {
        assert!(Settings::parse("route 10.0.0.0 255.0.0.0", 1500, &[]).is_err());
        assert!(Settings::parse("ifconfig-ipv6 fd00::2/64 fd00::1", 1000, &[]).is_err());
    }

    #[test]
    fn dns_override_is_explicit_and_never_system_dns() {
        let s = Settings::parse(
            "ifconfig 10.8.0.2 10.8.0.1\ndhcp-option DNS 10.8.0.1",
            1500,
            &["10.9.0.1".parse().unwrap()],
        )
        .unwrap();
        assert_eq!(s.dns, vec!["10.9.0.1".parse::<IpAddr>().unwrap()]);
        assert!(
            Settings::parse("ifconfig 10.8.0.2 10.8.0.1", 1500, &[])
                .unwrap()
                .dns
                .is_empty()
        );
    }
}
