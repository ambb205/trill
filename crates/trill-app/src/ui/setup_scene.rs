//! Made-up machines, for pictures of the Setup panel.
//!
//! A screenshot run keeps nothing on disk and fetches nothing, so its
//! panel would always say the same thing: that there is nothing to show.
//! A scene puts what the panel reads in the place of this machine: the
//! parts as they are really published (their names, sizes and licence
//! lines are the real ones), in states a machine could be in. Nothing is
//! fetched and nothing is timed; a button pressed in a scene says that
//! this run keeps nothing.

use super::setup_instruments::Instruments;
use super::setup_program::Program;
use std::collections::HashMap;
use std::path::PathBuf;
use trill_catalogue::{Catalogue, Kind};
use trill_fetch::{ListStatus, PartState, PartStatus};
use trill_runtime::Ask;
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
    /// The catalogue of instruments as it is first met: nothing fetched,
    /// the pianos opened, one set read.
    Instruments,
    /// The starter selection on its way: sets that are here, one being
    /// fetched, the rest waiting, and one that could not be fetched.
    InstrumentsFetching,
    /// The catalogue narrowed by a search, on a set with instruments that
    /// are not put forward.
    InstrumentsSearch,
    /// A newer Trill is out: the tab of Trill itself, and the line in the
    /// status bar.
    Update,
    /// The newer Trill is fetched and waits for the next start.
    UpdateReady,
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
    /// What the tab of the instruments shows, where the scene is of it.
    pub instruments: Option<Instruments>,
    /// The instruments of the catalogue that are in the library.
    pub here: HashMap<String, PathBuf>,
    /// Which Trill the made-up machine has, and what it knows of a newer one.
    pub program: Program,
    /// Whether the tab of Trill itself is the one shown.
    pub on_program: bool,
}

