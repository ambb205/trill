//! The Mixer tab: a channel strip for every track, in the order the
//! arrangement shows them, and the master's pinned at the right.
//!
//! A strip reads top to bottom as the signal flows: the chain's devices
//! (a plugin's with the button for its own window), the sends, pan, then
//! the fader beside its meter, and mute and solo at the foot. Every strip
//! is laid out to the same plan, whatever it holds,
//! so that faders, pans and sends line up across the row. The plan fits
//! the detail panel's height: the device lists and then the sends give
//! way before the faders get too short to use. With many tracks the strips
//! keep a width that still fits a name and the row scrolls sideways.

use super::arrangement::{self, display_order};
use super::automation::{self, Hold};
use super::state::{Cx, DetailTab};
use super::{colors, editors, recording, units, widgets};
use eframe::egui::{self, Align2, Color32, Rect, Sense, Ui, pos2, vec2};
use trill_core::{AutomationTarget, Device, Track, TrackId, TrackKind};
use trill_noir::{Chip, Fader, Icon, fade, paint, text};

/// Strips are never narrower than this, so thirty tracks scroll rather
/// than squeeze, and never wider than `MAX_STRIP`, so three do not sprawl.
const MIN_STRIP: f32 = 82.0;
const MAX_STRIP: f32 = 116.0;
const MASTER_STRIP: f32 = 104.0;
/// Space between a strip's edge and its controls.
const PAD: f32 = 7.0;
const COLOR_BAR: f32 = 3.0;
const NAME_ROW: f32 = 22.0;
const READOUT_ROW: f32 = 16.0;
/// The shortest fader worth dragging.
const MIN_FADER: f32 = 64.0;
const FADER_WIDTH: f32 = 22.0;
const METER_WIDTH: f32 = 11.0;
/// At most this many devices are listed before "+ n more".
const MAX_DEVICES: usize = 4;

/// What a strip stands for.
#[derive(Copy, Clone)]
enum Channel<'a> {
    Track(&'a Track),
    Master,
}

/// How much of a strip's height each part gets. One plan for the whole
/// row, so the strips' controls line up.
#[derive(Copy, Clone, Debug, PartialEq)]
struct Budget {
    /// Rows for devices: as many as the longest chain needs, if they fit.
    devices: usize,
    /// Rows for sends, of the return tracks there are.
    sends: usize,
    fader: f32,
}

/// Divides `height` (what is left of a strip under its name) between
/// `devices` rows of devices, `sends` send bars and the fader. `extras` is
/// what the rows every strip has take. The fader keeps [`MIN_FADER`] if at
/// all possible; sends come before devices, since a send can be set
/// nowhere else at this size and a device can.
fn budget(height: f32, row: f32, devices: usize, sends: usize, extras: f32) -> Budget {
    let mut left = height - extras - MIN_FADER;
    let fits = |left: f32, wanted: usize| ((left / row).floor().max(0.0) as usize).min(wanted);
    let sends = fits(left, sends);
    left -= sends as f32 * row;
    let devices = fits(left, devices.min(MAX_DEVICES));
    left -= devices as f32 * row;
    Budget { devices, sends, fader: (MIN_FADER + left).max(0.0) }
}

/// Width of each track's strip when `count` of them share `width`.
fn strip_width(width: f32, count: usize) -> f32 {
    (width / count.max(1) as f32).floor().clamp(MIN_STRIP, MAX_STRIP)
}

