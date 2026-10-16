use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;
use wayshare::auth::StaticPassword;
use wayshare::config::Settings;
use wayshare::rfb::clipboard::MemoryClipboard;
use wayshare::rfb::media::StubHpBackend;
use wayshare::rfb::srp::{self, VerifierRecord};
use wayshare::rfb::{self, AuthConfig, Config};

#[derive(Parser)]
#[command(
    version,
    about = "Screen Sharing server for the stock macOS client",
    args_conflicts_with_subcommands = true
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
    #[command(flatten)]
    serve: ServeArgs,
}

#[derive(clap::Args)]
struct ServeArgs {
    /// Config file (default: $XDG_CONFIG_HOME/wayshare/config.toml). Flags override it.
    #[arg(long)]
    config: Option<PathBuf>,
    #[arg(long)]
    listen: Option<SocketAddr>,
    /// SRP verifier file written by `wayshare set-password` (security type 36).
    #[arg(long)]
    srp_file: Option<PathBuf>,
    /// Username for the DH test password (security type 30, development only).
    #[arg(long, requires = "test_password_file")]
    username: Option<String>,
    /// Plaintext password file enabling DH auth, for development only.
    #[arg(long, requires = "username")]
    test_password_file: Option<PathBuf>,
    #[arg(long)]
    name: Option<String>,
    /// Wayland output to share (Linux), e.g. HEADLESS-1. Unset: test pattern.
    #[arg(long, alias = "capture")]
    output: Option<String>,
    #[arg(long, overrides_with = "no_hp")]
    hp: bool,
    #[arg(long)]
    no_hp: bool,
    /// UDP port for HP video (audio uses port + 2).
    #[arg(long)]
    hp_port: Option<u16>,
    /// HP video bitrate in bits per second.
    #[arg(long)]
    hp_bitrate: Option<u64>,
    #[arg(long, overrides_with = "no_input")]
    input: bool,
    #[arg(long)]
    no_input: bool,
    /// Resize the output to the viewer's requested mode (msg 29); restored afterwards.
    #[arg(long, alias = "resize-output", overrides_with = "no_resize")]
    resize: bool,
    #[arg(long)]
    no_resize: bool,
    #[arg(long, overrides_with = "no_mdns")]
    mdns: bool,
    #[arg(long)]
    no_mdns: bool,
    /// Experimental: offer Apple in-band stream encryption (client message 18, encoding 1103).
    #[arg(long)]
    stream_encryption: bool,
    /// Advertise HP with a stub backend that saves client messages 28/29 here and never answers.
    #[arg(long)]
    hp_stub_dir: Option<PathBuf>,
    /// Use the msg 28 key pair in the opposite order (diagnostic).
    #[arg(long)]
    hp_swap_keys: bool,
}

impl ServeArgs {
    fn apply(self, s: &mut Settings) {
        let flag = |on: bool, off: bool, cur: bool| if off { false } else { on || cur };
        if let Some(v) = self.listen {
            s.listen = v;
        }
        if self.srp_file.is_some() {
            s.srp_file = self.srp_file;
        }
        if self.name.is_some() {
            s.name = self.name;
        }
        if self.output.is_some() {
            s.output = self.output;
        }
        s.hp.enabled = flag(self.hp, self.no_hp, s.hp.enabled);
        s.input.enabled = flag(self.input, self.no_input, s.input.enabled);
        s.resize = flag(self.resize, self.no_resize, s.resize);
        s.mdns.enabled = flag(self.mdns, self.no_mdns, s.mdns.enabled);
        if let Some(v) = self.hp_port {
            s.hp.port = v;
        }
        if self.hp_bitrate.is_some() {
            s.hp.bitrate = self.hp_bitrate;
        }
        s.dev.stream_encryption |= self.stream_encryption;
        s.dev.hp_swap_keys |= self.hp_swap_keys;
        if self.hp_stub_dir.is_some() {
            s.dev.hp_stub_dir = self.hp_stub_dir;
        }
        if self.username.is_some() {
            s.dev.dh_username = self.username;
            s.dev.dh_password_file = self.test_password_file;
        }
    }
}

#[derive(Subcommand)]
enum Command {
    /// Store an SRP verifier (salt, iterations, v) for a user. The password is not stored.
    SetPassword {
        #[arg(long)]
        user: String,
        /// Read the password from this file instead of prompting.
        #[arg(long)]
        password_file: Option<PathBuf>,
        /// Defaults to $XDG_CONFIG_HOME/wayshare/srp-<user>.
        #[arg(long)]
        srp_file: Option<PathBuf>,
        #[arg(long, default_value_t = srp::DEFAULT_ITERATIONS)]
        iterations: u32,
    },
    /// Write a systemd user unit that runs this binary (for installs without a package).
    InstallService,
}

