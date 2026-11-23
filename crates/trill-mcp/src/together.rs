//! `shared_song`: working in one song with other people, each in a Trill
//! of their own.
//!
//! The tool shares the open song (an invitation to hand to someone),
//! joins someone else's by theirs, leaves, and says who is here: what
//! each has selected and has hold of, and where this program's own work
//! and someone else's clashed (`notices`), each with the edit that puts
//! one's own back. Everything it does is the runtime's
//! ([`trill_runtime::Runtime::share`], `join`, `leave`, `together`) and
//! the session's ([`trill_engine::Session::notices`], `remedy`): a tool
//! call that edits the song is a step of this program's person, said to
//! the others as it is made, whether or not anyone is there.
//!
//! The assistant in the window does not share and does not join. Both
//! open the user's song to someone else, an invitation is text a stranger
//! can supply, and its panel cannot ask: that is the user's to do, in the
//! window (File, Share this song).
//!
//! A song is shared on this computer, on its networks, or through a
//! *meeting point* (`reach`), a small server that two programs at home
//! both connect out to. Which meeting point, and the key its keeper gave,
//! are the user's to set, in the window or in the settings file: no tool
//! takes either, at either door, and no reply says the key. (Whatever a
//! tool is called with is in the journal and in the client's own record
//! of the conversation: a key is neither's to keep.)

use crate::server::{TrillServer, finish};
use crate::view::round;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::schemars::{self, JsonSchema};
use rmcp::{tool, tool_router};
use serde::Deserialize;
use serde_json::{Value, json};
use std::time::{Duration, Instant};
use trill_runtime::{Meeting, Reach, Waiting};

/// Appended to the server's instructions.
pub(crate) const INSTRUCTIONS: &str = "\
Working in one song with other people
- A song can be shared (shared_song): other people then have it open in a Trill of their own and change it at the \
same time as you do. Nobody is the host and nothing is locked. So between two of your calls the song may have \
changed: read what you are about to change again where it matters (get_project, get_notes), and change the notes \
you mean with transform_notes or add_notes rather than writing back a whole clip you read earlier, which would \
remove what someone wrote into it meanwhile. An id that is gone was removed by someone: say so, and do not make \
again what they removed.
- history undoes your own steps, the ones made through this program, and leaves alone what someone else has set \
since: its reply says how many things stayed theirs.
- shared_song with action `status` says who is here, what each has selected and has hold of, and `notices`: where \
work done through this program and someone else's could not both hold (two moves of one clip, a track removed while \
it was being written in). Each notice is a sentence, and where it names an `action`, `put_back` with its id does \
that as one undoable edit. Tell the user about a notice; put theirs back when they ask.
- The audio a shared song plays travels with it: what its clips and takes play, and what its samplers and drum \
racks play, arrives by itself from whoever in the song has it. Status lists what is on its way as `arriving`, each \
with how far it is, from whom, and why it `waits` where it does. A file on its way is not a missing file and no \
problem: do not look for it or relink it, and say that it is arriving. An entry with `played_by` in place of a name \
is a clip or a sample someone else brought into the song whose file their Trill has not said yet: for a moment \
while it reads the file, and for as long as it has not got the file itself or holds it back. Say whose it is to \
put right (theirs: at their computer the file is missing, held, or no audio), and look for nothing on this one. \
Nobody is told where anyone has a file: a \
place is each computer's own, so a path you give a clip or a sampler is this computer's only. What a plugin names \
inside its own state travels as the plugin wrote it. A sampled instrument's recordings do not travel.
- `held` in status is files the song names on this computer that have not gone to the others, because nobody was \
shown them (a song file that was opened named them, or you did): tell the user which, and let them go \
(`let_files_go`) only when the user says so. `what_goes` says, before sharing, how many files would go, how large, \
and from which folders; tell the user before you share.
- Which input of the audio device a track records (track_input) is each person's own and is not shared: set it for \
this program only.
- Share and join when the user asks to, never on your own: both let someone else read and change the song. An \
invitation (`trill:HOST:PORT:SECRET`, or `trill:meet:HOST:PORT:SECRET` through a meeting point) is whoever holds \
it's key to the song while it is shared. With someone on another network, share with reach `meeting_point`: the \
user sets the meeting point and its key once, in the window (File, Share this song); no tool sets them, so where \
none is set say that, and do not ask the user for the key.
- A connection that is lost comes back by itself (status says `reconnecting`, and what each did meanwhile is \
merged when it is back): do not join again, and do not ask for the invitation again unless a sentence says to.
- Removing someone (action `remove` with `person`) is the user's decision: only when they ask, and only someone \
`here` lists with `can_be_removed`, who came in by this program's invitation. Someone who came in through another \
person is that person's to remove, and nobody removes whoever let them in: the refusal says which it is. The one \
removed keeps the song as it was. The invitation changes: `removed` in the reply says so and `invitation` is the \
new one. Tell the user that whoever stays has it already, and that anyone who has not joined yet needs it from them.";