pub fn show(ui: &mut Ui, cx: &mut Cx) {
    let theme = cx.theme;
    let area = ui.max_rect();
    ui.painter().rect_filled(area, 0.0, theme.bg);
    let tracks = &cx.snap.project.tracks;
    let order = display_order(tracks);

    // The master keeps its place at the right while the tracks scroll.
    let master = Rect::from_min_max(pos2((area.right() - MASTER_STRIP).max(area.left()), area.top()), area.max);
    let rest = Rect::from_min_max(area.min, pos2(master.left(), area.bottom()));
    let width = strip_width(rest.width(), order.len());

    // The plan every strip follows: rows for the longest chain and for
    // every return track, then pan, the fader, its readout, mute and solo.
    let row = theme.metric.bar + theme.metric.gap;
    let chains = tracks.iter().map(|track| track.devices.len()).chain([cx.snap.project.master.devices.len()]);
    let returns = tracks.iter().filter(|track| track.kind == TrackKind::Return).count();
    let under_name = area.height() - (COLOR_BAR + 3.0 + NAME_ROW - 1.0 + PAD);
    let extras = READOUT_ROW + 2.0 * row + 4.0;
    let shares = budget(under_name, row, chains.max().unwrap_or(0), returns, extras);

    widgets::place(ui, rest, "strips", egui::Layout::left_to_right(egui::Align::Min), |ui| {
        ui.set_clip_rect(rest.intersect(ui.clip_rect()));
        egui::ScrollArea::horizontal().id_salt("mixer").auto_shrink([false, false]).show(ui, |ui| {
            let content = vec2((width * order.len() as f32).max(1.0), rest.height());
            let (row, _) = ui.allocate_exact_size(content, Sense::hover());
            for (position, (index, member)) in order.iter().enumerate() {
                let rect = Rect::from_min_size(
                    pos2(row.left() + position as f32 * width, row.top()),
                    vec2(width, row.height()),
                );
                // Thirty strips are laid out; only those in view are built.
                if ui.is_rect_visible(rect) {
                    strip(ui, cx, Channel::Track(&tracks[*index]), rect, *member, shares);
                }
            }
        });
    });
    strip(ui, cx, Channel::Master, master, false, shares);
    paint::vline(ui.painter(), master.left(), master.y_range(), theme.hairline);
}

