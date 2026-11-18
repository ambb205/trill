//! Made-up machines, for pictures of the Setup panel.
//!
//! A screenshot run keeps nothing on disk and fetches nothing, so its
//! panel would always say the same thing: that there is nothing to show.
//! A scene puts what the panel reads in the place of this machine: the
//! parts as they are really published (their names, sizes and licence
//! lines are the real ones), in states a machine could be in. Nothing is
//! fetched and nothing is timed; a button pressed in a scene says that
//! this run keeps nothing.

use std::path::PathBuf;
use trill_fetch::{ListStatus, PartState, PartStatus};
use trill_study::helper::Checked;
use trill_study::models::real::Provider;
use trill_study::setup::{Found, Machine, Timed, list};

/// What the made-up machine has.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum SetupScene {
    /// Trill as it is first started: nothing fetched, nothing timed.
    Fresh,
    /// In the middle of being set up: parts that are here, one on its
    /// way, one waiting, and one that could not be fetched.
    Fetching,
    /// Everything here and the models timed: the tab of the machine.
    Machine,
}

/// What a scene shows.
pub struct Staged {
    pub lists: Vec<ListStatus>,
    pub machine: Option<Machine>,
    pub untested: bool,
    pub runs_on: String,
    pub programs: Vec<Found>,
    pub studies: (PathBuf, usize, u64),
    /// Whether the machine's tab is the one shown.
    pub on_machine: bool,
    /// The part whose details are shown.
    pub selected: &'static str,
}

fn timed(provider: Provider, device: &str, seconds: f64, ready: f64, megabytes: u64) -> Timed {
    Timed {
        provider,
        device: device.to_owned(),
        checked: Some(Checked {
            device: device.to_owned(),
            ready_seconds: ready,
            seconds_a_minute: seconds,
            sum: 543_772.9,
        }),
        error: None,
        peak: megabytes << 20,
    }
}

