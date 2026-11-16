//! The computer keyboard as an instrument: while it is switched on, the two
//! middle rows of letters play the selected track.
//!
//! [`keys`] runs first in a frame, before anything else reads the keyboard,
//! and sends each note to the session there and then: a key event and its
//! note are in the same frame, with nothing queued between them.
//!
//! **The rule for letters.** While the keyboard is on, a letter it uses is a
//! note, and the shortcut that letter had is the same letter with Alt held
//! (see [`plays`], which the shortcut table asks). Keys the keyboard does
//! not use are shortcuts as ever. With Ctrl, Alt, Shift or the Windows key
//! held, and while a text field or the list of sounds has the keyboard,
//! nothing here takes a key.
//!
//! **No note is left sounding.** Every note that is started is written down
//! in [`Keyboard::sounding`] and every way of stopping goes through that
//! list, so a note cannot be started and forgotten. Notes stop when their
//! key comes up; and all of them stop when the keyboard is switched off,
//! when what they play changes (another track selected, the track or its
//! instrument gone, another project), when a text field, a dialog or the
//! list of sounds takes the keyboard, when the window is no longer the one
//! typed into, and when the program exits. An octave change lets go of the
//! keys held through it. The sustain key keeps notes whose keys came up,
//! until it comes up itself.
//!
//! What is in [`layout`] says which key does what; [`remembered`] keeps
//! the switch, the octave and the velocity between sessions; [`view`]
//! draws the switch in the top bar and the strip above the detail panel.

mod layout;
mod remembered;
mod view;

pub use layout::{plays, reference};
pub use view::{height, letter_on, show, switch};

use super::state::{BrowserTab, Cx, FocusArea, UiState};
use eframe::egui::{self, Event, Key};
use layout::Role;
use remembered::Remembered;
use std::path::PathBuf;
use trill_core::device::{DeviceCategory, DeviceDescriptor};
use trill_core::pitch::pitch_name;
use trill_core::{Device, Track, TrackId, TrackKind};
use trill_engine::{Session, SharedSession};

/// The pitch of the lowest key when nothing was remembered: middle C.
pub const DEFAULT_LOWEST: u8 = 60;
pub const DEFAULT_VELOCITY: u8 = 100;
/// The highest the lowest key goes: every key above it is still a note.
const HIGHEST_LOWEST: u8 = 108;
/// The velocities `C` and `V` step through: tens, so that a number says
/// where one is, with the softest and the loudest note at the ends.
const VELOCITIES: [u8; 14] = [1, 10, 20, 30, 40, 50, 60, 70, 80, 90, 100, 110, 120, 127];
/// Frames the settings must stay as they are before they are written down
/// for the next session: not on every step of the velocity.
const REMEMBER_AFTER_FRAMES: u32 = 45;

/// How the keyboard starts when the command line says so.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Options {
    /// Start with it on, whatever was remembered.
    pub on: bool,
    /// Keys held down from the first frame on, for pictures of the keyboard
    /// being played. Nothing lets go of them.
    pub hold: Vec<Key>,
}

impl Options {
    /// The keys named by `letters`, as `--hold` takes them: `"adg"`.
    pub fn holding(letters: &str) -> Result<Vec<Key>, String> {
        let key = |letter: char| {
            layout::key_labeled(&letter.to_string()).ok_or_else(|| {
                format!("'{letter}' is not a key that plays a note (the keys are A W S E D F T G Y H U J K O L P ; ')")
            })
        };
        letters.chars().filter(|letter| !letter.is_whitespace()).map(key).collect()
    }
}

/// A note that was started and has not been stopped.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
struct Sounding {
    track: TrackId,
    pitch: u8,
    /// The key that holds it down; `None` once only the sustain does.
    key: Option<Key>,
}

/// Why the keys are not heard.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Silence {
    /// A text field has the keyboard.
    Typing,
    /// The list of sounds has it: typing there searches.
    Searching,
    NoTrack,
    /// The selected track mixes others and has no instrument of its own.
    Group(String),
    Return(String),
    NoInstrument(String),
    Muted(String),
    /// Another track is soloed.
    Soloed(String),
    NoOutput,
}

