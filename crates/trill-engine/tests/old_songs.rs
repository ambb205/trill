//! A song saved before the built-in effects were rebuilt must sound as it
//! did.
//!
//! Each case here is a project file of format 1, as the builds before
//! format 2 wrote it: one audio track with one effect, set as such a file
//! set it (only what was changed from the defaults of the day is in the
//! file). It is opened as any file is and rendered, and the render is held
//! to numbers that the last build with the old effects (commit 724556d)
//! wrote down from the same file: the level of each side, and sixteen
//! samples of each from the first to the tail.
//!
//! The numbers are never to be written down again from a newer build: that
//! would make the test agree with whatever the effects have become.

mod common;

use common::*;
use trill_core::Project;
use trill_core::audio::AudioBuffer;
use trill_dsp::BuiltinFactory;
use trill_engine::render::{RenderOptions, render};
use trill_media::WavFormat;

/// The frames a render is sampled at.
const POINTS: usize = 16;
fn frame(point: usize) -> usize {
    997 + point * 9001
}

/// What a render is held to: the rms of the left and of the right side,
/// then the left and the right sample at each of the [`POINTS`].
type Print = [f32; 2 + 2 * POINTS];

/// Two seconds of stereo sound with something for every effect to work on:
/// a burst of noise that decays, a low tone that swells and one an octave
/// up on the other side, two high tones, and a single click on the left.
fn source() -> AudioBuffer {
    let frames = (2.0 * SR) as usize;
    let (mut left, mut right) = (vec![0.0f32; frames], vec![0.0f32; frames]);
    let mut seed = 0x2545_F491u32;
    let mut noise = || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        (seed >> 8) as f64 / 8_388_608.0 - 1.0
    };
    for n in 0..frames {
        let t = n as f64 / SR;
        let turn = std::f64::consts::TAU * t;
        let (mut l, mut r) = (0.0, 0.0);
        if t < 0.5 {
            let (a, b) = (noise(), noise());
            let fall = (-t / 0.08).exp();
            l += 0.7 * a * fall;
            r += 0.7 * (0.6 * a + 0.4 * b) * fall;
        }
        if (0.25..1.25).contains(&t) {
            let swell = 0.5 - 0.5 * (std::f64::consts::TAU * (t - 0.25)).cos();
            l += 0.45 * swell * (110.0 * turn).sin();
            r += 0.3 * swell * (220.0 * turn).sin();
        }
        if (1.0..1.5).contains(&t) {
            let both = 0.2 * (1000.0 * turn).sin() + 0.15 * (6000.0 * turn).sin();
            l += both;
            r += both;
        }
        if n == (1.5 * SR) as usize {
            l += 0.9;
        }
        left[n] = l as f32;
        right[n] = r as f32;
    }
    AudioBuffer { sample_rate: SR, left, right }
}

/// A project file of format 1: `source` on a track through the device
/// `uri` with `params` as the file has them.
fn old_file(source: &str, uri: &str, params: &str) -> String {
    format!(
        r#"{{
  "format_version": 1,
  "name": "old",
  "tempo": 120.0,
  "time_signature": {{ "numerator": 4, "denominator": 4 }},
  "loop_region": {{ "enabled": false, "start": 0.0, "end": 16.0 }},
  "tracks": [
    {{
      "id": 1,
      "name": "Audio",
      "color": 15898954,
      "volume_db": 0.0,
      "pan": 0.0,
      "mute": false,
      "solo": false,
      "devices": [
        {{ "id": 3, "uri": "{uri}", "name": "Effect", "enabled": true, "params": {params} }}
      ],
      "clips": [
        {{
          "id": 2,
          "name": "clip",
          "start": 0.0,
          "length": 4.0,
          "muted": false,
          "content": {{ "type": "audio", "path": "{source}", "offset_seconds": 0.0, "gain_db": 0.0 }}
        }}
      ]
    }}
  ],
  "master": {{ "volume_db": 0.0, "devices": [] }},
  "automation": [],
  "next_id": 4
}}"#
    )
}

