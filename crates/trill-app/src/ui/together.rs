//! Working in one song with other people: the window's part.
//!
//! Everything that merges is the model's (`trill_core::merge`), the
//! session's and the runtime's ([`trill_runtime::Runtime::share`], `join`,
//! `together`). The window does four things with it:
//!
//! * **File, Share this song and Join a song** open a panel: the name one
//!   goes by, where to be reached (this computer, its networks, or
//!   through a meeting point, whose address and key are given there
//!   once), the invitation to hand to someone or the one that was handed
//!   over.
//! * **Who is here** is in the status bar while the song is shared, each
//!   person in their colour ([`trill_noir::people`]); a click opens the
//!   panel. A connection that was lost, and a meeting point that is away,
//!   are said there for as long as they are being tried again.
//! * **What they hold** is drawn where it is: an outline in their colour
//!   on the clips and notes they have selected, a bar on their tracks, a
//!   line with their name where their song stands. None of it is part of
//!   the song, and none of it is read from the session: the runtime has it
//!   ([`trill_runtime::Together`]), and the window copies it once a frame.
//! * **A clash is a notice**: a card with the sentence and the button
//!   that puts one's own back ([`trill_engine::Notice`]), said once for
//!   the thing it is about, and gone when it is dealt with.
//!
//! [`TogetherScene`] is made-up company, for pictures and tests.

use super::UiOptions;
use super::snapshot::{Live, Snapshot};
use super::state::{Cx, UiState};
use super::toast::ToastKind;
use eframe::egui::{self, Align2, Color32, Painter, Rect, Sense, Ui, pos2, vec2};
use std::time::{Duration, Instant};
use trill_core::{ClipId, Project, TrackId};
use trill_engine::Notice;
use trill_noir::{Chip, MenuItem, Variant, paint, text};
use trill_runtime::{Away, Going, Here, Meeting, MeetingPointSet, Mine, OnItsWay, Person, Reach, Runtime, Waiting};

const WIDTH: f32 = 520.0;
const ROW: f32 = 18.0;
/// How wide a notice is.
const NOTICE: f32 = 380.0;

/// Which of the two panels is open.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Panel {
    Share,
    Join,
}

/// Where a song that is shared is reached from.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum Reached {
    /// From this computer only.
    #[default]
    ThisComputer,
    /// From the networks this computer is on.
    Network,
    /// From anywhere, through a meeting point both programs connect out
    /// to: nothing listens here.
    MeetingPoint,
}

/// What the window keeps for working together: what its panel shows and
/// what it last told the others.
#[derive(Default)]
pub struct TogetherState {
    pub panel: Option<Panel>,
    /// The invitation as it is typed or pasted into the Join panel.
    pub invitation: String,
    /// The name as it is typed. Empty until the panel first opens.
    pub name: String,
    /// Where to be reached from.
    pub reach: Reached,
    /// The meeting point the settings name, as of when the panel opened
    /// or it was last set here. (Whether it has a key: never the key.)
    pub point: Option<MeetingPointSet>,
    /// A meeting point's address and key as they are typed, while one is
    /// being set. The key is let go of as soon as it is kept.
    pub point_address: String,
    pub point_key: String,
    /// Whether the two fields are shown where a meeting point is set.
    pub changing_point: bool,
    /// Join: the song that is open may be replaced though it has changes
    /// that are not saved.
    pub replace: bool,
    /// What a share or a join came to, where that was a refusal.
    pub refusal: Option<String>,
    /// The Join panel has just opened: what is typed or pasted goes into
    /// its field.
    focus: bool,
    /// The number of the last sentence that was shown.
    heard: u64,
    /// What was last said of what this window's person has selected.
    said: Option<Mine>,
    /// What sharing the song would hand to whoever joins, looked at by a
    /// worker when the Share panel opens: `None` until it has looked.
    pub going: std::sync::Arc<parking_lot::Mutex<Option<Going>>>,
    /// Whom the panel is asking about before they are let go: their
    /// number, after *Remove* on their row and until it is answered.
    pub removing: Option<u64>,
}

/// What the window holds of the others, as of this frame.
#[derive(Clone, Debug, Default)]
pub struct Seen {
    pub me: Option<Person>,
    /// The invitation the song is shared under, while it is.
    pub invitation: Option<String>,
    pub joining: bool,
    pub connected: usize,
    pub people: Vec<Here>,
    /// Where one's own work and someone else's clashed.
    pub notices: Vec<Notice>,
    /// Where the song is shared through a meeting point: its address,
    /// and how the meeting stands there.
    pub meeting: Option<(String, Meeting)>,
    /// The track this program is recording for the others, who have not
    /// got all its devices, while it does.
    pub recording_for_others: Option<String>,
    /// The connections that were lost and are being asked for again.
    pub away: Vec<Away>,
    /// The song's audio that is on its way here from the others.
    pub files: Vec<OnItsWay>,
    /// The files the song names on this computer that have not gone to
    /// the others: nobody was shown them.
    pub held: Vec<String>,
    /// For how many seconds the song's audio still waits for the meeting
    /// point's next day.
    pub files_wait: Option<u64>,
}

impl Seen {
    /// Whether the song is being worked in with others, or offered to be.
    pub fn is_on(&self) -> bool {
        self.invitation.is_some() || self.joining || self.connected > 0 || !self.away.is_empty()
    }

    fn who(&self, has: impl Fn(&Here) -> bool) -> Option<&Here> {
        self.people.iter().find(|here| has(here))
    }

    /// Who else has `clip` selected, if anyone has.
    pub fn on_clip(&self, clip: ClipId) -> Option<&Here> {
        self.who(|here| here.clips.contains(&clip))
    }

    pub fn on_track(&self, track: TrackId) -> Option<&Here> {
        self.who(|here| here.tracks.contains(&track))
    }

    pub fn on_note(&self, clip: ClipId, note: u64) -> Option<&Here> {
        self.who(|here| here.notes.contains(&(clip, note)))
    }
}

/// Company that is made up, for a picture or a test: who would be here
/// and what they would hold in `project`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum TogetherScene {
    /// Two friends in the song, each with something selected, one of them
    /// playing; and a clash to answer.
    Two,
    /// The song is shared and nobody has come yet.
    Waiting,
    /// The song is shared through a meeting point, and a friend who is
    /// somewhere else is in it.
    Meeting,
    /// One joined a friend's song and the connection was lost: it is
    /// being asked for again.
    Lost,
    /// One joined a friend's song and its audio is on its way: one file
    /// arriving, one that waits for a word, one that nobody has said, and
    /// one of one's own that is held.
    Arriving,
}

