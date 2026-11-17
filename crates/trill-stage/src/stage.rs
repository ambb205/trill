//! The stage itself: give it a song and a time, get a picture.
//!
//! A [`Stage`] owns an egui context of its own, with the kit's type and
//! theme installed. Painting a moment runs one pass of that context, takes
//! what was painted out of its three layers and tessellates each: the
//! window's own context and renderer never see any of it, which is what
//! lets the passes of [`crate::gpu`] run over the interface, and lets a
//! film be painted on a thread of its own while the window goes on.

use crate::camera::Camera;
use crate::director::Plan;
use crate::light;
use crate::picture::{Format, Look, Picture, Textures};
use crate::scene::{self, Canvas, Layout, Moment};
use crate::score::Score;
use crate::sound::Heard;
use egui::epaint::ClippedShape;
use egui::{Id, LayerId, Order};
use std::num::NonZeroUsize;
use trill_core::{Beats, Project};
use trill_noir::Theme;

/// The widest blur, as a radius in points.
const BLUR: f32 = 7.5;
/// How much a kick enlarges the picture, how far a snare pulls its colors
/// apart (in points) and how much light a crash lets in.
const KICK_PUNCH: f32 = 0.011;
const SNARE_TEAR: f32 = 1.6;
const CRASH_FLARE: f32 = 0.022;
/// How much brighter the frame of a cut is, and how fast that falls away,
/// in beats.
const CUT_FLASH: f32 = 0.24;
const CUT_FALL: f64 = 0.35;

/// A song as the stage films it: read once, directed once.
#[derive(Clone, Debug, PartialEq)]
pub struct Song {
    pub score: Score,
    pub plan: Plan,
}

impl Song {
    /// Reads and directs `project`, taking a track for drums where it says
    /// so or plays through a built-in drum instrument.
    pub fn of(project: &Project) -> Self {
        Self::from(Score::read(project))
    }

    /// The camera at `beat`, for a picture of `format`.
    pub fn camera(&self, beat: Beats, format: Format) -> Camera {
        Camera::at(&self.score, &self.plan, beat, Layout::axis_ratio(format.points()))
    }
}

impl From<Score> for Song {
    fn from(score: Score) -> Self {
        let plan = Plan::direct(&score);
        Self { score, plan }
    }
}

impl Look {
    /// The look of a moment. What was heard sets how bright the picture
    /// is and how much it glows; the song's own hits set what lands on a
    /// beat: the punch of a kick, the tear of a snare, the flare of a
    /// crash, the flash of a cut. `frame` is the number of the frame, which
    /// the grain is drawn from.
    pub fn of(song: &Song, camera: &Camera, heard: &Heard, frame: u32) -> Self {
        let (score, beat) = (&song.score, camera.beat);
        let hits = &score.hits;
        let kick = light::strike(&hits.kicks, beat, score.tempo, light::KICK_FALL);
        let snare = light::strike(&hits.snares, beat, score.tempo, light::SNARE_FALL);
        let crash = light::strike(&hits.crashes, beat, score.tempo, light::CRASH_FALL);
        let cut = camera.since_cut.map_or(0.0, |since| (-since / CUT_FALL).exp() as f32);
        Self {
            light: 1.5 + 1.1 * heard.punch + 0.6 * kick,
            glow: 0.5 + 0.55 * heard.level + 0.5 * crash,
            spread: 0.8,
            blur: BLUR,
            exposure: 0.86 + 0.16 * heard.level + CUT_FLASH * cut,
            shoulder: 0.72,
            flare: CRASH_FLARE * crash,
            flare_color: [0.92, 0.9, 1.0],
            punch: KICK_PUNCH * kick,
            tear: SNARE_TEAR * snare,
            vignette: 0.42,
            grain: 0.03,
            seed: frame,
        }
    }
}

/// Paints moments of songs.
pub struct Stage {
    ctx: egui::Context,
    theme: Theme,
    /// What the font's texture gained and no renderer has taken in yet.
    textures: Textures,
}

impl Stage {
    pub fn new(theme: Theme) -> Self {
        let ctx = egui::Context::default();
        trill_noir::install(&ctx, &theme);
        // One pass a frame: the stage asks for no second one, and must not
        // be painted twice into its layers.
        ctx.options_mut(|options| options.max_passes = NonZeroUsize::MIN);
        Self { ctx, theme, textures: Textures::default() }
    }

    pub fn theme(&self) -> &Theme {
        &self.theme
    }

