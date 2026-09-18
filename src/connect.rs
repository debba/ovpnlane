//! Read-only discovery of OpenVPN Connect 3 profiles on macOS.
//!
//! The on-disk format is private to Connect. Names, usernames and the saved
//! password flag come from metadata; passwords are read separately from Keychain
//! only when connecting. Profile paths come from the profiles directory, never JSON.
mod password;
use anyhow::{Context, Result, bail, ensure};
use serde_json::Value;
use std::{
    fs,
    io::{self, IsTerminal, Read, Write},
    path::{Path, PathBuf},
};

#[derive(Clone, Debug)]
pub struct Profile {
    pub id: String,
    pub name: String,
    pub path: PathBuf,
    pub username: Option<String>,
    pub saved_password: bool,
}

impl Profile {
    fn password_account(&self, username: &str) -> Option<&str> {
        // Never reuse a saved password for an explicitly different user, or a
        // stale Keychain entry after "Save password" was disabled in Connect.
        (self.saved_password && self.username.as_deref() == Some(username))
            .then_some(self.id.as_str())
    }

    pub fn saved_password(&self, username: &str) -> Result<Option<zeroize::Zeroizing<String>>> {
        match self.password_account(username) {
            Some(id) => password::read(id),
            None => Ok(None),
        }
    }
}

pub fn discover() -> Result<Vec<Profile>> {
    ensure!(
        cfg!(target_os = "macos"),
        "OpenVPN Connect discovery is only supported on macOS; use --config PATH"
    );
    let home = std::env::var_os("HOME").context("HOME is not set; use --config PATH")?;
    discover_in(&PathBuf::from(home).join("Library/Application Support/OpenVPN Connect"))
}

fn discover_in(root: &Path) -> Result<Vec<Profile>> {
    let directory = root.join("profiles");
    let entries = fs::read_dir(&directory).with_context(|| {
        format!(
            "could not read OpenVPN Connect profiles in {}; import a profile in Connect or use --config PATH",
            directory.display()
        )
    })?;
    let metadata = read_metadata(&root.join("config.json"))?;
    let mut profiles = Vec::new();
    for entry in entries {
        let entry = entry.context("could not read OpenVPN Connect profile directory entry")?;
        let path = entry.path();
        // Do not follow symlinks or metadata filePath values (which can point at
        // an original import rather than Connect's current, merged profile).
        if !entry.file_type()?.is_file()
            || !path
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| ext.eq_ignore_ascii_case("ovpn"))
        {
            continue;
        }
        let id = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .context("OpenVPN Connect profile filename is not valid UTF-8")?
            .to_owned();
        let record = &metadata[&id];
        let name = ["profileDisplayName", "name", "friendlyName", "profileName"]
            .into_iter()
            .find_map(|key| nonempty_string(&record[key]))
            .unwrap_or(&id)
            .to_owned();
        let username = nonempty_string(&record["username"])
            .or_else(|| nonempty_string(&record["profileConfig"]["userlockedUsername"]))
            .map(str::to_owned);
        profiles.push(Profile {
            id,
            name,
            path,
            username,
            saved_password: record["savedPassword"].as_bool().unwrap_or(false),
        });
    }
    profiles.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.id.cmp(&b.id)));
    ensure!(
        !profiles.is_empty(),
        "no .ovpn profiles found in {}; import a profile in OpenVPN Connect or use --config PATH",
        directory.display()
    );
    Ok(profiles)
}

fn nonempty_string(value: &Value) -> Option<&str> {
    value.as_str().filter(|s| !s.trim().is_empty())
}

// Redux Persist stores both persist:root and its status member as JSON strings.
// Accept objects too, but never recursively interpret arbitrary profile values.
fn decode(value: Value) -> Result<Value> {
    match value {
        Value::String(text) => serde_json::from_str(&text).map_err(|_| metadata_error()),
        value => Ok(value),
    }
}

fn metadata_error() -> anyhow::Error {
    anyhow::anyhow!(
        "unrecognized OpenVPN Connect config.json format; use --config PATH with an exported .ovpn profile"
    )
}

fn read_metadata(path: &Path) -> Result<Value> {
    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Value::Null),
        Err(error) => return Err(error).context("could not read OpenVPN Connect metadata"),
    };
    const MAX_METADATA: u64 = 16 * 1024 * 1024;
    let mut bytes = Vec::new();
    file.take(MAX_METADATA + 1)
        .read_to_end(&mut bytes)
        .context("could not read OpenVPN Connect metadata")?;
    ensure!(
        bytes.len() as u64 <= MAX_METADATA,
        "OpenVPN Connect metadata exceeds 16 MiB; use --config PATH"
    );
    // Suppress parser details: this file can contain secrets in unrelated fields.
    let document: Value = serde_json::from_slice(&bytes).map_err(|_| metadata_error())?;
    let root = decode(document.get("persist:root").cloned().unwrap_or(document))?;
    let status = decode(root.get("status").cloned().ok_or_else(metadata_error)?)?;
    status
        .get("profiles")
        .filter(|profiles| profiles.is_object())
        .cloned()
        .ok_or_else(metadata_error)
}