impl TogetherScene {
    pub const ALL: [(&'static str, TogetherScene); 5] = [
        ("two", TogetherScene::Two),
        ("waiting", TogetherScene::Waiting),
        ("meeting", TogetherScene::Meeting),
        ("lost", TogetherScene::Lost),
        ("arriving", TogetherScene::Arriving),
    ];

    pub fn named(name: &str) -> Option<Self> {
        Self::ALL.iter().find(|(known, _)| *known == name).map(|(_, scene)| *scene)
    }

    /// What the window would see in the scene: the same every time, from
    /// what the song holds.
    pub fn of(self, project: &Project) -> Seen {
        let me = Person { id: 3, name: "Kit".into(), colour: 3 };
        let invitation = Some("trill:192.168.1.20:47474:7Q2M-K9TX-4HNB-V3RC-8WJD-6PFA-ZY5E-1GSK".to_owned());
        if self == TogetherScene::Waiting {
            return Seen { me: Some(me), invitation, ..Seen::default() };
        }
        if self == TogetherScene::Lost {
            let away = Away { name: "Jo".into(), since: Instant::now(), tries: 3 };
            return Seen { me: Some(me), away: vec![away], ..Seen::default() };
        }
        let tracks: Vec<TrackId> = project.tracks.iter().map(|track| track.id).collect();
        let clips: Vec<ClipId> = project.tracks.iter().flat_map(|track| &track.clips).map(|clip| clip.id).collect();
        let nth = |list: &[ClipId], index: usize| list.get(index % list.len().max(1)).copied();
        // The notes of the first clip that has some, for the piano roll.
        let with_notes = project.tracks.iter().flat_map(|track| &track.clips).find(|clip| clip.notes().len() >= 4);
        let notes = |from: usize| -> Vec<(ClipId, u64)> {
            let Some(clip) = with_notes else { return Vec::new() };
            clip.notes().iter().skip(from).step_by(3).take(4).map(|note| (clip.id, note.id)).collect()
        };
        let heard = Instant::now();
        let jo = Here {
            person: Person { id: 0, name: "Jo".into(), colour: 0 },
            beat: 9.5,
            playing: false,
            heard,
            tracks: tracks.get(1).copied().into_iter().collect(),
            clips: nth(&clips, 1).into_iter().collect(),
            notes: notes(0),
            holding: Some("the volume of the track \u{201C}Bass\u{201D}".to_owned()),
            lacks: Vec::new(),
            // (In the song by this window's invitation, where it shares.)
            can_be_removed: true,
        };
        let sam = Here {
            person: Person { id: 1, name: "Sam".into(), colour: 1 },
            beat: 22.0,
            playing: false,
            heard,
            tracks: tracks.get(3).or(tracks.last()).copied().into_iter().collect(),
            clips: nth(&clips, 4).into_iter().chain(nth(&clips, 5)).collect(),
            notes: notes(1),
            holding: None,
            lacks: Vec::new(),
            can_be_removed: true,
        };
        // And a clash to answer, in the words a real one has.
        let text = "Jo set where the clip \u{201C}Lead\u{201D} starts at the same moment as you, and theirs is what the song has.";
        if self == TogetherScene::Meeting {
            // (No meeting point is anywhere but in someone's settings:
            // this one is nobody's.)
            let at = "meet.example.org:443";
            let invitation = Some(format!("trill:meet:{at}:7Q2M-K9TX-4HNB-V3RC-8WJD-6PFA-ZY5E-1GSK"));
            let meeting = Some((at.to_owned(), Meeting::Held));
            return Seen { me: Some(me), invitation, connected: 1, people: vec![jo], meeting, ..Seen::default() };
        }
        if self == TogetherScene::Arriving {
            let file = |name: &str, mb: u64, here: u64, from: Option<&str>, waits: Option<Waiting>| OnItsWay {
                name: name.to_owned(),
                bytes: mb << 20,
                here: here << 20,
                from: from.map(str::to_owned),
                waits,
            };
            let files = vec![
                file("Vocal take 3.wav", 48, 20, Some("Jo"), None),
                file("Room mics.flac", 612, 0, None, Some(Waiting::ForAWord)),
                // (A clip Jo brought, of whose file nothing was said.)
                file("Guide guitar", 0, 0, None, Some(Waiting::ToBeSaid)),
            ];
            // (A place that is nobody's: a made-up folder.)
            let held = vec![r"D:\Sessions\Elsewhere\guide vocal.wav".to_owned()];
            // (Jo shares this one: nobody here is this window's to let go.)
            let jo = Here { can_be_removed: false, ..jo };
            return Seen { me: Some(me), connected: 1, people: vec![jo], files, held, ..Seen::default() };
        }
        let notices = vec![Notice::made_up(1, text, Some("Put mine back"))];
        let (connected, people) = (2, vec![jo, sam]);
        Seen { me: Some(me), invitation, connected, people, notices, ..Seen::default() }
    }
}

/// Brings what the window holds of the others up to date, says what there
/// is to say, and tells the others what this window's person has
/// selected. Once a frame, without the session.
pub fn sync(state: &mut UiState, live: &mut Live, snapshot: &Snapshot, runtime: &Runtime, options: &UiOptions) {
    if let Some(scene) = options.together_scene {
        live.together = scene.of(&snapshot.project);
        return;
    }
    let status = runtime.together();
    let notices = std::mem::take(&mut live.together.notices);
    live.together = Seen {
        me: Some(status.me),
        invitation: status.invitation,
        joining: status.joining,
        connected: status.connected,
        people: status.people,
        notices,
        meeting: status.meeting,
        away: status.away,
        files: status.files,
        held: status.held,
        files_wait: status.files_wait,
        recording_for_others: status.recording_for_others,
    };
    // What happened: who came, who went and why, what was refused, what
    // was lost and is being tried again.
    for (number, sentence) in runtime.said_together(state.together.heard) {
        state.together.heard = number;
        let refused = ["Not joined", "No connection", "Not shared", "No longer in the song"];
        // (Not refused, and to be read all the same: it stays a while.)
        let lost = [
            "The connection to",
            "The meeting point",
            "More of the shared song's audio",
            "Audio files from",
            "What arrived from",
        ];
        let lost_or_held = |sentence: &str| {
            lost.iter().any(|start| sentence.starts_with(start)) || sentence.contains("not gone to the others")
        };
        if refused.iter().any(|start| sentence.starts_with(start)) {
            state.together.refusal = Some(sentence.clone());
            state.toasts.lasting(ToastKind::Error, sentence, Duration::from_secs(12));
        } else if lost_or_held(&sentence) && !sentence.contains("is back") {
            state.toasts.lasting(ToastKind::Info, sentence, Duration::from_secs(12));
        } else {
            state.toasts.info(sentence);
        }
    }
    if !live.together.is_on() {
        state.together.said = None;
        return;
    }
    // What this window's person has selected, for the others to see.
    let project = &snapshot.project;
    let mut mine = Mine {
        tracks: state.selected_track.into_iter().collect(),
        clips: state.selected_clips.clone(),
        ..Mine::default()
    };
    if let Some(clip) = state.selected_clip.and_then(|id| project.clip(id).ok()) {
        // Notes are selected by what they are; the others are told which.
        let held =
            |note: &trill_core::Note| clip.notes().iter().find(|found| *found == note).map(|found| (clip.id, found.id));
        mine.notes = state.selected_notes.iter().filter_map(held).collect();
    }
    if state.together.said.as_ref() != Some(&mine) {
        runtime.holding(mine.clone());
        state.together.said = Some(mine);
    }
}

/// Opens the panel that shares the song.
pub fn open_share(cx: &mut Cx) {
    open(cx, Panel::Share);
}

/// Opens the panel that joins someone's song.
pub fn open_join(cx: &mut Cx) {
    open(cx, Panel::Join);
}

fn open(cx: &mut Cx, panel: Panel) {
    let together = &mut cx.state.together;
    if together.name.is_empty() {
        together.name = cx.live.together.me.as_ref().map(|me| me.name.clone()).unwrap_or_default();
    }
    together.refusal = None;
    together.focus = panel == Panel::Join;
    together.panel = Some(panel);
    // The meeting point, as the settings have it now. (Asked when the
    // panel opens and not while it is drawn.)
    together.point = cx.sounds.runtime.meeting_point();
    (together.changing_point, together.point_key) = (false, String::new());
    // What would go with the song, for whoever is about to share it:
    // looked at away from here, since it asks the disk about every file.
    if panel == Panel::Share {
        let going = cx.state.together.going.clone();
        *going.lock() = None;
        cx.sounds.quietly(move |runtime| {
            *going.lock() = Some(runtime.files_to_go());
            Ok(())
        });
    }
}

/// The File menu's items.
pub fn menu(ui: &mut Ui, cx: &mut Cx) {
    let theme = cx.theme;
    let on = cx.live.together.is_on();
    let sharing = cx.live.together.invitation.is_some();
    let label = if sharing { "Sharing this song\u{2026}" } else { "Share this song\u{2026}" };
    if MenuItem::new(label)
        .detail("Work in it with someone else, each in a Trill of their own")
        .show(ui, theme)
        .clicked()
    {
        open_share(cx);
    }
    if MenuItem::new("Join a song\u{2026}").detail("With an invitation someone handed you").show(ui, theme).clicked() {
        open_join(cx);
    }
    if MenuItem::new("Leave the shared song").enabled(on).show(ui, theme).clicked() {
        cx.sounds.runtime.leave();
    }
}

/// The status bar's words while the song is shared: who is here.
pub fn status_words(seen: &Seen) -> Option<String> {
    if !seen.is_on() {
        return None;
    }
    let names: Vec<&str> = seen.people.iter().map(|here| here.person.name.as_str()).collect();
    // A connection that was lost is said for as long as it is asked for
    // again: nobody has to do anything, and should know why they are alone.
    let mut words = match (names.as_slice(), seen.joining, seen.away.first()) {
        ([], _, Some(away)) => format!("Connection to {} lost \u{00B7} trying again", away.name),
        ([], true, None) => "Joining\u{2026}".to_owned(),
        ([], false, None) => "Shared \u{00B7} nobody else is here".to_owned(),
        (names, _, Some(away)) => {
            format!("Here: {} \u{00B7} connection to {} lost, trying again", names.join(", "), away.name)
        }
        (names, _, None) => format!("Here: {}", names.join(", ")),
    };
    match &seen.meeting {
        Some((_, Meeting::Away(_))) => words.push_str(" \u{00B7} meeting point away, trying again"),
        Some((_, Meeting::Reaching)) => words.push_str(" \u{00B7} reaching the meeting point"),
        Some((_, Meeting::Held)) | None => {}
    }
    // The song's audio: how far what is on its way is, and what waits.
    // (What nobody has said yet is not on its way: said apart, below.)
    let unsaid = seen.files.iter().filter(|file| file.waits == Some(Waiting::ToBeSaid)).count();
    let coming = || seen.files.iter().filter(|file| file.waits != Some(Waiting::ToBeSaid));
    if coming().next().is_some() {
        let (here, all) = coming().fold((0u64, 0u64), |(here, all), file| (here + file.here, all + file.bytes));
        let files = match coming().count() {
            1 => "1 audio file".to_owned(),
            count => format!("{count} audio files"),
        };
        let asked = coming().any(|file| file.waits == Some(Waiting::ForAWord));
        let waiting = coming().all(|file| file.waits.is_some());
        match (asked, waiting) {
            (true, _) => words.push_str(&format!(" \u{00B7} {files} wait for your word")),
            (false, true) => words.push_str(&format!(" \u{00B7} {files} wait")),
            (false, false) => {
                let percent = (here * 100).checked_div(all).unwrap_or(0);
                words.push_str(&format!(" \u{00B7} {files} arriving, {percent} %"));
            }
        }
    }
    match unsaid {
        0 => {}
        1 => words.push_str(" \u{00B7} 1 audio file waits for whoever brought it"),
        unsaid => words.push_str(&format!(" \u{00B7} {unsaid} audio files wait for whoever brought them")),
    }
    if seen.files_wait.is_some() {
        words.push_str(" \u{00B7} audio waits for the meeting point");
    }
    if !seen.held.is_empty() {
        let files = if seen.held.len() == 1 { "1 file".to_owned() } else { format!("{} files", seen.held.len()) };
        words.push_str(&format!(" \u{00B7} {files} held here"));
    }
    Some(words)
}

/// The status bar's item: a square in each person's colour and the words.
/// Returns where it ends and its response, while the song is shared.
pub fn status_item(ui: &Ui, cx: &Cx, painter: &Painter, x: f32, bar: Rect) -> Option<(f32, egui::Response)> {
    let seen = &cx.live.together;
    let words = status_words(seen)?;
    let theme = cx.theme;
    let mut at = x;
    let colours = seen.me.iter().map(|me| me.colour).chain(seen.people.iter().map(|here| here.person.colour));
    for colour in colours {
        trill_noir::person_dot(painter, pos2(at + 3.5, bar.center().y), theme.person(colour));
        at += 10.0;
    }
    at += 3.0;
    let rest = Rect::from_min_max(pos2(at, bar.top()), bar.max);
    at += text::paint(painter, rest, Align2::LEFT_CENTER, theme.text.micro, &words, theme.ink_dim);
    let item = Rect::from_min_max(pos2(x - 4.0, bar.top()), pos2(at + 4.0, bar.bottom()));
    let response = ui.interact(item, ui.scope_id().with("together"), Sense::click());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, format!("Shared song: {words}")));
    if response.hovered() {
        painter.rect_filled(item, 0.0, theme.hover);
    }
    Some((at, response))
}

