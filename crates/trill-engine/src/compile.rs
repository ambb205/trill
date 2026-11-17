//! Compiles the project model into a render graph. Runs on the controller
//! side, never on the audio thread, so it is free to allocate and to load
//! plugins and audio files.

use crate::graph::{
    AudioRegion, Bus, Chain, DeviceSlot, Envelope, Graph, MAX_BLOCK, MasterNode, Meter, NoteEvent, ParamAutomation,
    RegionAudio, SendNode, TrackNode, TrackState,
};
use crate::ready::{LoadWatch, Plugins};
use crate::sources::AudioSources;
use parking_lot::Mutex;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};
use trill_core::device::{DeviceController, DeviceFactory, DeviceWarning, ParamInfo, Purpose};
use trill_core::project::{AutomationTarget, Clip, ClipContent};
use trill_core::{ClipId, Device, DeviceId, Project, Track, TrackId, TrackKind};
use trill_dsp::util::db_to_gain;

/// How far apart, in beats, the end of one clip and the start of the next
/// may be and still count as touching. Splitting a clip computes the two
/// from each other, which can leave them a rounding error apart.
const TOUCHING_BEATS: f64 = 1e-9;
/// How far apart, in frames, two clips' positions in the same audio may be
/// and still count as one continuing the other.
const CONTINUOUS_FRAMES: f64 = 1e-3;

/// How long a plugin that was created from a saved state is given to take
/// that state on before what it says its state is counts for more than
/// what it was given.
///
/// Some plugins take a state on with their first audio block, not when
/// they are handed it, and until then report the state they were born
/// with. Read back in that moment (a project opened and saved or rendered
/// at once), that would replace the project's state with a blank one.
/// Nothing a user does to a plugin can happen this soon after it is
/// created: its editor is not open yet.
const SETTLING: Duration = Duration::from_millis(500);

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
    /// Until when a plugin that was created from a saved state is taken
    /// to have that state, whatever it reports: see [`SETTLING`]. `None`
    /// for a device that was created from nothing.
    pub settling: Option<Instant>,
    /// Why the device could not be created, if it could not. Such a device
    /// stays in the project and passes audio through.
    pub error: Option<String>,
    /// What the factory found wrong with the device when it was created,
    /// short of failing: samples it plays without.
    pub warnings: Vec<DeviceWarning>,
    /// Whether the device can listen to a key: a track other than its own
    /// ([`trill_core::Device::sidechain`]).
    pub takes_key: bool,
}

/// The devices that exist in the running graph.
#[derive(Default)]
pub struct DeviceBank {
    pub devices: HashMap<DeviceId, DeviceMeta>,
    /// Which of the plugins among them were made lately and may still be
    /// loading what they play: told of every plugin that is made to be
    /// heard live.
    pub loads: Arc<LoadWatch>,
}

/// Track meters, kept across compiles so readers hold stable handles.
#[derive(Default)]
pub struct Meters {
    pub tracks: HashMap<TrackId, Arc<Meter>>,
    pub master: Arc<Meter>,
}

/// The clips of the running graph that play a stand-in while their
/// stretched audio is being made (see [`AudioSources`]). Kept across
/// compiles, like the device bank, so that the graph which finally gets the
/// real audio can fade over from what was playing.
#[derive(Default)]
pub struct StandIns {
    playing: HashMap<ClipId, RegionAudio>,
}

impl StandIns {
    /// How many clips are playing a stand-in.
    pub fn len(&self) -> usize {
        self.playing.len()
    }

    pub fn is_empty(&self) -> bool {
        self.playing.is_empty()
    }
}

/// Something that went wrong while compiling but did not stop it.
#[derive(Clone, Debug, PartialEq)]
pub enum Problem {
    /// The device could not be created; audio passes through where it sits.
    Device { device: DeviceId, reason: String },
    /// The device runs, without the sample in `slot` (or, with no slot,
    /// without any: its saved state is unreadable). `reason` names the slot.
    Sample { device: DeviceId, slot: Option<String>, reason: String },
    /// The clip's file cannot be read, so the clip is silent, or cannot be
    /// stretched, so it plays repitched. `reason` names the file.
    AudioFile { clip: ClipId, path: String, reason: String },
}

