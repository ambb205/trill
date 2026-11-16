//! MIDI input ports: finding them, listening to them, and coping when they
//! come and go.
//!
//! Every port that is present and not switched off is listened to, one
//! plugged in while the program runs included: a keyboard should play
//! without a visit to the settings. What a port sends goes straight from
//! the driver's thread to the audio thread ([`LiveSender`]), stamped with
//! the time it arrived, without a lock or an allocation on the way.
//!
//! Ports are looked after by a thread of their own. Listing, opening and
//! closing them means calling into drivers, and a driver that takes its
//! time (or never comes back) must cost nothing but MIDI: the window and
//! the housekeeping thread only ever read what this thread last found out.

use parking_lot::{Condvar, Mutex};
use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use trill_core::device::EventKind;
use trill_engine::LiveSender;

/// How often the ports are looked at for ones that came or went.
const WATCH_EVERY: Duration = Duration::from_secs(1);
/// How long the end of the program waits for the watching thread.
const STOP_WAIT: Duration = Duration::from_millis(500);

/// A MIDI input port as the system lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MidiPort {
    /// What the backend knows the port by.
    pub id: String,
    /// What the user knows it by.
    pub name: String,
}

/// Where a backend delivers the messages of an open port, one complete
/// message per call, on whatever thread the driver uses.
pub type MidiSink = Box<dyn FnMut(&[u8]) + Send>;

/// The MIDI ports of a system. The program's is [`SystemMidi`]; tests bring
/// their own ([`scripted::ScriptedMidi`]).
pub trait MidiBackend: Send {
    /// The input ports present right now.
    fn ports(&mut self) -> Result<Vec<MidiPort>, String>;

    /// Starts listening to `port`. Messages go to `sink` until what is
    /// returned is dropped.
    fn open(&mut self, port: &MidiPort, sink: MidiSink) -> Result<Box<dyn Send>, String>;
}

/// A MIDI input as a front end shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MidiInputInfo {
    /// The port's name, which is also how it is switched on and off. The
    /// second port of a name gets a number (`"Keystation (2)"`).
    pub name: String,
    /// Whether the user wants it listened to. On unless switched off.
    pub enabled: bool,
    /// Whether it is plugged in right now. A port that is switched off
    /// stays listed while it is away, so it can be switched on again.
    pub present: bool,
    /// Whether it is being listened to.
    pub open: bool,
    /// Why it is not being listened to though it is present and switched
    /// on: another program has it, say.
    pub problem: Option<String>,
    /// How many messages it has sent since the program started. A view
    /// lights its lamp when the number moves.
    pub events: u64,
}

/// What a MIDI message means to the engine, if anything. `message` is one
/// complete message. The channel is ignored: every channel plays.
pub fn parse(message: &[u8]) -> Option<EventKind> {
    let (&status, data) = message.split_first()?;
    let first = *data.first()? & 0x7F;
    let second = data.get(1).map(|byte| byte & 0x7F);
    match status & 0xF0 {
        // A note-on with velocity 0 is how many keyboards say note-off.
        0x90 if second? > 0 => Some(EventKind::NoteOn { pitch: first, velocity: second? as f32 / 127.0 }),
        0x80 | 0x90 => Some(EventKind::NoteOff { pitch: first }),
        0xB0 => match first {
            // All sound off, all notes off.
            120 | 123 => Some(EventKind::AllNotesOff),
            controller => Some(EventKind::Control { controller, value: second? as f32 / 127.0 }),
        },
        0xE0 => {
            // Fourteen bits, 8192 at rest. The top of the range is one
            // step short of the bottom's, so each side is scaled to reach
            // exactly 1.
            let raw = i32::from(first) | i32::from(second?) << 7;
            let centred = raw - 8192;
            let value = if centred >= 0 { centred as f32 / 8191.0 } else { centred as f32 / 8192.0 };
            Some(EventKind::PitchBend { value })
        }
        // Pressure on the whole keyboard; pressure on one key is not played.
        0xD0 => Some(EventKind::ChannelPressure { value: first as f32 / 127.0 }),
        // Program changes and system messages are not played either.
        _ => None,
    }
}

/// A port this program has seen.
struct Known {
    name: String,
    /// The backend's id while the port is present.
    id: Option<String>,
    open: bool,
    problem: Option<String>,
    events: Arc<AtomicU64>,
}

