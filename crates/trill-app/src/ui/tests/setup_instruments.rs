//! The Instruments tab of the Setup panel: the catalogue of sampled
//! instruments by kind, with a search, and fetching from it: one set, a
//! kind, the starter selection as one thing that is stopped and gone on
//! with.
//!
//! The catalogue here is one the test writes, of sets the test makes (an
//! SFZ file and two tones each) and puts on a shelf of its own: nothing is
//! asked of a network, nothing of anyone's is read, and nothing of the
//! user's is touched. The last test is the real thing, on the catalogue
//! that is built in: it runs only when asked.

use super::sounds::tone;
use super::*;
use std::path::Path;
use trill_catalogue::check::{address, file_sum, tree_sum};
use trill_catalogue::{Catalogue, Fetch, Kind, Set};
use trill_engine::AudioBackend;
use trill_fetch::scripted::Shelf;
use trill_fetch::{PartState, said};
use trill_library::{EntryKind, Query};
use trill_runtime::{Ask, INSTRUMENTS, Parts, SETS_FOLDER, Storage, Studying};
use trill_study::models::scripted::Script;
use trill_study::run::Programs;

const DOT: &str = "\u{00B7}";
const VERSION: &str = "0123456789abcdef0123456789abcdef01234567";

/// One instrument of a set the test makes: its file, its name, its marks
/// as the catalogue writes them, and what else its entry says.
struct Made {
    file: &'static str,
    name: &'static str,
    marks: &'static str,
    more: &'static str,
}

/// Writes a set as its maker lays it out into `folder` (a licence, two
/// tones and a file for each instrument), and answers with its entry and,
/// for one that is fetched file by file, its list.
#[allow(clippy::too_many_arguments)]
fn made_set(
    folder: &Path,
    id: &str,
    name: &str,
    kind: &str,
    licence: (&str, &str),
    credit: Option<&str>,
    by_file: bool,
    instruments: &[Made],
) -> (String, String) {
    tone(folder.join("Samples").join("low.wav"), 0.3);
    tone(folder.join("Samples").join("high.wav"), 0.2);
    std::fs::write(folder.join("LICENSE"), format!("{}\n", licence.1)).unwrap();
    for made in instruments {
        // Each a file of its own, so that each is known again by it.
        let sfz = format!(
            "// {}\n<control> default_path=Samples/\n<region> sample=low.wav lokey=36 hikey=59 pitch_keycenter=48\n\
             <region> sample=high.wav lokey=60 hikey=84 pitch_keycenter=72\n",
            made.name
        );
        std::fs::write(folder.join(made.file), sfz).unwrap();
    }
    let tree = tree_sum(folder).unwrap();
    let sum = |path: &str| file_sum(&path.split('/').fold(folder.to_owned(), |at, piece| at.join(piece))).unwrap();
    let size = |path: &str| {
        std::fs::metadata(path.split('/').fold(folder.to_owned(), |at, piece| at.join(piece))).unwrap().len()
    };
    let mut paths = vec!["LICENSE".to_owned(), "Samples/high.wav".to_owned(), "Samples/low.wav".to_owned()];
    paths.extend(instruments.iter().map(|made| made.file.to_owned()));
    let (fetch, list) = if by_file {
        let lines: String = paths.iter().map(|path| format!("{} {} {path}\n", sum(path), size(path))).collect();
        (
            format!("base = \"https://example.org/{id}/raw/{VERSION}/\"\nlist = \"{id}\""),
            format!("{id} = '''\n{lines}'''\n"),
        )
    } else {
        (
            format!(
                "archive = \"https://example.org/{id}/archive/{VERSION}.zip\"\nunder = \"{id}-{VERSION}/\"\nsum = \"{}\"",
                tree.sum
            ),
            String::new(),
        )
    };
    let mut entry = format!(
        "[[set]]\nid = \"{id}\"\nname = \"{name}\"\nabout = \"{name}, made up for a test.\"\nby = \"A. Maker\"\n\
         licence = \"{}\"\nlicence_at = \"https://example.org/{id}/LICENSE\"\nlicence_says = \"{}\"\n",
        licence.0, licence.1
    );
    if let Some(credit) = credit {
        entry.push_str(&format!("credit = \"{credit}\"\n"));
    }
    entry.push_str(&format!(
        "from = \"https://example.org/{id}\"\nversion = \"{VERSION}\"\nread = \"2026-11-17\"\nbytes = {}\nfiles = {}\n\
         [set.fetch]\n{fetch}\n",
        tree.bytes, tree.files
    ));
    for made in instruments {
        entry.push_str(&format!(
            "[[set.instrument]]\nfile = \"{}\"\ntext = \"{}\"\nname = \"{}\"\nkind = \"{kind}\"\n\
             words = \"{}, made up for a test\"\nrange = [36, 84]\nlayers = 1\ntakes = 1\nrecordings = 2\nbytes = {}\n{}\n",
            made.file,
            sum(made.file),
            made.name,
            made.name,
            size("Samples/high.wav") + size("Samples/low.wav"),
            if made.marks.is_empty() { made.more.to_owned() } else { format!("marks = [{}]\n{}", made.marks, made.more) },
        ));
    }
    (entry, list)
}

