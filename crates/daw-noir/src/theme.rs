//! Design tokens: every color, type size and metric the interface uses.
//!
//! A [`Theme`] is plain data, so the look is swapped by handing the widgets
//! another one. [`Theme::noir`] is Instrument Noir as `core/tokens.json` of
//! the kit defines it, at the type sizes this app settled on (nothing below
//! 10 px). [`Theme::graphite`] keeps the geometry and trades the palette
//! for mid-gray surfaces with dark ink.
//!
//! Legibility is part of the contract and is enforced by this module's
//! tests: every ink reaches WCAG AA (4.5:1) on the surfaces it is allowed
//! on, and text drawn on an accent fill does too.

use egui::Color32;

/// How many track hues a theme provides.
pub const TRACK_HUES: usize = 10;

/// A text style: size in points, letter spacing, and how the text is set.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct TextStyle {
    pub size: f32,
    /// Extra space between letters, in points.
    pub tracking: f32,
    /// Set in capitals.
    pub upper: bool,
    /// Give every digit the same width, so columns of numbers line up and
    /// a value does not jitter while it changes.
    pub tabular: bool,
}

/// The type scale.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Type {
    /// Badges and status text.
    pub micro: TextStyle,
    /// Labels, tabs, headers, chips.
    pub caps: TextStyle,
    /// Labels inside bars, where the value beside them needs the room.
    pub label: TextStyle,
    /// Numbers.
    pub value: TextStyle,
    /// Names and lists.
    pub body: TextStyle,
    /// The large numbers of the transport.
    pub readout: TextStyle,
    /// The wordmark.
    pub brand: TextStyle,
}

/// Sizes of the kit's parts, in points.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Metrics {
    /// Height of a Bar.
    pub bar: f32,
    /// Height of a Chip.
    pub chip: f32,
    /// Horizontal padding inside a chip.
    pub chip_pad: f32,
    /// Height of buttons, menu items and text fields.
    pub control: f32,
    /// Height of a row of tabs.
    pub tab: f32,
    /// Height of a section header, rule included.
    pub section: f32,
    /// Gap between stacked controls.
    pub gap: f32,
    /// Padding between a control's edge and its text.
    pub pad: f32,
    /// Side of a status dot.
    pub dot: f32,
    /// Width of a scroll bar.
    pub scroll_bar: f32,
}

/// The palette, the type scale and the metrics.
#[derive(Clone, Debug, PartialEq)]
pub struct Theme {
    pub name: &'static str,
    pub dark: bool,

    // ---- surfaces --------------------------------------------------------
    /// The window, lanes and wells.
    pub bg: Color32,
    /// Bars, headers, the browser and device panels.
    pub panel: Color32,
    /// A selected row.
    pub raised: Color32,

    // ---- ink -------------------------------------------------------------
    pub ink: Color32,
    /// Secondary text. Legible on every surface, washes included.
    pub ink_dim: Color32,
    /// Quiet labels. Legible on `bg` and `panel` only: see [`Self::quiet_on`].
    pub ink_faint: Color32,
    /// Decoration (ticks, inactive marks). Never text.
    pub ghost: Color32,

    // ---- lines and washes, translucent over whatever is behind them -------
    pub hairline: Color32,
    /// Bar lines.
    pub grid: Color32,
    /// Lines between bar lines.
    pub grid_fine: Color32,
    /// The filled part of a Bar.
    pub fill: Color32,
    pub hover: Color32,
    /// Pressed, selected and "on".
    pub selected: Color32,
    /// The empty part of a meter.
    pub well: Color32,
    /// Darkens rows that should step back (black-key rows, time past a
    /// clip's end).
    pub shade: Color32,
    /// Behind a modal dialog.
    pub backdrop: Color32,

    // ---- accents ---------------------------------------------------------
    /// The one loud color: the playhead, alerts, meter overs.
    pub signal: Color32,
    pub ok: Color32,
    pub warn: Color32,
    pub info: Color32,
    /// Measurements that are neither good nor bad (CPU load).
    pub data: Color32,
    /// Track hues, in the order new tracks take them.
    pub tracks: [Color32; TRACK_HUES],
    /// Text on a track hue or another accent fill.
    pub on_accent: Color32,

