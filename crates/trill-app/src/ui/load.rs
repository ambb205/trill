//! Playback load: how the machine keeps up with the song as it plays, and
//! what each track costs.
//!
//! A song that plays has to be made in the time its buffers last. The top
//! bar shows how much of that time is in use (CPU); a click on it opens
//! this panel, which says the rest: how many buffers came late, on how
//! many threads the song is made, and every track with the share of a
//! processor it takes, the costliest first.
//!
//! When the song is more than the machine plays in time the window says
//! so in words, as one thing: a notice when it begins, once (and again
//! only after the song has kept up for a minute in between), a line in
//! the status bar for as long as it holds, and the whole sentence at the
//! top of the panel. The words are the session's
//! ([`trill_engine::Shortfall`]), which a model reads too.
//!
//! What is shown comes from [`Session::keeping_up`](trill_engine::Session::keeping_up),
//! read when the window reads everything else of the session. A picture
//! shows no load of its own, since how busy the machine was while it was
//! taken is not part of what the window looks like; [`LoadScene`] is a
//! made-up one, for pictures and tests.

use super::state::Cx;
use super::toast::ToastKind;
use eframe::egui::{self, Align2, Rect, Sense, Ui, pos2, vec2};
use std::time::Duration;
use trill_core::Project;
use trill_engine::{Cost, KeepingUp, Load, Shortfall, TrackCost};
use trill_noir::{paint, text};

const WIDTH: f32 = 560.0;
const ROW: f32 = 18.0;
const HEADING: f32 = 22.0;
/// The most tracks that are shown before they scroll.
const TRACK_ROWS: f32 = 16.0;
/// Where a track's share is written, and where its bar ends, from the
/// right edge.
const SHARE_WIDTH: f32 = 64.0;
const BAR_WIDTH: f32 = 180.0;
/// How long the notice stays: long enough to be read through.
const NOTICE: Duration = Duration::from_secs(14);

/// A load that is made up, for a picture or a test.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum LoadScene {
    /// A song the machine plays with most of its time to spare.
    Calm,
    /// One that is more than it plays in time.
    Short,
}

