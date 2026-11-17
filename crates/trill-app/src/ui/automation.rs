//! What an automation target is to the views: its name, the scale its
//! values are drawn on, and the actions every automated control offers.
//! The lane editor is in the arrangement; this is what it shares with the
//! track headers, the device rack and the mixer.
//!
//! An automated control is not locked: moving it takes hold of it. The
//! envelope is switched off (not thrown away) and the control stays where
//! it is put, until the envelope is switched on again from the mark in the
//! control's corner or from its lane. [`Hold`] says which of the three a
//! control is in, and [`respond`] is what every such control does about it.

use super::snapshot::Snapshot;
use super::state::{Cx, LaneOwner, lane_owner};
use super::units::{self, ParamScale};
use eframe::egui::{self, Rect, Ui};
use trill_core::device::{ParamInfo, ParamKind};
use trill_core::project::MIN_VOLUME_DB;
use trill_core::{AutomationPoint, AutomationTarget, ControllerKind, TrackKind, Travel};
use trill_noir::{Automation, BarOutput, MenuItem, paint};

/// How an envelope's values map onto the height of its lane, and how they
/// read. The same mappings as the controls they automate, so a point half
/// way up the lane is where the fader would be half way up. And since an
/// envelope's lines are straight along the travel of its control, they are
/// straight lines in its lane.
#[derive(Clone, Debug, PartialEq)]
pub enum Scale {
    /// Decibels on the fader scale: a volume, or a send, whose bottom is
    /// shut.
    Volume,
    /// -1 (left) to 1 (right).
    Pan,
    Param(ParamInfo),
    /// A MIDI controller in a clip: 0 to 127 as the controller counts,
    /// the pitch wheel from fully down to fully up, a pedal up or down.
    Controller(ControllerKind),
}

impl Scale {
    /// The scale of `target`; `None` for a device parameter the snapshot
    /// does not describe (its device failed to load, or is not covered yet).
    pub fn of(snapshot: &Snapshot, target: &AutomationTarget) -> Option<Self> {
        Some(match target {
            AutomationTarget::TrackVolume { .. } | AutomationTarget::MasterVolume | AutomationTarget::Send { .. } => {
                Scale::Volume
            }
            AutomationTarget::TrackPan { .. } => Scale::Pan,
            AutomationTarget::DeviceParam { device, param } => {
                Scale::Param(snapshot.param(*device, param)?.info.clone())
            }
        })
    }

    /// Height in the lane, 0 (bottom) to 1 (top), of `value`.
    pub fn position(&self, value: f64) -> f32 {
        match self {
            Scale::Volume => units::db_to_fader(value as f32),
            Scale::Pan => ((value as f32 + 1.0) / 2.0).clamp(0.0, 1.0),
            Scale::Param(info) => ParamScale::of(info).normalize(info, value) as f32,
            Scale::Controller(kind) => {
                let (low, high) = kind.range();
                (((value - low) / (high - low)) as f32).clamp(0.0, 1.0)
            }
        }
    }

    /// The value at height `position`, rounded the way the control itself
    /// would set it.
    pub fn value(&self, position: f32) -> f64 {
        match self {
            // Tenths of a decibel: finer is not audible, and it reads cleanly.
            Scale::Volume => (units::fader_to_db(position) as f64 * 10.0).round() / 10.0,
            Scale::Pan => {
                let pan = ((position.clamp(0.0, 1.0) * 2.0 - 1.0) as f64 * 100.0).round() / 100.0;
                // A small dead zone makes center easy to find again.
                if pan.abs() < 0.03 { 0.0 } else { pan }
            }
            Scale::Param(info) => ParamScale::of(info).denormalize(info, position as f64),
            // A pedal is down or up; the wheel moves in hundredths, with a
            // dead zone that makes its rest easy to find again; every
            // other controller in the 128 steps MIDI gives it.
            Scale::Controller(kind) if kind.is_switch() => f64::from(u8::from(position >= 0.5)),
            Scale::Controller(ControllerKind::PitchBend) => {
                let bend = ((position.clamp(0.0, 1.0) * 2.0 - 1.0) as f64 * 100.0).round() / 100.0;
                if bend.abs() < 0.03 { 0.0 } else { bend }
            }
            Scale::Controller(_) => (position.clamp(0.0, 1.0) as f64 * 127.0).round() / 127.0,
        }
    }

