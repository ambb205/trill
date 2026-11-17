//! Whisper: a network that writes down what is sung.
//!
//! The model is two graphs. The encoder hears thirty seconds at a time,
//! as 128 bands a hundred times a second, and the decoder writes them
//! down one piece of a word after another, with a time marked before and
//! after each phrase. What is here is everything around them: what the
//! model is fed, the loop that asks the decoder for the next piece, and
//! the rules that keep what it writes a transcript.
//!
//! **Against invented words.** A transcriber that is handed silence makes
//! lines up. So it is handed none: the vocal is cut down to the stretches
//! in which something is sung (it is an isolated vocal, and silent where
//! nobody sings), and the times it answers with are put back where they
//! were. A window the model itself takes for silence is passed over, and
//! one whose text goes round in circles, or that the model was unsure of,
//! is written again less strictly. Words with nothing sung under them
//! are dropped later, when each word has its place ([`crate::run`]).
//!
//! Nothing is carried from one window to the next: a line that was heard
//! wrong does not pull the following ones after it.

use crate::lyrics::Envelope;
use crate::models::{Progress, Said, Stretch, Transcript};
use flate2::Compression;
use flate2::write::ZlibEncoder;
use ort::session::Session;
use ort::value::TensorRef;
use std::collections::HashMap;
use std::io::Write;
use std::path::Path;
use trill_listen::RATE;
use trill_listen::signal::{percentile_of, resample};
use trill_listen::spectrum::MelBands;

/// The rate the model listens at.
const MODEL_RATE: f64 = 16_000.0;
const SIZE: usize = 400;
const HOP: usize = 160;
const BANDS: usize = 128;
/// The frames of one window: thirty seconds.
const WINDOW: usize = 3000;
/// The most pieces a window is written in.
const LONGEST: usize = 224;
/// Seconds to one step of the model's clock.
const TICK: f64 = 0.02;
/// A window whose text packs down further than this goes round in circles.
const REPEATS: f64 = 2.4;
/// A window the model was less sure of than this, on average, is written again.
const UNSURE: f64 = -1.0;
/// A window the model takes for silence with at least this likelihood,
/// and was unsure of, is passed over.
const SILENCE: f32 = 0.6;
/// How strictly a window is written: the likeliest piece every time, and
/// then with more and more left to chance.
const TEMPERATURES: [f32; 3] = [0.0, 0.2, 0.4];

