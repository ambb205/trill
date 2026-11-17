//! An instrument that was recorded note by note, as the player takes it:
//! which recording sounds for which keys and how hard they are struck, and
//! everything that is done to it on the way out.
//!
//! Nothing here knows of a file format. A reader ([`crate::sfz`]) fills
//! this in from a file of its format; another format is another reader.

use std::collections::BTreeMap;

/// The lowest and the highest of something that is counted from 0 to 127:
/// keys, velocities, positions of a controller. Both ends belong to it.
pub type Span = (u8, u8);

/// What makes a region sound.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum Trigger {
    /// A key going down.
    #[default]
    Attack,
    /// A key coming up, or the pedal that held it: the thump of a damper,
    /// the ring a note leaves.
    Release,
    /// A key coming up, whether or not the pedal holds its note.
    ReleaseKey,
    /// A key going down while no other is held.
    First,
    /// A key going down while another is held.
    Legato,
}

/// How a region's recording is played through.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum LoopMode {
    /// From its start to its end, or until the note is let go of.
    NoLoop,
    /// To its end, whatever the key does: a drum.
    OneShot,
    /// Round its loop for as long as it sounds, the release included.
    Continuous,
    /// Round its loop while the note is held, and on to the end after.
    Sustain,
}

/// How quickly a voice goes when another group silences it.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum OffMode {
    /// In a few milliseconds.
    Fast,
    /// By its own release.
    Normal,
    /// In this many seconds.
    Time(f32),
}

/// The shape a crossfade follows.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum FadeCurve {
    /// Equal power: the two sides add up to the same loudness all the way.
    #[default]
    Power,
    /// A straight line of gain.
    Gain,
}

impl FadeCurve {
    /// The gain at `t` of the way in, 0..=1.
    pub fn at(self, t: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        match self {
            FadeCurve::Power => t.sqrt(),
            FadeCurve::Gain => t,
        }
    }
}

/// A fade in over the bottom of a span and out over the top of it, in the
/// units of what is faded over (keys, velocities, a controller's 0..=127).
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Fade {
    /// Silent at and below `in_low`, full from `in_high` up.
    pub in_low: f32,
    pub in_high: f32,
    /// Full up to `out_low`, silent from `out_high` up.
    pub out_low: f32,
    pub out_high: f32,
}

impl Default for Fade {
    fn default() -> Self {
        Self { in_low: 0.0, in_high: 0.0, out_low: 127.0, out_high: 127.0 }
    }
}

impl Fade {
    /// Whether it fades anything.
    pub fn is_set(&self) -> bool {
        *self != Self::default()
    }

    /// The gain at `value`.
    pub fn gain(&self, value: f32, curve: FadeCurve) -> f32 {
        let fade_in = if value < self.in_low {
            0.0
        } else if value < self.in_high {
            curve.at((value - self.in_low) / (self.in_high - self.in_low).max(1e-6))
        } else {
            1.0
        };
        let fade_out = if value > self.out_high {
            0.0
        } else if value > self.out_low {
            curve.at((self.out_high - value) / (self.out_high - self.out_low).max(1e-6))
        } else {
            1.0
        };
        fade_in * fade_out
    }
}

/// What a controller moves of a region while it sounds.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum Moved {
    /// The level, in dB at the top of the controller's travel.
    Volume,
    /// The level, as a share of what it is: 100 leaves it at the top and
    /// silences it at the bottom.
    Amplitude,
    /// The place between the sides, -100..=100 at the top.
    Pan,
    /// The pitch, in cents at the top.
    Tune,
    /// The filter's cutoff, in cents at the top.
    Cutoff,
    /// How deep the vibrato is, in cents at the top.
    Vibrato,
    /// How deep the tremolo is, in dB at the top.
    Tremolo,
}

/// A controller that moves something of a region.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Control {
    pub controller: u8,
    pub moved: Moved,
    pub depth: f32,
}

/// Which part of an envelope a controller or the velocity lengthens.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Stage {
    Delay,
    Attack,
    Hold,
    Decay,
    Sustain,
    Release,
}