#[derive(Default)]
struct State {
    disabled: BTreeSet<String>,
    known: Vec<Known>,
    /// How many times the ports have been looked at.
    rounds: u64,
    /// Set to have them looked at now.
    poked: bool,
    stopped: bool,
    /// Set by the watching thread as it ends, its ports closed.
    finished: bool,
}

struct Shared {
    state: Mutex<State>,
    changed: Condvar,
}

/// The program's MIDI inputs; see the module documentation.
pub(crate) struct MidiInputs {
    shared: Arc<Shared>,
}

impl MidiInputs {
    /// Starts watching the ports of `backend`. Those named in `disabled`
    /// are left alone; the rest are listened to and what they send is
    /// played through `live`. `on_lost` is called when a port that was
    /// being listened to goes away or is switched off: it could not let go
    /// of the keys that were down, so somebody must.
    pub fn start(
        backend: Box<dyn MidiBackend>,
        live: LiveSender,
        disabled: &[String],
        on_lost: Box<dyn Fn() + Send>,
    ) -> std::io::Result<Self> {
        let state = State { disabled: disabled.iter().cloned().collect(), ..State::default() };
        let shared = Arc::new(Shared { state: Mutex::new(state), changed: Condvar::new() });
        let watching = shared.clone();
        std::thread::Builder::new().name("midi-watch".into()).spawn(move || {
            let ending = watching.clone();
            Watcher { shared: watching, backend, live, on_lost, connections: Vec::new() }.run();
            ending.state.lock().finished = true;
            ending.changed.notify_all();
        })?;
        Ok(Self { shared })
    }

    /// The inputs as they stood when the ports were last looked at.
    pub fn inputs(&self) -> Vec<MidiInputInfo> {
        let state = self.shared.state.lock();
        state
            .known
            .iter()
            .map(|known| MidiInputInfo {
                name: known.name.clone(),
                enabled: !state.disabled.contains(&known.name),
                present: known.id.is_some(),
                open: known.open,
                problem: known.problem.clone(),
                events: known.events.load(Ordering::Relaxed),
            })
            .collect()
    }

    /// The names of the inputs that are switched off, as the settings keep
    /// them.
    pub fn disabled(&self) -> Vec<String> {
        self.shared.state.lock().disabled.iter().cloned().collect()
    }

    /// Switches an input on or off. The port is opened or closed in the
    /// background; this does not wait for it.
    pub fn set_enabled(&self, name: &str, enabled: bool) {
        let mut state = self.shared.state.lock();
        let changed = if enabled { state.disabled.remove(name) } else { state.disabled.insert(name.to_string()) };
        if changed {
            state.poked = true;
            self.shared.changed.notify_all();
        }
    }

    /// Has the ports looked at now rather than at the next second, and
    /// waits until that is done, up to `timeout`. Returns whether it was.
    pub fn rescan(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut state = self.shared.state.lock();
        // A round that is under way may have listed the ports already: the
        // one that counts is the next to start.
        let wanted = state.rounds + 2;
        state.poked = true;
        self.shared.changed.notify_all();
        while state.rounds < wanted && !state.stopped {
            if self.shared.changed.wait_until(&mut state, deadline).timed_out() {
                return state.rounds >= wanted;
            }
            // Keep the watcher going until the round that counts is done.
            if state.rounds < wanted {
                state.poked = true;
                self.shared.changed.notify_all();
            }
        }
        state.rounds >= wanted
    }
}

impl Drop for MidiInputs {
    fn drop(&mut self) {
        let deadline = Instant::now() + STOP_WAIT;
        let mut state = self.shared.state.lock();
        state.stopped = true;
        self.shared.changed.notify_all();
        // The thread closes its ports on the way out. If a driver has it
        // stuck, the program ends without it.
        while !state.finished {
            if self.shared.changed.wait_until(&mut state, deadline).timed_out() {
                break;
            }
        }
        if !state.finished {
            log::warn!("the MIDI ports were not closed in time; leaving them to the system");
        }
    }
}

/// The thread's side: the backend and the open ports are its alone.
struct Watcher {
    shared: Arc<Shared>,
    backend: Box<dyn MidiBackend>,
    live: LiveSender,
    on_lost: Box<dyn Fn() + Send>,
    /// The open ports, by name.
    connections: Vec<(String, Box<dyn Send>)>,
}

