//! The words of a song in sung lines, each word where it is sung.
//!
//! A transcriber knows the words but is loose about when, and an aligner
//! has the start of a word right but can leave a held note's last letters
//! stranded. What neither can do is hear where the singing actually starts
//! and stops, which the loudness of the isolated vocal says plainly. So
//! every boundary is moved to the nearest quiet moment, words with nothing
//! sung under them are dropped as made up, and the words are cut into
//! lines at the breaths.

use serde::{Deserialize, Serialize};
use trill_listen::VoiceNote;
use trill_listen::signal::{RATE, db, frame_rms, percentile};

/// One sung word.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Word {
    /// Where it starts and ends, in seconds of the recording.
    pub start: f64,
    pub end: f64,
    pub text: String,
    /// How sure the transcriber was of it, 0 to 1.
    pub p: f32,
    /// How sure the aligner was of where it is, when it was placed by one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub a: Option<f32>,
    /// The MIDI note it is mostly sung at.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<u8>,
}

/// One sung line.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Line {
    pub start: f64,
    pub end: f64,
    pub text: String,
    pub words: Vec<Word>,
}

/// The words of a recording.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Lyrics {
    /// The language sung, as the transcriber told it.
    pub language: Option<String>,
    pub lines: Vec<Line>,
    /// Whether each word was placed by an aligner, or only as well as the
    /// transcriber's stretches allow.
    #[serde(default)]
    pub placed: bool,
}

impl Lyrics {
    pub fn words(&self) -> impl Iterator<Item = &Word> {
        self.lines.iter().flat_map(|line| &line.words)
    }
}

/// The loudness of a vocal over time, every five milliseconds.
pub struct Envelope {
    levels: Vec<f64>,
    /// Anything under this is silence or bleed.
    pub floor: f64,
}

const HOP: usize = (RATE / 200.0) as usize;

impl Envelope {
    /// Of a vocal as one channel at [`RATE`].
    pub fn new(vocal: &[f32]) -> Self {
        let levels: Vec<f64> = frame_rms(vocal, HOP * 2, HOP).iter().map(|&level| db(f64::from(level))).collect();
        let loud = percentile(&levels, 95.0);
        Self { levels, floor: (loud - 42.0).max(-66.0) }
    }

    fn frame(&self, seconds: f64) -> usize {
        ((seconds * RATE / HOP as f64).round().max(0.0) as usize).min(self.levels.len().saturating_sub(1))
    }

    fn seconds(&self, frame: usize) -> f64 {
        frame as f64 * HOP as f64 / RATE
    }

    /// The loudest moment between two times.
    pub fn level(&self, start: f64, end: f64) -> f64 {
        let first = self.frame(start);
        let last = (first + 1).max(self.frame(end)).min(self.levels.len());
        self.levels[first.min(last.saturating_sub(1))..last].iter().copied().fold(f64::NEG_INFINITY, f64::max)
    }

    /// Whether anything is sung between two times.
    pub fn sung(&self, start: f64, end: f64) -> bool {
        !self.levels.is_empty() && self.level(start, end) > self.floor
    }

    /// The stretches in which something is sung, in seconds: silences
    /// shorter than `shortest_silence` do not divide one, a stretch
    /// shorter than a quarter of a second is none, and each is given
    /// `pad` seconds either side.
    pub fn sung_stretches(&self, shortest_silence: f64, pad: f64) -> Vec<(f64, f64)> {
        let mut stretches: Vec<(f64, f64)> = Vec::new();
        let mut begun: Option<usize> = None;
        for frame in 0..=self.levels.len() {
            let sounding = self.levels.get(frame).is_some_and(|&level| level > self.floor);
            match (begun, sounding) {
                (None, true) => begun = Some(frame),
                (Some(first), false) => {
                    let (start, end) = (self.seconds(first), self.seconds(frame));
                    match stretches.last_mut() {
                        Some(last) if start - last.1 < shortest_silence => last.1 = end,
                        _ => stretches.push((start, end)),
                    }
                    begun = None;
                }
                _ => {}
            }
        }
        let total = self.seconds(self.levels.len());
        let mut padded: Vec<(f64, f64)> = Vec::new();
        for (start, end) in stretches.into_iter().filter(|(start, end)| end - start >= 0.25) {
            let (start, end) = ((start - pad).max(0.0), (end + pad).min(total));
            match padded.last_mut() {
                Some(last) if start <= last.1 => last.1 = end,
                _ => padded.push((start, end)),
            }
        }
        padded
    }

