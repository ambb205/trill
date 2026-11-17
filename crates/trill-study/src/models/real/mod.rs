//! The real models: networks run by ONNX Runtime, on a graphics card or
//! on the processor.
//!
//! Nothing of the runtime is part of Trill. Its library, the part that
//! drives a graphics card and the models' own files are fetched by the
//! user in the setup, each from where its makers publish it, into one
//! folder ([`Setup::parts`]); this module loads them from there when a
//! study is run, in the process that runs it.
//!
//! - [`demucs`] splits a recording into stems (Hybrid Transformer Demucs).
//! - [`beat`] finds beats and downbeats (Beat This).
//! - [`voice`] hears where someone sings (Silero VAD), which is all that
//! - [`whisper`] is handed to write down the words (Whisper), and
//! - [`align`] places each word (a CTC model and the best path through
//!   the letters).
//!
//! Each model is made ready when its stage begins and let go of when it
//! ends, so that a card holds one at a time. Whatever is fed to a model
//! goes through [`tensor`], which refuses a shape the model was not made
//! for before the runtime sees it.

pub mod align;
pub mod beat;
pub mod demucs;
pub mod voice;
pub mod whisper;

use super::{Models, Progress, Transcript};
use crate::lyrics::Word;
use crate::store::StemChoice;
use ort::environment::Environment;
use ort::session::Session;
use ort::value::Tensor;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use trill_listen::{Sound, Stem, TrackedBeats};

/// The folders of the setup's parts, by what is in them.
pub const RUNTIME: &str = "runtime";
pub const ANY_CARD: &str = "any-card";
pub const NVIDIA: &str = "nvidia";
pub const STEMS: &str = "stems";
pub const STEMS_BEST: &str = "stems-best";
pub const BEATS: &str = "beats";
pub const VOICE: &str = "voice";
pub const WORDS: &str = "words";
pub const WORDS_PROCESSOR: &str = "words-processor";
pub const WORD_TIMES: &str = "word-times";

/// What runs the models.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    /// The processor: always there, and slower than a real graphics card.
    #[default]
    Processor,
    /// A graphics card of any make, through the provider that needs
    /// nothing of its maker's: the `adapter`-th the system lists.
    AnyCard { adapter: u32 },
    /// An NVIDIA card through NVIDIA's own libraries.
    Nvidia,
}

/// Where the models' files are and what is to run them.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Setup {
    /// The folder the setup keeps its parts in.
    pub parts: PathBuf,
    #[serde(default)]
    pub provider: Provider,
}

/// The part of the runtime that drives a graphics card, registered.
struct Card {
    /// What the runtime calls the provider.
    provider: String,
    options: Vec<(String, String)>,
    /// Which of the provider's devices.
    index: usize,
    name: String,
}

pub struct OnnxModels {
    setup: Setup,
    env: Environment,
    card: Option<Card>,
}

/// What every sentence about a part that is not there ends with.
const SET_UP: &str = "Setup, in the File menu, shows what there is to fetch";

/// The file `name` of the part `part`, or a sentence that says it has not
/// been fetched.
fn part(parts: &Path, part: &str, name: &str, what: &str) -> Result<PathBuf, String> {
    let path = parts.join(part).join(name);
    if path.is_file() {
        Ok(path)
    } else {
        Err(format!("{what} has not been fetched ({} is not there): {SET_UP}", path.display()))
    }
}

/// A tensor of `shape` holding `data`, for a model that takes at most
/// `most` values: a shape that does not hold exactly the data, or more
/// than the model was made for, is refused before the runtime sees it.
/// A network asks for memory by the square of what it is fed, without a
/// word.
pub(crate) fn tensor<T: ort::value::PrimitiveTensorElementType + std::fmt::Debug + Clone + 'static>(
    shape: &[usize],
    data: Vec<T>,
    most: usize,
) -> Result<Tensor<T>, String> {
    let count: usize = shape.iter().product();
    if count != data.len() || count == 0 || count > most {
        return Err(format!(
            "a model was about to be fed {} values in the shape {shape:?}, where at most {most} in a shape that holds them are its own: refused",
            data.len()
        ));
    }
    let shape: Vec<i64> = shape.iter().map(|&size| size as i64).collect();
    Tensor::from_array((shape, data)).map_err(|error| error.to_string())
}