impl DeviceMeta {
    /// Whether the device is a plugin that was created from the state
    /// `held` so recently that it may not have taken it on yet.
    pub fn is_settling_into(&self, held: Option<&Vec<u8>>) -> bool {
        let fresh = self.settling.is_some_and(|until| Instant::now() < until);
        fresh && held.is_some() && held == self.applied_state.as_ref()
    }
}

impl std::fmt::Display for Problem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Problem::Device { device, reason } | Problem::Sample { device, reason, .. } => {
                write!(f, "{device}: {reason}")
            }
            Problem::AudioFile { clip, reason, .. } => write!(f, "{clip}: {reason}"),
        }
    }
}

pub struct Compiler<'a> {
    pub factory: &'a dyn DeviceFactory,
    pub bank: &'a mut DeviceBank,
    pub meters: &'a mut Meters,
    pub audio: &'a AudioSources,
    /// `Some` for a graph that will be heard live: clips whose stretched
    /// audio is not ready get stand-ins, recorded here, and the work is
    /// left to the background. `None` for a graph that is rendered: the
    /// compile waits until every clip has its real audio.
    pub stand_ins: Option<&'a mut StandIns>,
    pub sample_rate: f64,
    /// What the devices this compile creates are for: played or rendered.
    pub purpose: Purpose,
}

