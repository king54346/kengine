//! Extended material parameters shared by importers and application code.
//! Transmission is screen-space; thin film and sheen are real-time approximations.
use kasset::Resource;
use kmaterial::{BlendMode, Material};
use kmath::{Vec3, Vec4};
use kshader::Shader;
use std::sync::OnceLock;

/// Parameters of the extended material shader. Defaults preserve ordinary PBR.
#[derive(Debug, Clone)]
pub struct Physical {
    /// 透射比例。大于 0 时走屏幕空间折射，材质自动转成半透明。
    pub transmission: f32,
    /// 折射率。玻璃约 1.5，水约 1.33。
    pub ior: f32,
    /// 介质厚度，用于按 Beer–Lambert 衰减透过的光。
    pub thickness: f32,
    /// 色散强度（KHR_materials_dispersion 的 `dispersion`）。
    pub dispersion: f32,
    /// 薄膜干涉的强度（KHR_materials_iridescence 的 `iridescenceFactor`）。
    pub iridescence: f32,
    /// 薄膜厚度，单位纳米。决定虹彩的颜色循环。
    pub film_thickness: f32,
    /// 绒感颜色（KHR_materials_sheen 的 `sheenColorFactor`）。
    pub sheen: Vec3,
    /// 绒感粗糙度。越大越像天鹅绒，越小越像丝绸。
    pub sheen_roughness: f32,
    /// alpha 裁切阈值。大于 0 时低于阈值的片元不参与着色。
    pub alpha_cutoff: f32,
    /// 不受光照，基础色直接输出。
    pub unlit: bool,
}
impl Default for Physical {
    fn default()->Self { Self{transmission:0.0,ior:1.5,thickness:0.0,dispersion:0.0,iridescence:0.0,film_thickness:400.0,sheen:Vec3::ZERO,sheen_roughness:0.5,alpha_cutoff:0.0,unlit:false} }
}
impl Physical {
    /// Apply the shared shader and per-material parameters. No shader recompilation when values change.
    pub fn apply(&self, material:&mut Material) {
        static SHADER:OnceLock<Resource<Shader>>=OnceLock::new();
        material.set_shader(SHADER.get_or_init(||Resource::new_ok("builtin/physical.wgsl",Shader::snippet(include_str!("physical.wgsl")))).clone());
        material.set_param(0,Vec4::new(self.transmission.clamp(0.0,1.0),self.ior.max(1.0),self.thickness.max(0.0),self.dispersion.max(0.0)));
        material.set_param(1,Vec4::new(self.iridescence.clamp(0.0,1.0),self.film_thickness.max(0.0),self.sheen_roughness.clamp(0.0,1.0),self.alpha_cutoff));
        material.set_param(2,self.sheen.extend(if self.unlit {1.0}else{0.0}));
        if self.transmission>0.0 { material.set_blend_mode(BlendMode::Alpha); }
    }
}
