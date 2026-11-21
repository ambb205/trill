//! A change in plain words, read from what differs.
//!
//! Most steps of a session come with words of their own: a tool call says
//! what it is for. An edit made by hand in the window does not, and neither
//! does a step taken back. [`describe`] reads those from the song before and
//! after: *Added the track "Bass"*, *Moved the clip "Verse"*, *Added 3 notes
//! to "Hook"*. A step that did several things is called by the first of
//! them, in the order below, and says how many more there were.

use super::change::{Change, Op, StateChange};
use crate::{AutomationTarget, ChainRef, Channel, Clip, Device, Project, TimeSignature, Track};

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
        AutomationTarget::Send { track: id, to } => format!("the send of {} to {}", track(*id), track(*to)),
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
    // A reference is listened to or made, which is said as what it is.
    match (&was.reference, &now.reference) {
        (None, Some(_)) => return format!("Made {} a reference", quoted(&now.name)),
        (Some(_), None) => return format!("Made {} a track of the song again", quoted(&now.name)),
        (Some(before), Some(after)) if before.listening != after.listening => {
            return match after.listening {
                true => format!("Listened to the reference {}", quoted(&now.name)),
                false => format!("Went back from the reference {} to the song", quoted(&now.name)),
            };
        }
        _ => {}
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

/// Where two versions of a clip hold the same notes but for the channels
/// some are on, and those all went to one channel: how many, and which.
fn rechannelled(was: &Clip, now: &Clip) -> Option<(usize, Channel)> {
    let listed = |clip: &Clip| {
        let key = |note: &crate::Note| (note.start.to_bits(), note.pitch, note.length.to_bits(), note.velocity);
        let mut notes: Vec<_> = clip.notes().iter().map(|note| (key(note), note.channel)).collect();
        notes.sort_unstable();
        notes
    };
    let (was, now) = (listed(was), listed(now));
    if was.len() != now.len() || was.iter().zip(&now).any(|(was, now)| was.0 != now.0) {
        return None;
    }
    let mut moved = was.iter().zip(&now).filter(|(was, now)| was.1 != now.1).map(|(_, now)| now.1);
    let channel = moved.next()?;
    let others = moved.clone().count();
    moved.all(|other| other == channel).then_some((others + 1, channel))
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
        // The curve that was shaped, or the one that went: its channel is
        // worth a word where it is not the first.
        let shaped = now.controllers().iter().find(|curve| !was.controllers().contains(curve));
        let gone = was.controllers().iter().find(|curve| !now.controllers().contains(curve));
        match shaped.or(gone).filter(|curve| !curve.channel.is_first()) {
            Some(curve) => format!("Shaped a controller of {clip} on channel {}", curve.channel),
            None => format!("Shaped a controller of {clip}"),
        }
    } else if let (Some(played), Some(plays)) = (was.audio(), now.audio())
        && played.path != plays.path
    {
        // The file it plays, by its name: where it is is nobody's story.
        format!("Had the clip {clip} play {}", quoted(crate::files::file_name(&plays.path)))
    } else {
        format!("Changed the clip {clip}")
    }
}

