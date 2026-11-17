//! What the user is looking at, put into words for the assistant.
//!
//! This is what the assistant in the window has over one that connects from
//! outside: the selection, the playhead, the loop, the tab that is open.
//! It goes along with every message, so that "make this busier" has a
//! "this". The tools still hold the truth; the words here only point.
//!
//! Positions are given in beats, as the tools take them, with the bar,
//! beat and sixteenth the window shows beside them. Things are named, and
//! given the id the tools know them by.

use crate::ui::snapshot::Live;
use crate::ui::state::{DetailTab, UiState};
use crate::ui::timeline;
use std::fmt::Write;
use trill_core::pitch::pitch_name;
use trill_core::{Beats, ChainRef, Clip, MeterMap, Note, Project, Track, TrackKind};

/// Notes are listed one by one up to this many; beyond it they are summed
/// up, and the assistant reads the rest with a tool if it wants them.
const NOTES_LISTED: usize = 16;
/// Clips are listed one by one up to this many.
const CLIPS_LISTED: usize = 12;

/// What the assistant is told with a message.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Context {
    /// The text handed over, one fact to a line.
    pub text: String,
    /// The same in a few words, for the line above the message field that
    /// shows the user what the assistant will see.
    pub summary: Vec<String>,
}

/// A number of beats without needless decimals: `16`, `0.5`, `2.333`.
fn beats(value: Beats) -> String {
    let rounded = (value * 1000.0).round() / 1000.0;
    // Negative zero would print as "-0".
    format!("{}", if rounded == 0.0 { 0.0 } else { rounded })
}

/// `beat 16 (5.1.1)`.
fn at(beat: Beats, signature: &MeterMap) -> String {
    format!("beat {} ({})", beats(beat), timeline::format_position(beat, signature))
}

/// `beats 16 to 32 (5.1.1 to 9.1.1)`.
fn span(start: Beats, end: Beats, signature: &MeterMap) -> String {
    format!(
        "beats {} to {} ({} to {})",
        beats(start),
        beats(end),
        timeline::format_position(start, signature),
        timeline::format_position(end, signature)
    )
}

/// The span as the window's ruler would label it: `5 to 9`, `5.3 to 6`.
fn short_span(start: Beats, end: Beats, signature: &MeterMap) -> String {
    format!("{}\u{2013}{}", timeline::format_bar_beat(start, signature), timeline::format_bar_beat(end, signature))
}

fn track_kind(track: &Track) -> &'static str {
    match track.kind {
        TrackKind::Normal => "",
        TrackKind::Group => ", a group",
        TrackKind::Return => ", a return",
    }
}

fn clip_line(track: &Track, clip: &Clip, signature: &MeterMap) -> String {
    let holds = match (clip.midi(), clip.audio()) {
        (Some(_), _) => match clip.notes().len() {
            1 => "MIDI, 1 note".to_owned(),
            count => format!("MIDI, {count} notes"),
        },
        (None, Some(audio)) => {
            let file = std::path::Path::new(&audio.path).file_name().map(|name| name.to_string_lossy().into_owned());
            format!("audio, {}", file.unwrap_or_else(|| audio.path.clone()))
        }
        (None, None) => "empty".to_owned(),
    };
    let muted = if clip.muted { ", muted" } else { "" };
    format!(
        "\"{}\" ({}, on \"{}\", {}, {holds}{muted})",
        clip.name,
        clip.id,
        track.name,
        span(clip.start, clip.end(), signature)
    )
}