/// Puts a set on the shelf where its entry says it is published.
fn publish(shelf: &Shelf, set: &Set, made: &Path) {
    let read = |path: &str| std::fs::read(path.split('/').fold(made.to_owned(), |at, piece| at.join(piece))).unwrap();
    let mut paths = vec!["LICENSE".to_owned(), "Samples/high.wav".to_owned(), "Samples/low.wav".to_owned()];
    paths.extend(set.instruments.iter().map(|instrument| instrument.file.clone()));
    match &set.fetch {
        Fetch::Archive { url, under, .. } => {
            let members: Vec<(String, Vec<u8>)> =
                paths.iter().map(|path| (format!("{under}{path}"), read(path))).collect();
            let borrowed: Vec<(&str, &[u8], bool)> =
                members.iter().map(|(name, bytes)| (name.as_str(), bytes.as_slice(), true)).collect();
            shelf.put(url, trill_fetch::zip::archive_of(&borrowed));
        }
        Fetch::Files { base, files } => {
            for file in files {
                shelf.put(&address(base, &file.path), read(&file.path));
            }
        }
    }
}

struct Fixture {
    harness: Harness,
    shelf: Arc<Shelf>,
    catalogue: Catalogue,
    /// Holds the settings, the sets as they were made and as they are fetched.
    folder: tempfile::TempDir,
}

/// The window on a runtime whose catalogue is three sets on a shelf: a
/// grand that asks to be named (in the starter selection), strings that
/// are fetched file by file (one in the starter selection, one that is
/// not put forward) and a kit.
fn fixture() -> Fixture {
    let folder = tempfile::tempdir().unwrap();
    let made = folder.path().join("made");
    let attribution = ("CC-BY-4.0", "Attribution 4.0 International");
    let dedicated = ("CC0-1.0", "This collection is under a Creative Commons 0 license.");
    let plain = |file, name, marks| Made { file, name, marks, more: "" };
    let sets = [
        made_set(
            &made.join("small-grand"),
            "small-grand",
            "Small Grand",
            "piano",
            attribution,
            Some("Small Grand by A. Maker, licensed under CC BY 4.0 (https://creativecommons.org/licenses/by/4.0/)"),
            false,
            &[plain("Small Grand.sfz", "Small Grand", "\"starter\"")],
        ),
        made_set(
            &made.join("chamber-strings"),
            "chamber-strings",
            "Chamber Strings",
            "strings",
            dedicated,
            None,
            true,
            &[
                plain("Violins.sfz", "Violins, held", "\"starter\""),
                Made {
                    file: "Violas.sfz",
                    name: "Violas, scratchy",
                    marks: "\"out-of-tune\"",
                    more: "tuning = [31.0, 4.0, 40.0]\n",
                },
            ],
        ),
        made_set(
            &made.join("garage-kit"),
            "garage-kit",
            "Garage Kit",
            "drum-kit",
            dedicated,
            None,
            false,
            &[Made { file: "Kit.sfz", name: "Garage Kit", marks: "", more: "drums = true\n" }],
        ),
    ];
    let entries: String = sets.iter().map(|(entry, _)| entry.as_str()).collect();
    let lists: String = sets.iter().map(|(_, list)| list.as_str()).collect();
    let catalogue =
        Catalogue::read(&format!("format = 1\n{entries}"), &format!("format = 1\n[list]\n{lists}"), "format = 1\n")
            .unwrap();
    let shelf = Arc::new(Shelf::default());
    for set in &catalogue.sets {
        publish(&shelf, set, &made.join(&set.id));
    }
    let studying = Studying {
        fetcher: Some(shelf.clone()),
        parts: Some(Vec::new()),
        stand_in: Some(Script::default()),
        programs: Some(Programs::default()),
        program: None,
        free: None,
    };
    let storage = Storage::At { settings: folder.path().join("config").join("settings.json"), library_index: None };
    let options = RuntimeOptions { audio: AudioBackend::Null { sample_rate: 48_000 }, plugins: None, storage };
    let parts = Parts { studying, catalogue: Some(catalogue.clone()), ..Parts::default() };
    let runtime = Arc::new(Runtime::start_custom(options, parts).unwrap());
    let harness = Harness::on(runtime, UiOptions::default(), |_| {});
    Fixture { harness, shelf, catalogue, folder }
}