#[derive(Copy, Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SharedSongAction {
    /// Who is here, what they hold, and the notices. Reads only.
    Status,
    /// Start listening for other people's Trill and get the invitation to hand them.
    Share,
    /// Stop listening: the invitation is no longer one. Whoever is connected stays.
    StopSharing,
    /// Join the song of whoever handed out `invitation`.
    Join,
    /// End every connection and stop sharing. The song stays as it is, and can be merged with the others' again.
    Leave,
    /// Let one person go from the song, by `person`: someone `here` lists with `can_be_removed`, who came in by
    /// this program's invitation. Their Trill is told and keeps the song as it was. The invitation is another
    /// from then on (the reply has it): the one they hold lets nobody in, whoever stays is told the new one by
    /// itself, and whoever has not joined yet needs it. The user's to decide.
    Remove,
    /// Do what the notice numbered `notice` offers: the edit that puts this program's own back.
    PutBack,
    /// Take the notice numbered `notice` as read.
    Dismiss,
    /// What sharing the open song would hand to whoever joins: how many audio files, how large, in which
    /// folders, and each file that lies outside the song's own folder, Trill's recordings and the sample
    /// library. Reads only: nothing is shared. Tell the user before sharing.
    WhatGoes,
    /// Let the files in `held` go to the others: the files the song names on this computer that nobody was
    /// shown. The user's to decide, having been told which they are.
    LetFilesGo,
    /// The user's word that however much of the song's audio may be on its way to this computer at once: the
    /// files in `arriving` that wait for it arrive.
    LetFilesArrive,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SharedSongRequest {
    pub action: SharedSongAction,
    /// For `share`: `this_machine` (the default: only programs on this computer can join), `network` (programs
    /// on the networks this computer is on can; the first time, the system's firewall asks the user whether that
    /// is allowed), or `meeting_point` (people anywhere can: both programs connect out to the meeting point the
    /// user has set in the window, and nothing listens on this computer. Where none is set the reply says so;
    /// setting one, and its key, is the user's to do there and is not a tool's).
    pub reach: Option<String>,
    /// For `share` on this computer or its networks: the port to listen on. Leave it out for whichever is free.
    pub port: Option<u16>,
    /// For `join`: the invitation as the other person's Trill shows it, `trill:HOST:PORT:SECRET` or
    /// `trill:meet:HOST:PORT:SECRET`.
    pub invitation: Option<String>,
    /// For `join`: true lets the song that is handed over take the place of an open song that has changes which
    /// are not saved. Without it such a join is refused, and nothing is lost. A song this program already shares
    /// with the other is never replaced: the two are merged.
    pub replace_unsaved: Option<bool>,
    /// For `share` and `join`: the name this program's person goes by among the others (by default the name they
    /// are signed in to the computer by).
    pub name: Option<String>,
    /// For `put_back` and `dismiss`: the notice's id.
    pub notice: Option<u64>,
    /// For `remove`: the name of the person to let go, as `here` in status has it.
    pub person: Option<String>,
}

/// How long `join` waits to say what came of it.
const JOINING: Duration = Duration::from_secs(12);
/// How long `share` waits for a meeting point to open the meeting.
const REACHING: Duration = Duration::from_secs(12);