fn strip(ui: &mut Ui, cx: &mut Cx, channel: Channel, rect: Rect, member: bool, shares: Budget) {
    let theme = cx.theme;
    let snapshot = cx.snap;
    let (bar, gap) = (theme.metric.bar, theme.metric.gap);
    let row = bar + gap;
    let id = match channel {
        Channel::Track(track) => ui.scope_id().with(("strip", track.id)),
        Channel::Master => ui.scope_id().with("master-strip"),
    };
    let selected = match channel {
        Channel::Track(track) => cx.state.selected_track == Some(track.id) && !cx.state.master_selected,
        Channel::Master => cx.state.master_selected,
    };
    let surface = if selected { theme.raised } else { theme.panel };
    let (name, color, devices) = match channel {
        Channel::Track(track) => {
            (track.name.as_str(), colors::track_color(theme, track.color), track.devices.as_slice())
        }
        Channel::Master => ("Master", theme.ink, snapshot.project.master.devices.as_slice()),
    };

    // Registered first, so the controls placed over it take the pointer.
    let background = ui.interact(rect, id.with("background"), Sense::click());
    let painter = ui.painter().clone();
    painter.rect_filled(rect, 0.0, surface);
    paint::vline_left_of(&painter, rect.right(), rect.y_range(), theme.hairline);
    let muted = matches!(channel, Channel::Track(track) if track.mute);
    let top_bar = Rect::from_min_max(rect.min, pos2(rect.right() - 1.0, rect.top() + COLOR_BAR));
    painter.rect_filled(top_bar, 0.0, if muted { fade(color, 0.35) } else { color });
    if member {
        // A member of a group carries the group's color under its own.
        let group = match channel {
            Channel::Track(track) => track.group.and_then(|group| snapshot.track(group)),
            Channel::Master => None,
        };
        let hue = group.map_or(theme.ghost, |group| colors::track_color(theme, group.color));
        let tie = Rect::from_min_max(
            pos2(rect.left(), top_bar.bottom() + 1.0),
            pos2(rect.right() - 1.0, top_bar.bottom() + 3.0),
        );
        painter.rect_filled(tie, 0.0, fade(hue, 0.6));
    }
    cx.hint(&background, format!("{name} \u{00B7} click to select"));
    if background.clicked() {
        select(cx, channel);
    }

    let inner = Rect::from_min_max(
        pos2(rect.left() + PAD, rect.top() + COLOR_BAR + 3.0),
        pos2(rect.right() - PAD - 1.0, rect.bottom() - PAD),
    );
    let name_row = Rect::from_min_size(inner.min, vec2(inner.width(), NAME_ROW - 4.0));
    let style = match channel {
        Channel::Track(_) => trill_noir::TextStyle { size: 12.0, ..theme.text.body },
        Channel::Master => theme.text.caps,
    };
    let ink = if muted { theme.ink_dim } else { theme.ink };
    let mut name_room = name_row;
    if let Channel::Track(track) = channel
        && let Some(badge) = kind_badge(track.kind)
    {
        let quiet = theme.quiet_on(surface);
        let used = text::paint(&painter, name_row, Align2::RIGHT_CENTER, theme.text.micro, badge, quiet);
        name_room.max.x -= used + 4.0;
    }
    text::paint(&painter, name_room, Align2::LEFT_CENTER, style, name, ink);

    // Under the name every strip keeps the same rows, used or not, so
    // that what the strips share lines up across the mixer.
    let mut top = name_row.bottom() + 3.0;
    let line = |top: f32| Rect::from_min_size(pos2(inner.left(), top), vec2(inner.width(), bar));
    device_chips(ui, cx, channel, devices, shares.devices, top, inner, id);
    top += shares.devices as f32 * row;
    if let Channel::Track(track) = channel {
        // A return has nowhere to send to; its rows stay empty.
        let returns = snapshot.project.tracks.iter().filter(|other| other.kind == TrackKind::Return);
        let sending = track.kind != TrackKind::Return;
        for (index, target) in returns.take(shares.sends).enumerate().filter(|_| sending) {
            let rect = line(top + index as f32 * row);
            arrangement::send_bar(ui, cx, track, target, rect, id.with(("send", target.id)));
        }
        arrangement::pan_bar(ui, cx, track, line(top + shares.sends as f32 * row), id.with("pan"));
    }
    top += (shares.sends + 1) as f32 * row;

    // The fader and its meter, side by side in the middle of the strip.
    let fader_area =
        Rect::from_min_max(pos2(inner.left(), top + 1.0), pos2(inner.right(), inner.bottom() - row - READOUT_ROW));
    if fader_area.height() >= 24.0 {
        let pair = FADER_WIDTH + 5.0 + METER_WIDTH;
        let left = (fader_area.center().x - pair / 2.0).round();
        let fader_rect = Rect::from_min_size(pos2(left, fader_area.top()), vec2(FADER_WIDTH, fader_area.height()));
        let meter_rect = Rect::from_min_size(
            pos2(fader_rect.right() + 5.0, fader_area.top()),
            vec2(METER_WIDTH, fader_area.height()),
        );
        let volume = fader(ui, cx, channel, fader_rect, id.with("volume"));
        let meter = match channel {
            Channel::Track(track) => cx.live.track_meter(track.id),
            Channel::Master => cx.live.master,
        };
        trill_noir::stereo_meter(&painter, theme, meter_rect, widgets::levels(meter), widgets::meter_zones());

        let readout =
            Rect::from_min_size(pos2(inner.left(), fader_area.bottom() + 1.0), vec2(inner.width(), READOUT_ROW - 2.0));
        let style = trill_noir::TextStyle { size: 11.0, ..theme.text.value };
        let peak_ink = if meter.clipped { theme.signal } else { theme.quiet_on(surface) };
        text::paint(&painter, readout, Align2::LEFT_CENTER, style, &units::format_db(volume), theme.ink);
        // The held peak, as a number beside the level it is read against.
        let peak = units::format_level(meter.peak_db).replace(" dB", "");
        text::paint(&painter, readout, Align2::RIGHT_CENTER, style, &peak, peak_ink);
    }

    if let Channel::Track(track) = channel {
        let chips = Rect::from_min_size(pos2(inner.left(), inner.bottom() - bar), vec2(inner.width(), bar));
        mute_and_solo(ui, cx, track, chips, id, theme.quiet_on(surface));
    }
}

fn kind_badge(kind: TrackKind) -> Option<&'static str> {
    match kind {
        TrackKind::Normal => None,
        TrackKind::Group => Some("Grp"),
        TrackKind::Return => Some("Ret"),
    }
}

