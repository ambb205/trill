//! A song whose tempo and time signature change, played by the engine
//! through a scripted plugin of each format, which writes down what it is
//! told with every block ([`super::transport::written`]). One body for
//! both formats: `clap/tests.rs` and `vst3/tests.rs` each stage it with
//! their own plugin.
//!
//! What is held: the time a plugin is told is the song's (the frames it
//! has played, which is the tempo map's time for the beat it is at), the
//! bar is the meter map's, the tempo carries one block's position to the
//! next block's, and all of it is the same whatever the size of the
//! buffers the song is played in. And what a plugin is told when the song
//! comes round its loop or is moved.

use crate::transport::written::{self, Block, Format};
use std::sync::Mutex;
use trill_core::device::{DeviceDescriptor, DeviceError, DeviceFactory, DeviceInstance, Purpose};
use trill_core::{ChainRef, Device, DeviceId, MeterMap, Note, Project, TempoMap};
use trill_engine::compile::{Compiler, DeviceBank, Meters};
use trill_engine::{AudioSources, Processor};

const SR: f64 = 48_000.0;

/// A format's scripted plugin, as these tests need it.
pub(crate) struct Stage<'a> {
    pub format: Format,
    pub factory: &'a dyn DeviceFactory,
    /// An instrument of it.
    pub uri: String,
    /// The serial of an instance, which it reports as a parameter.
    pub serial: fn(&mut DeviceInstance) -> u64,
}

/// A factory that has every instance it makes write down what it is told
/// of the song, and keeps their serials with the devices they are for.
pub(crate) struct Noting<'a> {
    pub format: Format,
    pub factory: &'a dyn DeviceFactory,
    pub serial: fn(&mut DeviceInstance) -> u64,
    pub made: Mutex<Vec<(DeviceId, u64)>>,
}

impl DeviceFactory for Noting<'_> {
    fn descriptors(&self) -> Vec<DeviceDescriptor> {
        self.factory.descriptors()
    }

    fn create(&self, device: &Device, sample_rate: f64, max_block: usize) -> Result<DeviceInstance, DeviceError> {
        self.create_for(device, sample_rate, max_block, Purpose::Playing)
    }

    fn create_for(
        &self,
        device: &Device,
        sample_rate: f64,
        max_block: usize,
        purpose: Purpose,
    ) -> Result<DeviceInstance, DeviceError> {
        let mut instance = self.factory.create_for(device, sample_rate, max_block, purpose)?;
        let serial = (self.serial)(&mut instance);
        written::begin(self.format, serial);
        self.made.lock().unwrap().push((device.id, serial));
        Ok(instance)
    }
}

/// A song of 120 that slows to 90 at beat 4, goes to 140 at beat 8, glides
/// from beat 12 down to 70 at beat 16 and takes off at 200 from there; in
/// 4/4, in 7/8 from beat 8 and in 3/4 from beat 15. One track, with the
/// stage's instrument and a note for it.
fn changing(stage: &Stage) -> Project {
    let mut project = Project::new("t");
    project.set_tempo_at(4.0, 90.0, false).unwrap();
    project.set_tempo_at(8.0, 140.0, false).unwrap();
    project.set_tempo_at(12.0, 140.0, false).unwrap();
    project.set_tempo_at(16.0, 70.0, true).unwrap();
    project.set_tempo_at(16.0, 200.0, false).unwrap();
    project.set_time_signature_at(8.0, 7, 8).unwrap();
    project.set_time_signature_at(15.0, 3, 4).unwrap();
    let track = project.add_track("Lead");
    project.add_device(ChainRef::Track(track), stage.uri.clone(), "Plugin", None).unwrap();
    let clip = project.add_midi_clip(track, "clip", 0.0, 24.0).unwrap();
    project.clip_mut(clip).unwrap().add_notes(&[Note { pitch: 60, start: 1.0, length: 1.0, velocity: 100 }]).unwrap();
    project
}