impl Watcher {
    fn run(mut self) {
        loop {
            self.look();
            let mut state = self.shared.state.lock();
            state.rounds += 1;
            self.shared.changed.notify_all();
            if !state.poked && !state.stopped {
                self.shared.changed.wait_for(&mut state, WATCH_EVERY);
            }
            if state.stopped {
                break;
            }
            state.poked = false;
        }
        // Closed here, on the thread that opened them.
        self.connections.clear();
    }

    /// Lists the ports and brings what is open in line with what is there
    /// and what is wanted.
    fn look(&mut self) {
        let listed = match self.backend.ports() {
            Ok(mut ports) => {
                number_duplicates(&mut ports);
                ports
            }
            Err(error) => {
                log::warn!("cannot list the MIDI inputs: {error}");
                return;
            }
        };
        let disabled = self.shared.state.lock().disabled.clone();

        // Ports that went away, or were switched off, are closed.
        let before = self.connections.len();
        self.connections.retain(|(name, _)| {
            let stays = listed.iter().any(|port| port.name == *name) && !disabled.contains(name);
            if !stays {
                log::info!("MIDI input '{name}' closed");
            }
            stays
        });
        let lost = self.connections.len() != before;

        // Ports that are there, wanted and not open are opened.
        let mut problems: Vec<(String, String)> = Vec::new();
        for port in &listed {
            if disabled.contains(&port.name) || self.connections.iter().any(|(name, _)| *name == port.name) {
                continue;
            }
            let events = self.events_of(&port.name);
            let live = self.live.clone();
            let sink: MidiSink = Box::new(move |message: &[u8]| {
                events.fetch_add(1, Ordering::Relaxed);
                if let Some(kind) = parse(message) {
                    live.play(kind);
                }
            });
            match self.backend.open(port, sink) {
                Ok(connection) => {
                    log::info!("MIDI input '{}' opened", port.name);
                    self.connections.push((port.name.clone(), connection));
                }
                Err(error) => problems.push((port.name.clone(), error)),
            }
        }

        // The ports as they are now, in the system's order, each keeping
        // what was known of it; then those that are away and switched off,
        // which is all there is to remember about a port that is away.
        let mut state = self.shared.state.lock();
        let mut before = std::mem::take(&mut state.known);
        let mut take = |name: &str| match before.iter().position(|known| known.name == name) {
            Some(index) => before.remove(index),
            None => {
                let events = Arc::new(AtomicU64::new(0));
                Known { name: name.to_string(), id: None, open: false, problem: None, events }
            }
        };
        let mut known = Vec::with_capacity(listed.len());
        for port in &listed {
            let mut entry = take(&port.name);
            let problem = problems.iter().find(|(name, _)| *name == port.name).map(|(_, problem)| problem.clone());
            if problem.is_some() && problem != entry.problem {
                // Said once per problem, not once a second.
                log::warn!("MIDI input '{}' cannot be listened to: {}", port.name, problem.as_deref().unwrap_or(""));
            }
            entry.id = Some(port.id.clone());
            entry.open = self.connections.iter().any(|(name, _)| *name == port.name);
            entry.problem = problem;
            known.push(entry);
        }
        let away: Vec<String> =
            state.disabled.iter().filter(|name| !listed.iter().any(|port| port.name == **name)).cloned().collect();
        for name in away {
            let entry = take(&name);
            known.push(Known { id: None, open: false, problem: None, ..entry });
        }
        state.known = known;
        drop(state);

        if lost {
            // The port cannot say that its keys came up.
            (self.on_lost)();
        }
    }

    /// The message counter of the port called `name`: the one it had, if
    /// it has been seen before.
    fn events_of(&self, name: &str) -> Arc<AtomicU64> {
        let mut state = self.shared.state.lock();
        if let Some(known) = state.known.iter().find(|known| known.name == name) {
            return known.events.clone();
        }
        let events = Arc::new(AtomicU64::new(0));
        state.known.push(Known {
            name: name.to_string(),
            id: None,
            open: false,
            problem: None,
            events: events.clone(),
        });
        events
    }
}

/// Gives every port a name of its own: the second and later ports of one
/// name get a number.
fn number_duplicates(ports: &mut [MidiPort]) {
    let names: Vec<String> = ports.iter().map(|port| port.name.clone()).collect();
    for (index, port) in ports.iter_mut().enumerate() {
        let earlier = names[..index].iter().filter(|name| **name == names[index]).count();
        if earlier > 0 {
            port.name = format!("{} ({})", names[index], earlier + 1);
        }
    }
}

