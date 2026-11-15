//! Compiles the project model into a render graph. Runs on the controller
//! side, never on the audio thread, so it is free to allocate and to load
//! plugins and audio files.

use crate::audio_file::AudioCache;
use crate::graph::{
    AudioRegion, Bus, Chain, DeviceSlot, Graph, MAX_BLOCK, MasterNode, Meter, NoteEvent, ParamAutomation, TrackNode,
    TrackState,
};
use parking_lot::Mutex;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use trill_core::device::{DeviceController, DeviceFactory, ParamInfo};
use trill_core::project::{AutomationTarget, Clip, ClipContent};
use trill_core::{Device, DeviceId, Project, Track, TrackId, TrackKind};
use trill_dsp::util::db_to_gain;

/// What the controller knows about a device whose processor lives on the
/// audio thread.
pub struct DeviceMeta {
    pub uri: String,
    pub params: Vec<ParamInfo>,
    /// Behind a lock so that reading a plugin's values needs only shared
    /// access to the bank.
    pub controller: Option<Mutex<Box<dyn DeviceController>>>,
    /// The saved state the running instance reflects: what it was created
    /// from, or what was last captured from it. When the model asks for a
    /// different state (a sampler given another file, an undo), the device
    /// is created again.
    pub applied_state: Option<Vec<u8>>,
    /// Why the device could not be created, if it could not. Such a device
    /// stays in the project and passes audio through.
    pub error: Option<String>,
}

/// The devices that exist in the running graph.
#[derive(Default)]
pub struct DeviceBank {
    pub devices: HashMap<DeviceId, DeviceMeta>,
}

/// Track meters, kept across compiles so readers hold stable handles.
#[derive(Default)]
pub struct Meters {
    pub tracks: HashMap<TrackId, Arc<Meter>>,
    pub master: Arc<Meter>,
}

/// Something that went wrong while compiling but did not stop it.
#[derive(Clone, Debug, PartialEq)]
pub enum Problem {
    Device { device: DeviceId, reason: String },
    AudioFile { path: String, reason: String },
}

impl std::fmt::Display for Problem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Problem::Device { device, reason } => write!(f, "{device}: {reason}"),
            Problem::AudioFile { path, reason } => write!(f, "audio file '{path}': {reason}"),
        }
    }
}

pub struct Compiler<'a> {
    pub factory: &'a dyn DeviceFactory,
    pub bank: &'a mut DeviceBank,
    pub meters: &'a mut Meters,
    pub audio: &'a mut AudioCache,
    pub sample_rate: f64,
}

