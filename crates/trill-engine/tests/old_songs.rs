//! A song saved before the built-in effects were rebuilt must sound as it
//! did.
//!
//! Each case here is a project file of format 1, as the builds before
//! format 2 wrote it: one audio track with one effect, set as such a file
//! set it (only what was changed from the defaults of the day is in the
//! file). It is opened as any file is and rendered, and the render is held
//! to numbers that the last build with the old effects (commit 724556d)
//! wrote down from the same file: the level of each side, and sixteen
//! samples of each from the first to the tail. Further down are the files
//! of formats 2, 3 and 5, each with the numbers of the last build that
//! wrote that format.
//!
//! The numbers are never to be written down again from a newer build: that
//! would make the test agree with whatever the effects have become.

mod common;

use common::*;
use std::collections::BTreeMap;
use std::sync::Arc;
use trill_core::audio::AudioBuffer;
use trill_core::controllers::ControllerKind;
use trill_core::device::DeviceFactory;
use trill_core::project::{AudioContent, AutomationPoint, AutomationTarget, FORMAT_VERSION};
use trill_core::travel::Travel;
use trill_core::{ChainRef, DeviceId, Note, Project, TrackId};
use trill_dsp::{BuiltinFactory, Builtins, drumrack, sampler};
use trill_engine::render::{RenderOptions, render};
use trill_media::{MediaCache, WavFormat};

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
    print_with(file, &BuiltinFactory)
}

/// The same, with a factory that can read the files of sample players.
fn print_with(file: &str, factory: &dyn DeviceFactory) -> Print {
    let project = Project::from_json(file).unwrap();
    let options = RenderOptions { sample_rate: SR as u32, max_tail_seconds: 2.0, ..RenderOptions::default() };
    let out = render(&project, factory, &options).unwrap();
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
    let (path, _) = files(&folder);
    for (uri, params, expected) in OLD {
        let print = print_of(&old_file(&path, uri, params));
        assert_as_it_was(&print, expected, &format!("{uri} with {params}"));
    }
}

// ---- Format 2 --------------------------------------------------------------
//
// The same for a file of format 2, the last before the sample players'
// velocity law, the synth's level, the saturator's level and the lowest
// value of a level changed. Commit e0bd78a wrote these numbers down: the
// effects on the source above, and each instrument playing `played`.

/// What is on the one track of a file of format 2.
#[derive(Copy, Clone, Debug)]
enum Plays {
    /// The source, through the device.
    Effect,
    /// The device, playing [`played`] with the pitch wheel of [`wheel`].
    Synth,
    /// The same, the device a sampler with [`hit`] as its file.
    Sampler,
    /// The same, a drum rack with [`hit`] on three of its pads.
    Rack,
}

/// The notes an instrument plays: three of the rack's pads and keys across
/// the keyboard, hard and soft, apart and together.
fn played() -> Vec<Note> {
    [
        (36, 0.0, 0.5, 127),
        (60, 0.25, 0.4, 100),
        (42, 0.5, 0.25, 75),
        (64, 1.0, 0.45, 75),
        (67, 1.0, 0.45, 60),
        (48, 1.5, 0.5, 46),
        (42, 1.75, 0.2, 110),
        (72, 2.0, 0.25, 110),
        (36, 2.5, 0.3, 20),
        (55, 2.5, 1.0, 20),
        (42, 3.0, 0.2, 100),
        (60, 3.0, 0.75, 90),
        (48, 3.5, 0.4, 64),
    ]
    .into_iter()
    .map(|(pitch, start, length, velocity)| Note { pitch, start, length, velocity })
    .collect()
}

/// Where the pitch wheel is while they play: down, up and back to rest.
fn wheel() -> Vec<AutomationPoint> {
    [(1.0, 0.0), (1.4, -0.5), (2.0, 1.0), (2.2, 0.0)]
        .into_iter()
        .map(|(beat, value)| AutomationPoint::new(beat, value))
        .collect()
}

/// A drum hit of a third of a second for the sample players: a thump under
/// a burst of noise, not the same on both sides.
fn hit() -> AudioBuffer {
    let rate = 44_100.0;
    let frames = (0.33 * rate) as usize;
    let (mut left, mut right) = (vec![0.0f32; frames], vec![0.0f32; frames]);
    let mut seed = 0x1F12_3BB5u32;
    let mut noise = || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        (seed >> 8) as f64 / 8_388_608.0 - 1.0
    };
    for n in 0..frames {
        let t = n as f64 / rate;
        let thump = 0.7 * (-t / 0.09).exp() * (std::f64::consts::TAU * 150.0 * t).sin();
        let (a, b) = (noise(), noise());
        let fall = (-t / 0.03).exp();
        left[n] = (thump + 0.25 * a * fall) as f32;
        right[n] = (thump + 0.25 * (0.5 * a + 0.5 * b) * fall) as f32;
    }
    AudioBuffer { sample_rate: rate, left, right }
}

/// A project file of format 2 with one track: `plays`, the device `uri`
/// with `params` as the file has them, `source` and `hit` being the paths
/// of the two files.
fn later_file(plays: Plays, uri: &str, params: &[(&str, f64)], source: &str, hit: &str) -> String {
    let mut project = Project::new("old");
    project.master.devices.clear();
    project.set_tempo(120.0).unwrap();
    let track = project.add_track("Track");
    let device = project.add_device(ChainRef::Track(track), uri, "Device", None).unwrap();
    let params: BTreeMap<String, f64> = params.iter().map(|(id, value)| (id.to_string(), *value)).collect();
    project.device_mut(device).unwrap().params = params;
    project.device_mut(device).unwrap().state = match plays {
        Plays::Sampler => Some(sampler::State::new(hit).to_bytes()),
        Plays::Rack => {
            let mut state = drumrack::State::default();
            for pad in [36, 42, 48] {
                state.set_pad(pad, hit).unwrap();
            }
            Some(state.to_bytes())
        }
        Plays::Effect | Plays::Synth => None,
    };
    match plays {
        Plays::Effect => {
            project.add_audio_clip(track, "clip", 0.0, 4.0, AudioContent::new(source)).unwrap();
        }
        Plays::Synth | Plays::Sampler | Plays::Rack => {
            let clip = project.add_midi_clip(track, "clip", 0.0, 4.0).unwrap();
            let clip = project.clip_mut(clip).unwrap();
            clip.add_notes(&played()).unwrap();
            clip.set_controller(ControllerKind::PitchBend, wheel()).unwrap();
        }
    }
    // The file as the build of the day wrote it: this one's, but for the
    // number that says which format it is.
    let file = project.to_json();
    let now = format!("\"format_version\": {FORMAT_VERSION}");
    assert!(file.contains(&now), "{file}");
    file.replace(&now, "\"format_version\": 2")
}

