//! kmaterial —— 材质资源。
//!
//! 材质 = 一个着色器 + 一组命名参数（标量、向量、纹理）。
//!
//! ```
//! use kmaterial::prelude::*;
//! use kmath::Vec4;
//!
//! let material = Material::standard()
//!     .with_base_color(Vec4::new(1.0, 0.5, 0.2, 1.0))
//!     .with_roughness(0.3);
//!
//! assert_eq!(material.base_color(), Vec4::new(1.0, 0.5, 0.2, 1.0));
//! assert_eq!(material.roughness(), 0.3);
//! ```
//!
//! # 与渲染器的关系
//!
//! 参数表本身是通用的——可以放任意名字的值。但渲染器目前只消费
//! [`standard`] 里列出的那几个标准参数；自定义着色器配自定义参数需要
//! 按 naga 反射出的绑定布局动态建管线，那部分尚未实现。

#![warn(missing_docs)]

use fxhash::FxHashMap;
use kasset::{Resource, ResourceData};
use kcore::uuid::{Uuid, uuid};
use kmath::{Vec2, Vec3, Vec4};
use kshader::Shader;
use ktexture::Texture;

/// [`Material`] 的资源类型标识。
pub const MATERIAL_TYPE_UUID: Uuid = uuid!("8e5c2f14-9a07-4d3b-b6e8-4c1f7a92d508");

/// 常用类型的集中导出。
pub mod prelude {
    pub use crate::{Material, MaterialValue, standard};
}

/// 渲染器认识的标准参数名。
pub mod standard {
    /// 基础颜色，`Vec4`（RGBA，线性空间）。
    pub const BASE_COLOR: &str = "base_color";
    /// 基础颜色贴图，`Texture`。
    pub const BASE_COLOR_TEXTURE: &str = "base_color_texture";
    /// 金属度，`Float`，取值 `[0, 1]`。
    pub const METALLIC: &str = "metallic";
    /// 粗糙度，`Float`，取值 `[0, 1]`。
    pub const ROUGHNESS: &str = "roughness";
}

/// 一个材质参数值。
#[derive(Debug, Clone)]
pub enum MaterialValue {
    /// 标量。
    Float(f32),
    /// 二维向量。
    Vec2(Vec2),
    /// 三维向量。
    Vec3(Vec3),
    /// 四维向量，颜色也用它。
    Vec4(Vec4),
    /// 纹理句柄。
    Texture(Resource<Texture>),
}

impl MaterialValue {
    /// 取标量值。
    pub fn as_float(&self) -> Option<f32> {
        match self {
            Self::Float(v) => Some(*v),
            _ => None,
        }
    }

    /// 取四维向量。
    pub fn as_vec4(&self) -> Option<Vec4> {
        match self {
            Self::Vec4(v) => Some(*v),
            _ => None,
        }
    }

    /// 取三维向量。
    pub fn as_vec3(&self) -> Option<Vec3> {
        match self {
            Self::Vec3(v) => Some(*v),
            _ => None,
        }
    }

    /// 取纹理句柄。
    pub fn as_texture(&self) -> Option<&Resource<Texture>> {
        match self {
            Self::Texture(t) => Some(t),
            _ => None,
        }
    }
}

impl From<f32> for MaterialValue {
    fn from(value: f32) -> Self {
        Self::Float(value)
    }
}

impl From<Vec2> for MaterialValue {
    fn from(value: Vec2) -> Self {
        Self::Vec2(value)
    }
}

impl From<Vec3> for MaterialValue {
    fn from(value: Vec3) -> Self {
        Self::Vec3(value)
    }
}

impl From<Vec4> for MaterialValue {
    fn from(value: Vec4) -> Self {
        Self::Vec4(value)
    }
}

impl From<Resource<Texture>> for MaterialValue {
    fn from(value: Resource<Texture>) -> Self {
        Self::Texture(value)
    }
}

/// 一份材质。
///
/// 克隆共享同一个 `id`，渲染器据此缓存管线与绑定组。
#[derive(Debug, Clone)]
pub struct Material {
    id: Uuid,
    shader: Option<Resource<Shader>>,
    values: FxHashMap<String, MaterialValue>,
}

impl Default for Material {
    fn default() -> Self {
        Self::standard()
    }
}

impl Material {
    /// 创建一个没有任何参数的空材质。
    pub fn new() -> Self {
        Self {
            id: Uuid::new_v4(),
            shader: None,
            values: FxHashMap::default(),
        }
    }

    /// 创建标准材质：白色、非金属、中等粗糙度。
    ///
    /// 不指定着色器时渲染器会使用内置的标准着色器。
    pub fn standard() -> Self {
        Self::new()
            .with_base_color(Vec4::ONE)
            .with_metallic(0.0)
            .with_roughness(0.5)
    }

    /// 缓存键。克隆的材质共享同一个 id。
    pub fn id(&self) -> Uuid {
        self.id
    }

    /// 指定着色器。为 [`None`] 时使用引擎内置的标准着色器。
    pub fn shader(&self) -> Option<&Resource<Shader>> {
        self.shader.as_ref()
    }