/// What differs between two versions of a device, in the song as it is
/// `after`.
fn device_change(was: &Device, now: &Device, state: StateChange, after: &Project) -> String {
    let device = quoted(&now.name);
    if was.enabled != now.enabled {
        format!("{} {device}", if now.enabled { "Switched on" } else { "Bypassed" })
    } else if was.sidechain != now.sidechain {
        // The track it listens to in place of what comes into it.
        match now.sidechain.and_then(|track| after.track(track).ok()) {
            Some(track) => format!("Had {device} listen to {}", quoted(&track.name)),
            None => format!("Had {device} listen to its own input"),
        }
    } else if was.preset != now.preset
        && let Some(preset) = &now.preset
    {
        match preset.bringer() {
            // An effect that was saved as part of its track's sound: it
            // is on no preset of its own.
            Some(_) => format!("Made {device} part of the sound {}", quoted(&preset.name)),
            None => format!("Set {device} to the preset {}", quoted(&preset.name)),
        }
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

fn signature(signature: TimeSignature) -> String {
    format!("{}/{}", signature.numerator, signature.denominator)
}

/// The one thing of `now` that `was` does not have, and the one thing of
/// `was` that `now` does not have: what a single edit of a list leaves.
fn one_changed<T: PartialEq + Copy>(was: &[T], now: &[T]) -> (Option<T>, Option<T>) {
    let only = |these: &[T], those: &[T]| {
        let mut left = these.iter().filter(|item| !those.contains(item));
        left.next().copied().filter(|_| left.next().is_none())
    };
    (only(now, was), only(was, now))
}

/// What a step did to the tempo along the song.
fn tempo_change(before: &Project, after: &Project) -> String {
    let (was, now) = (&before.tempo_changes, &after.tempo_changes);
    let at = |project: &Project, beat: f64| project.meter_map().said(beat);
    match one_changed(was, now) {
        (Some(set), _) if now.len() + 1 >= was.len() && now.len() <= was.len() + 1 => match set.ramp {
            true => format!("Made the tempo glide to {} BPM by {}", set.tempo, at(after, set.beat)),
            false => format!("Set the tempo to {} BPM at {}", set.tempo, at(after, set.beat)),
        },
        (None, Some(gone)) if now.len() + 1 == was.len() => {
            format!("Removed the change of tempo at {}", at(before, gone.beat))
        }
        _ if now.is_empty() => "Removed every change of tempo".to_owned(),
        _ => "Changed the tempo along the song".to_owned(),
    }
}

/// What a step did to the time signature along the song.
fn meter_change(before: &Project, after: &Project) -> String {
    let (was, now) = (&before.meter_changes, &after.meter_changes);
    match one_changed(was, now) {
        (Some(set), _) if now.len() + 1 >= was.len() && now.len() <= was.len() + 1 => {
            format!("Set the time signature to {} at {}", signature(set.signature), after.meter_map().said(set.beat))
        }
        (None, Some(gone)) if now.len() + 1 == was.len() => {
            format!("Removed the change of time signature at {}", before.meter_map().said(gone.beat))
        }
        _ if now.is_empty() => "Removed every change of time signature".to_owned(),
        _ => "Changed the time signature along the song".to_owned(),
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
        // The channel the new notes are on, where they share one that is
        // not the first.
        let on = match put.split_first() {
            Some((first, rest))
                if !first.channel().is_first() && rest.iter().all(|code| code.channel() == first.channel()) =>
            {
                format!(" on channel {}", first.channel())
            }
            _ => String::new(),
        };
        let moved = match (before.clip(*clip), after.clip(*clip)) {
            (Ok(was), Ok(now)) => rechannelled(was, now),
            _ => None,
        };
        said.push(match (cut, put.len(), moved) {
            (0, added, _) => format!("Added {} to {name}{on}", count(added, "note", "notes")),
            (removed, 0, _) => format!("Removed {} from {name}", count(*removed, "note", "notes")),
            (_, _, Some((moved, channel))) => {
                format!("Put {} of {name} on channel {channel}", count(moved, "note", "notes"))
            }
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
            Ok(was) => said.push(device_change(was, now, *state, after)),
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
                    // The same points along another travel are another curve.
                    Some(was) if was.points == lane.points && was.enabled == lane.enabled => match lane.travel {
                        crate::Travel::Linear => format!("Made the automation of {what} go straight in its units"),
                        _ => format!("Made the automation of {what} follow its control"),
                    },
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
                            AutomationTarget::Send { track, to } => {
                                after.track(*track).is_ok() && after.track(*to).is_ok()
                            }
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
                    (before.key != after.key, "key"),
                    (before.tempo_changes != after.tempo_changes, "tempo along the song"),
                    (before.meter_changes != after.meter_changes, "time signature along the song"),
                    (before.loop_region != after.loop_region, "loop"),
                    (before.master.volume_db != after.master.volume_db, "master volume"),
                ] {
                    if differs {
                        changed.push(what);
                    }
                }
                said.push(match changed.as_slice() {
                    ["tempo"] => format!("Set the tempo to {} BPM", after.tempo),
                    ["time signature"] => format!("Set the time signature to {}", signature(after.time_signature)),
                    ["key"] => match after.key {
                        Some(key) => format!("Said the song is in {}", key.name()),
                        None => "Took back what key the song is in".to_owned(),
                    },
                    ["tempo along the song"] => tempo_change(before, after),
                    ["time signature along the song"] => meter_change(before, after),
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
        Note { pitch: 60, start, length: 0.5, velocity: 100, ..Default::default() }
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
        let mut with_audio = project.clone();
        let content = crate::project::AudioContent::new("A/fx/riser.wav");
        let riser = with_audio.add_audio_clip(bass, "Riser", 32.0, 4.0, content).unwrap();
        let relink = crate::Relink { place: crate::FilePlace::Clip(riser), path: r"B\fx\Riser 2.wav".into() };
        assert_eq!(
            words(&with_audio, |p| p.relink(std::slice::from_ref(&relink), &crate::files::NoDeviceFiles).unwrap()),
            "Had the clip \u{201C}Riser\u{201D} play \u{201C}Riser 2.wav\u{201D}"
        );
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
        // Channels are said where they are not the first: notes written
        // on one, notes put on one, a curve shaped on one.
        let second = Channel::new(2).unwrap();
        assert_eq!(
            words(p, |p| p
                .clip_mut(hook)
                .unwrap()
                .add_notes(&[note_at(0.0).on(second), note_at(1.0).on(second)])
                .unwrap()),
            "Added 2 notes to \u{201C}Hook\u{201D} on channel 2"
        );
        assert_eq!(
            words(&with_notes, |p| {
                p.clip_mut(hook).unwrap().set_note_channels(second, |_| true).unwrap();
            }),
            "Put 2 notes of \u{201C}Hook\u{201D} on channel 2"
        );
        let point = vec![crate::AutomationPoint::new(0.0, 0.5)];
        assert_eq!(
            words(&with_notes, |p| {
                let clip = p.clip_mut(hook).unwrap();
                clip.set_controller_on(second, crate::ControllerKind::PitchBend, point.clone()).unwrap()
            }),
            "Shaped a controller of \u{201C}Hook\u{201D} on channel 2"
        );
        assert_eq!(
            words(&with_notes, |p| {
                p.clip_mut(hook).unwrap().set_controller(crate::ControllerKind::EXPRESSION, point.clone()).unwrap()
            }),
            "Shaped a controller of \u{201C}Hook\u{201D}"
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
            words(p, |p| p.set_sidechain(limiter, Some(bass)).unwrap()),
            "Had \u{201C}Limiter\u{201D} listen to \u{201C}Bass\u{201D}"
        );
        let mut keyed = p.clone();
        keyed.set_sidechain(limiter, Some(bass)).unwrap();
        assert_eq!(
            words(&keyed, |p| p.set_sidechain(limiter, None).unwrap()),
            "Had \u{201C}Limiter\u{201D} listen to its own input"
        );
        assert_eq!(
            words(p, |p| p.device_mut(synth).unwrap().params.insert("cutoff".into(), 900.0)),
            "Adjusted \u{201C}Poly Synth\u{201D}"
        );
        assert_eq!(
            words(p, |p| p.device_mut(synth).unwrap().state = Some(vec![1, 2, 3])),
            "Changed the sound of \u{201C}Poly Synth\u{201D}"
        );
        // A preset the device was given; and a track's sound that was
        // saved, which its effects are part of without being on it.
        let tone = crate::presets::PresetRef { id: "tone".into(), name: "Tone".into() };
        assert_eq!(
            words(p, |p| p.device_mut(synth).unwrap().preset = Some(tone.clone())),
            "Set \u{201C}Poly Synth\u{201D} to the preset \u{201C}Tone\u{201D}"
        );
        assert_eq!(
            words(p, |p| p.device_mut(limiter).unwrap().preset = Some(crate::presets::PresetRef::brought_by(&tone))),
            "Made \u{201C}Limiter\u{201D} part of the sound \u{201C}Tone\u{201D}"
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
        // The same points along another travel are another curve.
        assert_eq!(
            words(&automated, |p| p.set_automation_travel(&volume, crate::Travel::Linear).unwrap()),
            "Made the automation of the volume of \u{201C}Lead\u{201D} go straight in its units"
        );
        let mut straight = automated.clone();
        straight.set_automation_travel(&volume, crate::Travel::Linear).unwrap();
        assert_eq!(
            words(&straight, |p| p.set_automation_travel(&volume, crate::Travel::FADER).unwrap()),
            "Made the automation of the volume of \u{201C}Lead\u{201D} follow its control"
        );
        // A send that is opened by its envelope, and the envelope taken away.
        let mut with_return = project.clone();
        let echo = with_return.add_track_of_kind("Echo", crate::TrackKind::Return);
        let throw = AutomationTarget::Send { track: lead, to: echo };
        assert_eq!(
            words(&with_return, |p| p.set_automation(throw.clone(), vec![AutomationPoint::new(0.0, -6.0)]).unwrap()),
            "Automated the send of \u{201C}Lead\u{201D} to \u{201C}Echo\u{201D}"
        );
        let mut thrown = with_return.clone();
        thrown.set_automation(throw.clone(), vec![AutomationPoint::new(0.0, -6.0)]).unwrap();
        assert_eq!(
            words(&thrown, |p| p.set_automation(throw.clone(), Vec::new()).unwrap()),
            "Removed the automation of the send of \u{201C}Lead\u{201D} to \u{201C}Echo\u{201D}"
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

        // The tempo and the time signature, at the start and along the song.
        assert_eq!(words(p, |p| p.set_time_signature(6, 8).unwrap()), "Set the time signature to 6/8");
        assert_eq!(words(p, |p| p.set_tempo_at(8.0, 140.0, false).unwrap()), "Set the tempo to 140 BPM at bar 3");
        assert_eq!(
            words(p, |p| p.set_tempo_at(18.0, 90.0, true).unwrap()),
            "Made the tempo glide to 90 BPM by bar 5 beat 3"
        );
        assert_eq!(words(p, |p| p.set_time_signature_at(8.0, 7, 8).unwrap()), "Set the time signature to 7/8 at bar 3");
        let mut changing = p.clone();
        changing.set_tempo_at(8.0, 140.0, false).unwrap();
        changing.set_tempo_at(16.0, 100.0, true).unwrap();
        changing.set_time_signature_at(8.0, 7, 8).unwrap();
        changing.set_time_signature_at(15.0, 4, 4).unwrap();
        let p = &changing;
        assert_eq!(words(p, |p| p.set_tempo_at(8.0, 150.0, false).unwrap()), "Set the tempo to 150 BPM at bar 3");
        assert_eq!(words(p, |p| p.remove_tempo_at(16.0).unwrap()), "Removed the change of tempo at bar 5 beat 2");
        assert_eq!(
            words(p, |p| p.remove_time_signature_at(15.0).unwrap()),
            "Removed the change of time signature at bar 5"
        );
        assert_eq!(words(p, |p| p.set_tempo_changes(Vec::new()).unwrap()), "Removed every change of tempo");

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
