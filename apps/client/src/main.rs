//! VeloxDesk client and host application (`veloxdesk`).

mod app;
mod embedded_host;
mod gamepad;
mod input;
mod latest;
mod session;
mod stats;
mod tailscale;
mod tray;
mod updater;
mod viewer;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, ValueEnum};
use tracing_subscriber::EnvFilter;
use veloxdesk_render::winit::event_loop::EventLoop;
use veloxdesk_ui::{Mode, Present, Settings, TunnelSpec, parse_tunnel_spec};

use crate::app::{App, Launch, TestPatternOpts, UserEvent, ViewerCliOpts};

pub(crate) fn device_id_label() -> String {
    viewer::device_id_label()
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum PresentArg {
    Mailbox,
    Immediate,
    Fifo,
}

impl PresentArg {
    fn to_ui(self) -> Present {
        match self {
            Self::Mailbox => Present::Mailbox,
            Self::Immediate => Present::Immediate,
            Self::Fifo => Present::Fifo,
        }
    }
}

#[derive(Debug, Parser)]
#[command(
    name = "veloxdesk",
    version = env!("VELOXDESK_BUILD_DISPLAY"),
    about = "VeloxDesk low-latency remote desktop"
)]
struct Cli {
    /// Run as host (stream this desktop to connecting viewers).
    #[arg(long, conflicts_with_all = ["connect", "test_pattern"])]
    host: bool,

    /// Connect to a host by 9-digit Device ID or direct address (`ip[:port]`).
    #[arg(long, value_name = "ID_OR_ADDR", conflicts_with_all = ["host", "test_pattern"])]
    connect: Option<String>,

    /// Rendezvous server address (`host[:port]`, default port 21116) for Device ID connections.
    #[arg(long, value_name = "ADDR")]
    rendezvous: Option<String>,

    /// Password or one-time password for viewer authentication.
    #[arg(long, value_name = "PASSWORD")]
    password: Option<String>,

    /// Force zero-knowledge relay fallback when connecting by Device ID.
    #[arg(long)]
    relay_only: bool,

    /// Present a synthetic NV12 test pattern to benchmark the renderer.
    #[arg(long, conflicts_with_all = ["host", "connect"])]
    test_pattern: bool,

    /// Test-pattern width in pixels.
    #[arg(long, default_value_t = 1920)]
    width: u32,

    /// Test-pattern height in pixels.
    #[arg(long, default_value_t = 1080)]
    height: u32,

    /// Test-pattern target FPS (0 = uncapped).
    #[arg(long, default_value_t = 120)]
    fps: u32,

    /// Exit test-pattern mode after presenting N frames and print stats.
    #[arg(long)]
    frames: Option<u32>,

    /// Override presentation mode (`mailbox`, `immediate`, `fifo`).
    #[arg(long, value_enum)]
    present: Option<PresentArg>,

    /// Start with the Performance HUD visible.
    #[arg(long)]
    hud: bool,

    /// Start in Gaming mode (relative mouse, separate cursor).
    #[arg(long)]
    gaming: bool,

    /// Force software video decoding on the viewer.
    #[arg(long)]
    sw_decode: bool,

    /// Stream local microphone audio to the host (track 1).
    #[arg(long)]
    mic: bool,

    /// Forward a local TCP port through the remote host (`local_port:remote_host:remote_port`, repeatable).
    #[arg(long = "tunnel", value_name = "SPEC")]
    tunnels: Vec<String>,

    /// Query the OS host service status, active desktop kind, and local daemon IPC status.
    #[arg(
        long,
        conflicts_with_all = ["host", "connect", "test_pattern", "service_install", "service_uninstall", "service_run"]
    )]
    service_status: bool,

    /// Install VeloxDesk as an OS system service (Windows SCM / Linux systemd).
    #[arg(
        long,
        conflicts_with_all = ["host", "connect", "test_pattern", "service_status", "service_uninstall", "service_run"]
    )]
    service_install: bool,

    /// Uninstall the VeloxDesk OS system service.
    #[arg(
        long,
        conflicts_with_all = ["host", "connect", "test_pattern", "service_status", "service_install", "service_run"]
    )]
    service_uninstall: bool,

    /// Service entry point invoked by the OS service manager (implies `--host`).
    #[arg(
        long,
        conflicts_with_all = ["connect", "test_pattern", "service_status", "service_install", "service_uninstall"]
    )]
    service_run: bool,

    /// Portable mode: store config, settings, and identity next to the executable (also provided by `HostArgs` when `net` is enabled).
    #[cfg(not(feature = "net"))]
    #[arg(long)]
    portable: bool,

    /// Optional configuration file path (provided by `HostArgs` when `net` is enabled).
    #[cfg(not(feature = "net"))]
    #[arg(long = "config", value_name = "FILE")]
    config: Option<PathBuf>,

    /// Host options (used with `--host`, includes `--config` and `--portable`).
    #[cfg(feature = "net")]
    #[command(flatten)]
    host_args: veloxdesk_service::HostArgs,
}

