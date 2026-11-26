//! CDJ module: Pro DJ Link, read-only.
//!
//! Stage 1: listens for keep-alives (UDP 50000) and beat and position packets (UDP 50001). This
//! needs nothing from Boothmate's side. Stage 2: announces Boothmate as a silent virtual player
//! with a device number outside the player range (7 and up), so players also send it their
//! status (UDP 50002). Stage 3 (`metadata`): asks the database of the player that holds the
//! media about the loaded track (TCP 12523 and the port it names). Boothmate never sends
//! anything else: no media browsing, no track loading, no player control.

pub mod analysis;
pub mod check;
pub mod dbserver;
pub mod metadata;
pub mod packet;
pub mod pcap;
pub mod replay;
pub mod sim;
pub mod simdb;
pub mod tracker;

use crate::state::now_ms;
use crate::App;
use packet::{Kind, Packet, ANNOUNCE_PORT, BEAT_PORT, STATUS_PORT};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::UdpSocket;
use tracker::{CdjState, Event};

/// Name of the virtual player on the network.
pub const VCDJ_NAME: &str = "Boothmate";
const KEEP_ALIVE_MS: u64 = 1_500;
const RETRY_BIND_MS: u64 = 5_000;
/// Device numbers the virtual player may use: outside the players (1-6), below rekordbox (17+).
const VCDJ_RANGE: std::ops::RangeInclusive<u8> = 7..=15;

/// The network interface DJ Link runs on.
#[derive(Debug, Clone)]
struct Iface {
    name: String,
    ip: Ipv4Addr,
    mac: [u8; 6],
}

impl Iface {
    fn label(&self) -> String {
        format!("{} {}", self.name, self.ip)
    }
}

/// Finds the interface by name or IP (`want`), or the one whose subnet holds `peer`.
fn find_iface(want: &str, peer: Option<Ipv4Addr>) -> Option<Iface> {
    for i in netdev::get_interfaces() {
        let mac = i.mac_addr.map(|m| m.octets()).unwrap_or_default();
        for net in &i.ipv4 {
            let hit = if want.is_empty() {
                peer.is_some_and(|p| net.contains(&p))
            } else {
                i.name == want
                    || i.friendly_name.as_deref() == Some(want)
                    || net.addr().to_string() == want
            };
            if hit {
                return Some(Iface {
                    name: i.friendly_name.clone().unwrap_or_else(|| i.name.clone()),
                    ip: net.addr(),
                    mac,
                });
            }
        }
    }
    None
}

fn bind(port: u16) -> std::io::Result<UdpSocket> {
    use socket2::{Domain, Protocol, Socket, Type};
    let s = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    // Share the ports with other DJ Link software where the OS allows it.
    s.set_reuse_address(true)?;
    #[cfg(all(unix, not(target_os = "solaris"), not(target_os = "illumos")))]
    s.set_reuse_port(true)?;
    s.set_broadcast(true)?;
    s.set_nonblocking(true)?;
    s.bind(&SocketAddr::from(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, port)).into())?;
    UdpSocket::from_std(s.into())
}

async fn recv(s: &Option<UdpSocket>, buf: &mut [u8]) -> std::io::Result<(usize, SocketAddr)> {
    match s {
        Some(s) => s.recv_from(buf).await,
        None => std::future::pending().await,
    }
}

struct Net {
    announce: Option<UdpSocket>,
    beat: Option<UdpSocket>,
    status: Option<UdpSocket>,
    errors: Vec<String>,
}

impl Net {
    fn open(&mut self) {
        self.errors.clear();
        for (slot, port) in [
            (&mut self.announce, ANNOUNCE_PORT),
            (&mut self.beat, BEAT_PORT),
            (&mut self.status, STATUS_PORT),
        ] {
            if slot.is_none() {
                match bind(port) {
                    Ok(s) => *slot = Some(s),
                    Err(e) => self.errors.push(format!("UDP {port}: {e}")),
                }
            }
        }
    }
}

/// Runs DJ Link while it is switched on. Switching it off drops the running part: sockets,
/// the virtual player, the simulator and a recording all stop with it.
pub async fn run(app: Arc<App>) {
    let mut first = true;
    loop {
        if !app.modules.borrow().cdj {
            if first {
                crate::log::info("DJ Link: off");
            }
            app.wait_modules(|m| m.cdj).await;
        }
        first = false;
        app.update(|st| {
            st.cdj = CdjState {
                enabled: true,
                ..Default::default()
            }
        });
        tokio::select! {
            _ = run_on(app.clone()) => return,
            _ = metadata::run(app.clone()) => return,
            _ = app.wait_modules(|m| !m.cdj) => {}
        }
        crate::log::info("DJ Link: switched off");
        app.tracks.clear();
        app.update(|st| st.cdj = CdjState::default());
    }
}

