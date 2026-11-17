//! The stage in the window: the song filmed as it plays.
//!
//! While [`UiState::stage_open`](super::state::UiState) is set the window
//! shows one picture in place of its panes: the song's own lanes, clips and
//! piano rolls, composed by a director and lit by what plays (the
//! `trill-stage` crate). It is the same playback; Space still plays and
//! stops, and leaving puts every pane back where it was, since none of
//! them was touched.
//!
//! The picture is painted from the snapshot, like every view, into an egui
//! context of the stage's own, and drawn by the stage's passes on the
//! window's graphics device into a texture, which the window then shows.
//! The window's own renderer only ever sees that texture. The passes are
//! built on a helper thread the first time the stage is opened (compiling
//! them takes a moment); until they are there, and where there is no
//! graphics device to build them on (the tests), the interface is painted
//! straight into the window without its light and its focus.
//!
//! The session says where the song is once per block of audio, which is
//! not once per frame. A picture that moves with the playhead would
//! stutter on that, so the stage keeps a [`Clock`] that runs at the song's
//! tempo between the session's reports and is held to them.

use super::snapshot::{Live, MeterState, Snapshot};
use super::units::METER_FLOOR_DB;
use eframe::egui::{self, Align2, Color32, Id, LayerId, Order, Rect, Role, Sense, Ui, WidgetInfo, pos2, vec2};
use eframe::egui_wgpu::{self, wgpu};
use std::sync::mpsc;
use std::time::{Duration, Instant};
use trill_noir::{Theme, fade, text};
use trill_stage::gpu::{Gpu, Quality, Renderer};
use trill_stage::scene::{self, Canvas, Moment};
use trill_stage::{Format, Heard, Look, Score, Song, Stage};

/// How long the words that say how to leave stay up after the stage is
/// opened or the pointer moves.
const HINT_FOR: Duration = Duration::from_millis(2600);
/// How far the clock may be from where the session says the song is
/// before it gives up running and jumps there, in beats.
const CLOCK_JUMP: f64 = 0.75;
/// How much of the way to the session's report the clock goes each frame.
const CLOCK_PULL: f64 = 0.06;
/// Seconds the body of the sound takes to follow the meters.
const LEVEL_RISE: f32 = 0.25;
const LEVEL_FALL: f32 = 0.9;

/// Where the song is, for a picture: the session's position made steady.
#[derive(Debug, Default)]
struct Clock {
    beat: f64,
    at: Option<Instant>,
}

impl Clock {
    /// The beat to show at `now`, given where the session last said the
    /// song was and whether it plays.
    fn beat(&mut self, reported: f64, playing: bool, tempo: f64, now: Instant) -> f64 {
        let elapsed = self.at.map_or(0.0, |at| now.duration_since(at).as_secs_f64());
        self.at = Some(now);
        if !playing {
            self.beat = reported;
            return self.beat;
        }
        // Run at the song's own pace, and lean towards what was reported:
        // the reports come in steps, the clock does not.
        let run = self.beat + elapsed.min(0.25) * tempo / 60.0;
        let off = reported - run;
        self.beat = if off.abs() > CLOCK_JUMP { reported } else { run + off * CLOCK_PULL };
        self.beat
    }
}

/// What the stage cost while the song played, for the figures a smoke run
/// prints. Frames made while the song stands still are not counted: the
/// window then paints only when something changes.
#[derive(Debug, Default)]
struct Costs {
    /// Frames that followed another while the song played, and the time
    /// they covered.
    frames: u32,
    played: Duration,
    /// What the stage's part of those frames took, the longest of them,
    /// and how much of it was handing the picture to the graphics card.
    spent: Duration,
    worst: Duration,
    card: Duration,
    /// Frames that came more than a frame and a half of a sixty-a-second
    /// display after the one before, and the longest wait for one.
    late: u32,
    longest: Duration,
    /// When the frame before this one began, if the song played then.
    last: Option<Instant>,
}

impl Costs {
    fn add(&mut self, started: Instant, playing: bool, card: Duration) {
        if !playing {
            self.last = None;
            return;
        }
        if let Some(last) = self.last {
            let (took, waited) = (started.elapsed(), started.duration_since(last));
            self.frames += 1;
            self.played += waited;
            self.spent += took;
            self.worst = self.worst.max(took);
            self.card += card;
            self.longest = self.longest.max(waited);
            self.late += u32::from(waited > Duration::from_millis(25));
        }
        self.last = Some(started);
    }
}