impl Silence {
    /// What to tell the player, in the few words the strip has room for.
    pub fn text(&self) -> String {
        match self {
            Silence::Typing => "Paused while you type".to_owned(),
            Silence::Searching => "Paused \u{00B7} typing searches the sounds".to_owned(),
            Silence::NoTrack => "No track selected \u{00B7} click one to play it".to_owned(),
            Silence::Group(name) => format!("{name} is a group \u{00B7} it has no instrument"),
            Silence::Return(name) => format!("{name} is a return \u{00B7} it has no instrument"),
            Silence::NoInstrument(name) => format!("{name} has no instrument \u{00B7} add one"),
            Silence::Muted(name) => format!("{name} is muted"),
            Silence::Soloed(name) => format!("{name} is silent \u{00B7} another track is soloed"),
            Silence::NoOutput => "No audio output \u{00B7} nothing will be heard".to_owned(),
        }
    }
}

/// What the keys play as of this frame.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Playing {
    /// The track notes go to.
    pub track: Option<TrackId>,
    /// Its name and its instrument's, for the strip.
    pub name: String,
    pub instrument: String,
    /// Why nothing is heard, when nothing is.
    pub silence: Option<Silence>,
}

/// A message to the session, as the tests read it back.
#[cfg(test)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Sent {
    On(TrackId, u8, u8),
    Off(TrackId, u8),
}

pub struct Keyboard {
    on: bool,
    /// The pitch of the lowest key: a C.
    lowest: u8,
    velocity: u8,
    /// Whether the strip shows the picture of the keys.
    key_map: bool,
    /// Every note that was started and not yet stopped.
    sounding: Vec<Sounding>,
    /// The sustain key is down.
    sustain: bool,
    /// What the keys play, as worked out at the start of the frame.
    playing: Playing,
    /// The project the notes were started in.
    generation: u64,
    /// The frame the octave last changed on: the piano roll brings the
    /// keys' range into view, if it is on screen to do it.
    moved_at: Option<u32>,
    /// Keys the command line holds down, until they have been pressed.
    hold: Vec<Key>,
    /// True in a run that holds keys for a picture: the window's focus is
    /// not asked about, because nobody types into such a window.
    staged: bool,
    /// Where the settings are kept between sessions; `None` in a run that
    /// keeps nothing.
    folder: Option<PathBuf>,
    /// What is written there, what the settings were on the last frame,
    /// and the frame they last changed on.
    remembered: (Remembered, Remembered, u32),
    frames: u32,
    /// Stands in for the Windows key in tests, which must not depend on
    /// what is held on the machine they run on.
    #[cfg(test)]
    pub windows_key: bool,
    /// Every message sent to the session, in order.
    #[cfg(test)]
    pub sent: Vec<Sent>,
}

impl Keyboard {
    /// The keyboard as it was left in `folder`, or as the command line
    /// asks for it.
    pub fn new(folder: Option<PathBuf>, options: &Options) -> Self {
        let saved = folder.as_deref().and_then(Remembered::load).unwrap_or_default();
        let lowest = if saved.lowest.is_multiple_of(12) && saved.lowest <= HIGHEST_LOWEST {
            saved.lowest
        } else {
            DEFAULT_LOWEST
        };
        let mut keyboard = Self {
            on: saved.on || options.on || !options.hold.is_empty(),
            lowest,
            velocity: saved.velocity.clamp(1, 127),
            key_map: saved.key_map,
            sounding: Vec::new(),
            sustain: false,
            playing: Playing::default(),
            generation: 0,
            moved_at: None,
            hold: options.hold.clone(),
            staged: !options.hold.is_empty(),
            folder,
            remembered: (saved.clone(), saved, 0),
            frames: 0,
            #[cfg(test)]
            windows_key: false,
            #[cfg(test)]
            sent: Vec::new(),
        };
        // What the command line switched on is not what the user left.
        keyboard.remembered.1 = keyboard.settings();
        keyboard
    }