    pub fn format(&self, value: f64) -> String {
        match self {
            Scale::Volume => units::format_db(value as f32),
            Scale::Pan => units::format_pan(value as f32),
            Scale::Param(info) => units::format_param(info, value),
            Scale::Controller(kind) if kind.is_switch() => if value >= 0.5 { "Down" } else { "Up" }.to_owned(),
            Scale::Controller(ControllerKind::PitchBend) => format!("{:+.0} %", value * 100.0),
            Scale::Controller(_) => format!("{:.0}", value * 127.0),
        }
    }

    /// Where the control rests when nothing moves it: the line the lane
    /// draws for reference.
    pub fn default_position(&self) -> f32 {
        match self {
            Scale::Volume => units::db_to_fader(0.0),
            Scale::Pan => 0.5,
            Scale::Param(info) => self.position(info.default),
            Scale::Controller(kind) => self.position(kind.rest().unwrap_or(0.0)),
        }
    }

    /// True for switches and choices, whose values come in steps: a line
    /// between two of them could only be heard as a jump.
    pub fn stepped(&self) -> bool {
        match self {
            Scale::Param(info) => info.kind != ParamKind::Float,
            Scale::Controller(kind) => kind.is_switch(),
            _ => false,
        }
    }

    /// The travel of the control the scale is laid out by: what a new
    /// envelope's lines are straight along.
    pub fn travel(&self) -> Travel {
        match self {
            Scale::Volume => Travel::FADER,
            Scale::Pan | Scale::Controller(_) => Travel::Linear,
            Scale::Param(info) => ParamScale::of(info).0,
        }
    }

    /// True when a line of an envelope that travels along `lines` is a
    /// straight line on this scale: when the envelope follows its control.
    /// One from a file saved before envelopes did is straight in its units,
    /// and on a fader's or a frequency's scale that is a curve.
    pub fn straight(&self, lines: Travel) -> bool {
        self.travel() == lines
    }

    /// What the two ways an envelope's lines can go are called in a menu,
    /// if they differ on this scale: along the control, and straight in
    /// the units.
    pub fn travel_words(&self) -> Option<(&'static str, String)> {
        let unit = match self {
            Scale::Volume => "dB",
            Scale::Param(info) if !info.unit.trim().is_empty() => info.unit.trim(),
            _ => "its units",
        };
        match self.travel() {
            Travel::Linear => None,
            Travel::Fader { .. } => Some(("Lines follow the fader", format!("Lines straight in {unit}"))),
            Travel::Log => Some(("Lines follow the knob", format!("Lines straight in {unit}"))),
        }
    }
}

/// What the lines of `target`'s envelope are straight along: as its lane
/// has it, or, where nothing is written yet, as its control travels, which
/// is what the first point written will make of it. And the travel of the
/// control itself.
pub fn lines(snapshot: &Snapshot, target: &AutomationTarget) -> (Travel, Travel) {
    let control = Scale::of(snapshot, target).map(|scale| scale.travel()).unwrap_or_default();
    (snapshot.lane(target).map_or(control, |lane| lane.travel), control)
}

/// Rewrites the envelope of `target` as one edit against the envelope as it
/// is right now: `change` is given its points and what its lines are
/// straight along, and returns the points it is to have. An envelope that
/// is there keeps its travel; a new one follows its control.
pub fn rewrite(
    cx: &mut Cx,
    target: &AutomationTarget,
    change: impl FnOnce(&[AutomationPoint], Travel) -> Vec<AutomationPoint>,
) -> bool {
    let (_, control) = lines(cx.snap, target);
    let target = target.clone();
    cx.edit(|project| match project.automation.iter().find(|lane| lane.target == target) {
        Some(lane) => {
            let next = change(&lane.points, lane.travel);
            project.set_automation(target, next)
        }
        None => project.write_automation(target, change(&[], control), control),
    })
    .is_some()
}