/// The envelope of a region's level: times in seconds, levels 0..=1.
#[derive(Clone, Debug, PartialEq)]
pub struct Envelope {
    pub delay: f32,
    /// The level the attack begins at.
    pub start: f32,
    pub attack: f32,
    pub hold: f32,
    pub decay: f32,
    pub sustain: f32,
    pub release: f32,
    /// What a full velocity adds to a stage (seconds, or level for the
    /// sustain), and what a controller at the top of its travel adds.
    pub by_velocity: Vec<(Stage, f32)>,
    pub by_controller: Vec<(Stage, u8, f32)>,
}

impl Default for Envelope {
    fn default() -> Self {
        Self {
            delay: 0.0,
            start: 0.0,
            attack: 0.0,
            hold: 0.0,
            decay: 0.0,
            sustain: 1.0,
            release: 0.0,
            by_velocity: Vec::new(),
            by_controller: Vec::new(),
        }
    }
}

impl Envelope {
    /// The length or level of `stage` for a note of `velocity` (0..=1)
    /// with the controllers where `controller` says they are (0..=1).
    pub fn stage(&self, stage: Stage, velocity: f32, controller: impl Fn(u8) -> f32) -> f32 {
        let base = match stage {
            Stage::Delay => self.delay,
            Stage::Attack => self.attack,
            Stage::Hold => self.hold,
            Stage::Decay => self.decay,
            Stage::Sustain => self.sustain,
            Stage::Release => self.release,
        };
        let by_velocity: f32 =
            self.by_velocity.iter().filter(|(which, _)| *which == stage).map(|(_, amount)| amount * velocity).sum();
        let by_controller: f32 = self
            .by_controller
            .iter()
            .filter(|(which, _, _)| *which == stage)
            .map(|(_, number, amount)| amount * controller(*number))
            .sum();
        let value = base + by_velocity + by_controller;
        if stage == Stage::Sustain { value.clamp(0.0, 1.0) } else { value.max(0.0) }
    }
}

/// The kinds of filter a region may have.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum FilterKind {
    LowPass1,
    HighPass1,
    #[default]
    LowPass2,
    HighPass2,
    BandPass2,
    LowPass4,
}

/// A region's filter.
#[derive(Clone, Debug, PartialEq)]
pub struct Filter {
    pub kind: FilterKind,
    pub cutoff: f32,
    /// How far the filter rings at its cutoff, in dB.
    pub resonance: f32,
    /// How far a full velocity opens it, in cents.
    pub by_velocity: f32,
    /// How far each key from `centre` moves it, in cents.
    pub by_key: f32,
    pub centre: u8,
    /// Its own envelope, and how far that opens it at its top, in cents.
    pub envelope: Option<(Envelope, f32)>,
}

impl Default for Filter {
    fn default() -> Self {
        Self {
            kind: FilterKind::default(),
            cutoff: 20_000.0,
            resonance: 0.0,
            by_velocity: 0.0,
            by_key: 0.0,
            centre: 60,
            envelope: None,
        }
    }
}

/// A slow wave that moves a region's pitch (vibrato) or level (tremolo).
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct Wobble {
    /// Cycles a second.
    pub rate: f32,
    /// How far it moves what it moves: cents for the pitch, dB for the
    /// level. What a controller adds is in [`Region::controls`].
    pub depth: f32,
    /// Seconds before it begins, and seconds it takes to reach its depth.
    pub delay: f32,
    pub fade: f32,
}

/// What a keyswitch has to be for a region to sound.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Switched {
    /// The last switch pressed has to be one of these keys.
    pub last: Option<Span>,
    /// This key has to be held.
    pub down: Option<u8>,
    /// This key must not be held.
    pub up: Option<u8>,
    /// The note before has to have been this key.
    pub previous: Option<u8>,
}

/// One recording and when it sounds.
#[derive(Clone, Debug, PartialEq)]
pub struct Region {
    /// Which of [`Instrument::recordings`]; `None` for a region that is
    /// there only to silence others.
    pub recording: Option<u32>,
    pub keys: Span,
    pub velocities: Span,
    /// The key that plays the recording at its own pitch.
    pub root: u8,
    /// Cents the recording is tuned by, its transposition included.
    pub tune: f32,
    /// Cents each key from the root moves the pitch: 100 for an
    /// instrument, 0 for a drum.
    pub pitch_by_key: f32,
    /// Cents a full velocity moves the pitch.
    pub pitch_by_velocity: f32,
    /// Cents the pitch may be off either way, at random.
    pub pitch_random: f32,
    /// Cents the pitch wheel bends at its top, and at its bottom.
    pub bend_up: f32,
    pub bend_down: f32,

