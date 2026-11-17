//! A change in plain words, read from what differs.
//!
//! Most steps of a session come with words of their own: a tool call says
//! what it is for. An edit made by hand in the window does not, and neither
//! does a step taken back. [`describe`] reads those from the song before and
//! after: *Added the track "Bass"*, *Moved the clip "Verse"*, *Added 3 notes
//! to "Hook"*. A step that did several things is called by the first of
//! them, in the order below, and says how many more there were.

use super::change::{Change, Op, StateChange};
use crate::{AutomationTarget, ChainRef, Clip, Device, Project, Track};

/// A name in the quotation marks names are shown in.
pub fn quoted(name: &str) -> String {
    format!("\u{201C}{name}\u{201D}")
}

/// "volume", "volume and pan", "volume, pan and mute".
pub fn list(items: &[&str]) -> String {
    match items {
        [] => String::new(),
        [one] => (*one).to_owned(),
        [first @ .., last] => format!("{} and {last}", first.join(", ")),
    }
}

fn count(number: usize, one: &str, many: &str) -> String {
    if number == 1 { format!("1 {one}") } else { format!("{number} {many}") }
}

/// What an envelope automates, in words.
fn automated(target: &AutomationTarget, project: &Project) -> String {
    let track = |id| project.track(id).map_or_else(|_| "a track".to_owned(), |track: &Track| quoted(&track.name));
    match target {
        AutomationTarget::TrackVolume { track: id } => format!("the volume of {}", track(*id)),
        AutomationTarget::TrackPan { track: id } => format!("the pan of {}", track(*id)),
        AutomationTarget::MasterVolume => "the master volume".to_owned(),
        AutomationTarget::DeviceParam { device, param } => {
            let device = project.device(*device).map_or_else(|_| "a device".to_owned(), |device| quoted(&device.name));
            format!("{param} of {device}")
        }
    }
}

/// Whose chain it is, in words.
fn owner(chain: ChainRef, project: &Project) -> String {
    match chain {
        ChainRef::Master => "the master".to_owned(),
        ChainRef::Track(id) => project.track(id).map_or_else(|_| "a track".to_owned(), |track| quoted(&track.name)),
    }
}

/// What differs between two versions of a track's settings.
fn track_change(was: &Track, now: &Track) -> String {
    if was.name != now.name {
        return format!("Renamed {} to {}", quoted(&was.name), quoted(&now.name));
    }
    let mut changed = Vec::new();
    for (differs, what) in [
        (was.volume_db != now.volume_db, "volume"),
        (was.pan != now.pan, "pan"),
        (was.mute != now.mute, "mute"),
        (was.solo != now.solo, "solo"),
        (was.group != now.group, "group"),
        (was.sends != now.sends, "sends"),
        (was.color != now.color, "color"),
    ] {
        if differs {
            changed.push(what);
        }
    }
    match changed.as_slice() {
        [] if was.profile != now.profile => format!("Noted what the instrument of {} does", quoted(&now.name)),
        [] => format!("Changed {}", quoted(&now.name)),
        changed => format!("Changed the {} of {}", list(changed), quoted(&now.name)),
    }
}

/// What differs between two versions of a clip's settings.
fn clip_change(was: &Clip, now: &Clip) -> String {
    let clip = quoted(&now.name);
    if was.name != now.name {
        format!("Renamed the clip {} to {clip}", quoted(&was.name))
    } else if was.muted != now.muted {
        format!("{} the clip {clip}", if now.muted { "Muted" } else { "Unmuted" })
    } else if was.start != now.start && was.length == now.length {
        format!("Moved the clip {clip}")
    } else if was.length != now.length {
        format!("Resized the clip {clip}")
    } else if was.controllers() != now.controllers() {
        format!("Shaped a controller of {clip}")
    } else {
        format!("Changed the clip {clip}")
    }
}