fn heading(ui: &mut Ui, cx: &Cx, words: &str) {
    let theme = cx.theme;
    ui.add_space(8.0);
    let (rect, _) = ui.allocate_exact_size(vec2(WIDTH, 20.0), Sense::hover());
    text::paint(ui.painter(), rect, Align2::LEFT_CENTER, theme.text.caps, words, theme.ink_dim);
    paint::hline_above(ui.painter(), rect.x_range(), rect.bottom(), theme.hairline);
    ui.add_space(6.0);
}

/// A line of the panel, on as many rows as it takes.
fn line(ui: &mut Ui, cx: &Cx, words: &str, ink: Color32) {
    let theme = cx.theme;
    ui.scope(|ui| {
        ui.set_width(WIDTH);
        let said = egui::RichText::new(words).color(ink).size(theme.text.body.size);
        ui.add(egui::Label::new(said).wrap());
    });
}

/// The name one goes by, as a field.
fn name_field(ui: &mut Ui, cx: &mut Cx) {
    let theme = cx.theme;
    let (rect, _) = ui.allocate_exact_size(vec2(WIDTH, theme.metric.control), Sense::hover());
    let label = rect.with_max_x(rect.left() + 110.0);
    text::paint(ui.painter(), label, Align2::LEFT_CENTER, theme.text.micro, "Your name", theme.ink_faint);
    let field = rect.with_min_x(label.right()).with_max_x(label.right() + 220.0);
    let id = egui::Id::unique("together-name");
    let response = trill_noir::text_field(ui, theme, field, id, &mut cx.state.together.name, "What the others see");
    if response.changed() || response.lost_focus() {
        let name = cx.state.together.name.trim().to_owned();
        if !name.is_empty() {
            cx.sounds.runtime.set_name_together(&name);
            let remembered = name.clone();
            cx.sounds.quietly(move |runtime| {
                runtime
                    .update_settings(|settings| settings.together.name = Some(remembered))
                    .map_err(|error| error.to_string())
            });
        }
    }
    ui.add_space(6.0);
}

