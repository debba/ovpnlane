use anyhow::{Context, Result, bail, ensure};
use clap::Args;
use flate2::read::GzDecoder;
use semver::Version;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{self, Cursor, IsTerminal, Read, Write},
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

const API: &str = "https://api.github.com/repos/debba/ovpnlane/releases";
const DOWNLOADS: &str = "https://github.com/debba/ovpnlane/releases/download";
const MAX_DOWNLOAD: u64 = 100 * 1024 * 1024;
const MAX_UNPACKED: u64 = 200 * 1024 * 1024;

#[derive(Args, Debug)]
pub struct UpdateArgs {
    /// Check the release and matching assets without installing anything.
    #[arg(long, conflicts_with = "dry_run")]
    check: bool,
    /// Show the selected version and executable path without installing.
    #[arg(long)]
    dry_run: bool,
    /// Install without an interactive confirmation.
    #[arg(short, long)]
    yes: bool,
    /// Select a release, for example 0.0.1 or v0.0.1 (allows reinstall/downgrade).
    #[arg(long)]
    version: Option<String>,
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    draft: bool,
    prerelease: bool,
    assets: Vec<Asset>,
}

#[derive(Deserialize)]
struct Asset {
    name: String,
}

fn parse_version(text: &str) -> Result<Version> {
    let version = Version::parse(text.strip_prefix('v').unwrap_or(text))
        .context("expected a semantic version such as 0.0.1 or v0.0.1")?;
    ensure!(
        version.build.is_empty(),
        "release versions must not contain build metadata"
    );
    Ok(version)
}

fn platform() -> Result<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Ok("macos-arm64"),
        ("macos", "x86_64") => Ok("macos-x86_64"),
        ("linux", "x86_64") if cfg!(target_env = "gnu") => Ok("linux-x86_64"),
        ("windows", "x86_64") if cfg!(target_env = "msvc") => Ok("windows-x86_64"),
        _ => bail!("no prebuilt release for this platform; update from source"),
    }
}

fn archive_name(version: &Version, platform: &str) -> String {
    let extension = if platform.starts_with("windows-") {
        "zip"
    } else {
        "tar.gz"
    };
    format!("ovpnlane-{version}-{platform}.{extension}")
}

fn validate_release(
    release: &Release,
    requested: Option<&Version>,
    platform: &str,
) -> Result<(Version, String)> {
    ensure!(!release.draft, "cannot install a draft release");
    let version = parse_version(&release.tag_name)?;
    ensure!(
        release.tag_name == format!("v{version}"),
        "unexpected release tag format"
    );
    if let Some(requested) = requested {
        ensure!(
            requested == &version,
            "GitHub returned a different release than requested"
        );
    } else {
        ensure!(
            !release.prerelease && version.pre.is_empty(),
            "latest release is not stable"
        );
    }
    let archive = archive_name(&version, platform);
    for name in [&archive, &format!("{archive}.sha256")] {
        ensure!(
            release
                .assets
                .iter()
                .filter(|asset| &asset.name == name)
                .count()
                == 1,
            "release {} must contain exactly one {name} asset",
            release.tag_name
        );
    }
    Ok((version, archive))
}

