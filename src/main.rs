//! Boothmate: portable companion for grandMA3.

mod cdj;
mod config;
mod log;
mod midi;
mod osc;
mod plugin;
mod profile;
mod state;
mod tempo;
mod timecode;
mod web;

use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};
use tokio::sync::{watch, Notify};

pub struct App {
    /// Data folder (next to the binary).
    pub dir: PathBuf,
    pub config: config::Config,
    pub profiles: Vec<profile::Profile>,
    pub profile: RwLock<profile::Profile>,
    pub state: Mutex<state::State>,
    /// Bumped on every state change; the web UI listens to it.
    pub changes: watch::Sender<u64>,
    /// Asks the plugin to send a fresh page snapshot.
    pub resync: Notify,
    /// Messages from the test controller in the web UI.
    pub midi_test: tokio::sync::mpsc::UnboundedSender<Vec<u8>>,
    /// Events from the DJ Link module, for the tempo link.
    pub cdj_events: tokio::sync::broadcast::Sender<Arc<Vec<cdj::tracker::Event>>>,
    /// JSON lines for the Lua plugin (speed and phase).
    pub plugin_ops: tokio::sync::broadcast::Sender<String>,
    /// Capture file to replay as DJ Link traffic (`--replay`).
    pub cdj_replay: Option<PathBuf>,
    /// Socket for OSC input to grandMA3 (executor keys and faders).
    pub osc_out: std::net::UdpSocket,
}

impl App {
    pub fn update(&self, f: impl FnOnce(&mut state::State)) {
        f(&mut self.state.lock().unwrap());
        self.changes.send_modify(|n| *n = n.wrapping_add(1));
    }
}

const USAGE: &str = "Usage: boothmate [--dir <data folder>] [--verbose] [--replay <capture>]
       boothmate --check <capture>...

--replay plays a DJ Link capture (pcap/pcapng, e.g. logs/djlink-*.pcap) as if the devices were
on the network. --check prints what Boothmate understands in a capture.

Reads boothmate.toml, midi.toml, ma-profiles/, devices/ and lua/ from the data folder. The default data folder is
the folder that holds the binary.";

/// The data folder: `--dir`, else next to the binary. During development (`cargo run`) the
/// binary sits in target/, so fall back to the current folder when it has `ma-profiles/`.
fn data_dir(arg: Option<PathBuf>) -> PathBuf {
    if let Some(d) = arg {
        return d;
    }
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()));
    if let Some(d) = &exe_dir {
        if d.join("ma-profiles").is_dir() {
            return d.clone();
        }
    }
    let cwd = std::env::current_dir().unwrap_or_default();
    if cwd.join("ma-profiles").is_dir() {
        return cwd;
    }
    exe_dir.unwrap_or(cwd)
}

fn main() {
    let mut dir = None;
    let mut verbose = false;
    let mut replay = None;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--dir" => dir = args.next().map(PathBuf::from),
            "-v" | "--verbose" => verbose = true,
            "--replay" => replay = args.next().map(PathBuf::from),
            "--check" => {
                let mut failed = false;
                for f in args.by_ref() {
                    match std::fs::read(&f)
                        .map_err(|e| format!("{f}: {e}"))
                        .and_then(|d| cdj::check::report(&f, &d))
                    {
                        Ok((r, problems)) => {
                            println!("{r}");
                            failed |= problems > 0;
                        }
                        Err(e) => {
                            eprintln!("error: {e}");
                            failed = true;
                        }
                    }
                }
                std::process::exit(failed as i32);
            }
            "-V" | "--version" => {
                println!("boothmate {}", env!("CARGO_PKG_VERSION"));
                return;
            }
            _ => {
                println!("{USAGE}");
                return;
            }
        }
    }
    let dir = data_dir(dir);
    if let Err(e) = run(dir, verbose, replay) {
        eprintln!("error: {e}");
        // Keep the window open when started by double-click on Windows.
        if cfg!(windows) {
            eprintln!("Press Enter to close.");
            let _ = std::io::stdin().read_line(&mut String::new());
        }
        std::process::exit(1);
    }
}

fn run(dir: PathBuf, verbose: bool, cdj_replay: Option<PathBuf>) -> Result<(), String> {
    log::init(&dir, verbose);
    log::info(&format!(
        "Boothmate {} - data folder {}",
        env!("CARGO_PKG_VERSION"),
        dir.display()
    ));
    let config = config::Config::load_or_create(&dir)?;
    let profiles = profile::load_all(&dir.join("ma-profiles"))?;
    let (start, _) = profile::select(&profiles, &config.ma.profile, None);
    let start = start.clone();

    let mut st = state::State::default();
    st.connections.osc_listen = config.osc.listen.clone();
    st.connections.plugin_addr = format!("{}:{}", config.plugin.host, config.plugin.port);
    st.connections.profile = start.id.clone();

    let osc_out = std::net::UdpSocket::bind("0.0.0.0:0")
        .map_err(|e| format!("cannot open OSC output socket: {e}"))?;
    let (midi_test, midi_test_rx) = tokio::sync::mpsc::unbounded_channel();
    let app = Arc::new(App {
        dir: dir.clone(),
        config,
        profiles,
        profile: RwLock::new(start),
        state: Mutex::new(st),
        changes: watch::channel(0).0,
        resync: Notify::new(),
        midi_test,
        cdj_replay,
        cdj_events: tokio::sync::broadcast::channel(256).0,
        plugin_ops: tokio::sync::broadcast::channel(64).0,
        osc_out,
    });

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    rt.block_on(async move {
        let web = tokio::net::TcpListener::bind(&app.config.web.listen)
            .await
            .map_err(|e| format!("cannot start web UI on {}: {e}", app.config.web.listen))?;
        let url = web_url(&app.config.web.listen);
        log::info(&format!("Web UI: {url}"));
        println!("\n  Boothmate is running. Open {url} in your browser. Ctrl+C to quit.\n");

        tokio::spawn(osc::run(app.clone()));
        tokio::spawn(plugin::run(app.clone()));
        tokio::spawn(midi::run(app.clone(), midi_test_rx));
        tokio::spawn(cdj::run(app.clone()));
        tokio::spawn(tempo::run(app.clone()));
        tokio::spawn(timecode::run(app.clone()));
        let server = axum::serve(web, web::router(app.clone()));
        tokio::select! {
            r = server => r.map_err(|e| format!("web UI: {e}")),
            _ = tokio::signal::ctrl_c() => {
                log::info("stopped");
                Ok(())
            }
        }
    })
}

fn web_url(listen: &str) -> String {
    let local = listen.replace("0.0.0.0", "127.0.0.1");
    format!("http://{local}/")
}