/// The passes that draw the picture: not asked for yet, being built, or
/// there.
enum Drawing {
    NotYet,
    Building(mpsc::Receiver<Renderer>),
    Ready(Box<Renderer>),
}

pub struct StageView {
    stage: Stage,
    /// The song as the stage films it, and the revision it was read at.
    song: Option<(u64, Song)>,
    drawing: Drawing,
    /// The texture the window shows, and the size it was registered at.
    shown: Option<(egui::TextureId, [u32; 2])>,
    clock: Clock,
    /// The body of the sound: the mix's meter, followed slowly.
    level: f32,
    frames: u32,
    /// When the hint was last called for.
    stirred: Option<Instant>,
    was_open: bool,
    costs: Costs,
}

impl StageView {
    pub fn new(theme: &Theme) -> Self {
        Self {
            stage: Stage::new(theme.clone()),
            song: None,
            drawing: Drawing::NotYet,
            shown: None,
            clock: Clock::default(),
            level: 0.0,
            frames: 0,
            stirred: None,
            was_open: false,
            costs: Costs::default(),
        }
    }

    /// The song of `snapshot`, read again only when the project changed.
    fn song(&mut self, snapshot: &Snapshot) -> &Song {
        if self.song.as_ref().is_none_or(|(revision, _)| *revision != snapshot.revision) {
            // The snapshot knows of plugins what the project alone does
            // not: which of them are drum instruments.
            let score = Score::of(&snapshot.project, |track| snapshot.has_drum_instrument(track.id));
            self.song = Some((snapshot.revision, Song::from(score)));
        }
        &self.song.as_ref().expect("read just above").1
    }

    /// What is heard now, from the meters.
    fn heard(&mut self, snapshot: &Snapshot, live: &Live, elapsed: f32) -> Heard {
        let loudest = |meter: MeterState| meter.left_db.max(meter.right_db).max(METER_FLOOR_DB);
        let tracks = snapshot.project.tracks.iter().map(|track| (track.id, loudest(live.track_meter(track.id))));
        let mut heard = Heard::from_meters(loudest(live.master), tracks);
        // The meter is the fast level; the body of the sound follows it.
        let pace = if heard.punch > self.level { LEVEL_RISE } else { LEVEL_FALL };
        self.level += (heard.punch - self.level) * (1.0 - (-elapsed / pace).exp());
        heard.level = self.level;
        heard
    }