/// The notes selected in the piano roll, which are positioned from the
/// start of their clip.
fn notes_line(notes: &[Note], clip: &Clip) -> String {
    let (mut low, mut high) = (u8::MAX, u8::MIN);
    let (mut first, mut last) = (f64::INFINITY, f64::NEG_INFINITY);
    for note in notes {
        low = low.min(note.pitch);
        high = high.max(note.pitch);
        first = first.min(note.start);
        last = last.max(note.start + note.length);
    }
    let pitches = if low == high { pitch_name(low) } else { format!("{} to {}", pitch_name(low), pitch_name(high)) };
    let mut line = format!(
        "{} in \"{}\" ({}), {pitches}, from beat {} to {} of the clip",
        notes.len(),
        clip.name,
        clip.id,
        beats(first),
        beats(last)
    );
    if notes.len() <= NOTES_LISTED {
        let mut sorted: Vec<&Note> = notes.iter().collect();
        sorted.sort_by(|a, b| a.start.total_cmp(&b.start).then(a.pitch.cmp(&b.pitch)));
        let listed: Vec<String> = sorted
            .iter()
            .map(|note| {
                format!(
                    "[{}, {}, {}, {}]",
                    pitch_name(note.pitch),
                    beats(note.start),
                    beats(note.length),
                    note.velocity
                )
            })
            .collect();
        let _ = write!(line, "; as [pitch, start, length, velocity]: {}", listed.join(" "));
    }
    line
}

