//! Compiles the project model into a render graph. Runs on the controller
//! side, never on the audio thread, so it is free to allocate and to load
//! plugins and audio files.

use crate::graph::{
    AudioRegion, Bus, Chain, DeviceSlot, Envelope, Graph, MAX_BLOCK, MasterNode, Meter, NoteEvent, ParamAutomation,
    RegionAudio, SendNode, TrackNode, TrackState,
};
use crate::processor::CLIP_FADE_SECONDS;
use crate::ready::{LoadWatch, Plugins};
use crate::sources::AudioSources;
use parking_lot::Mutex;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};
use trill_core::device::{
    DeviceController, DeviceError, DeviceFactory, DeviceInstance, DeviceWarning, ParamInfo, Purpose,
};
use trill_core::project::{AudioContent, AutomationTarget, Clip, ClipContent};
use trill_core::{ClipId, Device, DeviceId, Project, TempoMap, Track, TrackId, TrackKind};
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
    /// For a device that is made for a key or for none
    /// ([`trill_core::device::DeviceProcessor::key_is_set_when_made`]):
    /// which it was made for. When the model then has it the other way,
    /// the device is created again.
    pub made_keyed: Option<bool>,
    /// The device is not there yet: it is being made away from here, for a
    /// song that was opened a moment ago ([`DeviceBank::later`]). Its place
    /// in the graph is empty until it arrives, as that of a device that
    /// could not be made is for good: an instrument is silent, and an
    /// effect lets what comes into it through.
    pub coming: bool,
    /// What a plugin last said its parameter values are. Asking takes the
    /// thread plugins are made on, so while one is being made nobody asks:
    /// whoever wants the values meanwhile has these.
    pub reported: Mutex<Option<Vec<f64>>>,
}

/// The devices that exist in the running graph.
#[derive(Default)]
pub struct DeviceBank {
    pub devices: HashMap<DeviceId, DeviceMeta>,
    /// Which of the plugins among them were made lately and may still be
    /// loading what they play: told of every plugin that is made to be
    /// heard live.
    pub loads: Arc<LoadWatch>,
    /// Set by a session that plays: a device that takes time to make
    /// ([`DeviceFactory::takes_time`]) is not made where the compiler
    /// stands. Its place is kept ([`DeviceMeta::coming`]), and whoever set
    /// this has it made elsewhere and hands it in ([`DeviceBank::arrived`]).
    pub later: bool,
    /// What the compiles since it was last emptied let go of that has a
    /// plugin behind it: the records of devices that left the song or were
    /// made anew. Letting go of one may be waited for, so whoever compiles
    /// with the session held takes them from here and frees them
    /// elsewhere.
    pub gone: Vec<DeviceMeta>,
    /// Devices that were made elsewhere, or could not be: the next compile
    /// puts each in its place. Only for a device whose place is kept, and
    /// that is still the device it was made as: whoever puts one here has
    /// seen to that.
    pub arrived: HashMap<DeviceId, Result<DeviceInstance, DeviceError>>,
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
    playing: HashMap<(ClipId, u32), RegionAudio>,
}

impl StandIns {
    /// How many clips are playing a stand-in.
    pub fn len(&self) -> usize {
        self.playing.keys().map(|(clip, _)| clip).collect::<HashSet<_>>().len()
    }

    pub fn is_empty(&self) -> bool {
        self.playing.is_empty()
    }
}

/// Something that went wrong while compiling but did not stop it.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
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

/// Whether `device` is a plugin that loaded a preset itself, in a program
/// that has not read its state back yet: the song names the preset and
/// holds neither a state nor anything for the plugin to start from. In
/// the program it happened in the running plugin is what the song means.
/// In another one that holds the song the state follows, a moment later:
/// what is running there is neither made again from nothing meanwhile nor
/// set to the values the preset took away.
pub(crate) fn awaits_state(factory: &dyn DeviceFactory, device: &Device) -> bool {
    device.state.is_none() && device.preset.is_some() && device.from_preset.is_none() && factory.takes_time(&device.uri)
}