    pub volume: f32,
    /// A share of the recording's level, 0..=1 and above.
    pub amplitude: f32,
    /// -1 (left) ..= 1 (right).
    pub pan: f32,
    /// How much of a stereo recording's width is kept: 1 all, 0 mono.
    pub width: f32,
    /// How much of its level the velocity decides: 1 for the usual law,
    /// 0 for every note as loud, negative for softer when struck harder.
    pub level_by_velocity: f32,
    /// The gain for each velocity where the instrument draws its own
    /// curve, in place of the usual law.
    pub velocity_curve: Option<Box<[f32; 128]>>,
    /// dB each key from `level_centre` adds.
    pub level_by_key: f32,
    pub level_centre: u8,
    /// dB the level may be off, at random.
    pub level_random: f32,

    /// The part of a random number from 0 to 1 that plays it: several
    /// recordings of one note share the number out between them.
    pub random: (f32, f32),
    /// Its turn among the recordings of one note that are taken in turn:
    /// which, counted from 1, of how many.
    pub turn: (u16, u16),
    pub trigger: Trigger,
    /// dB a second by which a release sound is quieter for every second
    /// its note was held.
    pub release_decay: f32,
    pub switched: Switched,
    /// Controllers that have to be within a span (0..=127) for it to sound.
    pub when: Vec<(u8, f32, f32)>,

    /// Voices of the same group are silenced by the group named in
    /// `off_by`; 0 is no group.
    pub group: u32,
    pub off_by: Option<u32>,
    pub off_mode: OffMode,
    /// The most voices one key may have sounding in this region's group.
    pub note_polyphony: Option<u16>,

    /// Frames into the recording it starts at, and how many more at
    /// random.
    pub offset: u32,
    pub offset_random: u32,
    /// The last frame it plays; the recording's own end where `None`.
    pub end: Option<u32>,
    /// Seconds before it sounds, and how many more at random.
    pub delay: f32,
    pub delay_random: f32,
    /// How it loops; `None` leaves it to the recording, which loops if its
    /// file says where.
    pub loop_mode: Option<LoopMode>,
    pub loop_start: Option<u32>,
    pub loop_end: Option<u32>,
    /// Seconds over which the end of the loop is faded into its start.
    pub loop_crossfade: f32,

    pub envelope: Envelope,
    pub filter: Option<Filter>,
    pub vibrato: Option<Wobble>,
    pub tremolo: Option<Wobble>,
    pub controls: Vec<Control>,

    pub fade_keys: Fade,
    pub fade_velocities: Fade,
    pub fade_controllers: Vec<(u8, Fade)>,
    pub key_curve: FadeCurve,
    pub velocity_fade_curve: FadeCurve,
    pub controller_curve: FadeCurve,
}

impl Default for Region {
    fn default() -> Self {
        Self {
            recording: None,
            keys: (0, 127),
            velocities: (0, 127),
            root: 60,
            tune: 0.0,
            pitch_by_key: 100.0,
            pitch_by_velocity: 0.0,
            pitch_random: 0.0,
            bend_up: 200.0,
            bend_down: -200.0,
            volume: 0.0,
            amplitude: 1.0,
            pan: 0.0,
            width: 1.0,
            level_by_velocity: 1.0,
            velocity_curve: None,
            level_by_key: 0.0,
            level_centre: 60,
            level_random: 0.0,
            random: (0.0, 1.0),
            turn: (1, 1),
            trigger: Trigger::Attack,
            release_decay: 0.0,
            switched: Switched::default(),
            when: Vec::new(),
            group: 0,
            off_by: None,
            off_mode: OffMode::Fast,
            note_polyphony: None,
            offset: 0,
            offset_random: 0,
            end: None,
            delay: 0.0,
            delay_random: 0.0,
            loop_mode: None,
            loop_start: None,
            loop_end: None,
            loop_crossfade: 0.0,
            envelope: Envelope::default(),
            filter: None,
            vibrato: None,
            tremolo: None,
            controls: Vec::new(),
            fade_keys: Fade::default(),
            fade_velocities: Fade::default(),
            fade_controllers: Vec::new(),
            key_curve: FadeCurve::Power,
            velocity_fade_curve: FadeCurve::Power,
            controller_curve: FadeCurve::Power,
        }
    }
}