fn select(cx: &mut Cx, channel: Channel) {
    match channel {
        Channel::Track(track) => cx.state.select_track(track.id),
        Channel::Master => cx.state.select_master(),
    }
}

/// The chain's devices as chips in `shown` rows, the last of which counts
/// the devices there was no room for. A click shows that device in the
/// Devices tab. A plugin's chip ends in a button of its own: it opens the
/// plugin's window, and is lit while that is open.
#[expect(clippy::too_many_arguments, reason = "a placement call: what to list, how much of it and where")]
fn device_chips(
    ui: &Ui,
    cx: &mut Cx,
    channel: Channel,
    devices: &[Device],
    shown: usize,
    mut top: f32,
    inner: Rect,
    id: egui::Id,
) {
    let theme = cx.theme;
    let row = theme.metric.chip + theme.metric.gap;
    // The last row counts the devices it has no room for.
    let listed = if devices.len() > shown { shown.saturating_sub(1) } else { shown };
    for device in devices.iter().take(listed) {
        let mut rect = Rect::from_min_size(pos2(inner.left(), top), vec2(inner.width(), theme.metric.chip));
        if editors::has(cx, device.id) {
            // A square at the chip's end, set off like one chip from the next.
            let cell = rect.with_min_x(rect.right() - rect.height());
            rect.max.x = cell.left() - (theme.metric.gap - 1.0);
            let open = editors::is_open(cx, device.id);
            let color = if open { theme.ink } else { theme.ink_dim };
            let button = id.with(("editor", device.id));
            let response = trill_noir::icon_button(ui, theme, cell, button, Icon::Window, open, color);
            cx.hint(&response, editors::hint(cx, device.id, &device.name));
            if response.clicked() {
                editors::toggle(cx, device.id);
            }
        }
        let lit = cx.state.selected_device == Some(device.id) && cx.state.shown_chain() == chain_of(channel);
        let name = format!("{} in the mixer", device.name);
        let dot = if device.enabled { theme.ok } else { theme.ghost };
        let chip = Chip::new(&device.name, lit).dot(dot).named(&name);
        let response = trill_noir::chip(ui, theme, rect, id.with(("device", device.id)), &chip);
        let state = if device.enabled { "on" } else { "bypassed" };
        cx.hint(&response, format!("{}, {state} \u{00B7} click to show it in the Devices tab", device.name));
        if response.clicked() {
            select(cx, channel);
            cx.state.selected_device = Some(device.id);
            cx.state.detail_tab = DetailTab::Devices;
        }
        top += row;
    }
    if shown > 0 && devices.len() > listed {
        let rect = Rect::from_min_size(pos2(inner.left(), top), vec2(inner.width(), theme.metric.chip));
        let more = format!("+ {} more", devices.len() - listed);
        text::paint(ui.painter(), rect, Align2::LEFT_CENTER, theme.text.micro, &more, theme.ink_faint);
    }
}

fn chain_of(channel: Channel) -> trill_core::ChainRef {
    match channel {
        Channel::Track(track) => trill_core::ChainRef::Track(track.id),
        Channel::Master => trill_core::ChainRef::Master,
    }
}

/// The volume fader of a strip. Returns the volume it shows, in dB. While
/// an envelope drives the volume the fader shows what the envelope says;
/// dragging it takes hold of the volume (see [`automation::respond`]).
fn fader(ui: &Ui, cx: &mut Cx, channel: Channel, rect: Rect, id: egui::Id) -> f32 {
    let (target, resting, name) = match channel {
        Channel::Track(track) => {
            (AutomationTarget::TrackVolume { track: track.id }, track.volume_db, format!("Volume of {}", track.name))
        }
        Channel::Master => {
            (AutomationTarget::MasterVolume, cx.snap.project.master.volume_db, "Master volume".to_owned())
        }
    };
    let hold = Hold::of(cx, &target);
    let volume = hold.driven().map_or(resting, |value| value as f32);
    let spec = Fader::new(&name, units::db_to_fader(volume)).default(units::db_to_fader(0.0));
    let output = trill_noir::fader(ui, cx.theme, rect, id, &spec.automation(hold.mark()));
    let what = format!("{name} \u{00B7} {}", units::format_db(volume));
    let gestures = "drag to set, Ctrl-drag for fine, double-click for 0 dB";
    if let Some(position) = automation::respond(cx, &target, hold, &output, &what, gestures) {
        let next = if output.reset { 0.0 } else { units::fader_to_db(position) };
        if next != resting {
            set_volume(cx, channel, next);
        }
    }
    volume
}