    /// Shows the stage in the whole of `ui`. `gpu` is the window's
    /// graphics device, where it has one.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        snapshot: &Snapshot,
        live: &Live,
        latency_ms: f64,
        gpu: Option<&egui_wgpu::RenderState>,
    ) {
        let started = Instant::now();
        let ctx = ui.ctx().clone();
        let rect = ui.max_rect();
        if !self.was_open {
            self.was_open = true;
            self.stirred = Some(started);
            self.costs = Costs::default();
        }
        if ctx.input(|input| input.pointer.is_moving()) {
            self.stirred = Some(started);
        }
        let elapsed = ctx.input(|input| input.stable_dt).min(0.1);
        let tempo = snapshot.project.tempo;
        // What is heard now left the engine a little earlier than where
        // the engine is: the picture is of what is heard.
        let behind = if live.playing { latency_ms / 1000.0 * tempo / 60.0 } else { 0.0 };
        let beat = self.clock.beat(live.position - behind, live.playing, tempo, started).max(0.0);
        let heard = self.heard(snapshot, live, elapsed);
        self.song(snapshot);
        let Some((_, song)) = &self.song else { return };
        self.frames = self.frames.wrapping_add(1);

        let scale = ctx.pixels_per_point();
        let format = Format::new((rect.width() * scale).round() as u32, (rect.height() * scale).round() as u32);
        let camera = song.camera(beat, format);
        // For assistive technology and the tests: that this is the stage,
        // and where in the song it is.
        let section = song.score.sections.get(song.score.section_at(beat)).map_or("", |section| section.name.as_str());
        let label = format!("Stage: {} {}", section, song.score.name);
        let response = ui.interact(rect, Id::unique("stage"), Sense::hover());
        response.widget_info(|| WidgetInfo::labeled(Role::Label, true, &label));

        if let (Drawing::NotYet, Some(gpu)) = (&self.drawing, gpu) {
            // Building the passes compiles them, which takes a moment: not
            // on this thread.
            let (sender, receiver) = mpsc::channel();
            let device = Gpu::on(gpu.device.clone(), gpu.queue.clone(), gpu.adapter.get_info().name);
            let waker = ctx.clone();
            std::thread::spawn(move || {
                if sender.send(Renderer::new(device, Quality::LIVE)).is_ok() {
                    waker.request_repaint();
                }
            });
            self.drawing = Drawing::Building(receiver);
        }
        if let Drawing::Building(receiver) = &self.drawing
            && let Ok(renderer) = receiver.try_recv()
        {
            self.drawing = Drawing::Ready(Box::new(renderer));
        }

        let mut card = Duration::ZERO;
        match (&mut self.drawing, gpu) {
            (Drawing::Ready(renderer), Some(gpu)) => {
                let look = Look::of(song, &camera, &heard, self.frames);
                let picture = self.stage.paint(song, &camera, &heard, format, 1, look);
                let painted = Instant::now();
                renderer.draw(&picture);
                card = painted.elapsed();
                if let (Some(view), Some(size)) = (renderer.finished(), renderer.finished_size()) {
                    let texture = show_texture(gpu, &mut self.shown, view, size);
                    let whole = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
                    ui.painter().image(texture, rect, whole, Color32::WHITE);
                }
            }
            _ => plain(
                &ctx,
                self.stage.theme(),
                rect,
                &Moment { score: &song.score, plan: &song.plan, camera: &camera, heard: &heard },
            ),
        }
        self.hint(ui, rect, started);
        // The picture moves with the song, and its light fades after it.
        if live.playing || live.meters_moving() {
            ctx.request_repaint();
        }
        self.costs.add(started, live.playing, card);
    }

    /// The words that say how to leave, for a moment after the stage is
    /// opened or the pointer moves; and while they are away, no pointer.
    fn hint(&self, ui: &Ui, rect: Rect, now: Instant) {
        let since = self.stirred.map_or(HINT_FOR, |stirred| now.duration_since(stirred));
        if since >= HINT_FOR {
            ui.ctx().set_cursor_icon(egui::CursorIcon::None);
            return;
        }
        ui.ctx().request_repaint_after(HINT_FOR - since);
        let left = (HINT_FOR - since).as_secs_f32();
        let shown = (left / 0.6).min(1.0);
        let theme = self.stage.theme();
        let style = theme.text.micro;
        let words = "F11 or Esc leaves the stage \u{00B7} Space plays";
        let painter = ui.painter();
        let width = text::width(painter, style, words) + 28.0;
        let plate = Rect::from_center_size(pos2(rect.center().x, rect.top() + 26.0), vec2(width, 24.0));
        painter.rect_filled(plate, 0.0, fade(theme.panel, 0.92 * shown));
        trill_noir::paint::frame(painter, plate, fade(theme.hairline, shown));
        text::paint(painter, plate, Align2::CENTER_CENTER, style, words, fade(theme.ink_dim, shown));
    }

    /// Lets go of what the stage holds on the graphics card, once it is
    /// no longer shown. The passes are kept: opening the stage again is
    /// then immediate.
    pub fn closed(&mut self, gpu: Option<&egui_wgpu::RenderState>) {
        if !self.was_open {
            return;
        }
        self.was_open = false;
        if let (Some((texture, _)), Some(gpu)) = (self.shown.take(), gpu) {
            gpu.renderer.write().free_texture(&texture);
        }
        if let Drawing::Ready(renderer) = &mut self.drawing {
            renderer.release();
        }
        self.clock = Clock::default();
    }

    /// What the stage cost while it was last open and the song played, for
    /// a smoke run to say.
    pub fn report(&self) -> Option<String> {
        let costs = &self.costs;
        if costs.frames == 0 || costs.played.is_zero() {
            return None;
        }
        let each = |total: Duration| total.as_secs_f64() * 1000.0 / costs.frames as f64;
        Some(format!(
            "stage: {} frames while the song played, {:.1} a second, {} of them late (the longest wait {:.1} ms); \
             its part of a frame {:.2} ms on average ({:.2} ms of it handing the picture to the graphics card), \
             {:.2} ms at worst; {}",
            costs.frames,
            costs.frames as f64 / costs.played.as_secs_f64(),
            costs.late,
            costs.longest.as_secs_f64() * 1000.0,
            each(costs.spent),
            each(costs.card),
            costs.worst.as_secs_f64() * 1000.0,
            match &self.drawing {
                Drawing::Ready(renderer) => format!("drawn on {}", renderer.gpu().name),
                _ => "painted plain".to_string(),
            }
        ))
    }
}