impl Compiler<'_> {
    /// Builds a graph for `project`. Devices already in the bank are left
    /// for the audio thread to carry over; new ones are created here.
    pub fn compile(&mut self, project: &Project, looping: bool) -> (Box<Graph>, Vec<Problem>) {
        let mut problems = Vec::new();
        let routing = Routing::of(project);
        let frames_per_beat = 60.0 * self.sample_rate / project.tempo;
        let live = self.stand_ins.is_some();
        // The stretched audio this graph plays or waits for, and the clips
        // that have a stand-in meanwhile.
        let mut wanted = HashSet::new();
        let mut waiting = HashMap::new();
        // The stretched copies this graph has given a stand-in for.
        let mut held = HashSet::new();

        // A render makes and plays only what is heard, and what feeds it:
        // a track that is muted, or left out by a solo, would be made,
        // waited for and run only to be thrown away. What is played keeps
        // every track running, so that it is there the moment it is
        // unmuted.
        let needed = (self.purpose == Purpose::Rendering).then(|| routing.needed(project));
        let mut tracks = Vec::with_capacity(project.tracks.len());
        for track in &project.tracks {
            if needed.as_ref().is_some_and(|needed| !needed.contains(&track.id)) {
                continue;
            }
            let mut notes = Vec::new();
            let mut audio = Vec::new();
            for clip in track.clips.iter().filter(|clip| !clip.muted) {
                let content = match &clip.content {
                    ClipContent::Midi(_) => {
                        flatten_notes(clip, &mut notes);
                        continue;
                    }
                    ClipContent::Audio(content) => content,
                };
                let report = |reason| Problem::AudioFile { clip: clip.id, path: content.path.clone(), reason };
                let found = match self.audio.clip_audio(content, project.tempo, self.sample_rate, !live, &mut held) {
                    Ok(found) => found,
                    Err(reason) => {
                        problems.push(report(reason));
                        continue;
                    }
                };
                problems.extend(found.problem.map(report));
                wanted.extend(found.key);
                let region_audio = RegionAudio {
                    origin: content.offset_seconds * found.frames_per_second
                        - clip.start * frames_per_beat * found.step,
                    step: found.step,
                    data: found.data,
                };
                let replaces = if found.stand_in {
                    waiting.insert(clip.id, region_audio.clone());
                    None
                } else {
                    self.stand_ins.as_ref().and_then(|stand_ins| stand_ins.playing.get(&clip.id).cloned())
                };
                audio.push(AudioRegion {
                    start: clip.start,
                    end: clip.end(),
                    fade_in: content.fade_in,
                    fade_out: content.fade_out,
                    joins_previous: false,
                    joins_next: false,
                    gain: db_to_gain(content.gain_db),
                    audio: region_audio,
                    replaces,
                    blended: 0,
                });
            }
            notes.sort_by(|a, b| a.start.total_cmp(&b.start));
            join_continuations(&mut audio);

            let meter = self.meters.tracks.entry(track.id).or_default().clone();
            let chain = self.chain(&track.devices, project, &routing.keys, &mut problems);
            tracks.push(TrackNode {
                id: track.id,
                notes,
                controllers: track.controller_lanes(),
                fresh_devices: chain.devices.iter().any(|slot| slot.processor.is_some()),
                audio,
                chain,
                volume_db: track.volume_db,
                pan: track.pan,
                audible: routing.audible(project, track),
                feeds_key: routing.feeds_key(project, track),
                volume_automation: envelope(
                    project,
                    |target| matches!(target, AutomationTarget::TrackVolume { track: id } if *id == track.id),
                ),
                pan_automation: envelope(
                    project,
                    |target| matches!(target, AutomationTarget::TrackPan { track: id } if *id == track.id),
                ),
                meter,
                state: TrackState::default(),
                input_bus: routing.bus(track.id),
                output_bus: track.group.and_then(|group| routing.bus(group)),
                sends: sends(project, track, &routing),
                key_out: routing.key(track.id),
                tap: None,
                source: None,
            });
        }
        // Every node must run before the bus it feeds is read, and before
        // the devices that listen to it.
        tracks.sort_by_key(|node| routing.place(node.id));

        let master = MasterNode {
            chain: self.chain(&project.master.devices, project, &routing.keys, &mut problems),
            volume_db: project.master.volume_db,
            volume_automation: envelope(project, |target| matches!(target, AutomationTarget::MasterVolume)),
            meter: self.meters.master.clone(),
            fader: Default::default(),
            tap: None,
        };

        let live_devices: HashSet<DeviceId> = project.devices().map(|device| device.id).collect();
        for gone in self.bank.devices.keys().filter(|id| !live_devices.contains(id)) {
            self.bank.loads.gone(*gone);
        }
        self.bank.devices.retain(|id, _| live_devices.contains(id));
        let live_tracks: HashSet<TrackId> = project.tracks.iter().map(|track| track.id).collect();
        self.meters.tracks.retain(|id, _| live_tracks.contains(id));
        if let Some(stand_ins) = self.stand_ins.as_deref_mut() {
            stand_ins.playing = waiting;
            self.audio.set_wanted(wanted);
        }

        let region = project.loop_region;
        let moves_params =
            !master.chain.automation.is_empty() || tracks.iter().any(|track| !track.chain.automation.is_empty());
        let graph = Graph {
            moves_params,
            tracks,
            buses: (0..routing.bus_count()).map(|_| Bus::new()).collect(),
            keys: routing.keys.iter().map(|_| Bus::new()).collect(),
            master,
            tempo: project.tempo,
            time_signature: project.time_signature,
            loop_region: (looping && region.enabled).then_some((region.start, region.end)),
        };
        (Box::new(graph), problems)
    }

    /// The devices of a chain, and what moves their parameters. `keys` are
    /// the tracks that devices listen to, in the order of the graph's keys.
    fn chain(&mut self, devices: &[Device], project: &Project, keys: &[TrackId], problems: &mut Vec<Problem>) -> Chain {
        let slots: Vec<DeviceSlot> = devices.iter().map(|device| self.slot(device, keys, problems)).collect();

        let mut automation = Vec::new();
        for lane in project.automation.iter().filter(|lane| lane.enabled) {
            let AutomationTarget::DeviceParam { device, param } = &lane.target else { continue };
            let Some(slot) = devices.iter().position(|d| d.id == *device) else { continue };
            let Some(meta) = self.bank.devices.get(device) else { continue };
            if let Some(index) = meta.params.iter().position(|info| info.id == *param) {
                automation.push(ParamAutomation { slot, param: index, envelope: Envelope::of(lane), last: f64::NAN });
            }
        }
        Chain { devices: slots, automation }
    }

    fn slot(&mut self, device: &Device, keys: &[TrackId], problems: &mut Vec<Problem>) -> DeviceSlot {
        let running = self.bank.devices.get(&device.id).is_some_and(|meta| {
            meta.uri == device.uri
                && meta.applied_state == device.state
                // A device that came up short is looked at again on every
                // compile. If what is wrong with it has changed (a missing
                // sample is back), it is created afresh to pick that up.
                && (meta.warnings.is_empty() || meta.warnings == self.factory.warnings(device))
        });
        let processor = if running {
            None
        } else {
            match self.factory.create_for(device, self.sample_rate, MAX_BLOCK, self.purpose) {
                Ok(instance) => {
                    if self.purpose == Purpose::Playing && Plugins::is_plugin(&device.uri) {
                        self.bank.loads.began(device.id);
                    }
                    self.bank.devices.insert(
                        device.id,
                        DeviceMeta {
                            uri: device.uri.clone(),
                            params: instance.params,
                            controller: instance.controller.map(Mutex::new),
                            applied_state: device.state.clone(),
                            settling: device.state.as_ref().map(|_| Instant::now() + SETTLING),
                            error: None,
                            warnings: self.factory.warnings(device),
                            takes_key: instance.processor.takes_key(),
                        },
                    );
                    Some(instance.processor)
                }
                Err(error) => {
                    self.bank.devices.insert(
                        device.id,
                        DeviceMeta {
                            uri: device.uri.clone(),
                            params: Vec::new(),
                            controller: None,
                            applied_state: device.state.clone(),
                            settling: None,
                            error: Some(error.to_string()),
                            warnings: Vec::new(),
                            takes_key: false,
                        },
                    );
                    None
                }
            }
        };
        // Reported on every compile, not only the one that found them: the
        // problems of a project are those of its current graph.
        if let Some(meta) = self.bank.devices.get(&device.id) {
            problems
                .extend(meta.error.iter().map(|reason| Problem::Device { device: device.id, reason: reason.clone() }));
            problems.extend(meta.warnings.iter().map(|warning| Problem::Sample {
                device: device.id,
                slot: warning.slot.clone(),
                reason: warning.message.clone(),
            }));
        }
        // A key only for a device that can listen to one, from a track
        // that is played before it.
        let listens = self.bank.devices.get(&device.id).is_some_and(|meta| meta.takes_key);
        let key = device.sidechain.filter(|_| listens).and_then(|track| keys.iter().position(|key| *key == track));
        DeviceSlot { id: device.id, processor, enabled: device.enabled, key }
    }
}

