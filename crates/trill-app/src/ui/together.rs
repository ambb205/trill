//! Working in one song with other people: the window's part.
//!
//! Everything that merges is the model's (`trill_core::merge`), the
//! session's and the runtime's ([`trill_runtime::Runtime::share`], `join`,
//! `together`). The window does four things with it:
//!
//! * **File, Share this song and Join a song** open a panel: the name one
//!   goes by, where to be reached, the invitation to hand to someone or
//!   the one that was handed over.
//! * **Who is here** is in the status bar while the song is shared, each
//!   person in their colour ([`trill_noir::people`]); a click opens the
//!   panel.
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
use trill_runtime::{Here, Mine, Person, Reach, Runtime};

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

/// What the window keeps for working together: what its panel shows and
/// what it last told the others.
#[derive(Default)]
pub struct TogetherState {
    pub panel: Option<Panel>,
    /// The invitation as it is typed or pasted into the Join panel.
    pub invitation: String,
    /// The name as it is typed. Empty until the panel first opens.
    pub name: String,
    /// Whether to be reached from the network, or from this computer only.
    pub network: bool,
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
}

impl Seen {
    /// Whether the song is being worked in with others, or offered to be.
    pub fn is_on(&self) -> bool {
        self.invitation.is_some() || self.joining || self.connected > 0
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
}

impl TogetherScene {
    pub const ALL: [(&'static str, TogetherScene); 2] = [("two", TogetherScene::Two), ("waiting", TogetherScene::Waiting)];

    pub fn named(name: &str) -> Option<Self> {
        Self::ALL.iter().find(|(known, _)| *known == name).map(|(_, scene)| *scene)
    }

    /// What the window would see in the scene: the same every time, from
    /// what the song holds.
    pub fn of(self, project: &Project) -> Seen {
        let me = Person { id: 3, name: "Allen".into(), colour: 3 };
        let invitation = Some("trill:192.168.1.20:47474:7Q2M-K9TX-4HNB-V3RC-8WJD-6PFA-ZY5E-1GSK".to_owned());
        if self == TogetherScene::Waiting {
            return Seen { me: Some(me), invitation, ..Seen::default() };
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
        };
        Seen { me: Some(me), invitation, joining: false, connected: 2, people: vec![jo, sam], notices: Vec::new() }
    }
}

/// Brings what the window holds of the others up to date, says what there
/// is to say, and tells the others what this window's person has
/// selected. Once a frame, without the session.
pub fn sync(state: &mut UiState, live: &mut Live, snapshot: &Snapshot, runtime: &Runtime, options: &UiOptions) {
    if let Some(scene) = options.together_scene {
        let notices = std::mem::take(&mut live.together.notices);
        live.together = scene.of(&snapshot.project);
        live.together.notices = notices;
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
    };
    // What happened: who came, who went and why, what was refused.
    for (number, sentence) in runtime.said_together(state.together.heard) {
        state.together.heard = number;
        let refused = sentence.starts_with("Not joined") || sentence.starts_with("No connection");
        if refused {
            state.together.refusal = Some(sentence.clone());
            state.toasts.lasting(ToastKind::Error, sentence, Duration::from_secs(12));
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
    let mut mine = Mine { tracks: state.selected_track.into_iter().collect(), clips: state.selected_clips.clone(), ..Mine::default() };
    if let Some(clip) = state.selected_clip.and_then(|id| project.clip(id).ok()) {
        // Notes are selected by what they are; the others are told which.
        let held = |note: &trill_core::Note| clip.notes().iter().find(|found| *found == note).map(|found| (clip.id, found.id));
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
    Some(match (names.as_slice(), seen.joining) {
        ([], true) => "Joining\u{2026}".to_owned(),
        ([], false) => "Shared \u{00B7} nobody else is here".to_owned(),
        (names, _) => format!("Here: {}", names.join(", ")),
    })
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
                runtime.update_settings(|settings| settings.together.name = Some(remembered)).map_err(|error| error.to_string())
            });
        }
    }
    ui.add_space(6.0);
}

/// Everyone who is here, one a row: their colour, their name, what they
/// hold.
fn people(ui: &mut Ui, cx: &Cx) {
    let theme = cx.theme;
    let seen = &cx.live.together;
    let mut row = |colour: u8, name: &str, about: &str| {
        let (rect, response) = ui.allocate_exact_size(vec2(WIDTH, ROW), Sense::hover());
        trill_noir::person_dot(ui.painter(), pos2(rect.left() + 4.0, rect.center().y), theme.person(colour));
        let name_room = rect.with_min_x(rect.left() + 16.0).with_max_x(rect.left() + 150.0);
        text::paint(ui.painter(), name_room, Align2::LEFT_CENTER, theme.text.body, name, theme.ink);
        let about_room = rect.with_min_x(rect.left() + 156.0);
        text::paint(ui.painter(), about_room, Align2::LEFT_CENTER, theme.text.micro, about, theme.ink_faint);
        response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Label, true, format!("{name}: {about}")));
    };
    if let Some(me) = &seen.me {
        row(me.colour, &me.name, "you");
    }
    for here in &seen.people {
        row(here.person.colour, &here.person.name, &about(here, &cx.snap.project));
    }
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
    let modal = egui::Modal::new(egui::Id::unique("together")).frame(frame).backdrop_color(theme.backdrop).show(ctx, |ui| {
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
        if cx.live.together.is_on() {
            heading(ui, cx, "Who is here");
            people(ui, cx);
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
    }
}

fn share_panel(ui: &mut Ui, cx: &mut Cx) {
    let theme = cx.theme;
    let invitation = cx.live.together.invitation.clone();
    match invitation {
        None => {
            let about = "Someone else opens this song in their own Trill and you both change it at once. Nobody is \
                         the host: each of you has all of it, and can go on alone and merge again later. They need \
                         the same plugins and audio files to hear what you hear.";
            line(ui, cx, about, theme.ink_dim);
            heading(ui, cx, "Who can reach this computer");
            let (rect, _) = ui.allocate_exact_size(vec2(WIDTH, theme.metric.chip), Sense::hover());
            let labels = ["This computer only", "My network"];
            let at = usize::from(cx.state.together.network);
            let id = ui.scope_id().with("reach");
            if let (Some(chosen), _) = trill_noir::chip_row(ui, theme, rect.min, WIDTH, id, &labels, Some(at), false) {
                cx.state.together.network = chosen == 1;
            }
            ui.add_space(6.0);
            let about = match cx.state.together.network {
                false => "Another Trill on this computer can join. Nothing is opened to any network.",
                true => {
                    "Computers on the networks this one is on can join: at home, over a VPN, or through a port you \
                     forwarded. The first time, Windows asks whether Trill may be reached; say yes for the network \
                     you are on."
                }
            };
            line(ui, cx, about, theme.ink_faint);
            ui.add_space(12.0);
            if trill_noir::button(ui, theme, "Share", Variant::Primary).clicked() {
                let reach = if cx.state.together.network { Reach::Network } else { Reach::ThisMachine };
                match cx.sounds.runtime.share(reach, 0) {
                    Ok(_) => cx.state.together.refusal = None,
                    Err(why) => cx.state.together.refusal = Some(why),
                }
            }
        }
        Some(invitation) => {
            line(ui, cx, "Hand this invitation to whoever is to join. They choose File, Join a song.", theme.ink_dim);
            ui.add_space(8.0);
            let (rect, response) = ui.allocate_exact_size(vec2(WIDTH, 30.0), Sense::click());
            ui.painter().rect_filled(rect, 0.0, theme.bg);
            paint::frame(ui.painter(), rect, theme.hairline);
            let inner = rect.shrink2(vec2(8.0, 0.0));
            text::paint(ui.painter(), inner, Align2::LEFT_CENTER, theme.text.value, &invitation, theme.ink);
            response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Label, true, format!("Invitation: {invitation}")));
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing = vec2(8.0, 0.0);
                if trill_noir::button(ui, theme, "Copy the invitation", Variant::Primary).clicked() || response.clicked() {
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

fn join_panel(ui: &mut Ui, cx: &mut Cx) {
    let theme = cx.theme;
    line(ui, cx, "Paste the invitation the other person's Trill shows (File, Share this song).", theme.ink_dim);
    ui.add_space(8.0);
    let (rect, _) = ui.allocate_exact_size(vec2(WIDTH, 30.0), Sense::hover());
    let id = egui::Id::unique("together-invitation");
    trill_noir::text_field(ui, theme, rect, id, &mut cx.state.together.invitation, "trill:HOST:PORT:SECRET");
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
    }
}