    /// Paints `song` at `beat` as `heard`, for a picture of `format` drawn
    /// `oversample` times larger each way. `frame` numbers the picture.
    ///
    /// The pictures of one stage go to one renderer: each brings what the
    /// font's texture has gained since that renderer last drew one.
    pub fn picture(
        &mut self,
        song: &Song,
        heard: &Heard,
        beat: Beats,
        format: Format,
        oversample: u32,
        frame: u32,
    ) -> Picture {
        let camera = song.camera(beat, format);
        let look = Look::of(song, &camera, heard, frame);
        self.paint(song, &camera, heard, format, oversample, look)
    }

    /// Paints what `camera` sees, with `look`.
    pub fn paint(
        &mut self,
        song: &Song,
        camera: &Camera,
        heard: &Heard,
        format: Format,
        oversample: u32,
        look: Look,
    ) -> Picture {
        let oversample = oversample.clamp(1, 2);
        let size = format.points();
        let scale = format.pixels_per_point() * oversample as f32;
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
            max_texture_side: Some(8192),
            viewports: std::iter::once((
                egui::ViewportId::ROOT,
                egui::ViewportInfo { native_pixels_per_point: Some(scale), ..Default::default() },
            ))
            .collect(),
            ..Default::default()
        };
        let layers = [Order::Background, Order::Middle, Order::Foreground]
            .map(|order| LayerId::new(order, Id::unique("trill-stage").with(order as u8)));
        let mut painted: [Vec<ClippedShape>; 3] = Default::default();
        let ctx = self.ctx.clone();
        let theme = &self.theme;
        let output = ctx.run_ui(input, |ui| {
            let ctx = ui.ctx();
            let [base, light, focus] = layers.map(|layer| ctx.layer_painter(layer));
            let canvas = Canvas { base, light, focus, theme };
            let moment = Moment { score: &song.score, plan: &song.plan, camera, heard };
            scene::paint(&canvas, &moment, size);
            // Out of the context before it would mix them into one list.
            for (layer, shapes) in layers.iter().zip(&mut painted) {
                *shapes = ctx.graphics_mut(|graphics| {
                    graphics
                        .get_mut(*layer)
                        .map(std::mem::take)
                        .map_or_else(Vec::new, |list| list.all_entries().cloned().collect())
                });
            }
        });
        let [base, light, focus] = painted.map(|shapes| ctx.tessellate(shapes, scale));
        self.textures.add(output.textures_delta);
        let textures = self.textures.clone();
        Picture { base, light, focus, textures, format, oversample, background: self.theme.bg, look }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::score::Hit;
    use egui::epaint::Primitive;
    use trill_core::{Note, Role};

    /// Sixteen bars: a kick on every beat and a line of eighths.
    fn song() -> Song {
        let mut project = Project::new("Test Song");
        let drums = project.add_track("Drums");
        let clip = project.add_midi_clip(drums, "beat", 0.0, 64.0).unwrap();
        let kicks: Vec<Note> =
            (0..64).map(|beat| Note { pitch: 36, start: beat as f64, length: 0.25, velocity: 110 }).collect();
        project.clip_mut(clip).unwrap().add_notes(&kicks).unwrap();
        let profile = trill_core::InstrumentProfile { role: Some(Role::Drums), ..Default::default() };
        project.set_track_profile(drums, profile).unwrap();
        let lead = project.add_track("Lead");
        let clip = project.add_midi_clip(lead, "line", 0.0, 64.0).unwrap();
        let line: Vec<Note> = (0..128)
            .map(|step| Note { pitch: 60 + (step % 7) as u8, start: step as f64 * 0.5, length: 0.45, velocity: 100 })
            .collect();
        project.clip_mut(clip).unwrap().add_notes(&line).unwrap();
        project.add_marker(0.0, "Intro").unwrap();
        project.add_marker(32.0, "Verse").unwrap();
        Song::of(&project)
    }

    fn vertices(primitives: &[egui::ClippedPrimitive]) -> usize {
        primitives
            .iter()
            .map(|clipped| match &clipped.primitive {
                Primitive::Mesh(mesh) => mesh.vertices.len(),
                Primitive::Callback(_) => 0,
            })
            .sum()
    }

    #[test]
    fn a_moment_is_painted_into_three_layers() {
        let song = song();
        let mut stage = Stage::new(Theme::noir());
        let picture = stage.picture(&song, &Heard::silence(), 40.0, Format::HD, 1, 0);
        assert!(vertices(&picture.base) > 1000, "the interface: {}", vertices(&picture.base));
        assert!(vertices(&picture.light) > 0, "the playhead and the notes that sound");
        assert!(vertices(&picture.light) < vertices(&picture.base) / 4, "little of the picture emits light");
        assert!(vertices(&picture.focus) > 0);
        assert_eq!((picture.drawn_size(), picture.drawn_pixels_per_point()), ([1920, 1080], 1.5));
        // The first picture brings the font's texture with it.
        assert!(!picture.textures.is_empty());
        let larger = stage.picture(&song, &Heard::silence(), 40.0, Format::HD, 2, 1);
        assert_eq!((larger.drawn_size(), larger.drawn_pixels_per_point()), ([3840, 2160], 3.0));
    }