async fn run_on(app: Arc<App>) {
    let cfg = app.config.cdj.clone();
    let mut net = Net {
        announce: None,
        beat: None,
        status: None,
        errors: vec![],
    };
    let mut iface: Option<Iface> = None;
    let mut vcdj: Option<u8> = None;
    let mut sim: Option<sim::Sim> = None;
    let mut replay = app
        .cdj_replay
        .as_ref()
        .and_then(|p| load_replay(p, now_ms()));
    if let Some(r) = &replay {
        let name = r.name.clone();
        app.update(|st| st.cdj.replay = Some(name));
    }
    let mut rec: Option<Recorder> = None;
    let mut next_bind = 0u64;
    let mut next_keep_alive = 0u64;
    let mut next_house = 0u64;
    let (mut b0, mut b1, mut b2) = ([0u8; 1500], [0u8; 1500], [0u8; 1500]);
    let mut tick = tokio::time::interval(Duration::from_millis(10));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        let now = now_ms();
        if now >= next_bind
            && (net.announce.is_none() || net.beat.is_none() || net.status.is_none())
        {
            next_bind = now + RETRY_BIND_MS;
            let before = net.errors.join(", ");
            net.open();
            let after = net.errors.join(", ");
            if after.is_empty() {
                crate::log::info("DJ Link: listening on UDP 50000-50002");
            } else if after != before {
                crate::log::warn(&format!(
                    "DJ Link: cannot open {after}. Is rekordbox or another DJ Link tool running on this computer?"
                ));
            }
            if after != before {
                app.update(|st| st.cdj.error = (!after.is_empty()).then_some(after));
            }
        }

        let (port, len, src) = tokio::select! {
            r = recv(&net.announce, &mut b0) => match r { Ok((n, a)) => (ANNOUNCE_PORT, n, a), Err(_) => continue },
            r = recv(&net.beat, &mut b1) => match r { Ok((n, a)) => (BEAT_PORT, n, a), Err(_) => continue },
            r = recv(&net.status, &mut b2) => match r { Ok((n, a)) => (STATUS_PORT, n, a), Err(_) => continue },
            _ = tick.tick() => (0, 0, SocketAddr::from(([0, 0, 0, 0], 0))),
        };
        let src_addr_port = src.port();
        let now = now_ms();

        if port != 0 {
            let buf: &[u8] = match port {
                ANNOUNCE_PORT => &b0[..len],
                BEAT_PORT => &b1[..len],
                _ => &b2[..len],
            };
            let src = match src {
                SocketAddr::V4(a) => Some(*a.ip()),
                _ => None,
            };
            if let Some(r) = &mut rec {
                let (sp, dst) = (
                    src_addr_port,
                    iface.as_ref().map_or(Ipv4Addr::BROADCAST, |i| i.ip),
                );
                r.write(src.unwrap_or(Ipv4Addr::UNSPECIFIED), sp, dst, port, buf);
            }
            let Some(p) = packet::parse(port, buf) else {
                continue;
            };
            if let Packet::Conflict { number } = p {
                if Some(number) == vcdj {
                    crate::log::warn(&format!(
                        "DJ Link: device number {number} is taken, picking another"
                    ));
                    vcdj = None;
                }
                continue;
            }
            handle(&app, p, src, now, false);
            continue;
        }

        // Simulator.
        let want_sim = app.state.lock().unwrap().cdj.simulator;
        match (&mut sim, want_sim) {
            (None, true) => {
                crate::log::info("DJ Link: simulator on");
                sim = Some(sim::Sim::new(now));
            }
            (Some(_), false) => {
                crate::log::info("DJ Link: simulator off");
                sim = None;
                app.update(|st| st.cdj.clear_simulated());
            }
            _ => {}
        }
        if let Some(s) = &mut sim {
            for (port, bytes) in s.tick(now) {
                if let Some(p) = packet::parse(port, &bytes) {
                    handle(&app, p, None, now, true);
                }
            }
        }
        if let Some(r) = &mut replay {
            let rounds = r.rounds;
            let due: Vec<(u16, Ipv4Addr, Vec<u8>)> = r
                .tick(now)
                .into_iter()
                .map(|d| (d.dst_port, d.src, d.payload.clone()))
                .collect();
            if r.rounds != rounds {
                app.update(|st| st.cdj.clear_simulated());
            }
            for (port, src, bytes) in due {
                if let Some(p) = packet::parse(port, &bytes) {
                    handle(&app, p, Some(src), now, true);
                }
            }
        }

        // Recording.
        let want_rec = app.state.lock().unwrap().cdj.record;
        match (&rec, want_rec) {
            (None, true) => match Recorder::start(&app.dir) {
                Ok(r) => {
                    crate::log::info(&format!("DJ Link: recording to {}", r.path));
                    let path = r.path.clone();
                    rec = Some(r);
                    app.update(|st| st.cdj.recording = Some(path));
                }
                Err(e) => {
                    crate::log::warn(&format!("DJ Link: cannot record: {e}"));
                    app.update(|st| {
                        st.cdj.record = false;
                        st.cdj.error = Some(format!("cannot record: {e}"));
                    });
                }
            },
            (Some(_), false) => {
                if let Some(mut r) = rec.take() {
                    r.finish();
                    crate::log::info(&format!(
                        "DJ Link: recorded {} packets to {}",
                        r.count, r.path
                    ));
                }
                app.update(|st| st.cdj.recording = None);
            }
            _ => {}
        }

        if now < next_house {
            continue;
        }
        next_house = now + 500;
        if let Some(r) = &mut rec {
            r.flush();
            let n = r.count;
            if app.state.lock().unwrap().cdj.recorded != n {
                app.update(|st| st.cdj.recorded = n);
            }
        }
        let events = {
            let mut st = app.state.lock().unwrap();
            st.cdj.prune(now)
        };
        if !events.is_empty() {
            log_events(&events);
            app.changes.send_modify(|n| *n = n.wrapping_add(1));
        }

        // Pick the interface: from the config, or the one the first real device is on.
        if iface.is_none() {
            let peer = {
                let st = app.state.lock().unwrap();
                st.cdj
                    .devices
                    .values()
                    .find(|d| !d.simulated)
                    .and_then(|d| d.ip)
            };
            if !cfg.interface.is_empty() || peer.is_some() {
                iface = find_iface(&cfg.interface, peer);
                match &iface {
                    Some(i) => {
                        crate::log::info(&format!("DJ Link: using interface {}", i.label()));
                        let label = i.label();
                        app.update(|st| st.cdj.interface = Some(label));
                    }
                    None if !cfg.interface.is_empty() => {
                        let e = format!("network interface \"{}\" not found", cfg.interface);
                        app.update(|st| st.cdj.status_note = Some(e));
                    }
                    None => {}
                }
            }
        }

        // Stage 2: the silent virtual player.
        let note = if !cfg.status {
            Some("Status off (cdj.status = false): beats and BPM only.".to_string())
        } else if net.status.is_none() {
            Some("Status needs UDP 50002, which another program holds.".to_string())
        } else if iface.is_none() {
            Some("Status starts when the first CDJ or mixer is found.".to_string())
        } else {
            None
        };
        let Some(i) = iface.as_ref().filter(|_| note.is_none()) else {
            if vcdj.is_some() || app.state.lock().unwrap().cdj.status_note != note {
                vcdj = None;
                app.update(|st| {
                    st.cdj.vcdj = None;
                    st.cdj.status_note = note;
                });
            }
            continue;
        };
        if now < next_keep_alive {
            continue;
        }
        next_keep_alive = now + KEEP_ALIVE_MS;
        let (targets, taken): (Vec<Ipv4Addr>, Vec<u8>) = {
            let st = app.state.lock().unwrap();
            let real = st.cdj.devices.values().filter(|d| !d.simulated);
            let targets = real
                .clone()
                // Pioneer Stagehand crashes on these packets.
                .filter(|d| !d.name.to_lowercase().contains("stagehand"))
                .filter_map(|d| d.ip)
                .collect();
            (targets, real.map(|d| d.number).collect())
        };
        if vcdj.is_none_or(|n| taken.contains(&n)) {
            let start = cfg.player.clamp(*VCDJ_RANGE.start(), *VCDJ_RANGE.end());
            vcdj = (start..=*VCDJ_RANGE.end())
                .chain(*VCDJ_RANGE.start()..start)
                .find(|n| !taken.contains(n));
            match vcdj {
                Some(n) => crate::log::info(&format!(
                    "DJ Link: announcing as silent virtual player {n} on {} to receive player status",
                    i.label()
                )),
                None => crate::log::warn("DJ Link: no free device number for the virtual player"),
            }
            app.update(|st| {
                st.cdj.vcdj = vcdj;
                st.cdj.status_note = vcdj
                    .is_none()
                    .then(|| "No free device number for status.".into());
            });
        }
        let (Some(n), Some(sock)) = (vcdj, &net.announce) else {
            continue;
        };
        let ka = packet::keep_alive(VCDJ_NAME, n, Kind::Player, i.mac, i.ip);
        for t in targets {
            let _ = sock.send_to(&ka, (t, ANNOUNCE_PORT)).await;
            if let Some(r) = &mut rec {
                r.write(i.ip, ANNOUNCE_PORT, t, ANNOUNCE_PORT, &ka);
            }
        }
    }
}