    pub fn is_on(&self) -> bool {
        self.on
    }

    /// The pitch of the lowest key.
    pub fn lowest(&self) -> u8 {
        self.lowest
    }

    /// The pitch of the highest key that plays.
    pub fn highest(&self) -> u8 {
        (self.lowest + layout::SPAN).min(127)
    }

    pub fn velocity(&self) -> u8 {
        self.velocity
    }

    pub fn sustaining(&self) -> bool {
        self.sustain
    }

    pub fn shows_key_map(&self) -> bool {
        self.key_map
    }

    pub fn set_key_map(&mut self, shown: bool) {
        self.key_map = shown;
    }

    /// What the keys play as of this frame.
    pub fn playing(&self) -> &Playing {
        &self.playing
    }

    /// Whether `pitch` is sounding on `track` from this keyboard.
    pub fn is_sounding(&self, track: TrackId, pitch: u8) -> bool {
        self.sounding.iter().any(|note| note.track == track && note.pitch == pitch)
    }

    /// The pitches sounding from this keyboard, lowest first.
    pub fn pitches(&self) -> Vec<u8> {
        let mut pitches: Vec<u8> = self.sounding.iter().map(|note| note.pitch).collect();
        pitches.sort_unstable();
        pitches.dedup();
        pitches
    }