impl OnnxModels {
    pub fn new(mut setup: Setup) -> Result<Self, String> {
        // The runtime looks for a part it is handed from where its own library is, unless it is told the whole way.
        if let Ok(whole) = std::path::absolute(&setup.parts) {
            setup.parts = whole;
        }
        let library = part(&setup.parts, RUNTIME, runtime_library(), "the models' runtime")?;
        let (plugin, what) = match setup.provider {
            Provider::Processor => (None, ""),
            Provider::AnyCard { .. } => (
                Some(part(
                    &setup.parts,
                    ANY_CARD,
                    card_library("webgpu"),
                    "the part that runs models on a graphics card",
                )?),
                "any",
            ),
            Provider::Nvidia => {
                // NVIDIA's libraries are looked for where programs are: beside the provider is not enough.
                let folder = setup.parts.join(NVIDIA);
                let mut folders = vec![folder];
                folders.extend(std::env::var_os("PATH").iter().flat_map(std::env::split_paths));
                if let Ok(joined) = std::env::join_paths(folders) {
                    // SAFETY: this process has one thread yet: it was started to study and has done nothing else.
                    unsafe { std::env::set_var("PATH", joined) };
                }
                (
                    Some(part(
                        &setup.parts,
                        NVIDIA,
                        card_library("cuda"),
                        "the part that runs models on an NVIDIA card",
                    )?),
                    "nvidia",
                )
            }
        };
        let env = ort::init_from(&library)
            .map_err(|error| format!("the models' runtime ({}) could not be loaded: {error}", library.display()))?
            .with_name("trill-study")
            .build()
            .map_err(|error| format!("the models' runtime could not be started: {error}"))?;
        let card = match plugin {
            None => None,
            Some(plugin) => {
                let before: Vec<String> =
                    env.devices().filter_map(|device| device.ep().ok().map(str::to_owned)).collect();
                env.register_ep_library("trill-study-card", &plugin).map_err(|error| {
                    format!(
                        "the part that runs models on a graphics card ({}) could not be loaded: {error}",
                        plugin.display()
                    )
                })?;
                let devices: Vec<(String, String)> = env
                    .devices()
                    .filter_map(|device| {
                        Some((
                            device.ep().ok()?.to_owned(),
                            device.hardware_device().vendor().unwrap_or("a").to_owned(),
                        ))
                    })
                    .filter(|(provider, _)| !before.contains(provider))
                    .collect();
                let index = match setup.provider {
                    Provider::AnyCard { adapter } => adapter as usize,
                    _ => 0,
                };
                let Some((provider, vendor)) = devices.get(index).cloned() else {
                    return Err(match devices.len() {
                        0 => "the system lists no graphics card that can run the models: the processor can".to_owned(),
                        count => format!(
                            "there is no graphics card {index} here: the system lists {count}. Run the timed test again (Setup, in the File menu)"
                        ),
                    });
                };
                let options = if what == "any" {
                    vec![(format!("{provider}.adapterIndex"), index.to_string())]
                } else {
                    Vec::new()
                };
                Some(Card { provider, options, index, name: format!("a graphics card ({})", vendor.trim()) })
            }
        };
        Ok(Self { setup, env, card })
    }

    /// The graphics cards a provider lists, by the name of who made each.
    pub fn cards(&self) -> Vec<String> {
        let Some(card) = &self.card else { return Vec::new() };
        self.env
            .devices()
            .filter(|device| device.ep().is_ok_and(|provider| provider == card.provider))
            .map(|device| device.hardware_device().vendor().unwrap_or("unknown").trim().to_owned())
            .collect()
    }

    /// The model in `file`, made ready on whatever runs the models.
    pub(crate) fn session(&self, file: &Path) -> Result<Session, String> {
        let name = file.file_name().unwrap_or_default().to_string_lossy().into_owned();
        let cannot = |error: String| format!("the model {name} could not be made ready on {}: {error}", self.device());
        let mut builder = Session::builder(&self.env).map_err(|error| cannot(error.to_string()))?;
        if let Some(card) = &self.card {
            let devices: Vec<_> = self
                .env
                .devices()
                .filter(|device| device.ep().is_ok_and(|provider| provider == card.provider))
                .collect();
            // The provider takes one device; which card it is, is among its options.
            let chosen = if card.options.is_empty() { card.index.min(devices.len().saturating_sub(1)) } else { 0 };
            builder = builder
                .with_devices(devices.into_iter().skip(chosen).take(1), Some(&card.options))
                .map_err(|error| cannot(error.to_string()))?;
        }
        builder.commit_from_file(file).map_err(|error| cannot(error.to_string()))
    }

    /// The model in `file`, made ready on the processor whatever runs the
    /// others: for one that is too small to be worth a card.
    pub(crate) fn session_on_processor(&self, file: &Path) -> Result<Session, String> {
        let name = file.file_name().unwrap_or_default().to_string_lossy().into_owned();
        let cannot = |error: String| format!("the model {name} could not be made ready on the processor: {error}");
        let builder = Session::builder(&self.env).map_err(|error| cannot(error.to_string()))?;
        let mut builder = builder.with_intra_threads(1).map_err(|error| cannot(error.to_string()))?;
        builder.commit_from_file(file).map_err(|error| cannot(error.to_string()))
    }

