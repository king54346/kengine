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
//! 参数表本身是通用的——可以放任意名字的值。渲染器消费的是
//! [`standard`] 里列出的那些名字：几个标准 PBR 参数，外加
//! [`PARAMS`](standard::PARAMS)（4 个 `vec4` 槽位）与
//! [`CUSTOM_TEXTURES`](standard::CUSTOM_TEXTURES)（2 张贴图），
//! 后两组是给自定义着色器钩子用的。
//!
//! **不认识的名字会被静默忽略**。让每个材质按 naga 反射出的绑定布局
//! 各建一套管线是另一条路，代价是每个自定义材质一条管线、一个绑定组，
//! 而且合批只能按材质而不是按贴图——固定槽位换来的是「自定义材质仍然
//! 走同一条管线、仍然能合批」。

#![warn(missing_docs)]

use fxhash::FxHashMap;
use kasset::{Resource, ResourceData};
use kcore::{
    uuid::{Uuid, uuid},
    visitor::{Visit, VisitResult, Visitor},
};
use kmath::{Vec2, Vec3, Vec4};
use kshader::Shader;
use ktexture::Texture;

/// [`Material`] 的资源类型标识。
pub const MATERIAL_TYPE_UUID: Uuid = uuid!("8e5c2f14-9a07-4d3b-b6e8-4c1f7a92d508");

/// 常用类型的集中导出。
pub mod prelude {
    pub use crate::{BlendMode, Material, MaterialValue, standard};
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

    /// 自定义材质参数的槽位名：`param0` … `param3`。
    ///
    /// 着色器钩子里读 `surface.params[i]`，i 就是这里的下标。
    ///
    /// 用**固定槽位**而不是任意名字，是因为渲染器必须在不解析着色器的
    /// 前提下知道该把哪个值放到哪个偏移上。按名字排序去分配槽位看着更
    /// 自由，但加一个参数就会把已有参数全部挪位——着色器那边不会报错，
    /// 只是颜色突然变成了速度。
    pub const PARAMS: [&str; PARAM_SLOTS] = ["param0", "param1", "param2", "param3"];

    /// 参数槽位的数量。
    pub const PARAM_SLOTS: usize = 4;

    /// 自定义材质贴图的槽位名：`custom_texture0` / `custom_texture1`。
    ///
    /// 着色器钩子里的变量名与这里同名，采样器复用 `base_color_sampler`
    /// ——一个材质的各张贴图共享同一套 UV，过滤与平铺方式理应一致。
    pub const CUSTOM_TEXTURES: [&str; CUSTOM_TEXTURE_SLOTS] =
        ["custom_texture0", "custom_texture1"];

    /// 自定义贴图槽位的数量。
    pub const CUSTOM_TEXTURE_SLOTS: usize = 2;

    /// 自定义**纹理数组**的槽位名。
    ///
    /// 着色器钩子里的变量名与这里同名：
    /// `textureSample(custom_texture_array, base_color_sampler, uv, layer)`。
    ///
    /// 只有一个槽位而上面的二维贴图有两个，是因为纹理数组本身就是
    /// 「一个槽位装很多张图」——真需要更多张就多加几层，不必多开槽位。
    pub const CUSTOM_TEXTURE_ARRAY: &str = "custom_texture_array";
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
///
/// # 为什么还有个 `version`
///
/// 光有 `id` 不足以当缓存键：克隆之后改参数会得到「id 相同、内容不同」的两份材质，
/// 渲染器照着 id 取缓存就会画出旧参数。每次改动 bump 一次 `version`，
/// 用 [`cache_key`](Self::cache_key) 做键才是可靠的。
#[derive(Debug, Clone)]
pub struct Material {
    id: Uuid,
    /// 作者给它起的名字，通常来自 glTF 的 `materials[i].name`。
    ///
    /// 存在的理由是**按名字找到它**：美术在 Blender 里把头盔的皮革部分命名为
    /// `LeatherPartsMat`，游戏侧就该能凭这个名字把那一块染成红色，而不必去数
    /// 「第几个 primitive」——那个序号在美术重新导出一次之后就变了。
    ///
    /// 没有名字的材质（代码里现造的）是 [`None`]，不是空串：
    /// 「没起名字」和「名字是空的」在按名字查找时是两回事。
    name: Option<String>,
    blend_mode: BlendMode,
    /// 关掉背面剔除，两面都画。
    ///
    /// 布料、树叶、纸片这类**没有厚度**的东西必须开：它们只有一层三角形，
    /// 剔掉背面之后从另一侧看就是透明的。
    ///
    /// 默认关着，因为它是有代价的——背面剔除本来能省掉一半的片元着色，
    /// 而绝大多数模型是闭合的实体，背面本来就看不见。
    double_sided: bool,
    /// 内容版本号，每次改动 +1。
    ///
    /// 用版本号而不是「改动就换新 id」：换 id 会让渲染器把管线、绑定组
    /// 连同贴图一起重建，而绝大多数改动（调个颜色、改个粗糙度）只影响
    /// 那一小块逐实例的数值参数。
    version: u64,
    shader: Option<Resource<Shader>>,
    values: FxHashMap<String, MaterialValue>,
}

/// 材质怎么和它后面的画面混合。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BlendMode {
    /// 直接覆盖，写深度。绝大多数材质都是这种。
    #[default]
    Opaque,
    /// 按 alpha 混合，**不写深度**。
    ///
    /// 不写深度是必须的：半透明物体互相之间不该做深度剔除，
    /// 否则先画的那个会把后画的整块挡掉——透过玻璃看不到玻璃后面的玻璃。
    /// 代价是半透明物体之间的前后顺序只能靠排序保证，而排序是按
    /// 物体中心算的，两个互相穿插的半透明物体仍然会画错。
    Alpha,
}

