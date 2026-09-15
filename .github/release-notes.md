OvpnLane 0.0.3 fixes terminal handling when password entry is interrupted.

### Terminal handling

OvpnLane now installs its interrupt handler before prompting for secrets. Pressing Ctrl+C during a password prompt therefore exits cleanly and restores the terminal state, rather than potentially leaving echo and terminal control keys disabled.

The same platform-specific interrupt stream remains active while the VPN session runs, preserving graceful shutdown behavior on Unix and Windows.

### Downloads

- `ovpnlane-0.0.3-macos-arm64.tar.gz`: macOS, Apple Silicon.
- `ovpnlane-0.0.3-macos-x86_64.tar.gz`: macOS, Intel.
- `ovpnlane-0.0.3-linux-x86_64.tar.gz`: Linux x86_64, built on Ubuntu 24.04 with glibc.
- `ovpnlane-0.0.3-windows-x86_64.zip`: Windows x86_64, MSVC.

Each download has an accompanying `.sha256` file. macOS binaries are not notarized, and Windows binaries are not Authenticode-signed.

### Usage

Install or update on macOS and Linux:

```sh
curl -fsSL https://raw.githubusercontent.com/debba/ovpnlane/main/install.sh | sh
ovpnlane update --check
ovpnlane update --yes
```

Wait for `CONNECTED`, then configure applications to use the local SOCKS5 endpoint, `127.0.0.1:1080` by default.
