//! 环境预设：天空、海况、水色、曝光一起切。
//!
//! 一套预设只是一组数，拿来当起点——界面上的滑条在预设的基础上改。

use crate::ocean::{OceanSettings, WaterLook};
use crate::sky::SkySettings;
use kmath::Vec3;

/// 一套环境。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EnvironmentPreset {
    pub name: &'static str,
    pub sky: SkySettings,
    /// 风速（m/s）。
    pub wind_speed: f32,
    /// 峰值波长（米）。
    pub peak_wavelength: f32,
    /// 涌浪强度。
    pub swell: f32,
    /// 尖浪程度。
    pub choppiness: f32,
    pub look: WaterLook,
    /// 后处理曝光。
    pub exposure: f32,
}

impl EnvironmentPreset {
    /// 把海况和水色套到海洋设置上（保留其余字段）。
    pub fn apply_to_ocean(&self, ocean: &mut OceanSettings) {
        ocean.spectrum.wind_speed = self.wind_speed;
        ocean.spectrum.peak_wavelength = Some(self.peak_wavelength);
        ocean.spectrum.swell = self.swell;
        ocean.choppiness = self.choppiness;
        ocean.look = self.look;
    }
}

fn sky(time_of_day: f32, cloud_coverage: f32, turbidity: f32, haze: f32) -> SkySettings {
    SkySettings {
        time_of_day,
        cloud_coverage,
        turbidity,
        haze,
        ..SkySettings::default()
    }
}

/// 八套预设。
pub fn all() -> [EnvironmentPreset; 8] {
    let tropical = WaterLook::default();
    let open_sea = WaterLook {
        scatter: Vec3::new(0.003, 0.03, 0.06),
        shallow: Vec3::new(0.04, 0.3, 0.34),
        ..WaterLook::default()
    };
    let grey_sea = WaterLook {
        absorption: Vec3::new(0.4, 0.12, 0.1),
        scatter: Vec3::new(0.012, 0.03, 0.035),
        shallow: Vec3::new(0.08, 0.2, 0.2),
        foam_intensity: 0.9,
        roughness: 0.4,
        ..WaterLook::default()
    };
    let arctic = WaterLook {
        absorption: Vec3::new(0.5, 0.1, 0.07),
        scatter: Vec3::new(0.006, 0.04, 0.055),
        shallow: Vec3::new(0.1, 0.35, 0.4),
        ..WaterLook::default()
    };
    [
        EnvironmentPreset {
            name: "热带晴空",
            sky: sky(15.0, 0.37, 1.4, 0.3),
            wind_speed: 15.0,
            peak_wavelength: 47.0,
            swell: 0.3,
            choppiness: 1.2,
            look: tropical,
            exposure: 1.0,
        },
        EnvironmentPreset {
            name: "正午平静",
            sky: sky(12.2, 0.12, 1.1, 0.2),
            wind_speed: 5.0,
            peak_wavelength: 18.0,
            swell: 0.15,
            choppiness: 0.8,
            look: WaterLook {
                foam_intensity: 0.3,
                ..tropical
            },
            exposure: 0.9,
        },
        EnvironmentPreset {
            name: "黄金时刻",
            sky: sky(17.2, 0.3, 1.8, 0.35),
            wind_speed: 9.0,
            peak_wavelength: 35.0,
            swell: 0.4,
            choppiness: 1.0,
            look: open_sea,
            exposure: 1.3,
        },
        EnvironmentPreset {
            name: "落日",
            sky: SkySettings {
                sun_disc: 0.9,
                ..sky(17.85, 0.45, 2.4, 0.45)
            },
            wind_speed: 8.0,
            peak_wavelength: 40.0,
            swell: 0.5,
            choppiness: 1.0,
            look: open_sea,
            exposure: 1.9,
        },
        EnvironmentPreset {
            name: "清晨薄雾",
            sky: sky(6.6, 0.2, 4.5, 0.8),
            wind_speed: 4.0,
            peak_wavelength: 22.0,
            swell: 0.35,
            choppiness: 0.7,
            look: open_sea,
            exposure: 1.8,
        },
        EnvironmentPreset {
            name: "阴天",
            sky: SkySettings {
                cloud_density: 1.4,
                ..sky(13.0, 0.82, 3.0, 0.6)
            },
            wind_speed: 12.0,
            peak_wavelength: 55.0,
            swell: 0.4,
            choppiness: 1.3,
            look: grey_sea,
            exposure: 1.4,
        },
        EnvironmentPreset {
            name: "风暴",
            sky: SkySettings {
                cloud_density: 1.9,
                cloud_speed: 45.0,
                ..sky(16.0, 0.96, 4.0, 0.9)
            },
            wind_speed: 18.0,
            peak_wavelength: 70.0,
            swell: 0.6,
            choppiness: 1.35,
            look: WaterLook {
                foam_intensity: 1.0,
                ..grey_sea
            },
            exposure: 1.7,
        },
        EnvironmentPreset {
            name: "极地月夜",
            sky: SkySettings {
                night_light: 1.6,
                ..sky(23.0, 0.15, 1.0, 0.4)
            },
            wind_speed: 6.0,
            peak_wavelength: 30.0,
            swell: 0.3,
            choppiness: 0.9,
            look: arctic,
            exposure: 3.5,
        },
    ]
}