/// Room at the end of a person's row for the word that lets them go.
const REMOVE: f32 = 60.0;

/// Everyone who is here, one a row: their colour, their name, what they
/// hold. Whoever is in the song by this program's own invitation has
/// *Remove* at the end of their row; returns whose was clicked.
fn people(ui: &mut Ui, cx: &Cx) -> Option<u64> {
    let theme = cx.theme;
    let seen = &cx.live.together;
    let mut asked = None;
    let mut row = |ui: &mut Ui, colour: u8, name: &str, about: &str, mine_to_remove: Option<u64>| {
        let (rect, response) = ui.allocate_exact_size(vec2(WIDTH, ROW), Sense::hover());
        trill_noir::person_dot(ui.painter(), pos2(rect.left() + 4.0, rect.center().y), theme.person(colour));
        let name_room = rect.with_min_x(rect.left() + 16.0).with_max_x(rect.left() + 150.0);
        text::paint(ui.painter(), name_room, Align2::LEFT_CENTER, theme.text.body, name, theme.ink);
        let end = if mine_to_remove.is_some() { rect.right() - REMOVE } else { rect.right() };
        let about_room = rect.with_min_x(rect.left() + 156.0).with_max_x(end);
        text::paint(ui.painter(), about_room, Align2::LEFT_CENTER, theme.text.micro, about, theme.ink_faint);
        response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Label, true, format!("{name}: {about}")));
        let Some(person) = mine_to_remove else { return };
        let room = rect.with_min_x(end);
        let word = ui.interact(room, ui.scope_id().with(("remove", person)), Sense::click());
        let ink = if word.hovered() { theme.signal } else { theme.ink_dim };
        text::paint(ui.painter(), room, Align2::RIGHT_CENTER, theme.text.micro, "Remove", ink);
        word.widget_info(|| {
            egui::WidgetInfo::labeled(egui::Role::Button, true, format!("Remove {name} from the song"))
        });
        if word.clicked() {
            asked = Some(person);
        }
    };
    if let Some(me) = &seen.me {
        row(ui, me.colour, &me.name, "you", None);
    }
    for here in &seen.people {
        let mine_to_remove = here.can_be_removed.then_some(here.person.id);
        row(ui, here.person.colour, &here.person.name, &about(here, &cx.snap.project), mine_to_remove);
    }
    // Who is away because the connection was lost: asked for again, with
    // nothing for anyone to do.
    for away in &seen.away {
        let about = match away.tries {
            0 | 1 => "the connection was lost; Trill is asking for it again".to_owned(),
            tries => format!("the connection was lost; Trill has asked for it {tries} times and goes on"),
        };
        let (rect, response) = ui.allocate_exact_size(vec2(WIDTH, ROW), Sense::hover());
        let name_room = rect.with_min_x(rect.left() + 16.0).with_max_x(rect.left() + 150.0);
        text::paint(ui.painter(), name_room, Align2::LEFT_CENTER, theme.text.body, &away.name, theme.ink_faint);
        let about_room = rect.with_min_x(rect.left() + 156.0);
        text::paint(ui.painter(), about_room, Align2::LEFT_CENTER, theme.text.micro, &about, theme.warn);
        response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Label, true, format!("{}: {about}", away.name)));
    }
    asked
}

/// The question before someone is let go: what it does, said before it
/// is done, and the two answers. Shown while one is asked about.
fn removing(ui: &mut Ui, cx: &mut Cx) {
    let Some(person) = cx.state.together.removing else { return };
    let theme = cx.theme;
    // Whoever went meanwhile is nobody to ask about.
    let named = cx.live.together.people.iter().find(|here| here.person.id == person && here.can_be_removed);
    let Some(name) = named.map(|here| here.person.name.clone()) else {
        cx.state.together.removing = None;
        return;
    };
    ui.add_space(8.0);
    let about = format!(
        "Remove {name} from the song? They keep it as it is now, and get nothing of what is done after. The \
         invitation becomes another: the one they have lets nobody in, whoever else is here is told the new one by \
         itself, and anyone who has not joined yet needs it from you."
    );
    line(ui, cx, &about, theme.ink);
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing = vec2(8.0, 0.0);
        if trill_noir::button(ui, theme, &format!("Remove {name}"), Variant::Primary).clicked() {
            cx.state.together.removing = None;
            // (It waits for the meeting point where the song is shared
            // through one: not on the thread that draws. What came of it
            // is said as everything that happens in the song is.)
            cx.sounds.quietly(move |runtime| runtime.remove_from_song(person).map(drop));
        }
        if trill_noir::button(ui, theme, "Keep them", Variant::Default).clicked() {
            cx.state.together.removing = None;
        }
    });
}

/// What someone holds, in a few words.
pub fn about(here: &Here, project: &Project) -> String {
    if let Some(holding) = &here.holding {
        return format!("has hold of {holding}");
    }
    let clip = here.clips.first().and_then(|id| project.clip(*id).ok()).map(|clip| clip.name.clone());
    let track = here.tracks.first().and_then(|id| project.track(*id).ok()).map(|track| track.name.clone());
    match (here.notes.len(), clip, track) {
        (0, None, None) => "is here".to_owned(),
        (0, None, Some(track)) => format!("on the track \u{201C}{track}\u{201D}"),
        (0, Some(clip), _) if here.clips.len() == 1 => format!("on the clip \u{201C}{clip}\u{201D}"),
        (0, Some(_), _) => format!("on {} clips", here.clips.len()),
        (1, Some(clip), _) => format!("on a note in \u{201C}{clip}\u{201D}"),
        (notes, Some(clip), _) => format!("on {notes} notes in \u{201C}{clip}\u{201D}"),
        (notes, None, _) => format!("on {notes} notes"),
    }
}