impl Harness {
    /// Opens the Setup panel on its tab of instruments.
    fn open_instruments(&mut self) {
        self.key(Modifiers::COMMAND | Modifiers::SHIFT, Key::Comma);
        self.settle_instruments();
        self.click_label("Instruments");
        self.settle_instruments();
    }

    fn settle_instruments(&mut self) {
        self.wait_until("the Setup panel to settle", |harness| !harness.app.setup().is_busy());
        self.idle(3);
        self.wait_until("the Setup panel to settle", |harness| !harness.app.setup().is_busy());
        self.idle(2);
    }

    fn wait_to_see(&mut self, label: &str) {
        let wanted = label.to_owned();
        self.wait_until(&format!("'{label}'"), move |harness| harness.is_on_screen(&wanted));
    }

    /// Everything on screen that says something, for a failure to show.
    fn seen(&self) -> Vec<&str> {
        self.labels.iter().map(|(text, _)| text.as_str()).collect()
    }

    /// Where the set `id` is, as the engine that fetches says.
    fn state_of(&self, id: &str) -> PartState {
        self.runtime.fetches().unwrap().status_of(id).unwrap().state
    }

    fn library_instruments(&self) -> Vec<String> {
        assert!(self.runtime.library.wait_until_idle(Duration::from_secs(30)));
        let found = self.runtime.library.search(&Query { kind: Some(EntryKind::Instrument), ..Query::default() });
        let mut names: Vec<String> = found.entries().map(|entry| entry.name.clone()).collect();
        names.sort();
        names
    }
}

#[test]
fn the_catalogue_is_shown_by_kind_with_whose_each_set_is_and_only_what_is_asked_for_is_fetched() {
    let Fixture { mut harness, shelf, catalogue, folder } = fixture();
    let grand = catalogue.set("small-grand").unwrap();
    let size = said(grand.fetched_bytes());
    harness.open_instruments();
    // The two asks with what they take, and the catalogue by kind: nothing is opened, nothing is here.
    let starter: u64 = Ask::Starter.sets(&catalogue).iter().map(|set| set.fetched_bytes()).sum();
    let all: u64 = catalogue.sets.iter().map(|set| set.fetched_bytes()).sum();
    assert!(harness.is_on_screen(&format!("Starter {DOT} {}", said(starter))));
    assert!(harness.is_on_screen(&format!("Everything {DOT} {}", said(all))));
    assert!(harness.is_on_screen(&format!("Pianos {DOT} 1 set {DOT} 1 instrument {DOT} {size}")));
    assert!(harness.is_on_screen(&format!("Strings {DOT} 1 set {DOT} 2 instruments")));
    assert!(harness.is_on_screen(&format!("Drum kits {DOT} 1 set {DOT} 1 instrument")));
    assert!(!harness.is_on_screen("Small Grand"), "a kind is opened to see its sets");
    let kept = folder.path().join("config").join(SETS_FOLDER);
    assert!(harness.is_on_screen(&format!("Kept in {}", kept.display())));

    // A kind opens to its sets, and a set to its instruments and to everything there is to read of it.
    harness.click_label("Pianos");
    harness.idle(2);
    let row = format!("Small Grand {DOT} {size} {DOT} CC BY 4.0 {DOT} Not here");
    assert!(harness.is_on_screen(&row));
    harness.click_label(&row);
    harness.idle(2);
    assert!(harness.is_on_screen(&format!("Small Grand {DOT} C2\u{2013}C6 {DOT} starter")));
    assert!(harness.is_on_screen("Small Grand, made up for a test."));
    assert!(harness.is_on_screen("By: A. Maker"));
    assert!(harness.is_on_screen(
        "Licence: CC BY 4.0 (attribution). \u{201C}Attribution 4.0 International\u{201D} Read at \
         https://example.org/small-grand/LICENSE on 2026-11-17."
    ));
    assert!(harness.is_on_screen(
        "Credit: Small Grand by A. Maker, licensed under CC BY 4.0 (https://creativecommons.org/licenses/by/4.0/). \
         Trill gives this line for a song that plays it (File, Credits)."
    ));
    assert!(harness.is_on_screen("From: https://example.org/small-grand (as it was at 0123456789ab)"));
    assert!(harness.is_on_screen("Checked by: One archive, made when it is asked for: checked by what it unpacks to"));
    assert!(harness.is_on_screen("Nothing is fetched until you press Fetch"), "{:?}", harness.seen());
    // Opening the panel and reading all of that fetched nothing.
    assert!(shelf.asked().is_empty() && harness.runtime.library.roots().is_empty());

    // The set is fetched by its own button, and nothing else is.
    harness.click_label(&format!("Fetch {DOT} {size}"));
    harness.wait_to_see(&format!("Small Grand {DOT} {size} {DOT} CC BY 4.0 {DOT} Here"));
    harness.settle_instruments();
    assert_eq!(shelf.asked().len(), 1);
    assert_eq!(harness.state_of("chamber-strings"), PartState::Missing);
    assert!(kept.join("small-grand").join("Samples").join("low.wav").is_file());
    // It is the library's, as one instrument, and the tab says it is here.
    assert_eq!(harness.library_instruments(), ["Small Grand"]);
    harness.settle_instruments();
    harness.wait_to_see(&format!("Small Grand {DOT} C2\u{2013}C6 {DOT} starter"));
    assert!(harness.is_on_screen(&format!("1 of 3 sets here {DOT} ")), "{:?}", harness.seen());
    assert!(harness.is_on_screen(&format!("Pianos {DOT} 1 set {DOT} 1 instrument {DOT} {size} {DOT} 1 here")));

    // And removed again, with a word on the room that made; the library no longer lists it.
    harness.click_label("Remove");
    harness.wait_to_see(&format!("Small Grand {DOT} {size} {DOT} CC BY 4.0 {DOT} Not here"));
    harness.settle_instruments();
    assert!(harness.app.setup().last().unwrap().starts_with("Small Grand was removed: "));
    assert!(harness.library_instruments().is_empty() && !kept.join("small-grand").exists());
}