impl TrillServer {
    /// Who is here and what this program has to tell of it.
    fn together_json(&self) -> Value {
        let status = self.runtime.together();
        let session = self.session.lock();
        let name = |id: &trill_core::TrackId| session.project().track(*id).map(|track| track.name.clone()).ok();
        let clip = |id: &trill_core::ClipId| session.project().clip(*id).map(|clip| clip.name.clone()).ok();
        let device = |id: &trill_core::DeviceId| {
            let device = session.project().device(*id).ok()?;
            Some(format!("{} ({})", device.name, device.uri))
        };
        let here: Vec<Value> = (status.people.iter())
            .map(|here| {
                let mut person = json!({
                    "name": here.person.name,
                    "colour": here.person.colour,
                    "at_beat": round(here.beat),
                    "playing": here.playing,
                });
                let tracks: Vec<String> = here.tracks.iter().filter_map(name).collect();
                let clips: Vec<String> = here.clips.iter().filter_map(clip).collect();
                if !tracks.is_empty() {
                    person["selected_tracks"] = json!(tracks);
                }
                if !clips.is_empty() {
                    person["selected_clips"] = json!(clips);
                }
                if !here.notes.is_empty() {
                    person["selected_notes"] = json!(here.notes.len());
                }
                if let Some(holding) = &here.holding {
                    person["holding"] = json!(holding);
                }
                // The plugins of the song their computer has not got: what
                // these play they hear as someone who has them recorded it.
                let lacks: Vec<String> = here.lacks.iter().filter_map(device).collect();
                if !lacks.is_empty() {
                    person["has_not_got"] = json!(lacks);
                }
                // In the song by this program's invitation: its to let go.
                if here.can_be_removed {
                    person["can_be_removed"] = json!(true);
                }
                person
            })
            .collect();
        let notices: Vec<Value> = (session.notices().iter())
            .map(|notice| {
                let mut said = json!({ "id": notice.id, "text": notice.text });
                if let Some(remedy) = &notice.remedy {
                    said["action"] = json!(remedy.label);
                }
                said
            })
            .collect();
        let mut out = json!({
            "shared": session.shared_song().is_some(),
            "me": { "name": status.me.name, "colour": status.me.colour },
            "connected_programs": status.connected,
            "here": here,
        });
        if let Some(invitation) = &status.invitation {
            out["invitation"] = json!(invitation);
        }
        if status.joining {
            out["joining"] = json!(true);
        }
        // Where the song is shared through a meeting point: which, and
        // whether whoever has the invitation gets in by it right now.
        if let Some((at, meeting)) = &status.meeting {
            out["meeting_point"] = match meeting {
                Meeting::Held => json!({ "at": at, "open": true }),
                Meeting::Reaching => json!({ "at": at, "open": false, "why": "it is being reached" }),
                Meeting::Away(why) => {
                    json!({ "at": at, "open": false, "why": format!("{why}; this program goes on trying") })
                }
            };
        }
        // Connections that were lost on the way and are asked for again.
        if !status.away.is_empty() {
            let away: Vec<Value> = (status.away.iter())
                .map(|away| {
                    json!({ "with": away.name, "for_seconds": away.since.elapsed().as_secs(), "tries": away.tries })
                })
                .collect();
            out["reconnecting"] = json!(away);
        }
        if !notices.is_empty() {
            out["notices"] = json!(notices);
        }
        // The song's audio: what is on its way here, and what has not
        // gone from here.
        if !status.files.is_empty() {
            let mb = |bytes: u64| round(bytes as f64 / f64::from(1 << 20));
            let arriving: Vec<Value> = (status.files.iter())
                .map(|file| {
                    // One that nobody has said: nothing is known of it but
                    // the place that plays it.
                    if file.waits == Some(Waiting::ToBeSaid) {
                        return json!({
                            "played_by": file.name,
                            "waits": "for whoever brought it into the song: their Trill has not said what the file \
                                      is (it is still reading it, has not got it either, does not read it as audio, \
                                      or holds it until its user lets it go). It is not missing on this computer: \
                                      look for nothing here.",
                        });
                    }
                    let mut said = json!({ "name": file.name, "mb": mb(file.bytes), "here_mb": mb(file.here) });
                    if let Some(from) = &file.from {
                        said["from"] = json!(from);
                    }
                    if let Some(waits) = &file.waits {
                        said["waits"] = json!(match waits {
                            Waiting::ForAWord => {
                                "for the user's word: more is on its way than arrives unasked (let_files_arrive)"
                                    .to_owned()
                            }
                            Waiting::ForRoom { short } => {
                                format!("for room on the disk: {} MB are missing", mb(*short).max(1.0))
                            }
                            Waiting::ForTheWay { seconds } => format!(
                                "the meeting point has nearly passed its day's allowance; goes on in about {} min",
                                seconds.div_ceil(60)
                            ),
                            Waiting::ForSomeone => "nobody who is in the song has said they have it".to_owned(),
                            // (Said above.)
                            Waiting::ToBeSaid => "for whoever brought it into the song".to_owned(),
                        });
                    }
                    said
                })
                .collect();
            out["arriving"] = json!(arriving);
        }
        if !status.held.is_empty() {
            out["held"] = json!({
                "files": status.held,
                "why": "the song names these on this computer and nobody was shown them: they have not gone to \
                        the others. let_files_go lets them, once the user has said so.",
            });
        }
        if let Some(seconds) = status.files_wait {
            out["audio_files_wait_minutes"] = json!(seconds.div_ceil(60));
        }
        // Tracks with devices that are not on every computer: what stands
        // in for those where they are not.
        let recorded: Vec<Value> = (session.renditions().iter())
            .map(|found| {
                let mut said = json!({
                    "track": name(&found.track).unwrap_or_default(),
                    "in_place_of": found.devices.iter().filter_map(device).collect::<Vec<_>>(),
                    "recorded_by": found.by,
                    "played_here": found.played,
                    "of_the_song_as_it_is": found.as_the_song_is(),
                });
                if !found.lacking.is_empty() {
                    said["not_on_this_computer"] = json!(found.lacking.iter().filter_map(device).collect::<Vec<_>>());
                }
                if found.played && !found.as_the_song_is() {
                    said["note"] = json!(
                        "what is heard here is this track as it was before something it depends on was changed \
                         (its notes, those devices, the tempo); a program that has the devices records it again \
                         by itself once the change has stood for a moment. The fader, the pan, the sends and the \
                         devices after those are heard at once."
                    );
                }
                said
            })
            .collect();
        if !recorded.is_empty() {
            out["heard_as_recorded"] = json!(recorded);
        }
        if let Some(track) = &status.recording_for_others {
            out["recording_for_the_others"] = json!(track);
        }
        let said: Vec<String> =
            self.runtime.said_together(0).into_iter().rev().take(5).rev().map(|(_, said)| said).collect();
        if !said.is_empty() {
            out["lately"] = json!(said);
        }
        out
    }
}

