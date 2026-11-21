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

use crate::server::{TrillServer, finish};
use crate::view::round;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::schemars::{self, JsonSchema};
use rmcp::{tool, tool_router};
use serde::Deserialize;
use serde_json::{Value, json};
use std::time::{Duration, Instant};
use trill_runtime::Reach;

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
- A take someone else recorded is in the song with its lane and what the track plays of it, and names a file on \
their computer: here it is a missing file (missing_files) until the file is here too. Which input of the audio \
device a track records (track_input) is each person's own and is not shared: set it for this program only.
- Share and join when the user asks to, never on your own: both let someone else read and change the song. An \
invitation (`trill:HOST:PORT:SECRET`) is whoever holds it's key to the song while it is shared.";

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
    /// Do what the notice numbered `notice` offers: the edit that puts this program's own back.
    PutBack,
    /// Take the notice numbered `notice` as read.
    Dismiss,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SharedSongRequest {
    pub action: SharedSongAction,
    /// For `share`: `this_machine` (the default: only programs on this computer can join) or `network` (programs
    /// on the networks this computer is on can; the first time, the system's firewall asks the user whether that
    /// is allowed).
    pub reach: Option<String>,
    /// For `share`: the port to listen on. Leave it out for whichever is free.
    pub port: Option<u16>,
    /// For `join`: the invitation as the other person's Trill shows it, `trill:HOST:PORT:SECRET`.
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
}

/// How long `join` waits to say what came of it.
const JOINING: Duration = Duration::from_secs(12);

impl TrillServer {
    /// Who is here and what this program has to tell of it.
    fn together_json(&self) -> Value {
        let status = self.runtime.together();
        let session = self.session.lock();
        let name = |id: &trill_core::TrackId| session.project().track(*id).map(|track| track.name.clone()).ok();
        let clip = |id: &trill_core::ClipId| session.project().clip(*id).map(|clip| clip.name.clone()).ok();
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
        if !notices.is_empty() {
            out["notices"] = json!(notices);
        }
        let said: Vec<String> =
            self.runtime.said_together(0).into_iter().rev().take(5).rev().map(|(_, said)| said).collect();
        if !said.is_empty() {
            out["lately"] = json!(said);
        }
        out
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
    /// do it when the user asks. Every reply is the status after the action.
    #[tool(annotations(title = "Share the song, join one, see who is here"))]
    fn shared_song(&self, Parameters(request): Parameters<SharedSongRequest>) -> CallToolResult {
        use SharedSongAction as Action;
        let opens = matches!(request.action, Action::Share | Action::Join | Action::Leave | Action::StopSharing);
        if opens && self.is_guarded() {
            return finish(Err("Sharing a song, joining one and leaving one let someone else read and change the \
                               user's song, or end that: it is the user's to do, in the window (File, Share this \
                               song or Join a song). Say so. Who is here and the notices can be read with action \
                               `status`."
                .to_owned()));
        }
        if let Some(name) = &request.name {
            self.runtime.set_name_together(name);
        }
        let reply = (|| match request.action {
            Action::Status => Ok(self.together_json()),
            Action::Share => {
                let reach = match request.reach.as_deref() {
                    None | Some("this_machine") => Reach::ThisMachine,
                    Some("network") => Reach::Network,
                    Some(other) => return Err(format!("reach is `this_machine` or `network`, not `{other}`")),
                };
                self.runtime.share(reach, request.port.unwrap_or(0))?;
                Ok(self.together_json())
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