#[test]
fn a_search_narrows_the_catalogue_and_what_is_not_put_forward_says_what_was_measured_against_it() {
    let Fixture { mut harness, shelf, catalogue, folder: _folder } = fixture();
    harness.open_instruments();
    // Typed into the field, a word narrows the catalogue to what has it, opened.
    harness.click_label("Search the catalogue");
    harness.type_text("viol");
    harness.idle(3);
    assert!(harness.is_on_screen(&format!("Strings {DOT} 1 set {DOT} 2 instruments")));
    assert!(!harness.is_on_screen("Pianos") && !harness.is_on_screen("Drum kits"));
    assert!(harness.is_on_screen(&format!("Violins, held {DOT} C2\u{2013}C6 {DOT} starter")));
    // One that something measured speaks against is listed, and says so; it is not in the starter selection.
    assert!(harness.is_on_screen(&format!("Violas, scratchy {DOT} C2\u{2013}C6 {DOT} not put forward")));
    assert!(harness.is_on_screen(
        "Not put forward. It is out of tune: its keys lie +31 cents from equal temperament, and usually 4 cents \
         from one another."
    ));
    assert!(harness.is_on_screen("Credit: None is asked: it is in the public domain."));
    assert!(harness.is_on_screen("Checked by: 5 files, each by its SHA-256"));
    let strings = catalogue.set("chamber-strings").unwrap();
    assert!(strings.is_starter() && !strings.instruments[1].is_put_forward());

    // Fetched by its own button, file by file: the one that is not put forward comes with its set.
    harness.click_label(&format!("Fetch {DOT} {}", said(strings.fetched_bytes())));
    harness.wait_to_see(&format!("Chamber Strings {DOT} {} {DOT} CC0 {DOT} Here", said(strings.fetched_bytes())));
    assert_eq!(shelf.asked().len(), 5);
    assert_eq!(harness.library_instruments(), ["Violas", "Violins"]);

    // A search that finds nothing says so.
    harness.settle_instruments();
    harness.click_label("viol");
    harness.type_text("oncello nobody made");
    harness.idle(3);
    assert!(harness.is_on_screen("Nothing in the catalogue is called that."));
}

