//! Plugin editors: the window a plugin brings with it, opened from the
//! device's panel or from its chip in the mixer.
//!
//! The windows are the plugins' own and belong to the session, which is
//! asked, never assumed: the user closes an editor with its own close
//! button as often as with ours. Which devices have one is part of the
//! [`Snapshot`](super::snapshot::Snapshot) and which are open right now of
//! [`Live`](super::snapshot::Live), both read while the window has the
//! session anyway.
//!
//! Opening is the slow part. A plugin builds its interface when asked for
//! it, which can take seconds, so the request goes to a worker thread and
//! the window paints on; the control that asked shows the editor as on its
//! way meanwhile. Closing goes the same way. What the user then does in an
//! editor reaches the project by itself: the runtime's housekeeping takes
//! it in a few times a second.

use super::state::Cx;
use super::toast::Toasts;
use std::sync::mpsc::{Receiver, Sender, channel};
use trill_core::device::EditorOptions;
use trill_core::{ChainRef, DeviceId};

/// What a worker reports when it has opened or closed an editor.
struct Outcome {
    device: DeviceId,
    /// Why it could not, in words for the user.
    failure: Option<String>,
}

/// The window's side of the editors: who they belong to, and which are on
/// their way.
pub struct Editors {
    /// The application's own window, for editors to belong to (an `HWND`
    /// on Windows): they stay above it, and are minimized and closed with
    /// it. `None` in a run without a window.
    owner: Option<isize>,
    /// Whether an editor takes the keyboard as it opens. Not in a run
    /// nobody is sitting at.
    activate: bool,
    /// The devices a worker is busy with, and whether it is opening
    /// (`true`) or closing their editor.
    busy: Vec<(DeviceId, bool)>,
    sender: Sender<Outcome>,
    receiver: Receiver<Outcome>,
}

impl Editors {
    pub fn new(activate: bool) -> Self {
        let (sender, receiver) = channel();
        Self { owner: None, activate, busy: Vec::new(), sender, receiver }
    }

    /// Names the window that editors opened from now on belong to.
    pub fn set_owner(&mut self, owner: Option<isize>) {
        self.owner = owner;
    }

    /// Takes in what the workers finished: a failure is told to the user.
    /// Call before the live values are read, so that an editor that has
    /// just opened is seen as open on the same frame.
    pub fn poll(&mut self, toasts: &mut Toasts) {
        for outcome in self.receiver.try_iter() {
            self.busy.retain(|(device, _)| *device != outcome.device);
            if let Some(failure) = outcome.failure {
                toasts.error(failure);
            }
        }
    }

    /// Whether a worker is opening (`Some(true)`) or closing the editor of
    /// `device`.
    fn busy_with(&self, device: DeviceId) -> Option<bool> {
        self.busy.iter().find(|(busy, _)| *busy == device).map(|(_, opening)| *opening)
    }

    /// True while anything is on its way.
    #[cfg(test)]
    pub fn is_busy(&self) -> bool {
        !self.busy.is_empty()
    }
}

/// Whether `device` brings a window of its own. Built-in devices do not;
/// their panel is all there is to them.
pub fn has(cx: &Cx, device: DeviceId) -> bool {
    cx.snap.has_editor(device)
}

/// Whether the editor of `device` is open, or on its way to be: what its
/// button shows.
pub fn is_open(cx: &Cx, device: DeviceId) -> bool {
    cx.editors.busy_with(device).unwrap_or_else(|| cx.live.editor_open(device))
}

/// What the editor's window is called: the device and where it sits, so
/// that three instances of one synth are told apart.
fn title(cx: &Cx, device: DeviceId) -> String {
    let project = &cx.snap.project;
    let name = project.device(device).map_or("Device", |device| device.name.as_str());
    let track = match project.device_chain(device) {
        Ok(ChainRef::Track(track)) => project.track(track).ok().map(|track| track.name.as_str()),
        _ => None,
    };
    match track {
        Some(track) => format!("{name} on {track}"),
        None => format!("{name} on the master"),
    }
}

/// Opens the editor of `device`, or brings it to the front if it is open.
/// Returns at once; the editor appears when the plugin has built it, and a
/// failure arrives as a message.
pub fn open(cx: &mut Cx, device: DeviceId) {
    if cx.editors.busy_with(device).is_some() {
        return;
    }
    let title = title(cx, device);
    let name = cx.snap.project.device(device).map_or_else(|_| "the device".to_owned(), |device| device.name.clone());
    let editors = &mut *cx.editors;
    let options = EditorOptions { title, owner: editors.owner, position: None, activate: editors.activate };
    editors.busy.push((device, true));
    let sender = editors.sender.clone();
    cx.sounds.quietly(move |runtime| {
        let opened = runtime.session.lock().open_device_editor(device, &options);
        let failure = opened.err().map(|error| format!("The editor of {name} did not open: {error}"));
        // The receiver is gone only when the window has closed.
        let _ = sender.send(Outcome { device, failure });
        Ok(())
    });
}

/// Closes the editor of `device`, if it is open.
pub fn close(cx: &mut Cx, device: DeviceId) {
    if cx.editors.busy_with(device).is_some() {
        return;
    }
    cx.editors.busy.push((device, false));
    let sender = cx.editors.sender.clone();
    cx.sounds.quietly(move |runtime| {
        runtime.session.lock().close_device_editor(device);
        let _ = sender.send(Outcome { device, failure: None });
        Ok(())
    });
}

/// Opens the editor of `device` if it is closed and closes it if it is
/// open: what its button does. While one is on its way the button waits.
pub fn toggle(cx: &mut Cx, device: DeviceId) {
    if cx.live.editor_open(device) {
        close(cx, device);
    } else {
        open(cx, device);
    }
}

/// What the status bar says of the button for `name`'s editor.
pub fn hint(cx: &Cx, device: DeviceId, name: &str) -> String {
    match (cx.editors.busy_with(device), cx.live.editor_open(device)) {
        (Some(true), _) => format!("Opening the editor of {name}\u{2026}"),
        (Some(false), _) => format!("Closing the editor of {name}\u{2026}"),
        (None, true) => format!("The editor of {name} is open \u{00B7} click to close it"),
        (None, false) => format!("Open the editor of {name}: the plugin\u{2019}s own window"),
    }
}

/// Opens the editor of the first device called `name`: what `--editor`
/// asks for at startup.
pub fn open_named(cx: &mut Cx, name: &str) {
    let project = &cx.snap.project;
    let mut devices = project.tracks.iter().flat_map(|track| track.devices.iter()).chain(&project.master.devices);
    match devices.find(|device| device.name == name).map(|device| device.id) {
        Some(device) if has(cx, device) => open(cx, device),
        Some(_) => cx.state.toasts.error(format!("{name} has no editor of its own: its panel is all there is to it")),
        None => cx.state.toasts.error(format!("No device is called {name}, so no editor was opened")),
    }
}
