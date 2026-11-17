//! Command-line options.

use crate::audio_scene::AudioScene;
use crate::ui::{AssistantOptions, AssistantScene, BrowserTab, DetailTab, KeyboardOptions, UiOptions};
use anyhow::{Context, anyhow, bail};
use std::path::PathBuf;
use std::time::Duration;
use trill_core::{ClipId, ControllerKind, Project, TrackId};
use trill_engine::AudioBackend;
use trill_plugins::Scanner;
use trill_runtime::{RuntimeOptions, Storage};

pub const USAGE: &str = "\
Usage: trill [OPTIONS] [PROJECT.trill]

Options:
  --demo                  Start with the built-in demo song
  --tab <clip|devices|mixer>  Detail tab to show first (default: clip)
  --select <CLIP NAME>    Select the first clip with this name at startup
  --also-select <NAME>    Select this clip as well; may be given several times
  --automation            Start with a lane open for every automation envelope
  --lane <CONTROLLER>     What the lane under the piano roll's notes shows first: velocity, or a
                          MIDI controller by name or number (expression, mod_wheel, pitch_bend,
                          sustain, 74)
  --shortcuts             Start with the shortcut reference open
  --play                  Start playback when the window opens
  --size <WIDTHxHEIGHT>   Initial window size in points (default: 1440x900)
  --theme <noir|graphite> Colors of the window (default: noir)
  --no-browser            Start with the browser hidden (Ctrl+B shows it)
  --browse <devices|sounds>   What the browser lists first (default: as it was left)
  --library <FOLDER>      Add a folder of sounds to the library; may be given several times
  --search <TEXT>         Search the sound library for this at startup
  --sound <NAME>          Select the sound with this name in the browser
  --editor <DEVICE NAME>  Open this plugin's own window at startup
  --profile <FOLDER>      Keep settings, the library index and safety copies in this
                          folder instead of the user's own
  --null-audio            Run without an audio device
  --no-plugins            Offer only the built-in devices
  --mcp-port <PORT>       Port of the MCP server for AI assistants (default: 7777)
  --no-mcp                Do not start the MCP server
  --assistant             Start with the assistant's panel open (Ctrl+Shift+A shows it)
  --assistant-model <NAME>    Model the assistant asks Claude Code for in this run (default: the one chosen
                          in its panel, which unless one was chosen is Claude Code's own choice)
  --assistant-scene <NAME>    Show a canned conversation in the assistant, for screenshots: ready, working,
                          answered, markdown, model, effort, song, song-done, limit, api-key, signed-out
                          or not-installed
  --keyboard              Start with the computer keyboard playing notes (Ctrl+K switches it)
  --arm <TRACK NAME>      Arm this track at startup; may be given several times
  --record                Start recording on the armed tracks when the window opens (F9 does it)
  --stage                 Open on the stage: the song filmed as it plays (F11 shows it)
  --settings              Start with the Audio and MIDI panel open (Ctrl+, shows it)
  --audio-scene <NAME>    Show made-up audio and MIDI devices in that panel, for screenshots:
                          studio, fallback or none
  --hold <KEYS>           Hold these keys of it down, for screenshots: adg is a C major chord, and
                          zzat a kick and a hat two octaves down
  --screenshot <PATH.png> Render the window to a PNG and exit (implies --null-audio)
  --real-audio            With --screenshot: open this machine's audio output and MIDI inputs
                          all the same, to see the window on real hardware (nothing is played)
  --exit-after <SECONDS>  Close the window by itself after this long
  -h, --help              Show this help
";

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Cli {
    /// A project file to open at startup.
    pub project: Option<PathBuf>,
    pub demo: bool,
    pub tab: DetailTab,
    /// Name of a clip to select at startup.
    pub select: Option<String>,
    /// Names of more clips to select with it.
    pub also_select: Vec<String>,
    /// Unfold a lane for every automation envelope.
    pub automation: bool,
    /// Open the shortcut reference.
    pub shortcuts: bool,
    pub play: bool,
    pub size: Option<[f32; 2]>,
    /// Name of the theme to start in.
    pub theme: Option<&'static str>,
    pub no_browser: bool,
    pub browse: Option<BrowserTab>,
    /// Folders to add to the sound library.
    pub library: Vec<PathBuf>,
    /// What to search the sound library for.
    pub search: Option<String>,
    /// Name of a sound to select in the browser.
    pub sound: Option<String>,
    /// Name of a device whose editor to open.
    pub editor: Option<String>,
    /// A folder to keep settings, the library index and safety copies in.
    pub profile: Option<PathBuf>,
    pub null_audio: bool,
    pub no_plugins: bool,
    pub no_mcp: bool,
    pub mcp_port: Option<u16>,
    /// Open the assistant's panel.
    pub assistant: bool,
    pub assistant_model: Option<String>,
    pub assistant_scene: Option<AssistantScene>,
    /// How the computer keyboard starts, when not as it was left.
    pub keyboard: KeyboardOptions,
    /// Names of tracks to arm at startup.
    pub arm: Vec<String>,
    /// Start a take when the window opens.
    pub record: bool,
    /// Open on the stage.
    pub stage: bool,
    /// Open the Audio and MIDI panel.
    pub settings: bool,
    /// Made-up audio and MIDI devices to show in it.
    pub audio_scene: Option<AudioScene>,
    /// The controller the lane under the piano roll's notes shows first;
    /// `None` for velocity.
    pub lane: Option<ControllerKind>,
    pub screenshot: Option<PathBuf>,
    /// With a screenshot: open the machine's own devices all the same.
    pub real_audio: bool,
    pub exit_after: Option<Duration>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Parsed {
    Run(Box<Cli>),
    Help,
}

/// Parses the arguments after the program name.
pub fn parse(args: impl IntoIterator<Item = String>) -> anyhow::Result<Parsed> {
    let mut cli = Cli::default();
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        let mut value = |what: &str| args.next().ok_or_else(|| anyhow!("{arg} needs {what}"));
        match arg.as_str() {
            "-h" | "--help" => return Ok(Parsed::Help),
            "--demo" => cli.demo = true,
            "--play" => cli.play = true,
            "--null-audio" => cli.null_audio = true,
            "--real-audio" => cli.real_audio = true,
            "--no-plugins" => cli.no_plugins = true,
            "--no-mcp" => cli.no_mcp = true,
            "--no-browser" => cli.no_browser = true,
            "--assistant" => cli.assistant = true,
            "--assistant-model" => cli.assistant_model = Some(value("a model name")?),
            "--assistant-scene" => {
                let name = value("the name of a scene")?;
                let known: Vec<&str> = AssistantScene::ALL.iter().map(|(name, _)| *name).collect();
                let scene = AssistantScene::named(&name);
                cli.assistant_scene = Some(
                    scene.with_context(|| format!("unknown scene '{name}' (expected one of {})", known.join(", ")))?,
                );
            }
            "--keyboard" => cli.keyboard.on = true,
            "--arm" => cli.arm.push(value("a track name")?),
            "--record" => cli.record = true,
            "--stage" => cli.stage = true,
            "--settings" => cli.settings = true,
            "--audio-scene" => {
                let name = value("the name of a scene")?;
                let known: Vec<&str> = AudioScene::ALL.iter().map(|(name, _)| *name).collect();
                let scene = AudioScene::named(&name);
                cli.audio_scene = Some(
                    scene.with_context(|| format!("unknown scene '{name}' (expected one of {})", known.join(", ")))?,
                );
            }
            "--hold" => {
                let keys = value("the keys to hold")?;
                cli.keyboard.hold = KeyboardOptions::holding(&keys).map_err(|reason| anyhow!(reason))?;
            }
            "--automation" => cli.automation = true,
            "--lane" => {
                let name = value("what the lane shows")?;
                cli.lane = match name.as_str() {
                    "velocity" => None,
                    name => Some(ControllerKind::parse(name).with_context(|| {
                        format!(
                            "the lane cannot show '{name}' (expected velocity, or a controller such as expression,                              mod_wheel, pitch_bend, sustain or a number from 0 to 119)"
                        )
                    })?),
                };
            }
            "--shortcuts" => cli.shortcuts = true,
            "--also-select" => cli.also_select.push(value("a clip name")?),
            "--library" => cli.library.push(PathBuf::from(value("a folder")?)),
            "--search" => cli.search = Some(value("what to search for")?),
            "--sound" => cli.sound = Some(value("the name of a sound")?),
            "--editor" => cli.editor = Some(value("the name of a device")?),
            "--profile" => cli.profile = Some(PathBuf::from(value("a folder")?)),
            "--browse" => {
                cli.browse = Some(match value("what to list")?.as_str() {
                    "devices" => BrowserTab::Devices,
                    "sounds" => BrowserTab::Sounds,
                    other => bail!("the browser cannot list '{other}' (expected 'devices' or 'sounds')"),
                })
            }
            "--mcp-port" => {
                let text = value("a port number")?;
                cli.mcp_port = Some(text.parse().with_context(|| format!("'{text}' is not a port number"))?);
            }
            "--tab" => {
                cli.tab = match value("a tab name")?.as_str() {
                    "clip" => DetailTab::Clip,
                    "devices" => DetailTab::Devices,
                    "mixer" => DetailTab::Mixer,
                    other => bail!("unknown tab '{other}' (expected 'clip', 'devices' or 'mixer')"),
                }
            }
            "--select" => cli.select = Some(value("a clip name")?),
            "--size" => cli.size = Some(parse_size(&value("a size such as 1440x900")?)?),
            "--theme" => {
                let name = value("a theme name")?;
                let known = crate::ui::theme_names();
                let theme = known.iter().find(|theme| **theme == name);
                cli.theme =
                    Some(theme.with_context(|| format!("unknown theme '{name}' (expected {})", known.join(" or ")))?);
            }
            "--screenshot" => cli.screenshot = Some(PathBuf::from(value("a file path")?)),
            "--exit-after" => {
                let text = value("a number of seconds")?;
                let seconds: f64 = text.parse().with_context(|| format!("'{text}' is not a number of seconds"))?;
                if !seconds.is_finite() || seconds < 0.0 {
                    bail!("--exit-after must be zero or more seconds");
                }
                cli.exit_after = Some(Duration::from_secs_f64(seconds));
            }
            option if option.starts_with('-') => bail!("unknown option '{option}'\n\n{USAGE}"),
            _ if cli.project.is_some() => bail!("only one project file can be opened"),
            _ => cli.project = Some(PathBuf::from(arg)),
        }
    }
    if cli.demo && cli.project.is_some() {
        bail!("--demo cannot be combined with a project file");
    }
    Ok(Parsed::Run(Box::new(cli)))
}

/// The first clip of the project called `name`.
fn clip_named(project: &Project, name: &str) -> anyhow::Result<ClipId> {
    let mut clips = project.tracks.iter().flat_map(|track| track.clips.iter());
    let clip = clips.find(|clip| clip.name == name).with_context(|| format!("no clip is named '{name}'"))?;
    Ok(clip.id)
}

fn parse_size(text: &str) -> anyhow::Result<[f32; 2]> {
    let parsed =
        text.split_once(['x', 'X']).and_then(|(width, height)| Some([width.parse().ok()?, height.parse().ok()?]));
    match parsed {
        Some([width, height]) if width >= 320.0 && height >= 240.0 => Ok([width, height]),
        _ => bail!("'{text}' is not a window size (expected WIDTHxHEIGHT, at least 320x240)"),
    }
}

impl Cli {
    /// Screenshots run without audio hardware so they work on any machine;
    /// `--real-audio` is for the one that is to show this machine's.
    pub fn audio_backend(&self) -> AudioBackend {
        if self.null_audio || (self.screenshot.is_some() && !self.real_audio) {
            AudioBackend::Null { sample_rate: 48_000 }
        } else {
            AudioBackend::Auto
        }
    }

    /// The clip `--select` names, if the option was given.
    pub fn clip_to_select(&self, project: &Project) -> anyhow::Result<Option<ClipId>> {
        self.select.as_deref().map(|name| clip_named(project, name)).transpose()
    }

    /// The clips `--also-select` names.
    pub fn clips_to_also_select(&self, project: &Project) -> anyhow::Result<Vec<ClipId>> {
        self.also_select.iter().map(|name| clip_named(project, name)).collect()
    }

    /// Whether to host third-party plugins. Screenshots show only what
    /// ships with the app, so they look the same on every machine.
    pub fn plugins_enabled(&self) -> bool {
        !self.no_plugins && self.screenshot.is_none()
    }

    /// How to start the session and everything around it. Plugins are
    /// scanned in a second process of this executable. A screenshot run
    /// touches nothing of the user's: no plugins, no settings read or
    /// written, no library, so it comes out the same on every machine.
    /// With `--profile` what is kept between runs is kept in that folder,
    /// screenshot or not, and the user's own is left alone.
    pub fn runtime_options(&self) -> RuntimeOptions {
        let storage = match &self.profile {
            Some(folder) => Storage::At {
                settings: folder.join("settings.json"),
                library_index: Some(folder.join("library-index.json")),
            },
            None if self.screenshot.is_some() => Storage::Nowhere,
            None => Storage::User,
        };
        RuntimeOptions {
            audio: self.audio_backend(),
            plugins: self.plugins_enabled().then_some(Scanner::CurrentExecutable),
            storage,
        }
    }

    /// The local address the MCP server should listen on, if any. A
    /// screenshot run starts no server unless a port is asked for.
    pub fn mcp_address(&self) -> Option<std::net::SocketAddr> {
        if self.no_mcp || (self.screenshot.is_some() && self.mcp_port.is_none()) {
            return None;
        }
        Some(std::net::SocketAddr::from(([127, 0, 0, 1], self.mcp_port.unwrap_or(trill_mcp::DEFAULT_PORT))))
    }

    pub fn ui_options(&self) -> UiOptions {
        let defaults = UiOptions::default();
        UiOptions {
            detail_tab: self.tab,
            select_clip: None,
            also_select: Vec::new(),
            show_automation: self.automation,
            lane: self.lane,
            show_shortcuts: self.shortcuts,
            mcp_url: None,
            theme: self.theme.unwrap_or(defaults.theme),
            browser: !self.no_browser,
            browser_tab: self.browse,
            sound_search: self.search.clone(),
            select_sound: self.sound.clone(),
            screenshot: self.screenshot.clone(),
            autoplay: self.play,
            exit_after: self.exit_after,
            open_editor: self.editor.clone(),
            window_size: self.size.unwrap_or(defaults.window_size),
            assistant: AssistantOptions {
                open: self.assistant,
                scene: self.assistant_scene,
                model: self.assistant_model.clone(),
                // No address means the server was turned off; one that
                // could not be listened on is found out in `main`.
                mcp_off: self.mcp_address().is_none(),
            },
            keyboard: self.keyboard.clone(),
            show_settings: self.settings,
            record: self.record,
            stage: self.stage,
        }
    }

    /// The tracks `--arm` names.
    pub fn tracks_to_arm(&self, project: &Project) -> anyhow::Result<Vec<TrackId>> {
        let named = |name: &String| {
            let track = project.tracks.iter().find(|track| track.name == *name);
            track.map(|track| track.id).with_context(|| format!("no track is named '{name}'"))
        };
        self.arm.iter().map(named).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(args: &[&str]) -> anyhow::Result<Cli> {
        match parse(args.iter().map(|arg| arg.to_string()))? {
            Parsed::Run(cli) => Ok(*cli),
            Parsed::Help => bail!("help requested"),
        }
    }

    #[test]
    fn no_arguments_is_an_ordinary_run() {
        let cli = run(&[]).unwrap();
        assert_eq!(cli, Cli::default());
        assert_eq!(cli.audio_backend(), AudioBackend::Auto);
        let options = cli.ui_options();
        assert_eq!(options.detail_tab, DetailTab::Clip);
        assert!(options.screenshot.is_none() && !options.autoplay && options.exit_after.is_none());
    }

    #[test]
    fn plugins_and_mcp_are_on_unless_turned_off() {
        let default = run(&[]).unwrap();
        assert!(default.plugins_enabled());
        assert_eq!(default.mcp_address().unwrap().to_string(), "127.0.0.1:7777");

        let custom = run(&["--mcp-port", "9000", "--no-plugins"]).unwrap();
        assert!(!custom.plugins_enabled());
        assert_eq!(custom.mcp_address().unwrap().to_string(), "127.0.0.1:9000");
        assert_eq!(run(&["--no-mcp"]).unwrap().mcp_address(), None);
        assert!(run(&["--mcp-port", "seventy"]).is_err());
        assert!(run(&["--mcp-port"]).is_err());

        // Screenshots show a plain window: no plugins, and no server unless
        // a port is asked for.
        let shot = run(&["--screenshot", "out.png"]).unwrap();
        assert!(!shot.plugins_enabled());
        assert_eq!(shot.mcp_address(), None);
        assert!(run(&["--screenshot", "out.png", "--mcp-port", "9000"]).unwrap().mcp_address().is_some());
    }

    #[test]
    fn the_runtime_follows_the_options_and_screenshots_are_hermetic() {
        let ordinary = run(&[]).unwrap().runtime_options();
        assert_eq!(ordinary.audio, AudioBackend::Auto);
        assert!(matches!(ordinary.plugins, Some(Scanner::CurrentExecutable)));
        assert_eq!(ordinary.storage, Storage::User, "an ordinary run keeps the user's settings and library");

        let quiet = run(&["--null-audio", "--no-plugins"]).unwrap().runtime_options();
        assert_eq!(quiet.audio, AudioBackend::Null { sample_rate: 48_000 });
        assert!(quiet.plugins.is_none());
        assert_eq!(quiet.storage, Storage::User);

        // A screenshot must look the same everywhere: nothing of the
        // user's is read, written, scanned or hosted.
        let shot = run(&["--screenshot", "out.png", "--demo"]).unwrap().runtime_options();
        assert_eq!(shot.audio, AudioBackend::Null { sample_rate: 48_000 });
        assert!(shot.plugins.is_none());
        assert_eq!(shot.storage, Storage::Nowhere);

        // A profile folder takes the place of the user's own, in any run.
        let kept = Storage::At {
            settings: PathBuf::from("scratch/settings.json"),
            library_index: Some(PathBuf::from("scratch/library-index.json")),
        };
        assert_eq!(run(&["--profile", "scratch"]).unwrap().runtime_options().storage, kept);
        let shot = run(&["--profile", "scratch", "--screenshot", "out.png"]).unwrap().runtime_options();
        assert_eq!(shot.storage, kept, "which is how a picture of what is kept there is taken");
        assert!(shot.plugins.is_none());
    }

    #[test]
    fn options_are_parsed() {
        let cli = run(&[
            "--demo",
            "--tab",
            "devices",
            "--play",
            "--size",
            "1280x800",
            "--screenshot",
            "out/shot.png",
            "--exit-after",
            "2.5",
            "--select",
            "Arp B",
        ])
        .unwrap();
        assert!(cli.demo && cli.play);
        assert_eq!(cli.select.as_deref(), Some("Arp B"));
        assert_eq!(cli.tab, DetailTab::Devices);
        assert_eq!(cli.size, Some([1280.0, 800.0]));
        assert_eq!(cli.screenshot, Some(PathBuf::from("out/shot.png")));
        assert_eq!(cli.exit_after, Some(Duration::from_millis(2500)));
        assert_eq!(cli.audio_backend(), AudioBackend::Null { sample_rate: 48_000 }, "screenshots need no audio device");

        let options = cli.ui_options();
        assert_eq!(options.window_size, [1280.0, 800.0]);
        assert!(options.autoplay);
        assert_eq!(options.detail_tab, DetailTab::Devices);
    }

    #[test]
    fn the_theme_is_noir_unless_another_is_named() {
        assert_eq!(run(&[]).unwrap().ui_options().theme, "noir");
        assert!(run(&[]).unwrap().ui_options().browser);
        assert!(!run(&["--no-browser"]).unwrap().ui_options().browser);
        assert_eq!(run(&["--theme", "noir"]).unwrap().ui_options().theme, "noir");
        assert_eq!(run(&["--theme", "graphite"]).unwrap().ui_options().theme, "graphite");
        let error = run(&["--theme", "neon"]).unwrap_err().to_string();
        assert!(error.contains("neon") && error.contains("noir or graphite"), "{error}");
    }

    #[test]
    fn a_bare_argument_is_the_project_to_open() {
        let cli = run(&["songs/idea.trill", "--null-audio"]).unwrap();
        assert_eq!(cli.project, Some(PathBuf::from("songs/idea.trill")));
        assert_eq!(cli.audio_backend(), AudioBackend::Null { sample_rate: 48_000 });
        assert!(run(&["a.trill", "b.trill"]).is_err());
        assert!(run(&["--demo", "a.trill"]).is_err());
    }

    #[test]
    fn select_finds_a_clip_by_name() {
        let mut project = Project::new("t");
        let track = project.add_track("A");
        project.add_midi_clip(track, "Intro", 0.0, 4.0).unwrap();
        let verse = project.add_midi_clip(track, "Verse", 4.0, 4.0).unwrap();

        assert_eq!(run(&[]).unwrap().clip_to_select(&project).unwrap(), None);
        assert_eq!(run(&["--select", "Verse"]).unwrap().clip_to_select(&project).unwrap(), Some(verse));
        assert!(run(&["--select", "Chorus"]).unwrap().clip_to_select(&project).is_err());
    }

    #[test]
    fn the_views_to_start_in_can_be_chosen() {
        let cli = run(&["--tab", "mixer", "--automation", "--shortcuts", "--also-select", "A", "--also-select", "B"])
            .unwrap();
        assert_eq!(cli.tab, DetailTab::Mixer);
        assert_eq!(cli.also_select, ["A", "B"]);
        let options = cli.ui_options();
        assert!(options.show_automation && options.show_shortcuts);
        let plain = run(&[]).unwrap().ui_options();
        assert!(!plain.show_automation && !plain.show_shortcuts && plain.also_select.is_empty());

        let mut project = Project::new("t");
        let track = project.add_track("A");
        let a = project.add_midi_clip(track, "A", 0.0, 4.0).unwrap();
        let b = project.add_midi_clip(track, "B", 4.0, 4.0).unwrap();
        assert_eq!(cli.clips_to_also_select(&project).unwrap(), [a, b]);
        assert!(run(&["--also-select", "C"]).unwrap().clips_to_also_select(&project).is_err());
    }

    #[test]
    fn the_browser_can_start_on_the_sound_library() {
        let plain = run(&[]).unwrap();
        assert_eq!(plain.browse, None, "left to what the browser remembers");
        assert!(plain.library.is_empty() && plain.search.is_none() && plain.sound.is_none());

        let cli = run(&[
            "--browse",
            "sounds",
            "--library",
            "packs/drums",
            "--library",
            "packs/loops",
            "--search",
            "kick",
            "--sound",
            "Kick Deep 01",
        ])
        .unwrap();
        assert_eq!(cli.library, [PathBuf::from("packs/drums"), PathBuf::from("packs/loops")]);
        let options = cli.ui_options();
        assert_eq!(options.browser_tab, Some(BrowserTab::Sounds));
        assert_eq!(options.sound_search.as_deref(), Some("kick"));
        assert_eq!(options.select_sound.as_deref(), Some("Kick Deep 01"));
        assert_eq!(run(&["--browse", "devices"]).unwrap().browse, Some(BrowserTab::Devices));
    }

    #[test]
    fn the_assistant_can_be_opened_and_told_what_to_use() {
        let plain = run(&[]).unwrap();
        let options = plain.ui_options().assistant;
        assert!(!options.open && options.scene.is_none() && options.model.is_none());
        assert!(!options.mcp_off, "the MCP server it works through is on unless turned off");
        assert!(run(&["--no-mcp"]).unwrap().ui_options().assistant.mcp_off);

        let cli = run(&["--assistant", "--assistant-model", "haiku", "--assistant-scene", "working"]).unwrap();
        let options = cli.ui_options().assistant;
        assert!(options.open);
        assert_eq!(options.model.as_deref(), Some("haiku"));
        assert_eq!(options.scene, Some(AssistantScene::Working));
        for (name, scene) in AssistantScene::ALL {
            assert_eq!(run(&["--assistant-scene", name]).unwrap().assistant_scene, Some(scene));
            assert!(USAGE.contains(name), "the help does not list the scene '{name}'");
        }
        let error = run(&["--assistant-scene", "live"]).unwrap_err().to_string();
        assert!(error.contains("live") && error.contains("signed-out"), "{error}");
        assert!(run(&["--assistant-model"]).is_err() && run(&["--assistant-scene"]).is_err());
    }

    #[test]
    fn the_computer_keyboard_can_start_on_with_keys_held() {
        let plain = run(&[]).unwrap().ui_options().keyboard;
        assert!(!plain.on && plain.hold.is_empty(), "left to what was remembered");
        assert!(run(&["--keyboard"]).unwrap().ui_options().keyboard.on);
        let held = run(&["--hold", "adg"]).unwrap().ui_options().keyboard.hold;
        assert_eq!(held.len(), 3);
        assert_eq!(run(&["--hold", "zzat"]).unwrap().ui_options().keyboard.hold.len(), 4);
        let error = run(&["--hold", "aq"]).unwrap_err().to_string();
        assert!(error.contains("'q'"), "{error}");
        assert!(USAGE.contains("--keyboard") && USAGE.contains("--hold"));
        // What the lane under the piano roll's notes shows first.
        assert!(USAGE.contains("--lane"));
        assert_eq!(run(&["--lane", "expression"]).unwrap().ui_options().lane, Some(ControllerKind::EXPRESSION));
        assert_eq!(run(&["--lane", "pitch_bend"]).unwrap().ui_options().lane, Some(ControllerKind::PitchBend));
        assert_eq!(run(&["--lane", "74"]).unwrap().ui_options().lane, ControllerKind::numbered(74));
        assert_eq!(run(&["--lane", "velocity"]).unwrap().ui_options().lane, None);
        assert!(run(&["--lane", "loudness"]).unwrap_err().to_string().contains("cannot show 'loudness'"));
    }

    #[test]
    fn arming_recording_and_the_audio_panel_can_be_asked_for_at_startup() {
        let plain = run(&[]).unwrap();
        assert!(plain.arm.is_empty() && !plain.record && !plain.settings && plain.audio_scene.is_none());
        let options = plain.ui_options();
        assert!(!options.record && !options.show_settings);

        let cli =
            run(&["--arm", "Keys", "--arm", "Bass", "--record", "--settings", "--audio-scene", "studio"]).unwrap();
        assert_eq!(cli.audio_scene, Some(AudioScene::Studio));
        let options = cli.ui_options();
        assert!(options.record && options.show_settings);
        let mut project = Project::new("t");
        let keys = project.add_track("Keys");
        let bass = project.add_track("Bass");
        assert_eq!(cli.tracks_to_arm(&project).unwrap(), [keys, bass]);
        let error = run(&["--arm", "Tuba"]).unwrap().tracks_to_arm(&project).unwrap_err().to_string();
        assert!(error.contains("Tuba"), "{error}");
        for (name, scene) in AudioScene::ALL {
            assert_eq!(run(&["--audio-scene", name]).unwrap().audio_scene, Some(scene));
            assert!(USAGE.contains(name), "the help does not list the scene '{name}'");
        }
        let error = run(&["--audio-scene", "stadium"]).unwrap_err().to_string();
        assert!(error.contains("stadium") && error.contains("fallback"), "{error}");
        assert!(run(&["--arm"]).is_err() && run(&["--audio-scene"]).is_err());
    }

    #[test]
    fn a_picture_shows_this_machines_devices_only_when_asked_to() {
        let null = AudioBackend::Null { sample_rate: 48_000 };
        assert_eq!(run(&["--screenshot", "out.png", "--settings"]).unwrap().audio_backend(), null);
        let real = run(&["--screenshot", "out.png", "--settings", "--real-audio"]).unwrap();
        assert_eq!(real.audio_backend(), AudioBackend::Auto);
        // Still nothing of the user's: no plugins, no settings, no server.
        assert!(!real.plugins_enabled() && real.mcp_address().is_none());
        assert_eq!(real.runtime_options().storage, Storage::Nowhere);
        // And the switch that turns audio off wins over it.
        assert_eq!(run(&["--screenshot", "out.png", "--real-audio", "--null-audio"]).unwrap().audio_backend(), null);
        assert_eq!(run(&["--real-audio"]).unwrap().audio_backend(), AudioBackend::Auto, "without a picture, as ever");
        assert!(USAGE.contains("--real-audio"));
    }

    #[test]
    fn a_plugins_editor_can_be_asked_for_at_startup() {
        assert_eq!(run(&[]).unwrap().ui_options().open_editor, None);
        let cli = run(&["--editor", "Surge XT", "song.trill"]).unwrap();
        assert_eq!(cli.ui_options().open_editor.as_deref(), Some("Surge XT"));
    }

    #[test]
    fn help_is_recognized() {
        assert_eq!(parse(["--help".to_owned()]).unwrap(), Parsed::Help);
        assert_eq!(parse(["--demo".to_owned(), "-h".to_owned()]).unwrap(), Parsed::Help);
    }

    #[test]
    fn mistakes_are_reported() {
        for args in [
            &["--frobnicate"][..],
            &["--tab"],
            &["--tab", "sequencer"],
            &["--also-select"],
            &["--browse"],
            &["--browse", "presets"],
            &["--library"],
            &["--search"],
            &["--sound"],
            &["--editor"],
            &["--hold"],
            &["--hold", "q"],
            &["--profile"],
            &["--size", "big"],
            &["--size", "10x10"],
            &["--theme"],
            &["--exit-after", "soon"],
            &["--exit-after", "-1"],
            &["--screenshot"],
            &["--select"],
        ] {
            assert!(run(args).is_err(), "{args:?} should be rejected");
        }
    }
}
