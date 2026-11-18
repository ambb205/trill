//! A song that is being opened, and devices that are being made, as the
//! window says them.
//!
//! Two things take time when a song is opened, and the window says each
//! where it happens. The second of them is the same whatever brings a
//! plugin: a song that was opened, a device dropped on a track, an undo,
//! the audio device at another sample rate.
//!
//! **Its file is read**, by a helper thread that has the session for as
//! long as that takes: tens of milliseconds, or a second for a song of
//! many long recordings. The window draws from what it has, which is the
//! song before; so from the frame after a file is chosen that song is
//! dimmed, and a plate over it says which file is being opened
//! ([`Jobs::opening`](super::files::Jobs::opening)). Nothing is asked of
//! the session meanwhile: a key or a click that did would wait for it.
//!
//! **Its devices are made**, the plugins one at a time, after the song
//! has them ([`Session::arriving`](trill_engine::Session::arriving)).
//! The window is alive meanwhile: the song can be scrolled, selected,
//! edited and played. One line says how far the whole is ("12 of 38
//! devices: Archetype Tim Henson X on Lead"), on a plate with a bar in
//! the corner where the window says things, and in the status bar. A
//! device that is not there says so
//! where it is shown: on its panel in place of its parameters, and as a
//! mark on the header of its track.
//!
//! **Play is never refused.** A track whose instrument is not there yet is
//! silent until it is, and an effect lets through what comes into it;
//! starting the song says so once, and the computer keyboard's strip says
//! it of the track it would play.
//!
//! [`OpeningScene`] is a made-up opening, for pictures and tests.

use super::snapshot::Live;
use super::state::Cx;
use super::toast::ToastKind;
use eframe::egui::{self, Align2, Id, Rect, Sense, Ui, pos2, vec2};
use std::path::{Path, PathBuf};
use std::time::Duration;
use trill_core::project::Track;
use trill_core::{Device, DeviceId, Project};
use trill_engine::Arriving;
use trill_noir::{paint, text};

/// The plate that says how far the devices are, and the gap between it
/// and what is said above it.
const PLATE: egui::Vec2 = vec2(404.0, 64.0);
const PLATE_GAP: f32 = 6.0;
/// The plate that says which file is being opened.
const READING: egui::Vec2 = vec2(420.0, 78.0);
/// How long what is said when the song is started stays.
const SAID: Duration = Duration::from_secs(9);

/// A song in the middle of being opened that is made up, for a picture or
/// a test.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum OpeningScene {
    /// Its file is being read: the song before is still on the screen.
    Reading,
    /// It is there, and its devices are arriving.
    Arriving,
}

impl OpeningScene {
    pub const ALL: [(&'static str, OpeningScene); 2] =
        [("reading", OpeningScene::Reading), ("arriving", OpeningScene::Arriving)];

    pub fn named(name: &str) -> Option<Self> {
        Self::ALL.iter().find(|(known, _)| *known == name).map(|(_, scene)| *scene)
    }

    /// The file the window says is being opened, in the scene that has one.
    pub fn file(self) -> Option<PathBuf> {
        (self == OpeningScene::Reading).then(|| PathBuf::from("Night Shift.trill"))
    }

    /// How far `project` would be with its devices in the scene: the
    /// instruments of its tracks from the third on are not there yet, and
    /// the first of them is being made. The same every time.
    pub fn of(self, project: &Project) -> Option<Arriving> {
        if self != OpeningScene::Arriving {
            return None;
        }
        let coming: Vec<DeviceId> =
            project.tracks.iter().skip(2).filter_map(|track| track.devices.first()).map(|device| device.id).collect();
        let of = project.devices().count();
        let now = coming.first().copied();
        (!coming.is_empty()).then(|| Arriving { made: of - coming.len(), of, now, coming })
    }
}

/// What a device that is not there yet is doing.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Coming {
    /// Its turn has not come.
    Waiting,
    /// It is the one being made.
    Making,
}

impl Coming {
    /// The word a panel says ahead of the device's name.
    pub fn word(self) -> &'static str {
        match self {
            Coming::Waiting => "Waiting",
            Coming::Making => "Being made",
        }
    }
}

/// Whether `device` is one that the song has and that is not there yet,
/// and if so whether its turn has come.
pub fn of_device(live: &Live, device: DeviceId) -> Option<Coming> {
    let arriving = live.arriving.as_ref()?;
    arriving.coming.contains(&device).then(|| match arriving.now == Some(device) {
        true => Coming::Making,
        false => Coming::Waiting,
    })
}

/// What a device that is not there yet says of itself: on its panel, and
/// to whoever points at its mark.
pub fn device_words(live: &Live, device: &Device) -> String {
    let counted = live.arriving.as_ref().map(|arriving| format!("{} of {}", arriving.made, arriving.of));
    let counted = counted.unwrap_or_default();
    match of_device(live, device.id) {
        Some(Coming::Making) => format!(
            "{} is being made right now. The song has it, and plugins are made one at a time, away from the song: \
             {counted} devices are there. Until it is, an instrument is silent and an effect lets through what \
             comes into it.",
            device.name
        ),
        _ => format!(
            "{} is waiting for its turn. The song has it, and plugins are made one at a time, away from the song, \
             in the order they were asked for: {counted} devices are there. Until it is, an instrument is silent \
             and an effect lets through what comes into it.",
            device.name
        ),
    }
}

