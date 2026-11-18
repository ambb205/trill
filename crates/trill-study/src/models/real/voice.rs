//! Where someone sings: the stretches of a vocal that a small model
//! takes for a voice (Silero VAD), which is what the transcriber is then
//! handed and nothing else.
//!
//! A transcriber writes something down for whatever it is handed. Of an
//! isolated vocal that is mostly right, but a stem is not silent where
//! nobody sings: what bleeds into it, a breath, a chopped sample, a
//! scream are loud enough to pass any gate by loudness, and the model
//! makes words of them. A model that was trained to tell a voice from
//! everything else is the better judge, and it is small: a megabyte, and
//! a few seconds on a processor for a song. It always runs on the
//! processor.
//!
//! The model, the way it is run and the rule that turns its likelihoods
//! into stretches are those of the transcriber Magpie uses (faster-whisper,
//! which adapted them from Silero's own): a likelihood for every 32 ms,
//! each window heard with the last 4 ms of the one before it.

use super::tensor;
use ort::session::Session;

/// The rate the model listens at.
pub const RATE: f64 = 16_000.0;
/// The samples of one window, and how many of the window before it are
/// heard with it.
const WINDOW: usize = 512;
const CONTEXT: usize = 64;
/// The size of the model's memory.
const STATE: usize = 128;
/// How many windows the model is handed at once: five minutes.
const AT_ONCE: usize = 10_000;

/// How the likelihoods become stretches.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Rule {
    /// A window at least this likely a voice begins a stretch.
    pub begins: f32,
    /// A window less likely than this, within a stretch, may end it.
    pub ends: f32,
    /// A stretch ends where the voice is away for this long, in seconds.
    pub silence: f64,
    /// Each stretch is given this much room on either side, in seconds.
    pub room: f64,
}

impl Default for Rule {
    /// As Magpie asks: seven tenths of a second of silence between two
    /// stretches, and three tenths of room around each.
    fn default() -> Self {
        Self { begins: 0.5, ends: 0.35, silence: 0.7, room: 0.3 }
    }
}

/// How likely each window of `audio` (one channel at [`RATE`]) is a voice.
pub fn likelihoods(session: &mut Session, audio: &[f32]) -> Result<Vec<f32>, String> {
    let windows = audio.len().div_ceil(WINDOW);
    let sample = |index: usize| audio.get(index).copied().unwrap_or(0.0);
    let names: Vec<String> = session.outputs().iter().map(|outlet| outlet.name().to_owned()).collect();
    if names.len() != 3 || session.inputs().len() != 3 {
        return Err("the model that hears where someone sings is not the one the setup fetched".to_owned());
    }
    let cannot = |error: String| format!("the model that hears where someone sings could not be run: {error}");
    let (mut h, mut c) = (vec![0.0f32; STATE], vec![0.0f32; STATE]);
    let mut found = Vec::with_capacity(windows);
    let mut first = 0usize;
    // No more turns than there are windows: every one takes at least one.
    for _ in 0..windows.max(1) {
        if first >= windows {
            break;
        }
        let count = AT_ONCE.min(windows - first);
        let mut input = Vec::with_capacity(count * (CONTEXT + WINDOW));
        for window in first..first + count {
            let begin = window * WINDOW;
            // The end of the window before; nothing before the first.
            input.extend((0..CONTEXT).map(|index| if window == 0 { 0.0 } else { sample(begin - CONTEXT + index) }));
            input.extend((0..WINDOW).map(|index| sample(begin + index)));
        }
        let outputs = session
            .run(ort::inputs![
                "input" => tensor(&[count, CONTEXT + WINDOW], input, AT_ONCE * (CONTEXT + WINDOW))?,
                "h" => tensor(&[1, 1, STATE], h.clone(), STATE)?,
                "c" => tensor(&[1, 1, STATE], c.clone(), STATE)?,
            ])
            .map_err(|error| cannot(error.to_string()))?;
        let read = |name: &str| -> Result<Vec<f32>, String> {
            outputs[name]
                .try_extract_tensor::<f32>()
                .map(|(_, data)| data.to_vec())
                .map_err(|error| cannot(error.to_string()))
        };
        let likely = read(&names[0])?;
        if likely.len() != count {
            return Err(cannot("it answered in a shape that is not its own".to_owned()));
        }
        found.extend(likely);
        (h, c) = (read(&names[1])?, read(&names[2])?);
        if h.len() != STATE || c.len() != STATE {
            return Err(cannot("it answered in a shape that is not its own".to_owned()));
        }
        first += count;
    }
    Ok(found)
}