/// Plays `project` from `from` in buffers of `block` frames, as the audio
/// thread would, doing `between` before each buffer, which gets the
/// frames played so far and says whether to go on. Returns what the
/// stage's plugin was told, block by block.
fn played(
    stage: &Stage,
    project: &Project,
    from: f64,
    block: usize,
    mut between: impl FnMut(&mut Processor, usize) -> bool,
) -> Vec<Block> {
    let noting =
        Noting { format: stage.format, factory: stage.factory, serial: stage.serial, made: Mutex::new(Vec::new()) };
    let (mut bank, mut meters, audio) = (DeviceBank::default(), Meters::default(), AudioSources::default());
    let mut compiler = Compiler {
        factory: &noting,
        bank: &mut bank,
        meters: &mut meters,
        audio: &audio,
        stand_ins: None,
        sample_rate: SR,
        purpose: Purpose::Playing,
    };
    let (graph, problems) = compiler.compile(project, true);
    assert!(problems.is_empty(), "{problems:?}");
    let mut processor = Processor::new(SR);
    drop(processor.swap_graph(graph));
    processor.seek(from);
    processor.play();
    let (mut left, mut right) = (vec![0.0f32; block], vec![0.0f32; block]);
    let mut frames = 0;
    while between(&mut processor, frames) {
        processor.process(&mut left, &mut right);
        frames += block;
    }
    let made = noting.made.lock().unwrap().clone();
    assert_eq!(made.len(), 1, "the song has one plugin");
    written::taken(stage.format, made[0].1)
}

/// How far the song moves with a frame of `block`, in beats.
fn per_frame(block: &Block) -> f64 {
    block.tempo / (60.0 * SR)
}

/// Where the song is `frame` frames after `blocks` began, by what the
/// plugin was told: the block that frame is in, and the beat at it.
fn at(blocks: &[Block], starts: &[usize], frame: usize) -> (Block, f64) {
    let index = starts.partition_point(|start| *start <= frame) - 1;
    let block = blocks[index];
    (block, block.beats + (frame - starts[index]) as f64 * per_frame(&block))
}

/// The first frame of each block, counted from the first block.
fn starts(blocks: &[Block]) -> Vec<usize> {
    let mut frame = 0;
    blocks
        .iter()
        .map(|block| {
            let start = frame;
            frame += block.frames;
            start
        })
        .collect()
}

/// The bar that began last at `beat`, going on from what `block` says of
/// its own first frame.
fn bar_start_at(block: &Block, beat: f64) -> f64 {
    let per_bar = f64::from(block.signature.0) * 4.0 / f64::from(block.signature.1);
    block.bar_start + ((beat - block.bar_start) / per_bar + 1e-9).floor().max(0.0) * per_bar
}

/// Holds one run of the changing song to the song's maps.
fn held_to_the_maps(stage: &Stage, blocks: &[Block], tempo: &TempoMap, meter: &MeterMap) -> (f64, f64) {
    let starts = starts(blocks);
    let (mut worst_time, mut by_tempo_alone) = (0.0f64, 0.0f64);
    for (index, (block, start)) in blocks.iter().zip(&starts).enumerate() {
        let said = format!("{:?}, block {index} at frame {start}: {block:?}", stage.format);
        assert!(block.playing, "{said}");
        assert_eq!(block.steady, *start as i64, "{said}");

        // The beat is where the tempo has the song after that many frames:
        // in a glide, which is taken in short even steps, to within a
        // tenth of a frame.
        let wanted = tempo.beat_after(0.0, *start as f64, SR);
        let near = if tempo.is_ramping_at(wanted) { 5e-6 } else { 1e-9 };
        assert!((block.beats - wanted).abs() < near, "{said}: the song is at beat {wanted}");

        // The time is the frames played, to the frame, and so the map's
        // time for that beat. CLAP says seconds in steps of 2^-31.
        match block.samples {
            Some(samples) => assert_eq!(samples, *start as i64, "{said}"),
            None => assert!((block.seconds - *start as f64 / SR).abs() < 1e-9, "{said}"),
        }
        let off = (block.seconds - tempo.seconds_at(block.beats)).abs();
        assert!(off < 1e-5, "{said}: the map has beat {} at {} s", block.beats, tempo.seconds_at(block.beats));
        worst_time = worst_time.max(off);
        // What the block's tempo alone would have made of the beat.
        by_tempo_alone = by_tempo_alone.max((block.beats * 60.0 / block.tempo - tempo.seconds_at(block.beats)).abs());

        // The bar and the time signature are the song's there.
        let bar = meter.bar_at(block.beats);
        assert_eq!(block.bar_start, bar.start, "{said}: the bar is {bar:?}");
        if let Some(number) = block.bar {
            assert_eq!(number, bar.index, "{said}: the bar is {bar:?}");
        }
        assert_eq!(block.signature, (bar.signature.numerator.into(), bar.signature.denominator.into()), "{said}");
        assert_eq!(block.looping, None, "{said}");

        // Going on from this block at its tempo ends where the next
        // begins: to within the one frame a change of tempo falls in,
        // which is told at the tempo before it.
        if let Some(next) = blocks.get(index + 1) {
            let reached = block.beats + block.frames as f64 * per_frame(block);
            let slack = (per_frame(next) - per_frame(block)).abs() + 1e-9;
            assert!((reached - next.beats).abs() <= slack, "{said}: reaches {reached}, and the next says {next:?}");
        }
    }
    (worst_time, by_tempo_alone)
}