pub fn select<'a>(profiles: &'a [Profile], selector: &str) -> Result<&'a Profile> {
    // IDs take precedence, so duplicate display names can always be resolved.
    if let Some(profile) = profiles.iter().find(|p| p.id == selector) {
        return Ok(profile);
    }
    let mut matches = profiles.iter().filter(|p| p.name == selector);
    let profile = matches.next().context(
        "OpenVPN Connect profile not found; run `ovpnlane profiles` and use an exact name or ID",
    )?;
    ensure!(
        matches.next().is_none(),
        "multiple OpenVPN Connect profiles have that name; select an ID from `ovpnlane profiles`"
    );
    Ok(profile)
}

pub fn list(profiles: &[Profile], mut output: impl Write) -> Result<()> {
    writeln!(output, "ID\tNAME")?;
    for profile in profiles {
        writeln!(
            output,
            "{}\t{}",
            display(&profile.id),
            display(&profile.name)
        )?;
    }
    Ok(())
}

// Profile labels are untrusted terminal text, not format strings or shell input.
fn display(text: &str) -> String {
    text.chars().flat_map(char::escape_debug).collect()
}

pub fn choose<'a>(
    profiles: &'a [Profile],
    selector: Option<&str>,
    non_interactive: bool,
) -> Result<&'a Profile> {
    if let Some(selector) = selector {
        return select(profiles, selector);
    }
    if let [profile] = profiles {
        return Ok(profile);
    }
    ensure!(!profiles.is_empty(), "no OpenVPN Connect profiles found");
    ensure!(
        !non_interactive && io::stdin().is_terminal() && io::stderr().is_terminal(),
        "specify a profile name or ID: ovpnlane connect <PROFILE>; run `ovpnlane profiles` to list them"
    );
    let mut output = io::stderr().lock();
    for (index, profile) in profiles.iter().enumerate() {
        writeln!(
            output,
            "{}. {} [{}]",
            index + 1,
            display(&profile.name),
            display(&profile.id)
        )?;
    }
    write!(output, "Select VPN [1-{}]: ", profiles.len())?;
    output.flush()?;
    let mut line = String::new();
    io::stdin().read_line(&mut line)?;
    let index = line
        .trim()
        .parse::<usize>()
        .ok()
        .and_then(|n| n.checked_sub(1));
    match index.and_then(|index| profiles.get(index)) {
        Some(profile) => Ok(profile),
        None => bail!(
            "invalid selection; enter a number from 1 to {}",
            profiles.len()
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::TempDir;

    fn fixture(metadata: Value, persisted: bool) -> TempDir {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("profiles")).unwrap();
        // Discovery must not parse, copy or print profile contents.
        for id in ["100", "200"] {
            fs::write(
                dir.path().join(format!("profiles/{id}.ovpn")),
                "private-key-body",
            )
            .unwrap();
        }
        let status = json!({"profiles": metadata});
        let document = if persisted {
            json!({"persist:root": json!({"status": status.to_string()}).to_string()})
        } else {
            json!({"status": status})
        };
        fs::write(dir.path().join("config.json"), document.to_string()).unwrap();
        dir
    }

    #[test]
    fn reads_redux_metadata_and_uses_only_stored_profile_paths() {
        for persisted in [true, false] {
            let dir = fixture(
                json!({
                    "100": {"name": "Work VPN", "username": "alice", "filePath": "/outside/secret.ovpn", "savedPassword": true},
                    "200": {"profileDisplayName": "Home VPN", "username": ""},
                    "../outside": {"name": "Not a stored profile"}
                }),
                persisted,
            );
            fs::write(dir.path().join("profiles/ignored.txt"), "ignored").unwrap();
            fs::create_dir(dir.path().join("profiles/directory.ovpn")).unwrap();
            let profiles = discover_in(dir.path()).unwrap();
            assert_eq!(profiles.len(), 2);
            assert_eq!(profiles[0].name, "Home VPN");
            assert!(profiles[0].username.is_none());
            let work = select(&profiles, "Work VPN").unwrap();
            assert_eq!(work.username.as_deref(), Some("alice"));
            assert_eq!(work.path, dir.path().join("profiles/100.ovpn"));
            assert_eq!(select(&profiles, "100").unwrap().name, "Work VPN");
        }
    }

    #[test]
    fn uses_the_card_display_name_instead_of_the_import_filename() {
        let dir = fixture(
            json!({
                "100": {"name": "original-import", "profileDisplayName": "Office VPN", "profileName": "old-name"},
                "200": {"name": "Fallback name", "profileDisplayName": " "}
            }),
            true,
        );
        let profiles = discover_in(dir.path()).unwrap();
        assert_eq!(select(&profiles, "Office VPN").unwrap().id, "100");
        assert_eq!(select(&profiles, "Fallback name").unwrap().id, "200");
        let mut output = Vec::new();
        list(&profiles, &mut output).unwrap();
        assert!(
            !String::from_utf8(output)
                .unwrap()
                .contains("original-import")
        );
    }

    #[test]
    fn password_lookup_is_scoped_to_the_profile_and_saved_username() {
        let dir = fixture(
            json!({
                "100": {"username": "alice", "savedPassword": true},
                "200": {"profileConfig": {"userlockedUsername": "bob"}, "savedPassword": false}
            }),
            true,
        );
        let profiles = discover_in(dir.path()).unwrap();
        let alice = select(&profiles, "100").unwrap();
        assert_eq!(alice.password_account("alice"), Some("100"));
        assert_eq!(alice.password_account("bob"), None);
        assert_eq!(
            select(&profiles, "200").unwrap().password_account("bob"),
            None
        );
        assert_eq!(
            select(&profiles, "200").unwrap().username.as_deref(),
            Some("bob")
        );
        let mut missing = alice.clone();
        missing.username = None;
        assert_eq!(missing.password_account("alice"), None);
    }

    #[test]
    fn missing_metadata_falls_back_to_ids() {
        let dir = fixture(json!({}), true);
        fs::remove_file(dir.path().join("config.json")).unwrap();
        let profiles = discover_in(dir.path()).unwrap();
        assert_eq!(profiles[0].name, "100");
        assert!(profiles[0].username.is_none());
    }

    #[test]
    fn duplicate_names_require_an_id_and_unknown_names_fail() {
        let dir = fixture(
            json!({"100": {"name": "Work"}, "200": {"name": "Work"}}),
            true,
        );
        let profiles = discover_in(dir.path()).unwrap();
        assert!(
            select(&profiles, "Work")
                .unwrap_err()
                .to_string()
                .contains("multiple")
        );
        assert!(select(&profiles, "missing").is_err());
        assert_eq!(select(&profiles, "200").unwrap().id, "200");
        assert!(choose(&profiles, None, true).is_err());
        assert_eq!(choose(&profiles[..1], None, true).unwrap().id, "100");
        assert_eq!(choose(&profiles, Some("200"), true).unwrap().id, "200");
        assert!(choose(&[], None, true).is_err());
    }

    #[test]
    fn errors_do_not_disclose_metadata_and_empty_stores_fail() {
        let dir = fixture(json!({}), true);
        for text in [
            "password-secret",
            r#"{"persist:root":"password-secret"}"#,
            r#"{"status":{"profiles":"password-secret"}}"#,
            r#"{"status":true}"#,
            r#"{"persist:root":[]}"#,
            "null",
            "42",
        ] {
            fs::write(dir.path().join("config.json"), text).unwrap();
            let message = discover_in(dir.path()).unwrap_err().to_string();
            assert!(message.contains("--config"));
            assert!(!message.contains("password-secret"));
        }
        fs::remove_file(dir.path().join("config.json")).unwrap();
        fs::remove_dir_all(dir.path().join("profiles")).unwrap();
        assert!(discover_in(dir.path()).is_err());
        fs::create_dir(dir.path().join("profiles")).unwrap();
        assert!(
            discover_in(dir.path())
                .unwrap_err()
                .to_string()
                .contains("no .ovpn")
        );
    }

    #[test]
    fn rejects_oversized_metadata() {
        let dir = fixture(json!({}), true);
        fs::File::create(dir.path().join("config.json"))
            .unwrap()
            .set_len(16 * 1024 * 1024 + 1)
            .unwrap();
        assert!(
            discover_in(dir.path())
                .unwrap_err()
                .to_string()
                .contains("exceeds 16 MiB")
        );
    }

    #[test]
    fn listing_escapes_terminal_controls_and_omits_secrets() {
        let dir = fixture(
            json!({"100": {"name": "Work\n\u{1b}[31m", "username": "secret-user", "password": "secret-pass"}}),
            true,
        );
        let profiles = discover_in(dir.path()).unwrap();
        let mut output = Vec::new();
        list(&profiles, &mut output).unwrap();
        let output = String::from_utf8(output).unwrap();
        assert_eq!(output.lines().count(), 3);
        assert!(!output.contains('\u{1b}'));
        assert!(!output.contains("secret"));
        assert!(!output.contains("private-key-body"));
    }

    #[cfg(unix)]
    #[test]
    fn ignores_symlinked_profiles() {
        let dir = fixture(json!({}), true);
        std::os::unix::fs::symlink(
            dir.path().join("profiles/100.ovpn"),
            dir.path().join("profiles/link.ovpn"),
        )
        .unwrap();
        assert_eq!(discover_in(dir.path()).unwrap().len(), 2);
    }
}