impl DeviceMeta {
    /// What is known of `device` while it is being made elsewhere: what
    /// the model says of it, and nothing of its own yet.
    fn kept_for(device: &Device) -> Self {
        Self {
            uri: device.uri.clone(),
            params: Vec::new(),
            controller: None,
            applied_state: device.state.clone(),
            settling: None,
            error: None,
            warnings: Vec::new(),
            takes_key: false,
            made_keyed: None,
            coming: true,
            reported: Mutex::new(None),
        }
    }

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

/// The log target under which what took time is said: see
/// `session/opening.rs`, and the plugin host's `making`.
const MAKING: &str = "trill::making";

/// Says, for whoever measures what opening a song takes, that something
/// the compile waited for since `asked` took long enough to count.
fn said_if_slow(asked: Instant, what: impl FnOnce() -> String) {
    let took = asked.elapsed();
    if took >= Duration::from_millis(2) {
        log::debug!(target: MAKING, "{} after {:.0} ms", what(), took.as_secs_f64() * 1e3);
    }
}

impl Compiler<'_> {
    /// Builds a graph for `project`. Devices already in the bank are left
    /// for the audio thread to carry over; new ones are created here.
    pub fn compile(&mut self, project: &Project, looping: bool) -> (Box<Graph>, Vec<Problem>) {
        // A reference is a track with a rule of its own for when it is
        // heard: it comes to a mute and a solo here, and nothing further
        // on knows that there are references.
        let began = Instant::now();
        let heard = project.as_heard(self.purpose == Purpose::Rendering);
        let project: &Project = &heard;
        let mut problems = Vec::new();
        let routing = Routing::of(project);
        let tempo_map = project.tempo_map();
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
                // A clip that follows the tempo is a piece for each tempo
                // it lies under; any other is one piece, at the speed it
                // was recorded at.
                let pieces = pieces(content, clip, &tempo_map);
                let last = pieces.len() - 1;
                let mut said = false;
                for (index, piece) in pieces.iter().enumerate() {
                    let asked = Instant::now();
                    let found = self.audio.clip_audio(content, piece.tempo, self.sample_rate, !live, &mut held);
                    said_if_slow(asked, || format!("the audio of '{}' was on hand", content.path));
                    let found = match found {
                        Ok(found) => found,
                        Err(reason) => {
                            problems.push(report(reason));
                            break;
                        }
                    };
                    // One sentence for a clip, however many pieces it is.
                    problems.extend(found.problem.filter(|_| !std::mem::replace(&mut said, true)).map(report));
                    wanted.extend(found.key);
                    // Where in the recording the piece begins, and from
                    // there where beat 0 of the song would fall in it.
                    let within = piece.within;
                    let region_audio = RegionAudio {
                        origin: within * found.frames_per_second
                            - tempo_map.frames_at(piece.start, self.sample_rate) * found.step,
                        step: found.step,
                        data: found.data,
                    };
                    let place = (clip.id, index as u32);
                    let replaces = if found.stand_in {
                        waiting.insert(place, region_audio.clone());
                        None
                    } else {
                        self.stand_ins.as_ref().and_then(|stand_ins| stand_ins.playing.get(&place).cloned())
                    };
                    // At a seam each piece is played half a click guard
                    // into the other's time, at the tempo that has there.
                    let (seam_before, seam_after) = (index > 0, index < last);
                    let before = if seam_before { seam_beats(pieces[index - 1].tempo) } else { 0.0 };
                    let after = if seam_after { seam_beats(pieces[index + 1].tempo) } else { 0.0 };
                    audio.push(AudioRegion {
                        start: piece.start - before,
                        end: piece.end + after,
                        clip_start: clip.start,
                        clip_end: clip.end(),
                        seam_before,
                        seam_after,
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
            }
            notes.sort_by(|a, b| a.start.total_cmp(&b.start));
            join_continuations(&mut audio);

            let meter = self.meters.tracks.entry(track.id).or_default().clone();
            let chain = self.chain(&track.devices, project, &routing.keys, &mut problems);
            let switches = crate::graph::switches_of(&notes, &track.profile);
            let (switch_keys, stay_keys) = crate::graph::switch_keys(&switches);
            let unsaid = track.profile.articulations.iter().find(|way| way.default && way.key.is_some());
            tracks.push(TrackNode {
                id: track.id,
                switches,
                switch_keys,
                stay_keys,
                unsaid_key: unsaid.and_then(|way| way.key),
                notes,
                controllers: track.controller_lanes(),
                fresh_devices: chain.devices.iter().any(|slot| slot.processor.is_some()),
                audio,
                chain,
                volume_db: track.volume_db,
                pan: track.pan,
                heard: routing.heard(project, track),
                passes: routing.passes(project, track),
                to_output: routing.to_output(project, track),
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
        let gone: Vec<DeviceId> = self.bank.devices.keys().filter(|id| !live_devices.contains(id)).copied().collect();
        for gone in gone {
            self.bank.loads.gone(gone);
            let record = self.bank.devices.remove(&gone);
            self.let_go_of(record);
        }
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
            tempo_map,
            meter_map: project.meter_map(),
            loop_region: (looping && region.enabled).then_some((region.start, region.end)),
            rounds: None,
            another_song: false,
        };
        log::debug!(
            target: MAKING,
            "the song was compiled in {:.0} ms: {} tracks",
            began.elapsed().as_secs_f64() * 1e3,
            graph.tracks.len()
        );
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

    /// Takes a device that was made, or the reason it could not be, into
    /// the bank as `device`, and returns what goes into the graph for it.
    fn install(
        &mut self,
        device: &Device,
        made: Result<DeviceInstance, DeviceError>,
    ) -> Option<Box<dyn trill_core::device::DeviceProcessor>> {
        match made {
            Ok(instance) => {
                if self.purpose == Purpose::Playing && Plugins::is_plugin(&device.uri) {
                    self.bank.loads.began(device.id);
                }
                let before = self.bank.devices.insert(
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
                        made_keyed: instance.processor.key_is_set_when_made().then_some(device.sidechain.is_some()),
                        coming: false,
                        reported: Mutex::new(None),
                    },
                );
                self.let_go_of(before);
                Some(instance.processor)
            }
            Err(error) => {
                let before = self.bank.devices.insert(
                    device.id,
                    DeviceMeta { error: Some(error.to_string()), coming: false, ..DeviceMeta::kept_for(device) },
                );
                self.let_go_of(before);
                None
            }
        }
    }

    fn slot(&mut self, device: &Device, keys: &[TrackId], problems: &mut Vec<Problem>) -> DeviceSlot {
        // A plugin whose state is on its way from the program it loaded
        // a preset in goes on as it is until the state is there.
        let awaited = awaits_state(self.factory, device);
        let running = self.bank.devices.get(&device.id).is_some_and(|meta| {
            meta.uri == device.uri
                && (meta.applied_state == device.state || (awaited && meta.applied_state.is_some()))
                && meta.made_keyed.is_none_or(|keyed| keyed == device.sidechain.is_some())
                // A device that came up short is looked at again on every
                // compile. If what is wrong with it has changed (a missing
                // sample is back), it is created afresh to pick that up.
                && (meta.warnings.is_empty() || meta.warnings == self.factory.warnings(device))
        });
        let processor = if let Some(made) = self.bank.arrived.remove(&device.id) {
            // Made away from here while its place was kept.
            self.install(device, made)
        } else if running {
            None
        } else if self.bank.later && self.purpose == Purpose::Playing && self.factory.takes_time(&device.uri) {
            // The song is there first, and this comes after: its place is
            // kept as one that is like the model says, and has nothing in
            // it. (Or what was running in it until now, where the device
            // is made anew: the graph takes that over until its successor
            // arrives.)
            let before = self.bank.devices.insert(device.id, DeviceMeta::kept_for(device));
            self.let_go_of(before);
            None
        } else {
            // A render in a process of its own says which device it is
            // making: a plugin that takes the process down as it is made
            // is then known by name.
            crate::apart::stage(crate::apart::Stage::Making(device.id));
            let asked = Instant::now();
            let made = make(self.factory, device, self.sample_rate, self.purpose);
            said_if_slow(asked, || format!("{} ({}) was made, where the song stands", device.name, device.uri));
            self.install(device, made)
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
        let slow = self.factory.takes_time(&device.uri);
        DeviceSlot { id: device.id, processor, enabled: device.enabled, key, turn: None, slow }
    }
}

/// How a device that was just given the preset it is to start from is run
/// before it is handed on: so many blocks of so many frames of silence.
const SETTLING_BLOCKS: usize = 2;
const SETTLING_FRAMES: usize = 64;

/// Makes `device` as the song has it: by the factory, and then, where the
/// song says that it is to start from a preset of its own which only the
/// device can load ([`Device::from_preset`]), with that preset loaded and
/// the values the song has set since on top of it. Whoever makes a device
/// from the song makes it through here, so that the device a render plays
/// and the one that arrives in the session are the same.
pub(crate) fn make(
    factory: &dyn DeviceFactory,
    device: &Device,
    sample_rate: f64,
    purpose: Purpose,
) -> Result<DeviceInstance, DeviceError> {
    let mut instance = factory.create_for(device, sample_rate, MAX_BLOCK, purpose)?;
    // A device with a state is that state: the preset is in it.
    let Some(preset) = device.from_preset.as_ref().filter(|_| device.state.is_none()) else { return Ok(instance) };
    let loaded = match instance.controller.as_mut() {
        Some(controller) => controller.load_preset(preset).map_err(|error| error.to_string()),
        None => Err("it has no presets of its own to load".to_string()),
    };
    if let Err(reason) = loaded {
        let reason = format!("it did not load the preset '{}' it was to start from: {reason}", preset.name);
        return Err(DeviceError::LoadFailed { uri: device.uri.clone(), reason });
    }
    // A plugin may say yes to a preset and take it on with its next
    // block. So it is run for a moment, on silence, as it is when it is
    // given a preset while it plays: what is set on it after this goes on
    // top of the preset, and what it is asked it answers as the preset.
    let stopped = trill_core::device::ProcessInfo {
        sample_rate,
        tempo: 120.0,
        time_signature: Default::default(),
        playing: false,
        position_beats: 0.0,
        song: Default::default(),
        loop_range: None,
    };
    let (mut left, mut right) = ([0.0f32; SETTLING_FRAMES], [0.0f32; SETTLING_FRAMES]);
    for _ in 0..SETTLING_BLOCKS {
        left.fill(0.0);
        right.fill(0.0);
        instance.processor.process(&stopped, &[], &mut left, &mut right);
    }
    for (index, info) in instance.params.iter().enumerate() {
        if let Some(value) = device.params.get(&info.id) {
            instance.processor.set_param(index, *value);
        }
    }
    Ok(instance)
}

impl Compiler<'_> {
    /// Lets go of the record of a device that has left the song, or was
    /// made anew. Where the device is one that takes time and was there,
    /// its record has a plugin behind it and is kept to be freed away
    /// from the compile; any other goes here and now, as it always did.
    fn let_go_of(&mut self, record: Option<DeviceMeta>) {
        let behind = |record: &DeviceMeta| record.controller.is_some() && self.factory.takes_time(&record.uri);
        if let Some(record) = record.filter(behind) {
            self.bank.gone.push(record);
        }
    }
}

/// A stretch of a clip of audio under one tempo, and in one round of its
/// loop where it plays round.
struct Piece {
    start: f64,
    end: f64,
    /// The tempo the stretch is played at: the song's there, or for a
    /// stretch of a tempo that glides, the one tempo that takes as long.
    tempo: f64,
    /// Where in the recording it begins, in seconds.
    within: f64,
}

/// How far a clip that follows a gliding tempo may be from its beat. It
/// is played at one speed for a stretch of the glide, on time at both ends
/// of the stretch and off by the most half way through it; a stretch is as
/// long as keeps that under this many seconds, between
/// [`GLIDE_PIECE_BEATS`] bounds. Each stretch is a copy of the file
/// stretched for its tempo, so a steep glide under a long file costs time
/// and memory.
const GLIDE_DRIFT_SECONDS: f64 = 0.0015;
/// The shortest and the longest stretch of a glide played at one speed.
const GLIDE_PIECE_BEATS: (f64, f64) = (0.125, 1.0);

/// How many beats from `beat` on, where the tempo glides, a clip that
/// follows it is played at one speed.
fn glide_piece_beats(map: &TempoMap, beat: f64) -> f64 {
    let (shortest, longest) = GLIDE_PIECE_BEATS;
    let tempo = map.tempo_at(beat);
    // How much of itself the tempo gains or loses in a beat. Half way
    // through a stretch of `l` beats the clip is `rate * l * l / 8` beats
    // from where the glide has the song.
    let rate = ((map.tempo_at(beat + 1e-3) - tempo) / (1e-3 * tempo)).abs();
    if rate <= 0.0 {
        return longest;
    }
    (GLIDE_DRIFT_SECONDS * 8.0 * tempo / (60.0 * rate)).sqrt().clamp(shortest, longest)
}

/// How far on either side of a seam between two pieces of a clip both are
/// played, in beats at `tempo`: half the click guard, so that the fade
/// across the seam is as long as the guard is.
fn seam_beats(tempo: f64) -> f64 {
    0.5 * CLIP_FADE_SECONDS * tempo / 60.0
}

/// The pieces a clip of audio is played in: one, unless it follows the
/// tempo and the tempo changes under it.
fn pieces(content: &AudioContent, clip: &Clip, map: &TempoMap) -> Vec<Piece> {
    let (start, end) = (clip.start, clip.end());
    // Where in the recording a stretch that begins at `beat` begins: for a clip that does not play round,
    // its offset and what the beats since cover.
    let within = |beat: f64| content.offset_seconds + content.seconds_over(map, start, beat - start);
    let follows = content.followed_tempo().is_some();
    let steady = map.next_change(start).is_none_or(|change| change >= end) && !map.is_ramping_at(start);
    let mut pieces = Vec::new();
    if !follows || steady {
        // Read at the tempo where it starts, which for a clip that does
        // not follow the tempo decides nothing.
        pieces.push(Piece { start, end, tempo: map.tempo_at(start), within: within(start) });
    } else {
        let mut from = start;
        while from < end {
            let change = map.next_change(from).unwrap_or(f64::INFINITY);
            let gliding = map.is_ramping_at(from);
            let to = if gliding { change.min(from + glide_piece_beats(map, from)) } else { change }.min(end);
            // A sliver at the end of the clip goes with the piece before it.
            let to = if end - to < 1e-6 { end } else { to };
            let tempo = if gliding { (to - from) * 60.0 / map.seconds_between(from, to) } else { map.tempo_at(from) };
            pieces.push(Piece { start: from, end: to, tempo, within: within(from) });
            from = to;
        }
    }
    let Some(round) = content.round() else { return pieces };
    // A clip that plays round is cut again wherever it comes round: each round a piece that begins at
    // the start of the loop, joined to the one before it as pieces are where the tempo changes.
    let comes_round = content.rounds(map, start, start, end);
    let mut cut = Vec::with_capacity(pieces.len() + comes_round.len());
    for piece in pieces {
        let mut from = piece.start;
        let inside = comes_round.iter().copied().filter(|beat| *beat > piece.start + 1e-6 && *beat < piece.end - 1e-6);
        for beat in inside {
            cut.push(Piece { start: from, end: beat, tempo: piece.tempo, within: content.wrapped(within(from)) });
            from = beat;
        }
        cut.push(Piece { start: from, end: piece.end, tempo: piece.tempo, within: content.wrapped(within(from)) });
    }
    // A piece that begins where the clip comes round begins at the loop's start, to the frame.
    for piece in &mut cut {
        if comes_round.iter().any(|beat| (*beat - piece.start).abs() <= 1e-9) {
            piece.within = round.start_seconds;
        }
    }
    cut
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
            region.clip_start = end;
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
    /// Whether a track that is not a return is soloed.
    solo_beside_returns: bool,
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
        let soloed = |track: &&Track| track.solo;
        Self {
            buses,
            keys,
            order,
            any_solo: project.tracks.iter().any(|track| track.solo),
            solo_beside_returns: project.tracks.iter().filter(soloed).any(|track| track.kind != TrackKind::Return),
        }
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

    /// Mute and solo, resolved: whether a track's own sound is wanted at
    /// the master. Soloing a track keeps its group open, and soloing a
    /// group keeps its members. Returns stay open while a track or a group
    /// is soloed, so that it is heard with its reverb; a soloed return is
    /// heard without the others.
    fn heard(&self, project: &Project, track: &Track) -> bool {
        if track.mute {
            return false;
        }
        if !self.any_solo || track.solo {
            return true;
        }
        match track.kind {
            TrackKind::Return => self.solo_beside_returns,
            TrackKind::Group => project.tracks.iter().any(|member| member.group == Some(track.id) && member.solo),
            TrackKind::Normal => track.group.is_some_and(|group| project.track(group).is_ok_and(|group| group.solo)),
        }
    }

    /// Whether `id` is a return that is soloed: it is then heard as every
    /// track feeds it, whether or not that track is heard itself. That is
    /// what soloing a return is for: its sound alone, of the whole song.
    fn takes_every_send(&self, project: &Project, id: TrackId) -> bool {
        project.track(id).is_ok_and(|track| track.kind == TrackKind::Return && track.solo && !track.mute)
    }

    /// Whether a track's send to `to` carries anything. A return that is
    /// soloed takes every track's send, and so does one that is not heard
    /// and that a device listens to: the device then hears what it would
    /// in the whole mix, whatever is muted or soloed around it. Any other
    /// return takes the sends of the tracks that are heard, if it is heard
    /// itself.
    fn send_open(&self, project: &Project, track: &Track, to: TrackId) -> bool {
        let Ok(target) = project.track(to) else { return false };
        let heard_there = self.heard(project, target);
        let takes_all = self.takes_every_send(project, to) || (!heard_there && self.key(to).is_some());
        !track.mute && (takes_all || (heard_there && self.heard(project, track)))
    }

    /// Whether a track's sound goes on to its output, the master or its
    /// group: it is heard, or it plays through a group that is not heard
    /// and whose sound is wanted all the same, because the group sends to
    /// a soloed return or a device listens to it.
    fn to_output(&self, project: &Project, track: &Track) -> bool {
        if track.mute {
            return false;
        }
        let wanted_unheard = |group: &Track| {
            let sends = || sent_to(project, group).into_iter().any(|to| self.send_open(project, group, to));
            !self.heard(project, group) && (self.key(group.id).is_some() || sends())
        };
        self.heard(project, track)
            || track.group.and_then(|group| project.track(group).ok()).is_some_and(wanted_unheard)
    }

    /// Whether a track passes sound at all: on to its output, or into a
    /// send.
    fn passes(&self, project: &Project, track: &Track) -> bool {
        self.to_output(project, track)
            || sent_to(project, track).into_iter().any(|to| self.send_open(project, track, to))
    }

    /// The tracks a render has to make and play: the ones that are heard,
    /// and whatever feeds them. With nothing soloed that is every track
    /// that is not muted. With a solo it is the soloed tracks and the
    /// members of a soloed group; from those on, the groups and returns
    /// their sound is heard through; and from all of them back, every
    /// track that [feeds](Self::feeds) one that is made: the members of a
    /// group, the tracks whose sends reach a soloed return, and the tracks
    /// that devices listen to, which are made whether or not they are
    /// heard. A return that is open only so that what is soloed is heard
    /// with its reverb is not made when nothing that is made sends to it.
    fn needed(&self, project: &Project) -> HashSet<TrackId> {
        let own = |track: &&Track| {
            self.heard(project, track) && (!self.any_solo || track.solo || track.kind == TrackKind::Normal)
        };
        let mut needed: HashSet<TrackId> = project.tracks.iter().filter(own).map(|track| track.id).collect();
        // The master is always made, and with it what its devices listen to.
        needed.extend(listened_to(&project.master.devices).filter(|key| self.key(*key).is_some()));
        let feeds = self.feeds(project);
        let heard = |id: TrackId| project.track(id).is_ok_and(|track| self.heard(project, track));
        loop {
            let before = needed.len();
            for (from, to, how) in &feeds {
                // On, into what is heard; and back, from whatever is made.
                if *how == Feed::Sound && needed.contains(from) && heard(*to) {
                    needed.insert(*to);
                }
                if needed.contains(to) {
                    needed.insert(*from);
                }
            }
            if needed.len() == before {
                break;
            }
        }
        needed
    }

    /// Who feeds whom, as mute and solo leave it: a member plays into its
    /// group, a track into each return its sends reach (a send set by
    /// hand, or one that is its envelope: [`sent_to`]), and a track is
    /// listened to by the devices of another. Whatever else comes to feed
    /// a track is one more line here, and [`Routing::needed`] makes it.
    fn feeds(&self, project: &Project) -> Vec<(TrackId, TrackId, Feed)> {
        let mut feeds = Vec::new();
        for track in &project.tracks {
            let through = track.group.filter(|_| self.to_output(project, track));
            feeds.extend(through.map(|group| (track.id, group, Feed::Sound)));
            let open = sent_to(project, track).into_iter().filter(|to| self.send_open(project, track, *to));
            feeds.extend(open.map(|to| (track.id, to, Feed::Sound)));
            let keys = listened_to(&track.devices).filter(|key| self.key(*key).is_some());
            feeds.extend(keys.map(|key| (key, track.id, Feed::Key)));
        }
        feeds
    }
}

/// The tracks a render of `project` makes: see [`Routing::needed`].
pub(crate) fn needed(project: &Project) -> HashSet<TrackId> {
    // A render is of the song: never of a reference beside it.
    let project = project.as_heard(true);
    Routing::of(&project).needed(&project)
}

/// How one track feeds another.
#[derive(Copy, Clone, PartialEq, Eq)]
enum Feed {
    /// Its sound goes into it: a member into its group, a send into its
    /// return.
    Sound,
    /// A device of the other listens to it. It is made for that, and
    /// being made does not make the listener heard.
    Key,
}

/// The tracks that `devices` listen to.
fn listened_to(devices: &[Device]) -> impl Iterator<Item = TrackId> + '_ {
    devices.iter().filter_map(|device| device.sidechain)
}

/// The envelope that moves what `matches` picks out, if it has one.
fn envelope(project: &Project, matches: impl Fn(&AutomationTarget) -> bool) -> Option<Envelope> {
    // A lane that is switched off is no lane to the engine.
    project.automation.iter().find(|lane| lane.enabled && matches(&lane.target)).map(Envelope::of)
}

/// The returns `track` sends to: by a send set by hand, and by one that is
/// its envelope alone (switched on: off, it sends nothing). Whoever asks
/// what a track's sends reach asks this.
fn sent_to(project: &Project, track: &Track) -> Vec<TrackId> {
    let mut to: Vec<TrackId> = track.sends.iter().map(|send| send.to).collect();
    for lane in project.automation.iter().filter(|lane| lane.enabled) {
        if let AutomationTarget::Send { track: from, to: at } = lane.target
            && from == track.id
            && !to.contains(&at)
        {
            to.push(at);
        }
    }
    to
}

/// What `track` sends to the returns: each send with the level it was set
/// to by hand (nothing, for one that is its envelope alone), the envelope
/// that moves it if it has one, and whether mute and solo leave it open.
fn sends(project: &Project, track: &Track, routing: &Routing) -> Vec<SendNode> {
    sent_to(project, track)
        .into_iter()
        .filter_map(|to| {
            let set = track.sends.iter().find(|send| send.to == to);
            let moved = |target: &AutomationTarget| {
                matches!(target, AutomationTarget::Send { track: from, to: at } if *from == track.id && *at == to)
            };
            Some(SendNode {
                bus: routing.bus(to)?,
                to,
                gain: set.map_or(0.0, |send| db_to_gain(send.level_db)),
                automation: envelope(project, moved),
                open: routing.send_open(project, track, to),
                glide: Default::default(),
                gate: Default::default(),
            })
        })
        .collect()
}

/// A clip's notes as the engine plays them: absolute positions, with the
/// offset, loop and trimming already resolved by the model.
fn flatten_notes(clip: &Clip, out: &mut Vec<NoteEvent>) {
    out.extend(clip.timeline_notes().into_iter().map(|note| NoteEvent {
        start: note.start,
        end: note.end(),
        pitch: note.pitch,
        velocity: note.velocity,
        channel: note.channel,
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
            Note { pitch: 60, start: 0.0, length: 1.0, velocity: 100, ..Default::default() },
            Note { pitch: 62, start: 1.5, length: 4.0, velocity: 100, ..Default::default() },
            Note { pitch: 64, start: 6.0, length: 1.0, velocity: 100, ..Default::default() },
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
