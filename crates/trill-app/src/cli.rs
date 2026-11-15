//! Command-line options.

use crate::ui::{DetailTab, UiOptions};
use anyhow::{Context, anyhow, bail};
use std::path::PathBuf;
use std::time::Duration;
use trill_core::{ClipId, Project};
use trill_engine::AudioBackend;

pub const USAGE: &str = "\
Usage: trill [OPTIONS] [PROJECT.trill]

Options:
  --demo                  Start with the built-in demo song
  --tab <clip|devices>    Detail tab to show first (default: clip)
  --select <CLIP NAME>    Select the first clip with this name at startup
  --play                  Start playback when the window opens
  --size <WIDTHxHEIGHT>   Initial window size in points (default: 1440x900)
  --theme <noir|graphite> Colors of the window (default: noir)
  --no-browser            Start with the device browser hidden (Ctrl+B shows it)
  --null-audio            Run without an audio device
  --no-plugins            Offer only the built-in devices
  --mcp-port <PORT>       Port of the MCP server for AI assistants (default: 7777)
  --no-mcp                Do not start the MCP server
  --screenshot <PATH.png> Render the window to a PNG and exit (implies --null-audio)
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
    pub play: bool,
    pub size: Option<[f32; 2]>,
    /// Name of the theme to start in.
    pub theme: Option<&'static str>,
    pub no_browser: bool,
    pub null_audio: bool,
    pub no_plugins: bool,
    pub no_mcp: bool,
    pub mcp_port: Option<u16>,
    pub screenshot: Option<PathBuf>,
    pub exit_after: Option<Duration>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Parsed {
    Run(Cli),
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
            "--no-plugins" => cli.no_plugins = true,
            "--no-mcp" => cli.no_mcp = true,
            "--no-browser" => cli.no_browser = true,
            "--mcp-port" => {
                let text = value("a port number")?;
                cli.mcp_port = Some(text.parse().with_context(|| format!("'{text}' is not a port number"))?);
            }
            "--tab" => {
                cli.tab = match value("a tab name")?.as_str() {
                    "clip" => DetailTab::Clip,
                    "devices" => DetailTab::Devices,
                    other => bail!("unknown tab '{other}' (expected 'clip' or 'devices')"),
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
    Ok(Parsed::Run(cli))
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
    /// Screenshots run without audio hardware so they work on any machine.
    pub fn audio_backend(&self) -> AudioBackend {
        if self.null_audio || self.screenshot.is_some() {
            AudioBackend::Null { sample_rate: 48_000 }
        } else {
            AudioBackend::Auto
        }
    }

    /// The clip `--select` names, if the option was given.
    pub fn clip_to_select(&self, project: &Project) -> anyhow::Result<Option<ClipId>> {
        let Some(name) = &self.select else { return Ok(None) };
        let mut clips = project.tracks.iter().flat_map(|track| track.clips.iter());
        let clip = clips.find(|clip| clip.name == *name).with_context(|| format!("no clip is named '{name}'"))?;
        Ok(Some(clip.id))
    }

    /// Whether to host third-party plugins. Screenshots show only what
    /// ships with the app, so they look the same on every machine.
    pub fn plugins_enabled(&self) -> bool {
        !self.no_plugins && self.screenshot.is_none()
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
            mcp_url: None,
            theme: self.theme.unwrap_or(defaults.theme),
            browser: !self.no_browser,
            screenshot: self.screenshot.clone(),
            autoplay: self.play,
            exit_after: self.exit_after,
            window_size: self.size.unwrap_or(defaults.window_size),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(args: &[&str]) -> anyhow::Result<Cli> {
        match parse(args.iter().map(|arg| arg.to_string()))? {
            Parsed::Run(cli) => Ok(cli),
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
    fn help_is_recognized() {
        assert_eq!(parse(["--help".to_owned()]).unwrap(), Parsed::Help);
        assert_eq!(parse(["--demo".to_owned(), "-h".to_owned()]).unwrap(), Parsed::Help);
    }

    #[test]
    fn mistakes_are_reported() {
        for args in [
            &["--frobnicate"][..],
            &["--tab"],
            &["--tab", "mixer"],
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