impl Cli {
    fn is_portable(&self) -> bool {
        #[cfg(feature = "net")]
        {
            self.host_args.portable
        }
        #[cfg(not(feature = "net"))]
        {
            self.portable
        }
    }
}

fn portable_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(std::path::Path::to_path_buf))
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."))
}

fn parse_cli_tunnels(raw: &[String]) -> anyhow::Result<Vec<TunnelSpec>> {
    raw.iter()
        .map(|s| parse_tunnel_spec(s).map_err(|e| anyhow::anyhow!("invalid --tunnel `{s}`: {e}")))
        .collect()
}

fn init_logging() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,wgpu=warn,naga=warn"));
    let _ = tracing_subscriber::fmt().with_env_filter(filter).with_target(false).try_init();
}

#[cfg(feature = "net")]
fn run_service_status() -> anyhow::Result<()> {
    let status = veloxdesk_service::service_status().map_err(anyhow::Error::from)?;
    let desktop = veloxdesk_service::detect_desktop();
    println!("service: {status}");
    println!("desktop: {desktop}");
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    let addr = std::net::SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, veloxdesk_service::DEFAULT_IPC_PORT));
    match rt.block_on(veloxdesk_service::IpcClient::request(addr, &veloxdesk_service::IpcRequest::Status)) {
        Ok(veloxdesk_service::IpcResponse::Status { device_id, active_otp, active_sessions, desktop }) => {
            println!("ipc: connected ({addr})");
            println!("  device_id: {}", veloxdesk_ui::format_device_id(&device_id));
            if let Some(otp) = active_otp {
                println!("  active_otp: {otp}");
            }
            println!("  active_sessions: {active_sessions}");
            println!("  host_desktop: {desktop}");
        }
        Ok(veloxdesk_service::IpcResponse::Error { message }) => {
            println!("ipc: error ({message})");
        }
        Ok(other) => {
            println!("ipc: {other:?}");
        }
        Err(_) => {
            println!("ipc: not running");
        }
    }
    Ok(())
}

#[cfg(feature = "net")]
fn run_service_install(args: &veloxdesk_service::HostArgs, rendezvous: Option<&str>) -> anyhow::Result<()> {
    let exe = std::env::current_exe().map_err(anyhow::Error::from)?;
    let mut extra = Vec::new();
    if args.portable {
        extra.push("--portable".into());
    }
    if let Some(cfg) = &args.config {
        extra.push("--config".into());
        extra.push(cfg.display().to_string());
    }
    if let Some(bind) = args.bind {
        extra.push("--bind".into());
        extra.push(bind.to_string());
    }
    if let Some(rdv) = rendezvous {
        extra.push("--rendezvous".into());
        extra.push(rdv.to_string());
    }
    let msg = veloxdesk_service::install_service(&exe, &extra).map_err(anyhow::Error::from)?;
    if !msg.is_empty() {
        println!("{msg}");
    }
    Ok(())
}

#[cfg(feature = "net")]
fn run_service_uninstall() -> anyhow::Result<()> {
    let msg = veloxdesk_service::uninstall_service().map_err(anyhow::Error::from)?;
    if !msg.is_empty() {
        println!("{msg}");
    }
    Ok(())
}

#[cfg(not(feature = "net"))]
fn run_service_unavailable(flag: &str) -> anyhow::Result<()> {
    anyhow::bail!("{flag} requires the `net` feature (rebuild with `--features net`)")
}

#[cfg(feature = "net")]
fn run_host_cli(
    mut args: veloxdesk_service::HostArgs,
    rendezvous: Option<String>,
    password: Option<String>,
    service_run: bool,
) -> anyhow::Result<()> {
    if let Some(rdv) = rendezvous {
        args.rendezvous = Some(veloxdesk_ui::parse_rendezvous_addr(&rdv).map_err(anyhow::Error::msg)?);
    }
    if password.is_some() {
        args.password = password;
    }
    let cfg = args.resolve()?;
    let id = viewer::load_identity(cfg.identity_path.as_deref())?;
    println!(
        "VeloxDesk host {} listening on {} (device ID: {})",
        env!("VELOXDESK_BUILD_DISPLAY"),
        cfg.bind,
        veloxdesk_ui::format_device_id(&id.device_id()),
    );
    if cfg.portable {
        println!("  mode: portable");
    }
    if let Some(rdv) = cfg.rendezvous {
        println!("  rendezvous: {rdv}");
    }
    if service_run {
        veloxdesk_service::daemon::run_service_or_console(move |stop_rx| {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .map_err(veloxdesk_core_traits::CoreError::Io)?;
            rt.block_on(async move {
                let running = veloxdesk_service::RunningHost::start(
                    cfg,
                    id,
                    veloxdesk_service::HostBackends::system(),
                    None,
                )
                .await?;
                veloxdesk_service::daemon::wait_for_shutdown(stop_rx).await;
                running.stop();
                running.join().await?;
                Ok(())
            })
        })
        .map_err(anyhow::Error::from)?;
        return Ok(());
    }
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    rt.block_on(veloxdesk_service::run_host(cfg, id))?;
    Ok(())
}