    /// 设置着色器。
    pub fn set_shader(&mut self, shader: Resource<Shader>) {
        self.shader = Some(shader);
    }

    /// 链式设置着色器。
    pub fn with_shader(mut self, shader: Resource<Shader>) -> Self {
        self.set_shader(shader);
        self
    }

    /// 设置一个参数。
    pub fn set(&mut self, name: impl Into<String>, value: impl Into<MaterialValue>) {
        self.values.insert(name.into(), value.into());
    }

    /// 链式设置参数。
    pub fn with(mut self, name: impl Into<String>, value: impl Into<MaterialValue>) -> Self {
        self.set(name, value);
        self
    }

    /// 读取一个参数。
    pub fn get(&self, name: &str) -> Option<&MaterialValue> {
        self.values.get(name)
    }

    /// 移除一个参数。
    pub fn remove(&mut self, name: &str) -> Option<MaterialValue> {
        self.values.remove(name)
    }

    /// 全部参数。
    pub fn values(&self) -> impl Iterator<Item = (&str, &MaterialValue)> {
        self.values.iter().map(|(k, v)| (k.as_str(), v))
    }

    // ── 标准参数的便捷读写 ────────────────────────────────────────────────

    /// 基础颜色，未设置时为白色。
    pub fn base_color(&self) -> Vec4 {
        self.get(standard::BASE_COLOR)
            .and_then(MaterialValue::as_vec4)
            .unwrap_or(Vec4::ONE)
    }

    /// 设置基础颜色。
    pub fn with_base_color(self, color: Vec4) -> Self {
        self.with(standard::BASE_COLOR, color)
    }

    /// 金属度，未设置时为 `0.0`。
    pub fn metallic(&self) -> f32 {
        self.get(standard::METALLIC)
            .and_then(MaterialValue::as_float)
            .unwrap_or(0.0)
    }

    /// 设置金属度。
    pub fn with_metallic(self, value: f32) -> Self {
        self.with(standard::METALLIC, value)
    }

    /// 粗糙度，未设置时为 `0.5`。
    pub fn roughness(&self) -> f32 {
        self.get(standard::ROUGHNESS)
            .and_then(MaterialValue::as_float)
            .unwrap_or(0.5)
    }

    /// 设置粗糙度。
    pub fn with_roughness(self, value: f32) -> Self {
        self.with(standard::ROUGHNESS, value)
    }

    /// 基础颜色贴图。
    pub fn base_color_texture(&self) -> Option<&Resource<Texture>> {
        self.get(standard::BASE_COLOR_TEXTURE)
            .and_then(MaterialValue::as_texture)
    }

    /// 设置基础颜色贴图。
    pub fn with_base_color_texture(self, texture: Resource<Texture>) -> Self {
        self.with(standard::BASE_COLOR_TEXTURE, texture)
    }
}

impl ResourceData for Material {
    fn type_uuid(&self) -> Uuid {
        MATERIAL_TYPE_UUID
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn standard_material_has_sensible_defaults() {
        let material = Material::standard();

        assert_eq!(material.base_color(), Vec4::ONE);
        assert_eq!(material.metallic(), 0.0);
        assert_eq!(material.roughness(), 0.5);
        assert!(material.base_color_texture().is_none());
    }

    #[test]
    fn empty_material_falls_back_to_defaults() {
        // 空材质读取标准参数不应 panic，而是给出兜底值。
        let material = Material::new();

        assert_eq!(material.base_color(), Vec4::ONE);
        assert_eq!(material.roughness(), 0.5);
    }

    #[test]
    fn values_can_be_overwritten() {
        let material = Material::standard()
            .with_roughness(0.1)
            .with_roughness(0.9);

        assert_eq!(material.roughness(), 0.9);
    }

    #[test]
    fn wrong_type_read_falls_back() {
        // 把 base_color 设成标量，按 Vec4 读取时应回落到默认值而不是 panic。
        let material = Material::new().with(standard::BASE_COLOR, 1.0f32);

        assert_eq!(material.base_color(), Vec4::ONE);
    }

    #[test]
    fn custom_values_are_preserved() {
        let material = Material::new().with("wind_strength", 2.5f32);

        assert_eq!(
            material.get("wind_strength").and_then(MaterialValue::as_float),
            Some(2.5)
        );
        assert_eq!(material.values().count(), 1);
    }

    #[test]
    fn texture_value_round_trips() {
        let texture = Resource::new_ok("white", Texture::white());
        let material = Material::standard().with_base_color_texture(texture.clone());

        let stored = material.base_color_texture().expect("贴图应当存在");
        assert_eq!(stored, &texture);
    }

    #[test]
    fn clone_shares_cache_id() {
        let material = Material::standard();
        assert_eq!(material.id(), material.clone().id());
        assert_ne!(material.id(), Material::standard().id());
    }

    #[test]
    fn remove_deletes_value() {
        let mut material = Material::standard();

        assert!(material.remove(standard::ROUGHNESS).is_some());
        // 移除后回落到默认值。
        assert_eq!(material.roughness(), 0.5);
        assert!(material.get(standard::ROUGHNESS).is_none());
    }
}
