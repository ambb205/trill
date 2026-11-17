//! Putting each transcribed word where it is sung.
//!
//! A transcriber knows the words but is loose about when. Forced
//! alignment asks a different question of a different model (given these
//! words, where in this audio is each one?) and answers it to a fiftieth
//! of a second: the model says for every moment how likely each letter
//! is, and the words are laid along the audio by the likeliest path that
//! spells them in order.
//!
//! Windows of words are placed one at a time, each with a few of its
//! neighbours' words and their audio, and with a wildcard at each end
//! that soaks up whatever else is sung there, so that a word the
//! transcriber put on the wrong side of a pause is free to move to the
//! right one.

use super::tensor;
use crate::lyrics::Word;
use crate::models::Progress;
use ort::session::Session;
use std::collections::HashMap;
use std::path::Path;
use trill_listen::RATE;
use trill_listen::signal::resample;

/// The rate the model listens at.
const MODEL_RATE: f64 = 16_000.0;
/// Words are placed in runs of about this long, cut at pauses where there are any.
const WINDOW_SECONDS: f64 = 16.0;
/// How many of its neighbours' words a window is given either side.
const CONTEXT_WORDS: usize = 3;
/// The longest stretch of audio the model is handed at once, in seconds.
const LONGEST_SECONDS: f64 = 60.0;

/// The likeliest path of `tokens` through `emission` (frames by
/// vocabulary, logs of likelihoods), with the model's blank between: the
/// first and last frame of each token. A token numbered `vocabulary` is a
/// wildcard that costs nothing wherever it lies.
pub fn best_path(emission: &[f32], vocabulary: usize, tokens: &[usize], blank: usize) -> Option<Vec<(usize, usize)>> {
    let frames = emission.len() / vocabulary.max(1);
    let states = 2 * tokens.len() + 1;
    if frames < tokens.len() || tokens.is_empty() {
        return None;
    }
    let symbol = |state: usize| if state.is_multiple_of(2) { blank } else { tokens[state / 2] };
    let cost = |row: &[f32], state: usize| if symbol(state) >= vocabulary { 0.0 } else { row[symbol(state)] };
    let low = f32::NEG_INFINITY;
    let mut score = vec![low; states];
    let mut back = vec![0u8; frames * states];
    score[0] = emission[blank];
    score[1] = cost(&emission[..vocabulary], 1);
    for frame in 1..frames {
        let row = &emission[frame * vocabulary..(frame + 1) * vocabulary];
        let mut next = vec![low; states];
        for state in 0..states {
            let (mut best, mut step) = (score[state], 0u8);
            if state >= 1 && score[state - 1] > best {
                (best, step) = (score[state - 1], 1);
            }
            // A blank may be skipped between two different letters.
            if state >= 2 && state % 2 == 1 && symbol(state) != symbol(state - 2) && score[state - 2] > best {
                (best, step) = (score[state - 2], 2);
            }
            if best > low {
                next[state] = best + cost(row, state);
                back[frame * states + state] = step;
            }
        }
        score = next;
    }
    let mut state = if score[states - 1] >= score[states - 2] { states - 1 } else { states - 2 };
    if score[state] == low {
        return None;
    }
    let mut spans = vec![(usize::MAX, 0usize); tokens.len()];
    for frame in (0..frames).rev() {
        if state % 2 == 1 {
            let token = state / 2;
            spans[token] = (spans[token].0.min(frame), spans[token].1.max(frame));
        }
        state -= usize::from(back[frame * states + state]);
    }
    spans.iter().all(|span| span.0 != usize::MAX).then_some(spans)
}

/// Cuts the words into runs of about [`WINDOW_SECONDS`], at pauses where
/// there are any.
fn windows(words: &[Word]) -> Vec<(usize, usize)> {
    let mut found = Vec::new();
    let mut first = 0;
    for index in 1..words.len() {
        let length = words[index].end - words[first].start;
        let gap = words[index].start - words[index - 1].end;
        if (length > WINDOW_SECONDS && gap > 0.3) || length > WINDOW_SECONDS * 1.6 || index - first >= 60 {
            found.push((first, index));
            first = index;
        }
    }
    found.push((first, words.len()));
    found
}

