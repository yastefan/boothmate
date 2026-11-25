//! CDJ module: Pro DJ Link, read-only.
//!
//! Stage 1: listens for keep-alives (UDP 50000) and beat and position packets (UDP 50001). This
//! needs nothing from Boothmate's side. Stage 2: announces Boothmate as a silent virtual player
//! with a device number outside the player range (7 and up), so players also send it their
//! status (UDP 50002). Boothmate never sends anything else: no media browsing, no track loading,
//! no player control.

pub mod packet;
pub mod sim;
pub mod tracker;

use crate::state::now_ms;
use crate::App;
use packet::{Kind, Packet, ANNOUNCE_PORT, BEAT_PORT, STATUS_PORT};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::UdpSocket;
use tracker::Event;

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

pub async fn run(app: Arc<App>) {
    let cfg = app.config.cdj.clone();
    app.update(|st| st.cdj.enabled = cfg.enabled);
    if !cfg.enabled {
        crate::log::info("DJ Link: off (cdj.enabled = false)");
        return;
    }
    let mut net = Net {
        announce: None,
        beat: None,
        status: None,
        errors: vec![],
    };
    let mut iface: Option<Iface> = None;
    let mut vcdj: Option<u8> = None;
    let mut sim: Option<sim::Sim> = None;
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
        let now = now_ms();

        if port != 0 {
            let buf: &[u8] = match port {
                ANNOUNCE_PORT => &b0[..len],
                BEAT_PORT => &b1[..len],
                _ => &b2[..len],
            };
            let Some(p) = packet::parse(port, buf) else {
                continue;
            };
            let src = match src {
                SocketAddr::V4(a) => Some(*a.ip()),
                _ => None,
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

        if now < next_house {
            continue;
        }
        next_house = now + 500;
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
        }
    }
}

fn handle(app: &App, p: Packet, src: Option<Ipv4Addr>, now: u64, simulated: bool) {
    // Position packets come every 30 ms per CDJ-3000: store them without waking everybody up.
    let quiet = matches!(p, Packet::Position(_));
    let events = app.state.lock().unwrap().cdj.apply(p, src, now, simulated);
    if !quiet || !events.is_empty() {
        app.changes.send_modify(|n| *n = n.wrapping_add(1));
    }
    log_events(&events);
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