/// The value `target` has while no envelope drives it.
pub fn resting_value(snapshot: &Snapshot, target: &AutomationTarget) -> Option<f64> {
    match target {
        AutomationTarget::TrackVolume { track } => snapshot.track(*track).map(|track| track.volume_db as f64),
        AutomationTarget::TrackPan { track } => snapshot.track(*track).map(|track| track.pan as f64),
        AutomationTarget::MasterVolume => Some(snapshot.project.master.volume_db as f64),
        AutomationTarget::DeviceParam { device, param } => snapshot.param(*device, param).map(|state| state.value),
        // A track that has no send to the return sends it nothing.
        AutomationTarget::Send { track, to } => snapshot.track(*track).map(|track| {
            let set = track.sends.iter().find(|send| send.to == *to);
            set.map_or(MIN_VOLUME_DB, |send| send.level_db) as f64
        }),
    }
}

/// What `target` is called and, for a device's parameter, the device.
pub fn names(snapshot: &Snapshot, target: &AutomationTarget) -> (String, Option<String>) {
    match target {
        AutomationTarget::TrackVolume { .. } | AutomationTarget::MasterVolume => ("Volume".to_owned(), None),
        AutomationTarget::TrackPan { .. } => ("Pan".to_owned(), None),
        AutomationTarget::DeviceParam { device, param } => {
            let device_name = snapshot.project.device(*device).ok().map(|device| device.name.clone());
            let described = snapshot.param(*device, param).map(|state| units::short_param_name(&state.info.name));
            (described.unwrap_or(param).to_owned(), device_name)
        }
        AutomationTarget::Send { to, .. } => {
            (send_name(snapshot.track(*to).map_or("a return", |track| track.name.as_str())), None)
        }
    }
}

/// What a send to the return called `name` is called.
pub fn send_name(name: &str) -> String {
    format!("Send to {name}")
}

/// One thing a lane of `owner` can show.
#[derive(Clone, Debug, PartialEq)]
pub struct Choice {
    pub target: AutomationTarget,
    pub name: String,
    /// The device it belongs to; `None` for the mixer's own controls.
    pub device: Option<String>,
}

/// Everything that can be automated on `owner`: volume and pan, a send to
/// each return, then every parameter of its devices in chain order.
pub fn choices(snapshot: &Snapshot, owner: LaneOwner) -> Vec<Choice> {
    let mixer = |target: AutomationTarget, name: &str| Choice { target, name: name.to_owned(), device: None };
    let (mut found, devices) = match owner {
        LaneOwner::Track(track) => {
            let devices = snapshot.track(track).map(|track| track.devices.as_slice()).unwrap_or_default();
            let mut own = vec![
                mixer(AutomationTarget::TrackVolume { track }, "Volume"),
                mixer(AutomationTarget::TrackPan { track }, "Pan"),
            ];
            // A return has nowhere to send to.
            if snapshot.track(track).is_some_and(|track| track.kind != TrackKind::Return) {
                let returns = snapshot.project.tracks.iter().filter(|other| other.kind == TrackKind::Return);
                own.extend(returns.map(|to| mixer(AutomationTarget::Send { track, to: to.id }, &send_name(&to.name))));
            }
            (own, devices)
        }
        LaneOwner::Master => {
            (vec![mixer(AutomationTarget::MasterVolume, "Volume")], snapshot.project.master.devices.as_slice())
        }
    };
    for device in devices {
        let Some(Ok(params)) = snapshot.params.get(&device.id) else { continue };
        found.extend(params.iter().map(|state| Choice {
            target: AutomationTarget::DeviceParam { device: device.id, param: state.info.id.clone() },
            name: units::short_param_name(&state.info.name).to_owned(),
            device: Some(device.name.clone()),
        }));
    }
    found
}