fn read_password(path: Option<&Path>) -> Result<Vec<u8>> {
    let pw = match path {
        Some(p) => {
            let raw = std::fs::read(p).with_context(|| format!("reading {}", p.display()))?;
            let end = raw
                .iter()
                .rposition(|&b| b != b'\n' && b != b'\r')
                .map_or(0, |i| i + 1);
            raw[..end].to_vec()
        }
        None => {
            let a = rpassword::prompt_password("Password: ")?;
            let b = rpassword::prompt_password("Again: ")?;
            if a != b {
                bail!("passwords do not match");
            }
            a.into_bytes()
        }
    };
    if pw.is_empty() {
        bail!("empty password");
    }
    Ok(pw)
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stdout()))
        .init();
    let cli = Cli::parse();

    if let Some(Command::InstallService) = cli.command {
        let path = wayshare::service::install()?;
        println!("wrote {}", path.display());
        let reload = std::process::Command::new("systemctl")
            .args(["--user", "daemon-reload"])
            .status();
        if !reload.is_ok_and(|s| s.success()) {
            warn!("systemctl --user daemon-reload failed; run it before starting wayshare");
        }
        return Ok(());
    }

    if let Some(Command::SetPassword {
        user,
        password_file,
        srp_file,
        iterations,
    }) = cli.command
    {
        let path = match srp_file {
            Some(p) => p,
            None => wayshare::config::default_srp_path(&user)?,
        };
        let password = read_password(password_file.as_deref())?;
        VerifierRecord::generate(&user, &password, iterations).save(&path)?;
        println!("wrote SRP verifier for {user} to {}", path.display());
        return Ok(());
    }

    let config_path = cli.serve.config.clone();
    let mut settings = Settings::load(config_path.as_deref())?;
    cli.serve.apply(&mut settings);
    settings.validate()?;
    run(settings).await
}

fn hostname() -> String {
    std::fs::read_to_string("/etc/hostname")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| std::env::var("HOSTNAME").ok())
        .unwrap_or_else(|| "wayshare".into())
}

async fn run(s: Settings) -> Result<()> {
    let mut auth = AuthConfig::default();
    let srp_path = s.srp_path()?;
    match VerifierRecord::load(&srp_path) {
        Ok(rec) => auth.srp = Some(rec),
        Err(e) if s.srp_file.is_some() || s.dev.dh_username.is_none() => {
            return Err(e.context("run `wayshare set-password --user <name>` first"));
        }
        Err(_) => {}
    }
    if let (Some(user), Some(path)) = (&s.dev.dh_username, &s.dev.dh_password_file) {
        auth.dh = Some(Arc::new(StaticPassword::from_file(user.clone(), path)?));
    }
    let host = hostname();
    let name = s.name.clone().unwrap_or_else(|| host.clone());
    let mut cfg = Config::new(name.clone(), 1920, 1080, auth);
    cfg.clipboard = Some(Arc::new(MemoryClipboard::default()));
    cfg.stream_encryption = s.dev.stream_encryption;
    if let Some(dir) = &s.dev.hp_stub_dir {
        cfg.hp = Some(Arc::new(StubHpBackend::new(dir.clone(), s.hp.port)?));
    }
    #[cfg(target_os = "linux")]
    if let Some(output) = &s.output {
        wayshare::display::hyprland::Hyprland::from_env()?.check_version()?;
        cfg.frame_source = wayshare::server_linux::capture_factory(Some(output.clone()));
        cfg.cursor_source = wayshare::server_linux::cursor_factory(Some(output.clone()));
        match wayshare::clipboard::wayland::WaylandClipboard::spawn() {
            Ok(cb) => {
                let backend: Arc<dyn wayshare::clipboard::ClipboardBackend> = Arc::new(cb);
                cfg.clipboard = Some(Arc::new(wayshare::clipboard::TextBridge::new(
                    backend.clone(),
                )));
                cfg.clipboard_backend = Some(backend);
            }
            Err(e) => warn!("Wayland clipboard unavailable, text-only clipboard: {e:#}"),
        }
        if s.input.enabled {
            cfg.input = Some(wayshare::server_linux::input_factory(
                output.clone(),
                s.keyboard()?,
            ));
        }
        if s.resize {
            cfg.display = Some(Arc::new(wayshare::server_linux::HyprDisplay::new(
                output.clone(),
            )));
        }
        if s.hp.enabled && s.dev.hp_stub_dir.is_none() {
            match wayshare::server_linux::hp_encoder(&s.video_encoder()) {
                Ok(backend) => {
                    info!(?backend, "HP encoder");
                    cfg.hp = Some(Arc::new(wayshare::server_linux::LinuxHp::new(
                        Some(output.clone()),
                        s.hp.port,
                        s.video_encoder(),
                        s.hp.bitrate.is_some(),
                        s.hp.audio,
                        s.dev.hp_swap_keys,
                    )));
                }
                Err(e) => warn!("High Performance mode disabled: {e:#}"),
            }
        }
    }
    #[cfg(not(target_os = "linux"))]
    if s.output.is_some() {
        bail!("output capture is only supported on Linux");
    }
    info!(
        listen = %s.listen,
        output = ?s.output,
        hp = cfg.hp.is_some(),
        input = cfg.input.is_some(),
        resize = cfg.display.is_some(),
        "starting"
    );
    let _mdns = if s.mdns.enabled {
        match wayshare::mdns::advertise(
            &name,
            &host,
            s.listen.port(),
            s.mdns.device_model.as_deref(),
        ) {
            Ok(a) => Some(a),
            Err(e) => {
                warn!("mDNS advertisement failed: {e:#}");
                None
            }
        }
    } else {
        None
    };
    rfb::serve(s.listen, Arc::new(cfg), shutdown_signal()).await
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut term = signal(SignalKind::terminate()).expect("SIGTERM handler");
        tokio::select! {
            _ = term.recv() => info!("SIGTERM"),
            _ = tokio::signal::ctrl_c() => info!("SIGINT"),
        }
    }
    #[cfg(not(unix))]
    let _ = tokio::signal::ctrl_c().await;
}