/// The panel, while it is open.
pub fn window(ctx: &egui::Context, cx: &mut Cx) {
    let Some(panel) = cx.state.together.panel else { return };
    let theme = cx.theme;
    let frame = egui::Frame::NONE
        .fill(theme.panel)
        .stroke(egui::Stroke::new(1.0, theme.hairline))
        .inner_margin(egui::Margin::symmetric(18, 14));
    let modal =
        egui::Modal::new(egui::Id::unique("together")).frame(frame).backdrop_color(theme.backdrop).show(ctx, |ui| {
            ui.set_width(WIDTH);
            ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
            let (title, _) = ui.allocate_exact_size(vec2(WIDTH, 18.0), Sense::hover());
            let words = match panel {
                Panel::Share => "Share this song",
                Panel::Join => "Join a song",
            };
            text::paint(ui.painter(), title, Align2::LEFT_CENTER, theme.text.caps, words, theme.ink);
            text::paint(ui.painter(), title, Align2::RIGHT_CENTER, theme.text.micro, "Esc closes", theme.ink_faint);
            ui.add_space(10.0);
            name_field(ui, cx);
            match panel {
                Panel::Share => share_panel(ui, cx),
                Panel::Join => join_panel(ui, cx),
            }
            audio(ui, cx);
            if cx.live.together.is_on() {
                heading(ui, cx, "Who is here");
                if let Some(person) = people(ui, cx) {
                    cx.state.together.removing = Some(person);
                }
                removing(ui, cx);
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing = vec2(8.0, 0.0);
                    if trill_noir::button(ui, theme, "Leave", Variant::Default).clicked() {
                        cx.sounds.runtime.leave();
                        cx.state.together.panel = None;
                    }
                });
            }
        });
    if modal.should_close() {
        cx.state.together.panel = None;
        // (A key that was typed and not kept is not kept by the window either.)
        cx.state.together.point_key.clear();
    }
}

fn share_panel(ui: &mut Ui, cx: &mut Cx) {
    let theme = cx.theme;
    let invitation = cx.live.together.invitation.clone();
    match invitation {
        None => {
            let about = "Someone else opens this song in their own Trill and you both change it at once. Nobody is \
                         the host: each of you has all of it, and can go on alone and merge again later. The song's \
                         audio files go to whoever joins. A plugin they have not got they hear as your Trill \
                         records it for them.";
            line(ui, cx, about, theme.ink_dim);
            what_goes(ui, cx);
            heading(ui, cx, "Who can reach this song");
            let (rect, _) = ui.allocate_exact_size(vec2(WIDTH, theme.metric.chip), Sense::hover());
            let labels = ["This computer only", "My network", "A meeting point"];
            let reaches = [Reached::ThisComputer, Reached::Network, Reached::MeetingPoint];
            let at = reaches.iter().position(|reach| *reach == cx.state.together.reach);
            let id = ui.scope_id().with("reach");
            if let (Some(chosen), _) = trill_noir::chip_row(ui, theme, rect.min, WIDTH, id, &labels, at, false) {
                cx.state.together.reach = reaches[chosen];
                cx.state.together.refusal = None;
            }
            ui.add_space(6.0);
            let reach = cx.state.together.reach;
            let about = match reach {
                Reached::ThisComputer => "Another Trill on this computer can join. Nothing is opened to any network.",
                Reached::Network => {
                    "Computers on the networks this one is on can join: at home, over a VPN, or through a port you \
                     forwarded. The first time, Windows asks whether Trill may be reached; say yes for the network \
                     you are on."
                }
                Reached::MeetingPoint => {
                    "Someone anywhere can join, each of you at home behind your own router. Both programs connect \
                     out to a meeting point, a small server that someone you know keeps, and it passes on what the \
                     two say to each other. It cannot read the song or join it. Nothing listens on this computer, \
                     so no firewall asks."
                }
            };
            line(ui, cx, about, theme.ink_faint);
            let mut ready = true;
            if reach == Reached::MeetingPoint {
                ui.add_space(8.0);
                ready = meeting_point(ui, cx);
            }
            ui.add_space(12.0);
            ui.add_enabled_ui(ready, |ui| {
                if trill_noir::button(ui, theme, "Share", Variant::Primary).clicked() {
                    let shared = match reach {
                        Reached::ThisComputer => cx.sounds.runtime.share(Reach::ThisMachine, 0),
                        Reached::Network => cx.sounds.runtime.share(Reach::Network, 0),
                        Reached::MeetingPoint => cx.sounds.runtime.share_through_meeting_point(),
                    };
                    match shared {
                        Ok(_) => cx.state.together.refusal = None,
                        Err(why) => cx.state.together.refusal = Some(why),
                    }
                }
            });
        }
        Some(invitation) => {
            line(ui, cx, "Hand this invitation to whoever is to join. They choose File, Join a song.", theme.ink_dim);
            ui.add_space(8.0);
            // Through a meeting point: whether the invitation lets anyone
            // in right now is the meeting point's to say.
            if let Some((at, meeting)) = cx.live.together.meeting.clone() {
                let (words, ink) = match meeting {
                    Meeting::Reaching => (
                        format!(
                            "Reaching the meeting point {at}\u{2026} The invitation lets nobody in until it is \
                             reached."
                        ),
                        theme.ink_dim,
                    ),
                    Meeting::Held => (
                        format!(
                            "Open at the meeting point {at}: whoever has the invitation gets in, from anywhere. \
                             Nothing listens on this computer."
                        ),
                        theme.ink_dim,
                    ),
                    Meeting::Away(why) => (
                        format!(
                            "The meeting point {at} is away: {why}. Trill goes on trying and the invitation stays \
                             the one it is; the song is yours to go on with meanwhile."
                        ),
                        theme.warn,
                    ),
                };
                line(ui, cx, &words, ink);
                ui.add_space(8.0);
            }
            let (rect, response) = ui.allocate_exact_size(vec2(WIDTH, 30.0), Sense::click());
            ui.painter().rect_filled(rect, 0.0, theme.bg);
            paint::frame(ui.painter(), rect, theme.hairline);
            let inner = rect.shrink2(vec2(8.0, 0.0));
            text::paint(ui.painter(), inner, Align2::LEFT_CENTER, theme.text.value, &invitation, theme.ink);
            response.widget_info(|| {
                egui::WidgetInfo::labeled(egui::Role::Label, true, format!("Invitation: {invitation}"))
            });
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing = vec2(8.0, 0.0);
                if trill_noir::button(ui, theme, "Copy the invitation", Variant::Primary).clicked()
                    || response.clicked()
                {
                    ui.ctx().copy_text(invitation.clone());
                    cx.state.toasts.info("The invitation is on the clipboard.");
                }
                if trill_noir::button(ui, theme, "Stop sharing", Variant::Default).clicked() {
                    cx.sounds.runtime.stop_sharing();
                }
            });
            ui.add_space(8.0);
            let about = "Whoever has it can read and change this song for as long as you share it, and nobody else \
                         can: what is said between you is encrypted. Stop sharing and it is no longer an invitation; \
                         whoever is here stays.";
            line(ui, cx, about, theme.ink_faint);
        }
    }
    if let Some(refusal) = cx.state.together.refusal.clone() {
        ui.add_space(8.0);
        line(ui, cx, &refusal, theme.signal);
    }
}