    // ---- the piano -------------------------------------------------------
    pub key_white: Color32,
    pub key_black: Color32,
    /// Lines between keys and the note names on the white ones.
    pub key_ink: Color32,

    pub text: Type,
    pub metric: Metrics,
}

const fn rgb(hex: u32) -> Color32 {
    Color32::from_rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

/// White at `opacity` (0..=1), for washes over dark surfaces.
fn white(opacity: f32) -> Color32 {
    Color32::from_white_alpha((opacity * 255.0).round() as u8)
}

/// Black at `opacity` (0..=1).
fn black(opacity: f32) -> Color32 {
    Color32::from_black_alpha((opacity * 255.0).round() as u8)
}

const TYPE: Type = Type {
    micro: TextStyle { size: 10.0, tracking: 0.6, upper: true, tabular: false },
    caps: TextStyle { size: 11.0, tracking: 1.1, upper: true, tabular: false },
    label: TextStyle { size: 10.0, tracking: 1.0, upper: true, tabular: false },
    value: TextStyle { size: 11.5, tracking: 0.4, upper: false, tabular: true },
    body: TextStyle { size: 13.0, tracking: 0.0, upper: false, tabular: false },
    readout: TextStyle { size: 14.0, tracking: 0.6, upper: false, tabular: true },
    brand: TextStyle { size: 10.0, tracking: 2.4, upper: true, tabular: false },
};

const METRICS: Metrics = Metrics {
    bar: 18.0,
    chip: 18.0,
    chip_pad: 5.0,
    control: 24.0,
    tab: 24.0,
    section: 16.0,
    gap: 3.0,
    pad: 5.0,
    dot: 5.0,
    scroll_bar: 8.0,
};

impl Theme {
    /// Instrument Noir: near-black surfaces, ink text, hairlines and washes.
    pub fn noir() -> Self {
        let (violet, sky, teal, sage) = (rgb(0xA87FD1), rgb(0x72A9D9), rgb(0x4EC9B0), rgb(0x8FC47E));
        let (amber, rose, slate) = (rgb(0xD9B45C), rgb(0xDE8AA4), rgb(0x8FA5BE));
        // Three hues beyond the kit's seven, at the lightness and chroma of
        // the others (OKLCH 0.70-0.76 / 0.09-0.11), in the widest gaps of
        // the hue circle, so ten tracks are told apart.
        let (clay, iris, aqua) = (rgb(0xDD9B75), rgb(0x9196E2), rgb(0x67C0D5));
        Self {
            name: "noir",
            dark: true,
            bg: rgb(0x101013),
            panel: rgb(0x16161A),
            raised: rgb(0x1C1C21),
            ink: rgb(0xECECF1),
            ink_dim: rgb(0xA3A3AB),
            ink_faint: rgb(0x7F7F88),
            ghost: rgb(0x3A3A43),
            hairline: white(0.09),
            grid: white(0.06),
            grid_fine: white(0.03),
            fill: white(0.07),
            hover: white(0.06),
            selected: white(0.10),
            well: white(0.05),
            shade: black(0.28),
            backdrop: black(0.6),
            signal: rgb(0xFF6B35),
            ok: sage,
            warn: amber,
            info: sky,
            data: teal,
            tracks: [violet, sky, teal, sage, amber, rose, slate, clay, iris, aqua],
            on_accent: rgb(0x101013),
            key_white: rgb(0xC9C9D1),
            key_black: rgb(0x0B0B0D),
            key_ink: rgb(0x101013),
            text: TYPE,
            metric: METRICS,
        }
    }

    /// Mid-gray surfaces with dark ink, after the default look of Ableton
    /// Live, in the same geometry. Accents are the Noir hues darkened until
    /// they carry on gray.
    pub fn graphite() -> Self {
        let (violet, sky, teal, sage) = (rgb(0x59317C), rgb(0x1A4774), rgb(0x0A4E40), rgb(0x244E13));
        let (amber, rose, slate) = (rgb(0x574108), rgb(0x782445), rgb(0x354455));
        let (clay, iris, aqua) = (rgb(0x713408), rgb(0x3D3C86), rgb(0x0E4B5B));
        Self {
            name: "graphite",
            dark: false,
            bg: rgb(0xB4B4B4),
            panel: rgb(0xC6C6C6),
            raised: rgb(0xD4D4D4),
            ink: rgb(0x141416),
            ink_dim: rgb(0x303034),
            ink_faint: rgb(0x454549),
            ghost: rgb(0x8E8E92),
            hairline: black(0.22),
            grid: black(0.14),
            grid_fine: black(0.07),
            fill: black(0.10),
            hover: black(0.06),
            selected: black(0.14),
            well: black(0.12),
            shade: black(0.10),
            backdrop: black(0.45),
            signal: rgb(0x841D00),
            ok: sage,
            warn: amber,
            info: sky,
            data: teal,
            tracks: [violet, sky, teal, sage, amber, rose, slate, clay, iris, aqua],
            on_accent: rgb(0xF4F4F6),
            key_white: rgb(0xE6E6E8),
            key_black: rgb(0x2A2A2E),
            key_ink: rgb(0x121214),
            text: TYPE,
            metric: METRICS,
        }
    }

    /// The built-in theme called `name`, if there is one.
    pub fn named(name: &str) -> Option<Self> {
        Self::all().into_iter().find(|theme| theme.name == name)
    }

    /// Every built-in theme, the default first.
    pub fn all() -> Vec<Self> {
        vec![Self::noir(), Self::graphite()]
    }

    /// The quietest ink that still reads as text on `surface`: faint where
    /// it reaches 4.5:1 (the window and panels), dim on anything lighter (a
    /// selected row, a hovered control).
    pub fn quiet_on(&self, surface: Color32) -> Color32 {
        if contrast_ratio(self.ink_faint, surface) >= AA { self.ink_faint } else { self.ink_dim }
    }

    /// The hue for the `index`th track, cycling through the palette.
    pub fn track_hue(&self, index: usize) -> Color32 {
        self.tracks[index % TRACK_HUES]
    }

    /// The palette hue that stands in for an arbitrary color, such as one
    /// stored in a project made before this palette existed: the hue
    /// closest in angle, or the least chromatic one for grays.
    pub fn nearest_track_hue(&self, color: Color32) -> Color32 {
        if self.tracks.contains(&color) {
            return color;
        }
        let (chroma, hue) = chroma_and_hue(color);
        let neutral = self.tracks.iter().copied().min_by(|a, b| chroma_and_hue(*a).0.total_cmp(&chroma_and_hue(*b).0));
        if chroma < GRAY_CHROMA {
            return neutral.unwrap_or(color);
        }
        let distance = |candidate: &Color32| {
            let difference = (chroma_and_hue(*candidate).1 - hue).abs();
            difference.min(360.0 - difference)
        };
        // The neutral entry has a hue too, but nothing colorful should
        // collapse onto a gray.
        let colorful = self.tracks.iter().filter(|candidate| Some(**candidate) != neutral);
        colorful.copied().min_by(|a, b| distance(a).total_cmp(&distance(b))).unwrap_or(color)
    }
}

/// The contrast WCAG AA asks of text.
pub const AA: f32 = 4.5;
/// Colors with less OKLCH chroma than this read as gray.
const GRAY_CHROMA: f32 = 0.05;

fn linear(channel: u8) -> f32 {
    let c = channel as f32 / 255.0;
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

/// Relative luminance of an opaque color, 0 (black) to 1 (white).
pub fn relative_luminance(color: Color32) -> f32 {
    0.2126 * linear(color.r()) + 0.7152 * linear(color.g()) + 0.0722 * linear(color.b())
}

/// WCAG 2 contrast ratio of two opaque colors, 1.0 to 21.0.
pub fn contrast_ratio(a: Color32, b: Color32) -> f32 {
    let (la, lb) = (relative_luminance(a), relative_luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

/// `wash` composited over the opaque `surface`: the color that ends up on
/// screen. Washes are stored premultiplied, as egui blends them.
pub fn over(surface: Color32, wash: Color32) -> Color32 {
    let keep = 1.0 - wash.a() as f32 / 255.0;
    let channel = |under: u8, top: u8| (top as f32 + under as f32 * keep).round().min(255.0) as u8;
    Color32::from_rgb(channel(surface.r(), wash.r()), channel(surface.g(), wash.g()), channel(surface.b(), wash.b()))
}

/// `color` at `opacity` (0..=1) of its own.
pub fn fade(color: Color32, opacity: f32) -> Color32 {
    color.gamma_multiply(opacity.clamp(0.0, 1.0))
}

/// Chroma and hue angle (degrees) of a color in OKLCH, the space in which
/// equal steps look equal.
fn chroma_and_hue(color: Color32) -> (f32, f32) {
    let (r, g, b) = (linear(color.r()), linear(color.g()), linear(color.b()));
    let l = (0.412_221_46 * r + 0.536_332_55 * g + 0.051_445_995 * b).cbrt();
    let m = (0.211_903_5 * r + 0.680_699_5 * g + 0.107_396_96 * b).cbrt();
    let s = (0.088_302_46 * r + 0.281_718_85 * g + 0.629_978_7 * b).cbrt();
    let a = 1.977_998_5 * l - 2.428_592_2 * m + 0.450_593_7 * s;
    let b = 0.025_904_037 * l + 0.782_771_77 * m - 0.808_675_77 * s;
    (a.hypot(b), b.atan2(a).to_degrees().rem_euclid(360.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn each(check: impl Fn(&Theme)) {
        for theme in Theme::all() {
            check(&theme);
        }
    }

    #[test]
    fn inks_clear_wcag_aa_on_the_surfaces_they_are_used_on() {
        each(|t| {
            let surfaces = [("bg", t.bg), ("panel", t.panel), ("raised", t.raised)];
            for (name, surface) in surfaces {
                for (ink_name, ink) in [("ink", t.ink), ("ink_dim", t.ink_dim)] {
                    // Primary and secondary text also sit on hovered and
                    // selected controls.
                    for (wash_name, wash) in
                        [("plain", Color32::TRANSPARENT), ("hover", t.hover), ("selected", t.selected)]
                    {
                        let ratio = contrast_ratio(ink, over(surface, wash));
                        assert!(ratio >= AA, "{}: {ink_name} on {wash_name} {name} is {ratio:.2}:1", t.name);
                    }
                }
                let quiet = contrast_ratio(t.quiet_on(surface), surface);
                assert!(quiet >= AA, "{}: quiet text on {name} is {quiet:.2}:1", t.name);
            }
            for (name, surface) in [("bg", t.bg), ("panel", t.panel)] {
                let ratio = contrast_ratio(t.ink_faint, surface);
                assert!(ratio >= AA, "{}: ink_faint on {name} is {ratio:.2}:1", t.name);
            }
        });
    }

    #[test]
    fn faint_ink_gives_way_where_it_would_be_too_quiet() {
        let noir = Theme::noir();
        assert_eq!(noir.quiet_on(noir.panel), noir.ink_faint);
        assert_eq!(noir.quiet_on(noir.raised), noir.ink_dim, "a selected row is too light for faint text");
        assert_eq!(noir.quiet_on(over(noir.panel, noir.hover)), noir.ink_dim);
    }

    #[test]
    fn accents_carry_text_and_read_as_text() {
        each(|t| {
            let accents = [("signal", t.signal), ("ok", t.ok), ("warn", t.warn), ("info", t.info), ("data", t.data)];
            let tracks = t.tracks.iter().map(|hue| ("track", *hue));
            for (name, accent) in accents.into_iter().chain(tracks) {
                // A clip's name strip, a lit mute chip.
                let on_it = contrast_ratio(t.on_accent, accent);
                assert!(on_it >= AA, "{}: text on {name} {accent:?} is {on_it:.2}:1", t.name);
                // Status text and dots on the bars.
                for surface in [t.bg, t.panel, t.raised] {
                    let as_text = contrast_ratio(accent, surface);
                    assert!(as_text >= AA, "{}: {name} {accent:?} on {surface:?} is {as_text:.2}:1", t.name);
                }
            }
        });
    }

    #[test]
    fn nothing_is_set_below_ten_points() {
        each(|t| {
            let styles =
                [t.text.micro, t.text.caps, t.text.label, t.text.value, t.text.body, t.text.readout, t.text.brand];
            for style in styles {
                assert!(style.size >= 10.0, "{}: {style:?}", t.name);
            }
        });
    }

    #[test]
    fn the_ink_ramp_is_ordered_and_ghost_stays_decorative() {
        each(|t| {
            let towards_ink = |color| contrast_ratio(color, t.bg);
            assert!(towards_ink(t.ink) > towards_ink(t.ink_dim), "{}", t.name);
            assert!(towards_ink(t.ink_dim) > towards_ink(t.ink_faint), "{}", t.name);
            assert!(towards_ink(t.ink_faint) > towards_ink(t.ghost), "{}", t.name);
            assert!(towards_ink(t.ghost) < 3.0, "{}: ghost must read as structure, not text", t.name);
        });
    }

    #[test]
    fn piano_key_labels_are_legible() {
        each(|t| {
            assert!(contrast_ratio(t.key_ink, t.key_white) >= AA, "{}", t.name);
            assert!(contrast_ratio(t.key_white, t.key_black) >= 3.0, "{}: the two key colors must separate", t.name);
        });
    }

    #[test]
    fn track_hues_are_distinct_and_cycle() {
        each(|t| {
            for (index, hue) in t.tracks.iter().enumerate() {
                assert!(!t.tracks[..index].contains(hue), "{}: hue {index} repeats", t.name);
            }
            assert_eq!(t.track_hue(TRACK_HUES + 2), t.tracks[2]);
        });
    }

    #[test]
    fn arbitrary_colors_map_to_the_nearest_hue() {
        let noir = Theme::noir();
        let [violet, sky, teal, sage, amber, rose, slate, clay, iris, aqua] = noir.tracks;
        for hue in noir.tracks {
            assert_eq!(noir.nearest_track_hue(hue), hue, "a palette hue is its own nearest");
        }
        // Saturated colors of the kind older projects store.
        assert_eq!(noir.nearest_track_hue(rgb(0xF2994A)), clay, "orange");
        assert_eq!(noir.nearest_track_hue(rgb(0xF2C94C)), amber, "yellow");
        assert_eq!(noir.nearest_track_hue(rgb(0x27AE60)), sage, "green");
        assert_eq!(noir.nearest_track_hue(rgb(0x00C8A0)), teal);
        assert_eq!(noir.nearest_track_hue(rgb(0x56CCF2)), aqua, "light blue");
        assert_eq!(noir.nearest_track_hue(rgb(0x2D9CDB)), sky, "blue");
        assert_eq!(noir.nearest_track_hue(rgb(0x5050F0)), iris, "indigo");
        assert_eq!(noir.nearest_track_hue(rgb(0x9B51E0)), violet, "purple");
        assert_eq!(noir.nearest_track_hue(rgb(0xE27AB0)), rose, "pink");
        assert_eq!(noir.nearest_track_hue(rgb(0x808080)), slate, "grays take the neutral hue");
        assert_eq!(noir.nearest_track_hue(rgb(0x000000)), slate);
    }

    #[test]
    fn contrast_and_compositing_reference_points() {
        assert!((contrast_ratio(Color32::WHITE, Color32::BLACK) - 21.0).abs() < 0.01);
        assert!((contrast_ratio(rgb(0x101013), rgb(0x101013)) - 1.0).abs() < 1e-6);
        // White at 10 % over #16161A, as the kit's `blend.selected` has it.
        assert_eq!(over(rgb(0x16161A), white(0.10)), rgb(0x2E2E31));
        assert_eq!(over(rgb(0x16161A), Color32::TRANSPARENT), rgb(0x16161A));
        let (chroma, hue) = chroma_and_hue(rgb(0xA87FD1));
        assert!((chroma - 0.125).abs() < 0.002 && (hue - 306.3).abs() < 0.5, "violet is {chroma:.3} at {hue:.1}");
    }

    #[test]
    fn themes_are_found_by_name() {
        assert_eq!(Theme::named("noir").unwrap().name, "noir");
        assert_eq!(Theme::named("graphite").unwrap().name, "graphite");
        assert!(Theme::named("neon").is_none());
        assert!(Theme::all()[0].dark, "Noir is the default");
    }
}