/// Opens the file and renders it: the two seconds of the clip and what
/// rings on after it.
fn print_of(file: &str) -> Print {
    let project = Project::from_json(file).unwrap();
    let options = RenderOptions { sample_rate: SR as u32, max_tail_seconds: 2.0, ..RenderOptions::default() };
    let out = render(&project, &BuiltinFactory, &options).unwrap();
    assert!(out.problems.is_empty(), "{:?}", out.problems);
    let mut print = [0.0f32; 2 + 2 * POINTS];
    print[0] = rms(&out.left);
    print[1] = rms(&out.right);
    for point in 0..POINTS {
        print[2 + 2 * point] = out.left.get(frame(point)).copied().unwrap_or(0.0);
        print[3 + 2 * point] = out.right.get(frame(point)).copied().unwrap_or(0.0);
    }
    print
}

/// The device, what the old file set on it, and what it rendered to.
#[rustfmt::skip]
const OLD: &[(&str, &str, Print)] = &[
    (
        "builtin:reverb",
        r#"{}"#,
        [
            1.3523917e-1, 1.2991115e-1, -1.456095e-1, 1.21230915e-1, -2.1384245e-2, -4.1644085e-5,
            -4.782651e-2, 4.3672916e-2, 3.2844532e-1, 1.6575776e-1, -4.2713615e-1, -3.9294207e-1,
            1.0116399e-1, 2.7368082e-2, -6.5281704e-2, 2.2232133e-1, 3.39713e-2, 4.690073e-3,
            1.10318646e-1, -8.232623e-2, -6.526519e-2, 1.0901432e-1, -9.494956e-3, -2.4211084e-2,
            -2.0488778e-2, 1.3310372e-2, 1.772738e-3, -1.245066e-2, -1.0163592e-3, -1.755806e-4,
            -1.7914431e-3, 7.5976746e-3, 1.4073122e-3, -4.5358823e-3,
        ],
    ),
    (
        "builtin:reverb",
        r#"{"decay": 0.7, "predelay": 6.0, "size": 0.21, "damping": 0.3, "mix": 1.0, "width": 1.0, "low_cut": 110.0}"#,
        [
            1.4879893e-1, 1.2284879e-1, 0e0, 0e0, 1.6589642e-2, 4.5125387e-3,
            -2.0697331e-2, 1.1165067e-4, -9.284784e-2, -2.5255817e-1, 1.6960284e-1, 1.0241253e-1,
            7.834225e-2, 1.7724691e-1, -2.4573718e-1, 7.9151854e-2, 1.6081767e-1, 4.508106e-2,
            -5.6574866e-2, 3.3076644e-2, 7.586842e-2, -4.904277e-3, 3.5747099e-3, -3.520492e-5,
            1.1598944e-3, 2.804969e-4, -2.1716532e-4, 8.1094564e-5, -5.8422806e-6, 1.3718357e-5,
            0e0, 0e0, 0e0, 0e0,
        ],
    ),
    (
        "builtin:reverb",
        r#"{"decay": 4.0, "modulation": 0.8, "mix": 0.35, "width": 0.5, "low_cut": 400.0}"#,
        [
            1.2766832e-1, 1.0248566e-1, -1.456095e-1, 1.21230915e-1, -3.4127194e-2, -9.412565e-3,
            5.320854e-3, 4.2787872e-2, 3.1206462e-1, 1.558656e-1, -4.8848876e-1, -2.814012e-1,
            1.3587771e-1, 2.6755152e-2, -6.328748e-2, 8.192141e-2, 8.344348e-2, -4.9059663e-2,
            1.810207e-2, -3.1213889e-2, 3.0664379e-2, 7.8506954e-2, -2.7854873e-2, 3.938237e-2,
            6.7843264e-3, -4.004454e-2, 1.2042142e-2, 4.185561e-3, -2.6756007e-2, 1.3036553e-2,
            7.67409e-3, 1.9206073e-2, -5.763417e-3, -1.0874993e-2,
        ],
    ),
    (
        "builtin:delay",
        r#"{}"#,
        [
            1.3841206e-1, 9.58789e-2, -1.456095e-1, 1.21230915e-1, 1.674405e-2, -6.842109e-3,
            -4.5074206e-2, 2.7405798e-2, 2.851946e-1, 1.948557e-1, -4.2808452e-1, -1.1518275e-1,
            2.8001395e-1, -1.5488338e-1, -1.7031628e-1, 1.8340126e-3, 9.3110085e-2, 1.20304264e-1,
            -9.8588176e-2, -2.3596024e-2, -6.2292323e-2, -1.2259664e-2, -2.996412e-4, -2.0367416e-2,
            -4.3204305e-4, -1.665514e-2, 5.9413584e-3, 1.1308605e-3, -2.9850535e-3, 1.1266903e-3,
            5.2638905e-4, 5.955022e-3, 4.0613132e-4, -2.801303e-3,
        ],
    ),
    (
        "builtin:delay",
        r#"{"sync": 1.0, "time_beats": 0.75, "feedback": 0.3, "ping_pong": 1.0, "mix": 1.0, "low_cut": 318.2, "high_cut": 10182.3}"#,
        [
            9.1696024e-2, 1.981237e-2, 0e0, 0e0, 0e0, 0e0,
            -3.6309674e-2, 0e0, -1.1814898e-2, 0e0, 5.0073587e-3, 6.0526147e-2,
            2.4257565e-1, 3.9984724e-3, -2.890583e-1, 5.5851902e-5, -1.1994846e-2, 1.1294875e-2,
            -1.6507375e-1, -1.951812e-2, -1.01598404e-1, -8.612021e-3, 7.2341075e-4, -1.9776925e-2,
            1.0705533e-3, -2.1766832e-2, 1.3903266e-2, 5.504468e-4, -1.548579e-3, 3.812457e-4,
            4.2939544e-5, 6.453826e-3, 2.0061569e-5, -2.8672845e-3,
        ],
    ),
    (
        "builtin:delay",
        r#"{"sync": 0.0, "time_ms": 180.0, "feedback": 0.6, "ping_pong": 0.0, "mix": 0.5}"#,
        [
            2.065755e-1, 1.5392467e-1, -1.456095e-1, 1.21230915e-1, 2.782979e-1, 3.2541975e-1,
            1.867015e-1, 1.8542424e-1, 4.3984812e-1, 1.7053644e-1, -4.335009e-1, -1.8564081e-1,
            -1.866705e-1, 4.031564e-2, 1.8859206e-1, 1.2857421e-2, 2.2188683e-1, 2.0983866e-1,
            -2.3227148e-1, -2.3882188e-1, -5.016054e-2, -5.4655083e-2, -2.9646842e-2, -3.0575797e-2,
            -4.0722188e-2, -4.043085e-2, 1.4453901e-2, 1.4492174e-2, -1.8352773e-2, -1.8464284e-2,
            1.0754542e-2, 1.071455e-2, -7.1676983e-3, -7.1549444e-3,
        ],
    ),
    (
        "builtin:eq",
        r#"{}"#,
        [
            1.7293866e-1, 1.3360278e-1, -1.4560947e-1, 1.2123092e-1, 1.6744047e-2, -6.8421084e-3,
            -2.3288397e-2, 2.7405797e-2, 2.922835e-1, 1.9485568e-1, -4.310889e-1, -1.5674393e-1,
            1.3446854e-1, -1.5778778e-1, -1.740303e-3, 2.0501725e-3, 9.999996e-2, 9.999996e-2,
            0e0, 0e0, 0e0, 0e0, 0e0, 0e0,
            0e0, 0e0, 0e0, 0e0, 0e0, 0e0,
            0e0, 0e0, 0e0, 0e0,
        ],
    ),
    (
        "builtin:eq",
        r#"{"low_cut": 180.0, "low_freq": 120.0, "low_gain": 3.0, "mid1_freq": 300.0, "mid1_gain": -4.0, "mid1_q": 1.4, "mid2_freq": 3000.0, "mid2_gain": 2.5, "mid2_q": 0.7, "high_freq": 6000.0, "high_gain": 2.0, "high_cut": 12000.0}"#,
        [
            1.242619e-1, 1.21471226e-1, -1.0123478e-1, -1.4860642e-1, -4.1622533e-3, -1.6113719e-2,
            -3.3732906e-2, 3.704364e-2, 3.6058586e-2, 1.846075e-2, 9.551044e-2, -1.970257e-1,
            -1.1483833e-1, -8.565657e-3, 1.4334527e-1, 1.4787327e-1, 1.2668966e-1, 1.2670305e-1,
            -9.838998e-8, -2.103897e-7, 0e0, 0e0, 0e0, 0e0,
            0e0, 0e0, 0e0, 0e0, 0e0, 0e0,
            0e0, 0e0, 0e0, 0e0,
        ],
    ),
    (
        "builtin:compressor",
        r#"{}"#,
        [
            9.298435e-2, 7.311998e-2, -7.8429654e-2, 6.529861e-2, 1.344141e-2, -5.492553e-3,
            -2.2240162e-2, 2.6172232e-2, 1.6621634e-1, 1.108109e-1, -1.8388668e-1, -6.6861205e-2,
            7.262385e-2, -8.5218124e-2, -9.626436e-4, 1.1340515e-3, 5.9617557e-2, 5.9617557e-2,
            0e0, 0e0, 0e0, 0e0, 0e0, 0e0,
            0e0, 0e0, 0e0, 0e0, 0e0, 0e0,
            0e0, 0e0, 0e0, 0e0,
        ],
    ),
    (
        "builtin:compressor",
        r#"{"threshold": -24.0, "ratio": 6.0, "attack": 3.0, "release": 120.0, "knee": 6.0, "makeup": 10.0}"#,
        [
            1.4660458e-1, 1.1473271e-1, -9.575175e-2, 7.9720564e-2, 3.0474385e-2, -1.2452725e-2,
            -6.1835658e-2, 7.276822e-2, 2.5386465e-1, 1.6924311e-1, -2.8054407e-1, -1.0200585e-1,
            1.2162312e-1, -1.4271472e-1, -1.5002475e-3, 1.7673811e-3, 9.304271e-2, 9.304271e-2,
            0e0, 0e0, 0e0, 0e0, 0e0, 0e0,
            0e0, 0e0, 0e0, 0e0, 0e0, 0e0,
            0e0, 0e0, 0e0, 0e0,
        ],
    ),
    (
        "builtin:compressor",
        r#"{"threshold": -30.0, "ratio": 2.0, "attack": 30.0, "release": 400.0, "knee": 0.0, "makeup": 1.0, "mix": 0.6}"#,
        [
            1.5909174e-1, 1.2274833e-1, -1.6350503e-1, 1.3613029e-1, 1.7186005e-2, -7.0227054e-3,
            -2.6817137e-2, 3.1558413e-2, 2.7319813e-1, 1.8213211e-1, -3.6867058e-1, -1.3404866e-1,
            1.17333524e-1, -1.3768125e-1, -1.5517503e-3, 1.8280544e-3, 9.186525e-2, 9.186525e-2,
            0e0, 0e0, 0e0, 0e0, 0e0, 0e0,
            0e0, 0e0, 0e0, 0e0, 0e0, 0e0,
            0e0, 0e0, 0e0, 0e0,
        ],
    ),
    (
        "builtin:chorus",
        r#"{}"#,
        [
            1.906838e-1, 1.4701945e-1, -1.5099041e-1, 2.3926074e-2, 2.683738e-2, -2.7600633e-2,
            -4.274227e-2, -1.6809246e-2, -1.7349748e-2, 2.682445e-1, -6.037179e-1, -3.0799491e-2,
            1.8473896e-1, -7.025666e-3, 1.513012e-1, 1.5477508e-2, 2.718484e-1, -7.253691e-3,
            0e0, 0e0, 0e0, 0e0, 0e0, 0e0,
            0e0, 0e0, 0e0, 0e0, 0e0, 0e0,
            0e0, 0e0, 0e0, 0e0,
        ],
    ),
    (
        "builtin:chorus",
        r#"{"rate": 0.5, "depth": 0.25, "feedback": 0.3, "mix": 0.2}"#,
        [
            1.4424607e-1, 1.180391e-1, -6.500165e-2, 2.6857987e-1, 8.392058e-3, 1.6607837e-3,
            -1.5959052e-2, 3.7614603e-2, 2.0744224e-1, 1.348206e-1, -3.2206792e-1, -8.3503515e-2,
            1.496047e-1, -1.4665322e-1, 8.668801e-3, 5.117296e-2, 7.6224014e-2, 1.10958636e-1,
            -1.6559795e-2, -3.9980773e-2, 7.7998e-12, 1.9306808e-10, 3.2288612e-21, 1.5044514e-17,
            0e0, 0e0, 0e0, 0e0, 0e0, 0e0,
            0e0, 0e0, 0e0, 0e0,
        ],
    ),
    (
        "builtin:filter",
        r#"{}"#,
        [
            1.579226e-1, 1.2037632e-1, -1.298636e-1, -3.418452e-2, 1.7751293e-2, 1.19964825e-2,
            -1.1137675e-2, 1.681204e-2, 2.7441683e-1, 2.199817e-1, -4.4450766e-1, -1.0080676e-1,
            1.6121252e-1, -1.8114494e-1, -1.5887277e-1, -1.559449e-1, 1.8169402e-1, 1.816939e-1,
            9.2708755e-29, 5.5563007e-29, 2.4807304e-38, -3.7649972e-38, 2.4807304e-38, -3.7649972e-38,
            0e0, 0e0, 0e0, 0e0, 0e0, 0e0,
            0e0, 0e0, 0e0, 0e0,
        ],
    ),
    (
        "builtin:filter",
        r#"{"type": 0.0, "cutoff": 700.0, "resonance": 0.6, "lfo_rate": 3.0, "lfo_amount": 1.5}"#,
        [
            2.0059763e-1, 2.04939e-1, -1.6137901e-1, 7.4560116e-3, 2.0207863e-2, 1.7525937e-2,
            -2.0039171e-2, 2.7219027e-2, 3.329426e-1, 7.3461336e-1, -4.341789e-1, -1.5203041e-1,
            1.7104708e-1, -3.0614665e-1, -7.7973235e-1, -7.7554256e-1, -3.5675e-2, -3.5675e-2,
            -1.7150103e-5, -1.9302917e-5, 1.1227131e-33, 1.7158845e-33, -7.356146e-38, 7.6741073e-38,
            0e0, 0e0, 0e0, 0e0, 0e0, 0e0,
            0e0, 0e0, 0e0, 0e0,
        ],
    ),
    (
        "builtin:filter",
        r#"{"type": 1.0, "cutoff": 2500.0, "resonance": 0.12}"#,
        [
            7.637934e-2, 6.676543e-2, -1.7423116e-1, 1.08783506e-1, -3.7655558e-3, -1.1341746e-2,
            -2.3919053e-3, -6.893519e-4, -5.333423e-4, -1.5741586e-3, 8.3157414e-4, 9.7037846e-4,
            -2.7567145e-4, 1.2788324e-3, 6.57897e-2, 6.569778e-2, -1.07254684e-1, -1.07254684e-1,
            -6.04245e-38, 1.2477418e-38, -6.04245e-38, 1.2477418e-38, -6.04245e-38, 1.2477418e-38,
            0e0, 0e0, 0e0, 0e0, 0e0, 0e0,
            0e0, 0e0, 0e0, 0e0,
        ],
    ),
    (
        "builtin:filter",
        r#"{"type": 2.0, "cutoff": 900.0, "resonance": 0.8}"#,
        [
            3.0424953e-1, 3.035408e-1, 2.5966504e-1, 3.817881e-1, 2.9940635e-2, 7.3827947e-3,
            -9.648601e-3, 1.4148916e-2, 2.1572005e-2, -2.766359e-2, 1.4481327e-2, -6.707126e-2,
            -3.078955e-2, 2.5948036e-2, -8.099044e-1, -8.0872667e-1, 8.671955e-1, 8.671962e-1,
            -1.2900281e-3, -1.3918317e-3, 1.4141012e-27, 1.5540736e-27, 0e0, 0e0,
            0e0, 0e0, 0e0, 0e0, 0e0, 0e0,
            0e0, 0e0, 0e0, 0e0,
        ],
    ),
    (
        "builtin:filter",
        r#"{"type": 3.0, "cutoff": 1000.0}"#,
        [
            1.5633743e-1, 1.10871635e-1, -1.8502076e-1, 6.0384165e-2, 1.2318354e-2, -8.022859e-3,
            -1.4538499e-2, 1.4969806e-2, 2.71131e-1, 2.0934577e-1, -4.3914515e-1, -9.593265e-2,
            1.5930685e-1, -1.7242451e-1, 9.6639834e-2, 9.9081956e-2, -2.273473e-2, -2.2734655e-2,
            -8.9877185e-30, -7.657777e-29, 0e0, 0e0, 0e0, 0e0,
            0e0, 0e0, 0e0, 0e0, 0e0, 0e0,
            0e0, 0e0, 0e0, 0e0,
        ],
    ),
    (
        "builtin:limiter",
        r#"{"gain": 12.0, "ceiling": -1.0}"#,
        [
            4.1297373e-1, 3.195919e-1, 4.182748e-1, 5.8009066e-2, 1.7287571e-2, 7.1752e-2,
            2.2576769e-1, -2.3232853e-1, 2.252156e-2, 3.0019458e-2, -6.631806e-1, 6.0022336e-1,
            8.696124e-1, -6.219829e-2, 1.5763883e-1, 1.5729646e-1, -4.742187e-1, -4.742187e-1,
            0e0, 0e0, 0e0, 0e0, 0e0, 0e0,
            0e0, 0e0, 0e0, 0e0, 0e0, 0e0,
            0e0, 0e0, 0e0, 0e0,
        ],
    ),
    (
        "builtin:pump",
        r#"{}"#,
        [
            1.4845748e-1, 1.1494235e-1, -5.7508077e-2, 4.787982e-2, 1.5328711e-2, -6.26376e-3,
            -2.3253808e-2, 2.736509e-2, 1.9072145e-1, 1.2714763e-1, -4.170545e-1, -1.5164104e-1,
            1.3446233e-1, -1.5778048e-1, -1.4232285e-3, 1.676648e-3, 9.907214e-2, 9.907214e-2,
            0e0, 0e0, 0e0, 0e0, 0e0, 0e0,
            0e0, 0e0, 0e0, 0e0, 0e0, 0e0,
            0e0, 0e0, 0e0, 0e0,
        ],
    ),
    (
        "builtin:saturator",
        r#"{"drive": 18.0, "shape": 2.0}"#,
        [
            2.5556752e-1, 2.3148397e-1, -3.3929655e-1, 2.6454863e-1, 4.6914842e-2, -1.9274386e-2,
            -6.527122e-2, 7.604261e-2, 3.4804824e-1, 3.240939e-1, -4.985518e-1, -3.559069e-1,
            2.7986e-1, -3.573923e-1, -4.9046804e-3, 5.777675e-3, 2.3447986e-1, 2.3447986e-1,
            0e0, 0e0, 0e0, 0e0, 0e0, 0e0,
            0e0, 0e0, 0e0, 0e0, 0e0, 0e0,
            0e0, 0e0, 0e0, 0e0,
        ],
    ),
    (
        "builtin:utility",
        r#"{"gain": -3.0, "pan": -0.4, "width": 1.5}"#,
        [
            1.8275087e-1, 7.110826e-2, -1.9226813e-1, 9.8242894e-2, 2.0197174e-2, -5.7901796e-3,
            -3.2080866e-2, 1.8217523e-2, 2.8246802e-1, 7.749788e-2, -4.4574925e-1, -4.007089e-2,
            1.8513533e-1, -1.04930595e-1, -2.3978343e-3, 1.3626078e-3, 8.92078e-2, 4.5453634e-2,
            0e0, 0e0, 0e0, 0e0, 0e0, 0e0,
            0e0, 0e0, 0e0, 0e0, 0e0, 0e0,
            0e0, 0e0, 0e0, 0e0,
        ],
    ),
];

#[test]
fn a_song_of_format_1_renders_to_the_samples_it_rendered_to_before() {
    let folder = folder();
    let path = file(&folder, "source.wav");
    trill_media::write_wav(&path, &source(), WavFormat::Float32).unwrap();
    let path = path.display().to_string().replace('\\', "/");
    for (uri, params, expected) in OLD {
        let print = print_of(&old_file(&path, uri, params));
        // A level off by a hundredth of a dB, or a sample that has moved,
        // is far outside this; what a compiler may do to the last bit of a
        // coefficient is far inside it.
        for (index, (got, expected)) in print.iter().zip(expected).enumerate() {
            assert!(
                (got - expected).abs() <= 1e-5 + 1e-4 * expected.abs(),
                "{uri} with {params}: number {index} is {got:e}, and was {expected:e}"
            );
        }
    }
}