/// Megabytes as the panel says them.
fn megabytes(bytes: u64) -> String {
    let megabytes = bytes as f64 / f64::from(1 << 20);
    if megabytes < 10.0 { format!("{megabytes:.1} MB") } else { format!("{megabytes:.0} MB") }
}

/// Before the song is shared: what goes with it. How many audio files,
/// how large, the folders they lie in, and one by one the files that lie
/// outside the song's own folder, Trill's recordings and the library.
fn what_goes(ui: &mut Ui, cx: &mut Cx) {
    let theme = cx.theme;
    heading(ui, cx, "What goes with it");
    let going = cx.state.together.going.lock().clone();
    let Some(going) = going else {
        line(ui, cx, "Looking at the song's audio files\u{2026}", theme.ink_faint);
        ui.ctx().request_repaint_after(Duration::from_millis(100));
        return;
    };
    // What the song names that is no audio stays, and is shown all the
    // same: a song file someone else wrote may name anything.
    let stays = |ui: &mut Ui, cx: &mut Cx| {
        if going.no_audio.is_empty() {
            return;
        }
        ui.add_space(4.0);
        let about = match going.no_audio.len() {
            1 => "The song names 1 file that is no audio as Trill reads it. It stays here:".to_owned(),
            count => format!("The song names {count} files that are no audio as Trill reads them. They stay here:"),
        };
        line(ui, cx, &about, theme.ink_dim);
        for path in going.no_audio.iter().take(LISTED) {
            line(ui, cx, path, theme.ink_faint);
        }
        if going.no_audio.len() > LISTED {
            line(ui, cx, &format!("and {} more", going.no_audio.len() - LISTED), theme.ink_faint);
        }
    };
    if going.files == 0 {
        line(
            ui,
            cx,
            "The song names no audio files on this computer: nothing but the song itself goes.",
            theme.ink_dim,
        );
        stays(ui, cx);
        return;
    }
    let files = if going.files == 1 { "1 audio file".to_owned() } else { format!("{} audio files", going.files) };
    let said =
        format!("{files}, {} together, to whoever joins. Nobody is told where you keep them.", megabytes(going.bytes));
    line(ui, cx, &said, theme.ink_dim);
    for (folder, count, bytes) in &going.folders {
        let files = if *count == 1 { "1 file".to_owned() } else { format!("{count} files") };
        line(ui, cx, &format!("{files} in {folder} \u{00B7} {}", megabytes(*bytes)), theme.ink_faint);
    }
    if !going.elsewhere.is_empty() {
        ui.add_space(4.0);
        line(ui, cx, "From outside the song's folder, Trill's recordings and your sample library:", theme.warn);
        for path in going.elsewhere.iter().take(LISTED) {
            line(ui, cx, path, theme.ink);
        }
        if going.elsewhere.len() > LISTED {
            line(ui, cx, &format!("and {} more", going.elsewhere.len() - LISTED), theme.ink_faint);
        }
    }
    stays(ui, cx);
}

/// How many files a list in the panel names one by one.
const LISTED: usize = 8;

/// What the panel says of a clip or a sample whose file nobody has said:
/// it is with whoever brought it into the song, and theirs to put right.
fn unsaid_words(place: &str) -> String {
    format!(
        "{place} \u{00B7} waits for whoever brought it: their Trill has not said what its file is (it is reading \
         it, has not got it, or holds it back)"
    )
}

/// While the song is shared: its audio that is on its way here, and the
/// files on this computer that have not gone to the others.
fn audio(ui: &mut Ui, cx: &mut Cx) {
    let theme = cx.theme;
    let (files, held, wait) =
        (cx.live.together.files.clone(), cx.live.together.held.clone(), cx.live.together.files_wait);
    if !files.is_empty() {
        heading(ui, cx, "Audio on its way");
        for file in files.iter().take(LISTED) {
            // One that nobody has said: only the place that plays it is
            // known, and whose it is to put right.
            if file.waits == Some(Waiting::ToBeSaid) {
                line(ui, cx, &unsaid_words(&file.name), theme.ink_dim);
                continue;
            }
            let about = match (&file.waits, &file.from) {
                (Some(Waiting::ForAWord), _) => "waits for your word".to_owned(),
                (Some(Waiting::ForRoom { short }), _) => {
                    format!("waits for room on the disk: {} are missing", megabytes(*short))
                }
                (Some(Waiting::ForTheWay { seconds }), _) => {
                    format!("waits for the meeting point's next day, about {} min", seconds.div_ceil(60))
                }
                (Some(Waiting::ForSomeone), _) => "nobody who is here has it".to_owned(),
                // (Said above.)
                (Some(Waiting::ToBeSaid), _) => "waits for whoever brought it".to_owned(),
                (None, Some(from)) => format!("from {from}"),
                (None, None) => "arriving".to_owned(),
            };
            let said = format!(
                "{} \u{00B7} {} of {} \u{00B7} {about}",
                file.name,
                megabytes(file.here),
                megabytes(file.bytes)
            );
            line(ui, cx, &said, theme.ink_dim);
        }
        if files.len() > LISTED {
            line(ui, cx, &format!("and {} more", files.len() - LISTED), theme.ink_faint);
        }
        if files.iter().any(|file| file.waits == Some(Waiting::ForAWord)) {
            ui.add_space(6.0);
            let waiting: u64 =
                files.iter().filter(|file| file.waits.is_some()).map(|file| file.bytes - file.here).sum();
            let said = format!(
                "More audio is on its way than arrives without your word: {} wait. The song goes on meanwhile.",
                megabytes(waiting)
            );
            line(ui, cx, &said, theme.warn);
            ui.add_space(6.0);
            if trill_noir::button(ui, theme, "Let them arrive", Variant::Primary).clicked() {
                cx.sounds.runtime.let_files_arrive();
            }
        }
        ui.ctx().request_repaint_after(Duration::from_millis(250));
    }
    if let Some(seconds) = wait {
        heading(ui, cx, "Audio waits");
        let said = format!(
            "The meeting point has nearly passed what its keeper lets a key pass in a day. The song goes on; its \
             audio files go on in about {} min.",
            seconds.div_ceil(60)
        );
        line(ui, cx, &said, theme.warn);
    }
    if !held.is_empty() {
        heading(ui, cx, "Held on this computer");
        let about = "The song names these files here, and nobody was shown them: a song file that was opened named \
                     them, or something other than your own hand did. They have not gone to anyone.";
        line(ui, cx, about, theme.ink_dim);
        ui.add_space(4.0);
        for path in held.iter().take(LISTED) {
            line(ui, cx, path, theme.ink);
        }
        if held.len() > LISTED {
            line(ui, cx, &format!("and {} more", held.len() - LISTED), theme.ink_faint);
        }
        ui.add_space(6.0);
        if trill_noir::button(ui, theme, "Let them go", Variant::Default).clicked() {
            cx.sounds.runtime.let_files_go();
        }
    }
}