impl Region {
    /// Whether a key going down may sound it.
    pub fn on_key_down(&self) -> bool {
        matches!(self.trigger, Trigger::Attack | Trigger::First | Trigger::Legato)
    }

    /// The gain the velocity gives a note of this region, before any
    /// crossfade: the square of the velocity where it decides everything.
    pub fn velocity_gain(&self, velocity: f32) -> f32 {
        let curve = match &self.velocity_curve {
            Some(curve) => curve[(velocity * 127.0).round().clamp(0.0, 127.0) as usize],
            None => velocity * velocity,
        };
        let track = self.level_by_velocity;
        if track >= 0.0 { 1.0 - track + track * curve } else { 1.0 + track * curve }
    }
}

/// The keys of an instrument that choose how the notes after them are
/// played and sound nothing themselves.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Switches {
    /// The keys that switch; `None` for an instrument without any.
    pub keys: Option<Span>,
    /// The switch that is taken as pressed before any is.
    pub default: Option<u8>,
    /// What each switch chooses, as the instrument calls it.
    pub labels: BTreeMap<u8, String>,
}

/// Something in an instrument's file that this player does not do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unsupported {
    /// What the file calls it.
    pub name: String,
    /// How many times it is said.
    pub count: usize,
    /// What a listener misses for it, to end the sentence "it also asks
    /// for ...".
    pub miss: &'static str,
}

/// A sampled instrument: see the module documentation.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Instrument {
    pub name: String,
    pub regions: Vec<Region>,
    /// The recordings its regions play, as its file names them: each once,
    /// `/` between the parts, from the folder of the file.
    pub recordings: Vec<String>,
    pub switches: Switches,
    /// What the instrument calls its keys, where it says: the drums of a
    /// kit.
    pub key_labels: BTreeMap<u8, String>,
    /// What it calls the controllers it uses.
    pub controller_labels: BTreeMap<u8, String>,
    /// Where it wants controllers to be before any is moved, 0..=1.
    pub controller_starts: Vec<(u8, f32)>,
    /// Whether the sustain pedal holds its notes.
    pub pedal: bool,
    pub unsupported: Vec<Unsupported>,
    /// What is wrong with its file short of its not being read: a part of
    /// it that is missing, a line that makes no sense.
    pub problems: Vec<String>,
}

/// What an instrument is, for a list and for whoever writes for it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Summary {
    pub name: String,
    pub regions: usize,
    pub recordings: usize,
    /// The lowest and highest key that sounds a note; `None` for an
    /// instrument with no regions.
    pub range: Option<Span>,
    /// The most layers of loudness any key has.
    pub layers: usize,
    /// The most recordings any one note at one loudness has, taken in turn
    /// or at random.
    pub takes: usize,
    /// Whether its keys are drums and not pitches.
    pub drums: bool,
    /// The ways of playing it switches between by a key, with the key.
    pub switches: Vec<(u8, String)>,
    pub default_switch: Option<u8>,
    /// What it calls its keys, where it says.
    pub key_labels: Vec<(u8, String)>,
    /// The controllers it listens to, with what it calls them.
    pub controllers: Vec<(u8, String)>,
    /// Whether it has sounds of its own for a key coming up.
    pub releases: bool,
    /// Whether any of its recordings loops while a note is held.
    pub loops: bool,
    /// How far its pitch wheel bends, in semitones each way, if every
    /// region agrees.
    pub bend: Option<f32>,
}

/// Words in the name of a recording or a key that make it a drum's.
const DRUM_WORDS: &[&str] = &[
    "kick", "snare", "hihat", "hi-hat", "hat", "tom", "crash", "ride", "cymbal", "clap", "rim", "cowbell", "shaker",
    "tambourine", "conga", "bongo", "splash", "china", "stick", "bd", "sd", "hh",
];

impl Instrument {
    /// Whether `key` switches how notes are played.
    pub fn is_switch(&self, key: u8) -> bool {
        self.switches.keys.is_some_and(|(low, high)| (low..=high).contains(&key))
    }