    /// How much the voice drops away between two words, 0 to 0.4: the mark
    /// of a breath, even where no silence separates them.
    fn dip(&self, before: &Word, after: &Word) -> f64 {
        let first = self.frame((before.start + before.end) / 2.0);
        let last = (first + 2).max(self.frame((after.start + after.end) / 2.0)).min(self.levels.len());
        let lowest = self.levels[first.min(last.saturating_sub(1))..last].iter().copied().fold(f64::INFINITY, f64::min);
        let loud = self.level(before.start, before.end).min(self.level(after.start, after.end));
        ((loud - lowest - 6.0) / 30.0).clamp(0.0, 1.0) * 0.4
    }

    /// Moves every boundary to the nearest quiet moment, without letting
    /// one word take any of its neighbours.
    ///
    /// `placed` says where the times came from, because the two sources
    /// err in opposite directions. A transcriber stretches a word back
    /// over the silence before it; an aligner, which has the start right,
    /// can leave a held note's last letters stranded after the silence
    /// that follows it.
    pub fn refine(&self, words: &mut [Word], placed: bool) {
        if self.levels.is_empty() {
            return;
        }
        let total = self.levels.len();
        for index in 0..words.len() {
            let previous_end = if index > 0 { words[index - 1].end } else { 0.0 };
            let following = words.get(index + 1).map_or(self.seconds(total), |next| next.start);
            let (mut first, mut last) = (self.frame(words[index].start), self.frame(words[index].end));
            if last <= first {
                continue;
            }
            let peak = self.levels[first..=last].iter().copied().fold(f64::NEG_INFINITY, f64::max);
            let threshold = (peak - 26.0).max(self.floor);

            // Keep only the stretch that is actually sung. Where a silence
            // divides it, keep the side the source can be trusted on.
            let sounding: Vec<usize> = (first..=last).filter(|&frame| self.levels[frame] > threshold).collect();
            if let (Some(&begin), Some(&finish)) = (sounding.first(), sounding.last()) {
                let gap = (0.25 * RATE / HOP as f64) as usize;
                let breaks: Vec<usize> =
                    (1..sounding.len()).filter(|&at| sounding[at] - sounding[at - 1] > gap).collect();
                let (begin, finish) = match (breaks.first(), breaks.last()) {
                    (Some(&first_break), _) if placed => (begin, sounding[first_break - 1]),
                    (_, Some(&last_break)) => (sounding[last_break], finish),
                    _ => (begin, finish),
                };
                (first, last) = (begin, finish);
                words[index].start = self.seconds(first);
                words[index].end = self.seconds(last + 1);
            }

            // Start: where the sound first rises, searching a little either way.
            let low = self.frame(previous_end.max(words[index].start - 0.12));
            let high = self.frame((words[index].start + 0.08).min((words[index].start + words[index].end) / 2.0));
            let mut start = first;
            if self.levels[first] > threshold {
                while start > low && self.levels[start - 1] > threshold {
                    start -= 1;
                }
                if start == low && low > 0 && self.levels[low] > threshold {
                    // Sung straight on from the word before: split at the dip.
                    let quietest =
                        (low..=first).min_by(|&a, &b| self.levels[a].total_cmp(&self.levels[b]).then(a.cmp(&b)));
                    start = quietest.unwrap_or(low);
                }
            } else {
                while start < high && self.levels[start] <= threshold {
                    start += 1;
                }
            }

            // End: follow the tail out until it dies or the next word starts.
            let ceiling = self.frame(following.min(words[index].end + 4.0));
            let mut end = last;
            if self.levels[last.min(total - 1)] > threshold {
                while end < ceiling && self.levels[end] > threshold {
                    end += 1;
                }
            } else {
                while end > start + 4 && self.levels[end - 1] <= threshold {
                    end -= 1;
                }
            }

            let new_start = previous_end.max(self.seconds(start) - 0.004);
            let new_end = following.min(self.seconds(end) + 0.010);
            if new_end - new_start >= 0.05 {
                words[index].start = new_start;
                words[index].end = new_end;
            }
            // The next word may not start before this one ends.
            if index + 1 < words.len() && words[index + 1].start < words[index].end {
                let boundary = (words[index + 1].start + words[index].end) / 2.0;
                words[index].end = boundary;
                words[index + 1].start = boundary;
            }
        }
    }
}

/// Puts words that were placed a window at a time in order: windows can
/// disagree by a frame where they meet, and no word may have no length.
pub fn settle(words: &mut [Word]) {
    for index in 1..words.len() {
        if words[index].start < words[index - 1].end {
            let mut middle = (words[index].start + words[index - 1].end) / 2.0;
            if middle <= words[index - 1].start {
                middle = words[index - 1].start + 0.02;
            }
            words[index - 1].end = middle;
            words[index].start = middle.max(words[index].start);
        }
        if words[index].end <= words[index].start {
            words[index].end = words[index].start + 0.04;
        }
    }
}

fn ends_a_phrase(text: &str) -> bool {
    text.ends_with([',', '.', ';', ':', '!', '?'])
}

