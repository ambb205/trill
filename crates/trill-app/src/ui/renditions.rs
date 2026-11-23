//! A track heard where its plugin is not: what the window says of it.
//!
//! In a song that others hold too, a plugin may be on someone else's
//! computer and not on this one. Whoever has it records what the track's
//! devices make of the track, and this one plays that recording in their
//! place (`trill_core::renditions`). Nothing has to be done about it, and
//! nothing can be: so the window only says it, where the track is looked
//! at when something about it is not as expected.
//!
//! * A mark on the track's header: filled while what is heard is the track
//!   as the song has it, hollow while it is the track as it was (something
//!   the recording depends on was changed, and it is being recorded again)
//!   or while the recording is still on its way.
//! * The panel of a device that is not here says who recorded the track,
//!   in place of parameters it has not got.
//! * The status bar of whoever is recording says which track, while it
//!   does: their machine is busy with it.

use super::state::Cx;
use eframe::egui::{self, Id, Rect, Sense, Ui, pos2};
use trill_core::{Device, DeviceId, Project, Track, TrackId};
use trill_engine::RenditionHere;
use trill_noir::paint;

/// The rendition of `track` that this machine needs: it has not got one
/// of the devices the recording stands in for.
pub fn of_track(renditions: &[RenditionHere], track: TrackId) -> Option<&RenditionHere> {
    renditions.iter().find(|found| found.track == track && !found.lacking.is_empty())
}

/// The rendition that stands in for `device` on this machine, where the
/// machine has not got that device.
pub fn of_device(renditions: &[RenditionHere], device: DeviceId) -> Option<&RenditionHere> {
    renditions.iter().find(|found| found.lacking.contains(&device))
}

/// Who recorded it, as a sentence has them: "Ada's Trill", or "someone
/// else's Trill" where nobody is named.
fn whose(found: &RenditionHere) -> String {
    match found.by.trim() {
        "" => "someone else\u{2019}s Trill".to_owned(),
        name => format!("{name}\u{2019}s Trill"),
    }
}

/// The devices of `found` that are not on this machine, by name.
fn lacking(found: &RenditionHere, project: &Project) -> String {
    let names: Vec<&str> =
        found.lacking.iter().filter_map(|id| project.device(*id).ok()).map(|device| device.name.as_str()).collect();
    match names.as_slice() {
        [] => "one of its devices".to_owned(),
        names => trill_core::journal::words::list(names),
    }
}

/// What is heard of a track that has a device this machine has not got,
/// in a few sentences: for whoever points at its mark, and for the panel
/// of the device.
pub fn words(found: &RenditionHere, track: &Track, project: &Project) -> String {
    let (name, who, devices) = (&track.name, whose(found), lacking(found, project));
    let are = if found.lacking.len() == 1 { "is" } else { "are" };
    let yours = "Its fader, its pan, its sends and the devices after that are yours to move, and are heard at once.";
    match (found.played, found.as_the_song_is()) {
        (true, true) => {
            format!("{devices} {are} not on this computer. You hear {name} as {who} recorded it. {yours}")
        }
        (true, false) => format!(
            "{devices} {are} not on this computer. You hear {name} as {who} recorded it before the last change to \
             what it plays, and it is being recorded again: nobody has to do anything. {yours}"
        ),
        (false, _) => format!(
            "{devices} {are} not on this computer. What {who} recorded of {name} is on its way; until it is here \
             the track is silent."
        ),
    }
}

/// The few words a screen reader and a test read off the mark.
pub fn label(found: &RenditionHere, track: &Track) -> String {
    let who = whose(found);
    match (found.played, found.as_the_song_is()) {
        (true, true) => format!("{}: heard as {who} recorded it", track.name),
        (true, false) => format!("{}: heard as {who} recorded it before the last change", track.name),
        (false, _) => format!("{}: what {who} recorded of it is on its way", track.name),
    }
}

/// The mark of a track that is heard by a recording: a dot in `rect`,
/// filled while what is heard is the track as the song has it and hollow
/// otherwise, that says what is heard to whoever points at it. Returns
/// whether there was anything to mark.
pub fn track_mark(ui: &Ui, cx: &mut Cx, rect: Rect, id: Id, track: &Track) -> bool {
    let Some(found) = of_track(&cx.snap.renditions, track.id) else { return false };
    let theme = cx.theme;
    let response = ui.interact(rect, id, Sense::hover());
    let name = label(found, track);
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Label, true, &name));
    let left = pos2(rect.center().x - theme.metric.dot / 2.0, rect.center().y);
    match found.played && found.as_the_song_is() {
        true => paint::dot(ui.painter(), left, theme.metric.dot, theme.info),
        false => paint::hollow_dot(ui.painter(), left, theme.metric.dot, theme.info),
    };
    let said = words(found, track, &cx.snap.project);
    cx.hint(&response, said);
    true
}

/// What the panel of a device that is not on this machine says, where a
/// recording stands in for it: `None` for any other device.
pub fn device_words(renditions: &[RenditionHere], device: &Device, project: &Project) -> Option<String> {
    let found = of_device(renditions, device.id)?;
    let track = project.track(found.track).ok()?;
    Some(words(found, track, project))
}