fn load_replay(path: &std::path::Path, now: u64) -> Option<replay::Replay> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    match std::fs::read(path)
        .map_err(|e| e.to_string())
        .and_then(|d| pcap::read(&d).map_err(|e| e.to_string()))
    {
        Ok(all) => {
            let r = replay::Replay::new(name, check::djlink(all), now);
            crate::log::info(&format!(
                "DJ Link: replaying {} ({} packets, loops)",
                path.display(),
                r.len()
            ));
            Some(r)
        }
        Err(e) => {
            crate::log::warn(&format!("DJ Link: cannot replay {}: {e}", path.display()));
            None
        }
    }
}

/// Writes received DJ Link packets to `logs/djlink-<time>.pcap` (open it in Wireshark, or
/// replay it with `boothmate --replay`).
struct Recorder {
    w: pcap::Writer<std::io::BufWriter<std::fs::File>>,
    path: String,
    count: u64,
    failed: bool,
}

impl Recorder {
    fn start(dir: &std::path::Path) -> std::io::Result<Self> {
        let logs = dir.join("logs");
        std::fs::create_dir_all(&logs)?;
        let name = format!("djlink-{}.pcap", stamp());
        let path = logs.join(name);
        let f = std::fs::File::create(&path)?;
        Ok(Recorder {
            w: pcap::Writer::new(std::io::BufWriter::new(f))?,
            path: path.display().to_string(),
            count: 0,
            failed: false,
        })
    }