/// Finds the clips that carry straight on from another (the halves of a
/// split, typically) and joins them, so that together they play exactly
/// what one clip over the same span would: the same frames, with no fade
/// at the seam.
fn join_continuations(regions: &mut [AudioRegion]) {
    let mut ends: Vec<(f64, usize)> = regions.iter().enumerate().map(|(index, region)| (region.end, index)).collect();
    ends.sort_by(|a, b| a.0.total_cmp(&b.0));
    // Earliest first, so that a chain of clips all take up the first one's
    // position in the audio.
    let mut order: Vec<usize> = (0..regions.len()).collect();
    order.sort_by(|a, b| regions[*a].start.total_cmp(&regions[*b].start));

    for later in order {
        let start = regions[later].start;
        let first = ends.partition_point(|(end, _)| *end < start - TOUCHING_BEATS);
        let earlier = ends[first..].iter().take_while(|(end, _)| *end <= start + TOUCHING_BEATS).map(|(_, i)| *i).find(
            |&earlier| {
                let (a, b) = (&regions[earlier], &regions[later]);
                earlier != later
                    && a.start < b.start
                    && a.fade_out == 0.0
                    && b.fade_in == 0.0
                    && a.gain == b.gain
                    && a.audio.step == b.audio.step
                    && Arc::ptr_eq(&a.audio.data, &b.audio.data)
                    && (a.audio.origin - b.audio.origin).abs() <= CONTINUOUS_FRAMES
            },
        );
        if let Some(earlier) = earlier {
            let (end, origin) = (regions[earlier].end, regions[earlier].audio.origin);
            regions[earlier].joins_next = true;
            let region = &mut regions[later];
            region.joins_previous = true;
            // Exactly the same mapping and no gap or overlap, down to the
            // last bit: the two were computed separately and may differ by
            // a rounding error, which is enough to move a frame.
            region.start = end;
            region.audio.origin = origin;
        }
    }
}