    /// The controllers anything of the instrument listens to, lowest first.
    pub fn controllers(&self) -> Vec<u8> {
        let mut used: Vec<u8> = Vec::new();
        for region in &self.regions {
            used.extend(region.when.iter().map(|(controller, _, _)| *controller));
            used.extend(region.controls.iter().map(|control| control.controller));
            used.extend(region.fade_controllers.iter().map(|(controller, _)| *controller));
            used.extend(region.envelope.by_controller.iter().map(|(_, controller, _)| *controller));
        }
        used.sort_unstable();
        used.dedup();
        used
    }

    /// What the instrument is: see [`Summary`].
    pub fn summary(&self) -> Summary {
        let sounding: Vec<&Region> =
            self.regions.iter().filter(|region| region.recording.is_some() && region.on_key_down()).collect();
        let playing = |region: &&&Region| !self.is_switch(region.keys.0) || !self.is_switch(region.keys.1);
        let range = sounding.iter().filter(playing).fold(None, |range: Option<Span>, region| {
            let (low, high) = region.keys;
            Some(range.map_or((low, high), |(a, b)| (a.min(low), b.max(high))))
        });

        // Layers and takes, counted on every key that sounds.
        let (mut layers, mut takes) = (0, 0);
        if let Some((low, high)) = range {
            for key in low..=high {
                let mut spans: Vec<(Span, Option<Span>, usize)> = Vec::new();
                for region in sounding.iter().filter(|region| (region.keys.0..=region.keys.1).contains(&key)) {
                    let place = (region.velocities, region.switched.last);
                    match spans.iter_mut().find(|(velocities, last, _)| (*velocities, *last) == place) {
                        Some((_, _, count)) => *count += 1,
                        None => spans.push((region.velocities, region.switched.last, 1)),
                    }
                }
                // The layers of one way of playing: the most any has.
                let mut ways: Vec<(Option<Span>, usize)> = Vec::new();
                for (_, last, count) in &spans {
                    takes = takes.max(*count);
                    match ways.iter_mut().find(|(way, _)| way == last) {
                        Some((_, count)) => *count += 1,
                        None => ways.push((*last, 1)),
                    }
                }
                layers = layers.max(ways.iter().map(|(_, count)| *count).max().unwrap_or(0));
            }
        }

        let switches: Vec<(u8, String)> = match self.switches.keys {
            Some((low, high)) => {
                let mut keys: Vec<u8> = self
                    .regions
                    .iter()
                    .filter_map(|region| region.switched.last)
                    .flat_map(|(first, last)| first..=last)
                    .filter(|key| (low..=high).contains(key))
                    .collect();
                keys.sort_unstable();
                keys.dedup();
                keys.into_iter()
                    .map(|key| (key, self.switches.labels.get(&key).cloned().unwrap_or_default()))
                    .collect()
            }
            None => Vec::new(),
        };

        let bends: Vec<(f32, f32)> = sounding.iter().map(|region| (region.bend_up, region.bend_down)).collect();
        let bend = match bends.first() {
            Some((up, down)) if bends.iter().all(|other| other == &(*up, *down)) && *up == -*down => Some(up / 100.0),
            _ => None,
        };

        Summary {
            name: self.name.clone(),
            regions: self.regions.len(),
            recordings: self.recordings.len(),
            range,
            layers,
            takes,
            drums: self.plays_drums(&sounding),
            switches,
            default_switch: self.switches.default,
            key_labels: self.key_labels.iter().map(|(key, label)| (*key, label.clone())).collect(),
            controllers: self
                .controllers()
                .into_iter()
                .map(|controller| (controller, self.controller_labels.get(&controller).cloned().unwrap_or_default()))
                .collect(),
            releases: self.regions.iter().any(|region| !region.on_key_down() && region.recording.is_some()),
            loops: self
                .regions
                .iter()
                .any(|region| matches!(region.loop_mode, Some(LoopMode::Continuous | LoopMode::Sustain))),
            bend,
        }
    }