impl Compiler<'_> {
    /// Builds a graph for `project`. Devices already in the bank are left
    /// for the audio thread to carry over; new ones are created here.
    pub fn compile(&mut self, project: &Project, looping: bool) -> (Box<Graph>, Vec<Problem>) {
        let mut problems = Vec::new();
        let routing = Routing::of(project);

        let mut tracks = Vec::with_capacity(project.tracks.len());
        for track in &project.tracks {
            let mut notes = Vec::new();
            let mut audio = Vec::new();
            for clip in track.clips.iter().filter(|clip| !clip.muted) {
                match &clip.content {
                    ClipContent::Midi(_) => flatten_notes(clip, &mut notes),
                    ClipContent::Audio(content) => match self.audio.load(&content.path) {
                        Ok(data) => audio.push(AudioRegion {
                            start: clip.start,
                            end: clip.end(),
                            offset_frames: content.offset_seconds * data.sample_rate,
                            fade_in: content.fade_in,
                            fade_out: content.fade_out,
                            gain: db_to_gain(content.gain_db),
                            data,
                        }),
                        Err(reason) => problems.push(Problem::AudioFile { path: content.path.clone(), reason }),
                    },
                }
            }
            notes.sort_by(|a, b| a.start.total_cmp(&b.start));

            let meter = self.meters.tracks.entry(track.id).or_default().clone();
            tracks.push(TrackNode {
                id: track.id,
                notes,
                audio,
                chain: self.chain(&track.devices, project, &mut problems),
                volume_db: track.volume_db,
                pan: track.pan,
                audible: routing.audible(project, track),
                volume_automation: lane_points(
                    project,
                    |target| matches!(target, AutomationTarget::TrackVolume { track: id } if *id == track.id),
                ),
                pan_automation: lane_points(
                    project,
                    |target| matches!(target, AutomationTarget::TrackPan { track: id } if *id == track.id),
                ),
                meter,
                state: TrackState::default(),
                input_bus: routing.bus(track.id),
                output_bus: track.group.and_then(|group| routing.bus(group)),
                sends: track
                    .sends
                    .iter()
                    .filter_map(|send| Some((routing.bus(send.to)?, db_to_gain(send.level_db))))
                    .collect(),
            });
        }
        // Every node must run before the bus it feeds is read.
        tracks.sort_by_key(|node| routing.stage(node.id));

        let master = MasterNode {
            chain: self.chain(&project.master.devices, project, &mut problems),
            volume_db: project.master.volume_db,
            volume_automation: lane_points(project, |target| matches!(target, AutomationTarget::MasterVolume)),
            meter: self.meters.master.clone(),
            gain: 1.0,
            primed: false,
        };

        let live_devices: HashSet<DeviceId> = project.devices().map(|device| device.id).collect();
        self.bank.devices.retain(|id, _| live_devices.contains(id));
        let live_tracks: HashSet<TrackId> = project.tracks.iter().map(|track| track.id).collect();
        self.meters.tracks.retain(|id, _| live_tracks.contains(id));

        let region = project.loop_region;
        let graph = Graph {
            tracks,
            buses: (0..routing.bus_count()).map(|_| Bus::new()).collect(),
            master,
            tempo: project.tempo,
            time_signature: project.time_signature,
            loop_region: (looping && region.enabled).then_some((region.start, region.end)),
        };
        (Box::new(graph), problems)
    }

    fn chain(&mut self, devices: &[Device], project: &Project, problems: &mut Vec<Problem>) -> Chain {
        let slots: Vec<DeviceSlot> = devices.iter().map(|device| self.slot(device, problems)).collect();

        let mut automation = Vec::new();
        for lane in &project.automation {
            let AutomationTarget::DeviceParam { device, param } = &lane.target else { continue };
            let Some(slot) = devices.iter().position(|d| d.id == *device) else { continue };
            let Some(meta) = self.bank.devices.get(device) else { continue };
            if let Some(index) = meta.params.iter().position(|info| info.id == *param) {
                automation.push(ParamAutomation { slot, param: index, points: lane.points.clone(), last: f64::NAN });
            }
        }
        Chain { devices: slots, automation }
    }

    fn slot(&mut self, device: &Device, problems: &mut Vec<Problem>) -> DeviceSlot {
        let running = self
            .bank
            .devices
            .get(&device.id)
            .is_some_and(|meta| meta.uri == device.uri && meta.applied_state == device.state);
        let processor = if running {
            None
        } else {
            match self.factory.create(device, self.sample_rate, MAX_BLOCK) {
                Ok(instance) => {
                    self.bank.devices.insert(
                        device.id,
                        DeviceMeta {
                            uri: device.uri.clone(),
                            params: instance.params,
                            controller: instance.controller.map(Mutex::new),
                            applied_state: device.state.clone(),
                            error: None,
                        },
                    );
                    Some(instance.processor)
                }
                Err(error) => {
                    let reason = error.to_string();
                    problems.push(Problem::Device { device: device.id, reason: reason.clone() });
                    self.bank.devices.insert(
                        device.id,
                        DeviceMeta {
                            uri: device.uri.clone(),
                            params: Vec::new(),
                            controller: None,
                            applied_state: device.state.clone(),
                            error: Some(reason),
                        },
                    );
                    None
                }
            }
        };
        DeviceSlot { id: device.id, processor, enabled: device.enabled }
    }
}

/// Which tracks own a bus, and in what order tracks must be processed.
struct Routing {
    /// Group and return tracks, in bus order.
    buses: Vec<TrackId>,
    stages: HashMap<TrackId, u8>,
    any_solo: bool,
}

impl Routing {
    fn of(project: &Project) -> Self {
        let buses =
            project.tracks.iter().filter(|track| track.kind != TrackKind::Normal).map(|track| track.id).collect();
        let stages = project
            .tracks
            .iter()
            .map(|track| {
                let stage = match track.kind {
                    TrackKind::Normal => 0,
                    TrackKind::Group => 1,
                    TrackKind::Return => 2,
                };
                (track.id, stage)
            })
            .collect();
        Self { buses, stages, any_solo: project.tracks.iter().any(|track| track.solo) }
    }