pub fn run(args: UpdateArgs) -> Result<()> {
    let target = platform()?;
    let requested = args.version.as_deref().map(parse_version).transpose()?;
    let suffix = requested
        .as_ref()
        .map_or_else(|| "latest".into(), |version| format!("tags/v{version}"));
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .https_only(true)
        .timeout_global(Some(Duration::from_secs(120)))
        .user_agent(concat!("ovpnlane/", env!("CARGO_PKG_VERSION")))
        .build()
        .into();
    let release: Release = agent
        .get(&format!("{API}/{suffix}"))
        .header("Accept", "application/vnd.github+json")
        .call()
        .context("could not query GitHub Releases (check connectivity or API rate limits)")?
        .body_mut()
        .with_config()
        .limit(2 * 1024 * 1024)
        .read_json()
        .context("invalid GitHub release metadata")?;
    let (available, archive) = validate_release(&release, requested.as_ref(), target)?;
    let current = Version::parse(env!("CARGO_PKG_VERSION"))?;
    println!("Installed: {current}; selected release: {available}; platform: {target}");
    if args.check {
        println!(
            "{}",
            if available > current {
                "An update is available."
            } else {
                "No newer version is available."
            }
        );
        return Ok(());
    }
    if requested.is_none() && available <= current {
        println!("OvpnLane is up to date.");
        return Ok(());
    }
    let executable = std::env::current_exe().context("could not locate the running executable")?;
    println!("Install {available} at {}", executable.display());
    if args.dry_run {
        return Ok(());
    }
    if !args.yes {
        ensure!(
            io::stdin().is_terminal(),
            "use --yes for a non-interactive update"
        );
        print!("Continue? [y/N] ");
        io::stdout().flush()?;
        let mut answer = String::new();
        io::stdin().read_line(&mut answer)?;
        if !matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
            println!("Update cancelled.");
            return Ok(());
        }
    }
    let directory = executable
        .parent()
        .context("executable has no parent directory")?;
    let staging = tempfile::Builder::new()
        .prefix(".ovpnlane-update-")
        .tempdir_in(directory)
        .context(
            "installation directory is not writable; install OvpnLane in a user-writable directory",
        )?;
    let url = format!("{DOWNLOADS}/{}/{archive}", release.tag_name);
    let bytes = download(&agent, &url, MAX_DOWNLOAD)?;
    let checksum = download(&agent, &format!("{url}.sha256"), 4096)?;
    verify_checksum(&bytes, &checksum, &archive)?;
    let candidate = unpack_binary(&bytes, target.starts_with("windows-"), staging.path())?;
    let output = Command::new(&candidate)
        .arg("--version")
        .output()
        .context("downloaded binary cannot run on this system")?;
    ensure!(
        output.status.success()
            && String::from_utf8_lossy(&output.stdout).trim() == format!("ovpnlane {available}"),
        "downloaded binary did not report the expected version"
    );
    self_replace::self_replace(&candidate)
        .context("could not replace the executable; installation was not completed")?;
    println!("Updated OvpnLane from {current} to {available}.");
    Ok(())
}

fn download(agent: &ureq::Agent, url: &str, limit: u64) -> Result<Vec<u8>> {
    agent
        .get(url)
        .call()
        .with_context(|| format!("could not download {url}"))?
        .body_mut()
        .with_config()
        .limit(limit)
        .read_to_vec()
        .context("download failed or exceeded the size limit")
}

fn verify_checksum(bytes: &[u8], manifest: &[u8], archive: &str) -> Result<()> {
    let text = std::str::from_utf8(manifest).context("checksum is not UTF-8")?;
    let fields: Vec<_> = text.split_whitespace().collect();
    ensure!(
        fields.len() == 2 && fields[1] == archive,
        "checksum manifest must name the selected archive"
    );
    let expected = fields[0];
    ensure!(
        expected.len() == 64 && expected.bytes().all(|c| c.is_ascii_hexdigit()),
        "invalid SHA-256 checksum"
    );
    ensure!(
        expected.eq_ignore_ascii_case(&format!("{:x}", Sha256::digest(bytes))),
        "SHA-256 checksum mismatch; executable was not replaced"
    );
    Ok(())
}

fn unpack_binary(bytes: &[u8], windows: bool, destination: &Path) -> Result<PathBuf> {
    let name = if windows { "ovpnlane.exe" } else { "ovpnlane" };
    let output = destination.join(name);
    let mut found = false;
    if windows {
        let mut archive = zip::ZipArchive::new(Cursor::new(bytes))?;
        for index in 0..archive.len() {
            let mut entry = archive.by_index(index)?;
            if entry.name() != name {
                continue;
            }
            let mode = entry.unix_mode().unwrap_or(0) & 0o170000;
            ensure!(
                entry.is_file() && (mode == 0 || mode == 0o100000),
                "release binary must be a regular file"
            );
            ensure!(!found, "duplicate executable in release archive");
            write_candidate(&mut entry, &output)?;
            found = true;
        }
    } else {
        let decoder = GzDecoder::new(Cursor::new(bytes)).take(MAX_UNPACKED);
        let mut archive = tar::Archive::new(decoder);
        for entry in archive.entries()? {
            let mut entry = entry?;
            if entry.path_bytes().as_ref() != name.as_bytes() {
                continue;
            }
            ensure!(
                entry.header().entry_type().is_file(),
                "release binary must be a regular file"
            );
            ensure!(!found, "duplicate executable in release archive");
            write_candidate(&mut entry, &output)?;
            found = true;
        }
    }
    ensure!(found, "release archive does not contain {name}");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&output, fs::Permissions::from_mode(0o755))?;
    }
    Ok(output)
}