    /// Whether the keys are drums: most regions sit on one key each and
    /// play to their end whatever the key does or do not follow the keys
    /// in pitch, and the names say drums or the keys are not a row of
    /// pitches.
    fn plays_drums(&self, sounding: &[&Region]) -> bool {
        if sounding.is_empty() {
            return false;
        }
        let struck = |region: &&&Region| {
            let one_key = region.keys.0 == region.keys.1;
            let fixed = region.pitch_by_key == 0.0;
            fixed || (one_key && region.loop_mode == Some(LoopMode::OneShot))
        };
        let share = sounding.iter().filter(struck).count() as f32 / sounding.len() as f32;
        if share < 0.8 {
            return false;
        }
        // An instrument whose recordings do not follow the keys at all is
        // one of hits. Otherwise it may be a marimba played to the end of
        // each bar: then the names decide.
        if sounding.iter().all(|region| region.pitch_by_key == 0.0) {
            return true;
        }
        let named = |text: &str| {
            let text = text.to_lowercase();
            DRUM_WORDS.iter().any(|word| text.contains(word))
        };
        let recordings =
            sounding.iter().filter_map(|region| region.recording).map(|index| self.recordings[index as usize].as_str());
        let (mut total, mut drums) = (0, 0);
        for name in recordings {
            total += 1;
            drums += usize::from(named(name));
        }
        let labelled = self.key_labels.values().filter(|label| named(label)).count();
        drums * 2 > total || labelled * 2 > self.key_labels.len().max(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fade_comes_in_over_its_bottom_and_goes_out_over_its_top() {
        let fade = Fade { in_low: 20.0, in_high: 40.0, out_low: 80.0, out_high: 100.0 };
        assert!(fade.is_set() && !Fade::default().is_set());
        let gain = |value: f32| fade.gain(value, FadeCurve::Gain);
        assert_eq!((gain(0.0), gain(20.0), gain(30.0), gain(40.0), gain(60.0)), (0.0, 0.0, 0.5, 1.0, 1.0));
        assert_eq!((gain(80.0), gain(90.0), gain(100.0), gain(127.0)), (1.0, 0.5, 0.0, 0.0));
        // Equal power: the two sides of a crossing add up to one in power.
        let (up, down) = (fade.gain(30.0, FadeCurve::Power), fade.gain(90.0, FadeCurve::Power));
        assert!((up * up + down * down - 1.0).abs() < 1e-6);
        assert_eq!(Fade::default().gain(0.0, FadeCurve::Power), 1.0);
        assert_eq!(Fade::default().gain(127.0, FadeCurve::Power), 1.0);
    }

    #[test]
    fn the_velocity_decides_as_much_of_the_level_as_the_region_says() {
        let region = Region::default();
        assert_eq!(region.velocity_gain(1.0), 1.0);
        assert!((region.velocity_gain(0.5) - 0.25).abs() < 1e-6, "the square of the velocity");
        let flat = Region { level_by_velocity: 0.0, ..Region::default() };
        assert_eq!(flat.velocity_gain(0.1), 1.0);
        let half = Region { level_by_velocity: 0.5, ..Region::default() };
        assert!((half.velocity_gain(0.0) - 0.5).abs() < 1e-6);
        let upside_down = Region { level_by_velocity: -1.0, ..Region::default() };
        assert_eq!((upside_down.velocity_gain(0.0), upside_down.velocity_gain(1.0)), (1.0, 0.0));
        let mut curve = Box::new([0.0f32; 128]);
        curve[64] = 0.9;
        let drawn = Region { velocity_curve: Some(curve), ..Region::default() };
        assert_eq!(drawn.velocity_gain(64.0 / 127.0), 0.9);
    }

    #[test]
    fn an_envelope_is_lengthened_by_the_velocity_and_by_controllers() {
        let envelope = Envelope {
            attack: 0.1,
            release: 0.5,
            sustain: 0.8,
            by_velocity: vec![(Stage::Attack, -0.08), (Stage::Sustain, 0.5)],
            by_controller: vec![(Stage::Release, 72, 2.0)],
            ..Envelope::default()
        };
        let at = |stage, velocity, wheel: f32| envelope.stage(stage, velocity, |number| if number == 72 { wheel } else { 0.0 });
        assert!((at(Stage::Attack, 1.0, 0.0) - 0.02).abs() < 1e-6);
        assert_eq!(at(Stage::Attack, 0.0, 0.0), 0.1);
        assert_eq!(at(Stage::Release, 0.0, 0.5), 1.5);
        assert_eq!(at(Stage::Sustain, 1.0, 0.0), 1.0, "a level stays a level");
        assert_eq!(at(Stage::Decay, 1.0, 1.0), 0.0);
    }

    fn piano() -> Instrument {
        let mut regions = Vec::new();
        for (index, root) in [48u8, 51, 54, 57, 60].into_iter().enumerate() {
            for (layer, velocities) in [(0u32, (1u8, 60u8)), (1, (61, 127))] {
                regions.push(Region {
                    recording: Some(index as u32 * 2 + layer),
                    keys: (root - 1, root + 1),
                    velocities,
                    root,
                    ..Region::default()
                });
            }
        }
        regions.push(Region { recording: Some(10), trigger: Trigger::Release, ..Region::default() });
        Instrument {
            name: "Piano".into(),
            regions,
            recordings: (0..11).map(|index| format!("samples/{index}.wav")).collect(),
            pedal: true,
            ..Instrument::default()
        }
    }

    #[test]
    fn a_summary_says_what_an_instrument_is() {
        let summary = piano().summary();
        assert_eq!((summary.regions, summary.recordings), (11, 11));
        assert_eq!(summary.range, Some((47, 61)));
        assert_eq!((summary.layers, summary.takes), (2, 1));
        assert!(!summary.drums && summary.releases && !summary.loops);
        assert_eq!(summary.bend, Some(2.0));
        assert!(summary.switches.is_empty() && summary.controllers.is_empty());
        assert_eq!(Instrument::default().summary().range, None);

        // Two takes of each note in turn, and two ways of playing chosen by a key.
        let mut guitar = piano();
        guitar.switches = Switches { keys: Some((24, 25)), default: Some(24), labels: [(24, "Open".to_string())].into() };
        let regions = std::mem::take(&mut guitar.regions);
        for region in regions.iter().filter(|region| region.on_key_down()) {
            for turn in 1..=2 {
                for key in [24u8, 25] {
                    let switched = Switched { last: Some((key, key)), ..Switched::default() };
                    guitar.regions.push(Region { turn: (2, turn), switched, ..region.clone() });
                }
            }
        }
        guitar.regions.push(Region {
            recording: Some(0),
            loop_mode: Some(LoopMode::Sustain),
            controls: vec![Control { controller: 1, moved: Moved::Vibrato, depth: 30.0 }],
            bend_up: 1200.0,
            ..Region::default()
        });
        guitar.controller_labels.insert(1, "Vibrato".into());
        let summary = guitar.summary();
        assert_eq!((summary.layers, summary.takes), (2, 2));
        assert_eq!(summary.switches, [(24, "Open".to_string()), (25, String::new())]);
        assert_eq!(summary.default_switch, Some(24));
        assert_eq!(summary.controllers, [(1, "Vibrato".to_string())]);
        assert!(summary.loops && summary.bend.is_none(), "the regions do not agree about the wheel");
    }

    #[test]
    fn a_kit_of_hits_is_drums_and_a_row_of_pitches_is_not() {
        let hit = |key: u8, name: &str, index: u32| {
            (
                Region {
                    recording: Some(index),
                    keys: (key, key),
                    root: key,
                    loop_mode: Some(LoopMode::OneShot),
                    ..Region::default()
                },
                name.to_string(),
            )
        };
        let of = |hits: Vec<(Region, String)>| {
            let (regions, recordings) = hits.into_iter().unzip();
            Instrument { regions, recordings, ..Instrument::default() }
        };
        let kit = of(vec![hit(36, "Kick_01.wav", 0), hit(38, "Snare_Center.wav", 1), hit(42, "HH_closed.wav", 2)]);
        assert!(kit.summary().drums);
        // The same shape with the names of notes is a marimba.
        let marimba = of(vec![hit(60, "C4.wav", 0), hit(61, "Db4.wav", 1), hit(62, "D4.wav", 2)]);
        assert!(!marimba.summary().drums);
        // Hits that do not follow the keys at all are drums whatever they are called.
        let mut fixed = marimba.clone();
        fixed.regions.iter_mut().for_each(|region| region.pitch_by_key = 0.0);
        assert!(fixed.summary().drums);
        // And what the instrument calls its keys counts as the names do.
        let mut labelled = marimba;
        labelled.key_labels = [(60, "Kick".to_string()), (61, "Snare".to_string())].into();
        assert!(labelled.summary().drums);
        assert!(!piano().summary().drums);
    }
}