    /// The letter on the key that plays `pitch`, while the keyboard is on.
    pub fn letter(&self, pitch: u8) -> Option<&'static str> {
        (self.on && pitch >= self.lowest).then(|| layout::label_for(pitch - self.lowest)).flatten()
    }

    /// True once after the octave changed, for the piano roll to follow:
    /// in the frame of the change or the one after it, depending on which
    /// of the two was laid out first. A piano roll that was not on screen
    /// then does not jump when it next is.
    pub fn take_reveal(&mut self) -> bool {
        self.moved_at.take().is_some_and(|frame| self.frames.saturating_sub(frame) <= 1)
    }

    /// Switches the keyboard on or off. Off, nothing is left sounding.
    pub fn toggle(&mut self, session: &SharedSession) {
        self.on = !self.on;
        if !self.on {
            self.release_all(session);
        }
    }

    /// Moves the keys `octaves` up or down, as far as there are notes:
    /// what a click on the strip does, as `Z` and `X` do.
    pub fn shift_octave(&mut self, octaves: i8, session: &SharedSession) {
        self.shift(&mut session.lock(), octaves);
    }

    /// The keys held through an octave change are let go: they would come
    /// up as other notes than they went down as.
    fn shift(&mut self, session: &mut Session, octaves: i8) {
        let lowest = (self.lowest as i16 + 12 * octaves as i16).clamp(0, HIGHEST_LOWEST as i16) as u8;
        if lowest != self.lowest {
            self.let_go_of_keys(session);
            self.lowest = lowest;
            self.moved_at = Some(self.frames);
        }
    }

    /// Steps the velocity of the notes to come: louder for a positive
    /// `steps`, softer for a negative one.
    pub fn step_velocity(&mut self, steps: i8) {
        self.velocity = stepped_velocity(self.velocity, steps);
    }

    /// Stops every note this keyboard started.
    pub fn release_all(&mut self, session: &SharedSession) {
        self.sustain = false;
        if !self.sounding.is_empty() {
            self.stop_where(&mut session.lock(), |_| true);
        }
    }

    /// The window is told whether it is the one typed into before a frame
    /// is run, and also while it is hidden and no frame is: notes do not
    /// outlast its losing the keyboard, seen or not.
    pub fn window(&mut self, input: &egui::RawInput, session: &SharedSession) {
        let lost = input.events.iter().any(|event| matches!(event, Event::WindowFocused(false)));
        if (lost || !input.focused) && !self.staged {
            self.release_all(session);
        }
    }

    /// The program is closing: nothing is left sounding, and the settings
    /// are written down if they changed since they last were.
    pub fn close(&mut self, session: &SharedSession) {
        self.release_all(session);
        self.remember(true, None);
    }

    fn start(&mut self, session: &mut Session, track: TrackId, pitch: u8, key: Key) {
        session.note_on(track, pitch, self.velocity);
        self.sounding.push(Sounding { track, pitch, key: Some(key) });
        #[cfg(test)]
        self.sent.push(Sent::On(track, pitch, self.velocity));
    }

    fn stop(&mut self, session: &mut Session, track: TrackId, pitch: u8) {
        session.note_off(track, pitch);
        #[cfg(test)]
        self.sent.push(Sent::Off(track, pitch));
    }

    /// Stops the notes `which` picks out, and forgets them.
    fn stop_where(&mut self, session: &mut Session, which: impl Fn(&Sounding) -> bool) {
        let (stopped, kept): (Vec<Sounding>, Vec<Sounding>) =
            std::mem::take(&mut self.sounding).into_iter().partition(|note| which(note));
        self.sounding = kept;
        for note in stopped {
            self.stop(session, note.track, note.pitch);
        }
    }

    /// Lets go of the keys `which` picks out: their notes stop, unless the
    /// sustain holds them.
    fn let_go(&mut self, session: &mut Session, which: impl Fn(&Sounding) -> bool) {
        if self.sustain {
            for note in self.sounding.iter_mut().filter(|note| which(note)) {
                note.key = None;
            }
        } else {
            self.stop_where(session, which);
        }
    }

    fn let_go_of_keys(&mut self, session: &mut Session) {
        self.let_go(session, |note| note.key.is_some());
    }

    fn press(&mut self, session: &mut Session, key: Key, role: Role, repeat: bool) {
        match role {
            // A held key repeats; a held note does not start again.
            Role::Note(_) | Role::Octave(_) | Role::Sustain if repeat => {}
            Role::Note(semitone) => {
                let Some(track) = self.playing.track else { return };
                let pitch = self.lowest as u16 + semitone as u16;
                if pitch > 127 {
                    return;
                }
                let pitch = pitch as u8;
                // The key went down again without having come up, or the
                // note is still held by the sustain: it is struck afresh,
                // and not left with two beginnings and one end.
                self.stop_where(session, |note| note.key == Some(key) || (note.track, note.pitch) == (track, pitch));
                self.start(session, track, pitch, key);
            }
            Role::Octave(octaves) => self.shift(session, octaves),
            Role::Velocity(steps) => self.step_velocity(steps),
            Role::Sustain => self.sustain = true,
        }
    }

    fn lift(&mut self, session: &mut Session, key: Key, role: Role) {
        match role {
            Role::Note(_) => self.let_go(session, |note| note.key == Some(key)),
            Role::Sustain => {
                self.sustain = false;
                self.stop_where(session, |note| note.key.is_none());
            }
            Role::Octave(_) | Role::Velocity(_) => {}
        }
    }

    /// Whether the Windows key is down: with it, a letter is the system's.
    fn windows_key_held(&self) -> bool {
        #[cfg(test)]
        return self.windows_key;
        #[cfg(not(test))]
        windows_key_held()
    }

    fn settings(&self) -> Remembered {
        Remembered { on: self.on, lowest: self.lowest, velocity: self.velocity, key_map: self.key_map }
    }

    /// Writes the settings down once they have stopped changing, for the
    /// next session to start from; with `now`, without waiting. `worker`
    /// writes the file away from the frame when there is one.
    fn remember(&mut self, now: bool, worker: Option<&mut super::sounds::Sounds>) {
        // Keys held for a picture are not a session anyone left.
        let Some(folder) = self.folder.clone().filter(|_| !self.staged) else { return };
        let current = self.settings();
        let (saved, seen, changed_at) = &mut self.remembered;
        if current != *seen {
            *seen = current.clone();
            *changed_at = self.frames;
        }
        if current == *saved || (!now && self.frames < *changed_at + REMEMBER_AFTER_FRAMES) {
            return;
        }
        *saved = current.clone();
        match worker {
            Some(worker) if !now => worker.quietly(move |_| current.save(&folder)),
            _ => {
                if let Err(reason) = current.save(&folder) {
                    log::warn!("{reason}");
                }
            }
        }
    }

    /// Every note that was started and not stopped, in the order started.
    #[cfg(test)]
    pub fn sounding(&self) -> Vec<(TrackId, u8)> {
        self.sounding.iter().map(|note| (note.track, note.pitch)).collect()
    }

    /// The keys that hold a note down.
    #[cfg(test)]
    pub fn keys_down(&self) -> Vec<Key> {
        self.sounding.iter().filter_map(|note| note.key).collect()
    }

    /// How many notes only the sustain holds.
    #[cfg(test)]
    pub fn pedalled(&self) -> usize {
        self.sounding.iter().filter(|note| note.key.is_none()).count()
    }
}

