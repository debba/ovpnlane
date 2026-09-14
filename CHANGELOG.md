# Changelog

## 0.0.1

Initial public release of OvpnLane.

- OpenVPN-to-SOCKS5 proxy with an in-process TCP/IP stack.
- Native builds for Linux x86_64, macOS Apple Silicon and Intel, and Windows x86_64.
- SOCKS5 CONNECT for IPv4, IPv6 and DNS names, with TCP half-close support.
- DNS resolution for proxied destinations through the VPN, with explicit DNS overrides.
- Certificate and username/password authentication, encrypted private keys and static challenges.
- Loopback-only listener, bounded connection resources and no direct-network fallback.
- SSH integration using ProxyCommand and a SOCKS-capable connector.
- Connection diagnostics that preserve authentication failures and timeout causes.
- Packet integration tests and local OpenVPN interoperability tests.
- curl installer for macOS/Linux and checksum-verified self-updates from GitHub Releases.

See README.md for supported profiles, platform requirements and limitations.