/// What the model is fed: for each frame, a hundred a second, the level of
/// each band, and the count of frames. The audio is followed by thirty
/// seconds of silence, as the model was trained to hear its last window.
fn log_mel(audio: &[f32]) -> (Vec<f32>, usize) {
    let mut padded = Vec::with_capacity(audio.len() + WINDOW * HOP);
    padded.extend_from_slice(audio);
    padded.resize(audio.len() + WINDOW * HOP, 0.0);
    let frames = padded.len() / HOP;
    let window: Vec<f32> =
        (0..SIZE).map(|i| (0.5 - 0.5 * (std::f64::consts::TAU * i as f64 / SIZE as f64).cos()) as f32).collect();
    let (cos, sin): (Vec<f32>, Vec<f32>) = (0..SIZE)
        .map(|i| (std::f64::consts::TAU * i as f64 / SIZE as f64).sin_cos())
        .map(|(sin, cos)| (cos as f32, sin as f32))
        .unzip();
    let bands = MelBands::new(MODEL_RATE, SIZE, BANDS, 0.0, MODEL_RATE / 2.0);
    let mut out = vec![0.0f32; frames * BANDS];
    let last = padded.len() as isize - 1;
    let threads = std::thread::available_parallelism().map_or(1, |count| count.get()).min(16);
    let share = frames.div_ceil(threads).max(1);
    std::thread::scope(|scope| {
        for (part, rows) in out.chunks_mut(share * BANDS).enumerate() {
            let (padded, window, cos, sin, bands) = (&padded, &window, &cos, &sin, &bands);
            scope.spawn(move || {
                let (mut frame, mut power) = (vec![0.0f32; SIZE], vec![0.0f32; SIZE / 2 + 1]);
                for (index, row) in rows.chunks_mut(BANDS).enumerate() {
                    let centre = ((part * share + index) * HOP) as isize;
                    for (offset, slot) in frame.iter_mut().enumerate() {
                        // What lies beyond either end is the signal folded back.
                        let mut at = centre - (SIZE / 2) as isize + offset as isize;
                        while at < 0 || at > last {
                            at = if at < 0 { -at } else { 2 * last - at };
                        }
                        *slot = padded[at as usize] * window[offset];
                    }
                    // The transform of 400 samples, which is not a power of
                    // two: by its definition, with the waves from a table.
                    for (bin, slot) in power.iter_mut().enumerate() {
                        let (mut re, mut im) = (0.0f32, 0.0f32);
                        let mut turn = 0usize;
                        for &sample in &frame {
                            re += sample * cos[turn];
                            im -= sample * sin[turn];
                            turn += bin;
                            if turn >= SIZE {
                                turn -= SIZE;
                            }
                        }
                        *slot = re * re + im * im;
                    }
                    bands.apply(&power, row);
                }
            });
        }
    });
    // Tenfold steps of power, nothing more than eight of them under the
    // loudest, on the scale the model was trained on.
    out.iter_mut().for_each(|value| *value = value.max(1e-10).log10());
    let top = out.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    out.iter_mut().for_each(|value| *value = (value.max(top - 8.0) + 4.0) / 4.0);
    (out, frames)
}

/// The bytes a piece of the vocabulary stands for. The vocabulary is
/// written with one character for each byte: the printable ones as
/// themselves, the rest moved up out of the way.
fn bytes_of(piece: &str) -> Vec<u8> {
    let printable =
        |byte: u32| (33..=126).contains(&byte) || (161..=172).contains(&byte) || (174..=255).contains(&byte);
    piece
        .chars()
        .filter_map(|letter| {
            let code = letter as u32;
            if code < 256 && printable(code) {
                return Some(code as u8);
            }
            // The n-th byte that is not printable was written as 256 + n.
            let moved = code.checked_sub(256)? as usize;
            (0..=255u32).filter(|&byte| !printable(byte)).nth(moved).map(|byte| byte as u8)
        })
        .collect()
}

/// How far `text` packs down: text that goes round in circles packs well.
fn packs_down(text: &str) -> f64 {
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    let _ = encoder.write_all(text.as_bytes());
    let packed = encoder.finish().map_or(text.len(), |bytes| bytes.len());
    text.len() as f64 / packed.max(1) as f64
}

fn log_softmax(logits: &mut [f32]) {
    let top = logits.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let total = logits.iter().map(|&value| (value - top).exp()).sum::<f32>().ln() + top;
    logits.iter_mut().for_each(|value| *value -= total);
}

/// One window as the decoder wrote it.
struct Written {
    /// The pieces after the prompt, with how likely each was.
    pieces: Vec<(usize, f32)>,
    /// How sure the model was, on average: the mean log of the likelihoods.
    sureness: f64,
    /// How likely the model takes the window for silence.
    silence: f32,
}

pub struct Whisper {
    encoder: Session,
    decoder: Session,
    /// The bytes of every piece of the vocabulary, by its number.
    pieces: Vec<Vec<u8>>,
    end: usize,
    start: usize,
    transcribe: usize,
    silence: usize,
    /// The first of the pieces that are times: `<|0.00|>`.
    times: usize,
    languages: Vec<(String, usize)>,
    /// Pieces that are never written: sounds that are not speech, and the
    /// model's own marks.
    never: Vec<usize>,
    /// Pieces a window may not begin with.
    never_first: Vec<usize>,
    states: usize,
}

