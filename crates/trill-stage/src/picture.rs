//! What the painter hands the graphics card: the three layers of one
//! moment, and the numbers the passes over them run with.

use egui::epaint::ClippedPrimitive;
use egui::{Color32, TexturesDelta};
use parking_lot::Mutex;
use std::sync::Arc;

/// The size of a picture, in pixels.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct Format {
    pub width: u32,
    pub height: u32,
}

impl Format {
    /// 1920 by 1080.
    pub const HD: Self = Self { width: 1920, height: 1080 };
    /// 3840 by 2160.
    pub const UHD: Self = Self { width: 3840, height: 2160 };
    /// 1080 by 1920, for a phone held upright.
    pub const UPRIGHT: Self = Self { width: 1080, height: 1920 };

    /// The shorter side of every picture is this many points: the stage
    /// is composed once, and a larger picture is the same one, finer.
    pub const SHORT_SIDE: f32 = 720.0;

    pub fn new(width: u32, height: u32) -> Self {
        Self { width: width.max(16), height: height.max(16) }
    }

    /// How many pixels a point of the composition takes.
    pub fn pixels_per_point(self) -> f32 {
        self.width.min(self.height) as f32 / Self::SHORT_SIDE
    }

    /// The size of the composition, in points.
    pub fn points(self) -> egui::Vec2 {
        egui::vec2(self.width as f32, self.height as f32) / self.pixels_per_point()
    }

    /// Wider than high.
    pub fn is_wide(self) -> bool {
        self.width >= self.height
    }
}

/// The numbers the passes over a picture run with. Lengths are in points
/// of the composition, so a picture of any size looks the same.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Look {
    /// How strongly what the light layer holds is added to the picture: at
    /// 1 a lit thing is as bright again as it was painted.
    pub light: f32,
    /// How much of the light's bloom is added.
    pub glow: f32,
    /// How far the bloom reaches, 0..1.
    pub spread: f32,
    /// The radius of the blur of what is furthest out of focus.
    pub blur: f32,
    /// What the whole picture is multiplied by.
    pub exposure: f32,
    /// Light let in across the top of the frame (a crash), and its color.
    pub flare: f32,
    pub flare_color: [f32; 3],
    /// How much the picture is enlarged about its middle (a kick): 0.01 is
    /// one part in a hundred.
    pub punch: f32,
    /// How far red and blue are pulled apart sideways (a snare).
    pub tear: f32,
    /// How much darker the corners are than the middle, 0..1.
    pub vignette: f32,
    /// The strength of the grain.
    pub grain: f32,
    /// A number that differs from frame to frame, for the grain.
    pub seed: u32,
}

impl Look {
    /// The interface as it is: nothing added and nothing taken away.
    pub fn plain() -> Self {
        Self {
            light: 0.0,
            glow: 0.0,
            spread: 0.0,
            blur: 0.0,
            exposure: 1.0,
            flare: 0.0,
            flare_color: [0.0; 3],
            punch: 0.0,
            tear: 0.0,
            vignette: 0.0,
            grain: 0.0,
            seed: 0,
        }
    }
}

/// What the font's texture has gained that no renderer has taken in yet.
///
/// A stage and the pictures it paints share one of these. The renderer
/// that draws a picture takes in whatever is waiting, so a picture that is
/// painted and never drawn loses nothing: the next one drawn brings it.
/// One stage's pictures go to one renderer.
#[derive(Clone, Default)]
pub struct Textures(Arc<Mutex<Waiting>>);

#[derive(Default)]
struct Waiting(TexturesDelta);

impl Drop for Waiting {
    fn drop(&mut self) {
        // With the stage and its pictures gone there is no one to give
        // them to; egui wants that said.
        self.0.clear();
    }
}

impl Textures {
    pub(crate) fn add(&self, delta: TexturesDelta) {
        self.0.lock().0.append(delta);
    }

    /// Hands what is waiting to `take`, and keeps none of it.
    pub(crate) fn hand_over(&self, take: impl FnOnce(&TexturesDelta)) {
        let mut waiting = self.0.lock();
        take(&waiting.0);
        waiting.0.clear();
    }

    /// Whether nothing is waiting.
    pub fn is_empty(&self) -> bool {
        self.0.lock().0.is_empty()
    }
}

/// One moment of the stage, painted and ready to be drawn.
pub struct Picture {
    /// The interface.
    pub base: Vec<ClippedPrimitive>,
    /// What emits light, over black.
    pub light: Vec<ClippedPrimitive>,
    /// How far out of focus each part of the picture is: black is sharp,
    /// white as soft as [`Look::blur`].
    pub focus: Vec<ClippedPrimitive>,
    /// What the font's texture has gained since a picture was last drawn.
    pub textures: Textures,
    /// The size of the finished picture.
    pub format: Format,
    /// How many times larger each way the layers are drawn than the
    /// finished picture.
    pub oversample: u32,
    /// The color under everything.
    pub background: Color32,
    pub look: Look,
}

impl Picture {
    /// The size the layers are drawn at, in pixels.
    pub fn drawn_size(&self) -> [u32; 2] {
        [self.format.width * self.oversample, self.format.height * self.oversample]
    }

    /// Pixels per point of the layers as drawn.
    pub fn drawn_pixels_per_point(&self) -> f32 {
        self.format.pixels_per_point() * self.oversample as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_format_is_composed_on_a_short_side_of_720_points() {
        assert_eq!(Format::HD.pixels_per_point(), 1.5);
        assert_eq!(Format::UHD.pixels_per_point(), 3.0);
        assert_eq!(Format::UPRIGHT.pixels_per_point(), 1.5);
        assert_eq!(Format::HD.points(), egui::vec2(1280.0, 720.0));
        assert_eq!(Format::UHD.points(), Format::HD.points(), "a larger picture is the same one, finer");
        assert_eq!(Format::UPRIGHT.points(), egui::vec2(720.0, 1280.0));
        assert!(Format::HD.is_wide() && !Format::UPRIGHT.is_wide());
    }
}
