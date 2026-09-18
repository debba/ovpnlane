# Changelog

## 0.0.4

- Added `ovpnlane profiles` to list locally stored OpenVPN Connect profiles on macOS.
- Added `ovpnlane connect [NAME_OR_ID]` to start a profile as a userspace SOCKS5
  proxy, with interactive selection when several profiles are available and no
  name is supplied. All existing connection options remain available.
- Use the profile card's display name instead of the original import filename.
- Reuse OpenVPN Connect's saved VPN password from the macOS Keychain for the
  selected profile and saved username, decoding it only in memory. Explicit
  credentials and OvpnLane's own saved password take precedence. Missing,
  denied or unrecognized credentials fall back to the existing password input.
- Leave Connect's files and Keychain entries unchanged. `--save-password` may
  copy the password into OvpnLane's own store after successful authentication.
- Disable macOS Keychain dialogs with `--non-interactive`. Listing profiles and
  validating with `--check` never read credentials.
- Keep builds and existing file-based connections/updating cross-platform;
  only `profiles` and `connect` are restricted to macOS. Linux/Windows Connect
  integration is deferred, with explicit unsupported-platform errors for now.
- Document why OpenVPN's `--socks-proxy` is the reverse operation, not a built-in
  way to expose the VPN as a SOCKS server.
- Added profile discovery/selection tests, synthetic credential decoding and
  source-precedence tests, and macOS usage documentation.

## 0.0.3

### Terminal handling

- Install the interrupt handler before prompting for secrets, ensuring that
  pressing Ctrl+C during a password prompt restores the terminal correctly.
- Use platform-specific Tokio interrupt streams throughout the session so
  shutdown remains graceful on Unix and Windows.

## 0.0.2

### Password storage

- Added `--save-password` for saving VPN passwords in the operating system's
  native credential store: Keychain on macOS, Credential Manager on Windows,
  and Secret Service on Linux.
- Passwords are scoped to the canonical VPN profile path and username, then
  retrieved automatically on later runs without requiring `--save-password`
  again.
- Passwords are written only after OpenVPN reports `CONNECTED`. Rejected
  credentials (`AUTH_FAILED`), connection timeouts, certificate failures, and
  other failures before connection never persist the candidate password.
- `OVPN_PASS` together with `--save-password` replaces an existing entry only
  after the replacement credentials have been accepted by the VPN server.
- `--save-password` conflicts with `--auth-file`, which is already a persistent
  credential source. Private-key passwords and challenge responses are never
  stored.

### Builds and packaging

- Added the public TAP-Windows header and its MIT notices to make the Windows
  build inputs and redistribution terms explicit.
- Aligned the native OpenVPN C++ runtime with Rust's debug and release MSVC
  runtimes, avoiding mixed-runtime Windows builds.
- Added the required Windows exception flags and packaged the additional native
  license notices.
- Switched CI dependency caching to the official GitHub Actions cache action.

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