/// A row of the panel with a caption and a field after it. Returns the
/// field's place.
fn captioned(ui: &mut Ui, cx: &Cx, caption: &str) -> Rect {
    let theme = cx.theme;
    let (rect, _) = ui.allocate_exact_size(vec2(WIDTH, theme.metric.control), Sense::hover());
    let label = rect.with_max_x(rect.left() + 110.0);
    text::paint(ui.painter(), label, Align2::LEFT_CENTER, theme.text.micro, caption, theme.ink_faint);
    rect.with_min_x(label.right())
}

/// The meeting point a song is shared through: the one that is set, or
/// the two fields that set one. Returns whether there is one to share
/// through.
fn meeting_point(ui: &mut Ui, cx: &mut Cx) -> bool {
    let theme = cx.theme;
    let set = cx.state.together.point.clone();
    if let Some(point) = set.as_ref().filter(|_| !cx.state.together.changing_point) {
        let words = format!(
            "Through {} \u{00B7} the key its keeper gave you is kept in this computer's settings",
            point.address
        );
        let (rect, response) = ui.allocate_exact_size(vec2(WIDTH, ROW), Sense::hover());
        text::paint(ui.painter(), rect, Align2::LEFT_CENTER, theme.text.body, &words, theme.ink);
        response.widget_info(|| {
            egui::WidgetInfo::labeled(egui::Role::Label, true, format!("Meeting point: {}", point.address))
        });
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing = vec2(8.0, 0.0);
            if trill_noir::button(ui, theme, "Change the meeting point", Variant::Default).clicked() {
                let together = &mut cx.state.together;
                (together.changing_point, together.point_address) = (true, point.address.clone());
                together.point_key.clear();
            }
            if trill_noir::button(ui, theme, "Forget it", Variant::Default).clicked() {
                cx.state.together.point = None;
                cx.sounds.quietly(|runtime| runtime.forget_meeting_point());
            }
        });
        return true;
    }
    let about = "Its keeper gives you two things: where it is, and a key that is yours. Both are kept in this \
                 computer's settings and nowhere else: not in a song, and the key is shown to nobody again.";
    line(ui, cx, about, theme.ink_faint);
    ui.add_space(8.0);
    let field = captioned(ui, cx, "Address").with_max_x(ui.min_rect().left() + 110.0 + 300.0);
    let id = egui::Id::unique("together-point-address");
    let address =
        trill_noir::text_field(ui, theme, field, id, &mut cx.state.together.point_address, "meet.example.org:443");
    address.widget_info(|| egui::WidgetInfo::labeled(egui::Role::TextInput, true, "Meeting point's address"));
    ui.add_space(6.0);
    let field = captioned(ui, cx, "Key");
    let id = egui::Id::unique("together-point-key");
    let hint = match set.is_some() {
        true => "Leave empty to keep the key you have",
        false => "Ten groups of four letters, as its keeper wrote them",
    };
    let key = trill_noir::secret_field(ui, theme, field, id, &mut cx.state.together.point_key, hint);
    key.widget_info(|| egui::WidgetInfo::labeled(egui::Role::TextInput, true, "Meeting point's key"));
    ui.add_space(8.0);
    let typed = !cx.state.together.point_address.trim().is_empty();
    let keyed = !cx.state.together.point_key.trim().is_empty() || set.is_some();
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing = vec2(8.0, 0.0);
        ui.add_enabled_ui(typed && keyed, |ui| {
            if trill_noir::button(ui, theme, "Keep this meeting point", Variant::Default).clicked() {
                let together = &mut cx.state.together;
                // The key leaves the window with this, whatever comes of it.
                let key = Some(std::mem::take(&mut together.point_key)).filter(|key| !key.trim().is_empty());
                let address = together.point_address.clone();
                match Runtime::read_meeting_point(&address, key.as_deref()) {
                    Ok(shown) => {
                        together.point = Some(MeetingPointSet { address: shown, has_key: true });
                        (together.changing_point, together.refusal) = (false, None);
                        cx.sounds.quietly(move |runtime| runtime.set_meeting_point(&address, key.as_deref()));
                    }
                    Err(why) => together.refusal = Some(why),
                }
            }
        });
        if set.is_some() && trill_noir::button(ui, theme, "Cancel", Variant::Default).clicked() {
            cx.state.together.changing_point = false;
            cx.state.together.point_key.clear();
        }
    });
    false
}

fn join_panel(ui: &mut Ui, cx: &mut Cx) {
    let theme = cx.theme;
    line(ui, cx, "Paste the invitation the other person's Trill shows (File, Share this song).", theme.ink_dim);
    ui.add_space(8.0);
    let (rect, _) = ui.allocate_exact_size(vec2(WIDTH, 30.0), Sense::hover());
    let id = egui::Id::unique("together-invitation");
    let hint = "trill:HOST:PORT:SECRET, or trill:meet:HOST:PORT:SECRET";
    trill_noir::text_field(ui, theme, rect, id, &mut cx.state.together.invitation, hint);
    if std::mem::take(&mut cx.state.together.focus) {
        ui.memory_mut(|memory| memory.request_focus(id.with("editor")));
    }
    ui.add_space(8.0);
    // A song with changes that are not saved is not let go of unasked.
    let (modified, shared) = (cx.snap.modified, cx.snap.shared);
    if modified && !shared {
        let (rect, _) = ui.allocate_exact_size(vec2(WIDTH, theme.metric.chip), Sense::hover());
        let chip = Chip::new("Let the open song go", cx.state.together.replace);
        let width = chip.width(ui, theme);
        let at = Rect::from_min_size(rect.min, vec2(width, theme.metric.chip));
        if trill_noir::chip(ui, theme, at, ui.scope_id().with("replace"), &chip).clicked() {
            cx.state.together.replace = !cx.state.together.replace;
        }
        ui.add_space(6.0);
        let about = "Joining opens their song in place of the one that is open, which has changes that are not \
                     saved. Save it first, or say that it may go.";
        line(ui, cx, about, theme.ink_faint);
        ui.add_space(8.0);
    } else if shared {
        let about = "If this is the song you already share with them, the two are merged: what each of you did \
                     while apart is kept.";
        line(ui, cx, about, theme.ink_faint);
        ui.add_space(8.0);
    }
    let ready = !cx.state.together.invitation.trim().is_empty() && !cx.live.together.joining;
    ui.add_enabled_ui(ready, |ui| {
        let label = if cx.live.together.joining { "Joining\u{2026}" } else { "Join" };
        if trill_noir::button(ui, theme, label, Variant::Primary).clicked() {
            let replace = cx.state.together.replace || !modified;
            match cx.sounds.runtime.join(&cx.state.together.invitation, replace) {
                Ok(()) => cx.state.together.refusal = None,
                Err(why) => cx.state.together.refusal = Some(why),
            }
        }
    });
    if let Some(refusal) = cx.state.together.refusal.clone() {
        ui.add_space(8.0);
        line(ui, cx, &refusal, theme.signal);
    }
}