/// "I", "I'm", "I'll": capital wherever they stand.
fn always_capital(text: &str) -> bool {
    let plain = text.trim_matches([',', '.', ';', ':', '!', '?']);
    plain == "I"
        || plain.strip_prefix("I'").is_some_and(|rest| !rest.is_empty() && rest.chars().all(char::is_alphanumeric))
}

/// Breaks a run of words with no pause in it into lines of a singable
/// length, at the likeliest joins: a breath, a comma, a capital letter.
fn split_line(words: &[Word], envelope: &Envelope, out: &mut Vec<Vec<Word>>) {
    if words.len() <= 10 {
        out.push(words.to_vec());
        return;
    }
    let mut best: Option<(usize, f64)> = None;
    for index in 1..words.len() - 2 {
        let (before, after) = (&words[index], &words[index + 1]);
        let mut score = (after.start - before.end).max(0.0) + envelope.dip(before, after);
        if ends_a_phrase(&before.text) {
            score += 0.25;
        }
        // A transcriber writes lyrics the way they are printed: a capital
        // starts a line.
        if after.text.chars().next().is_some_and(char::is_uppercase) && !always_capital(&after.text) {
            score += 0.3;
        }
        if best.is_none_or(|(_, top)| score > top) && score > 0.0 {
            best = Some((index, score));
        }
    }
    match best.filter(|&(_, score)| score >= 0.12) {
        Some((index, _)) => {
            split_line(&words[..=index], envelope, out);
            split_line(&words[index + 1..], envelope, out);
        }
        None => out.push(words.to_vec()),
    }
}

/// Cuts words into sung lines: a new line at every real pause, and where
/// a run has none, at its breaths. A transcriber's own stretches are no
/// guide: it ends them wherever its window happened to.
pub fn lines(words: Vec<Word>, envelope: &Envelope) -> Vec<Line> {
    let mut runs: Vec<Vec<Word>> = Vec::new();
    let mut current: Vec<Word> = Vec::new();
    for word in words {
        if current.last().is_some_and(|last| word.start - last.end > 0.45) {
            split_line(&current, envelope, &mut runs);
            current.clear();
        }
        current.push(word);
    }
    if !current.is_empty() {
        split_line(&current, envelope, &mut runs);
    }
    let round = |seconds: f64| (seconds * 1e4).round() / 1e4;
    runs.into_iter()
        .filter(|run| !run.is_empty())
        .map(|mut run| {
            for word in &mut run {
                (word.start, word.end) = (round(word.start), round(word.end));
            }
            Line {
                start: run[0].start,
                end: run[run.len() - 1].end,
                text: run.iter().map(|word| word.text.as_str()).collect::<Vec<_>>().join(" "),
                words: run,
            }
        })
        .collect()
}