/// Across jumps of the tempo, a glide and changes of time signature, a
/// plugin is told the song's own time and bars, each block goes on where
/// the one before ended, and it is the same in buffers of any size.
pub(crate) fn a_plugin_is_told_the_songs_time_and_bars_whatever_the_buffer_size(stage: &Stage) {
    let project = changing(stage);
    let (tempo, meter) = (project.tempo_map(), project.meter_map());
    let length = (tempo.seconds_at(20.0) * SR) as usize;
    let run = |block: usize| played(stage, &project, 0.0, block, |_, frames| frames < length);

    let reference = run(512);
    let (worst, alone) = held_to_the_maps(stage, &reference, &tempo, &meter);
    println!(
        "{:?}: {} blocks through five tempos, a glide and three time signatures; the time told is within {:.1e} s \
         of the map's; worked out from each block's tempo alone it would have been up to {alone:.2} s off",
        stage.format,
        reference.len(),
        worst
    );
    assert!(alone > 1.0, "the song is one in which the block's tempo does not say the time");
    // Every tempo was told, and each time signature.
    for wanted in [120.0, 90.0, 140.0, 200.0] {
        assert!(reference.iter().any(|block| block.tempo == wanted), "{wanted} BPM was never told");
    }
    for wanted in [(4, 4), (7, 8), (3, 4)] {
        assert!(reference.iter().any(|block| block.signature == wanted), "{wanted:?} was never told");
    }
    // The glide is told as steps, none longer than 256 frames, each a
    // little slower than the one before.
    let starts_of = starts(&reference);
    let gliding: Vec<&Block> = reference.iter().filter(|block| tempo.is_ramping_at(block.beats + 1e-6)).collect();
    assert!(gliding.len() > 100, "{} blocks in the glide", gliding.len());
    assert!(gliding.iter().all(|block| block.frames <= 256), "a step of the glide is at most 256 frames");
    assert!(gliding.windows(2).all(|pair| pair[1].tempo <= pair[0].tempo), "the tempo comes down, and never goes up");
    let steps = gliding.windows(2).filter(|pair| pair[1].tempo < pair[0].tempo).count();
    assert!(steps > 100, "{steps} steps down in the glide");

    // In buffers of other sizes the plugin is told the same song: at the
    // first frame of each of their blocks, what the reference says of
    // that frame.
    for size in [333, 37, 4096] {
        let blocks = run(size);
        held_to_the_maps(stage, &blocks, &tempo, &meter);
        for (block, start) in blocks.iter().zip(starts(&blocks)) {
            let (there, beat) = at(&reference, &starts_of, start);
            let said = format!("{:?}, buffers of {size}, frame {start}: {block:?} for {there:?}", stage.format);
            assert!((block.beats - beat).abs() < 1e-9, "{said}");
            assert_eq!((block.tempo, block.signature), (there.tempo, there.signature), "{said}");
            assert_eq!(block.bar_start, bar_start_at(&there, beat), "{said}");
        }
    }
}