/// A release of Trill as its makers would announce it, made up: nothing
/// of it is published anywhere.
fn made_up_release() -> trill_update::Release {
    let version: trill_update::Version = trill_update::Version::new(0, 3, 0);
    trill_update::Release {
        version,
        date: "2026-12-04".to_owned(),
        notes: "Takes of audio: record an input, keep every take, and comp them by dragging.\n\
                A song that friends hold too, with its recordings arriving by themselves.\n\
                Trill's own wavetable synth, with a bank of sounds chosen by what they are for."
            .to_owned(),
        page: format!("https://github.com/ambb205/trill/releases/tag/v{version}"),
        builds: vec![trill_update::Build {
            // For whatever machine the picture is made on.
            runs_on: trill_update::release::runs_on(),
            url: format!("https://github.com/ambb205/trill/releases/download/v{version}/trill-{version}.zip"),
            sha256: "0".repeat(64),
            size: 41 << 20,
            files: vec![trill_update::Piece { path: "trill.exe".to_owned(), sha256: "0".repeat(64), size: 96 << 20 }],
        }],
    }
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
    pub const ALL: [(&'static str, SetupScene); 8] = [
        ("fresh", SetupScene::Fresh),
        ("fetching", SetupScene::Fetching),
        ("machine", SetupScene::Machine),
        ("instruments", SetupScene::Instruments),
        ("instruments-fetching", SetupScene::InstrumentsFetching),
        ("instruments-search", SetupScene::InstrumentsSearch),
        ("update", SetupScene::Update),
        ("update-ready", SetupScene::UpdateReady),
    ];

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
                (SetupScene::Fresh | SetupScene::Instruments, _) => (PartState::Missing, 0.0, None),
                (SetupScene::InstrumentsFetching | SetupScene::InstrumentsSearch, _) => (PartState::Ready, 1.0, None),
                (SetupScene::Machine, "stems-best") => (PartState::Missing, 0.0, None),
                (SetupScene::Machine | SetupScene::Update | SetupScene::UpdateReady, _) => {
                    (PartState::Ready, 1.0, None)
                }
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
                    part: part.clone().into(),
                    state,
                    got,
                    on_disk: got,
                    error: error.map(str::to_owned),
                }
            })
            .collect();
        let mut lists = vec![ListStatus {
            id: published.id,
            title: published.title,
            about: published.about,
            folder: published.folder,
            parts,
        }];
        // The catalogue of instruments, as it is really published: every set with its real words.
        let catalogue = Catalogue::shipped();
        let sets = trill_runtime::fetch_list(catalogue, folder.join(trill_runtime::SETS_FOLDER), None);
        let starter: Vec<&str> = Ask::Starter.sets(catalogue).into_iter().map(|set| set.id.as_str()).collect();
        let place = |id: &str| starter.iter().position(|known| *known == id);
        let set_state = |id: &str| -> (PartState, f64, Option<&'static str>) {
            match (self, place(id), id) {
                (SetupScene::InstrumentsFetching, Some(0..=4), _) => (PartState::Ready, 1.0, None),
                (SetupScene::InstrumentsFetching, Some(5), _) => (PartState::Fetching, 0.37, None),
                (SetupScene::InstrumentsFetching, Some(_), _) => (PartState::Waiting, 0.0, None),
                (SetupScene::InstrumentsFetching, None, "ixox-flute") => (PartState::Ready, 1.0, None),
                (SetupScene::InstrumentsFetching, None, "jsteeldrum") => (
                    PartState::Missing,
                    0.0,
                    Some(
                        "c4c4d2a1b3f5.zip could not be fetched from github.com: Failed to connect to github.com port 443 after 21044 ms: Could not connect to server. Its source makes it anew each time it is asked for, so fetching it again begins again",
                    ),
                ),
                (SetupScene::InstrumentsSearch | SetupScene::Machine, Some(_), _) => (PartState::Ready, 1.0, None),
                _ => (PartState::Missing, 0.0, None),
            }
        };
        let mut here = HashMap::new();
        let set_parts = sets
            .parts
            .iter()
            .map(|part| {
                let (state, share, error) = set_state(&part.id);
                let got = (part.size() as f64 * share) as u64;
                let on_disk = if state == PartState::Ready { part.room() } else { got };
                if state == PartState::Ready
                    && let Some(set) = catalogue.set(&part.id)
                {
                    for instrument in &set.instruments {
                        let file = instrument.file.replace('/', std::path::MAIN_SEPARATOR_STR);
                        here.insert(instrument.text.clone(), sets.folder.join(&part.id).join(file));
                    }
                }
                PartStatus {
                    folder: sets.folder.join(&part.id),
                    part: part.clone().into(),
                    state,
                    got,
                    on_disk,
                    error: error.map(str::to_owned),
                }
            })
            .collect();
        lists.push(ListStatus {
            id: sets.id,
            title: sets.title,
            about: sets.about,
            folder: sets.folder,
            parts: set_parts,
        });
        let chosen = |set: &str, kinds: &[Kind], search: &str, asked: bool| Instruments {
            search: search.to_owned(),
            open_kinds: kinds.iter().copied().collect(),
            open_sets: [set.to_owned()].into_iter().collect(),
            selected: Some(set.to_owned()),
            asked: asked.then(|| ("Starter".to_owned(), starter.iter().map(|id| (*id).to_owned()).collect())),
        };
        let instruments = match self {
            SetupScene::Instruments => Some(chosen("salamander-grand-piano", &[Kind::Piano], "", false)),
            SetupScene::InstrumentsFetching => {
                Some(chosen(starter.get(5).copied().unwrap_or("swagbass"), &[Kind::Strings], "", true))
            }
            SetupScene::InstrumentsSearch => Some(chosen("greg-sullivan-e-pianos", &[], "electric piano", false)),
            _ => None,
        };
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
        // The Trill of the made-up machine: a release, which knows of a newer one where the scene is of that.
        let on_program = matches!(self, SetupScene::Update | SetupScene::UpdateReady);
        let newer = on_program.then(made_up_release);
        let made_up = Program {
            status: Some(trill_update::Status {
                this: trill_update::This {
                    version: trill_update::Version::new(0, 2, 0),
                    commit: Some("0ce65fb".to_owned()),
                },
                runs_on: trill_update::release::runs_on(),
                off: None,
                looking: false,
                looked: None,
                step: match (self, &newer) {
                    (SetupScene::UpdateReady, Some(release)) => trill_update::Step::Ready(release.version),
                    _ => trill_update::Step::Idle,
                },
                newer,
                problem: None,
            }),
            look_at_start: true,
            quiet_about: None,
        };
        let runtime_offered = lists[0].parts.iter().any(|status| status.part.id == "runtime");
        Staged {
            program: made_up,
            on_program,
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
            // The runtime is offered for Windows alone so far; elsewhere the first part.
            selected: match self {
                SetupScene::Fetching => "stems",
                _ if runtime_offered => "runtime",
                _ => "stems",
            },
            instruments,
            here,
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
            // The catalogue of instruments is there in every scene, as it is in the program.
            let sets = &staged.lists[1];
            assert_eq!((sets.id.as_str(), sets.parts.len()), ("instruments", Catalogue::shipped().sets.len()));
            let sets_here = sets.parts.iter().filter(|status| status.state == PartState::Ready).count();
            match scene {
                SetupScene::Instruments => {
                    let view = staged.instruments.as_ref().expect("the scene is of the instruments");
                    assert!(sets_here == 0 && staged.here.is_empty() && view.asked.is_none());
                    assert!(sets.parts.iter().any(|status| Some(&status.part.id) == view.selected.as_ref()));
                }
                SetupScene::InstrumentsFetching => {
                    let view = staged.instruments.as_ref().expect("the scene is of the instruments");
                    let asked = &view.asked.as_ref().expect("the starter selection was asked for").1;
                    let going = |state: PartState| sets.parts.iter().filter(|status| status.state == state).count();
                    assert_eq!(
                        (going(PartState::Fetching), sets_here),
                        (1, 6),
                        "one on its way, five and one more here"
                    );
                    assert_eq!(going(PartState::Waiting), asked.len() - 6);
                    assert!(sets.parts.iter().any(|status| status.error.is_some()) && !staged.here.is_empty());
                }
                SetupScene::InstrumentsSearch => {
                    let view = staged.instruments.as_ref().expect("the scene is of the instruments");
                    assert!(!view.search.is_empty() && sets_here > 0);
                }
                SetupScene::Update | SetupScene::UpdateReady => {
                    let notice = staged.program.notice().expect("a newer Trill is there to be said");
                    assert!(staged.on_program && notice.ready == (scene == SetupScene::UpdateReady));
                    // What the picture shows is an announcement a program would take.
                    let status = staged.program.status.as_ref().unwrap();
                    status.newer.as_ref().unwrap().check().unwrap();
                    assert!(status.build().is_some());
                }
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
