# Third-party components

The application uses these components without vendoring their source into this
repository. Cargo.lock pins the complete dependency graph.

- [openvpn-connect 0.1.1](https://github.com/networks-rs/openvpn-connect-rs):
  third-party Rust bindings, MPL-2.0. Its sys crate contains OpenVPN 3 Core 3.11.7
  (MPL-2.0 or the upstream alternative license), Asio headers (Boost Software
  License), documented compatibility patches and upstream license notices.
  The bindings use Rust TLS/crypto backends. They are not maintained by OpenVPN Inc.
- [smoltcp 0.12](https://github.com/smoltcp-rs/smoltcp): Rust TCP/IP stack,
  0BSD. No kernel network interface is used.
- [Tokio](https://github.com/tokio-rs/tokio): asynchronous I/O, MIT.
- RustCrypto [AES-GCM](https://github.com/RustCrypto/AEADs),
  [PBKDF2](https://github.com/RustCrypto/password-hashes) and
  [SHA-1](https://github.com/RustCrypto/hashes): MIT/Apache-2.0; used for compatibility
  with OpenVPN Connect's existing saved-credential format, not a new storage scheme.
- [security-framework](https://github.com/kornelski/rust-security-framework):
  MIT/Apache-2.0; native macOS Keychain access, respecting its access controls.
- LZ4 is statically built through `lz4-sys`; see the source package's BSD license.
- The TAP-Windows public header is included under its MIT license option for
  Windows compilation only; see [vendor/windows/README.md](vendor/windows/README.md)
  for the exact upstream commit and [COPYRIGHT.MIT](vendor/windows/COPYRIGHT.MIT).

Release archives include THIRD_PARTY_LICENSES.txt, containing license files
and source download URLs for dependencies in the locked Cargo graph, including
build/development dependencies. The pinned OpenVPN/Asio sources and compatibility
patches are in the [openvpn-connect-sys 0.1.1 source archive](https://crates.io/api/v1/crates/openvpn-connect-sys/0.1.1/download).
OvpnLane source for each release is available under its Git tag at
https://github.com/debba/ovpnlane. No private VPN profiles are needed to build it.

The installer/update experience is inspired by
[TuxCleaner](https://github.com/debba/tuxcleaner); OvpnLane implements its own
platform selection, versioned package format and constrained archive handling.

Preserve the applicable notices and source availability obligations
when redistributing binaries. The build links standard platform libraries;
"static OpenVPN/LZ4" does not mean a fully static operating-system executable.