/// The stretches of a recording `samples` long, in seconds, that `likely`
/// (a likelihood for each window) says a voice is in.
pub fn stretches(likely: &[f32], samples: usize, rule: Rule) -> Vec<(f64, f64)> {
    let at = |window: usize| window * WINDOW;
    let silence = (rule.silence * RATE) as usize;
    let room = (rule.room * RATE) as usize;
    let mut found: Vec<(usize, usize)> = Vec::new();
    let mut begun: Option<usize> = None;
    // Where the voice was last heard to leave, while it may still come back.
    let mut left: Option<usize> = None;
    for (window, &voice) in likely.iter().enumerate() {
        if voice >= rule.begins {
            left = None;
            if begun.is_none() {
                begun = Some(at(window));
                continue;
            }
        }
        if let Some(start) = begun
            && voice < rule.ends
        {
            let since = *left.get_or_insert(at(window));
            if at(window) - since >= silence {
                if since > start {
                    found.push((start, since));
                }
                (begun, left) = (None, None);
            }
        }
    }
    if let Some(start) = begun
        && samples > start
    {
        found.push((start, samples));
    }
    // Room around each; two that are nearer than twice the room meet halfway.
    let count = found.len();
    for index in 0..count {
        if index == 0 {
            found[0].0 = found[0].0.saturating_sub(room);
        }
        if index + 1 < count {
            let between = found[index + 1].0.saturating_sub(found[index].1);
            if between < 2 * room {
                found[index].1 += between / 2;
                found[index + 1].0 = found[index + 1].0.saturating_sub(between / 2);
            } else {
                found[index].1 = (found[index].1 + room).min(samples);
                found[index + 1].0 = found[index + 1].0.saturating_sub(room);
            }
        } else {
            found[index].1 = (found[index].1 + room).min(samples);
        }
    }
    found.into_iter().map(|(start, end)| (start as f64 / RATE, end as f64 / RATE)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A likelihood for each window of `seconds`, by whether a voice is there.
    fn likely(seconds: f64, voice: impl Fn(f64) -> bool) -> (Vec<f32>, usize) {
        let samples = (seconds * RATE) as usize;
        let windows = samples.div_ceil(WINDOW);
        (
            (0..windows).map(|window| if voice(window as f64 * WINDOW as f64 / RATE) { 0.9 } else { 0.05 }).collect(),
            samples,
        )
    }

    #[test]
    fn a_voice_that_comes_and_goes_is_stretches_with_room_around_them() {
        // Sung from 2 to 5 seconds and from 8 to 9.
        let (heard, samples) = likely(12.0, |time| (2.0..5.0).contains(&time) || (8.0..9.0).contains(&time));
        let found = stretches(&heard, samples, Rule::default());
        assert_eq!(found.len(), 2, "{found:?}");
        let near = |a: f64, b: f64| (a - b).abs() < 0.04;
        assert!(near(found[0].0, 1.7) && near(found[0].1, 5.3), "{found:?}");
        assert!(near(found[1].0, 7.7) && near(found[1].1, 9.3), "{found:?}");
    }

    #[test]
    fn a_short_breath_does_not_end_a_stretch_and_two_that_are_near_meet_halfway() {
        // Half a second away is the same stretch; a second away, with less than the room between, meets halfway.
        let (heard, samples) = likely(10.0, |time| (1.0..3.0).contains(&time) || (3.5..5.0).contains(&time));
        assert_eq!(stretches(&heard, samples, Rule::default()).len(), 1);
        let rule = Rule { silence: 0.3, room: 0.4, ..Rule::default() };
        let found = stretches(&heard, samples, rule);
        assert_eq!(found.len(), 2, "{found:?}");
        assert!((found[0].1 - found[1].0).abs() < 1e-9 && (found[0].1 - 3.25).abs() < 0.04, "{found:?}");
        // A voice that is still there at the end is a stretch to the end; none at all is none.
        let (heard, samples) = likely(4.0, |time| time >= 3.0);
        let found = stretches(&heard, samples, Rule::default());
        assert_eq!((found.len(), found[0].1), (1, 4.0));
        let (heard, samples) = likely(4.0, |_| false);
        assert!(stretches(&heard, samples, Rule::default()).is_empty());
    }

    #[test]
    fn a_window_between_the_two_marks_keeps_a_stretch_and_begins_none() {
        // Somewhat likely (0.4): not enough to begin a stretch, enough not to end one.
        let samples = (6.0 * RATE) as usize;
        let windows = samples.div_ceil(WINDOW);
        let time = |window: usize| window as f64 * WINDOW as f64 / RATE;
        let unsure: Vec<f32> = (0..windows).map(|_| 0.4).collect();
        assert!(stretches(&unsure, samples, Rule::default()).is_empty());
        let held: Vec<f32> = (0..windows).map(|window| if time(window) < 1.0 { 0.9 } else { 0.4 }).collect();
        let found = stretches(&held, samples, Rule::default());
        assert_eq!(found, [(0.0, 6.0)]);
    }
}