    #[test]
    fn the_same_moment_is_the_same_picture() {
        let song = song();
        let mut stage = Stage::new(Theme::noir());
        let heard = Heard { level: 0.7, punch: 0.4, tracks: Vec::new() };
        // Painted once to settle the font's texture, which is not part of
        // the comparison.
        stage.picture(&song, &heard, 40.25, Format::HD, 1, 7);
        let first = stage.picture(&song, &heard, 40.25, Format::HD, 1, 7);
        let second = stage.picture(&song, &heard, 40.25, Format::HD, 1, 7);
        for (a, b) in [(&first.base, &second.base), (&first.light, &second.light), (&first.focus, &second.focus)] {
            assert_eq!(a.len(), b.len());
            for (a, b) in a.iter().zip(b.iter()) {
                assert_eq!(a.clip_rect, b.clip_rect);
                let (Primitive::Mesh(a), Primitive::Mesh(b)) = (&a.primitive, &b.primitive) else {
                    panic!("only meshes")
                };
                assert!(a.vertices == b.vertices && a.indices == b.indices);
            }
        }
        assert_eq!(first.look, second.look);
        // And another stage paints it the same: nothing is carried from
        // one picture to the next.
        let mut other = Stage::new(Theme::noir());
        other.picture(&song, &heard, 3.0, Format::HD, 1, 0);
        let elsewhere = other.picture(&song, &heard, 40.25, Format::HD, 1, 7);
        assert_eq!(vertices(&elsewhere.base), vertices(&first.base));
        assert_eq!(vertices(&elsewhere.light), vertices(&first.light));
    }

    #[test]
    fn more_lights_while_a_note_sounds_than_between_notes() {
        let song = song();
        let mut stage = Stage::new(Theme::noir());
        // On a kick and an eighth; and late in the gap after them, when
        // the kick has gone out and the line's note has ended.
        let on = stage.picture(&song, &Heard::silence(), 41.0, Format::HD, 1, 0);
        let between = stage.picture(&song, &Heard::silence(), 41.49, Format::HD, 1, 0);
        assert!(
            vertices(&on.light) > vertices(&between.light),
            "{} then {}",
            vertices(&on.light),
            vertices(&between.light)
        );
    }

    #[test]
    fn a_kick_moves_the_picture_on_the_kick_and_the_sound_sets_its_mood() {
        let song = song();
        assert_eq!(song.score.hits.kicks[8], Hit { beat: 8.0, hard: 110.0 / 127.0 });
        let look = |beat: Beats, heard: &Heard| Look::of(&song, &song.camera(beat, Format::HD), heard, 0);
        let quiet = Heard::silence();
        assert_eq!(
            look(39.999, &quiet).punch,
            0.0 + look(39.999, &quiet).punch.min(0.0002),
            "all but gone before the next"
        );
        let on = look(40.0, &quiet);
        assert!((on.punch - KICK_PUNCH * 110.0 / 127.0).abs() < 1e-6, "on the kick: {}", on.punch);
        assert!(look(40.2, &quiet).punch < on.punch * 0.5, "and falling away after it");
        assert_eq!(on.tear, 0.0, "there is no snare in this song");

        // The same moment, heard loud: brighter and with more glow, and
        // the punch is where it was.
        let loud = Heard { level: 1.0, punch: 1.0, tracks: Vec::new() };
        let lit = look(40.0, &loud);
        assert!(lit.exposure > on.exposure && lit.glow > on.glow && lit.light > on.light);
        assert_eq!(lit.punch, on.punch);
    }

    #[test]
    fn an_upright_picture_is_composed_for_its_shape() {
        let song = song();
        let mut stage = Stage::new(Theme::noir());
        let upright = stage.picture(&song, &Heard::silence(), 40.0, Format::UPRIGHT, 1, 0);
        assert_eq!(upright.drawn_size(), [1080, 1920]);
        let (wide, tall) = (song.camera(40.0, Format::HD), song.camera(40.0, Format::UPRIGHT));
        assert!(tall.window < wide.window * 0.6, "fewer beats across: {} against {}", tall.window, wide.window);
        assert!(vertices(&upright.base) > 500);
    }
}