/// Has the window's renderer show the stage's finished picture, and
/// returns what to paint it as. The picture is a new texture whenever its
/// size changes.
fn show_texture(
    gpu: &egui_wgpu::RenderState,
    shown: &mut Option<(egui::TextureId, [u32; 2])>,
    view: &wgpu::TextureView,
    size: [u32; 2],
) -> egui::TextureId {
    match shown {
        Some((texture, was)) if *was == size => *texture,
        Some((texture, was)) => {
            gpu.renderer.write().update_egui_texture_from_wgpu_texture(
                &gpu.device,
                view,
                wgpu::FilterMode::Linear,
                *texture,
            );
            *was = size;
            *texture
        }
        None => {
            let texture = gpu.renderer.write().register_native_texture(&gpu.device, view, wgpu::FilterMode::Linear);
            *shown = Some((texture, size));
            texture
        }
    }
}

/// Paints the stage's interface straight into the window, without its
/// light and its focus: what is shown until the passes are built, and
/// where there is nothing to build them on.
fn plain(ctx: &egui::Context, theme: &Theme, rect: Rect, moment: &Moment<'_>) {
    let layer = LayerId::new(Order::Middle, Id::unique("stage-plain"));
    // The stage is composed on a short side of 720 points whatever the
    // window's size; the layer is enlarged to fill the window.
    let enlarge = rect.width().min(rect.height()) / Format::SHORT_SIDE;
    let size = rect.size() / enlarge;
    ctx.set_transform_layer(layer, egui::emath::TSTransform::new(rect.min.to_vec2(), enlarge));
    let composed = Rect::from_min_size(pos2(0.0, 0.0), size);
    let base = ctx.layer_painter(layer).with_clip_rect(composed);
    // The light and the focus have nowhere to go.
    let nowhere = base.with_clip_rect(Rect::NOTHING);
    let canvas = Canvas { base, light: nowhere.clone(), focus: nowhere, theme };
    scene::paint(&canvas, moment, size);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_clock_runs_between_reports_and_is_held_to_them() {
        let mut clock = Clock::default();
        let start = Instant::now();
        let frame = Duration::from_micros(16_667);
        // 120 BPM: two beats a second. The session reports in steps of a
        // tenth of a beat; the clock moves a thirtieth every frame.
        assert_eq!(clock.beat(8.0, true, 120.0, start), 8.0);
        let mut before = 8.0;
        for index in 1..=120u32 {
            let truth = 8.0 + index as f64 / 30.0;
            let reported = (truth * 10.0).floor() / 10.0;
            let shown = clock.beat(reported, true, 120.0, start + frame * index);
            let step = shown - before;
            assert!(step > 0.02 && step < 0.045, "frame {index}: moved {step}");
            assert!((shown - truth).abs() < 0.12, "frame {index}: {shown} for {truth}");
            before = shown;
        }
    }

    #[test]
    fn the_clock_jumps_where_the_song_does_and_stops_with_it() {
        let mut clock = Clock::default();
        let start = Instant::now();
        clock.beat(8.0, true, 120.0, start);
        // The song was sent to bar 17.
        let after = clock.beat(64.0, true, 120.0, start + Duration::from_millis(16));
        assert_eq!(after, 64.0);
        // Stopped, it is where the session says, exactly.
        assert_eq!(clock.beat(64.37, false, 120.0, start + Duration::from_millis(32)), 64.37);
        // And a long pause in the frames is not run through.
        clock.beat(70.0, true, 120.0, start + Duration::from_millis(48));
        let late = clock.beat(70.1, true, 120.0, start + Duration::from_secs(5));
        assert!(late < 70.7, "{late}");
    }
}