/// Which tracks own a bus, which are listened to, and in what order tracks
/// must be processed.
struct Routing {
    /// Group and return tracks, in bus order.
    buses: Vec<TrackId>,
    /// The tracks that devices listen to, in key order.
    keys: Vec<TrackId>,
    /// The tracks in the order they are played.
    order: Vec<TrackId>,
    any_solo: bool,
}

impl Routing {
    fn of(project: &Project) -> Self {
        let buses =
            project.tracks.iter().filter(|track| track.kind != TrackKind::Normal).map(|track| track.id).collect();
        // The edits refuse a loop, but a file may hold anything: tracks
        // that wait for each other are played in the order of their kinds,
        // and nothing is listened to.
        let (order, sound) = match project.playing_order() {
            Some(order) => (order, true),
            None => {
                let mut without = project.clone();
                for track in &mut without.tracks {
                    track.devices.iter_mut().for_each(|device| device.sidechain = None);
                }
                (
                    without.playing_order().unwrap_or_else(|| project.tracks.iter().map(|track| track.id).collect()),
                    false,
                )
            }
        };
        let mut keys: Vec<TrackId> = Vec::new();
        if sound {
            for track in project.devices().filter_map(|device| device.sidechain) {
                if project.track(track).is_ok() && !keys.contains(&track) {
                    keys.push(track);
                }
            }
        }
        Self { buses, keys, order, any_solo: project.tracks.iter().any(|track| track.solo) }
    }

    /// The place of a track that is listened to among the graph's keys.
    fn key(&self, track: TrackId) -> Option<usize> {
        self.keys.iter().position(|id| *id == track)
    }

    /// Where a track comes in the order they are played in.
    fn place(&self, track: TrackId) -> usize {
        self.order.iter().position(|id| *id == track).unwrap_or(usize::MAX)
    }

    fn bus(&self, track: TrackId) -> Option<usize> {
        self.buses.iter().position(|id| *id == track)
    }