fn write_candidate(reader: impl Read, output: &Path) -> Result<()> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output)?;
    let size = io::copy(&mut reader.take(MAX_DOWNLOAD + 1), &mut file)?;
    ensure!(size > 0 && size <= MAX_DOWNLOAD, "invalid executable size");
    file.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::{Compression, write::GzEncoder};

    #[test]
    fn rejects_changed_or_mislabelled_downloads() {
        let bytes = b"example archive";
        let manifest = format!("{:x}  release.tar.gz\n", Sha256::digest(bytes));
        verify_checksum(bytes, manifest.as_bytes(), "release.tar.gz").unwrap();
        assert!(verify_checksum(b"changed", manifest.as_bytes(), "release.tar.gz").is_err());
        assert!(verify_checksum(bytes, manifest.as_bytes(), "other.tar.gz").is_err());
        assert!(verify_checksum(bytes, b"bad  release.tar.gz", "release.tar.gz").is_err());
    }

    #[test]
    fn selects_exact_version_and_complete_platform_assets() {
        let version = parse_version("v0.0.1").unwrap();
        let archive = archive_name(&version, "macos-arm64");
        let mut release = Release {
            tag_name: "v0.0.1".into(),
            draft: false,
            prerelease: false,
            assets: vec![
                Asset {
                    name: archive.clone(),
                },
                Asset {
                    name: format!("{archive}.sha256"),
                },
            ],
        };
        validate_release(&release, Some(&version), "macos-arm64").unwrap();
        assert!(validate_release(&release, Some(&Version::new(1, 0, 0)), "macos-arm64").is_err());
        assert!(validate_release(&release, None, "windows-x86_64").is_err());
        release.assets.pop();
        assert!(validate_release(&release, None, "macos-arm64").is_err());
        assert!(parse_version("../main").is_err());
    }

    fn tar_with_binary(link: bool) -> Vec<u8> {
        let encoder = GzEncoder::new(Vec::new(), Compression::default());
        let mut archive = tar::Builder::new(encoder);
        let mut header = tar::Header::new_gnu();
        header.set_mode(0o755);
        if link {
            header.set_entry_type(tar::EntryType::Symlink);
            header.set_link_name("/tmp/unrelated").unwrap();
            header.set_size(0);
            header.set_cksum();
            archive
                .append_data(&mut header, "ovpnlane", io::empty())
                .unwrap();
        } else {
            header.set_size(6);
            header.set_cksum();
            archive
                .append_data(&mut header, "ovpnlane", &b"binary"[..])
                .unwrap();
        }
        archive.into_inner().unwrap().finish().unwrap()
    }

    #[test]
    fn tar_extraction_rejects_symlinks_and_accepts_regular_binary() {
        let directory = tempfile::tempdir().unwrap();
        assert!(unpack_binary(&tar_with_binary(true), false, directory.path()).is_err());
        assert!(!directory.path().join("ovpnlane").exists());
        let path = unpack_binary(&tar_with_binary(false), false, directory.path()).unwrap();
        assert_eq!(fs::read(path).unwrap(), b"binary");
    }

    #[test]
    fn zip_extracts_only_the_exact_windows_executable() {
        let mut archive = zip::ZipWriter::new(Cursor::new(Vec::new()));
        archive
            .start_file("../unrelated", zip::write::SimpleFileOptions::default())
            .unwrap();
        archive.write_all(b"ignored").unwrap();
        archive
            .start_file("ovpnlane.exe", zip::write::SimpleFileOptions::default())
            .unwrap();
        archive.write_all(b"binary").unwrap();
        let bytes = archive.finish().unwrap().into_inner();
        let directory = tempfile::tempdir().unwrap();
        let path = unpack_binary(&bytes, true, directory.path()).unwrap();
        assert_eq!(fs::read(path).unwrap(), b"binary");
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
    }
}