/// The model's letters, each with its number.
fn letters(tokens: &Path) -> Result<HashMap<char, usize>, String> {
    let text = std::fs::read_to_string(tokens).map_err(|error| format!("{}: {error}", tokens.display()))?;
    // A letter and its number to a line; the letter may be a space.
    let found: HashMap<char, usize> = text
        .lines()
        .filter_map(|line| line.rsplit_once(' '))
        .filter_map(|(token, number)| {
            let mut letters = token.chars();
            let letter = letters.next()?;
            letters.next().is_none().then_some((letter, number.parse().ok()?))
        })
        .collect();
    if !found.contains_key(&' ') || found.len() < 20 {
        return Err(format!(
            "{} is not the list of letters of the model that places the words: fetch its part again",
            tokens.display()
        ));
    }
    Ok(found)
}

/// Places `words` in `vocal` (one channel at 44.1 kHz), starting from the
/// rough times they come with. `Ok(false)` leaves them alone: too few of
/// them are in letters the model reads.
pub fn place(
    session: &mut Session,
    tokens: &Path,
    vocal: &[f32],
    words: &mut [Word],
    progress: Progress,
) -> Result<bool, String> {
    let alphabet = letters(tokens)?;
    let size = alphabet.values().max().map_or(0, |most| most + 1);
    let (blank, space, star) = (0usize, alphabet[&' '], size);
    let spelled: Vec<Vec<usize>> = words
        .iter()
        .map(|word| {
            word.text
                .chars()
                .flat_map(char::to_lowercase)
                .filter(|letter| *letter != ' ')
                .filter_map(|letter| alphabet.get(&letter).copied())
                .collect()
        })
        .collect();
    if words.is_empty() || spelled.iter().filter(|letters| !letters.is_empty()).count() * 10 < words.len() * 6 {
        return Ok(false);
    }
    let speech = resample(vocal, RATE, MODEL_RATE);
    let duration = speech.len() as f64 / MODEL_RATE;
    let input = session.inputs()[0].name().to_owned();
    let runs = windows(words);
    for (done, &(first, last)) in runs.iter().enumerate() {
        progress(done as f32 / runs.len() as f32);
        let (begin, finish) = (first.saturating_sub(CONTEXT_WORDS), (last + CONTEXT_WORDS).min(words.len()));
        let start = (words[begin..finish].iter().map(|word| word.start).fold(f64::INFINITY, f64::min) - 0.6).max(0.0);
        let end = (words[begin..finish].iter().map(|word| word.end).fold(0.0f64, f64::max) + 0.8)
            .min(duration)
            .min(start + LONGEST_SECONDS);
        let (from, to) = ((start * MODEL_RATE) as usize, ((end * MODEL_RATE) as usize).min(speech.len()));
        if to < from + (MODEL_RATE as usize) / 4 {
            continue;
        }
        // Zero mean and unit deviation, as the model was trained to hear.
        let samples = &speech[from..to];
        let mean = samples.iter().map(|&v| f64::from(v)).sum::<f64>() / samples.len() as f64;
        let deviation = (samples.iter().map(|&v| (f64::from(v) - mean).powi(2)).sum::<f64>() / samples.len() as f64)
            .sqrt()
            .max(1e-7);
        let scaled: Vec<f32> = samples.iter().map(|&v| ((f64::from(v) - mean) / deviation) as f32).collect();

        // A wildcard, the words with a space between, a wildcard.
        let mut spelling = vec![star];
        let mut spans = Vec::new();
        for (index, letters) in spelled.iter().enumerate().take(finish).skip(begin) {
            if letters.is_empty() {
                continue;
            }
            if spelling.len() > 1 {
                spelling.push(space);
            }
            spans.push((index, spelling.len(), spelling.len() + letters.len()));
            spelling.extend(letters);
        }
        spelling.push(star);

        let count = scaled.len();
        let outputs = session
            .run(ort::inputs![input.as_str() => tensor(&[1, count], scaled, (LONGEST_SECONDS * MODEL_RATE) as usize)?])
            .map_err(|error| format!("the model that places the words could not be run: {error}"))?;
        let (shape, logits) = outputs[0].try_extract_tensor::<f32>().map_err(|error| error.to_string())?;
        if shape.len() != 3 || shape[2] as usize != size {
            return Err(
                "the model that places the words has other letters than the list beside it: fetch its part again"
                    .to_owned(),
            );
        }
        let frames = shape[1] as usize;
        if frames < spelling.len() + 2 {
            continue;
        }
        let mut emission = logits.to_vec();
        for row in emission.chunks_mut(size) {
            let top = row.iter().copied().fold(f32::NEG_INFINITY, f32::max);
            let total = row.iter().map(|&value| (value - top).exp()).sum::<f32>().ln() + top;
            row.iter_mut().for_each(|value| *value -= total);
        }
        let Some(path) = best_path(&emission, size, &spelling, blank) else { continue };
        let per_frame = count as f64 / MODEL_RATE / frames as f64;
        for (index, low, high) in spans {
            // A neighbour is here only for the others' sake.
            if index < first || index >= last {
                continue;
            }
            // How sure the model was of the word: its letters' likelihoods where they lie.
            let mut likelihood = 0.0f64;
            let mut counted = 0usize;
            for (token, span) in path[low..high].iter().enumerate() {
                for frame in span.0..=span.1 {
                    likelihood += f64::from(emission[frame * size + spelling[low + token]].exp());
                    counted += 1;
                }
            }
            words[index].start = start + path[low].0 as f64 * per_frame;
            words[index].end = start + (path[high - 1].1 + 1) as f64 * per_frame;
            words[index].a = Some(((likelihood / counted.max(1) as f64) * 1000.0).round() as f32 / 1000.0);
        }
    }
    progress(1.0);
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Frames in which each of `spoken` is certain, one after another.
    fn certain(spoken: &[usize], vocabulary: usize) -> Vec<f32> {
        let mut emission = vec![-20.0f32; spoken.len() * vocabulary];
        for (frame, &token) in spoken.iter().enumerate() {
            emission[frame * vocabulary + token] = 0.0;
        }
        emission
    }

    #[test]
    fn letters_are_laid_where_the_model_hears_them() {
        // Blank is 0. Heard: silence, 2 2, silence, 3, 2, silence.
        let emission = certain(&[0, 2, 2, 0, 3, 2, 0], 5);
        assert_eq!(best_path(&emission, 5, &[2, 3, 2], 0), Some(vec![(1, 2), (4, 4), (5, 5)]));
        // The same letter twice needs the blank between.
        let twice = certain(&[2, 0, 2], 5);
        assert_eq!(best_path(&twice, 5, &[2, 2], 0), Some(vec![(0, 0), (2, 2)]));
        // More letters than moments cannot be laid at all.
        assert_eq!(best_path(&certain(&[2], 5), 5, &[2, 3], 0), None);
    }

    #[test]
    fn a_wildcard_takes_whatever_else_is_there() {
        // Something else (4 4) before the word, and (3) after it.
        let emission = certain(&[4, 4, 2, 2, 3], 5);
        let path = best_path(&emission, 5, &[5, 2, 5], 0).unwrap();
        // The word lies where it is heard, and the wildcards have the rest.
        // A wildcard costs nothing, so it may take all of a word's moments
        // but one: which is why a window is given its neighbours' words,
        // and only its own are placed by it.
        assert!(path[1].0 >= 2 && path[1].1 <= 3, "{path:?}");
        assert_eq!((path[0].0, path[2].1), (0, 4));
    }

    #[test]
    fn words_are_placed_in_runs_cut_at_pauses() {
        let word = |start: f64| Word { start, end: start + 0.4, text: "la".into(), p: 0.9, a: None, note: None };
        // Forty words half a second apart, a pause of a second after the thirty-fifth.
        let words: Vec<Word> =
            (0..40).map(|index| word(index as f64 * 0.5 + if index >= 35 { 1.0 } else { 0.0 })).collect();
        assert_eq!(windows(&words), [(0, 35), (35, 40)]);
        assert_eq!(windows(&words[..3]), [(0, 3)]);
    }
}
