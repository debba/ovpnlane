//! CLI integration with synthetic Connect storage, never the user's real home.
#[cfg(target_os = "macos")]
mod macos {
    use serde_json::json;
    use std::{fs, process::Command};
    use tempfile::TempDir;

    fn fixture() -> TempDir {
        let home = tempfile::tempdir().unwrap();
        let root = home
            .path()
            .join("Library/Application Support/OpenVPN Connect");
        fs::create_dir_all(root.join("profiles")).unwrap();
        fs::write(
            root.join("profiles/100.ovpn"),
            "client\ndev tun\nproto udp\nremote 127.0.0.1 1194\nauth-user-pass\n",
        )
        .unwrap();
        let status = json!({"profiles": {"100": {
            "name": "original-import-name", "profileDisplayName": "Work VPN",
            "username": "fixture-user", "savedPassword": true
        }}});
        fs::write(
            root.join("config.json"),
            json!({"persist:root": json!({"status": status.to_string()}).to_string()}).to_string(),
        )
        .unwrap();
        home
    }

    fn command(home: &TempDir) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_ovpnlane"));
        command.env("HOME", home.path()).env_remove("OVPN_USER");
        command
    }

    #[test]
    fn lists_and_validates_by_name_id_or_single_profile_without_modifying_storage() {
        let home = fixture();
        let root = home
            .path()
            .join("Library/Application Support/OpenVPN Connect");
        let metadata = fs::read(root.join("config.json")).unwrap();
        let profile = fs::read(root.join("profiles/100.ovpn")).unwrap();
        let output = command(&home).arg("profiles").output().unwrap();
        assert!(output.status.success());
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            "ID\tNAME\n100\tWork VPN\n"
        );
        for selector in [Some("Work VPN"), Some("100"), None] {
            let mut command = command(&home);
            command.arg("connect");
            if let Some(selector) = selector {
                command.arg(selector);
            }
            let output = command
                .args(["--check", "--non-interactive"])
                .env("OVPN_USER", "environment-user")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(String::from_utf8_lossy(&output.stdout).contains("Profile valid."));
            assert!(!String::from_utf8_lossy(&output.stderr).contains("SOCKS5 listening"));
        }
        assert_eq!(fs::read(root.join("config.json")).unwrap(), metadata);
        assert_eq!(fs::read(root.join("profiles/100.ovpn")).unwrap(), profile);
        assert_eq!(fs::read_dir(root.join("profiles")).unwrap().count(), 1);
    }

    #[test]
    fn selection_and_invalid_listen_errors_happen_without_connecting() {
        let home = fixture();
        let profiles = home
            .path()
            .join("Library/Application Support/OpenVPN Connect/profiles");
        fs::copy(profiles.join("100.ovpn"), profiles.join("200.ovpn")).unwrap();
        for (args, expected) in [
            (
                vec!["connect", "--non-interactive", "--check"],
                "specify a profile name or ID",
            ),
            (vec!["connect", "Missing", "--check"], "profile not found"),
            (
                vec!["connect", "100", "--listen", "0.0.0.0:1080", "--check"],
                "loopback",
            ),
        ] {
            let output = command(&home).args(args).output().unwrap();
            assert!(!output.status.success());
            assert!(String::from_utf8_lossy(&output.stderr).contains(expected));
        }
    }
}

#[test]
fn file_based_validation_remains_available_on_every_platform() {
    let directory = tempfile::tempdir().unwrap();
    let profile = directory.path().join("client.ovpn");
    std::fs::write(
        &profile,
        "client\ndev tun\nproto udp\nremote 127.0.0.1 1194\nauth-user-pass\n",
    )
    .unwrap();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_ovpnlane"))
        .arg("--config")
        .arg(profile)
        .args(["--check", "--non-interactive"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("Profile valid."));
    assert!(!String::from_utf8_lossy(&output.stderr).contains("SOCKS5 listening"));
}

#[cfg(not(target_os = "macos"))]
#[test]
fn discovery_on_other_platforms_explains_the_config_alternative_before_loading_profiles() {
    // Even an unusable HOME must produce the platform error, never a filesystem
    // error or prompt. The commands remain recognized, but cannot run discovery.
    let directory = tempfile::tempdir().unwrap();
    for args in [
        vec!["profiles"],
        vec!["connect", "--non-interactive"],
        vec!["connect", "Work", "--check"],
    ] {
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_ovpnlane"))
            .env("HOME", directory.path().join("does-not-exist"))
            .args(args)
            .output()
            .unwrap();
        assert!(!output.status.success());
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(error.contains("only supported on macOS") && error.contains("--config"));
        assert!(!error.contains("could not read"));
        assert!(!error.contains("SOCKS5 listening"));
        assert!(output.stdout.is_empty());
    }
}
