//! Track colors: which palette hue a track is drawn in.
//!
//! A project stores each track's color as RGB. The window never paints that
//! value directly; it paints the theme's hue that stands for it, so projects
//! made before the palette existed (or by another program) still look like
//! part of the design, and a project file is never rewritten just to be
//! shown.

use daw_noir::{TRACK_HUES, Theme};
use eframe::egui::Color32;

/// The colors `daw_core::Project::add_track` hands out, in order.
///
/// They say "the nth track", not "this hue": two of the ten are greens of
/// one hue and two are purples. Mapped by hue they would collapse onto
/// seven palette entries, so they are mapped by position instead and ten
/// tracks stay ten colors.
const POSITIONAL: [u32; TRACK_HUES] =
    [0xF2994A, 0x56CCF2, 0xBB6BD9, 0x6FCF97, 0xEB5757, 0xF2C94C, 0x2D9CDB, 0x9B51E0, 0x27AE60, 0xE27AB0];

fn unpack(rgb: u32) -> Color32 {
    Color32::from_rgb((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
}

fn pack(color: Color32) -> u32 {
    (color.r() as u32) << 16 | (color.g() as u32) << 8 | color.b() as u32
}

/// The hue to draw a track in, given the color stored on it.
pub fn track_color(theme: &Theme, stored: u32) -> Color32 {
    // Compared against Noir's hues whatever the theme, since those are what
    // `new_track_color` stores: a track made in one theme keeps its place
    // in the palette of another.
    let noir = Theme::noir();
    let stored_hue = unpack(stored);
    let index = POSITIONAL
        .iter()
        .position(|legacy| *legacy == stored)
        .or_else(|| noir.tracks.iter().position(|hue| *hue == stored_hue))
        .or_else(|| {
            let nearest = noir.nearest_track_hue(stored_hue);
            noir.tracks.iter().position(|hue| *hue == nearest)
        });
    index.map_or(stored_hue, |index| theme.track_hue(index))
}

/// The color to store on the `index`th track of a project when the window
/// creates it: the next hue of the palette.
pub fn new_track_color(index: usize) -> u32 {
    pack(Theme::noir().track_hue(index))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors_core_assigns_by_position_keep_their_position() {
        let noir = Theme::noir();
        let shown: Vec<Color32> = POSITIONAL.iter().map(|stored| track_color(&noir, *stored)).collect();
        assert_eq!(shown, noir.tracks, "the nth default color is the nth hue");

        // A project made by daw-core alone gets these same colors.
        let mut project = daw_core::Project::new("t");
        for index in 0..TRACK_HUES + 1 {
            let track = project.add_track(format!("Track {index}"));
            assert_eq!(project.track(track).unwrap().color, POSITIONAL[index % TRACK_HUES], "track {index}");
        }
    }

    #[test]
    fn new_tracks_store_palette_hues_that_show_as_themselves() {
        let noir = Theme::noir();
        for index in 0..TRACK_HUES * 2 {
            let stored = new_track_color(index);
            assert_eq!(track_color(&noir, stored), noir.track_hue(index));
        }
        assert_eq!(new_track_color(0), 0xA87FD1, "violet first");
    }

    #[test]
    fn other_colors_take_the_nearest_hue() {
        let noir = Theme::noir();
        let [_violet, sky, _teal, sage, _amber, rose, slate, ..] = noir.tracks;
        assert_eq!(track_color(&noir, 0x00FF40), sage);
        assert_eq!(track_color(&noir, 0x1E90FF), sky);
        assert_eq!(track_color(&noir, 0xFF4F9A), rose);
        assert_eq!(track_color(&noir, 0x777777), slate);
    }

    #[test]
    fn a_track_keeps_its_palette_place_in_another_theme() {
        let (noir, graphite) = (Theme::noir(), Theme::graphite());
        for (index, hue) in graphite.tracks.into_iter().enumerate() {
            assert_eq!(track_color(&graphite, new_track_color(index)), hue);
            assert_eq!(track_color(&graphite, POSITIONAL[index]), hue);
        }
        assert_eq!(track_color(&graphite, 0x00FF40), graphite.tracks[3], "nearest hue, then that hue's place");
        assert_ne!(noir.tracks, graphite.tracks);
    }
}
