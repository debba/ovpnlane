use anyhow::{Context, Result, bail, ensure};
use clap::{Parser, Subcommand};
use openvpn_connect::{Client, Config, Credentials, merge_config_path};
use ovpnlane::{netstack::StackWorker, socks, update, vpn::UserspaceVpn};
use std::{
    io::{self, IsTerminal, Write},
    net::{IpAddr, SocketAddr},
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::{net::TcpListener, sync::watch};
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(
    version,
    subcommand_negates_reqs = true,
    args_conflicts_with_subcommands = true,
    about = "OpenVPN to SOCKS5, entirely in userspace (macOS, Linux, Windows)"
)]
struct Args {
    #[command(subcommand)]
    command: Option<Command>,
    /// OpenVPN profile; certificate/key paths are relative to it.
    #[arg(short, long, required = true)]
    config: Option<PathBuf>,
    /// SOCKS5 endpoint. Only loopback addresses are accepted.
    #[arg(short, long, default_value = "127.0.0.1:1080")]
    listen: SocketAddr,
    /// VPN DNS override. Repeat for multiple servers. Queries use the tunnel.
    #[arg(long)]
    dns: Vec<IpAddr>,
    /// VPN username; prompted when needed if omitted.
    #[arg(long, env = "OVPN_USER")]
    username: Option<String>,
    /// Two-line file containing VPN username and password.
    #[arg(long)]
    auth_file: Option<PathBuf>,
    /// Save the password in the system keychain after successful authentication.
    #[arg(long, conflicts_with = "auth_file")]
    save_password: bool,
    /// Never prompt. OVPN_PASS, OVPN_KEY_PASS and OVPN_RESPONSE are accepted.
    #[arg(long)]
    non_interactive: bool,
    /// Validate the profile and exit without connecting.
    #[arg(long)]
    check: bool,
    /// VPN startup and proxied DNS/TCP connection timeout, seconds.
    #[arg(long, default_value = "30", value_parser = clap::value_parser!(u32).range(1..=300))]
    connect_timeout: u32,
    /// Stream idle timeout, seconds; 0 disables.
    #[arg(long, default_value = "600")]
    idle_timeout: u32,
    /// Maximum concurrent SOCKS clients.
    #[arg(long, default_value = "128", value_parser = clap::value_parser!(u16).range(1..=4096))]
    max_connections: u16,
    #[arg(short, long)]
    verbose: bool,
}

