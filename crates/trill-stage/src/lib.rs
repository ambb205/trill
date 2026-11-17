//! The stage: Trill's own interface, composed by a director, lit by the
//! song and filmed.
//!
//! A frame of the stage is a function of four things and of nothing else:
//! a song ([`Score`], read from a project), what was measured of its sound
//! ([`Sound`]), a time, and the shot ([`Plan`], which the [`director`]
//! decides from the song). That is what lets the window show it while the
//! song plays and a film be made of it offline, frame by frame, with the
//! two agreeing; and it is what lets all of it but the last step be tested
//! without a screen.
//!
//! Two things drive the picture and are kept apart. What is *heard* (the
//! level of the mix and of each track) sets the mood: how bright the
//! picture is, how much glows. What is *written* (the song's own notes)
//! sets everything that must land exactly: a note lights on the beat it
//! starts on, and the picture answers a kick on the kick.
//!
//! The parts, in the order a frame passes through them:
//!
//! * [`score`] reads the song: sections, parts, notes, drum hits.
//! * [`director`] cuts it into takes, each with a shot.
//! * [`camera`] says where the camera is at a beat: between two takes it
//!   travels, and lanes open and close as parts come and go.
//! * [`light`] says how bright each note is at a moment, and how hard the
//!   picture was just hit.
//! * [`sound`] holds what was measured of the audio.
//! * [`scene`] paints the interface with the `trill-noir` kit, in three
//!   layers: what is there, what emits light, and how far out of focus
//!   each part of the picture is.
//! * [`gpu`] draws those layers and runs the passes over them that the
//!   window's own renderer has no place for: bloom from the light, a blur
//!   that follows the focus, grain, a vignette, and for a film motion blur
//!   and drawing larger than the output.
//! * [`Stage`] ties these together: give it a time, get a picture.
//! * [`film`] writes a stretch of the song as a file, with its audio.

pub mod camera;
pub mod director;
pub mod film;
pub mod gpu;
pub mod light;
mod picture;
pub mod scene;
pub mod score;
pub mod sound;
mod stage;

pub use camera::Camera;
pub use director::{Entry, Kind, Plan, Shot, Take};
pub use picture::{Format, Look, Picture, Textures};
pub use score::Score;
pub use sound::{Heard, Sound};
pub use stage::{Song, Stage};
