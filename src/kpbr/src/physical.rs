//! Extended material parameters shared by importers and application code.
//! Transmission is screen-space; thin film and sheen are real-time approximations.
//!
//! 各向异性（`KHR_materials_anisotropy`）的直射光是规范里的各向异性 GGX，
//! 环境光用 Filament 的「弯曲反射向量」近似；清漆（`KHR_materials_clearcoat`）
//! 是叠在上面的一层 IOR 1.5 的 GGX，环境部分只采全局环境、不做探针视差。
use kasset::Resource;
use kmaterial::{BlendMode, Material};
use ktexture::Texture;
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
    /// 各向异性强度（`anisotropyStrength`），0..1。
    pub anisotropy: f32,
    /// 各向异性方向相对切线的旋转，弧度（`anisotropyRotation`）。
    pub anisotropy_rotation: f32,
    /// 各向异性贴图：RG 是切线空间方向，B 是强度倍率。绑在 `custom_texture0`。
    pub anisotropy_texture: Option<Resource<Texture>>,
    /// 清漆层强度（`clearcoatFactor`）。
    pub clearcoat: f32,
    /// 清漆层粗糙度（`clearcoatRoughnessFactor`）。
    pub clearcoat_roughness: f32,
}
impl Default for Physical {
    fn default()->Self { Self{transmission:0.0,ior:1.5,thickness:0.0,dispersion:0.0,iridescence:0.0,film_thickness:400.0,sheen:Vec3::ZERO,sheen_roughness:0.5,alpha_cutoff:0.0,unlit:false,anisotropy:0.0,anisotropy_rotation:0.0,anisotropy_texture:None,clearcoat:0.0,clearcoat_roughness:0.0} }
}
impl Physical {
    /// 有没有哪一项需要扩展着色器。全是默认值时不必换掉标准着色器。
    pub fn is_needed(&self) -> bool {
        self.transmission>0.0 || self.iridescence>0.0 || self.sheen!=Vec3::ZERO || self.unlit || self.alpha_cutoff>0.0 || self.anisotropy>0.0 || self.clearcoat>0.0
    }

    /// Apply the shared shader and per-material parameters. No shader recompilation when values change.
    pub fn apply(&self, material:&mut Material) {
        static SHADER:OnceLock<Resource<Shader>>=OnceLock::new();
        material.set_shader(SHADER.get_or_init(||Resource::new_ok("builtin/physical.wgsl",Shader::snippet(include_str!("physical.wgsl")))).clone());
        material.set_param(0,Vec4::new(self.transmission.clamp(0.0,1.0),self.ior.max(1.0),self.thickness.max(0.0),self.dispersion.max(0.0)));
        material.set_param(1,Vec4::new(self.iridescence.clamp(0.0,1.0),self.film_thickness.max(0.0),self.sheen_roughness.clamp(0.0,1.0),self.alpha_cutoff));
        material.set_param(2,self.sheen.extend(if self.unlit {1.0}else{0.0}));
        // 第四个槽位：各向异性强度、旋转、清漆强度、清漆粗糙度。
        // 有各向异性贴图时强度加 2 作为标记——`custom_texture0` 没绑时是
        // 1×1 白图，着色器分不出「没绑」和「绑了一张白图」，只能另外告诉它。
        let has_texture = self.anisotropy_texture.is_some() && self.anisotropy > 0.0;
        material.set_param(3,Vec4::new(self.anisotropy.clamp(0.0,1.0)+if has_texture {2.0}else{0.0},self.anisotropy_rotation,self.clearcoat.clamp(0.0,1.0),self.clearcoat_roughness.clamp(0.0,1.0)));
        if let Some(texture)=self.anisotropy_texture.as_ref().filter(|_|has_texture) { material.set_custom_texture(0,texture.clone()); }
        if self.transmission>0.0 { material.set_blend_mode(BlendMode::Alpha); }
    }
}