/// Unfolds the lane of `target` and brings it into view.
pub fn show_lane(cx: &mut Cx, target: AutomationTarget) {
    let project = &cx.snap.project;
    let Some(owner) = lane_owner(project, &target) else { return };
    let view = &mut cx.state.arrangement;
    // A lane inside a folded group would open out of sight.
    if let LaneOwner::Track(track) = owner
        && let Some(group) = project.track(track).ok().and_then(|track| track.group)
    {
        view.folded.remove(&group);
    }
    view.open_lane(owner, target.clone());
    view.reveal = Some(target);
}

/// Shows or hides the lanes of a track or of the master: `A`, and the chip
/// on its header. Showing opens a lane for every envelope it has, or for
/// its volume if it has none yet.
pub fn toggle_lanes(cx: &mut Cx, owner: LaneOwner) {
    let view = &mut cx.state.arrangement;
    if view.lanes.iter().any(|lane| lane.owner == owner) {
        view.lanes.retain(|lane| lane.owner != owner);
        view.points = None;
        return;
    }
    let project = &cx.snap.project;
    let automated = project.automation.iter().map(|lane| &lane.target);
    let mut targets: Vec<AutomationTarget> =
        automated.filter(|target| lane_owner(project, target) == Some(owner)).cloned().collect();
    if targets.is_empty() {
        targets.push(match owner {
            LaneOwner::Track(track) => AutomationTarget::TrackVolume { track },
            LaneOwner::Master => AutomationTarget::MasterVolume,
        });
    }
    for target in targets {
        show_lane(cx, target);
    }
}

/// True if any envelope belongs to `owner`.
#[cfg(test)]
pub fn has_automation(snapshot: &Snapshot, owner: LaneOwner) -> bool {
    owner_automation(snapshot, owner) != Automation::None
}

/// What the envelopes of `owner` come to, for the mark on the chip that
/// unfolds its lanes: none, all switched on, or at least one switched off
/// (which is the one worth looking for).
pub fn owner_automation(snapshot: &Snapshot, owner: LaneOwner) -> Automation {
    let project = &snapshot.project;
    let mut lanes = project.automation.iter().filter(|lane| lane_owner(project, &lane.target) == Some(owner));
    let Some(first) = lanes.next() else { return Automation::None };
    if first.enabled && lanes.all(|lane| lane.enabled) { Automation::On } else { Automation::Off }
}

/// Paints the mark of `owner`'s envelopes in the corner of the chip at
/// `rect`. Only a sign here: the chip itself unfolds the lanes.
pub fn paint_owner_mark(painter: &egui::Painter, cx: &Cx, owner: LaneOwner, rect: Rect) {
    match owner_automation(cx.snap, owner) {
        Automation::None => {}
        Automation::On => paint::corner_mark(painter, rect, cx.theme.signal),
        Automation::Off => paint::hollow_corner_mark(painter, rect, cx.theme.signal),
    }
}

/// The rows a control's context menu offers for its automation.
pub fn menu_items(ui: &mut Ui, cx: &mut Cx, target: &AutomationTarget) {
    let theme = cx.theme;
    let open = cx.state.arrangement.lane_open(target);
    if MenuItem::new(if open { "Hide automation" } else { "Show automation" }).show(ui, theme).clicked() {
        if open {
            cx.state.arrangement.close_lane(target);
        } else {
            show_lane(cx, target.clone());
        }
    }
    // Off, the envelope is kept and the control is the user's; on, the
    // envelope sets it again.
    let on = cx.snap.lane(target).map(|lane| lane.enabled);
    let label = if on == Some(false) { "Switch automation back on" } else { "Switch automation off" };
    if MenuItem::new(label).enabled(on.is_some()).show(ui, theme).clicked() {
        set_enabled(cx, target, on == Some(false));
    }
    if MenuItem::new("Clear automation").enabled(on.is_some()).show(ui, theme).clicked() {
        let target = target.clone();
        cx.edit(|project| project.set_automation(target, Vec::new()));
    }
}