/// What the status bar says while devices are arriving.
pub fn status_words(cx: &Cx) -> Option<String> {
    cx.live.arriving.as_ref().map(|_| format!("Loading {}", cx.live.arriving_words))
}

/// The mark of a track that has a device which is not there yet: a dot
/// in `rect`, hollow while the device waits and filled while it is being
/// made, that says which device to whoever points at it. Returns whether
/// there was anything to mark.
pub fn track_mark(ui: &Ui, cx: &mut Cx, rect: Rect, id: Id, track: &Track) -> bool {
    let live = cx.live;
    let Some((device, coming)) = track.devices.iter().find_map(|device| Some((device, of_device(live, device.id)?)))
    else {
        return false;
    };
    let theme = cx.theme;
    let response = ui.interact(rect, id, Sense::hover());
    let name = format!("{}: {} is not there yet", track.name, device.name);
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Label, true, &name));
    let left = pos2(rect.center().x - theme.metric.dot / 2.0, rect.center().y);
    match coming {
        Coming::Making => paint::dot(ui.painter(), left, theme.metric.dot, theme.signal),
        Coming::Waiting => paint::hollow_dot(ui.painter(), left, theme.metric.dot, theme.info),
    };
    cx.hint(&response, device_words(live, device));
    true
}

/// Says, when the song is started while its devices are still being
/// made, that it is not all there: what a silent track would otherwise
/// leave to be guessed.
pub fn played(cx: &mut Cx) {
    let Some(arriving) = &cx.live.arriving else { return };
    cx.state.toasts.lasting(ToastKind::Info, played_words(arriving), SAID);
}

/// What [`played`] says.
pub(super) fn played_words(arriving: &Arriving) -> String {
    let left = arriving.coming.len();
    let are = if left == 1 { "is" } else { "are" };
    format!(
        "Playing while {left} of the song\u{2019}s {} devices {are} still being made. A track whose instrument is \
         not there yet is silent until it is.",
        arriving.of
    )
}

/// The name a file is said by.
fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(|| path.display().to_string(), |name| name.to_string_lossy().into_owned())
}

/// Paints what there is to say of a song that is being opened, over the
/// window: the file that is being read, in the middle of it; or how far
/// its devices are, in `corner`, where the window's messages go. Returns
/// the corner that is left for those, above it.
pub fn plate(ctx: &egui::Context, cx: &mut Cx, corner: egui::Pos2) -> egui::Pos2 {
    if let Some(file) = cx.jobs.opening().map(Path::to_path_buf) {
        reading(ctx, cx, &file);
        corner
    } else if cx.live.arriving.is_some() {
        arriving(ctx, cx, corner)
    } else {
        corner
    }
}

/// The song before, dimmed, and over it which file is being opened.
fn reading(ctx: &egui::Context, cx: &mut Cx, file: &Path) {
    let theme = cx.theme;
    let screen = ctx.content_rect();
    let words = format!("Opening {}", file_name(file));
    egui::Area::new(Id::unique("opening-a-song")).order(egui::Order::Foreground).fixed_pos(screen.min).show(
        ctx,
        |ui| {
            // Everything under it is the song before, and is not to be
            // worked in: what was asked of it would wait for the session.
            let response = ui.allocate_rect(screen, Sense::click_and_drag());
            response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Label, true, &words));
            let painter = ui.painter();
            painter.rect_filled(screen, 0.0, theme.backdrop);
            let plate = Rect::from_center_size(screen.center(), READING);
            painter.rect_filled(plate, 0.0, theme.panel);
            paint::frame(painter, plate, theme.hairline);
            let inner = plate.shrink2(vec2(18.0, 14.0));
            let row = |index: f32| {
                Rect::from_min_size(pos2(inner.left(), inner.top() + index * 18.0), vec2(inner.width(), 16.0))
            };
            let after = paint::dot(painter, row(0.0).left_center(), theme.metric.dot, theme.signal) + 6.0;
            text::paint(
                painter,
                row(0.0).with_min_x(after),
                Align2::LEFT_CENTER,
                theme.text.caps,
                "Opening",
                theme.ink,
            );
            text::paint_name(painter, row(1.0), Align2::LEFT_CENTER, theme.text.body, &file_name(file), theme.ink);
            let then = "Its tracks and clips first, then its devices, one by one";
            text::paint(painter, row(2.0), Align2::LEFT_CENTER, theme.text.micro, then, theme.ink_faint);
        },
    );
    // The news that it is there comes with a repaint of its own; until
    // then nothing else moves the window.
    ctx.request_repaint_after(Duration::from_millis(50));
}