/// The velocity `steps` along the ladder from `velocity`: the next rung up
/// or down, also from a value that is not on one.
fn stepped_velocity(velocity: u8, steps: i8) -> u8 {
    let mut velocity = velocity;
    for _ in 0..steps.unsigned_abs() {
        let next = if steps > 0 {
            VELOCITIES.iter().copied().find(|rung| *rung > velocity)
        } else {
            VELOCITIES.iter().rev().copied().find(|rung| *rung < velocity)
        };
        velocity = next.unwrap_or(velocity);
    }
    velocity
}

/// Whether the Windows key is down, as far as this thread's messages say.
#[cfg(all(windows, not(test)))]
fn windows_key_held() -> bool {
    const VK_LWIN: i32 = 0x5B;
    const VK_RWIN: i32 = 0x5C;
    #[link(name = "user32")]
    unsafe extern "system" {
        fn GetKeyState(key: i32) -> i16;
    }
    // SAFETY: `GetKeyState` takes a key code and returns a number; it has
    // no other effect and no requirement on its caller.
    unsafe { GetKeyState(VK_LWIN) < 0 || GetKeyState(VK_RWIN) < 0 }
}

/// Elsewhere the key is a modifier the window is told about (Command on a
/// Mac, which the caller has already ruled out) or is kept by the system.
#[cfg(all(not(windows), not(test)))]
fn windows_key_held() -> bool {
    false
}

/// The device of `track` that notes play: the first one the catalog knows
/// as an instrument. A device the catalog does not know (a plugin still
/// being scanned for, or one that is not installed) may be one.
fn instrument<'a>(track: &'a Track, descriptors: &[DeviceDescriptor]) -> Option<&'a Device> {
    track.devices.iter().find(|device| {
        let known = descriptors.iter().find(|known| known.uri == device.uri);
        known.is_none_or(|known| known.category == DeviceCategory::Instrument)
    })
}

/// Whether the list of sounds has the keyboard: typing there is a search.
fn searching(state: &UiState) -> bool {
    state.browser.open && state.browser.tab == BrowserTab::Sounds && state.focus == FocusArea::Browser
}

/// What the keys would play now: the selected track, also while the
/// master is what the Devices tab shows, since the master plays no notes.
fn assess(cx: &Cx, paused: Option<Silence>) -> Playing {
    let Some(track) = cx.state.selected_track.and_then(|id| cx.snap.track(id)) else {
        return Playing { silence: Some(paused.unwrap_or(Silence::NoTrack)), ..Playing::default() };
    };
    let name = track.name.clone();
    let unplayable = match track.kind {
        TrackKind::Group => Some(Silence::Group(name.clone())),
        TrackKind::Return => Some(Silence::Return(name.clone())),
        TrackKind::Normal => None,
    };
    let instrument = instrument(track, cx.descriptors);
    let (Some(instrument), None) = (instrument, &unplayable) else {
        let silence = paused.or(unplayable).unwrap_or(Silence::NoInstrument(name.clone()));
        return Playing { track: None, name, instrument: String::new(), silence: Some(silence) };
    };
    // Played all the same, and not heard: say so.
    let group = track.group.and_then(|id| cx.snap.track(id));
    let soloed = track.solo || group.is_some_and(|group| group.solo);
    let soloed_elsewhere = !soloed && cx.snap.project.tracks.iter().any(|other| other.solo);
    let unheard = if cx.audio.warning.is_some() {
        Some(Silence::NoOutput)
    } else if track.mute {
        Some(Silence::Muted(name.clone()))
    } else if let Some(group) = group.filter(|group| group.mute) {
        Some(Silence::Muted(group.name.clone()))
    } else if soloed_elsewhere {
        Some(Silence::Soloed(name.clone()))
    } else {
        None
    };
    Playing { track: Some(track.id), name, instrument: instrument.name.clone(), silence: paused.or(unheard) }
}