/// The system's MIDI ports, through `midir`.
pub struct SystemMidi;

/// The name this program gives itself to the system's MIDI service.
const CLIENT_NAME: &str = "Trill";

impl MidiBackend for SystemMidi {
    fn ports(&mut self) -> Result<Vec<MidiPort>, String> {
        let input = midir::MidiInput::new(CLIENT_NAME).map_err(|error| error.to_string())?;
        let ports = input.ports();
        Ok(ports
            .iter()
            .filter_map(|port| Some(MidiPort { id: port.id(), name: input.port_name(port).ok()? }))
            .collect())
    }

    fn open(&mut self, port: &MidiPort, mut sink: MidiSink) -> Result<Box<dyn Send>, String> {
        let mut input = midir::MidiInput::new(CLIENT_NAME).map_err(|error| error.to_string())?;
        // System exclusive, clock and active sensing are not played.
        input.ignore(midir::Ignore::All);
        let found = input.find_port_by_id(&port.id).ok_or("the port is no longer there")?;
        let connection = input
            .connect(&found, "trill-input", move |_stamp, message, _| sink(message), ())
            .map_err(|error| in_words(&error.to_string()))?;
        Ok(Box::new(connection))
    }
}

/// A driver's refusal, in words for the user.
fn in_words(error: &str) -> String {
    if error.contains("MMSYSERR_ALLOCATED") {
        "another program is using this port".to_string()
    } else {
        error.to_string()
    }
}

pub mod scripted {
    //! MIDI ports that exist only in a test: they can be plugged in and
    //! pulled out, made to refuse being opened, and made to send.

    use super::{MidiBackend, MidiPort, MidiSink};
    use parking_lot::Mutex;
    use std::sync::Arc;

    #[derive(Default)]
    struct Ports {
        /// Name, and why it cannot be opened if it cannot.
        present: Vec<(String, Option<String>)>,
        /// The sinks of the open ports, by name.
        open: Vec<(String, u64, MidiSink)>,
        opened: u64,
        /// Why the ports cannot be listed, if they cannot.
        broken: Option<String>,
    }

    /// See the module documentation. Clones share their ports.
    #[derive(Clone, Default)]
    pub struct ScriptedMidi {
        ports: Arc<Mutex<Ports>>,
    }

    impl ScriptedMidi {
        pub fn new() -> Self {
            Self::default()
        }

        /// Plugs a port in.
        pub fn plug(&self, name: &str) {
            self.ports.lock().present.push((name.to_string(), None));
        }

        /// Pulls a port out. What it had open goes quiet, as a real one's
        /// does: nothing more arrives, and nobody is told.
        pub fn unplug(&self, name: &str) {
            let mut ports = self.ports.lock();
            ports.present.retain(|(present, _)| present != name);
            ports.open.retain(|(open, _, _)| open != name);
        }

        /// Makes a port refuse to be opened, for `reason`, or take it back
        /// with `None`.
        pub fn refuse(&self, name: &str, reason: Option<&str>) {
            for (present, refusal) in &mut self.ports.lock().present {
                if present == name {
                    *refusal = reason.map(str::to_string);
                }
            }
        }

        /// Makes listing the ports fail, or work again with `None`.
        pub fn break_listing(&self, reason: Option<&str>) {
            self.ports.lock().broken = reason.map(str::to_string);
        }

        /// The port called `name` sends `message`. Returns whether anybody
        /// was listening.
        pub fn send(&self, name: &str, message: &[u8]) -> bool {
            let mut ports = self.ports.lock();
            let mut heard = false;
            for (open, _, sink) in &mut ports.open {
                if open == name {
                    sink(message);
                    heard = true;
                }
            }
            heard
        }

        /// The names of the ports being listened to, in alphabetical
        /// order.
        pub fn open(&self) -> Vec<String> {
            let mut names: Vec<String> = self.ports.lock().open.iter().map(|(name, _, _)| name.clone()).collect();
            names.sort();
            names
        }
    }

    impl MidiBackend for ScriptedMidi {
        fn ports(&mut self) -> Result<Vec<MidiPort>, String> {
            let ports = self.ports.lock();
            if let Some(reason) = &ports.broken {
                return Err(reason.clone());
            }
            Ok(ports
                .present
                .iter()
                .enumerate()
                .map(|(index, (name, _))| MidiPort { id: format!("{index}:{name}"), name: name.clone() })
                .collect())
        }