impl LoadScene {
    pub const ALL: [(&'static str, LoadScene); 2] = [("calm", LoadScene::Calm), ("short", LoadScene::Short)];

    pub fn named(name: &str) -> Option<Self> {
        Self::ALL.iter().find(|(known, _)| *known == name).map(|(_, scene)| *scene)
    }

    /// What the audio threads would have made of `project` in the scene:
    /// the same figures every time, from the tracks' places in the song.
    pub fn of(self, project: &Project) -> KeepingUp {
        let sample_rate = 48_000.0;
        // Ten seconds of sound in buffers of 480 frames.
        let (buffers, frames) = (1_000u64, 480_000u64);
        let heavy = self == LoadScene::Short;
        let shares = [0.259, 0.031, 0.152, 0.004, 0.146, 0.077, 0.123, 0.001, 0.113, 0.024, 0.137, 0.002];
        let cost = |share: f64| Cost {
            blocks: 2_750,
            frames,
            nanos: (share * frames as f64 / sample_rate * 1e9) as u64,
            longest: (share * 16.0e6) as u64 + 90_000,
            longest_ran: 0,
        };
        let tracks: Vec<TrackCost> = (project.tracks.iter().enumerate())
            .map(|(index, track)| {
                let share = shares[index % shares.len()] * if heavy { 4.1 } else { 1.0 };
                TrackCost { track: track.id, name: track.name.clone(), cost: cost(share) }
            })
            .collect();
        let (mean, worst, late, threads, allowed) = match heavy {
            true => (1.07, 2.92, 647, 8, 8),
            false => (0.366, 0.74, 0, 8, 8),
        };
        let load = Load { callbacks: buffers, late, mean, worst, threads, allowed, frames, ..Load::default() };
        let short = heavy.then(|| {
            let mut costliest: Vec<(String, f64)> =
                tracks.iter().map(|track| (track.name.clone(), track.cost.share(sample_rate))).collect();
            costliest.sort_by(|a, b| b.1.total_cmp(&a.1));
            costliest.truncate(3);
            Shortfall { episode: 1, late, buffers, costliest, threads, allowed }
        });
        KeepingUp { seconds: 10.0, load, tracks, master: cost(0.002), sample_rate, short }
    }
}

/// Opens the panel.
pub fn open(cx: &mut Cx) {
    cx.state.load_open = true;
}

/// Says that the song has begun to fall short, once: again only when the
/// session says it is a new thing ([`Shortfall::episode`]).
pub fn notice(cx: &mut Cx) {
    let live = cx.live;
    let Some(short) = &live.keeping.short else { return };
    if cx.state.load_said == Some(short.episode) {
        return;
    }
    cx.state.load_said = Some(short.episode);
    cx.state.toasts.lasting(ToastKind::Error, notice_words(short), NOTICE);
}

/// What the notice says: what is the matter and what is not, and where
/// the rest is to be read.
pub(super) fn notice_words(short: &Shortfall) -> String {
    format!(
        "{}: buffers are coming late, and each is a gap in what is heard. An export is not affected. Click CPU for \
         what each track costs.",
        short.headline()
    )
}

/// What the status bar says for as long as the song falls short.
pub(super) fn status_words(keeping: &KeepingUp) -> Option<String> {
    keeping.short.as_ref().map(|short| format!("{} \u{00B7} {} buffers late", short.headline(), grouped(short.late)))
}

/// `1234567` as `1,234,567`.
fn grouped(number: u64) -> String {
    let digits = number.to_string();
    let mut out = String::new();
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

/// A share of a processor in words: `25.9 %`, and `0.4 %` for what is
/// next to nothing.
fn percent(share: f64) -> String {
    format!("{:.1} %", share * 100.0)
}

/// The lines over the tracks: what there is to say of the song as a
/// whole, each a label and what it has.
pub(super) fn summary(keeping: &KeepingUp, buffer_frames: u32) -> Vec<(&'static str, String)> {
    let load = &keeping.load;
    if load.callbacks == 0 {
        return vec![("Audio thread", "No buffer has been asked for yet".to_string())];
    }
    let late = match load.late {
        0 => format!("None of {} in the last {:.0} s", grouped(load.callbacks), keeping.seconds),
        late => format!("{} of {} in the last {:.0} s", grouped(late), grouped(load.callbacks), keeping.seconds),
    };
    let threads = match (load.threads, load.allowed) {
        (1, 1) => "1: the song has nothing to make at once, or may have no more".to_string(),
        (1, allowed) => format!("1 of the {allowed} it may have: one has it made with time to spare"),
        (threads, allowed) => format!("{threads} of the {allowed} it may have"),
    };
    let buffer = match keeping.sample_rate > 0.0 {
        true => format!(
            "{buffer_frames} frames at {:.0} Hz, {:.1} ms each",
            keeping.sample_rate,
            f64::from(buffer_frames) * 1e3 / keeping.sample_rate
        ),
        false => format!("{buffer_frames} frames"),
    };
    vec![
        (
            "Audio thread",
            format!(
                "{:.0} % of its time on average, {:.0} % for the worst buffer",
                load.mean * 100.0,
                load.worst * 100.0
            ),
        ),
        ("Late buffers", late),
        ("Threads", threads),
        ("Buffers", buffer),
    ]
}

/// The tracks as the panel lists them: the costliest first, each with the
/// share of one processor it takes.
pub(super) fn tracks(keeping: &KeepingUp) -> Vec<(String, f64)> {
    let mut tracks: Vec<(String, f64)> =
        keeping.tracks.iter().map(|track| (track.name.clone(), track.cost.share(keeping.sample_rate))).collect();
    tracks.sort_by(|a, b| b.1.total_cmp(&a.1));
    tracks
}

/// `words` broken at its spaces into rows no wider than `width`.
fn rows_of(words: &str, width: f32, measure: impl Fn(&str) -> f32) -> Vec<String> {
    let mut rows = Vec::new();
    let mut row = String::new();
    for word in words.split(' ') {
        let longer = if row.is_empty() { word.to_string() } else { format!("{row} {word}") };
        if !row.is_empty() && measure(&longer) > width {
            rows.push(std::mem::replace(&mut row, word.to_string()));
        } else {
            row = longer;
        }
    }
    rows.push(row);
    rows
}

/// A line of the panel, on as many rows as it takes.
fn line(ui: &mut Ui, words: &str, style: trill_noir::TextStyle, ink: egui::Color32) {
    let rows = rows_of(words, WIDTH, |row| text::width(ui.painter(), style, row));
    let (rect, response) = ui.allocate_exact_size(vec2(WIDTH, ROW * rows.len() as f32), Sense::hover());
    for (index, row) in rows.iter().enumerate() {
        let at = Rect::from_min_size(rect.min + vec2(0.0, ROW * index as f32), vec2(WIDTH, ROW));
        text::paint(ui.painter(), at, Align2::LEFT_CENTER, style, row, ink);
    }
    // One thing to whoever reads the screen, whatever rows it is drawn on.
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Label, true, words));
}

fn heading(ui: &mut Ui, cx: &Cx, words: &str, beside: &str) {
    let theme = cx.theme;
    ui.add_space(6.0);
    let (rect, _) = ui.allocate_exact_size(vec2(WIDTH, HEADING), Sense::hover());
    let label = rect.with_max_y(rect.bottom() - 3.0);
    text::paint(ui.painter(), label, Align2::LEFT_CENTER, theme.text.caps, words, theme.ink_dim);
    text::paint(ui.painter(), label, Align2::RIGHT_CENTER, theme.text.micro, beside, theme.ink_faint);
    paint::hline_above(ui.painter(), rect.x_range(), rect.bottom(), theme.hairline);
    ui.add_space(4.0);
}

/// A label and what it has, on one row.
fn readout(ui: &mut Ui, cx: &Cx, label: &str, value: &str, ink: egui::Color32) {
    let theme = cx.theme;
    let (rect, response) = ui.allocate_exact_size(vec2(WIDTH, ROW), Sense::hover());
    text::paint(ui.painter(), rect, Align2::LEFT_CENTER, theme.text.micro, label, theme.ink_faint);
    let value_rect = rect.with_min_x(rect.left() + 110.0);
    text::paint(ui.painter(), value_rect, Align2::LEFT_CENTER, theme.text.body, value, ink);
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Label, true, format!("{label}: {value}")));
}