#[derive(Subcommand)]
enum Command {
    /// Check for or install an update from GitHub Releases.
    Update(update::UpdateArgs),
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    if let Some(Command::Update(update_args)) = args.command {
        return tokio::task::spawn_blocking(move || update::run(update_args)).await?;
    }
    tracing_subscriber::fmt()
        .with_writer(io::stderr)
        .with_ansi(io::stderr().is_terminal())
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| {
            EnvFilter::new(if args.verbose {
                "ovpnlane=debug"
            } else {
                "ovpnlane=info"
            })
        }))
        .init();
    ensure!(
        args.listen.ip().is_loopback(),
        "--listen must be a loopback address (127.0.0.1 or ::1)"
    );
    let profile = args
        .config
        .context("--config is required to connect")?
        .canonicalize()
        .context("could not open profile")?;
    let merged = merge_config_path(
        profile
            .to_str()
            .context("profile path is not valid UTF-8")?,
        true,
    );
    ensure!(
        merged.error_text.is_empty(),
        "could not load profile: {}",
        merged.error_text
    );
    let worker = StackWorker::spawn(
        Duration::from_secs(args.connect_timeout.into()),
        Duration::from_secs(args.idle_timeout.into()),
        args.max_connections.into(),
    )?;
    let handler = UserspaceVpn::new(worker.tunnel(), args.dns.clone());
    let diagnostics = handler.diagnostics();
    let client = Client::new(handler)?;
    let mut config = Config::new(merged.profile_content)
        .with_gui_version(concat!("ovpnlane/", env!("CARGO_PKG_VERSION")));
    config.connection_timeout_seconds = args.connect_timeout as i32;
    config.tun_persist = false;
    config.google_dns_fallback = false;
    config.compression_mode = "no".into();
    config
        .content_list
        .push(("remote-cert-tls".into(), "server".into()));
    let mut evaluation = client
        .evaluate(&config)
        .context("profile evaluation failed")?;
    if args.check {
        println!(
            "Profile valid. Remote: {}:{} ({})",
            evaluation.remote_host, evaluation.remote_port, evaluation.remote_proto
        );
        println!("Username/password required: {}", !evaluation.autologin);
        println!(
            "Private key password required: {}",
            evaluation.private_key_password_required
        );
        println!("External PKI required: {}", evaluation.external_pki);
        println!(
            "Static challenge required: {}",
            !evaluation.static_challenge.is_empty()
        );
        return Ok(());
    }
    ensure!(
        !evaluation.external_pki,
        "external PKI/smart-card profiles are not supported"
    );
    tracing::info!(remote = %evaluation.remote_host, port = %evaluation.remote_port,
        protocol = %evaluation.remote_proto, username_password = !evaluation.autologin,
        "VPN profile loaded; authentication is verified by the server during connection");
    if evaluation.private_key_password_required {
        config.private_key_password = secret(
            "OVPN_KEY_PASS",
            "Private key password: ",
            args.non_interactive,
        )?;
        evaluation = client
            .evaluate(&config)
            .context("could not unlock private key")?;
    }
    let mut password_to_save = None;
    if !evaluation.autologin {
        let mut credentials = if let Some(path) = &args.auth_file {
            let content = std::fs::read_to_string(path).context("could not read auth file")?;
            let mut lines = content.lines();
            let user = lines
                .next()
                .filter(|s| !s.is_empty())
                .context("auth file must contain username on line 1")?;
            let password = lines
                .next()
                .context("auth file must contain password on line 2")?;
            Credentials::new(user, password)
        } else {
            let username = if let Some(user) = args.username.clone() {
                user
            } else if !evaluation.userlocked_username.is_empty() {
                evaluation.userlocked_username.clone()
            } else {
                ensure!(
                    !args.non_interactive,
                    "set --username/OVPN_USER or --auth-file"
                );
                eprint!("VPN username: ");
                io::stderr().flush()?;
                let mut user = String::new();
                io::stdin().read_line(&mut user)?;
                user.trim().to_owned()
            };
            let account = keychain_account(&profile, &username);
            let (password, should_save) = if let Ok(password) = std::env::var("OVPN_PASS") {
                (password, args.save_password)
            } else {
                match keychain_password(&account) {
                    Ok(Some(password)) => {
                        tracing::info!("Using VPN password from the system keychain");
                        (password, false)
                    }
                    Ok(None) => (
                        secret("OVPN_PASS", "VPN password: ", args.non_interactive)?,
                        args.save_password,
                    ),
                    Err(error) => {
                        tracing::debug!(%error, "Could not read the system keychain");
                        (
                            secret("OVPN_PASS", "VPN password: ", args.non_interactive)?,
                            args.save_password,
                        )
                    }
                }
            };
            if should_save {
                password_to_save = Some((account, password.clone()));
            }
            Credentials::new(username, password)
        };
        if !evaluation.static_challenge.is_empty() {
            credentials.response = secret(
                "OVPN_RESPONSE",
                "VPN challenge response: ",
                args.non_interactive,
            )?;
        }
        client
            .provide_credentials(&credentials)
            .context("could not set credentials")?;
    }
    let listener = TcpListener::bind(args.listen)
        .await
        .context("could not bind SOCKS5 port")?;
    tracing::info!(address = %listener.local_addr()?, "SOCKS5 listening; connections require VPN readiness");
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let mut proxy = tokio::spawn(socks::serve(
        listener,
        worker.tunnel(),
        args.max_connections.into(),
        shutdown_rx,
    ));
    let save_task = password_to_save.map(|(account, password)| {
        let diagnostics = diagnostics.clone();
        tokio::spawn(async move {
            diagnostics.wait_connected().await;
            match tokio::task::spawn_blocking(move || save_keychain_password(&account, &password))
                .await
            {
                Ok(Ok(())) => tracing::info!("VPN password saved in the system keychain"),
                Ok(Err(error)) => {
                    tracing::warn!(%error, "Could not save the VPN password in the system keychain")
                }
                Err(error) => tracing::warn!(%error, "Keychain task failed"),
            }
        })
    });
    let connecting = client.clone();
    let mut session = tokio::task::spawn_blocking(move || connecting.connect());
    let mut session_done = false;
    let result: Result<()> = tokio::select! {
        signal = tokio::signal::ctrl_c() => signal.context("could not receive interrupt"),
        result = &mut session => {
            session_done = true;
            match result { Ok(Ok(status)) => Err(anyhow::anyhow!(diagnostics.session_end(&status))), Ok(Err(error)) => Err(error.into()), Err(error) => Err(error.into()) }
        }
        result = &mut proxy => match result { Ok(Ok(())) => Ok(()), Ok(Err(error)) => Err(error.into()), Err(error) => Err(error.into()) },
    };
    let _ = shutdown_tx.send(true);
    client.stop();
    if !proxy.is_finished() {
        let _ = proxy.await;
    }
    if !session_done {
        let _ = session.await;
    }
    if let Some(save_task) = save_task {
        if diagnostics.was_connected() {
            let _ = save_task.await;
        } else {
            save_task.abort();
        }
    }
    drop(client);
    drop(worker);
    result
}

