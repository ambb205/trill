//! What an automation target is to the views: its name, the scale its
//! values are drawn on, and the actions every automated control offers.
//! The lane editor is in the arrangement; this is what it shares with the
//! track headers, the device rack and the mixer.

use super::snapshot::Snapshot;
use super::state::{Cx, LaneOwner, lane_owner};
use super::units::{self, ParamScale};
use eframe::egui::Ui;
use trill_core::AutomationTarget;
use trill_core::device::{ParamInfo, ParamKind};
use trill_noir::MenuItem;

/// How an envelope's values map onto the height of its lane, and how they
/// read. The same mappings as the controls they automate, so a point half
/// way up the lane is where the fader would be half way up.
#[derive(Clone, Debug, PartialEq)]
pub enum Scale {
    /// Decibels on the fader scale.
    Volume,
    /// -1 (left) to 1 (right).
    Pan,
    Param(ParamInfo),
}

impl Scale {
    /// The scale of `target`; `None` for a device parameter the snapshot
    /// does not describe (its device failed to load, or is not covered yet).
    pub fn of(snapshot: &Snapshot, target: &AutomationTarget) -> Option<Self> {
        Some(match target {
            AutomationTarget::TrackVolume { .. } | AutomationTarget::MasterVolume => Scale::Volume,
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
        }
    }

    pub fn format(&self, value: f64) -> String {
        match self {
            Scale::Volume => units::format_db(value as f32),
            Scale::Pan => units::format_pan(value as f32),
            Scale::Param(info) => units::format_param(info, value),
        }
    }

    /// Where the control rests when nothing moves it: the line the lane
    /// draws for reference.
    pub fn default_position(&self) -> f32 {
        match self {
            Scale::Volume => units::db_to_fader(0.0),
            Scale::Pan => 0.5,
            Scale::Param(info) => self.position(info.default),
        }
    }

    /// True for switches and choices, whose values come in steps: a line
    /// between two of them could only be heard as a jump.
    pub fn stepped(&self) -> bool {
        matches!(self, Scale::Param(info) if info.kind != ParamKind::Float)
    }

    /// True when equal distances in the lane are equal differences in
    /// value, so a straight envelope segment is a straight line.
    pub fn linear(&self) -> bool {
        match self {
            Scale::Volume => false,
            Scale::Pan => true,
            Scale::Param(info) => ParamScale::of(info) == ParamScale::Linear,
        }
    }
}

/// The value `target` has while no envelope drives it.
pub fn resting_value(snapshot: &Snapshot, target: &AutomationTarget) -> Option<f64> {
    match target {
        AutomationTarget::TrackVolume { track } => snapshot.track(*track).map(|track| track.volume_db as f64),
        AutomationTarget::TrackPan { track } => snapshot.track(*track).map(|track| track.pan as f64),
        AutomationTarget::MasterVolume => Some(snapshot.project.master.volume_db as f64),
        AutomationTarget::DeviceParam { device, param } => snapshot.param(*device, param).map(|state| state.value),
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
    }
}

/// One thing a lane of `owner` can show.
#[derive(Clone, Debug, PartialEq)]
pub struct Choice {
    pub target: AutomationTarget,
    pub name: String,
    /// The device it belongs to; `None` for the mixer's own controls.
    pub device: Option<String>,
}

/// Everything that can be automated on `owner`: volume and pan, then every
/// parameter of its devices in chain order.
pub fn choices(snapshot: &Snapshot, owner: LaneOwner) -> Vec<Choice> {
    let mixer = |target: AutomationTarget, name: &str| Choice { target, name: name.to_owned(), device: None };
    let (mut found, devices) = match owner {
        LaneOwner::Track(track) => {
            let devices = snapshot.track(track).map(|track| track.devices.as_slice()).unwrap_or_default();
            let own = vec![
                mixer(AutomationTarget::TrackVolume { track }, "Volume"),
                mixer(AutomationTarget::TrackPan { track }, "Pan"),
            ];
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
pub fn has_automation(snapshot: &Snapshot, owner: LaneOwner) -> bool {
    let project = &snapshot.project;
    project.automation.iter().any(|lane| lane_owner(project, &lane.target) == Some(owner))
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
    let automated = cx.snap.envelope(target).is_some();
    if MenuItem::new("Clear automation").enabled(automated).show(ui, theme).clicked() {
        let target = target.clone();
        cx.edit(|project| project.set_automation(target, Vec::new()));
    }
}

/// What a control shows while an envelope drives it: the envelope's value
/// at the playhead, which is what is heard, and the hint that says so.
pub fn driven(cx: &Cx, target: &AutomationTarget) -> Option<f64> {
    cx.snap.automated(target, cx.live.position)
}

/// The hint for a control that an envelope drives.
pub const DRIVEN_HINT: &str = "Automated: its envelope sets this value \u{00B7} click to show the lane";

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
        assert!(!Scale::Volume.linear() && !Scale::Volume.stepped());

        assert_eq!(Scale::Pan.position(-1.0), 0.0);
        assert_eq!(Scale::Pan.position(0.5), 0.75);
        assert_eq!(Scale::Pan.value(0.75), 0.5);
        assert_eq!(Scale::Pan.value(0.505), 0.0, "close to the middle is the middle");
        assert_eq!(Scale::Pan.format(-0.5), "L50");
        assert_eq!(Scale::Pan.default_position(), 0.5);
        assert!(Scale::Pan.linear());
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
        assert!(!scale.linear() && !scale.stepped());
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

        let master = choices(&snapshot, LaneOwner::Master);
        assert_eq!(master[0].target, AutomationTarget::MasterVolume);
        assert!(master.iter().skip(1).all(|choice| choice.device.as_deref() == Some("Limiter")));
        assert!(!has_automation(&snapshot, LaneOwner::Track(track)));
    }
}