impl SetupScene {
    pub const ALL: [(&'static str, SetupScene); 3] =
        [("fresh", SetupScene::Fresh), ("fetching", SetupScene::Fetching), ("machine", SetupScene::Machine)];

    pub fn named(name: &str) -> Option<Self> {
        Self::ALL.iter().find(|(known, _)| *known == name).map(|(_, scene)| *scene)
    }

    /// The machine the scene shows.
    pub fn staged(self) -> Staged {
        let folder = PathBuf::from(if cfg!(windows) {
            "C:\\Users\\you\\AppData\\Local\\trill"
        } else {
            "/home/you/.local/share/trill"
        });
        let published = list(folder.join("models"));
        // What each part is doing, by its id: here, or so many hundredths of it fetched.
        let state_of = |id: &str| -> (PartState, f64, Option<&'static str>) {
            match (self, id) {
                (SetupScene::Fresh, _) => (PartState::Missing, 0.0, None),
                (SetupScene::Machine, "stems-best") => (PartState::Missing, 0.0, None),
                (SetupScene::Machine, _) => (PartState::Ready, 1.0, None),
                (SetupScene::Fetching, "runtime" | "any-card" | "beats") => (PartState::Ready, 1.0, None),
                (SetupScene::Fetching, "stems") => (PartState::Fetching, 0.43, None),
                (SetupScene::Fetching, "words") => (PartState::Waiting, 0.0, None),
                (SetupScene::Fetching, "word-times") => (
                    PartState::Missing,
                    0.31,
                    Some(
                        "model.onnx could not be fetched from huggingface.co: Failed to connect to huggingface.co port 443 after 21044 ms: Could not connect to server. What came of it is kept, and fetching it again goes on from there",
                    ),
                ),
                (SetupScene::Fetching, _) => (PartState::Missing, 0.0, None),
            }
        };
        let parts = published
            .parts
            .iter()
            .map(|part| {
                let (state, share, error) = state_of(&part.id);
                let got = (part.size() as f64 * share) as u64;
                PartStatus {
                    folder: published.folder.join(&part.id),
                    part: part.clone(),
                    state,
                    got,
                    on_disk: got,
                    error: error.map(str::to_owned),
                }
            })
            .collect();
        let lists = vec![ListStatus {
            id: published.id,
            title: published.title,
            about: published.about,
            folder: published.folder,
            parts,
        }];
        let nvidia = Provider::Nvidia;
        let machine = (self == SetupScene::Machine).then(|| Machine {
            tested: 0,
            of: Vec::new(),
            results: vec![
                timed(Provider::Processor, "the processor", 19.2, 3.8, 2450),
                timed(Provider::AnyCard { adapter: 0 }, "a graphics card (NVIDIA)", 7.3, 6.4, 3170),
                timed(
                    Provider::AnyCard { adapter: 1 },
                    "a graphics card (Advanced Micro Devices, Inc.)",
                    69.1,
                    18.2,
                    3690,
                ),
                timed(nvidia, "a graphics card (NVIDIA) through NVIDIA's libraries", 3.0, 1.8, 3890),
            ],
            chosen: nvidia,
            by_user: false,
        });
        let program = |name: &'static str, what: &'static str, path: Option<&str>, get: &'static str| Found {
            name,
            what,
            path: path.map(PathBuf::from),
            get,
        };
        Staged {
            lists,
            untested: machine.is_none(),
            runs_on: match &machine {
                Some(_) => "an NVIDIA card through NVIDIA's libraries".to_owned(),
                None => "the processor".to_owned(),
            },
            machine,
            programs: vec![
                program(
                    "ffmpeg",
                    "Reads a recording in a format Trill does not read itself, and makes a film of a song.",
                    Some("C:\\Program Files\\ffmpeg\\bin\\ffmpeg.exe"),
                    "winget install Gyan.FFmpeg, or ffmpeg.org",
                ),
                program(
                    "yt-dlp",
                    "Fetches the recording behind a link, for a study of it. Needs ffmpeg.",
                    None,
                    "winget install yt-dlp.yt-dlp, or github.com/yt-dlp/yt-dlp",
                ),
                program(
                    "curl",
                    "Fetches the parts listed here. It comes with the system.",
                    Some("C:\\Windows\\System32\\curl.exe"),
                    "curl.se",
                ),
            ],
            studies: match self {
                SetupScene::Machine => (folder.join("studies"), 3, 912 << 20),
                _ => (folder.join("studies"), 0, 0),
            },
            on_machine: self == SetupScene::Machine,
            selected: match self {
                SetupScene::Fetching => "stems",
                _ => "runtime",
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_scene_shows_the_parts_as_they_are_published_in_the_states_it_says() {
        for (name, scene) in SetupScene::ALL {
            assert_eq!(SetupScene::named(name), Some(scene));
            let staged = scene.staged();
            let parts = &staged.lists[0].parts;
            // The real parts, with their real words: a picture shows what a user will read.
            let stems = parts.iter().find(|status| status.part.id == "stems").expect("the stems are published");
            assert!(stems.part.licence.starts_with("Unclear."), "{}", stems.part.licence);
            assert!(parts.iter().any(|status| status.part.id == staged.selected));
            let here = parts.iter().filter(|status| status.state == PartState::Ready).count();
            match scene {
                SetupScene::Fresh => assert!(here == 0 && staged.untested && staged.machine.is_none()),
                SetupScene::Fetching => {
                    assert_eq!(stems.state, PartState::Fetching);
                    assert!(stems.got > 0 && stems.got < stems.part.size());
                    assert!(parts.iter().any(|status| status.error.is_some()) && here >= 1);
                }
                SetupScene::Machine => {
                    let machine = staged.machine.as_ref().expect("it was timed");
                    assert!(staged.on_machine && !staged.untested && here + 1 == parts.len());
                    assert_eq!(trill_study::setup::choose(&machine.results), machine.chosen);
                }
            }
        }
        assert_eq!(SetupScene::named("nowhere"), None);
    }
}