fn json(folder: &Path, name: &str) -> Result<serde_json::Value, String> {
    let path = folder.join(name);
    let text = std::fs::read_to_string(&path).map_err(|error| format!("{}: {error}", path.display()))?;
    serde_json::from_str(&text).map_err(|error| format!("{} cannot be read: {error}", path.display()))
}

impl Whisper {
    /// The model whose graphs are made ready, with its vocabulary in `folder`.
    pub fn new(encoder: Session, decoder: Session, folder: &Path) -> Result<Self, String> {
        let (vocabulary, added, settings) =
            (json(folder, "vocab.json")?, json(folder, "added_tokens.json")?, json(folder, "generation_config.json")?);
        let mut numbers: HashMap<String, usize> = HashMap::new();
        for table in [&vocabulary, &added] {
            for (piece, number) in table.as_object().into_iter().flatten() {
                numbers.insert(piece.clone(), number.as_u64().unwrap_or(0) as usize);
            }
        }
        let count = numbers.values().max().map_or(0, |most| most + 1);
        let mut pieces = vec![Vec::new(); count];
        for (piece, number) in vocabulary.as_object().into_iter().flatten() {
            pieces[number.as_u64().unwrap_or(0) as usize] = bytes_of(piece);
        }
        let mark = |name: &str| {
            numbers.get(name).copied().ok_or_else(|| {
                format!(
                    "the transcriber's vocabulary in {} has no {name}: is it the one the setup fetched?",
                    folder.display()
                )
            })
        };
        let list = |name: &str| -> Vec<usize> {
            settings[name]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|number| number.as_u64())
                .map(|number| number as usize)
                .collect()
        };
        let (start, times) = (mark("<|startoftranscript|>")?, mark("<|0.00|>")?);
        let mut never = list("suppress_tokens");
        // Its own marks, from the start of a transcript up to the times.
        never.extend(start..times);
        let mut languages: Vec<(String, usize)> = settings["lang_to_id"]
            .as_object()
            .into_iter()
            .flatten()
            .filter_map(|(name, number)| {
                Some((name.trim_start_matches("<|").trim_end_matches("|>").to_owned(), number.as_u64()? as usize))
            })
            .collect();
        languages.sort_by_key(|(_, number)| *number);
        let states = match decoder
            .inputs()
            .iter()
            .find(|input| input.name() == "encoder_hidden_states")
            .map(|input| input.dtype())
        {
            Some(ort::value::ValueType::Tensor { shape, .. }) => {
                shape.iter().last().copied().filter(|&size| size > 0).unwrap_or(1280) as usize
            }
            _ => 1280,
        };
        Ok(Self {
            encoder,
            decoder,
            pieces,
            end: mark("<|endoftext|>")?,
            start,
            transcribe: mark("<|transcribe|>")?,
            silence: mark("<|nospeech|>")?,
            times,
            languages,
            never,
            never_first: list("begin_suppress_tokens"),
            states,
        })
    }

    /// What the encoder makes of one window of the model's input.
    fn hear(&mut self, mel: &[f32], frames: usize, seek: usize) -> Result<Vec<f32>, String> {
        // Band by band, as the model takes it; past the end is silence.
        let mut features = vec![0.0f32; BANDS * WINDOW];
        for frame in 0..WINDOW.min(frames.saturating_sub(seek)) {
            let row = &mel[(seek + frame) * BANDS..(seek + frame + 1) * BANDS];
            for (band, &level) in row.iter().enumerate() {
                features[band * WINDOW + frame] = level;
            }
        }
        let input = self.encoder.inputs()[0].name().to_owned();
        let outputs = self
            .encoder
            .run(ort::inputs![input.as_str() => super::tensor(&[1, BANDS, WINDOW], features, BANDS * WINDOW)?])
            .map_err(|error| format!("the model that writes down the words could not hear: {error}"))?;
        let (_, heard) = outputs[0].try_extract_tensor::<f32>().map_err(|error| error.to_string())?;
        if heard.len() != WINDOW / 2 * self.states {
            return Err("the model that writes down the words answered in a shape that is not its own: is its file the one the setup fetched?".to_owned());
        }
        Ok(heard.to_vec())
    }

    /// The decoder's word on what follows `written`: for the piece after
    /// the last, and for the one after the first.
    fn next(&mut self, written: &[usize], heard: &[f32]) -> Result<(Vec<f32>, Vec<f32>), String> {
        if written.is_empty() || written.len() > 448 {
            return Err(format!(
                "the model that writes down the words was about to be handed {} pieces, where at most 448 are its own: refused",
                written.len()
            ));
        }
        let ids: Vec<i64> = written.iter().map(|&piece| piece as i64).collect();
        let cannot = |error: ort::Error| format!("the model that writes down the words could not write: {error}");
        let outputs = self
            .decoder
            .run(ort::inputs![
                "input_ids" => TensorRef::from_array_view((vec![1i64, ids.len() as i64], &ids[..])).map_err(cannot)?,
                "encoder_hidden_states" => TensorRef::from_array_view((vec![1i64, (WINDOW / 2) as i64, self.states as i64], heard)).map_err(cannot)?,
            ])
            .map_err(cannot)?;
        let (_, logits) = outputs["logits"].try_extract_tensor::<f32>().map_err(|error| error.to_string())?;
        let size = logits.len() / written.len();
        if size != self.pieces.len() {
            return Err("the model that writes down the words has another vocabulary than the one beside it: fetch its part again".to_owned());
        }
        Ok((logits[(written.len() - 1) * size..].to_vec(), logits[..size].to_vec()))
    }

    /// The language sung in a window: the one the model would name first.
    fn language(&mut self, heard: &[f32]) -> Result<(String, usize), String> {
        let (logits, _) = self.next(&[self.start], heard)?;
        self.languages
            .iter()
            .max_by(|a, b| logits[a.1].total_cmp(&logits[b.1]))
            .cloned()
            .ok_or_else(|| "the transcriber's settings name no languages: fetch its part again".to_owned())
    }

    /// Keeps `logits`, for the piece after `written`, to what a transcript
    /// may hold there: times come in pairs and never go back, and a window
    /// begins with one.
    fn rules(&self, logits: &mut [f32], written: &[usize]) {
        let low = f32::NEG_INFINITY;
        for &piece in &self.never {
            logits[piece] = low;
        }
        if written.is_empty() {
            for &piece in &self.never_first {
                logits[piece] = low;
            }
        }
        let is_time = |piece: usize| piece >= self.times;
        let last = written.last().copied().is_some_and(is_time);
        let before = written.len() < 2 || is_time(written[written.len() - 2]);
        if last {
            if before {
                // After a pair of times comes text.
                logits[self.times..].fill(low);
            } else {
                // After text and a time comes the time that closes the pair, or the end.
                logits[..self.end].fill(low);
            }
        }
        if let Some(latest) = written.iter().rev().copied().find(|&piece| is_time(piece)) {
            // Times do not go back, and a phrase has a length.
            let from = if last && !before { latest } else { latest + 1 };
            let to = from.min(logits.len());
            logits[self.times..to].fill(low);
        }
        if written.is_empty() {
            // A window begins with a time, within its first second.
            logits[..self.times].fill(low);
            let latest = self.times + 50;
            let from = (latest + 1).min(logits.len());
            logits[from..].fill(low);
        }
        // Where a time is likelier than any one piece of text, it is a time.
        let mut likelihood = logits.to_vec();
        log_softmax(&mut likelihood);
        let top = likelihood[self.times..].iter().copied().fold(low, f32::max);
        let time = if top == low {
            low
        } else {
            likelihood[self.times..].iter().map(|&value| (value - top).exp()).sum::<f32>().ln() + top
        };
        if time > likelihood[..self.times].iter().copied().fold(low, f32::max) {
            logits[..self.times].fill(low);
        }
    }

    /// Writes one window down. With a `temperature` over nothing, each
    /// piece is drawn by lot among the likely ones; `seed` says which lot.
    fn write(&mut self, heard: &[f32], language: usize, temperature: f32, mut seed: u32) -> Result<Written, String> {
        let mut prompt = vec![self.start, language, self.transcribe];
        let begun = prompt.len();
        let (mut pieces, mut sum, mut silence) = (Vec::new(), 0.0f64, 0.0f32);
        for step in 0..LONGEST {
            let (mut logits, mut first) = self.next(&prompt, heard)?;
            if step == 0 {
                log_softmax(&mut first);
                silence = first[self.silence].exp();
            }
            self.rules(&mut logits, &prompt[begun..]);
            let mut likelihood = logits.clone();
            log_softmax(&mut likelihood);
            let chosen = if temperature <= 0.0 {
                (0..likelihood.len())
                    .max_by(|&a, &b| likelihood[a].total_cmp(&likelihood[b]).then(b.cmp(&a)))
                    .unwrap_or(self.end)
            } else {
                // By lot: each piece as likely as the model has it, sharpened.
                let mut sharpened: Vec<f32> = logits.iter().map(|&value| value / temperature).collect();
                log_softmax(&mut sharpened);
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                let mut lot = (seed >> 8) as f32 / 16_777_216.0;
                let mut drawn = self.end;
                for (piece, &value) in sharpened.iter().enumerate() {
                    lot -= value.exp();
                    if lot <= 0.0 {
                        drawn = piece;
                        break;
                    }
                }
                drawn
            };
            sum += f64::from(likelihood[chosen]);
            if chosen == self.end {
                break;
            }
            pieces.push((chosen, likelihood[chosen].exp()));
            prompt.push(chosen);
        }
        Ok(Written { sureness: sum / (pieces.len() + 1) as f64, pieces, silence })
    }

    /// The text of pieces that are text.
    fn text(&self, pieces: &[(usize, f32)]) -> String {
        let bytes: Vec<u8> = pieces
            .iter()
            .filter(|(piece, _)| *piece < self.end)
            .flat_map(|(piece, _)| self.pieces[*piece].iter().copied())
            .collect();
        String::from_utf8_lossy(&bytes).into_owned()
    }

    /// The words of pieces that are text, each with the mean likelihood of
    /// its pieces: a piece that begins with a space begins a word.
    fn words(&self, pieces: &[(usize, f32)]) -> Vec<Said> {
        let mut words: Vec<(Vec<u8>, Vec<f32>)> = Vec::new();
        for &(piece, likelihood) in pieces.iter().filter(|(piece, _)| *piece < self.end) {
            let bytes = &self.pieces[piece];
            if bytes.first() == Some(&b' ') || words.is_empty() {
                words.push((Vec::new(), Vec::new()));
            }
            let word = words.last_mut().expect("just made sure of one");
            word.0.extend_from_slice(bytes);
            word.1.push(likelihood);
        }
        words
            .into_iter()
            .map(|(bytes, likelihoods)| Said {
                text: String::from_utf8_lossy(&bytes).trim().to_owned(),
                p: ((likelihoods.iter().sum::<f32>() / likelihoods.len().max(1) as f32) * 1000.0).round() / 1000.0,
            })
            .filter(|word| !word.text.is_empty())
            .collect()
    }

    /// The words sung in `vocal`, one channel at 44.1 kHz. `sung` is where
    /// someone sings, in seconds, as a model heard it; without one, it is
    /// taken to be wherever the vocal is loud.
    pub fn transcribe(
        &mut self,
        vocal: &[f32],
        sung: Option<Vec<(f64, f64)>>,
        language: Option<&str>,
        progress: Progress,
    ) -> Result<Transcript, String> {
        // Only what is sung: stretches of the vocal, laid end to end, with
        // where each came from.
        let stretches = sung.unwrap_or_else(|| Envelope::new(vocal).sung_stretches(0.7, 0.3));
        let speech = resample(vocal, RATE, MODEL_RATE);
        let mut stream: Vec<f32> = Vec::new();
        // For each stretch: where it begins in the stream, and where in the vocal.
        let mut from: Vec<(f64, f64)> = Vec::new();
        for &(start, end) in &stretches {
            let (first, last) = ((start * MODEL_RATE) as usize, ((end * MODEL_RATE) as usize).min(speech.len()));
            if last > first {
                from.push((stream.len() as f64 / MODEL_RATE, start));
                stream.extend_from_slice(&speech[first..last]);
            }
        }
        if stream.len() < (0.5 * MODEL_RATE) as usize {
            return Ok(Transcript { language: language.map(str::to_owned), stretches: Vec::new() });
        }
        let restore = |seconds: f64| -> f64 {
            let stretch = from
                .iter()
                .rev()
                .find(|(begins, _)| *begins <= seconds + 1e-9)
                .or(from.first())
                .copied()
                .unwrap_or((0.0, 0.0));
            stretch.1 + (seconds - stretch.0)
        };
        // Sung vocals are quieter and far more dynamic than speech: the
        // level is brought up to where the model was trained to listen.
        let peak = percentile_of(&stream.iter().map(|sample| sample.abs()).collect::<Vec<f32>>(), 99.9) as f32;
        if peak > 0.0 {
            stream.iter_mut().for_each(|sample| *sample = (*sample * 0.7 / peak).clamp(-1.0, 1.0));
        }

        let (mel, frames) = log_mel(&stream);
        let content = frames - WINDOW;
        let mut chosen: Option<(String, usize)> = match language {
            Some(wanted) => Some(self.languages.iter().find(|(name, _)| name == wanted).cloned().ok_or_else(|| {
                format!("the transcriber does not know the language '{wanted}': it takes two letters, such as en, es, de, ja")
            })?),
            None => None,
        };
        let mut found: Vec<Stretch> = Vec::new();
        let mut seek = 0usize;
        // No more windows than the audio has hundredths of a second: every one moves on.
        for window in 0..content.max(1) {
            if seek >= content {
                break;
            }
            progress(seek as f32 / content as f32);
            let heard = self.hear(&mel, frames, seek)?;
            let language = match &chosen {
                Some((_, number)) => *number,
                None => {
                    let told = self.language(&heard)?;
                    chosen = Some(told.clone());
                    told.1
                }
            };
            // Written strictly first, and again with more left to chance
            // where it went round in circles or the model was unsure.
            let mut written = self.write(&heard, language, TEMPERATURES[0], 0)?;
            for (attempt, &temperature) in TEMPERATURES.iter().enumerate().skip(1) {
                let quiet = written.silence > SILENCE && written.sureness < UNSURE;
                if quiet || (packs_down(&self.text(&written.pieces)) <= REPEATS && written.sureness >= UNSURE) {
                    break;
                }
                written = self.write(
                    &heard,
                    language,
                    temperature,
                    0x9E37_79B9 ^ (window as u32).wrapping_mul(31).wrapping_add(attempt as u32),
                )?;
            }
            let span = WINDOW.min(content - seek);
            if written.silence > SILENCE && written.sureness < UNSURE {
                seek += span;
                continue;
            }
            let offset = seek as f64 * HOP as f64 / MODEL_RATE;
            let is_time = |piece: usize| piece >= self.times;
            let pieces = &written.pieces;
            let ends_on_one_time =
                pieces.len() >= 2 && !is_time(pieces[pieces.len() - 2].0) && is_time(pieces[pieces.len() - 1].0)
                    || pieces.len() == 1 && is_time(pieces[0].0);
            // A phrase lies between two times; where two times follow one
            // another, one phrase ends and the next begins.
            let mut cuts: Vec<usize> =
                (1..pieces.len()).filter(|&at| is_time(pieces[at - 1].0) && is_time(pieces[at].0)).collect();
            let phrase = |slice: &[(usize, f32)], start: f64, end: f64, found: &mut Vec<Stretch>| {
                let words = self.words(slice);
                if !words.is_empty() && end > start {
                    found.push(Stretch {
                        start: (restore(start) * 100.0).round() / 100.0,
                        end: (restore(end) * 100.0).round() / 100.0,
                        words,
                    });
                }
            };
            if cuts.is_empty() {
                // One phrase for the window: to the last time it names, or its end.
                let named = pieces
                    .iter()
                    .rev()
                    .find(|(piece, _)| is_time(*piece))
                    .map(|(piece, _)| (piece - self.times) as f64 * TICK)
                    .filter(|&time| time > 0.0);
                phrase(pieces, offset, offset + named.unwrap_or(span as f64 * HOP as f64 / MODEL_RATE), &mut found);
                seek += span;
            } else {
                if ends_on_one_time {
                    cuts.push(pieces.len());
                }
                let mut begin = 0usize;
                for &cut in &cuts {
                    let slice = &pieces[begin..cut];
                    let (first, last) = (slice[0].0, slice[slice.len() - 1].0);
                    if is_time(first) && is_time(last) {
                        phrase(
                            slice,
                            offset + (first - self.times) as f64 * TICK,
                            offset + (last - self.times) as f64 * TICK,
                            &mut found,
                        );
                    }
                    begin = cut;
                }
                if ends_on_one_time {
                    seek += span;
                } else {
                    // On to the last time that closed a phrase: what follows it was cut off by the window's end.
                    let reached = (pieces[begin - 1].0 - self.times) * 2;
                    seek += reached.clamp(1, span);
                }
            }
        }
        progress(1.0);
        Ok(Transcript { language: chosen.map(|(name, _)| name), stretches: found })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pieces_of_the_vocabulary_are_the_bytes_they_stand_for() {
        // A space is written as the character 288, a newline as 266.
        assert_eq!(bytes_of("\u{120}hello"), b" hello");
        assert_eq!(bytes_of("\u{10a}"), b"\n");
        assert_eq!(bytes_of("don't"), b"don't");
        // Two bytes of one letter, each written as its own character.
        assert_eq!(String::from_utf8(bytes_of("\u{c3}\u{a9}")).unwrap(), "\u{e9}");
    }

    #[test]
    fn text_that_goes_round_in_circles_packs_down_and_a_line_of_a_song_does_not() {
        assert!(packs_down(&"la la la la ".repeat(20)) > REPEATS);
        assert!(packs_down("I remember when the nights were long and the river ran by") < REPEATS);
    }

    #[test]
    fn what_the_model_is_fed_is_a_hundred_frames_a_second_with_a_window_of_silence_after() {
        let tone: Vec<f32> =
            (0..16_000).map(|i| (i as f64 * std::f64::consts::TAU * 1000.0 / 16_000.0).sin() as f32 * 0.5).collect();
        let (mel, frames) = log_mel(&tone);
        assert_eq!(frames, 100 + WINDOW);
        assert_eq!(mel.len(), frames * BANDS);
        // The tone is the loudest thing there is, in a band a little under the middle.
        let row = &mel[50 * BANDS..51 * BANDS];
        let loudest = (0..BANDS).max_by(|&a, &b| row[a].total_cmp(&row[b])).unwrap();
        assert!((40..60).contains(&loudest), "{loudest}");
        assert!((row[loudest] - mel.iter().copied().fold(f32::NEG_INFINITY, f32::max)).abs() < 0.2);
        // And the silence after is as far under it as the scale goes: two whole steps.
        let silent = mel[(frames - 1) * BANDS];
        assert!((row[loudest] - silent - 2.0).abs() < 0.05, "{} against {silent}", row[loudest]);
    }
}