/// What differs between two versions of a device.
fn device_change(was: &Device, now: &Device, state: StateChange) -> String {
    let device = quoted(&now.name);
    if was.enabled != now.enabled {
        format!("{} {device}", if now.enabled { "Switched on" } else { "Bypassed" })
    } else if was.preset != now.preset
        && let Some(preset) = &now.preset
    {
        format!("Set {device} to the preset {}", quoted(&preset.name))
    } else if was.params != now.params {
        format!("Adjusted {device}")
    } else if was.name != now.name {
        format!("Renamed {} to {device}", quoted(&was.name))
    } else if state != StateChange::Keep {
        format!("Changed the sound of {device}")
    } else {
        format!("Changed {device}")
    }
}

/// Says in words what `change` did, which turned `before` into `after`.
pub fn describe(before: &Project, after: &Project, change: &Change) -> String {
    let mut said: Vec<String> = Vec::new();

    // Tracks that came and went.
    let added: Vec<&Track> = after.tracks.iter().filter(|track| before.track(track.id).is_err()).collect();
    let removed: Vec<&Track> = before.tracks.iter().filter(|track| after.track(track.id).is_err()).collect();
    match added.as_slice() {
        [] => {}
        [track] => said.push(format!("Added the track {}", quoted(&track.name))),
        tracks => said.push(format!("Added {} tracks", tracks.len())),
    }
    match removed.as_slice() {
        [] => {}
        [track] => said.push(format!("Deleted the track {}", quoted(&track.name))),
        tracks => said.push(format!("Deleted {} tracks", tracks.len())),
    }
    let is_new_track = |id| added.iter().any(|track| track.id == id);

    // Clips that came (on tracks that were there) and went (from tracks
    // that still are).
    let mut new_clips: Vec<(&Clip, String)> = Vec::new();
    for op in change.ops() {
        if let Op::Clip { track, clip } = op
            && before.clip(clip.id).is_err()
            && !is_new_track(*track)
            && let Ok(clip) = after.clip(clip.id)
        {
            let on =
                after.clip_track(clip.id).map_or_else(|_| String::new(), |track| owner(ChainRef::Track(track), after));
            new_clips.push((clip, on));
        }
    }
    match new_clips.as_slice() {
        [] => {}
        [(clip, on)] => said.push(format!("Added the clip {} to {on}", quoted(&clip.name))),
        clips => said.push(format!("Added {} clips", clips.len())),
    }
    // Only the tracks whose clips are not the ones they were are looked
    // through: a song has thousands of clips, and most edits touch none.
    let mut gone: Vec<&Clip> = Vec::new();
    for op in change.ops() {
        let Op::Clips(orders) = op else { continue };
        for (track, kept) in orders {
            let Ok(was) = before.track(*track) else { continue };
            let missing = was.clips.iter().filter(|clip| !kept.contains(&clip.id));
            gone.extend(missing.filter(|clip| after.clip(clip.id).is_err()));
        }
    }
    match gone.as_slice() {
        [] => {}
        [clip] => said.push(format!("Deleted the clip {}", quoted(&clip.name))),
        clips => said.push(format!("Deleted {} clips", clips.len())),
    }

    // Notes, clip by clip.
    for op in change.ops() {
        let Op::Notes { clip, cut, put, .. } = op else { continue };
        let name = after.clip(*clip).map_or_else(|_| "a clip".to_owned(), |clip| quoted(&clip.name));
        said.push(match (cut, put.len()) {
            (0, added) => format!("Added {} to {name}", count(added, "note", "notes")),
            (removed, 0) => format!("Removed {} from {name}", count(*removed, "note", "notes")),
            _ => format!("Changed the notes of {name}"),
        });
    }

    // Clips whose settings changed, or that went to another track.
    for op in change.ops() {
        match op {
            Op::ClipSettings(clip) => {
                if let (Ok(was), Ok(now)) = (before.clip(clip.id), after.clip(clip.id)) {
                    said.push(clip_change(was, now));
                }
            }
            Op::Clip { clip, .. } => {
                if let (Ok(was), Ok(now)) = (before.clip(clip.id), after.clip(clip.id)) {
                    said.push(clip_change(was, now));
                }
            }
            _ => {}
        }
    }
    if let Some(Op::Clips(orders)) = change.ops().iter().find(|op| matches!(op, Op::Clips(_))) {
        let moved = orders.iter().flat_map(|(track, clips)| clips.iter().map(move |clip| (*track, *clip)));
        let moved: Vec<String> = moved
            .filter(|(track, clip)| before.clip_track(*clip).is_ok_and(|was| was != *track))
            .filter_map(|(track, clip)| {
                let name = quoted(&after.clip(clip).ok()?.name);
                Some(format!("Moved the clip {name} to {}", owner(ChainRef::Track(track), after)))
            })
            .collect();
        said.extend(moved);
    }

    // Devices.
    for op in change.ops() {
        let Op::Device { chain, device, state } = op else { continue };
        let Ok(now) = after.device(device.id) else { continue };
        match before.device(device.id) {
            Ok(was) => said.push(device_change(was, now, *state)),
            Err(_) if matches!(chain, ChainRef::Track(track) if is_new_track(*track)) => {}
            Err(_) => said.push(format!("Added {} to {}", now.name, owner(*chain, after))),
        }
    }
    for op in change.ops() {
        let Op::Devices(orders) = op else { continue };
        for (chain, kept) in orders {
            let Ok(was) = before.chain(*chain) else { continue };
            let missing = was.iter().filter(|device| !kept.contains(&device.id));
            let removed = missing.filter(|device| after.device(device.id).is_err());
            said.extend(removed.map(|device| format!("Removed {}", quoted(&device.name))));
        }
    }

    // Tracks whose settings changed, and their order.
    for op in change.ops() {
        if let Op::Track(track) = op
            && let (Ok(was), Ok(now)) = (before.track(track.id), after.track(track.id))
        {
            said.push(track_change(was, now));
        }
    }
    if added.is_empty() && removed.is_empty() && change.ops().iter().any(|op| matches!(op, Op::Tracks(_))) {
        said.push("Reordered the tracks".to_owned());
    }

    // Envelopes.
    for op in change.ops() {
        match op {
            Op::Lane(lane) => {
                let what = automated(&lane.target, after);
                let was = before.automation.iter().find(|was| was.target == lane.target);
                said.push(match was {
                    Some(was) if was.points == lane.points && lane.enabled => {
                        format!("Switched on the automation of {what}")
                    }
                    Some(was) if was.points == lane.points => format!("Switched off the automation of {what}"),
                    _ => format!("Automated {what}"),
                });
            }
            Op::Lanes(_) => {
                let removed = before.automation.iter().filter(|was| {
                    !after.automation.iter().any(|lane| lane.target == was.target)
                        // One that went with its track or device needs no words.
                        && super::place::lane(before, &was.target).is_some()
                        && match &was.target {
                            AutomationTarget::TrackVolume { track } | AutomationTarget::TrackPan { track } => {
                                after.track(*track).is_ok()
                            }
                            AutomationTarget::DeviceParam { device, .. } => after.device(*device).is_ok(),
                            AutomationTarget::MasterVolume => true,
                        }
                });
                said.extend(removed.map(|was| format!("Removed the automation of {}", automated(&was.target, before))));
            }
            _ => {}
        }
    }

    // The song's own settings and its markers.
    for op in change.ops() {
        match op {
            Op::Song(_) => {
                let mut changed = Vec::new();
                for (differs, what) in [
                    (before.name != after.name, "name"),
                    (before.tempo != after.tempo, "tempo"),
                    (before.time_signature != after.time_signature, "time signature"),
                    (before.loop_region != after.loop_region, "loop"),
                    (before.master.volume_db != after.master.volume_db, "master volume"),
                ] {
                    if differs {
                        changed.push(what);
                    }
                }
                said.push(match changed.as_slice() {
                    ["tempo"] => format!("Set the tempo to {} BPM", after.tempo),
                    ["name"] => format!("Renamed the project to {}", quoted(&after.name)),
                    [] => "Changed the project".to_owned(),
                    changed => format!("Changed the {}", list(changed)),
                });
            }
            Op::Markers(markers) => said.push(match markers.len().cmp(&before.markers.len()) {
                std::cmp::Ordering::Greater => "Added a marker".to_owned(),
                std::cmp::Ordering::Less => "Removed a marker".to_owned(),
                std::cmp::Ordering::Equal => "Changed the markers".to_owned(),
            }),
            _ => {}
        }
    }

    let mut said = said.into_iter();
    match (said.next(), said.count()) {
        (None, _) => "Changed the song".to_owned(),
        (Some(first), 0) => first,
        (Some(first), 1) => format!("{first}, and 1 more change"),
        (Some(first), more) => format!("{first}, and {more} more changes"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::Blobs;
    use crate::{AutomationPoint, ClipId, DeviceId, Note, TrackId};

    fn song() -> (Project, [TrackId; 2], [ClipId; 2], [DeviceId; 2]) {
        let mut project = Project::new("Song");
        let lead = project.add_track("Lead");
        let bass = project.add_track("Bass");
        let synth = project.add_device(ChainRef::Track(lead), "builtin:polysynth", "Poly Synth", None).unwrap();
        let limiter = project.add_device(ChainRef::Master, "builtin:limiter", "Limiter", None).unwrap();
        let hook = project.add_midi_clip(lead, "Hook", 0.0, 8.0).unwrap();
        let line = project.add_midi_clip(bass, "Line", 8.0, 8.0).unwrap();
        (project, [lead, bass], [hook, line], [synth, limiter])
    }

    /// The words for an edit.
    fn words<R>(before: &Project, edit: impl FnOnce(&mut Project) -> R) -> String {
        let mut after = before.clone();
        edit(&mut after);
        let change = Change::between(before, &after, &mut Blobs::default());
        describe(before, &after, &change)
    }

    fn note_at(start: f64) -> Note {
        Note { pitch: 60, start, length: 0.5, velocity: 100 }
    }

    #[test]
    fn an_edit_made_by_hand_reads_as_what_it_did() {
        let (project, [lead, bass], [hook, line], [synth, limiter]) = song();
        let p = &project;
        assert_eq!(words(p, |_| {}), "Changed the song");

        // Tracks.
        assert_eq!(words(p, |p| p.add_track("Drums")), "Added the track \u{201C}Drums\u{201D}");
        assert_eq!(
            words(p, |p| {
                let drums = p.add_track("Drums");
                p.add_device(ChainRef::Track(drums), "builtin:drumkit", "Drum Kit", None).unwrap();
                p.add_midi_clip(drums, "Beat", 0.0, 4.0).unwrap();
            }),
            "Added the track \u{201C}Drums\u{201D}",
            "a track comes with what is on it"
        );
        assert_eq!(words(p, |p| p.remove_track(bass).unwrap()), "Deleted the track \u{201C}Bass\u{201D}");
        assert_eq!(
            words(p, |p| p.track_mut(lead).unwrap().name = "Keys".into()),
            "Renamed \u{201C}Lead\u{201D} to \u{201C}Keys\u{201D}"
        );
        assert_eq!(
            words(p, |p| {
                p.set_track_volume(lead, -6.0).unwrap();
                p.set_track_pan(lead, 0.5).unwrap();
            }),
            "Changed the volume and pan of \u{201C}Lead\u{201D}"
        );
        assert_eq!(words(p, |p| p.move_track(bass, 0).unwrap()), "Reordered the tracks");

        // Clips and their notes.
        assert_eq!(
            words(p, |p| p.add_midi_clip(bass, "Fill", 16.0, 4.0).unwrap()),
            "Added the clip \u{201C}Fill\u{201D} to \u{201C}Bass\u{201D}"
        );
        assert_eq!(words(p, |p| p.remove_clip(line).unwrap()), "Deleted the clip \u{201C}Line\u{201D}");
        assert_eq!(words(p, |p| p.set_clip_span(hook, 4.0, 8.0).unwrap()), "Moved the clip \u{201C}Hook\u{201D}");
        assert_eq!(words(p, |p| p.set_clip_span(hook, 0.0, 4.0).unwrap()), "Resized the clip \u{201C}Hook\u{201D}");
        assert_eq!(
            words(p, |p| p.move_clip(hook, Some(bass), 0.0).unwrap()),
            "Moved the clip \u{201C}Hook\u{201D} to \u{201C}Bass\u{201D}"
        );
        assert_eq!(words(p, |p| p.clip_mut(hook).unwrap().muted = true), "Muted the clip \u{201C}Hook\u{201D}");
        assert_eq!(
            words(p, |p| p.clip_mut(hook).unwrap().add_notes(&[note_at(0.0), note_at(1.0), note_at(2.0)]).unwrap()),
            "Added 3 notes to \u{201C}Hook\u{201D}"
        );
        let mut with_notes = project.clone();
        with_notes.clip_mut(hook).unwrap().add_notes(&[note_at(0.0), note_at(1.0)]).unwrap();
        assert_eq!(
            words(&with_notes, |p| p.clip_mut(hook).unwrap().remove_notes(|note| note.start == 1.0).unwrap()),
            "Removed 1 note from \u{201C}Hook\u{201D}"
        );
        assert_eq!(
            words(&with_notes, |p| p.clip_mut(hook).unwrap().set_notes(vec![note_at(0.0), note_at(3.0)]).unwrap()),
            "Changed the notes of \u{201C}Hook\u{201D}"
        );

        // Devices.
        assert_eq!(
            words(p, |p| p.add_device(ChainRef::Track(bass), "builtin:reverb", "Reverb", None).unwrap()),
            "Added Reverb to \u{201C}Bass\u{201D}"
        );
        assert_eq!(
            words(p, |p| p.add_device(ChainRef::Master, "builtin:eq", "EQ", None).unwrap()),
            "Added EQ to the master"
        );
        assert_eq!(words(p, |p| p.remove_device(synth).unwrap()), "Removed \u{201C}Poly Synth\u{201D}");
        assert_eq!(words(p, |p| p.device_mut(limiter).unwrap().enabled = false), "Bypassed \u{201C}Limiter\u{201D}");
        assert_eq!(
            words(p, |p| p.device_mut(synth).unwrap().params.insert("cutoff".into(), 900.0)),
            "Adjusted \u{201C}Poly Synth\u{201D}"
        );
        assert_eq!(
            words(p, |p| p.device_mut(synth).unwrap().state = Some(vec![1, 2, 3])),
            "Changed the sound of \u{201C}Poly Synth\u{201D}"
        );

        // Envelopes, the song's settings, markers.
        let volume = AutomationTarget::TrackVolume { track: lead };
        assert_eq!(
            words(p, |p| p.set_automation(volume.clone(), vec![AutomationPoint::new(0.0, -6.0)]).unwrap()),
            "Automated the volume of \u{201C}Lead\u{201D}"
        );
        let mut automated = project.clone();
        automated.set_automation(volume.clone(), vec![AutomationPoint::new(0.0, -6.0)]).unwrap();
        assert_eq!(
            words(&automated, |p| p.set_automation(volume.clone(), Vec::new()).unwrap()),
            "Removed the automation of the volume of \u{201C}Lead\u{201D}"
        );
        assert_eq!(
            words(&automated, |p| p.set_automation_enabled(&volume, false).unwrap()),
            "Switched off the automation of the volume of \u{201C}Lead\u{201D}"
        );
        assert_eq!(words(p, |p| p.set_tempo(96.0).unwrap()), "Set the tempo to 96 BPM");
        assert_eq!(words(p, |p| p.name = "Night".into()), "Renamed the project to \u{201C}Night\u{201D}");
        assert_eq!(
            words(p, |p| {
                p.set_tempo(96.0).unwrap();
                p.set_loop(true, 0.0, 8.0).unwrap();
            }),
            "Changed the tempo and loop"
        );
        assert_eq!(words(p, |p| p.add_marker(8.0, "Verse").unwrap()), "Added a marker");

        // A step that did several things goes by the first of them.
        assert_eq!(
            words(p, |p| {
                p.add_track("Drums");
                p.set_tempo(96.0).unwrap();
                p.set_track_volume(lead, -3.0).unwrap();
            }),
            "Added the track \u{201C}Drums\u{201D}, and 2 more changes"
        );
    }
}