/// Why the keyboard is not listening at all, apart from being off: `Some`
/// with a reason to show, or with `None` when what took the keyboard is
/// plain to see (a dialog, another window).
fn paused(ctx: &egui::Context, cx: &Cx, typing: bool) -> Option<Option<Silence>> {
    let focused = cx.keyboard.staged || ctx.input(|input| input.focused);
    if !focused || cx.jobs.confirm.is_some() || cx.recovery.is_open() || cx.state.shortcuts_open {
        Some(None)
    } else if typing {
        Some(Some(Silence::Typing))
    } else if searching(cx.state) {
        Some(Some(Silence::Searching))
    } else {
        None
    }
}

/// Stops the notes that no longer have what they were played on: another
/// project is open, or another track is the one being played.
fn stop_the_orphaned(cx: &mut Cx) {
    let keyboard = &mut *cx.keyboard;
    let moved = keyboard.sounding.iter().any(|note| Some(note.track) != keyboard.playing.track);
    if moved || keyboard.generation != cx.state.project_generation {
        keyboard.release_all(cx.session);
    }
    keyboard.generation = cx.state.project_generation;
}

/// What happened to the keys this frame: one went down (and whether as a
/// repeat of a key already held) or came up, or the window stopped being
/// the one typed into, after which no key-up can be counted on.
enum Change {
    Down(Key, Role, bool),
    Up(Key, Role),
    Lost,
}

/// The keyboard's share of the keys pressed this frame. Runs before the
/// window's shortcuts, and takes what it uses out of their sight. `typing`
/// says a text field has the keyboard.
pub fn keys(ctx: &egui::Context, cx: &mut Cx, typing: bool) {
    cx.keyboard.frames += 1;
    let paused = paused(ctx, cx, typing);
    let is_paused = paused.is_some();
    cx.keyboard.playing = assess(cx, paused.flatten());
    stop_the_orphaned(cx);
    if !cx.keyboard.on {
        return;
    }
    if is_paused {
        cx.keyboard.release_all(cx.session);
        return;
    }

    // In the order they happened: a key that went down and came up within
    // the frame is a short note, not a stuck one.
    let windows_key = cx.keyboard.windows_key_held();
    let mut changes = Vec::new();
    let mut tab = false;
    ctx.input_mut(|input| {
        input.events.retain(|event| {
            if matches!(event, Event::WindowFocused(false)) {
                changes.push(Change::Lost);
            }
            let Event::Key { key, physical_key, pressed, repeat, modifiers } = event else { return true };
            let key = layout::identify(*key, *physical_key);
            let Some(role) = layout::role(key) else { return true };
            if !*pressed {
                // Whatever is held by now: the key that started a note
                // ends it. Left for anything else that watches for it.
                changes.push(Change::Up(key, role));
                return true;
            }
            if modifiers.any() || windows_key {
                return true;
            }
            tab |= role == Role::Sustain;
            changes.push(Change::Down(key, role, *repeat));
            false
        });
    });
    if tab {
        // Tab would otherwise also walk the keyboard focus through the
        // window's controls.
        ctx.memory_mut(|memory| memory.move_focus(egui::FocusDirection::None));
    }

    let held = std::mem::take(&mut cx.keyboard.hold);
    if changes.is_empty() && held.is_empty() {
        return;
    }
    let keyboard = &mut *cx.keyboard;
    let mut session = cx.session.lock();
    for key in held {
        if let Some(role) = layout::role(key) {
            keyboard.press(&mut session, key, role, false);
        }
    }
    for change in changes {
        match change {
            Change::Down(key, role, repeat) => keyboard.press(&mut session, key, role, repeat),
            Change::Up(key, role) => keyboard.lift(&mut session, key, role),
            Change::Lost => {
                keyboard.sustain = false;
                keyboard.stop_where(&mut session, |_| true);
            }
        }
    }
}