    fn write(
        &mut self,
        src: Ipv4Addr,
        src_port: u16,
        dst: Ipv4Addr,
        dst_port: u16,
        payload: &[u8],
    ) {
        if self.failed {
            return;
        }
        let d = pcap::Datagram {
            time_us: crate::state::now_ms() * 1000,
            src,
            dst,
            src_port,
            dst_port,
            payload: payload.to_vec(),
        };
        if let Err(e) = self.w.write(&d) {
            crate::log::warn(&format!("DJ Link: recording stopped: {e}"));
            self.failed = true;
        }
        self.count += 1;
    }

    fn flush(&mut self) {
        let _ = self.w.flush();
    }

    fn finish(&mut self) {
        self.flush();
    }
}

/// `YYYYMMDD-HHMMSS` (UTC) for file names.
fn stamp() -> String {
    let secs = now_ms() / 1000;
    let (d, t) = (secs / 86_400, secs % 86_400);
    let (y, m, day) = civil(d as i64);
    format!(
        "{y:04}{m:02}{day:02}-{:02}{:02}{:02}",
        t / 3600,
        t / 60 % 60,
        t % 60
    )
}

/// Days since 1970 to (year, month, day), UTC.
fn civil(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + (m <= 2) as i64, m, d)
}

fn handle(app: &App, p: Packet, src: Option<Ipv4Addr>, now: u64, simulated: bool) {
    // Position packets come every 30 ms per CDJ-3000: store them without waking everybody up.
    let quiet = matches!(p, Packet::Position(_));
    let events = app.state.lock().unwrap().cdj.apply(p, src, now, simulated);
    if !quiet || !events.is_empty() {
        app.changes.send_modify(|n| *n = n.wrapping_add(1));
    }
    log_events(&events);
    if !quiet || !events.is_empty() {
        let _ = app.cdj_events.send(Arc::new(events));
    }
}

fn log_events(events: &[Event]) {
    for e in events {
        let line = match e {
            Event::Found { number, name } => format!("DJ Link: found {name} ({number})"),
            Event::Lost { number, name } => format!("DJ Link: lost {name} ({number})"),
            Event::Master(Some(n)) => format!("DJ Link: tempo master is player {n}"),
            Event::Master(None) => "DJ Link: no tempo master".into(),
            Event::Track {
                number,
                track: Some(t),
            } => {
                format!(
                    "DJ Link: player {number} loaded track {} from player {} {}",
                    t.id, t.player, t.slot
                )
            }
            Event::Track {
                number,
                track: None,
            } => format!("DJ Link: player {number} has no track"),
            Event::Play { number, playing } => {
                crate::log::debug(&format!(
                    "DJ Link: player {number} {}",
                    if *playing { "plays" } else { "stops" }
                ));
                continue;
            }
            Event::Beat { .. } => continue,
        };
        crate::log::info(&line);
    }
}
