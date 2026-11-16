//! Sounds: the browser's library tab, previews, drops onto tracks, samplers
//! and pads, waveforms on audio clips, and the audio clip panel.
//!
//! Each test gets a sample pack of its own in a temporary folder and a
//! runtime that keeps nothing: no test reads or writes the user's settings,
//! library or samples.

use super::*;

#[test]
fn placeholder() {
    let harness = Harness::new(UiOptions::default(), |_| {});
    assert!(harness.runtime.library.roots().is_empty(), "a test's library starts empty");
}