/// Runs when the views have: what they changed this frame (the selection,
/// a text field given the keyboard, a dialog put up) stops the notes it
/// must before the frame is over, and the settings are written down once
/// they have settled.
pub fn settle(ctx: &egui::Context, cx: &mut Cx) {
    let typing = ctx.text_edit_focused();
    let paused = paused(ctx, cx, typing);
    let is_paused = paused.is_some();
    let playing = assess(cx, paused.flatten());
    cx.keyboard.playing.track = playing.track;
    stop_the_orphaned(cx);
    if is_paused {
        cx.keyboard.release_all(cx.session);
    }
    cx.keyboard.remember(false, Some(&mut *cx.sounds));
}

/// The range of the keys in words: `"C4 – F5"`.
pub fn range_name(keyboard: &Keyboard) -> String {
    format!("{} \u{2013} {}", pitch_name(keyboard.lowest()), pitch_name(keyboard.highest()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn velocity_steps_along_tens_between_the_softest_and_the_loudest() {
        assert_eq!(stepped_velocity(100, 1), 110);
        assert_eq!(stepped_velocity(100, -1), 90);
        assert_eq!(stepped_velocity(120, 1), 127);
        assert_eq!(stepped_velocity(127, 1), 127, "nothing is louder");
        assert_eq!(stepped_velocity(127, -1), 120, "back onto the tens from the top");
        assert_eq!(stepped_velocity(10, -1), 1);
        assert_eq!(stepped_velocity(1, -1), 1, "a note is never silent");
        assert_eq!(stepped_velocity(1, 1), 10);
        // A velocity from elsewhere (an older file) steps to the nearest rung.
        assert_eq!(stepped_velocity(64, 1), 70);
        assert_eq!(stepped_velocity(64, -1), 60);
        assert_eq!(stepped_velocity(100, 3), 127);
        let mut velocity = 127;
        let mut presses = 0;
        while velocity > 1 {
            velocity = stepped_velocity(velocity, -1);
            presses += 1;
        }
        assert_eq!(presses, VELOCITIES.len() - 1);
    }

    #[test]
    fn it_starts_as_it_was_left_and_ignores_what_makes_no_sense() {
        let fresh = Keyboard::new(None, &Options::default());
        assert!(!fresh.is_on(), "off until it is switched on");
        assert_eq!((fresh.lowest(), fresh.highest(), fresh.velocity()), (60, 77, 100));
        assert_eq!(range_name(&fresh), "C4 \u{2013} F5");
        assert!(fresh.shows_key_map());

        let folder = tempfile::tempdir().unwrap();
        Remembered { on: true, lowest: 36, velocity: 70, key_map: false }.save(folder.path()).unwrap();
        let left = Keyboard::new(Some(folder.path().to_path_buf()), &Options::default());
        assert!(left.is_on() && !left.shows_key_map());
        assert_eq!((left.lowest(), left.velocity()), (36, 70));

        // A lowest key that is no C, or beyond the last octave, and a
        // velocity of nothing.
        Remembered { on: false, lowest: 61, velocity: 0, key_map: true }.save(folder.path()).unwrap();
        let odd = Keyboard::new(Some(folder.path().to_path_buf()), &Options::default());
        assert_eq!((odd.lowest(), odd.velocity()), (60, 1));
        Remembered { on: false, lowest: 120, velocity: 100, key_map: true }.save(folder.path()).unwrap();
        assert_eq!(Keyboard::new(Some(folder.path().to_path_buf()), &Options::default()).lowest(), 60);
    }

    #[test]
    fn the_command_line_switches_it_on_and_held_keys_are_not_remembered() {
        let folder = tempfile::tempdir().unwrap();
        let options = Options { on: true, hold: Vec::new() };
        let mut keyboard = Keyboard::new(Some(folder.path().to_path_buf()), &options);
        assert!(keyboard.is_on());
        // Asked for, it is the user's from then on: it is on, and that is
        // what the next session starts with.
        keyboard.remember(true, None);
        assert!(Remembered::load(folder.path()).is_some_and(|saved| saved.on));

        // Keys held for a picture are not a session anyone left.
        let staged = tempfile::tempdir().unwrap();
        Remembered::default().save(staged.path()).unwrap();
        let options = Options { on: false, hold: Options::holding("adg").unwrap() };
        let mut keyboard = Keyboard::new(Some(staged.path().to_path_buf()), &options);
        assert!(keyboard.is_on());
        keyboard.remember(true, None);
        assert_eq!(Remembered::load(staged.path()), Some(Remembered::default()), "the file is as it was");
    }

    #[test]
    fn letters_name_the_keys_to_hold() {
        assert_eq!(Options::holding("adg").unwrap(), [Key::A, Key::D, Key::G]);
        assert_eq!(Options::holding("A k ;").unwrap(), [Key::A, Key::K, Key::Semicolon]);
        assert!(Options::holding("").unwrap().is_empty());
        let error = Options::holding("az").unwrap_err();
        assert!(error.contains("'z'") && error.contains("A W S E"), "{error}");
    }

    #[test]
    fn the_letter_on_a_pitch_follows_the_octave() {
        let mut keyboard = Keyboard::new(None, &Options { on: true, hold: Vec::new() });
        assert_eq!(keyboard.letter(60), Some("A"));
        assert_eq!(keyboard.letter(61), Some("W"));
        assert_eq!(keyboard.letter(72), Some("K"));
        assert_eq!(keyboard.letter(77), Some("'"));
        assert_eq!((keyboard.letter(59), keyboard.letter(78)), (None, None));
        keyboard.lowest = 108;
        assert_eq!(keyboard.highest(), 125);
        keyboard.on = false;
        assert_eq!(keyboard.letter(108), None, "off, no key plays anything");
    }

    #[test]
    fn the_first_instrument_is_what_plays() {
        let catalog = trill_core::device::DeviceFactory::descriptors(&trill_dsp::BuiltinFactory);
        let mut project = trill_core::Project::new("t");
        let track = project.add_track("A");
        assert!(instrument(project.track(track).unwrap(), &catalog).is_none(), "an empty track plays nothing");
        let chain = trill_core::ChainRef::Track(track);
        project.add_device(chain, "builtin:delay", "Delay", None).unwrap();
        assert!(instrument(project.track(track).unwrap(), &catalog).is_none(), "an effect is no instrument");
        project.add_device(chain, "builtin:polysynth", "Synth", None).unwrap();
        assert_eq!(instrument(project.track(track).unwrap(), &catalog).unwrap().name, "Synth");
        // A device the catalog has not heard of may well be an instrument.
        assert!(instrument(project.track(track).unwrap(), &[]).is_some());
    }

    #[test]
    fn every_reason_for_silence_says_what_to_do_or_what_is_so() {
        let name = || "Lead".to_owned();
        let reasons = [
            Silence::Typing,
            Silence::Searching,
            Silence::NoTrack,
            Silence::Group(name()),
            Silence::Return(name()),
            Silence::NoInstrument(name()),
            Silence::Muted(name()),
            Silence::Soloed(name()),
            Silence::NoOutput,
        ];
        for reason in reasons {
            assert!(reason.text().len() > 10, "{reason:?}");
        }
        assert_eq!(Silence::NoInstrument(name()).text(), "Lead has no instrument \u{00B7} add one");
    }
}