fn set_volume(cx: &mut Cx, channel: Channel, volume: f32) {
    match channel {
        Channel::Track(track) => {
            let id = track.id;
            cx.edit(|project| project.set_track_volume(id, volume));
        }
        Channel::Master => drop(cx.edit(|project| project.set_master_volume(volume))),
    }
}

/// Mute, solo and arm, each a third of the strip wide. The arm control's
/// place is kept on the strips that have none, so the others line up.
fn mute_and_solo(ui: &Ui, cx: &mut Cx, track: &Track, row: Rect, id: egui::Id, quiet: Color32) {
    let theme = cx.theme;
    let third = ((row.width() - 2.0 * theme.metric.gap) / 3.0).floor();
    let left = Rect::from_min_size(row.min, vec2(third, row.height()));
    let right = Rect::from_min_size(pos2(left.right() + theme.metric.gap, row.top()), vec2(third, row.height()));
    let arm = Rect::from_min_max(pos2(right.right() + theme.metric.gap, row.top()), row.max);
    let track_id: TrackId = track.id;
    if recording::armable(cx, track) {
        let name = format!("Arm {} in the mixer", track.name);
        recording::arm_chip(ui, cx, track, arm, id.with("arm"), quiet, &name);
    }

    let name = format!("Mute {} in the mixer", track.name);
    let chip = Chip::new("M", track.mute).accent(theme.warn).quiet(quiet).named(&name);
    let response = trill_noir::chip(ui, theme, left, id.with("mute"), &chip);
    cx.hint(&response, "Mute this track");
    if response.clicked() {
        let mute = !track.mute;
        cx.edit(|project| {
            project.track_mut(track_id)?.mute = mute;
            Ok(())
        });
    }
    let name = format!("Solo {} in the mixer", track.name);
    let chip = Chip::new("S", track.solo).accent(theme.info).quiet(quiet).named(&name);
    let response = trill_noir::chip(ui, theme, right, id.with("solo"), &chip);
    cx.hint(&response, "Solo: hear only the soloed tracks");
    if response.clicked() {
        let solo = !track.solo;
        cx.edit(|project| {
            project.track_mut(track_id)?.solo = solo;
            Ok(())
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_share_the_width_between_a_minimum_and_a_maximum() {
        assert_eq!(strip_width(900.0, 3), MAX_STRIP, "a few tracks do not sprawl");
        assert_eq!(strip_width(900.0, 9), 100.0);
        assert_eq!(strip_width(900.0, 30), MIN_STRIP, "thirty tracks scroll instead of squeezing");
        assert_eq!(strip_width(900.0, 0), MAX_STRIP);
    }

    #[test]
    fn a_short_strip_gives_up_devices_then_sends_before_its_fader() {
        let row = 21.0;
        let extras = 62.0;
        // Tall enough for everything: the fader takes what is left.
        let tall = budget(320.0, row, 3, 2, extras);
        assert_eq!((tall.devices, tall.sends), (3, 2));
        assert_eq!(tall.fader, 320.0 - extras - 5.0 * row);

        // Shorter: sends stay, devices go one by one.
        let medium = budget(190.0, row, 3, 2, extras);
        assert_eq!((medium.devices, medium.sends), (1, 2));
        assert!(medium.fader >= MIN_FADER);
        let short = budget(150.0, row, 3, 2, extras);
        assert_eq!((short.devices, short.sends), (0, 1));
        assert!(short.fader >= MIN_FADER);
        // At the panel's minimum height only the fader is left.
        let least = budget(130.0, row, 3, 2, extras);
        assert_eq!((least.devices, least.sends), (0, 0));

        // A long chain is listed up to a point.
        assert_eq!(budget(900.0, row, 12, 0, extras).devices, MAX_DEVICES);
    }
}