/// The notices, as cards above `bottom_right`: each the sentence, the
/// button that puts one's own back where there is one, and a way to take
/// it as read. Returns the top of the highest card, for whatever is
/// stacked above them.
pub fn notices(ctx: &egui::Context, cx: &mut Cx, bottom_right: egui::Pos2) -> f32 {
    let theme = cx.theme;
    let notices = cx.live.together.notices.clone();
    let mut bottom = bottom_right.y;
    for notice in notices.iter().rev().take(4) {
        let mut act = None;
        let area = egui::Area::new(egui::Id::unique(("notice", notice.id)))
            .order(egui::Order::Foreground)
            .pivot(Align2::RIGHT_BOTTOM)
            .fixed_pos(pos2(bottom_right.x, bottom))
            .show(ctx, |ui| {
                let frame = egui::Frame::NONE
                    .fill(theme.raised)
                    .stroke(egui::Stroke::new(1.0, theme.warn))
                    .inner_margin(egui::Margin { left: 12, right: 12, top: 10, bottom: 10 });
                frame.show(ui, |ui| {
                    ui.set_width(NOTICE);
                    ui.spacing_mut().item_spacing = vec2(8.0, 8.0);
                    ui.add(egui::Label::new(egui::RichText::new(&notice.text).color(theme.ink)).wrap());
                    ui.horizontal(|ui| {
                        if let Some(remedy) = &notice.remedy
                            && trill_noir::button(ui, theme, &remedy.label, Variant::Primary).clicked()
                        {
                            act = Some(true);
                        }
                        if trill_noir::button(ui, theme, "Leave it", Variant::Default).clicked() {
                            act = Some(false);
                        }
                    });
                });
            });
        bottom = area.response.rect.top() - 6.0;
        match act {
            Some(true) => {
                let id = notice.id;
                cx.try_session(|session| session.remedy(id));
            }
            Some(false) => {
                let id = notice.id;
                cx.with_session(|session| session.dismiss_notice(id));
            }
            None => {}
        }
    }
    bottom
}

/// Where each of the others' songs stands, over `area` whose top is the
/// ruler's: a line in their colour and their name. `x_of` turns a beat
/// into a place on screen.
pub fn playheads(painter: &Painter, cx: &Cx, area: Rect, x_of: impl Fn(f64) -> f32) {
    let theme = cx.theme;
    let map = cx.snap.project.tempo_map();
    for here in &cx.live.together.people {
        // A song that plays has moved on since it was last heard of.
        let beat = match here.playing {
            true => map.beat_at_seconds(map.seconds_at(here.beat) + here.heard.elapsed().as_secs_f64()),
            false => here.beat,
        };
        let x = x_of(beat).round();
        if x < area.left() - 6.0 || x > area.right() + 6.0 {
            continue;
        }
        let colour = theme.person(here.person.colour);
        trill_noir::person_playhead(painter, x, area.top()..=area.bottom(), colour);
        trill_noir::person_tag(painter, theme, pos2(x + 5.0, area.top() + 1.0), &here.person.name, colour);
    }
}

/// Marks a clip that someone else has selected: an outline in their
/// colour and their name at its top right.
pub fn mark_clip(painter: &Painter, cx: &Cx, rect: Rect, clip: ClipId) {
    let Some(here) = cx.live.together.on_clip(clip) else { return };
    let colour = cx.theme.person(here.person.colour);
    trill_noir::person_outline(painter, rect, colour);
    let width = trill_noir::person_tag_width(painter, cx.theme, &here.person.name);
    if rect.width() > width + 8.0 {
        trill_noir::person_tag(painter, cx.theme, pos2(rect.right() - width, rect.top()), &here.person.name, colour);
    }
}

/// Marks a track's header that someone else has selected.
pub fn mark_track(painter: &Painter, cx: &Cx, rect: Rect, track: TrackId) {
    if let Some(here) = cx.live.together.on_track(track) {
        trill_noir::person_row(painter, rect, cx.theme.person(here.person.colour));
    }
}

/// Marks a note that someone else has selected.
pub fn mark_note(painter: &Painter, cx: &Cx, rect: Rect, clip: ClipId, note: u64) {
    if let Some(here) = cx.live.together.on_note(clip, note) {
        trill_noir::person_outline(painter, rect.expand(1.0), cx.theme.person(here.person.colour));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_status_bar_says_who_is_here_and_the_panel_what_each_holds() {
        let mut project = Project::new("Ours");
        let keys = project.add_track("Keys");
        let bass = project.add_track("Bass");
        let verse = project.add_midi_clip(keys, "Verse", 0.0, 8.0).unwrap();
        project.add_midi_clip(bass, "Line", 0.0, 8.0).unwrap();
        assert_eq!(status_words(&Seen::default()), None, "nothing while the song is one's own");
        let waiting = TogetherScene::Waiting.of(&project);
        assert_eq!(status_words(&waiting).as_deref(), Some("Shared \u{00B7} nobody else is here"));
        let two = TogetherScene::Two.of(&project);
        assert_eq!(status_words(&two).as_deref(), Some("Here: Jo, Sam"));
        // Through a meeting point, and what the bar says while something
        // is away and being tried again.
        let meeting = TogetherScene::Meeting.of(&project);
        assert_eq!(status_words(&meeting).as_deref(), Some("Here: Jo"));
        assert!(meeting.invitation.as_deref().is_some_and(|invitation| invitation.starts_with("trill:meet:")));
        let away =
            |meeting| Seen { meeting: Some(("meet.example.org:443".to_owned(), meeting)), ..meeting_seen(&project) };
        assert_eq!(
            status_words(&away(Meeting::Away("it did not answer".into()))).as_deref(),
            Some("Here: Jo \u{00B7} meeting point away, trying again")
        );
        assert_eq!(
            status_words(&away(Meeting::Reaching)).as_deref(),
            Some("Here: Jo \u{00B7} reaching the meeting point")
        );
        let lost = TogetherScene::Lost.of(&project);
        assert!(lost.is_on(), "someone whose connection was lost is still in a shared song");
        assert_eq!(status_words(&lost).as_deref(), Some("Connection to Jo lost \u{00B7} trying again"));
        let both = Seen { away: lost.away.clone(), ..two.clone() };
        assert_eq!(status_words(&both).as_deref(), Some("Here: Jo, Sam \u{00B7} connection to Jo lost, trying again"));
        assert_eq!(two.on_track(bass).map(|here| here.person.name.as_str()), Some("Jo"));
        assert_eq!(about(&two.people[0], &project), "has hold of the volume of the track \u{201C}Bass\u{201D}");
        let on = |clips: Vec<ClipId>, notes: usize| Here {
            clips,
            notes: (0..notes as u64).map(|note| (verse, note)).collect(),
            holding: None,
            ..two.people[1].clone()
        };
        assert_eq!(about(&on(vec![verse], 0), &project), "on the clip \u{201C}Verse\u{201D}");
        assert_eq!(about(&on(vec![verse], 3), &project), "on 3 notes in \u{201C}Verse\u{201D}");
        assert_eq!((TogetherScene::named("two"), TogetherScene::named("nope")), (Some(TogetherScene::Two), None));
        assert_eq!(TogetherScene::named("lost"), Some(TogetherScene::Lost));
    }

    fn meeting_seen(project: &Project) -> Seen {
        TogetherScene::Meeting.of(project)
    }
}
