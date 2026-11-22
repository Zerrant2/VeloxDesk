//! VeloxDesk client and host application (`veloxdesk`).

mod app;
mod gamepad;
mod input;
mod latest;
mod session;
mod stats;
mod viewer;

use std::process::ExitCode;

use clap::{Parser, ValueEnum};
use tracing_subscriber::EnvFilter;
use veloxdesk_render::winit::event_loop::EventLoop;
use veloxdesk_ui::{Mode, Present, Settings};

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
#[command(name = "veloxdesk", version, about = "VeloxDesk low-latency remote desktop")]
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

    /// Host options (used with `--host`).
    #[cfg(feature = "net")]
    #[command(flatten)]
    host_args: veloxdesk_service::HostArgs,
}

fn init_logging() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,wgpu=warn,naga=warn"));
    let _ = tracing_subscriber::fmt().with_env_filter(filter).with_target(false).try_init();
}

#[cfg(feature = "net")]
fn run_host_cli(
    mut args: veloxdesk_service::HostArgs,
    rendezvous: Option<String>,
    password: Option<String>,
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
        env!("CARGO_PKG_VERSION"),
        cfg.bind,
        veloxdesk_ui::format_device_id(&id.device_id()),
    );
    if let Some(rdv) = cfg.rendezvous {
        println!("  rendezvous: {rdv}");
    }
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    rt.block_on(veloxdesk_service::run_host(cfg, id))?;
    Ok(())
}

#[cfg(not(feature = "net"))]
fn run_host_cli() -> anyhow::Result<()> {
    anyhow::bail!("--host requires the `net` feature (rebuild with `--features net`)")
}

fn main() -> ExitCode {
    init_logging();
    let cli = Cli::parse();

    if cli.host {
        #[cfg(feature = "net")]
        let res = run_host_cli(cli.host_args, cli.rendezvous, cli.password);
        #[cfg(not(feature = "net"))]
        let res = run_host_cli();
        return match res {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("host error: {e:#}");
                ExitCode::FAILURE
            }
        };
    }

    let mut settings = Settings::load_or_default();
    if cli.hud {
        settings.show_hud = true;
    }
    if cli.gaming {
        settings.mode = Mode::Gaming;
    }
    if cli.sw_decode {
        settings.prefer_hw_decode = false;
    }
    if cli.mic {
        settings.mic_enabled = true;
    }
    #[cfg(feature = "net")]
    {
        if cli.host_args.no_audio {
            settings.audio_enabled = false;
        }
        if cli.host_args.hdr {
            settings.hdr_10bit = true;
        }
        if cli.host_args.yuv444 {
            settings.yuv444 = true;
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
}