    fn bus_count(&self) -> usize {
        self.buses.len()
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

    /// The tracks a render has to make and play: the ones that are heard
    /// for their own sake, the groups and returns their sound goes on
    /// through, and the tracks their devices listen to. With nothing
    /// soloed that is every track that is not muted. With a solo it is the
    /// soloed tracks, the members of a soloed group, and of the groups and
    /// returns that are open only so that those are heard through them,
    /// the ones something needed feeds. A track that is listened to is
    /// made whether or not it is heard: muted, or left out by a solo.
    fn needed(&self, project: &Project) -> HashSet<TrackId> {
        let audible: HashSet<TrackId> =
            project.tracks.iter().filter(|track| self.audible(project, track)).map(|track| track.id).collect();
        let own = |track: &Track| !self.any_solo || track.solo || track.kind == TrackKind::Normal;
        let mut needed: HashSet<TrackId> = project
            .tracks
            .iter()
            .filter(|track| audible.contains(&track.id) && own(track))
            .map(|track| track.id)
            .collect();
        let feeds = feeds(project);
        loop {
            let fed = feeds.iter().filter(|(from, to)| needed.contains(from) && audible.contains(to));
            let more: Vec<TrackId> = fed.map(|(_, to)| *to).filter(|to| !needed.contains(to)).collect();
            if more.is_empty() {
                break;
            }
            needed.extend(more);
        }

        // What a track that is made listens to is made too, though nobody
        // hears it: a compressor ducks to its key whether or not the key is
        // in the mix. That is the track itself (it is heard ahead of its
        // fader and its mute), whatever sounds into it if it is a group or
        // a return, and what the devices of all of those listen to in turn.
        // The master is always made.
        let keys_of = |devices: &[Device]| devices.iter().filter_map(|device| device.sidechain).collect::<Vec<_>>();
        let sounds_into = |from: &Track, to: &Track| match to.kind {
            TrackKind::Group => {
                from.group == Some(to.id) && (audible.contains(&from.id) || self.feeds_key(project, from))
            }
            TrackKind::Return => audible.contains(&from.id) && sends_to(project, from, to.id),
            TrackKind::Normal => false,
        };
        let made = project.tracks.iter().filter(|track| needed.contains(&track.id));
        let mut listened: Vec<TrackId> = made.flat_map(|track| keys_of(&track.devices)).collect();
        listened.extend(keys_of(&project.master.devices));
        while let Some(key) = listened.pop() {
            let Ok(track) = project.track(key) else { continue };
            if !needed.insert(key) {
                continue;
            }
            listened.extend(keys_of(&track.devices));
            listened.extend(project.tracks.iter().filter(|from| sounds_into(from, track)).map(|from| from.id));
        }
        needed
    }
}

/// Whose sound goes into whom: a member into its group, a track into each
/// return it sends to, by a send set by hand or by one that is its envelope.
/// (A device that listens to another track is fed by it too, the other way
/// round: [`Routing::needed`] follows those from the tracks that are made
/// to the ones they listen to.)
fn feeds(project: &Project) -> Vec<(TrackId, TrackId)> {
    let mut feeds = Vec::new();
    for track in &project.tracks {
        feeds.extend(track.group.map(|group| (track.id, group)));
        feeds.extend(track.sends.iter().map(|send| (track.id, send.to)));
    }
    feeds.extend(project.automation.iter().filter(|lane| lane.enabled).filter_map(|lane| match lane.target {
        AutomationTarget::Send { track, to } => Some((track, to)),
        _ => None,
    }));
    feeds
}

/// Whether `from` sends to the return `to`: by a send set by hand, or by
/// one that is its envelope.
fn sends_to(project: &Project, from: &Track, to: TrackId) -> bool {
    let moved = AutomationTarget::Send { track: from.id, to };
    from.sends.iter().any(|send| send.to == to)
        || project.automation.iter().any(|lane| lane.enabled && lane.target == moved)
}

impl Routing {
    /// Whether a track that is not heard still plays into its group: see
    /// [`TrackNode::feeds_key`].
    fn feeds_key(&self, project: &Project, track: &Track) -> bool {
        let listened = |group: &Track| self.key(group.id).is_some() && !self.audible(project, group);
        !track.mute
            && !self.audible(project, track)
            && track.group.is_some_and(|group| project.track(group).is_ok_and(listened))
    }
}

/// The envelope that moves what `matches` picks out, if it has one.
fn envelope(project: &Project, matches: impl Fn(&AutomationTarget) -> bool) -> Option<Envelope> {
    // A lane that is switched off is no lane to the engine.
    project.automation.iter().find(|lane| lane.enabled && matches(&lane.target)).map(Envelope::of)
}

/// What `track` sends to the returns: the sends set by hand, each moved by
/// its envelope if it has one, and the sends that are an envelope alone.
fn sends(project: &Project, track: &Track, routing: &Routing) -> Vec<SendNode> {
    let node = |to: TrackId, gain: f32, automation: Option<Envelope>| {
        Some(SendNode { bus: routing.bus(to)?, to, gain, automation, glide: Default::default() })
    };
    let moved = |to: TrackId| {
        envelope(
            project,
            |target| matches!(target, AutomationTarget::Send { track: from, to: at } if *from == track.id && *at == to),
        )
    };
    let by_hand = track.sends.iter().filter_map(|send| node(send.to, db_to_gain(send.level_db), moved(send.to)));
    let by_envelope = project.automation.iter().filter(|lane| lane.enabled).filter_map(|lane| match lane.target {
        AutomationTarget::Send { track: from, to }
            if from == track.id && !track.sends.iter().any(|send| send.to == to) =>
        {
            node(to, 0.0, Some(Envelope::of(lane)))
        }
        _ => None,
    });
    by_hand.chain(by_envelope).collect()
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