#[cfg(not(feature = "net"))]
fn run_host_cli() -> anyhow::Result<()> {
    anyhow::bail!("--host requires the `net` feature (rebuild with `--features net`)")
}

fn exit_from_result(label: &str, res: anyhow::Result<()>) -> ExitCode {
    match res {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{label}: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn main() -> ExitCode {
    init_logging();
    let cli = Cli::parse();

    if cli.service_status {
        #[cfg(feature = "net")]
        return exit_from_result("service-status error", run_service_status());
        #[cfg(not(feature = "net"))]
        return exit_from_result("service-status error", run_service_unavailable("--service-status"));
    }
    if cli.service_install {
        #[cfg(feature = "net")]
        return exit_from_result(
            "service-install error",
            run_service_install(&cli.host_args, cli.rendezvous.as_deref()),
        );
        #[cfg(not(feature = "net"))]
        return exit_from_result("service-install error", run_service_unavailable("--service-install"));
    }
    if cli.service_uninstall {
        #[cfg(feature = "net")]
        return exit_from_result("service-uninstall error", run_service_uninstall());
        #[cfg(not(feature = "net"))]
        return exit_from_result("service-uninstall error", run_service_unavailable("--service-uninstall"));
    }

    if cli.host || cli.service_run {
        #[cfg(feature = "net")]
        let res = run_host_cli(cli.host_args, cli.rendezvous, cli.password, cli.service_run);
        #[cfg(not(feature = "net"))]
        let res = run_host_cli();
        return exit_from_result("host error", res);
    }

    let parsed_tunnels = match parse_cli_tunnels(&cli.tunnels) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("{e:#}");
            return ExitCode::FAILURE;
        }
    };

    let (mut settings, settings_path) = if cli.is_portable() {
        let dir = portable_dir();
        viewer::set_identity_override(dir.join("identity.key"));
        let path = dir.join("veloxdesk-settings.json");
        let loaded = Settings::load(&path).unwrap_or_default();
        (loaded, Some(path))
    } else {
        (Settings::load_or_default(), None)
    };
    if cli.gaming {
        settings.set_mode(Mode::Gaming);
    }
    let active = settings.active_mut();
    if cli.hud {
        active.show_hud = true;
    }
    if cli.sw_decode {
        active.prefer_hw_decode = false;
    }
    if cli.mic {
        active.mic_enabled = true;
    }
    #[cfg(feature = "net")]
    {
        if cli.host_args.no_audio {
            active.audio_enabled = false;
        }
        if cli.host_args.hdr {
            active.hdr_10bit = true;
        }
        if cli.host_args.yuv444 {
            active.yuv444 = true;
        }
    }

    let launch = if cli.test_pattern {
        Launch::TestPattern(TestPatternOpts {
            width: cli.width,
            height: cli.height,
            fps: cli.fps,
            frames: cli.frames,
        })
    } else if let Some(addr) = cli.connect {
        Launch::Connect(addr)
    } else {
        Launch::Gui
    };

    let event_loop = match EventLoop::<UserEvent>::with_user_event().build() {
        Ok(el) => el,
        Err(e) => {
            eprintln!("cannot create window event loop: {e}");
            return ExitCode::FAILURE;
        }
    };
    let viewer_opts = ViewerCliOpts {
        rendezvous: cli.rendezvous,
        password: cli.password,
        relay_only: cli.relay_only,
        tunnels: parsed_tunnels,
        settings_path,
    };
    let proxy = event_loop.create_proxy();
    let mut app = App::new(launch, proxy, settings, cli.present.map(PresentArg::to_ui), viewer_opts);
    if let Err(e) = event_loop.run_app(&mut app) {
        eprintln!("event loop error: {e}");
        return ExitCode::FAILURE;
    }
    if app.exit_code == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(app.exit_code as u8)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_parses_modes() {
        let gui = Cli::try_parse_from(["veloxdesk"]).unwrap();
        assert!(!gui.host && gui.connect.is_none() && !gui.test_pattern);

        let conn = Cli::try_parse_from(["veloxdesk", "--connect", "127.0.0.1:21118", "--hud", "--gaming"]).unwrap();
        assert_eq!(conn.connect.as_deref(), Some("127.0.0.1:21118"));
        assert!(conn.hud && conn.gaming);

        let tp = Cli::try_parse_from([
            "veloxdesk",
            "--test-pattern",
            "--width",
            "1280",
            "--height",
            "720",
            "--fps",
            "0",
            "--frames",
            "60",
            "--present",
            "immediate",
        ])
        .unwrap();
        assert!(tp.test_pattern);
        assert_eq!((tp.width, tp.height, tp.fps, tp.frames), (1280, 720, 0, Some(60)));
        assert!(matches!(tp.present, Some(PresentArg::Immediate)));

        let p2p = Cli::try_parse_from([
            "veloxdesk",
            "--connect",
            "123 456 789",
            "--rendezvous",
            "127.0.0.1:21116",
            "--password",
            "s3cr3t",
            "--relay-only",
        ])
        .unwrap();
        assert_eq!(p2p.connect.as_deref(), Some("123 456 789"));
        assert_eq!(p2p.rendezvous.as_deref(), Some("127.0.0.1:21116"));
        assert_eq!(p2p.password.as_deref(), Some("s3cr3t"));
        assert!(p2p.relay_only);

        assert!(Cli::try_parse_from(["veloxdesk", "--host", "--connect", "127.0.0.1"]).is_err());
    }

    #[cfg(feature = "net")]
    #[test]
    fn cli_parses_host_flags() {
        let mut host = Cli::try_parse_from([
            "veloxdesk",
            "--host",
            "--bind",
            "127.0.0.1:21118",
            "--rendezvous",
            "127.0.0.1:21116",
            "--password",
            "hunter2",
            "--otp",
            "auto",
            "--require-accept",
            "--max-fps",
            "120",
            "--bitrate",
            "30000",
            "--codec",
            "h264",
            "--no-hw",
        ])
        .unwrap();
        assert!(host.host);
        host.host_args.rendezvous = host.rendezvous.as_deref().map(|s| veloxdesk_ui::parse_rendezvous_addr(s).unwrap());
        host.host_args.password = host.password.clone();
        let cfg = host.host_args.resolve().unwrap();
        assert_eq!(cfg.bind, "127.0.0.1:21118".parse().unwrap());
        assert_eq!(cfg.rendezvous, Some("127.0.0.1:21116".parse().unwrap()));
        assert!(cfg.auth.permanent_password_hash.is_some());
        assert!(cfg.auth.auto_otp);
        assert!(cfg.auth.require_accept);
        assert_eq!(cfg.max_fps, 120);
        assert_eq!(cfg.bitrate_kbps, 30000);
        assert!(!cfg.prefer_hw);
    }

    #[test]
    fn cli_m4_flags_and_no_clap_collisions() {
        use clap::CommandFactory;
        Cli::command().debug_assert();

        let conn = Cli::try_parse_from([
            "veloxdesk",
            "--connect",
            "127.0.0.1:21118",
            "--portable",
            "--tunnel",
            "8080:127.0.0.1:80",
            "--tunnel",
            "5432:db.internal:5432",
        ])
        .unwrap();
        assert!(conn.is_portable());
        let specs = parse_cli_tunnels(&conn.tunnels).unwrap();
        assert_eq!(specs.len(), 2);
        assert_eq!(specs[0].local_port, 8080);
        assert_eq!(specs[0].remote_host, "127.0.0.1");
        assert_eq!(specs[0].remote_port, 80);
        assert_eq!(specs[1].local_port, 5432);
        assert_eq!(specs[1].remote_host, "db.internal");
        assert_eq!(specs[1].remote_port, 5432);
        assert!(parse_cli_tunnels(&["not-a-tunnel".into()]).is_err());

        let svc_st = Cli::try_parse_from(["veloxdesk", "--service-status"]).unwrap();
        assert!(svc_st.service_status);
        let svc_in = Cli::try_parse_from(["veloxdesk", "--service-install", "--portable"]).unwrap();
        assert!(svc_in.service_install && svc_in.is_portable());
        let svc_un = Cli::try_parse_from(["veloxdesk", "--service-uninstall"]).unwrap();
        assert!(svc_un.service_uninstall);
        let svc_run = Cli::try_parse_from(["veloxdesk", "--host", "--service-run", "--config", "veloxdesk.toml"]).unwrap();
        assert!(svc_run.host && svc_run.service_run);

        assert!(Cli::try_parse_from(["veloxdesk", "--service-status", "--host"]).is_err());
        assert!(Cli::try_parse_from(["veloxdesk", "--service-install", "--service-uninstall"]).is_err());
    }
}