    fn file(&self, folder: &str, name: &str, what: &str) -> Result<PathBuf, String> {
        part(&self.setup.parts, folder, name, what)
    }

    fn on_processor(&self) -> bool {
        self.card.is_none()
    }
}

/// The runtime's library, as the system this was built for names it.
fn runtime_library() -> &'static str {
    if cfg!(windows) {
        "onnxruntime.dll"
    } else if cfg!(target_os = "macos") {
        "libonnxruntime.dylib"
    } else {
        "libonnxruntime.so"
    }
}

fn card_library(kind: &str) -> &'static str {
    match (kind, cfg!(windows), cfg!(target_os = "macos")) {
        ("webgpu", true, _) => "onnxruntime_providers_webgpu.dll",
        ("webgpu", _, true) => "libonnxruntime_providers_webgpu.dylib",
        ("webgpu", ..) => "libonnxruntime_providers_webgpu.so",
        (_, true, _) => "onnxruntime_providers_cuda.dll",
        _ => "libonnxruntime_providers_cuda.so",
    }
}

impl Models for OnnxModels {
    fn device(&self) -> String {
        match (&self.card, self.setup.provider) {
            (Some(card), Provider::Nvidia) => format!("{} through NVIDIA's libraries", card.name),
            (Some(card), _) => card.name.clone(),
            (None, _) => "the processor".to_owned(),
        }
    }

    fn places_words(&self) -> bool {
        self.setup.parts.join(WORD_TIMES).join("model.onnx").is_file()
    }

    fn stems(&mut self, mix: &Sound, choice: StemChoice, progress: Progress) -> Result<Vec<Stem>, String> {
        let files = match choice {
            StemChoice::Fast => {
                vec![self.file(STEMS, "htdemucs.onnx", "the model that splits a recording into stems")?]
            }
            StemChoice::Best => (0..4)
                .map(|index| {
                    self.file(
                        STEMS_BEST,
                        &format!("htdemucs_ft_{index}.onnx"),
                        "the four models that split a recording into stems at their best",
                    )
                })
                .collect::<Result<Vec<_>, _>>()?,
        };
        // Each made ready when its turn comes, and let go of after it.
        demucs::separate(files.len(), &mut |graph| self.session(&files[graph]), mix, progress)
    }

    fn beats(&mut self, mix: &[f32], progress: Progress) -> Result<TrackedBeats, String> {
        let mut session = self.session(&self.file(BEATS, "beat_this.onnx", "the model that finds the beat")?)?;
        beat::track(&mut session, mix, progress)
    }

    fn words(&mut self, vocal: &[f32], language: Option<&str>, progress: Progress) -> Result<Transcript, String> {
        // Half precision is a card's; a processor takes the same model in whole numbers.
        let (folder, kind, what) = if self.on_processor() {
            (WORDS_PROCESSOR, "int8", "the model that writes down the words, as a processor takes it,")
        } else {
            (WORDS, "fp16", "the model that writes down the words")
        };
        // What the transcriber is handed is where a model hears a voice.
        // Where that part was not fetched, it is where the vocal is loud.
        let detector = self.setup.parts.join(VOICE).join("silero_vad_v6.onnx");
        let sung = match detector.is_file() {
            true => {
                let mut session = self.session_on_processor(&detector)?;
                let heard = trill_listen::signal::resample(vocal, trill_listen::RATE, voice::RATE);
                let likely = voice::likelihoods(&mut session, &heard)?;
                Some(voice::stretches(&likely, heard.len(), voice::Rule::default()))
            }
            false => None,
        };
        let encoder = self.session(&self.file(folder, &format!("encoder_model_{kind}.onnx"), what)?)?;
        let decoder = self.session(&self.file(folder, &format!("decoder_model_{kind}.onnx"), what)?)?;
        let mut whisper = whisper::Whisper::new(encoder, decoder, &self.setup.parts.join(folder))?;
        whisper.transcribe(vocal, sung, language, progress)
    }

    fn word_times(&mut self, vocal: &[f32], words: &mut [Word], progress: Progress) -> Result<bool, String> {
        let what = "the model that places the words";
        let tokens = self.file(WORD_TIMES, "tokens.txt", what)?;
        let mut session = self.session(&self.file(WORD_TIMES, "model.onnx", what)?)?;
        align::place(&mut session, &tokens, vocal, words, progress)
    }
}
