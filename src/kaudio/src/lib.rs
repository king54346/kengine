//! kaudio —— 音频。

#![warn(missing_docs)]

mod buffer;
mod spatial;

pub use buffer::{AUDIO_BUFFER_TYPE_UUID, AudioBuffer};
pub use spatial::{Attenuation, Listener, Spatial, equal_power_pan};