/// What a plugin is told when the song comes round its loop, is moved
/// while it plays, and is stopped: the position is where the song now is,
/// in beats, in time and in bars; the time moves by a block's length with
/// every block but those; and the count of frames never jumps.
pub(crate) fn a_plugin_is_told_where_the_song_went_when_it_loops_or_is_moved(stage: &Stage) {
    let mut project = changing(stage);
    // Round a loop that has the first change of tempo in it.
    project.set_loop(true, 2.0, 6.0).unwrap();
    let (tempo, meter) = (project.tempo_map(), project.meter_map());
    let lap = tempo.frames_between(2.0, 6.0, SR);
    let block = 480;
    // Moved to beat 13, inside the glide, ten buffers after it has been
    // round twice; stopped twenty buffers later.
    let moved_at = (((tempo.frames_at(6.0, SR) + lap) / block as f64).ceil() as usize + 10) * block;
    let stopped_at = moved_at + 20 * block;
    let blocks = played(stage, &project, 0.0, block, |processor, frames| {
        if frames == moved_at {
            processor.seek(13.0);
        }
        if frames == stopped_at {
            processor.stop();
        }
        frames < stopped_at + 2 * block
    });
    let starts = starts(&blocks);
    let (loop_start, loop_end) = (tempo.seconds_at(2.0), tempo.seconds_at(6.0));

    // Where the time does not go on by the block's length, the song went
    // somewhere: twice back to the start of the loop, once to beat 13.
    let mut went = Vec::new();
    for (index, pair) in blocks.windows(2).enumerate() {
        let (block, next) = (&pair[0], &pair[1]);
        let said = format!("{:?}, block {index} at frame {}: {block:?}, then {next:?}", stage.format, starts[index]);
        assert_eq!(next.steady - block.steady, block.frames as i64, "{said}: the count of frames never jumps");
        // How far the time went on, in frames: a block's length while the
        // song plays on, and nothing once it is stopped.
        let went_on = match (block.samples, next.samples) {
            (Some(samples), Some(next)) => (next - samples) as f64,
            _ => (next.seconds - block.seconds) * SR,
        };
        if !block.playing {
            let stays = !next.playing && went_on.abs() < 1e-3 && next.beats == block.beats;
            assert!(stays, "{said}: a stopped song stays where it is");
        } else if (went_on - block.frames as f64).abs() > 1e-3 {
            went.push((starts[index + 1], next.beats));
        }
        // Wherever it is, the plugin is told the map's time and bar for
        // the beat, and the loop it is in.
        assert!((next.seconds - tempo.seconds_at(next.beats)).abs() < 1.5 / SR, "{said}");
        assert_eq!(next.bar_start, meter.bar_at(next.beats).start, "{said}");
        assert_eq!(next.looping, Some((2.0, 6.0)), "{said}");
        if let Some((start, end)) = next.loop_seconds {
            assert!((start - loop_start).abs() < 1e-9 && (end - loop_end).abs() < 1e-9, "{said}");
        }
    }
    let beats: Vec<f64> = went.iter().map(|(_, beat)| *beat).collect();
    assert_eq!(beats, [2.0, 2.0, 13.0], "{:?}: where the song went: {went:?}", stage.format);
    // The loop came round where the map has its end, to the frame, and
    // again a lap later.
    let first = tempo.frames_at(6.0, SR).ceil();
    assert_eq!(went[0].0 as f64, first, "{:?}", stage.format);
    assert!((went[1].0 as f64 - (first + lap)).abs() <= 1.0, "{:?}: {went:?} with laps of {lap} frames", stage.format);
    assert_eq!(went[2].0, moved_at, "{:?}", stage.format);
    // Stopped, the plugin is told so, and the song stays at the beat it
    // was stopped at.
    let last = blocks.last().unwrap();
    assert!(!last.playing, "{:?}: {last:?}", stage.format);
    assert!(last.beats > 13.0 && last.beats < 14.0, "{:?}: {last:?}", stage.format);
    println!(
        "{:?}: round the loop at frames {} and {}, moved to beat 13 at frame {}, stopped at beat {:.3}",
        stage.format, went[0].0, went[1].0, went[2].0, last.beats
    );
}