/// A track's row: its name, a bar as long as its share of `full`, and the
/// share in figures.
fn track_row(ui: &mut Ui, cx: &Cx, name: &str, share: f64, full: f64) {
    let theme = cx.theme;
    let (rect, response) = ui.allocate_exact_size(vec2(WIDTH, ROW), Sense::hover());
    let figures = Rect::from_min_max(pos2(rect.right() - SHARE_WIDTH, rect.top()), rect.max);
    let bar = Rect::from_min_size(pos2(figures.left() - BAR_WIDTH - 10.0, rect.center().y - 4.0), vec2(BAR_WIDTH, 8.0));
    let name_room = rect.with_max_x(bar.left() - 12.0);
    text::paint(ui.painter(), name_room, Align2::LEFT_CENTER, theme.text.body, name, theme.ink_dim);
    // The more of a processor a track takes, the louder its bar says so.
    let color = match share {
        share if share >= 0.5 => theme.signal,
        share if share >= 0.25 => theme.warn,
        _ => theme.data,
    };
    trill_noir::mini_meter(ui.painter(), theme, bar, (share / full.max(1e-9)) as f32, color);
    text::paint(ui.painter(), figures, Align2::RIGHT_CENTER, theme.text.value, &percent(share), theme.ink);
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::Role::Label, true, format!("{name}: {} of a processor", percent(share)))
    });
}