const KEYCHAIN_SERVICE: &str = "ovpnlane";

fn keychain_account(profile: &Path, username: &str) -> String {
    format!("{}\n{username}", profile.display())
}

fn keychain_password(account: &str) -> Result<Option<String>> {
    let entry =
        keyring::Entry::new(KEYCHAIN_SERVICE, account).context("could not open system keychain")?;
    match entry.get_password() {
        Ok(password) => Ok(Some(password)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(error) => Err(error).context("could not retrieve password from system keychain"),
    }
}

fn save_keychain_password(account: &str, password: &str) -> Result<()> {
    keyring::Entry::new(KEYCHAIN_SERVICE, account)
        .context("could not open system keychain")?
        .set_password(password)
        .context("could not store password in system keychain")
}

fn secret(variable: &str, prompt: &str, non_interactive: bool) -> Result<String> {
    if let Ok(value) = std::env::var(variable) {
        return Ok(value);
    }
    if non_interactive {
        bail!("set {variable} for non-interactive authentication");
    }
    rpassword::prompt_password(prompt).context("could not read password")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_does_not_require_or_accept_a_vpn_profile() {
        let args = Args::try_parse_from(["ovpnlane", "update", "--check"]).unwrap();
        assert!(matches!(args.command, Some(Command::Update(_))));
        assert!(args.config.is_none());
        assert!(
            Args::try_parse_from(["ovpnlane", "--config", "client.ovpn", "update", "--yes"])
                .is_err()
        );
    }

    #[test]
    fn connection_requires_a_profile_and_update_accepts_a_release_version() {
        assert!(Args::try_parse_from(["ovpnlane"]).is_err());
        assert!(Args::try_parse_from(["ovpnlane", "--config", "client.ovpn"]).is_ok());
        assert!(
            Args::try_parse_from(["ovpnlane", "update", "--version", "0.0.1", "--yes"]).is_ok()
        );
    }

    #[test]
    fn keychain_flag_conflicts_with_auth_file() {
        assert!(
            Args::try_parse_from([
                "ovpnlane",
                "--config",
                "client.ovpn",
                "--save-password",
                "--auth-file",
                "credentials.txt",
            ])
            .is_err()
        );
    }

    #[test]
    fn keychain_accounts_are_scoped_by_profile_and_username() {
        assert_ne!(
            keychain_account(Path::new("one.ovpn"), "alice"),
            keychain_account(Path::new("two.ovpn"), "alice")
        );
        assert_ne!(
            keychain_account(Path::new("one.ovpn"), "alice"),
            keychain_account(Path::new("one.ovpn"), "bob")
        );
    }
}