/// 材质的内容缓存键：同一份内容必然相等，内容一变必然不等。
pub type MaterialKey = (Uuid, u64);

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
            name: None,
            blend_mode: BlendMode::Opaque,
            double_sided: false,
            version: 0,
            shader: None,
            values: FxHashMap::default(),
        }
    }

    /// 材质的名字，没有就是 [`None`]。
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// 改名字。
    ///
    /// 不 bump `version`：名字不参与渲染，改它没有必要让渲染器重建任何东西。
    pub fn set_name(&mut self, name: impl Into<String>) {
        self.name = Some(name.into());
    }

    /// 链式版本的 [`set_name`](Self::set_name)。
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.set_name(name);
        self
    }

    /// 名字是不是这个。
    ///
    /// 比 `material.name() == Some("X")` 顺手，而按名字比对正是这个字段
    /// 存在的全部理由。
    pub fn is_named(&self, name: &str) -> bool {
        self.name.as_deref() == Some(name)
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

    /// 资源标识。克隆的材质共享同一个 id。
    ///
    /// **不要拿它单独当缓存键**——克隆后改参数会得到 id 相同、内容不同的两份材质。
    /// 要缓存请用 [`cache_key`](Self::cache_key)。
    pub fn id(&self) -> Uuid {
        self.id
    }

    /// 内容版本号，每次改动 +1。
    pub fn version(&self) -> u64 {
        self.version
    }

    /// 内容缓存键：同一份内容必然相等，内容一变必然不等。
    pub fn cache_key(&self) -> MaterialKey {
        (self.id, self.version)
    }

    /// 标记内容已改变。
    ///
    /// 通过 [`values_mut`](Self::values_mut) 之类的途径绕过常规 setter 改了内容时，
    /// 必须自己调一次，否则渲染器会继续用缓存里的旧值。
    pub fn touch(&mut self) {
        self.version = self.version.wrapping_add(1);
    }

    /// 指定着色器。为 [`None`] 时使用引擎内置的标准着色器。
    pub fn shader(&self) -> Option<&Resource<Shader>> {
        self.shader.as_ref()
    }

    /// 设置着色器。
    pub fn set_shader(&mut self, shader: Resource<Shader>) {
        self.shader = Some(shader);
        self.touch();
    }

    /// 链式设置着色器。
    pub fn with_shader(mut self, shader: Resource<Shader>) -> Self {
        self.set_shader(shader);
        self
    }

    /// 摘掉着色器，退回引擎的标准材质。
    ///
    /// 材质钩子是**可插拔**的——装得上就该摘得下：切换渲染风格、
    /// 关掉某个效果做对比、材质编辑器里点掉一个节点，都要它。
    /// 其余参数（颜色、粗糙度、贴图）一个都不动。
    pub fn clear_shader(&mut self) {
        if self.shader.is_some() {
            self.shader = None;
            self.touch();
        }
    }

    /// 这份材质的混合方式。
    pub fn blend_mode(&self) -> BlendMode {
        self.blend_mode
    }

    /// 两面都画还是只画正面。
    pub fn double_sided(&self) -> bool {
        self.double_sided
    }

    /// 设置是否两面都画。
    ///
    /// 开了之后**法线不会自动翻转**：背面用的仍是几何法线，所以从背面看
    /// 光照是反的（该亮的地方暗）。布料这类薄片通常可以接受，
    /// 要更准就得在材质钩子里按 `@builtin(front_facing)` 翻一下——
    /// 而那个内置量目前没有传进 `Surface`。
    pub fn set_double_sided(&mut self, double_sided: bool) {
        if self.double_sided != double_sided {
            self.double_sided = double_sided;
            self.version = self.version.wrapping_add(1);
        }
    }

    /// 建造式：两面都画。
    pub fn with_double_sided(mut self) -> Self {
        self.double_sided = true;
        self
    }

    /// 设置混合方式。
    pub fn set_blend_mode(&mut self, mode: BlendMode) {
        self.blend_mode = mode;
        self.touch();
    }

    /// 链式设置混合方式。
    pub fn with_blend_mode(mut self, mode: BlendMode) -> Self {
        self.set_blend_mode(mode);
        self
    }

    /// 设置一个参数。
    pub fn set(&mut self, name: impl Into<String>, value: impl Into<MaterialValue>) {
        self.values.insert(name.into(), value.into());
        self.touch();
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

    /// 设置一个自定义材质参数（着色器钩子里的 `surface.params[slot]`）。
    ///
    /// # Panics
    ///
    /// `slot >= `[`standard::PARAM_SLOTS`] 时 panic。这是代码里写死的下标，
    /// 越界属于编程错误——静默忽略的话表现为「那个参数怎么设都不起作用」，
    /// 而着色器一句话都不会说。
    pub fn set_param(&mut self, slot: usize, value: impl Into<MaterialValue>) {
        assert!(
            slot < standard::PARAM_SLOTS,
            "材质参数槽位越界：{slot}，一共只有 {} 个",
            standard::PARAM_SLOTS
        );
        self.set(standard::PARAMS[slot], value);
    }

    /// 链式设置自定义材质参数。
    pub fn with_param(mut self, slot: usize, value: impl Into<MaterialValue>) -> Self {
        self.set_param(slot, value);
        self
    }

    /// 读取一个自定义材质参数。
    pub fn param(&self, slot: usize) -> Option<&MaterialValue> {
        self.get(standard::PARAMS.get(slot)?)
    }

    /// 设置一张自定义材质贴图（着色器钩子里的 `custom_texture0` / `1`）。
    ///
    /// # Panics
    ///
    /// `slot >= `[`standard::CUSTOM_TEXTURE_SLOTS`] 时 panic，理由同
    /// [`set_param`](Self::set_param)。
    pub fn set_custom_texture(&mut self, slot: usize, texture: Resource<Texture>) {
        assert!(
            slot < standard::CUSTOM_TEXTURE_SLOTS,
            "自定义贴图槽位越界：{slot}，一共只有 {} 个",
            standard::CUSTOM_TEXTURE_SLOTS
        );
        self.set(standard::CUSTOM_TEXTURES[slot], texture);
    }

    /// 链式设置自定义材质贴图。
    pub fn with_custom_texture(mut self, slot: usize, texture: Resource<Texture>) -> Self {
        self.set_custom_texture(slot, texture);
        self
    }

    /// 读取一张自定义材质贴图。
    pub fn custom_texture(&self, slot: usize) -> Option<&Resource<Texture>> {
        self.get(standard::CUSTOM_TEXTURES.get(slot)?)
            .and_then(MaterialValue::as_texture)
    }

    /// 设置自定义纹理数组（着色器钩子里的 `custom_texture_array`）。
    ///
    /// 传的应当是一张 [`Texture::layers`] 大于 1 的纹理
    /// （[`Texture::from_layers`] 造出来的那种）。传普通贴图也能工作，
    /// 只是那个数组只有一层。
    ///
    /// [`Texture::layers`]: ktexture::Texture::layers
    /// [`Texture::from_layers`]: ktexture::Texture::from_layers
    pub fn set_texture_array(&mut self, texture: Resource<Texture>) {
        self.set(standard::CUSTOM_TEXTURE_ARRAY, texture);
    }

    /// 链式设置自定义纹理数组。
    pub fn with_texture_array(mut self, texture: Resource<Texture>) -> Self {
        self.set_texture_array(texture);
        self
    }

    /// 读取自定义纹理数组。
    pub fn texture_array(&self) -> Option<&Resource<Texture>> {
        self.get(standard::CUSTOM_TEXTURE_ARRAY)
            .and_then(MaterialValue::as_texture)
    }

    /// 移除一个参数。
    pub fn remove(&mut self, name: &str) -> Option<MaterialValue> {
        let removed = self.values.remove(name);
        if removed.is_some() {
            self.touch();
        }
        removed
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

    /// 就地设置基础颜色。
    ///
    /// 和上面那个链式版本的区别只在借用形式，但差别很实在：运行时拿到的
    /// 几乎总是 `&mut Material`（受击闪红、选中高亮、按名字染色），
    /// 而链式版本要吃掉 `self`，在 `&mut` 后面用不了。
    pub fn set_base_color(&mut self, color: Vec4) {
        self.set(standard::BASE_COLOR, color);
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

    /// 就地设置金属度。
    pub fn set_metallic(&mut self, value: f32) {
        self.set(standard::METALLIC, value);
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

    /// 就地设置粗糙度。
    pub fn set_roughness(&mut self, value: f32) {
        self.set(standard::ROUGHNESS, value);
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

impl MaterialValue {
    /// 类型标签，序列化时区分变体用。
    ///
    /// 显式写死而不是靠声明顺序：将来在中间插一个变体，靠顺序的话
    /// 老文件会被解释成另一种类型，而且不会报错，只是画面莫名其妙。
    fn tag(&self) -> u8 {
        match self {
            Self::Float(_) => 0,
            Self::Vec2(_) => 1,
            Self::Vec3(_) => 2,
            Self::Vec4(_) => 3,
            Self::Texture(_) => 4,
        }
    }
}

impl Default for MaterialValue {
    fn default() -> Self {
        Self::Float(0.0)
    }
}

impl Visit for MaterialValue {
    fn visit(&mut self, name: &str, visitor: &mut Visitor) -> VisitResult {
        let mut region = visitor.enter_region(name)?;

        let mut tag = self.tag();
        tag.visit("Tag", &mut region)?;

        if region.is_reading() {
            // 读取时先按标签造一个同类型的空值，再往里填——
            // 这样下面的分支两个方向共用一套代码。
            *self = match tag {
                0 => Self::Float(0.0),
                1 => Self::Vec2(Vec2::ZERO),
                2 => Self::Vec3(Vec3::ZERO),
                3 => Self::Vec4(Vec4::ZERO),
                4 => Self::Texture(Resource::from_untyped(
                    kasset::UntypedResource::new_pending(std::path::PathBuf::new()),
                )),
                other => {
                    return Err(kcore::visitor::error::VisitError::User(format!(
                        "未知的材质参数类型标签 {other}"
                    )));
                }
            };
        }

        match self {
            Self::Float(v) => v.visit("Value", &mut region),
            Self::Vec2(v) => v.visit("Value", &mut region),
            Self::Vec3(v) => v.visit("Value", &mut region),
            Self::Vec4(v) => v.visit("Value", &mut region),
            Self::Texture(v) => kasset::visit_resource("Value", v, &mut region),
        }
    }
}

impl Visit for Material {
    fn visit(&mut self, name: &str, visitor: &mut Visitor) -> VisitResult {
        let mut region = visitor.enter_region(name)?;

        self.id.visit("Id", &mut region)?;
        kasset::visit_resource_option("Shader", &mut self.shader, &mut region)?;

        // 名字是后加的字段。老存档里没有这块区域，读不到就当匿名——
        // 为了一个不参与渲染的字符串让整个场景读不进来，不划算。
        // 空串按「没起名字」处理，理由见 `Material::name`。
        let mut name = self.name.clone().unwrap_or_default();
        if name.visit("Name", &mut region).is_ok() {
            self.name = (!name.is_empty()).then_some(name);
        } else if region.is_reading() {
            self.name = None;
        }

        // `FxHashMap` 的 `Visit` 要求键值都实现 `Visit`，这里手工展开成
        // 「一个长度 + 若干个键值区域」，顺带保证读出来的顺序稳定。
        let mut count = self.values.len() as u32;
        count.visit("Count", &mut region)?;

        if region.is_reading() {
            self.values.clear();
            for index in 0..count {
                let mut entry = region.enter_region(&format!("Value{index}"))?;
                let mut key = String::new();
                let mut value = MaterialValue::default();
                key.visit("Key", &mut entry)?;
                value.visit("Value", &mut entry)?;
                self.values.insert(key, value);
            }
            // 读出来的就是文件里那份内容，版本号从 0 重新起算。
            self.version = 0;
        } else {
            // 哈希表的迭代顺序不稳定，先按键排序——否则同一份材质
            // 存两次会得到不同的字节，没法做「内容没变就不重写」这类判断。
            let mut keys: Vec<&String> = self.values.keys().collect();
            keys.sort();
            for (index, key) in keys.into_iter().enumerate() {
                let mut entry = region.enter_region(&format!("Value{index}"))?;
                let mut key = key.clone();
                let mut value = self.values[&key].clone();
                key.visit("Key", &mut entry)?;
                value.visit("Value", &mut entry)?;
            }
        }

        // 后加的字段，放在最后当可选区域：老存档里没有，读不到就当
        // 「只画正面」——和加这个字段之前的行为一致。
        let _ = self.double_sided.visit("DoubleSided", &mut region);

        Ok(())
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
        let material = Material::standard().with_roughness(0.1).with_roughness(0.9);

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
            material
                .get("wind_strength")
                .and_then(MaterialValue::as_float),
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

    // ── 内容版本号 ──

    #[test]
    fn a_fresh_material_starts_at_version_zero() {
        assert_eq!(Material::new().version(), 0);
    }

    #[test]
    fn every_edit_bumps_the_version() {
        let mut material = Material::new();
        let before = material.version();

        material.set("a", 1.0f32);
        assert!(material.version() > before);

        let after_set = material.version();
        material.remove("a");
        assert!(material.version() > after_set);
    }

    #[test]
    fn removing_a_missing_key_is_not_a_change() {
        let mut material = Material::new();
        let before = material.version();

        material.remove("nope");

        assert_eq!(material.version(), before, "什么都没删却算成了一次改动");
    }

    #[test]
    fn a_clone_that_gets_edited_stops_matching_the_original() {
        // 这正是阶段 2 留下的坑：id 相同、内容不同的两份材质，
        // 渲染器照着 id 取缓存就会画出旧参数。
        let original = Material::standard();
        let mut edited = original.clone();
        edited.set(standard::METALLIC, 1.0f32);

        assert_eq!(edited.id(), original.id(), "id 仍应共享");
        assert_ne!(
            edited.cache_key(),
            original.cache_key(),
            "内容变了，缓存键必须跟着变"
        );
    }

    #[test]
    fn an_untouched_clone_keeps_the_same_cache_key() {
        let original = Material::standard();
        assert_eq!(original.clone().cache_key(), original.cache_key());
    }

    #[test]
    fn touch_covers_edits_made_behind_the_setters() {
        let mut material = Material::standard();
        let before = material.cache_key();
        material.touch();

        assert_ne!(material.cache_key(), before);
    }

    #[test]
    fn setting_a_shader_counts_as_a_change() {
        let mut material = Material::new();
        let before = material.version();
        let shader = kasset::ResourceManager::new().request::<Shader>("x.wgsl");

        material.set_shader(shader);

        assert!(material.version() > before);
    }

    // ── 序列化 ──

    /// 存进 Visitor 再读回来，读取端带上资源管理器。
    fn roundtrip(material: &Material, manager: &kasset::ResourceManager) -> Material {
        use std::sync::Arc;

        let mut visitor = Visitor::new();
        let mut source = material.clone();
        source.visit("M", &mut visitor).unwrap();
        let bytes = visitor.save_binary_to_vec().unwrap();

        let mut visitor = Visitor::load_binary_from_memory(&bytes).unwrap();
        visitor.blackboard.register(Arc::new(manager.clone()));
        let mut restored = Material::new();
        restored.visit("M", &mut visitor).unwrap();
        restored
    }

    #[test]
    fn numeric_parameters_survive_a_roundtrip() {
        let manager = kasset::ResourceManager::new();
        let material = Material::standard()
            .with_base_color(Vec4::new(0.2, 0.4, 0.6, 1.0))
            .with_metallic(0.75)
            .with_roughness(0.25)
            .with("custom_vec3", Vec3::new(1.0, 2.0, 3.0))
            .with("custom_vec2", Vec2::new(4.0, 5.0));

        let restored = roundtrip(&material, &manager);

        assert_eq!(restored.id(), material.id(), "id 丢了，管线缓存会失配");
        assert_eq!(restored.base_color(), material.base_color());
        assert_eq!(restored.metallic(), material.metallic());
        assert_eq!(restored.roughness(), material.roughness());
        assert_eq!(
            restored.get("custom_vec3").unwrap().as_vec3(),
            Some(Vec3::new(1.0, 2.0, 3.0))
        );
        assert_eq!(
            restored.get("custom_vec2").and_then(|v| match v {
                MaterialValue::Vec2(v) => Some(*v),
                _ => None,
            }),
            Some(Vec2::new(4.0, 5.0))
        );
    }

    #[test]
    fn texture_parameters_come_back_as_path_references() {
        let manager = kasset::ResourceManager::new();
        let texture = manager.request::<Texture>("assets/wall.png");
        let material = Material::standard().with_base_color_texture(texture);

        let restored = roundtrip(&material, &manager);

        let path = restored.base_color_texture().unwrap().path().to_path_buf();
        assert_eq!(path, std::path::Path::new("assets/wall.png"));
    }

    #[test]
    fn the_shader_reference_survives() {
        let manager = kasset::ResourceManager::new();
        let material = Material::new().with_shader(manager.request::<Shader>("custom.wgsl"));

        let restored = roundtrip(&material, &manager);

        assert_eq!(
            restored.shader().unwrap().path(),
            std::path::Path::new("custom.wgsl")
        );
    }

    #[test]
    fn a_material_without_a_shader_reads_back_without_one() {
        let manager = kasset::ResourceManager::new();
        assert!(
            roundtrip(&Material::standard(), &manager)
                .shader()
                .is_none()
        );
    }

    #[test]
    fn an_empty_material_survives_a_roundtrip() {
        let manager = kasset::ResourceManager::new();
        let restored = roundtrip(&Material::new(), &manager);

        assert_eq!(restored.values().count(), 0);
    }

    #[test]
    fn saving_the_same_material_twice_produces_identical_bytes() {
        // 哈希表的迭代顺序不稳定，不排序的话同一份材质会存出不同的字节，
        // 「内容没变就不重写」这类判断就无从谈起。
        fn bytes(material: &Material) -> Vec<u8> {
            let mut visitor = Visitor::new();
            let mut source = material.clone();
            source.visit("M", &mut visitor).unwrap();
            visitor.save_binary_to_vec().unwrap()
        }

        let material = Material::standard()
            .with("z", 1.0f32)
            .with("a", 2.0f32)
            .with("m", 3.0f32)
            .with("b", 4.0f32);

        assert_eq!(bytes(&material), bytes(&material));
    }

    #[test]
    fn a_restored_material_starts_a_fresh_version_count() {
        // 读出来的就是文件里那份内容，从 0 起算；沿用旧版本号只会
        // 让「同一个 id 不同版本」的空间变得难以推理。
        let manager = kasset::ResourceManager::new();
        let mut material = Material::standard();
        for _ in 0..5 {
            material.set("x", 1.0f32);
        }
        assert!(material.version() > 0);

        assert_eq!(roundtrip(&material, &manager).version(), 0);
    }

    #[test]
    fn an_unknown_value_tag_is_rejected_instead_of_guessed() {
        // 未知标签只能是文件坏了或版本对不上，硬猜一个类型会画出乱七八糟的东西。
        let mut visitor = Visitor::new();
        {
            let mut region = visitor.enter_region("V").unwrap();
            let mut tag = 99u8;
            tag.visit("Tag", &mut region).unwrap();
        }
        let bytes = visitor.save_binary_to_vec().unwrap();

        let mut visitor = Visitor::load_binary_from_memory(&bytes).unwrap();
        let mut value = MaterialValue::default();

        assert!(value.visit("V", &mut visitor).is_err());
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