/// Puts what the window shows into words.
pub fn describe(project: &Project, state: &UiState, live: &Live) -> Context {
    let signature = project.time_signature;
    // Bars are counted through every change of time signature.
    let bars = &project.meter_map();
    let mut text = String::new();
    let mut summary = Vec::new();
    let mut line = |line: String| {
        text.push_str(&line);
        text.push('\n');
    };

    line(format!(
        "project: \"{}\", {} BPM, {}/{} time (a bar is {} beats)",
        project.name,
        beats(project.tempo),
        signature.numerator,
        signature.denominator,
        beats(signature.beats_per_bar())
    ));
    let changes = project.tempo_changes.len() + project.meter_changes.len();
    if changes > 0 {
        line(format!(
            "the tempo or the time signature changes {changes} time(s) along the song: those above are what it begins \
             with, and get_tempo_map has every change with its bar and beat"
        ));
    }
    line("positions: in beats from the start of the song, then (bar.beat.sixteenth) as the window shows them".into());
    line(format!("playhead: {}, {}", at(live.position, bars), if live.playing { "playing" } else { "stopped" }));
    let region = project.loop_region;
    line(format!("loop: {}, {}", if region.enabled { "on" } else { "off" }, span(region.start, region.end, bars)));
    line(format!(
        "detail panel: {}",
        match state.detail_tab {
            DetailTab::Clip => "the Clip tab (the shown clip in the piano roll, or its audio)",
            DetailTab::Devices => "the Devices tab (the device chain of the selected track)",
            DetailTab::Mixer => "the Mixer tab (a channel strip for every track)",
        }
    ));

    // The track, or the master in its place.
    let track = state.selected_track.and_then(|id| project.track(id).ok());
    match (state.shown_chain(), track) {
        (ChainRef::Master, _) if state.master_selected || track.is_none() => {
            line(if project.tracks.is_empty() {
                "selected track: none (the song has no tracks yet)".to_owned()
            } else {
                "selected track: the master".to_owned()
            });
        }
        (_, Some(track)) => {
            line(format!("selected track: \"{}\" ({}{})", track.name, track.id, track_kind(track)));
        }
        (_, None) => line("selected track: none".to_owned()),
    }

    // The clips, the one the Clip tab shows last.
    let clips: Vec<(&Track, &Clip)> = state
        .selected_clips
        .iter()
        .filter_map(|id| {
            project.tracks.iter().find_map(|track| track.clips.iter().find(|clip| clip.id == *id).map(|c| (track, c)))
        })
        .collect();
    match clips.as_slice() {
        [] => line("selected clips: none".to_owned()),
        [(track, clip)] => line(format!("selected clip: {}", clip_line(track, clip, bars))),
        many => {
            line(format!("selected clips: {}", many.len()));
            for (track, clip) in many.iter().take(CLIPS_LISTED) {
                line(format!("- {}", clip_line(track, clip, bars)));
            }
            if many.len() > CLIPS_LISTED {
                line(format!("- and {} more", many.len() - CLIPS_LISTED));
            }
        }
    }
    let shown = state.selected_clip.and_then(|id| clips.iter().find(|(_, clip)| clip.id == id));
    if let (Some((_, clip)), true) = (shown, clips.len() > 1) {
        line(format!("shown in the Clip tab: \"{}\" ({})", clip.name, clip.id));
    }
    if let (Some((_, clip)), false) = (shown, state.selected_notes.is_empty()) {
        line(format!("selected notes: {}", notes_line(&state.selected_notes, clip)));
    }

    // The device the Devices tab and the browser's presets are about.
    let chain = state.shown_chain();
    let device = state.selected_device.and_then(|id| project.chain(chain).ok()?.iter().find(|device| device.id == id));
    if let Some(device) = device {
        let owner = match (chain, track) {
            (ChainRef::Track(_), Some(track)) => format!("on \"{}\"", track.name),
            _ => "on the master".to_owned(),
        };
        let bypassed = if device.enabled { "" } else { ", bypassed" };
        line(format!("selected device: \"{}\" ({}, {}{bypassed}) {owner}", device.name, device.id, device.uri));
    }

    let view = &state.arrangement;
    match &view.time_selection {
        Some(selection) => {
            let names: Vec<String> = selection
                .tracks
                .iter()
                .filter_map(|id| project.track(*id).ok())
                .map(|track| format!("\"{}\"", track.name))
                .collect();
            let on = if names.is_empty() { "every track".to_owned() } else { names.join(", ") };
            line(format!("time selection: {} on {on}", span(selection.start, selection.end, bars)));
        }
        None => line("time selection: none".to_owned()),
    }
    line(format!("edit cursor: {}", at(view.cursor, bars)));

    // The few words shown above the message field.
    match (clips.as_slice(), track) {
        ([], _) if state.master_selected => summary.push("Master".to_owned()),
        ([], Some(track)) => summary.push(track.name.clone()),
        ([], None) => {}
        ([(track, clip)], _) => summary.push(format!("{} / {}", track.name, clip.name)),
        (many, _) => summary.push(format!("{} clips", many.len())),
    }
    if shown.is_some() && !state.selected_notes.is_empty() {
        summary.push(match state.selected_notes.len() {
            1 => "1 note".to_owned(),
            count => format!("{count} notes"),
        });
    }
    if let (Some(device), DetailTab::Devices) = (device, state.detail_tab) {
        summary.push(device.name.clone());
    }
    if let Some(selection) = &view.time_selection {
        summary.push(format!("bars {}", short_span(selection.start, selection.end, bars)));
    }
    summary.push(if region.enabled {
        format!("loop {}", short_span(region.start, region.end, bars))
    } else {
        format!("playhead {}", timeline::format_bar_beat(live.position, bars))
    });

    Context { text, summary }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::state::TimeSelection;
    use trill_core::TrackId;

    fn note(pitch: u8, start: Beats) -> Note {
        Note { pitch, start, length: 0.5, velocity: 96 }
    }

    /// A song with two tracks, a clip with notes on the first, and a synth
    /// and a delay on it.
    fn song() -> (Project, TrackId, TrackId) {
        let mut project = Project::new("Night Drive");
        project.set_tempo(96.0).unwrap();
        let lead = project.add_track("Lead");
        let bass = project.add_track("Bass");
        project.add_device(ChainRef::Track(lead), "builtin:polysynth", "Poly Synth", None).unwrap();
        project.add_device(ChainRef::Track(lead), "builtin:delay", "Delay", None).unwrap();
        project.add_device(ChainRef::Master, "builtin:limiter", "Limiter", None).unwrap();
        let clip = project.add_midi_clip(lead, "Hook", 16.0, 8.0).unwrap();
        project.clip_mut(clip).unwrap().add_notes(&[note(60, 0.0), note(64, 1.0), note(67, 2.5)]).unwrap();
        project.add_midi_clip(bass, "Line", 16.0, 16.0).unwrap();
        (project, lead, bass)
    }

    fn lines(context: &Context) -> Vec<&str> {
        context.text.lines().collect()
    }

    fn line<'a>(context: &'a Context, key: &str) -> &'a str {
        context.text.lines().find(|line| line.starts_with(key)).unwrap_or_else(|| panic!("no {key} in {context:?}"))
    }

    #[test]
    fn an_untouched_window_still_says_where_things_stand() {
        let project = Project::new("Untitled");
        let mut state = UiState::new(DetailTab::Clip);
        state.reconcile(&project);
        let context = describe(&project, &state, &Live::default());
        assert_eq!(
            lines(&context),
            [
                "project: \"Untitled\", 120 BPM, 4/4 time (a bar is 4 beats)",
                "positions: in beats from the start of the song, then (bar.beat.sixteenth) as the window shows them",
                "playhead: beat 0 (1.1.1), stopped",
                "loop: off, beats 0 to 16 (1.1.1 to 5.1.1)",
                "detail panel: the Clip tab (the shown clip in the piano roll, or its audio)",
                "selected track: none (the song has no tracks yet)",
                "selected clips: none",
                "time selection: none",
                "edit cursor: beat 0 (1.1.1)",
            ]
        );
        assert_eq!(context.summary, ["playhead 1"]);
    }

    #[test]
    fn the_selection_is_named_with_the_ids_the_tools_take() {
        let (project, lead, _) = song();
        let hook = project.track(lead).unwrap().clips[0].id;
        let synth = project.track(lead).unwrap().devices[0].id;
        let mut state = UiState::new(DetailTab::Clip);
        state.reconcile(&project);
        state.select_clip(lead, hook);
        state.selected_notes = vec![note(64, 1.0), note(60, 0.0)];
        let mut live = Live::default();
        (live.playing, live.position) = (true, 18.5);

        let context = describe(&project, &state, &live);
        assert_eq!(line(&context, "project"), "project: \"Night Drive\", 96 BPM, 4/4 time (a bar is 4 beats)");
        assert_eq!(line(&context, "playhead"), "playhead: beat 18.5 (5.3.3), playing");
        assert_eq!(line(&context, "selected track"), format!("selected track: \"Lead\" ({lead})"));
        assert_eq!(
            line(&context, "selected clip"),
            format!("selected clip: \"Hook\" ({hook}, on \"Lead\", beats 16 to 24 (5.1.1 to 7.1.1), MIDI, 3 notes)")
        );
        assert_eq!(
            line(&context, "selected notes"),
            format!(
                "selected notes: 2 in \"Hook\" ({hook}), C4 to E4, from beat 0 to 1.5 of the clip; as [pitch, start, \
                 length, velocity]: [C4, 0, 0.5, 96] [E4, 1, 0.5, 96]"
            )
        );
        assert_eq!(
            line(&context, "selected device"),
            format!("selected device: \"Poly Synth\" ({synth}, builtin:polysynth) on \"Lead\"")
        );
        assert_eq!(context.summary, ["Lead / Hook", "2 notes", "playhead 5.3"]);
    }

    #[test]
    fn several_clips_a_time_selection_and_the_loop() {
        let (mut project, lead, bass) = song();
        project.set_loop(true, 16.0, 32.0).unwrap();
        let hook = project.track(lead).unwrap().clips[0].id;
        let bass_line = project.track(bass).unwrap().clips[0].id;
        let mut state = UiState::new(DetailTab::Mixer);
        state.reconcile(&project);
        state.select_clip(lead, hook);
        state.toggle_clip(bass, bass_line);
        state.arrangement.time_selection = Some(TimeSelection { start: 16.0, end: 24.0, tracks: vec![lead, bass] });
        state.arrangement.cursor = 16.0;

        let context = describe(&project, &state, &Live::default());
        let text = lines(&context);
        let clips = text.iter().position(|line| *line == "selected clips: 2").expect("the count");
        assert!(text[clips + 1].starts_with("- \"Hook\" (") && text[clips + 2].starts_with("- \"Line\" ("), "{text:?}");
        assert_eq!(line(&context, "shown in"), format!("shown in the Clip tab: \"Line\" ({bass_line})"));
        assert_eq!(line(&context, "loop"), "loop: on, beats 16 to 32 (5.1.1 to 9.1.1)");
        assert_eq!(
            line(&context, "time selection"),
            "time selection: beats 16 to 24 (5.1.1 to 7.1.1) on \"Lead\", \"Bass\""
        );
        assert_eq!(line(&context, "edit cursor"), "edit cursor: beat 16 (5.1.1)");
        assert_eq!(line(&context, "detail panel"), "detail panel: the Mixer tab (a channel strip for every track)");
        assert_eq!(line(&context, "selected track"), format!("selected track: \"Bass\" ({bass})"));
        assert_eq!(context.summary, ["2 clips", "bars 5\u{2013}7", "loop 5\u{2013}9"]);

        // A selection drawn on the ruler spans every track.
        state.arrangement.time_selection = Some(TimeSelection { start: 0.0, end: 6.0, tracks: Vec::new() });
        let context = describe(&project, &state, &Live::default());
        assert_eq!(line(&context, "time selection"), "time selection: beats 0 to 6 (1.1.1 to 2.3.1) on every track");
    }

    #[test]
    fn the_master_and_its_devices() {
        let (project, ..) = song();
        let limiter = project.master.devices[0].id;
        let mut state = UiState::new(DetailTab::Devices);
        state.reconcile(&project);
        state.select_master();
        state.resolve_device(&project);

        let context = describe(&project, &state, &Live::default());
        assert_eq!(line(&context, "selected track"), "selected track: the master");
        assert_eq!(
            line(&context, "selected device"),
            format!("selected device: \"Limiter\" ({limiter}, builtin:limiter) on the master")
        );
        assert_eq!(context.summary, ["Master", "Limiter", "playhead 1"]);
    }

    #[test]
    fn many_notes_are_summed_up_and_odd_meters_counted() {
        let mut project = Project::new("Six");
        project.set_time_signature(6, 8).unwrap();
        let track = project.add_track("Keys");
        let clip = project.add_midi_clip(track, "Run", 0.0, 12.0).unwrap();
        let notes: Vec<Note> = (0..40).map(|index| note(48 + index as u8 % 12, index as f64 * 0.25)).collect();
        project.clip_mut(clip).unwrap().add_notes(&notes).unwrap();
        let mut state = UiState::new(DetailTab::Clip);
        state.reconcile(&project);
        state.select_clip(track, clip);
        state.selected_notes = notes;

        let mut live = Live::default();
        live.position = 3.0;
        let context = describe(&project, &state, &live);
        assert_eq!(line(&context, "project"), "project: \"Six\", 120 BPM, 6/8 time (a bar is 3 beats)");
        assert_eq!(line(&context, "playhead"), "playhead: beat 3 (2.1.1), stopped");
        assert_eq!(
            line(&context, "selected notes"),
            format!("selected notes: 40 in \"Run\" ({clip}), C3 to B3, from beat 0 to 10.25 of the clip")
        );
    }

    #[test]
    fn numbers_read_plainly() {
        assert_eq!(beats(16.0), "16");
        assert_eq!(beats(0.5), "0.5");
        assert_eq!(beats(7.0 / 3.0), "2.333");
        assert_eq!(beats(-0.0), "0");
        assert_eq!(beats(120.25), "120.25");
    }
}