/// The panel, while it is open.
pub fn window(ctx: &egui::Context, cx: &mut Cx) {
    if !cx.state.load_open {
        return;
    }
    let theme = cx.theme;
    let live = cx.live;
    let keeping = &live.keeping;
    let buffer_frames = cx.audio.info.buffer_frames;
    let frame = egui::Frame::NONE
        .fill(theme.panel)
        .stroke(egui::Stroke::new(1.0, theme.hairline))
        .inner_margin(egui::Margin::symmetric(18, 14));
    let modal =
        egui::Modal::new(egui::Id::unique("load")).frame(frame).backdrop_color(theme.backdrop).show(ctx, |ui| {
            ui.set_width(WIDTH);
            ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
            let (title, _) = ui.allocate_exact_size(vec2(WIDTH, 18.0), Sense::hover());
            text::paint(ui.painter(), title, Align2::LEFT_CENTER, theme.text.caps, "Playback load", theme.ink);
            text::paint(ui.painter(), title, Align2::RIGHT_CENTER, theme.text.micro, "Esc closes", theme.ink_faint);

            if let Some(short) = &keeping.short {
                ui.add_space(8.0);
                line(ui, &short.sentence(), theme.text.body, theme.signal);
            }

            heading(ui, cx, "The song as it plays", "");
            let late = keeping.load.late > 0;
            for (label, value) in summary(keeping, buffer_frames) {
                let ink = if label == "Late buffers" && late { theme.signal } else { theme.ink };
                readout(ui, cx, label, &value, ink);
            }

            let listed = tracks(keeping);
            if !listed.is_empty() && keeping.load.callbacks > 0 {
                heading(ui, cx, "What each track costs", "of one processor");
                // A bar is full at the costliest track, or at a quarter of
                // a processor where none takes that much: a song of small
                // tracks is not drawn as if it were heavy.
                let full = listed.first().map_or(0.25, |(_, share)| share.max(0.25));
                egui::ScrollArea::vertical().max_height(TRACK_ROWS * ROW).auto_shrink([false, true]).show(ui, |ui| {
                    for (name, share) in &listed {
                        track_row(ui, cx, name, *share, full);
                    }
                });
                paint::hline_above(ui.painter(), ui.max_rect().x_range(), ui.cursor().top() + 3.0, theme.hairline);
                ui.add_space(6.0);
                let all: f64 = listed.iter().map(|(_, share)| share).sum();
                readout(ui, cx, "All the tracks", &percent(all), theme.ink);
                readout(ui, cx, "The master", &percent(keeping.master.share(keeping.sample_rate)), theme.ink);
                ui.add_space(6.0);
                let about = "A track costs this whether or not it is muted or anything plays on it: its devices run \
                             all the time. Switching a device off, or removing it, is what takes its cost away. An \
                             export takes the time it needs and is never late.";
                line(ui, about, theme.text.micro, theme.ink_faint);
            }
        });
    if modal.should_close() {
        cx.state.load_open = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn song() -> Project {
        let mut project = Project::new("Load");
        for name in ["Pad", "Drums", "Bass"] {
            project.add_track(name);
        }
        project
    }

    #[test]
    fn the_panel_lists_the_costliest_track_first_and_says_how_the_song_is_played() {
        let calm = LoadScene::Calm.of(&song());
        assert!(calm.short.is_none() && status_words(&calm).is_none());
        let listed = tracks(&calm);
        assert_eq!(listed.iter().map(|(name, _)| name.as_str()).collect::<Vec<_>>(), ["Pad", "Bass", "Drums"]);
        assert_eq!(percent(listed[0].1), "25.9 %");
        let said = summary(&calm, 480);
        assert_eq!(said[0], ("Audio thread", "37 % of its time on average, 74 % for the worst buffer".to_string()));
        assert_eq!(said[1], ("Late buffers", "None of 1,000 in the last 10 s".to_string()));
        assert_eq!(said[2], ("Threads", "8 of the 8 it may have".to_string()));
        assert_eq!(said[3], ("Buffers", "480 frames at 48000 Hz, 10.0 ms each".to_string()));
        // Before a device has asked for anything there is nothing to count.
        assert_eq!(summary(&KeepingUp::default(), 0).len(), 1);

        let short = LoadScene::Short.of(&song());
        assert_eq!(summary(&short, 480)[1].1, "647 of 1,000 in the last 10 s");
        let falling = short.short.clone().expect("the scene falls short");
        assert_eq!(
            status_words(&short).as_deref(),
            Some("This song is more than this machine plays in time \u{00B7} 647 buffers late")
        );
        let notice = notice_words(&falling);
        assert!(notice.starts_with("This song is more than this machine plays in time: buffers are coming late"));
        assert!(notice.contains("An export is not affected.") && notice.ends_with("what each track costs."));
        assert_eq!((LoadScene::named("short"), LoadScene::named("nope")), (Some(LoadScene::Short), None));
    }

    #[test]
    fn a_long_line_is_broken_at_its_spaces() {
        let seven_a_letter = |row: &str| row.chars().count() as f32 * 7.0;
        assert_eq!(rows_of("one two three four", 70.0, seven_a_letter), ["one two", "three four"]);
        assert_eq!(rows_of("short", 70.0, seven_a_letter), ["short"]);
    }
}