/// What the status bar says where tracks are heard by recordings because
/// their plugins are not on this computer: how many, how many of those
/// are being recorded again (they are heard as they were), and how many
/// are still on their way.
pub fn heard_words(renditions: &[RenditionHere]) -> Option<String> {
    let needed: Vec<&RenditionHere> = renditions.iter().filter(|found| !found.lacking.is_empty()).collect();
    let played = needed.iter().filter(|found| found.played).count();
    let before = needed.iter().filter(|found| found.played && !found.as_the_song_is()).count();
    let coming = needed.len() - played;
    let tracks = |count: usize| if count == 1 { "1 track".to_owned() } else { format!("{count} tracks") };
    let mut words = Vec::new();
    if played > 0 {
        words.push(format!("{} heard as recorded", tracks(played)));
    }
    if before > 0 {
        words.push(format!("{before} being recorded again"));
    }
    if coming > 0 {
        words.push(format!("{} on {} way", tracks(coming), if coming == 1 { "its" } else { "their" }));
    }
    (!words.is_empty()).then(|| words.join(" \u{00B7} "))
}

/// What the status bar says while this program records a track for the
/// others.
pub fn status_words(recording: Option<&str>) -> Option<String> {
    recording.map(|track| format!("Recording {track} for the others"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use trill_core::ChainRef;

    fn song() -> (Project, TrackId, DeviceId) {
        let mut song = Project::new("Song");
        let bass = song.add_track("Bass");
        let plugin = song.add_device(ChainRef::Track(bass), "vst3:NOT-HERE", "Serum 2", None).unwrap();
        song.add_device(ChainRef::Track(bass), "builtin:eq", "EQ", None).unwrap();
        (song, bass, plugin)
    }

    fn found(track: TrackId, plugin: DeviceId) -> RenditionHere {
        RenditionHere {
            track,
            devices: vec![plugin],
            lacking: vec![plugin],
            by: "Ada".into(),
            current: true,
            played: true,
            newest: true,
        }
    }

    #[test]
    fn a_track_heard_by_a_recording_says_whose_and_whether_it_is_the_song_as_it_is() {
        let (song, bass, plugin) = song();
        let track = song.track(bass).unwrap();
        let as_it_is = found(bass, plugin);
        assert_eq!(label(&as_it_is, track), "Bass: heard as Ada\u{2019}s Trill recorded it");
        let said = words(&as_it_is, track, &song);
        assert!(said.starts_with("Serum 2 is not on this computer. You hear Bass as Ada\u{2019}s Trill recorded it."));
        assert!(said.contains("yours to move, and are heard at once"), "{said}");

        // Something it depends on was changed, by a count or by a
        // recording that has not arrived: it is the track as it was.
        for before in
            [RenditionHere { current: false, ..as_it_is.clone() }, RenditionHere { newest: false, ..as_it_is.clone() }]
        {
            assert_eq!(label(&before, track), "Bass: heard as Ada\u{2019}s Trill recorded it before the last change");
            let said = words(&before, track, &song);
            assert!(said.contains("before the last change") && said.contains("nobody has to do anything"), "{said}");
        }
        let coming = RenditionHere { played: false, ..as_it_is.clone() };
        assert_eq!(label(&coming, track), "Bass: what Ada\u{2019}s Trill recorded of it is on its way");
        assert!(words(&coming, track, &song).contains("until it is here the track is silent"));
        let nobody = RenditionHere { by: String::new(), ..as_it_is.clone() };
        assert_eq!(label(&nobody, track), "Bass: heard as someone else\u{2019}s Trill recorded it");

        // Only where this machine has not got the device: whoever has it
        // plays it, and is told nothing.
        let renditions = [as_it_is.clone()];
        assert!(of_track(&renditions, bass).is_some() && of_device(&renditions, plugin).is_some());
        let has_it = [RenditionHere { lacking: Vec::new(), played: false, ..as_it_is }];
        assert!(of_track(&has_it, bass).is_none() && of_device(&has_it, plugin).is_none());
        let eq = track.devices[1].id;
        assert_eq!(device_words(&renditions, &track.devices[1], &song), None, "{eq}");
        assert!(device_words(&renditions, &track.devices[0], &song).is_some());
        assert_eq!(status_words(Some("Bass")).as_deref(), Some("Recording Bass for the others"));
        assert_eq!(status_words(None), None);

        // The status bar counts them: as recorded, of those the ones that
        // are being recorded again, and the ones still on their way.
        let one = |played: bool, current: bool| RenditionHere { played, current, ..renditions[0].clone() };
        assert_eq!(heard_words(&renditions).as_deref(), Some("1 track heard as recorded"));
        let three = [one(true, true), one(true, false), one(false, true)];
        let said = heard_words(&three).unwrap();
        assert_eq!(said, "2 tracks heard as recorded \u{00B7} 1 being recorded again \u{00B7} 1 track on its way");
        assert_eq!(heard_words(&has_it), None, "nothing is said to whoever has the plugins");
    }
}