/// Notes each word with the pitch it is mostly sung at, from the notes of
/// the melody.
pub fn pitch_words(lyrics: &mut Lyrics, sung: &[VoiceNote]) {
    for word in lyrics.lines.iter_mut().flat_map(|line| &mut line.words) {
        let mut weights: Vec<(u8, f64)> = Vec::new();
        for note in sung {
            let overlap = note.end.min(word.end) - note.start.max(word.start);
            if overlap > 0.0 {
                match weights.iter_mut().find(|(midi, _)| *midi == note.midi) {
                    Some(slot) => slot.1 += overlap,
                    None => weights.push((note.midi, overlap)),
                }
            }
        }
        word.note = weights
            .iter()
            .copied()
            .max_by(|a, b| a.1.total_cmp(&b.1))
            .filter(|&(_, weight)| weight >= 0.25 * (word.end - word.start))
            .map(|(midi, _)| midi);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A vocal that sounds in the given stretches and is silent between.
    fn vocal(seconds: f64, sung: &[(f64, f64)]) -> Vec<f32> {
        let mut signal = vec![0.0f32; (seconds * RATE) as usize];
        for &(start, end) in sung {
            let from = (start * RATE) as usize;
            for (index, slot) in signal[from..(end * RATE) as usize].iter_mut().enumerate() {
                *slot = 0.4 * ((from + index) as f32 * 0.06).sin();
            }
        }
        signal
    }

    fn word(start: f64, end: f64, text: &str) -> Word {
        Word { start, end, text: text.to_owned(), p: 0.9, a: None, note: None }
    }

    #[test]
    fn a_word_is_trimmed_to_where_it_is_sung() {
        let envelope = Envelope::new(&vocal(4.0, &[(1.0, 1.5), (2.2, 2.9)]));
        // A transcriber's times: stretched back over the silence before.
        let mut words = vec![word(0.4, 1.5, "hold"), word(1.6, 2.9, "on")];
        envelope.refine(&mut words, false);
        assert!((words[0].start - 1.0).abs() < 0.02 && (words[0].end - 1.5).abs() < 0.03, "{:?}", words[0]);
        assert!((words[1].start - 2.2).abs() < 0.02 && (words[1].end - 2.9).abs() < 0.03, "{:?}", words[1]);
        assert!(envelope.sung(1.0, 1.5) && !envelope.sung(1.7, 2.0));
        // What is sung, with a breath's room either side; a silence under 0.7 s does not divide it.
        let stretches = envelope.sung_stretches(0.7, 0.3);
        assert_eq!(stretches.len(), 1, "{stretches:?}");
        assert!((stretches[0].0 - 0.7).abs() < 0.02 && (stretches[0].1 - 3.2).abs() < 0.03, "{stretches:?}");
        assert_eq!(envelope.sung_stretches(0.5, 0.0).len(), 2);
    }

    #[test]
    fn an_aligner_and_a_transcriber_are_each_trusted_on_their_own_side_of_a_silence() {
        // The word's span covers two sung stretches with half a second between.
        let envelope = Envelope::new(&vocal(4.0, &[(1.0, 1.4), (2.0, 2.4)]));
        let mut placed = vec![word(1.0, 2.4, "stay")];
        envelope.refine(&mut placed, true);
        assert!((placed[0].start - 1.0).abs() < 0.02 && (placed[0].end - 1.4).abs() < 0.03, "{:?}", placed[0]);
        let mut heard = vec![word(1.0, 2.4, "stay")];
        envelope.refine(&mut heard, false);
        assert!((heard[0].start - 2.0).abs() < 0.02 && (heard[0].end - 2.4).abs() < 0.03, "{:?}", heard[0]);
    }

    #[test]
    fn words_sung_straight_on_do_not_take_from_one_another() {
        let envelope = Envelope::new(&vocal(3.0, &[(1.0, 2.0)]));
        let mut words = vec![word(0.9, 1.6, "all"), word(1.4, 2.1, "night")];
        envelope.refine(&mut words, true);
        assert!(words[0].end <= words[1].start + 1e-9, "{words:?}");
        assert!(words[0].start >= 0.95 && words[1].end <= 2.05);
        let mut crossed = vec![word(1.0, 1.5, "a"), word(1.2, 1.2, "b")];
        settle(&mut crossed);
        assert!(crossed[0].end <= crossed[1].start && crossed[1].end > crossed[1].start, "{crossed:?}");
    }

    #[test]
    fn words_are_cut_into_lines_at_pauses_and_a_long_run_at_its_commas_and_capitals() {
        let envelope = Envelope::new(&vocal(30.0, &[(0.0, 30.0)]));
        let mut words: Vec<Word> =
            (0..4).map(|index| word(index as f64 * 0.4, index as f64 * 0.4 + 0.35, "la")).collect();
        // A pause of more than 0.45 s, then fourteen words with no pause.
        let texts = [
            "Now", "we", "run", "and", "run", "away,", "never", "ever", "looking", "back", "Then", "the", "night",
            "falls",
        ];
        words.extend(
            texts
                .iter()
                .enumerate()
                .map(|(index, text)| word(3.0 + index as f64 * 0.4, 3.35 + index as f64 * 0.4, text)),
        );
        let found = lines(words, &envelope);
        let texts: Vec<&str> = found.iter().map(|line| line.text.as_str()).collect();
        assert_eq!(texts, ["la la la la", "Now we run and run away, never ever looking back", "Then the night falls"]);
        assert_eq!((found[0].start, found[0].end), (0.0, 1.55));
        assert_eq!(found[2].words.len(), 4);
        assert!(always_capital("I'm") && always_capital("I,") && !always_capital("It"));
    }

    #[test]
    fn a_word_takes_the_note_it_is_mostly_sung_at() {
        let mut lyrics = Lyrics {
            language: Some("en".into()),
            lines: vec![Line {
                start: 1.0,
                end: 3.0,
                text: "oh no".into(),
                words: vec![word(1.0, 2.0, "oh"), word(2.0, 3.0, "no")],
            }],
            placed: true,
        };
        let sung = [
            VoiceNote { start: 0.9, end: 1.3, midi: 60, cents: 0.0, level: -20.0 },
            VoiceNote { start: 1.3, end: 2.0, midi: 62, cents: 0.0, level: -20.0 },
            // A flicker under the second word: not a note it is sung at.
            VoiceNote { start: 2.1, end: 2.2, midi: 65, cents: 0.0, level: -20.0 },
        ];
        pitch_words(&mut lyrics, &sung);
        let notes: Vec<Option<u8>> = lyrics.words().map(|word| word.note).collect();
        assert_eq!(notes, [Some(62), None]);
    }
}
