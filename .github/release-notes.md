OvpnLane's first public release: a native OpenVPN-to-SOCKS5 proxy for macOS, Linux and Windows.

Only applications configured to use the local SOCKS5 endpoint send their traffic through the VPN. OvpnLane runs without Docker, a kernel TUN device, administrator privileges or changes to system routing and DNS.

### Downloads

- `ovpnlane-0.0.1-macos-arm64.tar.gz`: macOS, Apple Silicon.
- `ovpnlane-0.0.1-macos-x86_64.tar.gz`: macOS, Intel.
- `ovpnlane-0.0.1-linux-x86_64.tar.gz`: Linux x86_64, built on Ubuntu 24.04 with glibc.
- `ovpnlane-0.0.1-windows-x86_64.zip`: Windows x86_64, MSVC.

Each archive includes the executable, documentation and third-party license notices. Verify downloads using the accompanying `.sha256` file. macOS binaries are not notarized; Windows binaries are not Authenticode-signed. Standard platform runtime libraries are required.

### Usage

Install on macOS or Linux:

```sh
curl -fsSL https://raw.githubusercontent.com/debba/ovpnlane/main/install.sh | sh
```

```sh
ovpnlane --config /path/to/client.ovpn
ovpnlane update --check
ovpnlane update --yes
```

Wait for `CONNECTED`, then configure SSH, curl or another TCP application to use `127.0.0.1:1080` as a SOCKS5 proxy. Read the README for SSH examples and authentication options.

### Scope

SOCKS5 CONNECT supports IPv4, IPv6 and DNS names. UDP ASSOCIATE, BIND, TAP, compression, browser SSO, interactive dynamic challenges and external PKI are not supported. Established TCP connections close if the VPN reconnects. This is an initial release and has not undergone an independent security audit.

The application and TCP/IP stack are written in Rust; OpenVPN protocol handling uses OpenVPN 3 Core through the third-party `openvpn-connect` bindings. See the README and THIRD_PARTY.md for sources and licensing. Development used OpenAI Codex.