    fn bus(&self, track: TrackId) -> Option<usize> {
        self.buses.iter().position(|id| *id == track)
    }

    fn bus_count(&self) -> usize {
        self.buses.len()
    }

    fn stage(&self, track: TrackId) -> u8 {
        self.stages.get(&track).copied().unwrap_or(0)
    }

    /// Mute and solo, resolved. Soloing a track keeps its group open, and
    /// soloing a group keeps its members; returns stay open so a soloed
    /// track is heard with its reverb.
    fn audible(&self, project: &Project, track: &Track) -> bool {
        if track.mute {
            return false;
        }
        if !self.any_solo || track.solo {
            return true;
        }
        match track.kind {
            TrackKind::Return => true,
            TrackKind::Group => project.tracks.iter().any(|member| member.group == Some(track.id) && member.solo),
            TrackKind::Normal => track.group.is_some_and(|group| project.track(group).is_ok_and(|group| group.solo)),
        }
    }
}

fn lane_points(
    project: &Project,
    matches: impl Fn(&AutomationTarget) -> bool,
) -> Option<Vec<trill_core::AutomationPoint>> {
    project.automation.iter().find(|lane| matches(&lane.target)).map(|lane| lane.points.clone())
}

/// A clip's notes as the engine plays them: absolute positions, with the
/// offset, loop and trimming already resolved by the model.
fn flatten_notes(clip: &Clip, out: &mut Vec<NoteEvent>) {
    out.extend(clip.timeline_notes().into_iter().map(|note| NoteEvent {
        start: note.start,
        end: note.end(),
        pitch: note.pitch,
        velocity: note.velocity,
    }));
}

#[cfg(test)]
mod tests {
    use super::*;
    use trill_core::{Note, Project};

    fn starts(notes: &[NoteEvent]) -> Vec<f64> {
        notes.iter().map(|n| n.start).collect()
    }

    #[test]
    fn notes_are_offset_trimmed_and_looped() {
        let mut project = Project::new("t");
        let track = project.add_track("A");
        let clip = project.add_midi_clip(track, "c", 8.0, 5.0).unwrap();
        let c = project.clip_mut(clip).unwrap();
        c.add_notes(&[
            Note { pitch: 60, start: 0.0, length: 1.0, velocity: 100 },
            Note { pitch: 62, start: 1.5, length: 4.0, velocity: 100 },
            Note { pitch: 64, start: 6.0, length: 1.0, velocity: 100 },
        ])
        .unwrap();

        let clip_ref = project.clip(clip).unwrap();
        let mut plain = Vec::new();
        flatten_notes(clip_ref, &mut plain);
        assert_eq!(starts(&plain), [8.0, 9.5], "the note past the clip end is dropped");
        assert_eq!(plain[1].end, 13.0, "a note hanging over the clip end is trimmed");

        project.clip_mut(clip).unwrap().set_loop_length(Some(2.0)).unwrap();
        let clip_ref = project.clip(clip).unwrap();
        let mut looped = Vec::new();
        flatten_notes(clip_ref, &mut looped);
        assert_eq!(starts(&looped), [8.0, 9.5, 10.0, 11.5, 12.0]);
        assert_eq!(looped[1].end, 10.0, "notes are cut at the loop boundary");
        assert_eq!(looped[4].end, 13.0);

        // Entering the loop half a beat in shifts every repeat earlier and
        // drops the note that would have started before the clip.
        project.clip_mut(clip).unwrap().content =
            trill_core::project::ClipContent::Midi(trill_core::project::MidiContent {
                start_offset: 0.5,
                ..project.clip(clip).unwrap().midi().unwrap().clone()
            });
        let clip_ref = project.clip(clip).unwrap();
        let mut offset = Vec::new();
        flatten_notes(clip_ref, &mut offset);
        assert_eq!(starts(&offset), [9.0, 9.5, 11.0, 11.5]);
        assert_eq!(offset[0].end, 9.5, "the long note is cut where its loop pass ends");
        assert_eq!(offset[1].end, 10.5);
    }
}
