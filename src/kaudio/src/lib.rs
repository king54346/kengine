//! kaudio —— 音频。

#![warn(missing_docs)]

mod buffer;
mod decode;
mod device;
mod mixer;
mod spatial;

pub use buffer::{AUDIO_BUFFER_TYPE_UUID, AudioBuffer};
pub use decode::{AudioLoader, decode, encode_wav};
pub use device::AudioDevice;
pub use mixer::{Mixer, Sound, Status};
pub use spatial::{Attenuation, Listener, Spatial, equal_power_pan};