/// A case of format 2: what is on the track, the device, what the file set
/// on it, and what it rendered to.
type Later = (Plays, &'static str, &'static [(&'static str, f64)], Print);

#[rustfmt::skip]
const LATER: &[Later] = &[
    (
        Plays::Synth,
        "builtin:polysynth",
        &[],
        [
            8.083367e-2, 8.083367e-2, -5.99122e-2, -5.99122e-2, 1.2960912e-1, 1.2960912e-1,
            2.1975977e-2, 2.1975977e-2, -1.2794988e-1, -1.2794988e-1, 6.697058e-2, 6.697058e-2,
            -1.4385544e-1, -1.4385544e-1, -1.8616882e-3, -1.8616882e-3, -3.0625854e-2, -3.0625854e-2,
            7.9085104e-2, 7.9085104e-2, 5.6153733e-2, 5.6153733e-2, -6.602579e-2, -6.602579e-2,
            1.457565e-3, 1.457565e-3, -4.1970907e-5, -4.1970907e-5, 0e0, 0e0,
            0e0, 0e0, 0e0, 0e0,
        ],
    ),
    (
        Plays::Synth,
        "builtin:polysynth",
        &[("osc1_wave", 0.0), ("attack", 0.001), ("decay", 20.0), ("sustain", 1.0), ("release", 0.12), ("velocity_sens", 0.6), ("volume", -10.0)],
        [
            3.805487e-2, 3.805487e-2, 5.4078966e-2, 5.4078966e-2, -1.0065698e-1, -1.0065698e-1,
            2.850383e-4, 2.850383e-4, 6.9638185e-2, 6.9638185e-2, -3.082618e-2, -3.082618e-2,
            8.423329e-2, 8.423329e-2, -1.9598451e-4, -1.9598451e-4, 3.141987e-2, 3.141987e-2,
            3.450108e-3, 3.450108e-3, -3.3882793e-2, -3.3882793e-2, 1.329844e-2, 1.329844e-2,
            -1.25112e-5, -1.25112e-5, 0e0, 0e0, 0e0, 0e0,
            0e0, 0e0, 0e0, 0e0,
        ],
    ),
    (
        Plays::Synth,
        "builtin:polysynth",
        &[("noise_level", 1.0), ("osc_mix", 1.0), ("osc2_wave", 0.0), ("osc2_semitones", -24.0), ("filter_type", 3.0), ("cutoff", 8000.0), ("resonance", 0.15), ("attack", 0.002), ("release", 0.2), ("velocity_sens", 0.0), ("volume", -20.0)],
        [
            6.439263e-3, 6.439263e-3, 4.521841e-3, 4.521841e-3, 9.3962075e-3, 9.3962075e-3,
            1.4412231e-3, 1.4412231e-3, 5.9771026e-3, 5.9771026e-3, -3.2898467e-3, -3.2898467e-3,
            7.5462097e-3, 7.5462097e-3, -1.6855148e-3, -1.6855148e-3, 1.2947279e-2, 1.2947279e-2,
            2.0840413e-4, 2.0840413e-4, 1.0019033e-2, 1.0019033e-2, 4.6904446e-3, 4.6904446e-3,
            2.0700883e-5, 2.0700883e-5, 0e0, 0e0, 0e0, 0e0,
            0e0, 0e0, 0e0, 0e0,
        ],
    ),
    (
        Plays::Synth,
        "builtin:polysynth",
        &[("osc2_wave", 3.0), ("osc_mix", 0.4), ("osc2_detune", 6.0), ("sub_level", 0.5), ("noise_level", 0.2), ("unison", 3.0), ("fm_amount", 0.3), ("filter_type", 1.0), ("cutoff", 520.0), ("resonance", 0.25), ("filter_env", 2.5), ("mono", 1.0), ("glide", 0.03), ("velocity_sens", 0.4), ("volume", -48.0)],
        [
            3.0499094e-4, 3.0823651e-4, -6.178745e-4, -4.9732404e-4, 3.0950384e-4, 2.3782109e-4,
            -1.9883462e-4, 2.315149e-5, 7.360397e-5, -6.807641e-5, -1.6782671e-4, -2.4161545e-4,
            2.2940878e-4, 1.19908735e-4, 2.0028556e-4, 3.6270096e-4, -1.2637548e-4, -2.2828029e-4,
            -8.018735e-4, -7.034482e-4, -4.939016e-5, 1.2229921e-4, -1.4096271e-4, -1.1888988e-4,
            -4.4296035e-6, -1.961392e-6, 0e0, 0e0, 0e0, 0e0,
            0e0, 0e0, 0e0, 0e0,
        ],
    ),
    (
        Plays::Sampler,
        "builtin:sampler",
        &[],
        [
            1.1526465e-1, 1.1367371e-1, -3.5735133e-1, -3.7449113e-1, -1.828607e-1, -1.9405667e-1,
            -6.879738e-3, -6.76289e-3, -2.6170248e-2, -2.7028352e-2, -2.0898534e-2, -2.2840545e-2,
            -1.8232125e-1, -1.7883717e-1, -2.8078293e-4, -2.807948e-4, 1.4796767e-1, 1.3198353e-1,
            4.124061e-1, 3.409136e-1, 2.411041e-2, 2.4148028e-2, -2.1352509e-2, -2.1452673e-2,
            0e0, 0e0, 0e0, 0e0, 0e0, 0e0,
            0e0, 0e0, 0e0, 0e0,
        ],
    ),
    (
        Plays::Sampler,
        "builtin:sampler",
        &[("one_shot", 1.0), ("release", 0.02), ("velocity_sens", 0.8), ("volume", -12.0)],
        [
            6.044964e-2, 5.981674e-2, -1.7909993e-1, -1.8769018e-1, -9.190254e-2, -9.749288e-2,
            -1.0319605e-1, -1.0255108e-1, -5.599024e-2, -5.6715034e-2, -8.55358e-3, -9.184881e-3,
            -9.957921e-2, -9.774664e-2, 5.031776e-2, 4.9591165e-2, 5.9702665e-2, 5.539654e-2,
            2.0427711e-1, 1.7177139e-1, 4.1903548e-2, 4.121915e-2, -1.3800663e-3, -1.3684699e-3,
            0e0, 0e0, 0e0, 0e0, 0e0, 0e0,
            0e0, 0e0, 0e0, 0e0,
        ],
    ),
    (
        Plays::Sampler,
        "builtin:sampler",
        &[("velocity_sens", 1.0), ("volume", -48.0), ("transpose", 5.0)],
        [
            6.7055033e-4, 6.622287e-4, 3.5825017e-4, 4.2820096e-4, 1.1438535e-4, 9.7655044e-5,
            2.2050683e-5, 2.2180098e-5, -3.0447464e-4, -3.0254354e-4, 4.5905113e-6, 6.711892e-5,
            1.0565036e-3, 1.2212646e-3, 0e0, 0e0, 4.7098772e-5, 4.23847e-5,
            8.6193706e-4, 1.1322729e-3, -6.339215e-5, -6.339033e-5, -1.608566e-4, -1.6087164e-4,
            0e0, 0e0, 0e0, 0e0, 0e0, 0e0,
            0e0, 0e0, 0e0, 0e0,
        ],
    ),
    (
        Plays::Rack,
        "builtin:drumrack",
        &[],
        [
            1.1586747e-1, 1.14572875e-1, 3.7532258e-1, 3.5259074e-1, 6.9339894e-2, 6.921044e-2,
            -4.6114374e-2, -4.5608625e-2, 0e0, 0e0, 1.0411567e-1, 8.449789e-2,
            1.7864557e-2, 9.624408e-3, -2.0777488e-2, -2.0798424e-2, -6.702592e-4, -9.2804886e-4,
            3.273506e-1, 3.055559e-1, 4.7927245e-2, 4.800202e-2, -2.9988738e-2, -2.9848939e-2,
            0e0, 0e0, 0e0, 0e0, 0e0, 0e0,
            0e0, 0e0, 0e0, 0e0,
        ],
    ),
    (
        Plays::Rack,
        "builtin:drumrack",
        &[("velocity_sens", 0.5), ("volume", -6.0), ("pad42_level", 0.7), ("pad48_tune", -5.0)],
        [
            6.609665e-2, 6.536169e-2, 1.8810686e-1, 1.7671396e-1, 3.475227e-2, 3.4687385e-2,
            -2.5913702e-2, -2.56295e-2, 0e0, 0e0, 1.5032767e-1, 1.3487041e-1,
            1.9865701e-2, 1.674034e-2, -7.5700083e-3, -7.5774593e-3, -2.8556166e-3, -3.9539207e-3,
            1.3321745e-1, 1.23920396e-1, 2.0439686e-2, 2.0471575e-2, 3.9861254e-2, 3.917985e-2,
            2.1700198e-3, 2.1783048e-3, 0e0, 0e0, 0e0, 0e0,
            0e0, 0e0, 0e0, 0e0,
        ],
    ),
    (
        Plays::Effect,
        "builtin:saturator",
        &[],
        [
            2.1635906e-1, 1.7473346e-1, -2.0008089e-1, 1.6797978e-1, 2.3642804e-2, -9.664135e-3,
            -3.2872085e-2, 3.867318e-2, 3.716535e-1, 2.62163e-1, -4.929621e-1, -2.1445984e-1,
            1.8551177e-1, -2.1579817e-1, -2.4582297e-3, 2.8959366e-3, 1.3940863e-1, 1.3940863e-1,
            0e0, 0e0, 0e0, 0e0, 0e0, 0e0,
            0e0, 0e0, 0e0, 0e0, 0e0, 0e0,
            0e0, 0e0, 0e0, 0e0,
        ],
    ),
    (
        Plays::Effect,
        "builtin:saturator",
        &[("drive", 9.0), ("output", -8.0)],
        [
            9.326295e-2, 7.75462e-2, -9.219856e-2, 7.801157e-2, 1.1182484e-2, -4.5723147e-3,
            -1.5542348e-2, 1.828016e-2, 1.6056924e-1, 1.185459e-1, -1.9876029e-1, -9.843713e-2,
            8.580217e-2, -9.901387e-2, -1.1631098e-3, 1.3702085e-3, 6.5119244e-2, 6.5119244e-2,
            0e0, 0e0, 0e0, 0e0, 0e0, 0e0,
            0e0, 0e0, 0e0, 0e0, 0e0, 0e0,
            0e0, 0e0, 0e0, 0e0,
        ],
    ),
    (
        Plays::Effect,
        "builtin:saturator",
        &[("drive", 24.0), ("shape", 1.0), ("mix", 0.5)],
        [
            2.4805133e-1, 2.1795754e-1, -2.805786e-1, 2.6334035e-1, 5.8975052e-2, -2.4098929e-2,
            -8.202523e-2, 9.652732e-2, 3.8429284e-1, 3.1540093e-1, -4.8244306e-1, -2.8845188e-1,
            2.727008e-1, -2.8919e-1, -6.129598e-3, 7.2210324e-3, 2.4832784e-1, 2.4832784e-1,
            0e0, 0e0, 0e0, 0e0, 0e0, 0e0,
            0e0, 0e0, 0e0, 0e0, 0e0, 0e0,
            0e0, 0e0, 0e0, 0e0,
        ],
    ),
    (
        Plays::Effect,
        "builtin:utility",
        &[("gain", -70.0)],
        [
            1.5525827e-2, 1.0945996e-2, -1.8246645e-2, 1.51917115e-2, 5.29505e-6, -2.1637125e-6,
            -7.364601e-6, 8.666665e-6, 9.24302e-5, 6.1620136e-5, -1.3632528e-4, -4.9567883e-5,
            4.2523632e-5, -4.9897986e-5, -5.503434e-7, 6.483373e-7, 3.162347e-5, 3.162347e-5,
            0e0, 0e0, 0e0, 0e0, 0e0, 0e0,
            0e0, 0e0, 0e0, 0e0, 0e0, 0e0,
            0e0, 0e0, 0e0, 0e0,
        ],
    ),
    (
        Plays::Effect,
        "builtin:limiter",
        &[("gain", 7.0)],
        [
            3.6841285e-1, 2.8413364e-1, 4.06743e-1, 5.6409754e-2, 1.0213294e-2, 4.2390242e-2,
            1.2786524e-1, -1.3158101e-1, 1.8668354e-2, 2.488344e-2, -7.093116e-1, 6.41975e-1,
            6.338586e-1, -4.53362e-2, 1.4472194e-1, 1.4440762e-1, -4.051579e-1, -4.051579e-1,
            0e0, 0e0, 0e0, 0e0, 0e0, 0e0,
            0e0, 0e0, 0e0, 0e0, 0e0, 0e0,
            0e0, 0e0, 0e0, 0e0,
        ],
    ),
    (
        Plays::Effect,
        "builtin:limiter",
        &[("gain", 12.0), ("ceiling", -1.0), ("release", 300.0)],
        [
            3.982748e-1, 3.1016126e-1, 3.8202715e-1, 5.2982006e-2, 1.2615167e-2, 5.23592e-2,
            1.876456e-1, -1.9309862e-1, 2.2410227e-2, 2.9871061e-2, -6.5751106e-1, 5.95092e-1,
            7.7260023e-1, -5.5259574e-2, 1.5695553e-1, 1.5661463e-1, -4.742187e-1, -4.742187e-1,
            0e0, 0e0, 0e0, 0e0, 0e0, 0e0,
            0e0, 0e0, 0e0, 0e0, 0e0, 0e0,
            0e0, 0e0, 0e0, 0e0,
        ],
    ),
];

/// The two files the cases play, written into `folder`: the source and the
/// hit, as the paths a project file holds.
fn files(folder: &tempfile::TempDir) -> (String, String) {
    let written = |name: &str, audio: AudioBuffer| {
        let path = file(folder, name);
        trill_media::write_wav(&path, &audio, WavFormat::Float32).unwrap();
        path.display().to_string().replace('\\', "/")
    };
    (written("source.wav", source()), written("hit.wav", hit()))
}

/// Holds a render to what it was: a level off by a hundredth of a dB, or a
/// sample that has moved, is far outside this; what a compiler may do to
/// the last bit of a coefficient is far inside it.
fn assert_as_it_was(print: &Print, expected: &Print, what: &str) {
    for (index, (got, expected)) in print.iter().zip(expected).enumerate() {
        assert!(
            (got - expected).abs() <= 1e-5 + 1e-4 * expected.abs(),
            "{what}: number {index} is {got:e}, and was {expected:e}"
        );
    }
}

#[test]
fn a_song_of_format_2_renders_to_the_samples_it_rendered_to_before() {
    let folder = folder();
    let (source, hit) = files(&folder);
    let factory = Builtins::with_loader(Arc::new(MediaCache::default()));
    for (plays, uri, params, expected) in LATER {
        let print = print_with(&later_file(*plays, uri, params, &source, &hit), &factory);
        assert!(expected[0] > 0.0, "{uri} with {params:?} was not silent");
        assert_as_it_was(&print, expected, &format!("{uri} with {params:?}"));
    }
}

// ---- Format 3 --------------------------------------------------------------
//
// And for a file of format 3, the first whose envelopes say how their lines
// travel and the last with the devices as they were before format 4. The
// build that wrote that format (commit f2a63be) wrote these numbers down.
// Here the two things an older file is read by meet: the devices keep the
// sound they had, and an envelope on a parameter whose scale has moved
// since keeps the curve it played.

/// A track with `devices` on it, each set to what its file sets.
fn track(project: &mut Project, name: &str, devices: &[(&str, &[(&str, f64)])]) -> (TrackId, Vec<DeviceId>) {
    let track = project.add_track(name);
    let ids = devices
        .iter()
        .map(|(uri, params)| {
            let id = project.add_device(ChainRef::Track(track), *uri, *uri, None).unwrap();
            let params: BTreeMap<String, f64> = params.iter().map(|(id, value)| (id.to_string(), *value)).collect();
            project.device_mut(id).unwrap().params = params;
            id
        })
        .collect();
    (track, ids)
}

fn empty() -> Project {
    let mut project = Project::new("old");
    project.master.devices.clear();
    project.set_tempo(120.0).unwrap();
    project
}

/// A sampler beside a poly synth through a saturator, each playing
/// [`played`].
fn band(hit: &str) -> Project {
    let mut project = empty();
    let (hits, devices) = track(&mut project, "Hits", &[("builtin:sampler", &[])]);
    project.device_mut(devices[0]).unwrap().state = Some(sampler::State::new(hit).to_bytes());
    let synth: &[(&str, f64)] = &[("osc1_wave", 0.0), ("noise_level", 0.3), ("volume", -3.0)];
    let (bass, _) =
        track(&mut project, "Bass", &[("builtin:polysynth", synth), ("builtin:saturator", &[("drive", 12.0)])]);
    for part in [hits, bass] {
        let clip = project.add_midi_clip(part, "clip", 0.0, 4.0).unwrap();
        let clip = project.clip_mut(clip).unwrap();
        clip.add_notes(&played()).unwrap();
        clip.set_controller(ControllerKind::PitchBend, wheel()).unwrap();
    }
    project
}

/// A held note on a poly synth whose volume an envelope moves along the
/// travel `volume`, through a utility whose gain one fades along `gain`:
/// down to the bottom of the volume's range of the day and up over the
/// top of today's, and out to where a gain was 70 dB down.
fn faded(volume: Travel, gain: Travel) -> Project {
    let mut project = empty();
    let synth: &[(&str, f64)] = &[("osc1_wave", 0.0), ("sustain", 1.0), ("release", 0.05), ("velocity_sens", 0.0)];
    let (lead, devices) = track(&mut project, "Lead", &[("builtin:polysynth", synth), ("builtin:utility", &[])]);
    let clip = project.add_midi_clip(lead, "clip", 0.0, 4.0).unwrap();
    project.clip_mut(clip).unwrap().add_notes(&[Note { pitch: 57, start: 0.0, length: 4.0, velocity: 100 }]).unwrap();
    let points =
        |points: &[(f64, f64)]| points.iter().map(|(beat, value)| AutomationPoint::new(*beat, *value)).collect();
    let target = |device: DeviceId, param: &str| AutomationTarget::DeviceParam { device, param: param.to_string() };
    let line = points(&[(0.0, 0.0), (1.5, -30.0), (2.5, -48.0), (3.0, 6.0), (4.0, -12.0)]);
    project.write_automation(target(devices[0], "volume"), line, volume).unwrap();
    let line = points(&[(0.0, 0.0), (3.5, 0.0), (4.0, -70.0)]);
    project.write_automation(target(devices[1], "gain"), line, gain).unwrap();
    project
}

/// `project` as a build of `format` saved it.
fn of_format(project: &Project, format: u32) -> String {
    let file = project.to_json();
    let now = format!("\"format_version\": {FORMAT_VERSION}");
    assert!(file.contains(&now), "{file}");
    file.replace(&now, &format!("\"format_version\": {format}"))
}

/// What the three songs of format 3 rendered to: the band, the fade along
/// the faders of the day, and the same fade straight in decibels.
#[rustfmt::skip]
const THIRD: [Print; 3] = [
    [
        1.6985302e-1, 1.6874328e-1, -1.1886214e-1, -1.3600193e-1, -4.793711e-1, -4.905671e-1,
        -2.6985344e-2, -2.6868496e-2, 2.1439037e-1, 2.1353227e-1, -1.710221e-1, -1.7296411e-1,
        1.3515893e-1, 1.3864301e-1, 2.4152678e-2, 2.4152664e-2, 2.8187498e-1, 2.6589084e-1,
        3.2488525e-1, 2.5339276e-1, -1.4277056e-1, -1.4273293e-1, -3.633645e-2, -3.6436614e-2,
        -7.6887677e-3, -7.6887677e-3, 8.434271e-5, 8.434271e-5, 0e0, 0e0,
        0e0, 0e0, 0e0, 0e0,
    ],
    [
        9.604933e-2, 9.604933e-2, -8.7527804e-2, -8.7527804e-2, -9.750474e-2, -9.750474e-2,
        2.4662694e-2, 2.4662694e-2, 1.85178e-2, 1.85178e-2, -3.5000711e-3, -3.5000711e-3,
        -3.6042812e-3, -3.6042812e-3, 1.3156374e-3, 1.3156374e-3, 2.5477238e-2, 2.5477238e-2,
        -2.534566e-1, -2.534566e-1, -1.5738381e-1, -1.5738381e-1, 4.9167653e-3, 4.9167653e-3,
        0e0, 0e0, 0e0, 0e0, 0e0, 0e0,
        0e0, 0e0, 0e0, 0e0,
    ],
    [
        8.626552e-2, 8.626552e-2, -8.555548e-2, -8.555548e-2, -7.558947e-2, -7.558947e-2,
        1.684298e-2, 1.684298e-2, 1.3125865e-2, 1.3125865e-2, -3.37307e-3, -3.37307e-3,
        -2.493017e-3, -2.493017e-3, 7.8553386e-4, 7.8553386e-4, 5.5886456e-3, 5.5886456e-3,
        -2.5007606e-1, -2.5007606e-1, -1.4293873e-1, -1.4293873e-1, 8.286766e-4, 8.286766e-4,
        0e0, 0e0, 0e0, 0e0, 0e0, 0e0,
        0e0, 0e0, 0e0, 0e0,
    ],
];

#[test]
fn a_song_of_format_3_renders_to_the_samples_it_rendered_to_before() {
    let folder = folder();
    let (_, hit) = files(&folder);
    let factory = Builtins::with_loader(Arc::new(MediaCache::default()));
    // How that build had an envelope on these two parameters travel: along
    // a fader down to the lowest value each had then.
    let (volume, gain) = (Travel::Fader { floor: -48.0 }, Travel::Fader { floor: -70.0 });
    let songs = [
        ("a sampler beside a synth through a saturator", band(&hit)),
        ("an envelope on a synth's volume, along its fader", faded(volume, gain)),
        ("the same envelope, straight in decibels", faded(Travel::Linear, Travel::Linear)),
    ];
    for ((what, song), expected) in songs.iter().zip(&THIRD) {
        assert_as_it_was(&print_with(&of_format(song, 3), &factory), expected, what);
    }
    // The two fades are not the same curve, so each holds its own line:
    // a moved envelope that bent would be off by decibels, not by a bit.
    assert!((THIRD[1][0] / THIRD[2][0] - 1.0).abs() > 0.1);
}

// ---- Format 5 --------------------------------------------------------------
//
// And for a file of format 5, the last with the reverb, the echo, the
// compressor, the pitch shifter and the synth's volume as they were before
// format 6. The build that wrote that format (commit 9cd3a13) wrote these
// numbers down.

/// The source through one device, as a file sets it.
fn through_device(source: &str, uri: &str, params: &[(&str, f64)]) -> Project {
    let mut project = empty();
    let (audio, _) = track(&mut project, "Audio", &[(uri, params)]);
    project.add_audio_clip(audio, "clip", 0.0, 4.0, AudioContent::new(source)).unwrap();
    project
}

/// A track that plays [`played`] on a sampler of `hit`, with [`wheel`].
fn hits(project: &mut Project, hit: &str, devices: &[(&str, &[(&str, f64)])]) -> TrackId {
    let mut all: Vec<(&str, &[(&str, f64)])> = vec![("builtin:sampler", &[("one_shot", 1.0)])];
    all.extend_from_slice(devices);
    let (part, ids) = track(project, "Hits", &all);
    project.device_mut(ids[0]).unwrap().state = Some(sampler::State::new(hit).to_bytes());
    let clip = project.add_midi_clip(part, "clip", 0.0, 4.0).unwrap();
    project.clip_mut(clip).unwrap().add_notes(&played()).unwrap();
    part
}

/// A track that holds a chord on a poly synth set to `synth`, through
/// `devices`. Returns the devices, the synth first.
fn held(project: &mut Project, synth: &[(&str, f64)], devices: &[(&str, &[(&str, f64)])]) -> Vec<DeviceId> {
    let mut all: Vec<(&str, &[(&str, f64)])> = vec![("builtin:polysynth", synth)];
    all.extend_from_slice(devices);
    let (part, ids) = track(project, "Held", &all);
    let clip = project.add_midi_clip(part, "clip", 0.0, 4.0).unwrap();
    let chord: Vec<Note> =
        [57, 64, 69].into_iter().map(|pitch| Note { pitch, start: 0.0, length: 4.0, velocity: 100 }).collect();
    project.clip_mut(clip).unwrap().add_notes(&chord).unwrap();
    ids
}

fn line(project: &mut Project, device: DeviceId, param: &str, travel: Travel, points: &[(f64, f64)]) {
    let target = AutomationTarget::DeviceParam { device, param: param.to_string() };
    let points = points.iter().map(|(beat, value)| AutomationPoint::new(*beat, *value)).collect();
    project.write_automation(target, points, travel).unwrap();
}

/// A held chord under a compressor that listens to a track of hits, which
/// is not heard itself.
fn ducked(hit: &str) -> Project {
    let mut project = empty();
    let key = hits(&mut project, hit, &[]);
    project.track_mut(key).unwrap().mute = true;
    let duck: &[(&str, f64)] =
        &[("threshold", -28.0), ("ratio", 5.0), ("attack", 1.0), ("release", 90.0), ("key_high_cut", 400.0)];
    let devices = held(&mut project, &[("sustain", 1.0)], &[("builtin:compressor", duck)]);
    project.set_sidechain(devices[1], Some(key)).unwrap();
    project
}

/// A held chord that a pitch shifter is opened on and drawn two octaves
/// down over a beat.
fn dived() -> Project {
    let mut project = empty();
    let devices = held(&mut project, &[("sustain", 1.0)], &[("builtin:shifter", &[("mix", 0.0)])]);
    line(&mut project, devices[1], "mix", Travel::Linear, &[(0.0, 0.0), (1.95, 0.0), (2.0, 1.0)]);
    line(&mut project, devices[1], "pitch", Travel::Linear, &[(0.0, 0.0), (2.05, 0.0), (3.0, -24.0)]);
    project
}

/// A held chord whose volume an envelope swells and fades to silence,
/// along `travel`.
fn swelled(travel: Travel) -> Project {
    let mut project = empty();
    let devices = held(&mut project, &[("sustain", 1.0), ("osc1_wave", 0.0)], &[]);
    line(
        &mut project,
        devices[0],
        "volume",
        travel,
        &[(0.0, -30.0), (1.0, -10.0), (2.5, -40.0), (3.0, -70.0), (3.5, -6.0)],
    );
    project
}

/// The songs of format 5, by what each holds to.
fn fifth(source: &str, hit: &str) -> Vec<(&'static str, Project)> {
    let reverb = |params: &[(&str, f64)]| through_device(source, "builtin:reverb", params);
    let delay = |params: &[(&str, f64)]| through_device(source, "builtin:delay", params);
    let compressor = |params: &[(&str, f64)]| through_device(source, "builtin:compressor", params);
    let shifter = |params: &[(&str, f64)]| through_device(source, "builtin:shifter", params);
    let noisy = {
        let mut project = empty();
        hits(&mut project, hit, &[]);
        let (part, _) =
            track(&mut project, "Synth", &[("builtin:polysynth", &[("noise_level", 0.5), ("noise_width", 1.0)])]);
        let clip = project.add_midi_clip(part, "clip", 0.0, 4.0).unwrap();
        let clip = project.clip_mut(clip).unwrap();
        clip.add_notes(&played()).unwrap();
        clip.set_controller(ControllerKind::PitchBend, wheel()).unwrap();
        project
    };
    vec![
        ("a reverb as it comes", reverb(&[])),
        ("a room", reverb(&[("decay", 0.7), ("predelay", 6.0), ("size", 0.21), ("mix", 1.0), ("low_cut", 110.0)])),
        (
            "a hall",
            reverb(&[
                ("decay", 2.8),
                ("predelay", 20.0),
                ("size", 0.6),
                ("damping", 0.5),
                ("mix", 1.0),
                ("low_cut", 110.0),
            ]),
        ),
        (
            "a long reverb, narrow and moving",
            reverb(&[
                ("decay", 4.0),
                ("modulation", 0.8),
                ("mix", 0.35),
                ("width", 0.5),
                ("low_cut", 400.0),
                ("high_cut", 5000.0),
            ]),
        ),
        ("an echo as it comes", delay(&[])),
        (
            "a ping-pong echo",
            delay(&[
                ("sync", 1.0),
                ("time_beats", 0.75),
                ("feedback", 0.45),
                ("ping_pong", 1.0),
                ("mix", 1.0),
                ("low_cut", 318.2),
                ("high_cut", 10182.3),
            ]),
        ),
        (
            "an echo on each side",
            delay(&[("sync", 0.0), ("time_ms", 180.0), ("feedback", 0.6), ("ping_pong", 0.0), ("mix", 0.5)]),
        ),
        (
            "a compressor with automatic makeup",
            compressor(&[
                ("threshold", -24.0),
                ("ratio", 4.0),
                ("attack", 3.0),
                ("release", 80.0),
                ("auto_makeup", 1.0),
            ]),
        ),
        (
            "another, lower and faster",
            compressor(&[
                ("threshold", -30.0),
                ("ratio", 4.0),
                ("attack", 1.0),
                ("release", 80.0),
                ("auto_makeup", 1.0),
            ]),
        ),
        (
            "a bus compressor that lets go by itself",
            compressor(&[
                ("threshold", -10.0),
                ("ratio", 2.0),
                ("attack", 30.0),
                ("knee", 18.0),
                ("makeup", 1.0),
                ("detector", 1.0),
                ("auto_release", 1.0),
            ]),
        ),
        (
            "a compressor set by hand",
            compressor(&[("threshold", -24.0), ("ratio", 6.0), ("attack", 3.0), ("release", 120.0), ("makeup", 10.0)]),
        ),
        (
            "a track compressor that lets go by itself",
            compressor(&[("threshold", -20.0), ("ratio", 3.0), ("auto_release", 1.0)]),
        ),
        ("a chord ducked under hits it listens to", ducked(hit)),
        ("a pitch shifter an octave down", shifter(&[("pitch", -12.0)])),
        (
            "a pitch shifter a fifth up, half mixed, on a short window",
            shifter(&[("pitch", 7.0), ("mix", 0.5), ("window", 30.0)]),
        ),
        ("a chord drawn two octaves down", dived()),
        ("a synth's volume drawn along its fader", swelled(Travel::Fader { floor: -70.0 })),
        ("the same, straight in decibels", swelled(Travel::Linear)),
        ("a synth with noise beside a sampler", noisy),
    ]
}

/// What they rendered to, in the order [`fifth`] has them.
#[rustfmt::skip]
const FIFTH: [Print; 19] = [
    // a reverb as it comes
    [
        1.2487576e-1, 9.369102e-2, -1.456095e-1, 1.21230915e-1, 1.7791017e-3, 6.290332e-3,
        -3.2727312e-2, 3.1598184e-2, 3.07179e-1, 1.643636e-1, -4.2753652e-1, -2.334231e-1,
        7.8526735e-2, -3.688707e-2, 5.67325e-3, 8.0995545e-2, 7.262894e-2, -3.096625e-3,
        5.4949272e-2, -2.538193e-2, -3.1592753e-2, 5.0532427e-2, -2.7412546e-4, -1.2921012e-2,
        -7.0782253e-3, 2.4641426e-3, -1.5184189e-4, -4.8893704e-3, -5.477147e-4, 2.2243133e-3,
        -6.264941e-4, 2.0950544e-3, 1.006974e-3, -1.923849e-3,
    ],
    // a room
    [
        2.4334118e-2, 2.2823792e-2, 0e0, 0e0, 3.8653612e-3, 1.7410713e-3,
        -1.5469053e-3, 1.2879454e-3, -1.4219998e-2, -5.0088543e-2, 1.5747076e-2, 3.6362145e-2,
        2.7272122e-2, 4.0347584e-2, -4.5453224e-2, -1.7540518e-2, 4.320483e-2, 8.153635e-3,
        -2.2935841e-2, 5.397584e-3, 7.010493e-3, -3.3080778e-3, 1.102937e-3, 8.0599326e-5,
        1.4697916e-4, -1.2698163e-4, -3.043005e-6, -1.4876715e-6, 0e0, 0e0,
        0e0, 0e0, 0e0, 0e0,
    ],
    // a hall
    [
        4.875937e-2, 9.963664e-2, 0e0, 0e0, -4.136988e-2, -3.667916e-2,
        1.7562764e-2, -8.731279e-3, -1.9222887e-2, 3.6308557e-2, 1.324427e-1, -2.4928527e-1,
        -9.4876274e-2, -5.3555388e-2, -9.286055e-2, 3.3603054e-1, -5.958629e-2, -6.987414e-2,
        1.20702155e-1, -1.4695668e-1, -4.375379e-2, 8.186348e-2, -4.2718865e-2, -2.3151228e-2,
        -6.4993943e-3, 4.7408197e-2, 2.363555e-3, -3.1020546e-2, 4.54875e-3, 3.2328398e-3,
        -5.545939e-3, 7.486666e-3, 2.4277794e-3, -3.0151915e-4,
    ],
    // a long reverb, narrow and moving
    [
        1.2056145e-1, 8.48255e-2, -1.456095e-1, 1.21230915e-1, 2.460106e-3, -9.6708415e-3,
        -6.945077e-3, 3.3209678e-2, 2.9053664e-1, 1.7112944e-1, -4.3750528e-1, -1.4579266e-1,
        1.3500205e-1, -1.0284962e-1, 1.6448554e-3, -2.6242923e-2, 4.722578e-2, 1.2475668e-2,
        1.1245453e-2, 1.1726048e-2, 2.8606184e-2, 4.3278743e-2, -1.8398337e-2, -1.1615794e-2,
        4.5432555e-3, -1.31293675e-2, 4.358775e-3, 1.3571087e-2, -9.3285255e-3, 8.301351e-3,
        2.6618512e-3, 1.3360055e-3, -2.5163714e-3, -1.772475e-3,
    ],
    // an echo as it comes
    [
        1.4570792e-1, 1.1214071e-1, -1.456095e-1, 1.21230915e-1, 1.674405e-2, -6.842109e-3,
        1.7801391e-2, 2.7405798e-2, 2.8344643e-1, 1.948557e-1, -4.3045008e-1, -9.802877e-3,
        2.1118666e-1, -1.475191e-1, -7.1441114e-2, 1.2859178e-3, 5.6714635e-2, 1.717864e-1,
        -6.0462367e-2, -2.6330566e-2, -6.735341e-2, -4.370422e-2, 1.5825983e-2, -7.0299976e-2,
        -2.8567782e-3, -6.408724e-2, 2.1759478e-2, 1.8018998e-2, -1.6791916e-2, -2.2467042e-3,
        6.7808665e-3, 6.325043e-2, 4.107638e-3, -2.9024934e-2,
    ],
    // a ping-pong echo
    [
        1.0087546e-1, 1.0076511e-1, 0e0, 0e0, 0e0, 0e0,
        5.080104e-2, 0e0, -2.2122292e-2, 0e0, 4.35132e-3, 2.8532293e-1,
        5.9825327e-2, 1.884898e-2, -4.8845347e-2, 2.6328812e-4, -4.1742586e-2, 5.3244557e-2,
        -1.1408874e-1, 5.4309364e-2, -2.0378521e-1, -4.1813243e-2, 3.9707635e-2, -9.088155e-2,
        2.5824027e-3, -1.24404825e-1, 9.833225e-2, 2.928074e-2, -1.7216463e-2, 5.6621167e-3,
        -5.7372036e-3, 1.5286472e-1, 5.878913e-3, -7.6903865e-2,
    ],
    // an echo on each side
    [
        1.5919738e-1, 1.3976902e-1, -1.456095e-1, 1.21230915e-1, 1.5433486e-1, 1.8699533e-1,
        1.8080574e-1, 1.6049175e-1, 3.438541e-1, 1.8546793e-1, -3.030382e-1, -8.104991e-2,
        -4.188658e-2, -4.211251e-2, 1.8861089e-2, -3.4384567e-2, 2.329443e-1, 2.2779317e-1,
        -1.1394604e-1, -1.14264615e-1, -2.7986309e-2, -2.9556645e-2, -6.727503e-3, -7.5632087e-3,
        -3.9899573e-2, -3.981835e-2, 1.7635012e-2, 1.7749967e-2, -1.902259e-2, -1.9047385e-2,
        1.0601035e-2, 1.0564922e-2, -7.2452063e-3, -7.2448673e-3,
    ],
    // a compressor with automatic makeup
    [
        2.3961799e-1, 1.8811439e-1, -1.5910019e-1, 1.3246293e-1, 5.456176e-2, -2.2295533e-2,
        -8.977923e-2, 1.0565223e-1, 4.1217068e-1, 2.7478045e-1, -4.682053e-1, -1.7023948e-1,
        1.9842929e-1, -2.3284045e-1, -2.5047285e-3, 2.9507193e-3, 1.5294619e-1, 1.5294619e-1,
        0e0, 0e0, 0e0, 0e0, 0e0, 0e0,
        0e0, 0e0, 0e0, 0e0, 0e0, 0e0,
        0e0, 0e0, 0e0, 0e0,
    ],
    // another, lower and faster
    [
        1.9709028e-1, 1.5387945e-1, -1.19082026e-1, 9.914479e-2, 5.7467587e-2, -2.3482937e-2,
        -7.5651474e-2, 8.902668e-2, 3.413982e-1, 2.2759883e-1, -3.912589e-1, -1.4226176e-1,
        1.6957752e-1, -1.9898528e-1, -2.0256713e-3, 2.3863616e-3, 1.23679176e-1, 1.23679176e-1,
        0e0, 0e0, 0e0, 0e0, 0e0, 0e0,
        0e0, 0e0, 0e0, 0e0, 0e0, 0e0,
        0e0, 0e0, 0e0, 0e0,
    ],
    // a bus compressor that lets go by itself
    [
        1.6178074e-1, 1.2498948e-1, -1.3764171e-1, 1.14597134e-1, 1.5192597e-2, -6.20814e-3,
        -2.3317095e-2, 2.7439566e-2, 2.9196745e-1, 1.9464497e-1, -3.8516408e-1, -1.4004569e-1,
        1.292293e-1, -1.5163997e-1, -1.5741943e-3, 1.8544947e-3, 9.6466094e-2, 9.6466094e-2,
        0e0, 0e0, 0e0, 0e0, 0e0, 0e0,
        0e0, 0e0, 0e0, 0e0, 0e0, 0e0,
        0e0, 0e0, 0e0, 0e0,
    ],
    // a compressor set by hand
    [
        1.4660458e-1, 1.1473271e-1, -9.575175e-2, 7.9720564e-2, 3.0474385e-2, -1.2452725e-2,
        -6.1835658e-2, 7.276822e-2, 2.5386465e-1, 1.6924311e-1, -2.8054407e-1, -1.0200585e-1,
        1.2162312e-1, -1.4271472e-1, -1.5002475e-3, 1.7673811e-3, 9.304271e-2, 9.304271e-2,
        0e0, 0e0, 0e0, 0e0, 0e0, 0e0,
        0e0, 0e0, 0e0, 0e0, 0e0, 0e0,
        0e0, 0e0, 0e0, 0e0,
    ],
    // a track compressor that lets go by itself
    [
        8.216123e-2, 6.40649e-2, -7.436335e-2, 6.191311e-2, 1.0142031e-2, -4.1443305e-3,
        -1.7805979e-2, 2.0954082e-2, 1.4927135e-1, 9.9514246e-2, -1.6812478e-1, -6.113018e-2,
        6.0597867e-2, -7.110661e-2, -8.125975e-4, 9.572883e-4, 5.1136784e-2, 5.1136784e-2,
        0e0, 0e0, 0e0, 0e0, 0e0, 0e0,
        0e0, 0e0, 0e0, 0e0, 0e0, 0e0,
        0e0, 0e0, 0e0, 0e0,
    ],
    // a chord ducked under hits it listens to
    [
        6.8212725e-2, 6.8212725e-2, 2.900361e-3, 2.900361e-3, 4.2729266e-2, 4.2729266e-2,
        -7.324805e-2, -7.324805e-2, -2.593644e-2, -2.593644e-2, -9.748129e-2, -9.748129e-2,
        7.331698e-2, 7.331698e-2, -6.461617e-2, -6.461617e-2, 1.1391542e-2, 1.1391542e-2,
        -1.954452e-2, -1.954452e-2, 2.9850613e-2, 2.9850613e-2, -2.9783769e-2, -2.9783769e-2,
        1.5667204e-2, 1.5667204e-2, -2.5295343e-5, -2.5295343e-5, 0e0, 0e0,
        0e0, 0e0, 0e0, 0e0,
    ],
    // a pitch shifter an octave down
    [
        1.6927509e-1, 1.3017713e-1, -7.451034e-2, 1.0141225e-2, 2.5031518e-2, 1.5094188e-2,
        -1.744198e-2, -2.2853993e-2, 2.9647836e-1, -2.6884312e-2, -2.0339018e-1, 2.4176697e-1,
        -2.0891027e-1, -2.1367122e-1, 3.0365622e-1, 2.8604412e-1, 9.654566e-2, 9.654566e-2,
        -5.2132476e-2, -5.2132476e-2, 0e0, 0e0, 0e0, 0e0,
        0e0, 0e0, 0e0, 0e0, 0e0, 0e0,
        0e0, 0e0, 0e0, 0e0,
    ],
    // a pitch shifter a fifth up, half mixed, on a short window
    [
        1.7021798e-1, 1.311826e-1, -1.336291e-1, 6.5630324e-2, 4.5281835e-2, 2.0122994e-2,
        3.2330353e-2, 3.0804513e-2, 3.7973857e-1, 2.6996067e-1, -1.5563667e-1, 6.478372e-2,
        9.844776e-2, -1.07088424e-1, -1.5014377e-1, -1.5046038e-1, 2.0908614e-1, 2.0908614e-1,
        2.4601221e-2, 2.4601221e-2, 0e0, 0e0, 0e0, 0e0,
        0e0, 0e0, 0e0, 0e0, 0e0, 0e0,
        0e0, 0e0, 0e0, 0e0,
    ],
    // a chord drawn two octaves down
    [
        1.7751977e-1, 1.7751977e-1, 1.7407482e-2, 1.7407482e-2, 2.1309443e-1, 2.1309443e-1,
        -2.7073213e-1, -2.7073213e-1, -7.51098e-2, -7.51098e-2, -2.1937032e-1, -2.1937032e-1,
        3.160464e-1, 3.160464e-1, 3.0047205e-1, 3.0047205e-1, -7.200999e-2, -7.200999e-2,
        1.7763774e-1, 1.7763774e-1, -3.689953e-2, -3.689953e-2, 4.2411852e-1, 4.2411852e-1,
        -5.083904e-2, -5.083904e-2, 3.8393846e-6, 3.8393846e-6, 0e0, 0e0,
        0e0, 0e0, 0e0, 0e0,
    ],
    // a synth's volume drawn along its fader
    [
        4.7818443e-1, 4.7818443e-1, -3.196367e-2, -3.196367e-2, -4.4502828e-1, -4.4502828e-1,
        5.9285134e-1, 5.9285134e-1, 4.4705123e-1, 4.4705123e-1, 2.0408435e-1, 2.0408435e-1,
        -2.286552e-1, -2.286552e-1, 1.990352e-2, 1.990352e-2, -3.3608016e-3, -3.3608016e-3,
        5.766935e-3, 5.766935e-3, -4.5412222e-1, -4.5412222e-1, 1.3728515e0, 1.3728515e0,
        -1.751596e-1, -1.751596e-1, -4.424045e-4, -4.424045e-4, 0e0, 0e0,
        0e0, 0e0, 0e0, 0e0,
    ],
    // the same, straight in decibels
    [
        4.6010333e-1, 4.6010333e-1, -3.140862e-2, -3.140862e-2, -3.9684606e-1, -3.9684606e-1,
        5.5372727e-1, 5.5372727e-1, 4.095224e-1, 4.095224e-1, 1.6110153e-1, 1.6110153e-1,
        -1.7157228e-1, -1.7157228e-1, 1.6580578e-2, 1.6580578e-2, -2.0030793e-3, -2.0030793e-3,
        1.2937448e-3, 1.2937448e-3, -2.4718796e-1, -2.4718796e-1, 1.3728515e0, 1.3728515e0,
        -1.751596e-1, -1.751596e-1, -4.424045e-4, -4.424045e-4, 0e0, 0e0,
        0e0, 0e0, 0e0, 0e0,
    ],
    // a synth with noise beside a sampler
    [
        1.4931674e-1, 1.4838864e-1, -4.865037e-1, -4.8887795e-1, -8.656947e-2, -8.653714e-2,
        -2.0470463e-1, -1.964998e-1, -3.2341212e-1, -3.1657124e-1, 1.0486033e-2, 1.3006385e-2,
        -1.270334e-1, -1.001932e-1, 4.349604e-2, 4.8563015e-2, 7.919125e-2, 8.004441e-2,
        5.246479e-1, 4.5712695e-1, 1.462861e-1, 1.5325543e-1, -5.8479402e-2, -6.7085385e-2,
        1.1545108e-3, 1.7212626e-3, -3.716823e-5, -4.0281524e-5, 0e0, 0e0,
        0e0, 0e0, 0e0, 0e0,
    ],
];

#[test]
fn a_song_of_format_5_renders_to_the_samples_it_rendered_to_before() {
    let folder = folder();
    let (source, hit) = files(&folder);
    let factory = Builtins::with_loader(Arc::new(MediaCache::default()));
    let songs = fifth(&source, &hit);
    assert_eq!(songs.len(), FIFTH.len());
    for ((what, song), expected) in songs.iter().zip(&FIFTH) {
        assert_as_it_was(&print_with(&of_format(song, 5), &factory), expected, what);
    }
}