/// How an envelope stands to the control it automates, as of this frame.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum Hold {
    /// There is no envelope: the control is the user's.
    Free,
    /// The envelope sets the control: to this value at the playhead, which
    /// is what is heard and what the control shows.
    Driven(f64),
    /// There is an envelope and it is switched off: the user holds the
    /// control, which shows the value it was given by hand.
    Held,
}

impl Hold {
    pub fn of(cx: &Cx, target: &AutomationTarget) -> Self {
        match cx.snap.lane(target) {
            None => Hold::Free,
            Some(lane) if lane.enabled => Hold::Driven(lane.value_at(cx.live.position)),
            Some(_) => Hold::Held,
        }
    }

    /// What the envelope makes of the control right now, if it drives it.
    pub fn driven(self) -> Option<f64> {
        match self {
            Hold::Driven(value) => Some(value),
            Hold::Free | Hold::Held => None,
        }
    }

    /// The mark the control carries in its corner.
    pub fn mark(self) -> Automation {
        match self {
            Hold::Free => Automation::None,
            Hold::Driven(_) => Automation::On,
            Hold::Held => Automation::Off,
        }
    }

    /// The hint for the control: `what` it is and reads, then `gestures`,
    /// what the pointer does to it, or what its automation has to say.
    pub fn hint(self, what: &str, gestures: &str) -> String {
        match self {
            Hold::Free => format!("{what} \u{00B7} {gestures}"),
            Hold::Driven(_) => format!("{what} \u{00B7} {DRIVEN_HINT}"),
            Hold::Held => format!("{what} \u{00B7} {HELD_HINT}"),
        }
    }
}

/// What the hint says of a control an envelope drives.
pub const DRIVEN_HINT: &str = "automated \u{00B7} drag to take hold of it: its envelope is switched off, not lost";
/// And of one the user holds.
pub const HELD_HINT: &str = "its automation is off \u{00B7} click the mark in its corner to switch it back on";
/// The hint of the mark itself.
const RESUME_HINT: &str =
    "Automation is off: this stays where you set it \u{00B7} click to let the envelope set it again";

/// Switches the envelope of `target` on or off, as one undo step.
pub fn set_enabled(cx: &mut Cx, target: &AutomationTarget, enabled: bool) {
    let target = target.clone();
    cx.edit(|project| project.set_automation_enabled(&target, enabled));
}

/// Call before setting an automatable control by hand. If its envelope was
/// driving it, the envelope is switched off first, inside the undo gesture
/// of the drag (or of the click): taking hold and the value set while
/// holding are then one undo step, and undoing it gives the control back.
pub fn take_hold(cx: &mut Cx, target: &AutomationTarget, hold: Hold) {
    if matches!(hold, Hold::Driven(_)) {
        cx.begin_gesture();
        set_enabled(cx, target, false);
    }
}

/// The mark of `hold` on a control that is not a Bar or a Fader (those
/// carry their own): painted in the corner of `rect`, and, for an envelope
/// that is off, the click that switches it back on. Place it after the
/// control. `name` is what the control is called.
pub fn mark(ui: &Ui, cx: &mut Cx, target: &AutomationTarget, hold: Hold, rect: Rect, id: egui::Id, name: &str) {
    let mark = trill_noir::automation_mark(ui, cx.theme, rect, id, hold.mark(), name);
    resume_on_click(cx, target, mark.as_ref());
}

fn resume_on_click(cx: &mut Cx, target: &AutomationTarget, mark: Option<&egui::Response>) {
    if let Some(mark) = mark {
        cx.hint(mark, RESUME_HINT);
        if mark.clicked() {
            set_enabled(cx, target, true);
        }
    }
}

