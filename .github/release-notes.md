OvpnLane 0.0.2 adds secure VPN password persistence and improves Windows build reproducibility.

### Password storage

Use `--save-password` to save a VPN password in the operating system's native credential store:

- Keychain on macOS.
- Credential Manager on Windows.
- Secret Service on Linux.

```sh
ovpnlane --config /path/to/client.ovpn --username vpn-user --save-password
```

The password is not written when credentials are entered. OvpnLane waits until OpenVPN reports `CONNECTED`, so `AUTH_FAILED`, certificate errors, timeouts, and other failures before connection do not persist an unverified password.

On later runs, the saved password is retrieved automatically for the same canonical profile path and username; `--save-password` does not need to be supplied again. To replace an entry, provide `OVPN_PASS` together with `--save-password`: the new value replaces the old one only after successful authentication.

`--save-password` cannot be combined with `--auth-file`. Encrypted private-key passwords and static challenge responses are not persisted. Direct 1Password vault integration is not included in this release; only the native operating-system credential stores listed above are supported.

### Windows builds

- Added the public TAP-Windows header required by the native OpenVPN build and included its MIT copyright notice in packaged license information.
- Added the required C++ exception flags for the MSVC build.
- Matched the native OpenVPN C++ runtime to Rust's debug and release MSVC runtimes, preventing mixed-runtime binaries.

### CI and packaging

- Switched dependency caching to the official GitHub Actions cache action.
- Release archives continue to include the executable, README, changelog, project license, third-party notices, and generated dependency licenses.

### Downloads

- `ovpnlane-0.0.2-macos-arm64.tar.gz`: macOS, Apple Silicon.
- `ovpnlane-0.0.2-macos-x86_64.tar.gz`: macOS, Intel.
- `ovpnlane-0.0.2-linux-x86_64.tar.gz`: Linux x86_64, built on Ubuntu 24.04 with glibc.
- `ovpnlane-0.0.2-windows-x86_64.zip`: Windows x86_64, MSVC.

Each download has an accompanying `.sha256` file. macOS binaries are not notarized, and Windows binaries are not Authenticode-signed.

### Usage

Install or update on macOS and Linux:

```sh
curl -fsSL https://raw.githubusercontent.com/debba/ovpnlane/main/install.sh | sh
ovpnlane update --check
ovpnlane update --yes
```

Wait for `CONNECTED`, then configure applications to use the local SOCKS5 endpoint, `127.0.0.1:1080` by default.