impl TrillServer {
    /// What the open song hands to whoever joins it: how many audio files
    /// it names on this computer, how large, and where they lie.
    fn what_goes(&self) -> Value {
        let going = self.runtime.files_to_go();
        let mut out = json!({
            "files": going.files,
            "mb": round(going.bytes as f64 / f64::from(1 << 20)),
            "says": going.sentence(),
        });
        if !going.folders.is_empty() {
            let folders: Vec<Value> = (going.folders.iter())
                .map(|(folder, files, bytes)| {
                    json!({ "in": folder, "files": files, "mb": round(*bytes as f64 / f64::from(1 << 20)) })
                })
                .collect();
            out["folders"] = json!(folders);
        }
        if !going.elsewhere.is_empty() {
            out["outside_the_songs_folders"] = json!(going.elsewhere);
        }
        // What the song names that Trill does not read as audio: it goes
        // to nobody, and is said because a song file may name anything.
        if !going.no_audio.is_empty() {
            out["no_audio_and_stays"] = json!(going.no_audio);
        }
        if going.gone > 0 {
            out["not_there"] = json!(going.gone);
        }
        out
    }

    /// Shares through the meeting point the user has set, and says how
    /// things stand once the meeting is open there, or why it is not.
    fn share_through_meeting_point(&self) -> Result<Value, String> {
        if self.runtime.meeting_point().is_none() {
            return Err("No meeting point is set. Its address and the key its keeper gave are the user's to set, \
                        once, in the window (File, Share this song, Through a meeting point), or in the settings \
                        file under together.meeting_point: no tool takes them. Say so; do not ask for the key."
                .to_owned());
        }
        let heard = self.runtime.said_together(0).last().map_or(0, |(number, _)| *number);
        self.runtime.share_through_meeting_point()?;
        // The invitation is there at once; whether it lets anyone in is
        // the meeting point's to say, a moment later.
        let until = Instant::now() + REACHING;
        loop {
            let refusal =
                self.runtime.said_together(heard).into_iter().find(|(_, said)| said.starts_with("Not shared"));
            if let Some((_, why)) = refusal {
                return Err(why);
            }
            let reaching = matches!(self.runtime.together().meeting, Some((_, Meeting::Reaching)));
            if !reaching || Instant::now() > until {
                return Ok(self.together_json());
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

#[tool_router(router = together_router, vis = "pub(crate)")]
impl TrillServer {
    /// Work in the open song with other people, each in a Trill of their own, all changing it at once. `share`
    /// starts listening and returns an `invitation` to hand to someone; `join` takes one and brings the two
    /// programs to the same song (merged, if both already had it; otherwise the other's song takes the place of
    /// the open one); `leave` ends it, and the song stays as it is and can be merged again later. `status` says
    /// who is `here` (their name, what they have selected, what they have hold of, where their song stands) and
    /// the `notices`: where this program's work and someone else's clashed, each a sentence, with an `action`
    /// that `put_back` performs as one undoable edit. Sharing opens the song to whoever holds the invitation, so
    /// do it when the user asks. For someone on another network share with reach `meeting_point`. A connection
    /// that is lost on the way comes back by itself (`reconnecting`). `remove` lets one person go who came in by
    /// this program's invitation, and makes the invitation another. The song's audio files go to whoever joins:
    /// `what_goes` says which before sharing, and the reply to `share` says it again as `audio_files`. `arriving`
    /// is the audio on its way to this computer; `held` is what has not gone from it. Every reply is the status
    /// after the action.
    #[tool(annotations(title = "Share the song, join one, see who is here"))]
    fn shared_song(&self, Parameters(request): Parameters<SharedSongRequest>) -> CallToolResult {
        use SharedSongAction as Action;
        let opens = matches!(
            request.action,
            Action::Share | Action::Join | Action::Leave | Action::StopSharing | Action::Remove
        );
        if opens && self.is_guarded() {
            return finish(Err("Sharing a song, joining one, leaving one and removing someone from one let someone \
                               else read and change the user's song, or end that: it is the user's to do, in the \
                               window (File, Share this song or Join a song). Say so. Who is here and the notices \
                               can be read with action `status`."
                .to_owned()));
        }
        if matches!(request.action, Action::LetFilesGo | Action::LetFilesArrive) && self.is_guarded() {
            return finish(Err("Which of the user's files go to other people, and how much arrives on this \
                               computer, is the user's to say, in the window (File, Share this song). Say which \
                               files are held or wait (action `status`), and that the answer is theirs to give there."
                .to_owned()));
        }
        if let Some(name) = &request.name {
            self.runtime.set_name_together(name);
        }
        let reply = (|| match request.action {
            Action::Status => Ok(self.together_json()),
            Action::WhatGoes => Ok(self.what_goes()),
            Action::LetFilesGo => {
                self.runtime.let_files_go();
                std::thread::sleep(Duration::from_millis(300));
                Ok(self.together_json())
            }
            Action::LetFilesArrive => {
                self.runtime.let_files_arrive();
                std::thread::sleep(Duration::from_millis(300));
                Ok(self.together_json())
            }
            Action::Share => {
                // What goes with it, as it stands before it is shared.
                let goes = self.what_goes();
                let with_what_goes = |mut reply: Value| {
                    reply["audio_files"] = goes.clone();
                    reply
                };
                let reach = match request.reach.as_deref() {
                    None | Some("this_machine") => Reach::ThisMachine,
                    Some("network") => Reach::Network,
                    Some("meeting_point") => return self.share_through_meeting_point().map(with_what_goes),
                    Some(other) => {
                        return Err(format!("reach is `this_machine`, `network` or `meeting_point`, not `{other}`"));
                    }
                };
                self.runtime.share(reach, request.port.unwrap_or(0))?;
                Ok(with_what_goes(self.together_json()))
            }
            Action::StopSharing => {
                self.runtime.stop_sharing();
                std::thread::sleep(Duration::from_millis(120));
                Ok(self.together_json())
            }
            Action::Join => {
                let Some(invitation) = &request.invitation else {
                    return Err("join takes the `invitation` the other person's Trill shows: \
                                trill:HOST:PORT:SECRET"
                        .to_owned());
                };
                let heard = self.runtime.said_together(0).last().map_or(0, |(number, _)| *number);
                self.runtime.join(invitation, request.replace_unsaved.unwrap_or(false))?;
                // What came of it: the song in hand with the other in it,
                // or the sentence that says why not.
                let until = Instant::now() + JOINING;
                loop {
                    let said = self.runtime.said_together(heard);
                    let refusal = (said.iter())
                        .find(|(_, said)| said.starts_with("Not joined") || said.starts_with("No connection"));
                    if let Some((_, why)) = refusal {
                        return Err(why.clone());
                    }
                    let joined = said.iter().any(|(_, said)| said.starts_with("You are in the song with"));
                    if joined && !self.runtime.together().people.is_empty() {
                        return Ok(self.together_json());
                    }
                    if Instant::now() > until {
                        return Err("the other program did not answer in time: it may not be sharing its song \
                                    any more. shared_song status says how things stand."
                            .to_owned());
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
            }
            Action::Leave => {
                self.runtime.leave();
                let until = Instant::now() + Duration::from_secs(3);
                while self.runtime.together().is_on() && Instant::now() < until {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Ok(self.together_json())
            }
            Action::Remove => {
                let here = self.runtime.together().people;
                let names = || {
                    let named: Vec<&str> = here.iter().map(|here| here.person.name.as_str()).collect();
                    match named.is_empty() {
                        true => "nobody else is in the song".to_owned(),
                        false => format!("in the song are {}", trill_core::journal::words::list(&named)),
                    }
                };
                let Some(asked) = request.person.as_deref().map(str::trim).filter(|name| !name.is_empty()) else {
                    return Err(format!("remove takes `person`, a name as `here` has it: {}", names()));
                };
                let same = |name: &str| name.trim().eq_ignore_ascii_case(asked);
                let meant: Vec<u64> =
                    here.iter().filter(|here| same(&here.person.name)).map(|here| here.person.id).collect();
                let person = match meant.as_slice() {
                    [] => return Err(format!("nobody called '{asked}' is in the song: {}", names())),
                    [person] => *person,
                    several => {
                        return Err(format!(
                            "{} people in the song go by '{asked}', and a name cannot tell them apart: the user \
                             removes the one they mean in the window (File, Share this song)",
                            several.len()
                        ));
                    }
                };
                // The sentence that says what was done, with how things
                // stand after it: the new invitation among it.
                let done = self.runtime.remove_from_song(person)?;
                let mut reply = self.together_json();
                reply["removed"] = json!(done);
                Ok(reply)
            }
            Action::PutBack | Action::Dismiss => {
                let mut session = self.session.lock();
                let known: Vec<String> = session.notices().iter().map(|notice| notice.id.to_string()).collect();
                let missing = || match known.is_empty() {
                    true => "there are no notices".to_owned(),
                    false => format!("the notices there are have the ids {}", known.join(", ")),
                };
                let Some(id) = request.notice else {
                    return Err(format!("say which `notice`: {}", missing()));
                };
                let Some(notice) = session.notices().iter().find(|notice| notice.id == id).cloned() else {
                    return Err(format!("there is no notice {id}: {}", missing()));
                };
                match request.action {
                    Action::PutBack if notice.remedy.is_none() => {
                        return Err(format!("notice {id} offers nothing to put back; `dismiss` takes it as read"));
                    }
                    Action::PutBack => session.remedy(id).map_err(|error| error.to_string())?,
                    _ => session.dismiss_notice(id),
                }
                drop(session);
                Ok(self.together_json())
            }
        })();
        finish(reply)
    }
}