#[test]
fn the_starter_selection_is_asked_for_as_one_thing_stopped_gone_on_with_and_played() {
    let Fixture { mut harness, shelf, catalogue, folder } = fixture();
    harness.open_instruments();
    let starter = Ask::Starter.sets(&catalogue);
    assert_eq!(starter.len(), 2, "the grand and the strings");
    let all: u64 = starter.iter().map(|set| set.fetched_bytes()).sum();

    // Asked for at once, slowly: the whole of it is one line with one Stop.
    shelf.slow(Duration::from_millis(30));
    harness.click_label(&format!("Starter {DOT} {}", said(all)));
    harness.wait_until("the starter selection to be on its way", |harness| {
        harness.is_on_screen("Starter: 0 of 2 sets here") || harness.is_on_screen("Starter: 1 of 2 sets here")
    });
    assert!(harness.app.setup().last().unwrap().starts_with("Starter: 2 sets asked for, "));
    assert_eq!(harness.state_of("garage-kit"), PartState::Missing, "the kit is not in the starter selection");
    harness.click_label("Stop fetching instruments");
    harness.wait_until("everything to stop", |harness| !harness.runtime.fetches().unwrap().fetching());
    harness.wait_until("the line of what is on its way to go", |harness| !harness.is_on_screen("Starter: "));
    harness.settle_instruments();
    assert!(harness.app.setup().last().unwrap().starts_with("Stopped. The sets that were whole are kept"));
    assert!(!harness.is_on_screen("Stop fetching instruments"), "nothing is on its way any more: {:?}", harness.seen());
    let here = starter.iter().filter(|set| harness.state_of(&set.id) == PartState::Ready).count();
    assert!(here < 2, "it was stopped before the end");

    // Asked for again, it is gone on with to the end: the button says what is left, and then that all is here.
    shelf.slow(Duration::ZERO);
    harness.click_label(&format!("Starter {DOT} "));
    harness.wait_to_see(&format!("Starter {DOT} here"));
    harness.settle_instruments();
    assert!(starter.iter().all(|set| harness.state_of(&set.id) == PartState::Ready));
    assert_eq!(harness.state_of("garage-kit"), PartState::Missing);
    assert_eq!(harness.library_instruments(), ["Small Grand", "Violas", "Violins"]);
    let kept = std::path::absolute(folder.path().join("config").join(SETS_FOLDER)).unwrap();
    assert_eq!(harness.runtime.library.roots(), std::slice::from_ref(&kept));

    // One of them on a track, played: what was fetched is an instrument.
    harness.key(Modifiers::NONE, Key::Escape);
    let file = kept.join("small-grand").join("Small Grand.sfz").display().to_string();
    {
        let mut session = harness.session.lock();
        let track = session.edit(|project| Ok(project.add_track("Keys"))).unwrap();
        session.add_device(ChainRef::Track(track), "builtin:sampled", Some(&file), None).unwrap();
        session
            .edit(|project| {
                let clip = project.add_midi_clip(track, "c", 0.0, 2.0)?;
                project.clip_mut(clip)?.add_notes(&[Note { pitch: 48, start: 0.0, length: 1.0, velocity: 100 }])
            })
            .unwrap();
        assert!(session.problems().is_empty(), "{:?}", session.problems());
    }
    let job = harness.session.lock().render_job();
    let rendered = job.render(&trill_engine::render::RenderOptions::default()).unwrap();
    assert!(rendered.left.iter().fold(0.0f32, |peak, sample| peak.max(sample.abs())) > 0.05, "it sounds");
    // And the song owes its maker the line.
    assert!(harness.runtime.owed().text().starts_with("Small Grand by A. Maker, licensed under CC BY 4.0"));

    // A kind is fetched alone from its own row.
    harness.key(Modifiers::COMMAND | Modifiers::SHIFT, Key::Comma);
    harness.settle_instruments();
    harness.click_label("Fetch Drum kits");
    harness.wait_until("the kit to be here", |harness| harness.state_of("garage-kit") == PartState::Ready);
    harness.wait_to_see(&format!("Everything {DOT} here"));
    assert!(harness.is_on_screen(&format!("3 of 3 sets here {DOT} ")));
    assert_eq!(harness.runtime.fetches().unwrap().list_ids(), ["models", INSTRUMENTS]);
    assert!(catalogue.sets.iter().all(|set| set.kinds() != [Kind::Synth]));
}

/// The real thing: the starter selection of the catalogue that is built
/// in, asked for with the panel's own button, fetched from where its sets
/// are published, found by the library, put on a track and played. About
/// 1.8 GB over the network into a temporary folder (`TMP` says where).
#[test]
#[ignore = "fetches the starter selection from where it is published (1.8 GB): \
            cargo test -p trill-app the_starter_selection_is_fetched_from_the_panel -- --ignored --nocapture"]
