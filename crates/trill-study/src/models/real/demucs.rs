//! Hybrid Transformer Demucs around a graph that is cut at the complex
//! boundary: the network is the runtime's, and the transform to and from
//! the spectrogram, the chunks and their cross-fade are here.
//!
//! The graph takes a chunk of 7.8 seconds twice, as a waveform and as a
//! spectrogram with its real and imaginary parts as channels:
//!
//! ```text
//! in   mix  [1, 2, 343980]        mag  [1, 4, 2048, 336]
//! out  wav  [1, S, 2, 343980]     spec [1, S, 4, 2048, 336]
//! ```
//!
//! and a stem is `wav + inverse(spec)`. A recording is cut into chunks
//! that overlap by a quarter and are cross-faded; the whole of it is
//! brought to zero mean and unit deviation first and back after. The best
//! model is four graphs, one tuned for each stem, each asked for its own.
//!
//! Nothing here is drawn by lot: the same recording gives the same stems.

use super::tensor;
use crate::models::Progress;
use ort::session::Session;
use trill_dsp::fft::{RealFft, hann};
use trill_listen::{Sound, Stem};

/// A chunk, in frames at 44.1 kHz: what the model was trained on.
const SEGMENT: usize = 343_980;
const SIZE: usize = 4096;
const HOP: usize = 1024;
const BINS: usize = 2048;
const FRAMES: usize = 336;
/// How much of a chunk the next one shares with it.
const OVERLAP: f64 = 0.25;
/// The stems in the order the graph gives them.
const SOURCES: [&str; 4] = ["drums", "bass", "other", "vocals"];
/// And the order a study keeps them in.
const ORDER: [&str; 4] = ["vocals", "drums", "bass", "other"];

/// A position past either end of a signal, folded back into it.
fn reflect(index: isize, length: usize) -> usize {
    let last = length as isize - 1;
    let mut index = index;
    loop {
        if index < 0 {
            index = -index;
        } else if index > last {
            index = 2 * last - index;
        } else {
            return index as usize;
        }
    }
}

/// The transform of one channel of a chunk, and back.
struct Transform {
    fft: RealFft,
    window: Vec<f32>,
    frame: Vec<f32>,
    re: Vec<f32>,
    im: Vec<f32>,
    sum: Vec<f32>,
}

impl Transform {
    fn new() -> Self {
        Self {
            fft: RealFft::new(SIZE),
            window: hann(SIZE),
            frame: vec![0.0; SIZE],
            re: vec![0.0; SIZE / 2 + 1],
            im: vec![0.0; SIZE / 2 + 1],
            sum: Vec::new(),
        }
    }

    /// One channel of a chunk to its spectrogram: `re` and `im`, each
    /// `BINS * FRAMES`, bin by bin.
    fn forward(&mut self, signal: &[f32], re: &mut [f32], im: &mut [f32]) {
        let length = signal.len();
        let frames = length.div_ceil(HOP);
        let pad = HOP / 2 * 3;
        // Reflected first to a whole number of hops with a margin, then
        // again by half a window, as a centered transform does.
        let padded_length = length + 2 * pad + (frames * HOP - length);
        let scale = 1.0 / (SIZE as f32).sqrt();
        for frame in 0..frames {
            let centre = ((frame + 2) * HOP) as isize;
            for (offset, slot) in self.frame.iter_mut().enumerate() {
                let at = reflect(centre - (SIZE / 2) as isize + offset as isize, padded_length);
                *slot = signal[reflect(at as isize - pad as isize, length)] * self.window[offset];
            }
            self.fft.forward(&self.frame, &mut self.re, &mut self.im);
            for bin in 0..BINS {
                re[bin * frames + frame] = self.re[bin] * scale;
                im[bin * frames + frame] = self.im[bin] * scale;
            }
        }
    }

    /// The inverse: `out.len()` samples from `BINS * frames` bins, added
    /// to `out` with the weight `weight`.
    fn inverse_into(&mut self, re: &[f32], im: &[f32], weight: f32, out: &mut [f32]) {
        let length = out.len();
        let frames = length.div_ceil(HOP);
        let pad = HOP / 2 * 3;
        self.sum.clear();
        self.sum.resize(HOP * frames + 2 * pad + SIZE, 0.0);
        let scale = (SIZE as f32).sqrt();
        // Two silent frames either side, which the forward transform dropped.
        for frame in 2..frames + 2 {
            let source = frame - 2;
            for bin in 0..BINS {
                self.re[bin] = re[bin * frames + source];
                self.im[bin] = im[bin * frames + source];
            }
            self.re[BINS] = 0.0;
            self.im[BINS] = 0.0;
            self.fft.inverse(&self.re, &self.im, &mut self.frame);
            for (offset, &value) in self.frame.iter().enumerate() {
                self.sum[frame * HOP + offset] += value * scale * self.window[offset];
            }
        }
        // Every sample is under four windows, whose squares add up to 1.5.
        for (index, slot) in out.iter_mut().enumerate() {
            *slot += weight * self.sum[SIZE / 2 + pad + index] / 1.5;
        }
    }
}