/// How far the devices are, on a plate whose bottom right is `corner`:
/// the line, and a bar of how many are there. It takes no click: the song
/// is the one to work in. Returns the corner above it.
fn arriving(ctx: &egui::Context, cx: &mut Cx, corner: egui::Pos2) -> egui::Pos2 {
    let theme = cx.theme;
    let live = cx.live;
    let Some(arriving) = &live.arriving else { return corner };
    let plate = Rect::from_min_max(corner - PLATE, corner);
    let words = &live.arriving_words;
    let area = egui::Area::new(Id::unique("devices-arriving"))
        .order(egui::Order::Foreground)
        .fixed_pos(plate.min)
        .interactable(false);
    area.show(ctx, |ui| {
        let response = ui.allocate_rect(plate, Sense::hover());
        response
            .widget_info(|| egui::WidgetInfo::labeled(egui::Role::Label, true, format!("Loading devices: {words}")));
        let painter = ui.painter();
        painter.rect_filled(plate, 0.0, theme.raised);
        paint::frame(painter, plate, theme.hairline);
        let inner = plate.shrink2(vec2(12.0, 8.0));
        // How many, and under it which: a plugin's name and its track's
        // are long, and have a line of their own.
        let (counted, which) = words.split_once(": ").unwrap_or((words, ""));
        let line = |index: f32| Rect::from_min_size(inner.min + vec2(0.0, index * 18.0), vec2(inner.width(), 16.0));
        let after = paint::dot(painter, line(0.0).left_center(), theme.metric.dot, theme.signal) + 6.0;
        let first = line(0.0).with_min_x(after);
        let after =
            after + text::paint(painter, first, Align2::LEFT_CENTER, theme.text.caps, "Loading", theme.ink) + 8.0;
        text::paint(painter, line(0.0).with_min_x(after), Align2::LEFT_CENTER, theme.text.body, counted, theme.ink);
        text::paint_name(painter, line(1.0), Align2::LEFT_CENTER, theme.text.body, which, theme.ink_dim);
        // How many are there, of how many.
        let track = Rect::from_min_max(pos2(inner.left(), inner.bottom() - 4.0), inner.max);
        painter.rect_filled(track, 0.0, theme.well);
        let share = arriving.made as f32 / arriving.of.max(1) as f32;
        painter.rect_filled(track.with_max_x(track.left() + track.width() * share.clamp(0.0, 1.0)), 0.0, theme.signal);
    });
    // The next device arrives without any input.
    ctx.request_repaint_after(Duration::from_millis(100));
    pos2(corner.x, plate.top() - PLATE_GAP)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn song() -> Project {
        let mut project = Project::new("Song");
        for name in ["Drums", "Bass", "Lead", "Pad"] {
            let track = project.add_track(name);
            let chain = trill_core::ChainRef::Track(track);
            project.add_device(chain, "builtin:polysynth", name, None).unwrap();
            project.add_device(chain, "builtin:eq", "EQ", None).unwrap();
        }
        project
    }

    #[test]
    fn a_made_up_opening_has_the_later_instruments_still_to_come() {
        let project = song();
        assert_eq!(OpeningScene::Reading.of(&project), None);
        assert_eq!(OpeningScene::Reading.file(), Some(PathBuf::from("Night Shift.trill")));
        let arriving = OpeningScene::Arriving.of(&project).expect("two instruments are to come");
        let of = project.devices().count();
        assert_eq!((arriving.made, arriving.of, arriving.coming.len()), (of - 2, of, 2));
        assert_eq!(arriving.now, arriving.coming.first().copied());
        assert!(arriving.words(&project).ends_with("devices: Lead on Lead"), "{}", arriving.words(&project));
        assert_eq!(
            (OpeningScene::named("arriving"), OpeningScene::named("nope")),
            (Some(OpeningScene::Arriving), None)
        );
    }

    #[test]
    fn a_device_that_is_not_there_says_whether_its_turn_has_come() {
        let project = song();
        let arriving = OpeningScene::Arriving.of(&project).unwrap();
        let (making, waiting) = (arriving.coming[0], arriving.coming[1]);
        let mut live = Live::default();
        live.arriving = Some(arriving);
        assert_eq!(of_device(&live, making), Some(Coming::Making));
        assert_eq!(of_device(&live, waiting), Some(Coming::Waiting));
        let there = project.tracks[0].devices[0].id;
        assert_eq!(of_device(&live, there), None);
        let words = device_words(&live, project.device(waiting).unwrap());
        assert!(words.starts_with("Pad is waiting for its turn"), "{words}");
        assert!(words.contains("an instrument is silent"), "{words}");
        let words = device_words(&live, project.device(making).unwrap());
        assert!(words.starts_with("Lead is being made right now"), "{words}");
    }

    #[test]
    fn starting_the_song_says_how_much_of_it_is_still_to_come() {
        let project = song();
        let arriving = OpeningScene::Arriving.of(&project).unwrap();
        let said = played_words(&arriving);
        assert!(said.starts_with("Playing while 2 of the song\u{2019}s 8 devices are still being made"), "{said}");
        let one = Arriving { coming: vec![arriving.coming[0]], made: 7, ..arriving };
        assert!(played_words(&one).contains("1 of the song\u{2019}s 8 devices is still being made"));
    }
}