/// What every Bar and Fader that an envelope may drive does with what the
/// user did to it this frame: says what it is in the status bar, opens the
/// undo gesture of a drag, takes hold of the control if its envelope was
/// driving it, switches the envelope back on if its mark was clicked, and
/// offers the automation menu. Returns the position the user asked for,
/// for the caller to turn into a value and set.
pub fn respond(
    cx: &mut Cx,
    target: &AutomationTarget,
    hold: Hold,
    output: &BarOutput,
    what: &str,
    gestures: &str,
) -> Option<f32> {
    cx.hint(&output.response, hold.hint(what, gestures));
    resume_on_click(cx, target, output.mark.as_ref());
    if output.pressed {
        cx.begin_gesture();
    }
    if output.frac.is_some() {
        take_hold(cx, target, hold);
    }
    output.response.context_menu(|ui| menu_items(ui, cx, target));
    output.frac
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use trill_core::ChainRef;
    use trill_core::device::CompositeFactory;
    use trill_dsp::BuiltinFactory;
    use trill_engine::{AudioBackend, Session};

    fn session() -> Session {
        let factory = Arc::new(CompositeFactory::new(vec![Box::new(BuiltinFactory)]));
        Session::new(factory, AudioBackend::Null { sample_rate: 48_000 }).unwrap()
    }

    #[test]
    fn mixer_scales_match_the_controls() {
        assert_eq!(Scale::Volume.position(0.0), units::db_to_fader(0.0));
        assert_eq!(Scale::Volume.value(units::db_to_fader(-12.0)), -12.0);
        assert_eq!(Scale::Volume.value(0.3333), (units::fader_to_db(0.3333) as f64 * 10.0).round() / 10.0);
        assert_eq!(Scale::Volume.format(-6.0), "\u{2212}6.0 dB");
        assert!(!Scale::Volume.stepped());
        // A fader's envelope is straight in its lane when it follows the
        // fader, and a curve there when it is straight in decibels.
        assert!(Scale::Volume.straight(Travel::FADER) && !Scale::Volume.straight(Travel::Linear));
        assert_eq!(Scale::Volume.travel_words(), Some(("Lines follow the fader", "Lines straight in dB".to_owned())));

        assert_eq!(Scale::Pan.position(-1.0), 0.0);
        assert_eq!(Scale::Pan.position(0.5), 0.75);
        assert_eq!(Scale::Pan.value(0.75), 0.5);
        assert_eq!(Scale::Pan.value(0.505), 0.0, "close to the middle is the middle");
        assert_eq!(Scale::Pan.format(-0.5), "L50");
        assert_eq!(Scale::Pan.default_position(), 0.5);
        assert!(Scale::Pan.straight(Travel::Linear));
        assert_eq!(Scale::Pan.travel_words(), None, "there is one way only for a pan to go");
    }

    #[test]
    fn a_parameter_is_drawn_on_its_own_scale_and_in_its_own_units() {
        let mut session = session();
        let track = session.edit(|project| Ok(project.add_track("Lead"))).unwrap();
        let synth = session.add_device(ChainRef::Track(track), "builtin:polysynth", None, None).unwrap();
        let snapshot = Snapshot::capture(&session, &[track]);
        let cutoff = AutomationTarget::DeviceParam { device: synth, param: "cutoff".into() };
        let wave = AutomationTarget::DeviceParam { device: synth, param: "osc1_wave".into() };

        let scale = Scale::of(&snapshot, &cutoff).unwrap();
        let Scale::Param(info) = &scale else { panic!("a device parameter") };
        // A frequency: equal ratios are equal distances.
        let middle = scale.value(0.5);
        assert!((middle - (info.min * info.max).sqrt()).abs() < 1.0, "{middle}");
        assert!((scale.position(middle) - 0.5).abs() < 1e-4);
        assert!(scale.format(1000.0).contains("kHz"));
        assert!(scale.straight(Travel::Log) && !scale.straight(Travel::Linear) && !scale.stepped());
        assert_eq!(scale.travel_words(), Some(("Lines follow the knob", "Lines straight in Hz".to_owned())));
        assert_eq!(scale.default_position(), scale.position(info.default));
        assert!(Scale::of(&snapshot, &wave).unwrap().stepped(), "a choice moves in steps");

        assert_eq!(names(&snapshot, &cutoff), ("Filter Cutoff".to_owned(), Some("Poly Synth".to_owned())));
        assert_eq!(names(&snapshot, &AutomationTarget::TrackPan { track }), ("Pan".to_owned(), None));
        assert_eq!(resting_value(&snapshot, &cutoff), Some(info.default));
        assert_eq!(resting_value(&snapshot, &AutomationTarget::TrackVolume { track }), Some(0.0));

        // A snapshot that does not cover the track cannot describe it.
        let uncovered = Snapshot::capture(&session, &[]);
        assert_eq!(Scale::of(&uncovered, &cutoff), None);
        assert_eq!(names(&uncovered, &cutoff).0, "cutoff", "the parameter's id stands in for its name");
    }

    #[test]
    fn a_lane_can_show_the_mixer_and_every_device_parameter() {
        let mut session = session();
        let track = session.edit(|project| Ok(project.add_track("Lead"))).unwrap();
        session.add_device(ChainRef::Track(track), "builtin:polysynth", None, None).unwrap();
        session.add_device(ChainRef::Track(track), "builtin:delay", None, None).unwrap();
        let snapshot = Snapshot::capture(&session, &[track]);

        let found = choices(&snapshot, LaneOwner::Track(track));
        assert_eq!((found[0].name.as_str(), found[1].name.as_str()), ("Volume", "Pan"));
        assert!(found[0].device.is_none());
        let devices: Vec<&str> = found[2..].iter().filter_map(|choice| choice.device.as_deref()).collect();
        assert!(devices.starts_with(&["Poly Synth"]) && devices.ends_with(&["Delay"]), "in chain order");
        assert_eq!(devices.len(), found.len() - 2);
        assert!(found.iter().any(|choice| choice.name == "Filter Cutoff"));

        // With returns in the song, a send to each is among the mixer's own.
        let (echo, hall) = session
            .edit(|project| {
                let echo = project.add_track_of_kind("Echo", TrackKind::Return);
                Ok((echo, project.add_track_of_kind("Hall", TrackKind::Return)))
            })
            .unwrap();
        let snapshot = Snapshot::capture(&session, &[track]);
        let found = choices(&snapshot, LaneOwner::Track(track));
        let listed: Vec<&str> = found[..4].iter().map(|choice| choice.name.as_str()).collect();
        assert_eq!(listed, ["Volume", "Pan", "Send to Echo", "Send to Hall"]);
        let throw = AutomationTarget::Send { track, to: echo };
        assert_eq!((&found[2].target, &found[2].device), (&throw, &None));
        assert_eq!(names(&snapshot, &throw), ("Send to Echo".to_owned(), None));
        assert_eq!(Scale::of(&snapshot, &throw), Some(Scale::Volume));
        assert_eq!(resting_value(&snapshot, &throw), Some(MIN_VOLUME_DB as f64), "no send by hand is none");
        assert_eq!(lines(&snapshot, &throw), (Travel::FADER, Travel::FADER));
        // A return sends nowhere.
        let of_return: Vec<String> =
            choices(&snapshot, LaneOwner::Track(hall)).into_iter().map(|choice| choice.name).collect();
        assert_eq!(of_return, ["Volume", "Pan"]);

        let master = choices(&snapshot, LaneOwner::Master);
        assert_eq!(master[0].target, AutomationTarget::MasterVolume);
        assert!(master.iter().skip(1).all(|choice| choice.device.as_deref() == Some("Limiter")));
        assert!(!has_automation(&snapshot, LaneOwner::Track(track)));
    }
}