/// Runs one chunk of exactly [`SEGMENT`] frames through a graph and
/// answers with its stems, `[source][channel]`: all of them, or `only`
/// the one a graph of the best model was tuned for (the others are left
/// silent).
fn chunk(
    session: &mut Session,
    only: Option<usize>,
    transforms: &mut [Transform],
    left: &[f32],
    right: &[f32],
) -> Result<Vec<[Vec<f32>; 2]>, String> {
    let plane = BINS * FRAMES;
    let mut mag = vec![0.0f32; 4 * plane];
    {
        let (first, second) = mag.split_at_mut(2 * plane);
        let (two, _) = transforms.split_at_mut(2);
        std::thread::scope(|scope| {
            for ((planes, signal), transform) in [first, second].into_iter().zip([left, right]).zip(two.iter_mut()) {
                scope.spawn(move || {
                    let (re, im) = planes.split_at_mut(plane);
                    transform.forward(signal, re, im);
                });
            }
        });
    }
    let mut mix = Vec::with_capacity(2 * SEGMENT);
    mix.extend_from_slice(left);
    mix.extend_from_slice(right);

    let mut stems: Vec<[Vec<f32>; 2]> = (0..SOURCES.len()).map(|_| [vec![0.0; SEGMENT], vec![0.0; SEGMENT]]).collect();
    {
        let outputs = session
            .run(ort::inputs![
                "mix" => tensor(&[1, 2, SEGMENT], mix.clone(), 2 * SEGMENT)?,
                "mag" => tensor(&[1, 4, BINS, FRAMES], mag.clone(), 4 * plane)?,
            ])
            .map_err(|error| format!("the model that splits could not be run: {error}"))?;
        let (shape, spec) = outputs["spec"].try_extract_tensor::<f32>().map_err(|error| error.to_string())?;
        let (_, wav) = outputs["wav"].try_extract_tensor::<f32>().map_err(|error| error.to_string())?;
        if shape.len() != 5
            || shape[1] as usize != SOURCES.len()
            || spec.len() != SOURCES.len() * 4 * plane
            || wav.len() != SOURCES.len() * 2 * SEGMENT
        {
            return Err(format!(
                "the model that splits answered in a shape that is not its own ({shape:?}): is its file the one the setup fetched?"
            ));
        }
        let wanted: Vec<usize> = only.map_or_else(|| (0..SOURCES.len()).collect(), |source| vec![source]);
        let mut jobs: Vec<(usize, usize, &mut Vec<f32>)> = Vec::new();
        for (source, stem) in stems.iter_mut().enumerate().filter(|(source, _)| wanted.contains(source)) {
            for (channel, out) in stem.iter_mut().enumerate() {
                jobs.push((source, channel, out));
            }
        }
        std::thread::scope(|scope| {
            for ((source, channel, out), transform) in jobs.into_iter().zip(transforms.iter_mut()) {
                scope.spawn(move || {
                    let at = (source * 4 + channel * 2) * plane;
                    transform.inverse_into(&spec[at..at + plane], &spec[at + plane..at + 2 * plane], 1.0, out);
                    let branch = &wav[(source * 2 + channel) * SEGMENT..(source * 2 + channel + 1) * SEGMENT];
                    for (slot, value) in out.iter_mut().zip(branch) {
                        *slot += value;
                    }
                });
            }
        });
    }
    Ok(stems)
}

