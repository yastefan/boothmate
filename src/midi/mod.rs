//! MIDI module: controllers in, executor actions out to grandMA3, feedback back to controllers.

pub mod device;
pub mod mapping;
pub mod msg;
pub mod ports;

use crate::App;
use device::{DeviceProfile, TEST_ID};
use mapping::{Fb, MappingFile};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

/// Port key of the test controller in the web UI.
pub const TEST_PORT: &str = "test";

/// Fader feedback waits this long after the last input on a control.
const HOLD: Duration = Duration::from_millis(400);

pub fn mappings_path(dir: &Path) -> PathBuf {
    dir.join("midi.toml")
}

pub fn load_mappings(dir: &Path) -> Vec<mapping::Mapping> {
    let path = mappings_path(dir);
    let Ok(text) = std::fs::read_to_string(&path) else {
        return vec![];
    };
    match toml::from_str::<MappingFile>(&text) {
        Ok(f) => f.mapping,
        Err(e) => {
            crate::log::warn(&format!("MIDI: {}: {e}", path.display()));
            vec![]
        }
    }
}

fn save_mappings(dir: &Path, mappings: &[mapping::Mapping]) {
    let file = MappingFile {
        mapping: mappings.to_vec(),
    };
    let text = match toml::to_string(&file) {
        Ok(t) => format!("# MIDI mappings, written by Boothmate.\n\n{t}"),
        Err(e) => {
            crate::log::warn(&format!("MIDI: cannot save mappings: {e}"));
            return;
        }
    };
    let path = mappings_path(dir);
    if let Err(e) = std::fs::write(&path, text) {
        crate::log::warn(&format!("MIDI: cannot write {}: {e}", path.display()));
    }
}

/// A connected controller (or the test controller).
struct Device {
    id: String,
    profile: Option<DeviceProfile>,
    output: bool,
    touched: HashSet<String>,
    last_input: HashMap<String, Instant>,
    /// Last messages sent per control.
    sent: HashMap<String, Vec<Vec<u8>>>,
}

impl Device {
    fn new(port: &str, profiles: &[DeviceProfile]) -> Self {
        let profile = if port == TEST_PORT {
            profiles.iter().find(|p| p.id == TEST_ID).cloned()
        } else {
            profiles
                .iter()
                .find(|p| p.id != TEST_ID && p.matches_port(port))
                .cloned()
        };
        Device {
            id: profile
                .as_ref()
                .map(|p| p.id.clone())
                .unwrap_or_else(|| port.to_string()),
            profile,
            output: port == TEST_PORT,
            touched: HashSet::new(),
            last_input: HashMap::new(),
            sent: HashMap::new(),
        }
    }

    fn held(&self, control: &str, now: Instant) -> bool {
        self.touched.contains(control)
            || self
                .last_input
                .get(control)
                .is_some_and(|t| now.duration_since(*t) < HOLD)
    }
}

/// Runs MIDI while it is switched on. Switching it off drops the running part, which ends the
/// port thread and closes all ports. Mappings stay.
pub async fn run(app: Arc<App>, mut test_rx: mpsc::UnboundedReceiver<Vec<u8>>) {
    app.update(|st| st.midi.mappings = load_mappings(&app.dir));
    let mut first = true;
    loop {
        if !app.modules.borrow().midi {
            if first {
                crate::log::info("MIDI: off");
            }
            app.update(|st| st.midi.devices.clear());
            tokio::select! {
                _ = app.wait_modules(|m| m.midi) => {}
                // Drop test controller input while off.
                _ = async { while test_rx.recv().await.is_some() {} } => return,
            }
        }
        first = false;
        tokio::select! {
            _ = run_on(app.clone(), &mut test_rx) => return,
            _ = app.wait_modules(|m| !m.midi) => {}
        }
        crate::log::info("MIDI: switched off");
        app.update(|st| {
            st.midi.devices.clear();
            st.midi.learn = None;
            st.midi.last_input = None;
            st.midi.error = None;
        });
    }
}

