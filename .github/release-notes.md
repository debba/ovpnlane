OvpnLane 0.0.4 adds direct reuse of OpenVPN Connect profiles and saved VPN passwords on macOS.

### OpenVPN Connect integration on macOS

Use an existing profile without manually exporting its `.ovpn` file or re-entering a saved password:

```sh
ovpnlane profiles
ovpnlane connect
ovpnlane connect "Work VPN"
ovpnlane connect "Work VPN" --check
```

- List profiles using the display names shown on Connect's profile cards.
- Select by exact name or ID, choose from an interactive menu, or automatically select the only profile.
- Reuse the saved username and, when Keychain authorization allows it, the selected profile's saved VPN password.
- Keep using all existing connection options, including `--listen`, `--dns`, `--username`, `--auth-file` and `--save-password`.

The default SOCKS5 endpoint is `127.0.0.1:1080`. Wait for `CONNECTED`, then configure the applications that should use the VPN to use that proxy. Ctrl+C stops the session. OvpnLane does not activate or modify a system VPN, change system routes/DNS, or launch OpenVPN Connect.

### Credentials and permissions

Explicit credentials and OvpnLane's own saved password take precedence over Connect's password. Connect credentials are read only for the selected profile and matching saved username. macOS may request Keychain authorization; access denial or an unsupported credential format falls back to password input.

Passwords are decoded only in memory and are not printed or exported. Connect's files and Keychain entries remain unchanged. `--save-password` explicitly copies a password into OvpnLane's own store only after successful VPN authentication. `profiles` and `--check` do not read passwords.

On macOS, `--non-interactive` now suppresses Keychain authorization dialogs as well as terminal prompts. An entry that requires new authorization will be unavailable in this mode.

### Platform support

**OvpnLane still builds and runs on macOS, Linux and Windows.** Only the new `profiles` and `connect` commands are restricted to macOS. On Linux and Windows they return an explicit unsupported-platform error; existing `--config` connections and `update` remain supported. Linux/Windows OpenVPN Connect integration is deferred.

Connect's private storage/credential format was checked against macOS version 3.8.2. Application settings, server overrides, proxies, external certificates, tokens, OTPs and private-key passwords are not imported. Existing SSO/external-PKI limitations remain. Use an exported profile with `--config PATH` if discovery is unavailable.

### Downloads

- `ovpnlane-0.0.4-macos-arm64.tar.gz`: macOS, Apple Silicon.
- `ovpnlane-0.0.4-macos-x86_64.tar.gz`: macOS, Intel.
- `ovpnlane-0.0.4-linux-x86_64.tar.gz`: Linux x86_64, built on Ubuntu 24.04 with glibc.
- `ovpnlane-0.0.4-windows-x86_64.zip`: Windows x86_64, MSVC.

Each archive has an accompanying `.sha256` file. macOS binaries are not notarized; Windows binaries are not Authenticode-signed.

### Install or update

```sh
curl -fsSL https://raw.githubusercontent.com/debba/ovpnlane/main/install.sh | sh
# Or, for an existing installation:
ovpnlane update --check
ovpnlane update --yes
```

For the difference between exposing a VPN as SOCKS and OpenVPN's `--socks-proxy` option, credential precedence, troubleshooting and limitations, see the README.