        fn open(&mut self, port: &MidiPort, sink: MidiSink) -> Result<Box<dyn Send>, String> {
            let mut ports = self.ports.lock();
            let name = port.id.split_once(':').map_or(port.id.as_str(), |(_, name)| name).to_string();
            match ports.present.iter().find(|(present, _)| *present == name) {
                None => return Err("the port is no longer there".to_string()),
                Some((_, Some(reason))) => return Err(reason.clone()),
                Some((_, None)) => {}
            }
            ports.opened += 1;
            let serial = ports.opened;
            ports.open.push((name, serial, sink));
            Ok(Box::new(Open { ports: self.ports.clone(), serial }))
        }
    }

    /// Stops a port's messages when dropped.
    struct Open {
        ports: Arc<Mutex<Ports>>,
        serial: u64,
    }

    impl Drop for Open {
        fn drop(&mut self) {
            self.ports.lock().open.retain(|(_, serial, _)| *serial != self.serial);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_become_what_the_engine_plays() {
        assert_eq!(parse(&[0x90, 60, 127]), Some(EventKind::NoteOn { pitch: 60, velocity: 1.0 }));
        assert_eq!(
            parse(&[0x95, 61, 64]),
            Some(EventKind::NoteOn { pitch: 61, velocity: 64.0 / 127.0 }),
            "any channel"
        );
        assert_eq!(parse(&[0x90, 60, 0]), Some(EventKind::NoteOff { pitch: 60 }), "velocity 0 is a note-off");
        assert_eq!(parse(&[0x83, 60, 40]), Some(EventKind::NoteOff { pitch: 60 }));
        assert_eq!(parse(&[0xB0, 1, 127]), Some(EventKind::Control { controller: 1, value: 1.0 }));
        assert_eq!(parse(&[0xB0, 64, 0]), Some(EventKind::Control { controller: 64, value: 0.0 }));
        assert_eq!(parse(&[0xB0, 64, 127]).unwrap().sustain(), Some(true));
        assert_eq!(parse(&[0xB2, 123, 0]), Some(EventKind::AllNotesOff));
        assert_eq!(parse(&[0xB2, 120, 0]), Some(EventKind::AllNotesOff));

        // The wheel: at rest, at both ends, and a little way up.
        assert_eq!(parse(&[0xE0, 0, 64]), Some(EventKind::PitchBend { value: 0.0 }));
        assert_eq!(parse(&[0xE0, 127, 127]), Some(EventKind::PitchBend { value: 1.0 }));
        assert_eq!(parse(&[0xE0, 0, 0]), Some(EventKind::PitchBend { value: -1.0 }));
        let Some(EventKind::PitchBend { value }) = parse(&[0xE0, 0, 96]) else { panic!("a bend") };
        assert!((value - 0.5).abs() < 0.001, "{value}");

        assert_eq!(parse(&[0xD3, 127]), Some(EventKind::ChannelPressure { value: 1.0 }));
        assert_eq!(parse(&[0xD0, 0]), Some(EventKind::ChannelPressure { value: 0.0 }));

        // Not played: pressure on one key, program change, system messages,
        // and anything cut short.
        for message in
            [&[0xA0, 60, 50][..], &[0xC0, 5], &[0xF8], &[0xF0, 1, 2, 0xF7], &[0x90, 60], &[0x90], &[0xD0], &[]]
        {
            assert_eq!(parse(message), None, "{message:?}");
        }
        // Data bytes with the top bit set are read as the seven bits they
        // should have been.
        assert_eq!(parse(&[0x90, 0xBC, 0xFF]), Some(EventKind::NoteOn { pitch: 60, velocity: 1.0 }));
    }

    #[test]
    fn ports_of_one_name_are_numbered() {
        let port = |id: &str, name: &str| MidiPort { id: id.into(), name: name.into() };
        let mut ports = vec![port("a", "Keys"), port("b", "Pads"), port("c", "Keys"), port("d", "Keys")];
        number_duplicates(&mut ports);
        let names: Vec<&str> = ports.iter().map(|port| port.name.as_str()).collect();
        assert_eq!(names, ["Keys", "Pads", "Keys (2)", "Keys (3)"]);
    }

    #[test]
    fn a_drivers_refusal_is_put_in_words() {
        assert_eq!(
            in_words("could not create Windows MM MIDI input port (MMSYSERR_ALLOCATED)"),
            "another program is using this port"
        );
        assert_eq!(in_words("something else"), "something else");
    }
}