fn the_starter_selection_is_fetched_from_the_panel_and_played() {
    let folder = tempfile::tempdir().unwrap();
    let storage = Storage::At { settings: folder.path().join("config").join("settings.json"), library_index: None };
    let options = RuntimeOptions { audio: AudioBackend::Null { sample_rate: 48_000 }, plugins: None, storage };
    let studying = Studying { parts: Some(Vec::new()), ..Studying::default() };
    let runtime = Arc::new(Runtime::start_custom(options, Parts { studying, ..Parts::default() }).unwrap());
    // As the program does when it starts: it fetches with curl, and offers the catalogue that is built in.
    runtime.study_and_fetch_as_usual();
    let mut harness = Harness::on(runtime, UiOptions::default(), |_| {});
    let catalogue = Catalogue::shipped();
    let starter = Ask::Starter.sets(catalogue);
    let all: u64 = starter.iter().map(|set| set.fetched_bytes()).sum();
    harness.open_instruments();
    let began = Instant::now();
    harness.click_label(&format!("Starter {DOT} {}", said(all)));
    // Minutes: watched as the panel shows it, a look a second.
    let mut said_last = String::new();
    for _ in 0..3_600 {
        harness.idle(2);
        let status = harness.runtime.fetches().unwrap().status();
        let sets = status.iter().find(|list| list.id == INSTRUMENTS).unwrap();
        let here = sets.parts.iter().filter(|part| part.state == PartState::Ready).count();
        let failed: Vec<&str> = sets.parts.iter().filter_map(|part| part.error.as_deref()).collect();
        assert!(failed.is_empty(), "{failed:?}");
        let line = format!("{here} of {} here", starter.len());
        if line != said_last {
            println!("{:>5.0} s: {line}", began.elapsed().as_secs_f64());
            said_last = line;
        }
        if here == starter.len() && !harness.runtime.fetches().unwrap().fetching() {
            break;
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    harness.settle_instruments();
    assert!(harness.is_on_screen(&format!("Starter {DOT} here")), "the panel says all of it is here");
    println!("the starter selection, {}, was fetched in {:.0} s", said(all), began.elapsed().as_secs_f64());
    // Every instrument of every set is in the library, known for the catalogue's by its file.
    assert!(harness.runtime.library.wait_until_idle(Duration::from_secs(600)));
    let here = harness.runtime.catalogue_here();
    for set in &starter {
        for instrument in &set.instruments {
            assert!(here.contains_key(&instrument.text), "{} {} is not in the library", set.id, instrument.name);
        }
    }
    // Each instrument of the starter selection on a track, a note played, and heard.
    harness.key(Modifiers::NONE, Key::Escape);
    for (set, instrument) in catalogue.instruments().filter(|(_, instrument)| instrument.is_starter()) {
        let file = here[&instrument.text].display().to_string();
        let (low, high) = instrument.range.unwrap_or((48, 72));
        let key = low + (high - low) / 2;
        let track = {
            let mut session = harness.session.lock();
            let track = session.edit(|project| Ok(project.add_track(&instrument.name))).unwrap();
            session.add_device(ChainRef::Track(track), "builtin:sampled", Some(&file), None).unwrap();
            session
                .edit(|project| {
                    let clip = project.add_midi_clip(track, "c", 0.0, 4.0)?;
                    let keys = [low + (high - low) / 4, key, low + (high - low) / 4 * 3];
                    let notes: Vec<Note> = keys
                        .iter()
                        .enumerate()
                        .map(|(step, pitch)| Note { pitch: *pitch, start: step as f64, length: 0.9, velocity: 100 })
                        .collect();
                    project.clip_mut(clip)?.add_notes(&notes)
                })
                .unwrap();
            track
        };
        let job = harness.session.lock().render_job();
        let rendered = job.render(&trill_engine::render::RenderOptions::default()).unwrap();
        let peak = rendered.left.iter().fold(0.0f32, |peak, sample| peak.max(sample.abs()));
        println!("{} ({}): {:.0} dB", instrument.name, set.name, 20.0 * peak.max(1e-9).log10());
        assert!(peak > 0.001, "{} of {} is silent", instrument.name, set.id);
        harness.session.lock().edit(|project| project.remove_track(track).map(drop)).unwrap();
    }
    // What the song would owe for them is known.
    assert!(!catalogue.credits().is_empty());
}