async fn run_on(app: Arc<App>, test_rx: &mut mpsc::UnboundedReceiver<Vec<u8>>) {
    let profiles = device::load_all(&app.dir.join("devices"));
    crate::log::info(&format!(
        "MIDI: device profiles: {}",
        profiles
            .iter()
            .map(|p| p.id.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    ));
    let mut saved_rev = app.state.lock().unwrap().midi.rev;

    let (ev_tx, mut ev_rx) = mpsc::unbounded_channel();
    let cmd = ports::spawn(
        ports::Options {
            virtual_port: app.config.midi.virtual_port,
            ignore: app.config.midi.ignore.clone(),
            patterns: profiles.iter().flat_map(|p| p.ports.clone()).collect(),
        },
        ev_tx,
    );

    let mut devices: BTreeMap<String, Device> = BTreeMap::new();
    devices.insert(TEST_PORT.into(), Device::new(TEST_PORT, &profiles));
    publish_devices(&app, &devices);

    let mut changes = app.changes.subscribe();
    let mut modules = app.modules.subscribe();
    let mut tick = tokio::time::interval(Duration::from_millis(250));
    loop {
        tokio::select! {
            Some(ev) = ev_rx.recv() => match ev {
                ports::Event::Message { port, bytes } => {
                    if let Some(d) = devices.get_mut(&port) {
                        handle_input(&app, d, &bytes);
                    }
                }
                ports::Event::Ports(list) => {
                    devices.retain(|k, _| k == TEST_PORT || list.iter().any(|(n, _)| n == k));
                    for (name, out) in list {
                        let d = devices.entry(name.clone()).or_insert_with(|| Device::new(&name, &profiles));
                        d.output = out;
                    }
                    publish_devices(&app, &devices);
                }
                ports::Event::OutputConnected(port) => {
                    if let Some(d) = devices.get_mut(&port) {
                        d.output = true;
                        d.sent.clear();
                    }
                    publish_devices(&app, &devices);
                }
                ports::Event::Error(e) => {
                    crate::log::warn(&format!("MIDI: {e}"));
                    app.update(|st| st.midi.error = Some(e));
                }
            },
            Some(bytes) = test_rx.recv() => {
                if !app.modules.borrow().virtual_controller {
                    continue;
                }
                if let Some(d) = devices.get_mut(TEST_PORT) {
                    handle_input(&app, d, &bytes);
                }
            }
            r = changes.changed() => if r.is_err() { return },
            r = modules.changed() => if r.is_err() { return } else { publish_devices(&app, &devices) },
            _ = tick.tick() => {}
        }
        let rev = app.state.lock().unwrap().midi.rev;
        if rev != saved_rev {
            let m = app.state.lock().unwrap().midi.mappings.clone();
            save_mappings(&app.dir, &m);
            saved_rev = rev;
        }
        send_feedback(&app, &mut devices, &cmd);
    }
}

fn publish_devices(app: &App, devices: &BTreeMap<String, Device>) {
    let virtual_on = app.modules.borrow().virtual_controller;
    let list = devices
        .iter()
        .filter(|(port, _)| virtual_on || port.as_str() != TEST_PORT)
        .map(|(port, d)| mapping::DeviceInfo {
            id: d.id.clone(),
            port: port.clone(),
            name: d
                .profile
                .as_ref()
                .map(|p| p.name.clone())
                .unwrap_or_else(|| port.clone()),
            profile: d.profile.as_ref().map(|p| p.id.clone()),
            output: d.output,
        })
        .collect();
    app.update(|st| st.midi.devices = list);
}

fn handle_input(app: &App, d: &mut Device, bytes: &[u8]) {
    let Some(input) = msg::parse(bytes) else {
        return;
    };
    let now = Instant::now();
    if let Some(c) = d.profile.as_ref().and_then(|p| p.touched(&input.addr)) {
        if input.value > 0 {
            d.touched.insert(c.name.clone());
        } else {
            d.touched.remove(&c.name);
            d.last_input.insert(c.name.clone(), now);
        }
        return;
    }
    let step = d.profile.as_ref().map(|p| p.encoder_step).unwrap_or(1.0);
    let mut actions = vec![];
    let mut read_only = false;
    let mut name = String::new();
    app.update(|st| {
        let control = mapping::resolve(st, &d.id, d.profile.as_ref(), &input.addr);
        let generic = d
            .profile
            .as_ref()
            .and_then(|p| p.control(&control.name))
            .is_none();
        name = control.name.clone();
        actions = mapping::on_input(st, &d.id, &control, generic, input, step);
        read_only = st.connections.profile_fallback;
        if !read_only {
            for a in &actions {
                mapping::apply_local(st, a);
            }
        }
        if read_only && !actions.is_empty() {
            st.midi.error =
                Some("grandMA3 version without verified profile: MIDI is read-only".into());
        }
    });
    d.last_input.insert(name, now);
    crate::log::debug(&format!("midi: {} {:?} -> {actions:?}", d.id, input));
    if !read_only {
        let key = actions
            .iter()
            .any(|a| matches!(a, mapping::Action::Key { .. }));
        for a in actions {
            crate::osc::send_action(app, &a);
        }
        if key {
            // grandMA3 does not report what a key sent over OSC input did (toggle, go, flash...),
            // so ask the plugin to read the page again once grandMA3 has applied it.
            crate::plugin::refresh_soon(app);
        }
    }
}

fn send_feedback(
    app: &App,
    devices: &mut BTreeMap<String, Device>,
    cmd: &std::sync::mpsc::Sender<ports::Cmd>,
) {
    let now = Instant::now();
    let st = app.state.lock().unwrap().clone();
    let mut shown: BTreeMap<String, BTreeMap<String, Fb>> = BTreeMap::new();
    for (port, d) in devices.iter_mut() {
        let list = mapping::device_feedback(&st, &d.id, d.profile.as_ref());
        let entry = shown.entry(d.id.clone()).or_default();
        for (c, fb) in list {
            let held = mapping::is_motorised(&c) && d.held(&c.name, now);
            if !held && fb != Fb::default() {
                entry.insert(c.name.clone(), fb.clone());
            } else if held {
                if let Some(old) = st.midi.feedback.get(&d.id).and_then(|m| m.get(&c.name)) {
                    entry.insert(c.name.clone(), old.clone());
                }
            }
            if !d.output || port == TEST_PORT || held {
                continue;
            }
            let msgs = mapping::encode(d.profile.as_ref(), &c, &fb);
            if msgs.is_empty() || d.sent.get(&c.name) == Some(&msgs) {
                continue;
            }
            for bytes in &msgs {
                let _ = cmd.send(ports::Cmd::Send {
                    port: port.clone(),
                    bytes: bytes.clone(),
                });
            }
            d.sent.insert(c.name.clone(), msgs);
        }
    }
    shown.retain(|_, m| !m.is_empty());
    if shown != st.midi.feedback {
        app.update(|st| st.midi.feedback = shown);
    }
}
