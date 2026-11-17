//! Tools for how the song was made: the session's journal.
//!
//! * `get_journal` lists the story, a page at a time: the steps that
//!   changed the song, and on request what was said and looked at between
//!   them.
//! * `get_song_at` shows the song as it was after any step, the way
//!   `get_project` shows it as it is.
//! * `journal` is for the journal itself: whether one is kept, writing it
//!   out to hand on, deleting it.
//!
//! The journal is the engine's (`trill_core::journal`, kept by the
//! session). What is here is replies short enough to read, and what the
//! assistant in the window is not allowed: it works beside the
//! user, on what it reads, and must not be talked into deleting the record
//! of what it did, switching it off, or writing the conversation out to a
//! file.

use crate::requests::{GetJournalRequest, GetSongAtRequest, JournalAction, JournalBy, JournalRequest, JournalShow};
use crate::server::{Reply, TrillServer, finish};
use crate::view::{device_json, lane_json, round, track_json};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{tool, tool_router};
use serde_json::{Value, json};
use trill_core::Project;
use trill_core::journal::{Author, Entry, Journal, Place, PlanState, Reversal, States, What};

/// Appended to the server's instructions.
pub(crate) const INSTRUCTIONS: &str = "\
How the song was made
Trill keeps a journal of the session: every change to the song is a step in it, with who made it (the user at the \
window, the assistant in the window, or a program like you), when, what it was in words and where in the song it \
landed. Your own tool calls are in it as you make them.
- get_journal lists the steps, oldest first, a page at a time. `search` finds them by their words (\"chorus\", \
\"reverb\"), `by` keeps one author's, `last` gives the latest. With `show` \"all\" it also lists what was said in \
the window's conversation and the calls that only looked or listened, with what they heard.
- get_song_at shows the song as it was after any step, as get_project shows it now. To answer \"what changed \
since the first chorus was written\": find that step with get_journal `search`, then list the journal `after` it; \
to compare two moments, read the song at each.
- Undoing takes nothing out of the journal: a step that was undone is followed by a step that takes it back. \
Use history to undo; use the journal to see what happened.
- journal says whether one is kept and how large it is, switches it on or off, writes it out as a file to hand \
on (without the conversation unless asked), or deletes it. The journal lives on this machine in Trill's own \
folder, never in the project file: saving or sending a song does not send its story.";

/// The entries a page holds when none is asked for, and the most.
const DEFAULT_ENTRIES: usize = 40;
const MAX_ENTRIES: usize = 200;
/// How much of what was said a listing shows.
const SAID_CHARS: usize = 400;

/// Days since 1970 as a date: the civil calendar, worked out by hand so
/// that a date needs no library.
fn date(days: i64) -> (i64, i64, i64) {
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era = (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_part = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_part + 2) / 5 + 1;
    let month = if month_part < 10 { month_part + 3 } else { month_part - 9 };
    (year_of_era + era * 400 + i64::from(month <= 2), month, day)
}

/// A moment as it is written in a reply: "2026-11-16 14:03 UTC".
pub(crate) fn when(milliseconds: u64) -> String {
    let seconds = (milliseconds / 1000) as i64;
    let (year, month, day) = date(seconds.div_euclid(86_400));
    let in_day = seconds.rem_euclid(86_400);
    format!("{year:04}-{month:02}-{day:02} {:02}:{:02} UTC", in_day / 3600, in_day % 3600 / 60)
}

fn author(by: &Author) -> &str {
    match by {
        Author::Person => "person",
        Author::Assistant => "assistant",
        Author::Client(name) if name.is_empty() => "client",
        Author::Client(name) => name,
    }
}

/// Where a step landed, by the ids the other tools take.
fn where_json(places: &[Place]) -> Option<Value> {
    let (mut tracks, mut clips, mut devices, mut beats) = (Vec::new(), Vec::new(), Vec::new(), None);
    let mut envelopes = 0;
    for place in places {
        match place {
            Place::Track { id, .. } => tracks.push(*id),
            Place::Clips { clips: named } => clips.extend(named.iter().map(|(id, _)| *id)),
            Place::Device { id, .. } => devices.push(*id),
            Place::Lane { .. } => envelopes += 1,
            Place::Span { start, end } => beats = Some(json!([round(*start), round(*end)])),
        }
    }
    let mut out = json!({});
    if !tracks.is_empty() {
        out["tracks"] = json!(tracks);
    }
    if !clips.is_empty() {
        out["clips"] = json!(clips);
    }
    if !devices.is_empty() {
        out["devices"] = json!(devices);
    }
    if envelopes > 0 {
        out["envelopes"] = json!(envelopes);
    }
    if let Some(beats) = beats {
        out["beats"] = beats;
    }
    out.as_object().is_some_and(|object| !object.is_empty()).then_some(out)
}

/// What is said of one entry in a listing.
fn entry_json(entry: &Entry, began: u64) -> Value {
    let mut out = json!({
        "step": entry.seq,
        "t": round(entry.at.saturating_sub(began) as f64 / 1000.0),
        "by": author(&entry.by),
    });
    match &entry.what {
        What::Step(step) => {
            out["did"] = json!(step.label);
            if let Some(tool) = &step.tool {
                out["tool"] = json!(tool);
            }
            if let Some(found) = where_json(&step.places) {
                out["where"] = found;
            }
            if let Some(heard) = &step.heard {
                out["heard"] = json!(heard);
            }
            if let Some(reversed) = step.reverses {
                let key = match reversed.kind {
                    Reversal::Undo => "took_back",
                    Reversal::Redo => "brought_back",
                };
                out[key] = match (reversed.first, reversed.last) {
                    (0, _) => json!("steps from before this journal"),
                    (first, last) if first == last => json!(first),
                    (first, last) => json!([first, last]),
                };
            }
        }
        What::Said { text } => {
            let shown: String = text.chars().take(SAID_CHARS).collect();
            out["said"] = json!(if shown.len() < text.len() { format!("{shown}\u{2026}") } else { shown });
        }
        What::Plan { items } => {
            let lines = items.iter().map(|item| {
                let mark = match item.state {
                    PlanState::Done => "[x]",
                    PlanState::Doing => "[>]",
                    PlanState::Pending => "[ ]",
                };
                format!("{mark} {}", item.text)
            });
            out["plan"] = lines.collect();
        }
        What::Look(look) => {
            out["looked"] = json!(look.label);
            if let Some(heard) = &look.heard {
                out["heard"] = json!(heard);
            }
            if look.failed {
                out["failed"] = json!(true);
            }
        }
        What::Mark { label } => out["note"] = json!(label),
    }
    out
}

/// Whether a listing shows `entry`.
fn listed(entry: &Entry, request: &GetJournalRequest, search: Option<&str>) -> bool {
    if entry.is_aside() {
        return false;
    }
    if request.show == JournalShow::Steps && entry.step().is_none() {
        return false;
    }
    let by = match (request.by, &entry.by) {
        (None, _) => true,
        (Some(JournalBy::Person), by) => *by == Author::Person,
        (Some(JournalBy::Assistant), by) => *by == Author::Assistant,
        (Some(JournalBy::Client), by) => matches!(by, Author::Client(_)),
    };
    by && search.is_none_or(|search| entry.line().to_lowercase().contains(search))
}

/// A page of the journal.
fn journal_json(journal: &Journal, request: &GetJournalRequest) -> Value {
    let began = journal.began();
    let limit = request.last.or(request.limit).unwrap_or(DEFAULT_ENTRIES).clamp(1, MAX_ENTRIES);
    let search = request.search.as_deref().map(str::trim).filter(|search| !search.is_empty()).map(str::to_lowercase);
    let matching = journal.entries().filter(|entry| listed(entry, request, search.as_deref()));
    let mut out = json!({
        "keeping": true,
        "began": when(began),
        "steps": journal.steps(),
        "last_step": journal.next_seq() - 1,
    });
    let dropped = journal.dropped();
    if dropped.entries > 0 {
        out["earlier"] = json!(format!(
            "{} earlier entries ({} of them steps) were let go to keep the journal small; the story now begins \
             with the song as it was after them. The oldest go first.",
            dropped.entries, dropped.steps
        ));
    }
    let (page, more): (Vec<&Entry>, Option<u64>) = match request.last {
        Some(_) => {
            let mut latest: Vec<&Entry> = matching.rev().take(limit).collect();
            latest.reverse();
            (latest, None)
        }
        None => {
            let after = request.after.unwrap_or(0);
            let mut from = matching.filter(|entry| entry.seq > after);
            let page: Vec<&Entry> = from.by_ref().take(limit).collect();
            let more = from.next().and(page.last()).map(|last| last.seq);
            (page, more)
        }
    };
    out["entries"] = page.iter().map(|entry| entry_json(entry, began)).collect();
    if let Some(after) = more {
        out["more"] = json!({ "after": after });
    }
    if page.is_empty() {
        let unfiltered = search.is_none() && request.by.is_none();
        out["note"] = json!(match (journal.steps(), request.after) {
            (0, _) if unfiltered => "Nothing has changed in this song since its journal began.",
            (_, Some(_)) => "Nothing after that step.",
            (_, None) => "No entry matches. Try `show` \"all\", or other words.",
        });
    }
    out
}

/// A song without a session behind it: what `get_project` says of the
/// open one, less what only a running song has.
fn song_json(project: &Project, include_notes: bool) -> Value {
    let beats_per_bar = project.time_signature.beats_per_bar();
    let length = project.length_beats();
    let mut out = json!({
        "name": project.name,
        "tempo": project.tempo,
        "time_signature": format!("{}/{}", project.time_signature.numerator, project.time_signature.denominator),
        "length_beats": round(length),
        "length_bars": round(length / beats_per_bar),
        "tracks": project.tracks.iter().map(|track| track_json(track, beats_per_bar, include_notes)).collect::<Vec<_>>(),
        "master": {
            "volume_db": project.master.volume_db,
            "devices": project.master.devices.iter().map(device_json).collect::<Vec<_>>(),
        },
        "automation": project.automation.iter().map(|lane| lane_json(lane, false)).collect::<Vec<_>>(),
    });
    if project.loop_region.enabled {
        out["loop"] = json!({ "start": project.loop_region.start, "end": project.loop_region.end });
    }
    if !project.markers.is_empty() {
        out["markers"] = project
            .markers
            .iter()
            .map(|marker| json!({ "beat": round(marker.beat), "bar": round(marker.beat / beats_per_bar + 1.0), "name": marker.name }))
            .collect();
    }
    out
}

/// What to say where no journal is kept.
const NOT_KEPT: &str = "No journal is being kept: it was switched off. `journal` with action \"keep\" switches it \
                        on, from the song as it is now.";

#[tool_router(router = story_router, vis = "pub(crate)")]
impl TrillServer {
    /// How the song was made: the journal of this session, as a list of steps with who made each, when, what it
    /// was and where it landed. Oldest first, a page at a time; `search` finds steps by their words, `last` gives
    /// the latest. Use it to answer what was changed, by whom, and since when; then `get_song_at` to see the
    /// song as it was.
    #[tool(annotations(title = "Read the journal", read_only_hint = true))]
    fn get_journal(&self, Parameters(request): Parameters<GetJournalRequest>) -> CallToolResult {
        let session = self.session.lock();
        finish(match session.journal() {
            Some(journal) => Ok(journal_json(journal, &request)),
            None => Ok(json!({ "keeping": false, "entries": [], "note": NOT_KEPT })),
        })
    }

    /// The song as it was after a step of the journal: tracks, devices, clips, tempo and markers, as get_project
    /// shows the song as it is. Changes nothing: it is a look at the past, not a way back (use `history` to undo).
    #[tool(annotations(title = "The song as it was", read_only_hint = true))]
    fn get_song_at(&self, Parameters(request): Parameters<GetSongAtRequest>) -> CallToolResult {
        let session = self.session.lock();
        let reply = (|| -> Reply {
            let journal = session.journal().ok_or_else(|| NOT_KEPT.to_string())?;
            let step = request.step.unwrap_or(0);
            let (first, last) = (journal.first_seq(), journal.next_seq() - 1);
            if step > last {
                return Err(format!(
                    "there is no step {step}: the journal's last is {last}. get_journal lists them; the song as \
                     it is now is get_project's"
                ));
            }
            let song = journal.song_at(step, States::Without).map_err(|error| {
                format!("the journal does not add up at step {step}: {error}. The song itself is unharmed")
            })?;
            let mut out = song_json(&song, request.include_notes);
            let entry = journal.index_of(step).and_then(|index| journal.get(index));
            out["as_of"] = match entry {
                Some(entry) => {
                    json!({ "step": step, "did": entry.line(), "by": author(&entry.by), "when": when(entry.at) })
                }
                None => json!({
                    "step": first.saturating_sub(1),
                    "did": "the song as it was when the story begins",
                    "when": when(journal.began()),
                }),
            };
            let changed_since =
                journal.entries().rev().take_while(|entry| entry.seq > step).any(|entry| entry.step().is_some());
            if !changed_since && journal.steps() > 0 {
                out["note"] = json!("Nothing has changed since: this is the song as it is now.");
            }
            Ok(out)
        })();
        finish(reply)
    }

    /// The journal itself: whether one is kept ("status"), keep one or stop ("keep", "stop"), write the story out
    /// as a file to hand on ("export", to `path`), or delete it ("delete"). The journal stays on this machine in
    /// Trill's own folder and is never part of the project file.
    #[tool(annotations(title = "Keep, export or delete the journal", destructive_hint = true))]
    fn journal(&self, Parameters(request): Parameters<JournalRequest>) -> CallToolResult {
        let guarded = self.is_guarded();
        let from_the_window = "The user can do that from the window: Making Of, in the View menu.";
        let reply = (|| -> Reply {
            match request.action {
                JournalAction::Status => {}
                JournalAction::Keep => self.runtime().set_journal_keeping(true).map_err(|error| error.to_string())?,
                JournalAction::Stop if guarded => {
                    return Err(format!("from here the journal cannot be switched off. {from_the_window}"));
                }
                JournalAction::Stop => self.runtime().set_journal_keeping(false).map_err(|error| error.to_string())?,
                JournalAction::Delete if guarded => {
                    return Err(format!("from here the journal cannot be deleted. {from_the_window}"));
                }
                JournalAction::Delete => self.session.lock().delete_journal().map_err(|error| error.to_string())?,
                JournalAction::Export => {
                    if guarded && request.conversation {
                        return Err(format!(
                            "from here the story can only be written out without the conversation. {from_the_window}"
                        ));
                    }
                    let session = self.session.lock();
                    let kind = "the story of a song";
                    let path = self.write_path(&session, request.path.as_deref(), kind, &["trillstory"])?;
                    let path = path.ok_or_else(|| "export needs `path`: where to write the file".to_string())?;
                    session.export_journal(&path, request.conversation).map_err(|error| error.to_string())?;
                    return Ok(json!({
                        "exported": path.display().to_string(),
                        "conversation": request.conversation,
                        "holds": "the song as it was when the story begins and every step since; it can be \
                                  watched in Trill on another machine",
                    }));
                }
            }
            let mut session = self.session.lock();
            let mut out = json!({
                "keeping": session.keeps_journal(),
                "kept_in": "Trill's own folder on this machine, written when the song is saved; never in the \
                            project file",
            });
            if let Some((story, whole)) = session.journal_size() {
                let journal = session.journal().expect("it has a size");
                out["steps"] = json!(journal.steps());
                out["entries"] = json!(journal.len());
                out["began"] = json!(when(journal.began()));
                out["story_bytes"] = json!(story);
                out["bytes"] = json!(whole);
                let limits = journal.limits();
                out["limits"] = json!(format!(
                    "at most {} entries or {} MB of story; past that the oldest entries are let go",
                    limits.entries,
                    limits.bytes >> 20
                ));
            }
            if request.action == JournalAction::Delete {
                out["deleted"] = json!(true);
            }
            Ok(out)
        })();
        finish(reply)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_moment_is_written_as_a_date_and_a_time() {
        assert_eq!(when(0), "1970-01-01 00:00 UTC");
        assert_eq!(when(951_782_400_000), "2000-02-29 00:00 UTC");
        assert_eq!(when(1_794_060_180_000 + 59_999), "2026-11-07 14:03 UTC");
        assert_eq!(when(1_798_761_599_000), "2026-12-31 23:59 UTC");
        assert_eq!(when(1_798_761_600_000), "2027-01-01 00:00 UTC");
    }
}