/// Splits `mix` into its stems with one graph, or with the four of the
/// best model in the order drums, bass, other, vocals. `make` makes the
/// graph of that number ready. Of the four, each gives the stem it was
/// tuned for, and they are made ready and let go of one after another:
/// the recording is gone through four times, and a card holds one model
/// at a time, where four at once were measured to take twice the memory.
pub fn separate(
    graphs: usize,
    make: &mut dyn FnMut(usize) -> Result<Session, String>,
    mix: &Sound,
    progress: Progress,
) -> Result<Vec<Stem>, String> {
    if graphs != 1 && graphs != SOURCES.len() {
        return Err(format!("{graphs} models were handed to split a recording: it takes one, or the four of the best"));
    }
    let length = mix.frames();
    if length == 0 {
        return Err("there is nothing to split".to_owned());
    }
    // The whole recording to zero mean and unit deviation of its two channels as one.
    let mono: Vec<f64> = (0..length).map(|i| 0.5 * (f64::from(mix.left[i]) + f64::from(mix.right[i]))).collect();
    let mean = mono.iter().sum::<f64>() / length as f64;
    let deviation =
        (mono.iter().map(|value| (value - mean) * (value - mean)).sum::<f64>() / (length as f64 - 1.0).max(1.0)).sqrt()
            + 1e-8;
    let scaled = |channel: &[f32]| -> Vec<f32> {
        channel[..length].iter().map(|&v| ((f64::from(v) - mean) / deviation) as f32).collect()
    };
    let (left, right) = (scaled(&mix.left), scaled(&mix.right));

    let stride = ((1.0 - OVERLAP) * SEGMENT as f64) as usize;
    let half = SEGMENT / 2;
    // A triangle: a chunk counts most in its middle.
    let weight: Vec<f32> =
        (0..SEGMENT).map(|i| if i < half { (i + 1) as f32 } else { (SEGMENT - i) as f32 } / half as f32).collect();
    let mut transforms: Vec<Transform> = (0..2 * SOURCES.len()).map(|_| Transform::new()).collect();
    let mut out: Vec<[Vec<f32>; 2]> = (0..SOURCES.len()).map(|_| [vec![0.0; length], vec![0.0; length]]).collect();
    let mut total = vec![0.0f32; length];
    let chunks = length.div_ceil(stride);
    for pass in 0..graphs {
        let only = (graphs > 1).then_some(pass);
        let mut session = make(pass)?;
        for (number, offset) in (0..length).step_by(stride).enumerate() {
            let own = SEGMENT.min(length - offset);
            // A short last chunk is centered in real audio and padded with silence.
            let start = offset as isize - ((SEGMENT - own) / 2) as isize;
            let mut piece = [vec![0.0f32; SEGMENT], vec![0.0f32; SEGMENT]];
            for (slots, source) in piece.iter_mut().zip([&left, &right]) {
                for (index, slot) in slots.iter_mut().enumerate() {
                    if let Some(&sample) = usize::try_from(start + index as isize).ok().and_then(|at| source.get(at)) {
                        *slot = sample;
                    }
                }
            }
            let stems = chunk(&mut session, only, &mut transforms, &piece[0], &piece[1])?;
            let trim = (SEGMENT - own) / 2;
            for (source, (stem, out)) in stems.iter().zip(&mut out).enumerate() {
                if only.is_some_and(|wanted| wanted != source) {
                    continue;
                }
                for (channel, out) in out.iter_mut().enumerate() {
                    for index in 0..own {
                        out[offset + index] += weight[index] * stem[channel][trim + index];
                    }
                }
            }
            if pass == 0 {
                for index in 0..own {
                    total[offset + index] += weight[index];
                }
            }
            progress((pass * chunks + number + 1) as f32 / (graphs * chunks) as f32);
        }
    }
    let back = |channel: &[f32]| -> Vec<f32> {
        channel.iter().zip(&total).map(|(&v, &w)| (f64::from(v / w) * deviation + mean) as f32).collect()
    };
    let mut stems: Vec<Stem> = out
        .iter()
        .zip(SOURCES)
        .map(|(stem, name)| Stem {
            name: name.to_owned(),
            sound: Sound { left: back(&stem[0]), right: back(&stem[1]) },
        })
        .collect();
    stems.sort_by_key(|stem| ORDER.iter().position(|name| *name == stem.name));
    Ok(stems)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_chunk_comes_back_from_its_spectrogram_as_it_went_in() {
        let mut transform = Transform::new();
        let signal: Vec<f32> =
            (0..SEGMENT).map(|i| (i as f32 * 0.013).sin() * 0.5 + (i as f32 * 0.21).sin() * 0.2).collect();
        let plane = BINS * FRAMES;
        let (mut re, mut im) = (vec![0.0f32; plane], vec![0.0f32; plane]);
        transform.forward(&signal, &mut re, &mut im);
        let mut back = vec![0.0f32; SEGMENT];
        transform.inverse_into(&re, &im, 1.0, &mut back);
        // But for its first and last tenth of a second, where the frames
        // that hung over the ends were dropped as the model has them
        // dropped: the cross-fade gives those next to no weight.
        let inside = SIZE..SEGMENT - SIZE;
        let worst = signal[inside.clone()].iter().zip(&back[inside]).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
        assert!(worst < 2e-3, "{worst}");
        assert_eq!((reflect(-3, 10), reflect(12, 10), reflect(4, 10)), (3, 6, 4));
    }
}
