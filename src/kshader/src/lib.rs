//! kshader —— 着色器资源。
//!
//! 加载 WGSL 时会用 naga **解析并校验**，把语法与类型错误从「GPU 编译期崩溃」
//! 提前到「资源加载期返回 Err」，并顺便反射出入口点信息。
//!
//! ```
//! use kshader::prelude::*;
//!
//! let shader = Shader::from_wgsl(r#"
//!     @vertex fn vs_main() -> @builtin(position) vec4<f32> {
//!         return vec4<f32>(0.0, 0.0, 0.0, 1.0);
//!     }
//!     @fragment fn fs_main() -> @location(0) vec4<f32> {
//!         return vec4<f32>(1.0, 0.0, 0.0, 1.0);
//!     }
//! "#).unwrap();
//!
//! assert_eq!(shader.vertex_entry(), Some("vs_main"));
//! assert_eq!(shader.fragment_entry(), Some("fs_main"));
//! ```
//!
//! 写错的着色器会在这里就被拦下：
//!
//! ```
//! # use kshader::prelude::*;
//! let broken = Shader::from_wgsl("@vertex fn vs() -> InvalidType { }");
//! assert!(broken.is_err());
//! ```
//!
//! # 一份源码，多个变体
//!
//! 同一段 WGSL 要出几个不同版本（带不带法线贴图、色阶分几档）时，有两条路，
//! **两条都在这里**：
//!
//! | | 什么时候生效 | 代价 |
//! |---|---|---|
//! | [`defs`](Shader::from_wgsl_with_defs) | 编译**前**，整段代码留下或删掉 | 每个组合一份源码 |
//! | [`constants`](Shader::with_constant) | 编译**时**，交给 GPU 驱动替换 | 只能换数，不能换代码 |
//!
//! ```
//! # use kshader::prelude::*;
//! let source = "
//!     #ifdef TINT
//!     const COLOR: f32 = 1.0;
//!     #else
//!     const COLOR: f32 = 0.0;
//!     #endif
//!     @fragment fn fs() -> @location(0) vec4<f32> { return vec4<f32>(COLOR); }
//! ";
//!
//! let tinted = Shader::from_wgsl_with_defs(source, &["TINT"]).unwrap();
//! assert!(tinted.source().contains("1.0"));
//! assert!(!tinted.source().contains("0.0"));
//! ```
//!
//! 两者共同的要点是**变体即资源**：每建一个变体就是一个新的 [`Shader`]，
//! 带自己的 [`id`](Shader::id)，渲染器照着 id 缓存管线。这条不变量让
//! 「同一份材质换个开关」不必给渲染器加任何新概念。

#![warn(missing_docs)]

mod loader;
mod preprocess;

pub use loader::ShaderLoader;
pub use preprocess::preprocess;

use kasset::ResourceData;
use kcore::uuid::{Uuid, uuid};
use std::{error::Error, fmt};

/// [`Shader`] 的资源类型标识。
pub const SHADER_TYPE_UUID: Uuid = uuid!("d83f7b21-4e6a-4c09-b7d5-1f2a9c8e5b70");

/// 常用类型的集中导出。
pub mod prelude {
    pub use crate::{Shader, ShaderError, ShaderLoader, ShaderStage};
}

/// 着色器阶段。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShaderStage {
    /// 顶点着色器。
    Vertex,
    /// 片元着色器。
    Fragment,
    /// 计算着色器。
    Compute,
    /// 其他阶段：网格/任务着色器、光线追踪等。引擎目前不使用，但保留以免信息丢失。
    Other,
}

/// 着色器编译错误。
#[derive(Debug)]
pub enum ShaderError {
    /// 条件编译指令写坏了，还没轮到 WGSL 解析。
    Preprocess(String),
    /// WGSL 语法解析失败。
    Parse(String),
    /// 通过了语法解析，但类型/资源校验失败。
    Validation(String),
}

impl fmt::Display for ShaderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Preprocess(message) => write!(f, "条件编译失败：{message}"),
            Self::Parse(message) => write!(f, "WGSL 解析失败：{message}"),
            Self::Validation(message) => write!(f, "WGSL 校验失败：{message}"),
        }
    }
}

impl Error for ShaderError {}

/// 一个已通过校验的着色器。
#[derive(Debug, Clone)]
pub struct Shader {
    id: Uuid,
    source: String,
    entry_points: Vec<(String, ShaderStage)>,
    constants: Vec<(String, f64)>,
}

impl Shader {
    /// 解析并校验一段 WGSL。
    ///
    /// 校验通过后只保留源码与入口点信息——`naga::Module` 不再持有，
    /// 因为 wgpu 会自己重新解析源码，留着反而占内存。
    pub fn from_wgsl(source: impl Into<String>) -> Result<Self, ShaderError> {
        let source = source.into();

        let module = naga::front::wgsl::parse_str(&source)
            .map_err(|e| ShaderError::Parse(e.emit_to_string(&source)))?;

        // 校验能抓出解析阶段发现不了的问题，例如类型不匹配、绑定冲突。
        let mut validator = naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::default(),
        );
        validator
            .validate(&module)
            .map_err(|e| ShaderError::Validation(e.emit_to_string(&source)))?;

        let entry_points = module
            .entry_points
            .iter()
            .map(|ep| {
                let stage = match ep.stage {
                    naga::ShaderStage::Vertex => ShaderStage::Vertex,
                    naga::ShaderStage::Fragment => ShaderStage::Fragment,
                    naga::ShaderStage::Compute => ShaderStage::Compute,
                    // 网格/任务着色器与光追各阶段引擎还用不到，归入 Other 而不是硬塞进 Compute。
                    _ => ShaderStage::Other,
                };
                (ep.name.clone(), stage)
            })
            .collect();

        Ok(Self {
            id: Uuid::new_v4(),
            source,
            entry_points,
            constants: Vec::new(),
        })
    }

    /// 先按一组开关做条件编译，再解析并校验。
    ///
    /// 见 [`preprocess`]。**每个开关组合是一份独立的 [`Shader`]**，
    /// 各带自己的 [`id`](Self::id)，所以渲染器缓存管线时天然按变体分开。
    ///
    /// ```
    /// # use kshader::prelude::*;
    /// let source = "
    ///     @fragment fn fs() -> @location(0) vec4<f32> {
    ///         #ifdef RED
    ///         return vec4<f32>(1.0, 0.0, 0.0, 1.0);
    ///         #else
    ///         return vec4<f32>(1.0);
    ///         #endif
    ///     }
    /// ";
    ///
    /// let red = Shader::from_wgsl_with_defs(source, &["RED"]).unwrap();
    /// let plain = Shader::from_wgsl_with_defs(source, &[]).unwrap();
    ///
    /// assert_ne!(red.id(), plain.id(), "两个变体必须是两份资源");
    /// ```
    pub fn from_wgsl_with_defs(source: &str, defs: &[&str]) -> Result<Self, ShaderError> {
        Self::from_wgsl(preprocess(source, defs)?)
    }

    /// 一段**片段**着色器代码，不做独立校验。
    ///
    /// 材质钩子（`fn material_surface(...)`）引用引擎定义的 `Surface`
    /// 结构和各个绑定，单独拿去解析必然报「找不到标识符」。真正的校验
    /// 发生在渲染器把它和标准着色器拼起来之后——那时上下文才完整。
    ///
    /// # 代价
    ///
    /// 语法错误要到第一次用上这份材质时才会报出来，而不是加载时。
    /// 报错信息里的行号是**拼接之后**的，和源文件对不上。
    pub fn snippet(source: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4(),
            source: source.into(),
            // 片段里没有入口点，拼进主着色器之后用的是引擎的那些。
            entry_points: Vec::new(),
            constants: Vec::new(),
        }
    }

    /// 先做条件编译，再当作片段收下。
    ///
    /// 材质钩子最常要的就是这个：同一份钩子按开关出几个变体，
    /// 而钩子本身单独解析不了（引用引擎定义的 `Surface`）。
    ///
    /// # Errors
    ///
    /// 只有条件编译指令本身写坏了才会失败——WGSL 的语法错误仍然要等到
    /// 拼进主着色器之后才发现，和 [`snippet`](Self::snippet) 一样。
    pub fn snippet_with_defs(source: &str, defs: &[&str]) -> Result<Self, ShaderError> {
        Ok(Self::snippet(preprocess(source, defs)?))
    }

    /// 着色器源码，交给 wgpu 创建 `ShaderModule`。
    pub fn source(&self) -> &str {
        &self.source
    }

    /// 指定一个管线常量（WGSL 的 `override`）的值。
    ///
    /// 和 [`defs`](Self::from_wgsl_with_defs) 的分界很清楚：
    /// **defs 换代码，常量换数**。常量由 GPU 驱动在建管线时替换，
    /// 之后就是个编译期常数——循环次数、数组长度都能用它，
    /// 而 uniform 不行。
    ///
    /// ```
    /// # use kshader::prelude::*;
    /// let source = "
    ///     override LEVELS: f32 = 4.0;
    ///     @fragment fn fs() -> @location(0) vec4<f32> {
    ///         return vec4<f32>(floor(0.5 * LEVELS) / LEVELS);
    ///     }
    /// ";
    ///
    /// let coarse = Shader::from_wgsl(source).unwrap().with_constant("LEVELS", 2.0);
    /// assert_eq!(coarse.constants(), &[("LEVELS".to_string(), 2.0)]);
    /// ```
    ///
    /// 设了常量的着色器会拿到一个**新的 id**：不这样的话两个只有常量
    /// 不同的变体会共用一条管线，渲染器照着 id 取缓存就会画出另一个变体。
    ///
    /// 同一个名字设两次，后一次生效。
    pub fn with_constant(mut self, name: impl Into<String>, value: f64) -> Self {
        let name = name.into();
        match self.constants.iter_mut().find(|(key, _)| *key == name) {
            Some(slot) => slot.1 = value,
            None => self.constants.push((name, value)),
        }
        self.id = Uuid::new_v4();
        self
    }

    /// 已设定的管线常量。
    pub fn constants(&self) -> &[(String, f64)] {
        &self.constants
    }

    /// 管线常量，摆成 wgpu 的 `PipelineCompilationOptions::constants` 要的形状。
    pub fn constant_overrides(&self) -> Vec<(&str, f64)> {
        self.constants
            .iter()
            .map(|(name, value)| (name.as_str(), *value))
            .collect()
    }

    /// 显存缓存键。克隆的着色器共享同一个 id。
    pub fn id(&self) -> Uuid {
        self.id
    }

    /// 全部入口点。
    pub fn entry_points(&self) -> &[(String, ShaderStage)] {
        &self.entry_points
    }

    /// 指定阶段的第一个入口点名称。
    pub fn entry_point(&self, stage: ShaderStage) -> Option<&str> {
        self.entry_points
            .iter()
            .find(|(_, s)| *s == stage)
            .map(|(name, _)| name.as_str())
    }

    /// 顶点着色器入口点名称。
    pub fn vertex_entry(&self) -> Option<&str> {
        self.entry_point(ShaderStage::Vertex)
    }

    /// 片元着色器入口点名称。
    pub fn fragment_entry(&self) -> Option<&str> {
        self.entry_point(ShaderStage::Fragment)
    }
}

impl ResourceData for Shader {
    fn type_uuid(&self) -> Uuid {
        SHADER_TYPE_UUID
    }
}

#[cfg(test)]
mod test {
    use super::*;

    const VALID: &str = r#"
        @vertex
        fn vs_main(@location(0) pos: vec3<f32>) -> @builtin(position) vec4<f32> {
            return vec4<f32>(pos, 1.0);
        }

        @fragment
        fn fs_main() -> @location(0) vec4<f32> {
            return vec4<f32>(1.0, 1.0, 1.0, 1.0);
        }
    "#;

    #[test]
    fn reflects_entry_points() {
        let shader = Shader::from_wgsl(VALID).unwrap();

        assert_eq!(shader.vertex_entry(), Some("vs_main"));
        assert_eq!(shader.fragment_entry(), Some("fs_main"));
        assert_eq!(shader.entry_points().len(), 2);
    }

    #[test]
    fn syntax_error_is_caught_at_load_time() {
        let error = Shader::from_wgsl("@vertex fn broken( {").unwrap_err();

        assert!(matches!(error, ShaderError::Parse(_)));
        // 错误信息应当包含可定位的上下文，而不是一句空话。
        assert!(!error.to_string().is_empty());
    }

    #[test]
    fn type_error_is_caught_by_validator() {
        // 语法合法，但返回类型与声明不符——只有校验器能发现。
        let source = r#"
            @fragment
            fn fs_main() -> @location(0) vec4<f32> {
                return vec2<f32>(1.0, 0.0);
            }
        "#;

        assert!(Shader::from_wgsl(source).is_err());
    }

    #[test]
    fn compute_shader_is_recognized() {
        let source = r#"
            @compute @workgroup_size(1)
            fn cs_main() {}
        "#;

        let shader = Shader::from_wgsl(source).unwrap();

        assert_eq!(shader.entry_point(ShaderStage::Compute), Some("cs_main"));
        assert_eq!(shader.vertex_entry(), None);
    }

    #[test]
    fn clone_shares_gpu_id() {
        let shader = Shader::from_wgsl(VALID).unwrap();
        assert_eq!(shader.id(), shader.clone().id());
    }

    // ── 条件编译 ──

    const SWITCHED: &str = r#"
        @fragment
        fn fs_main() -> @location(0) vec4<f32> {
            #ifdef RED
            return vec4<f32>(1.0, 0.0, 0.0, 1.0);
            #else
            return vec4<f32>(1.0, 1.0, 1.0, 1.0);
            #endif
        }
    "#;

    #[test]
    fn defs_pick_a_branch_and_the_result_still_validates() {
        let red = Shader::from_wgsl_with_defs(SWITCHED, &["RED"]).unwrap();

        assert!(red.source().contains("1.0, 0.0, 0.0"));
        assert!(!red.source().contains("1.0, 1.0, 1.0"));
        assert_eq!(red.fragment_entry(), Some("fs_main"));
    }

    #[test]
    fn each_def_combination_is_its_own_resource() {
        // 变体共用 id 的话，渲染器照着 id 取缓存就会画出另一个变体。
        let red = Shader::from_wgsl_with_defs(SWITCHED, &["RED"]).unwrap();
        let plain = Shader::from_wgsl_with_defs(SWITCHED, &[]).unwrap();

        assert_ne!(red.id(), plain.id());
    }

    #[test]
    fn a_broken_directive_fails_before_wgsl_parsing() {
        // 报「条件编译」而不是「WGSL 解析」，才指得到真正出错的地方。
        let error = Shader::from_wgsl_with_defs("#ifdef A\n", &[]).unwrap_err();
        assert!(matches!(error, ShaderError::Preprocess(_)), "{error}");
    }

    #[test]
    fn a_def_that_leaves_broken_wgsl_still_gets_caught() {
        // 预处理只管指令，删剩下的东西照样要过 naga 那一关。
        let source = "#ifdef A\n@fragment fn fs() -> Nope {}\n#endif";

        assert!(Shader::from_wgsl_with_defs(source, &[]).is_ok(), "该被删掉");
        assert!(Shader::from_wgsl_with_defs(source, &["A"]).is_err());
    }

    #[test]
    fn snippet_defs_do_not_require_the_fragment_to_parse() {
        // 钩子引用引擎定义的 `Surface`，单独解析必然失败——
        // 但条件编译该照常生效。
        let hook = "#ifdef GLOW\nout.emissive = vec3<f32>(1.0);\n#endif";

        assert!(
            Shader::snippet_with_defs(hook, &["GLOW"])
                .unwrap()
                .source()
                .contains("emissive")
        );
        assert!(
            !Shader::snippet_with_defs(hook, &[])
                .unwrap()
                .source()
                .contains("emissive")
        );
    }

    // ── 管线常量 ──

    const OVERRIDABLE: &str = r#"
        override LEVELS: f32 = 4.0;

        @fragment
        fn fs_main() -> @location(0) vec4<f32> {
            return vec4<f32>(floor(0.5 * LEVELS) / LEVELS);
        }
    "#;

    #[test]
    fn override_declarations_pass_validation() {
        // 没给值的 `override` 也要能过校验——值是建管线时才填的。
        assert!(Shader::from_wgsl(OVERRIDABLE).is_ok());
    }

    #[test]
    fn constants_are_recorded_in_order() {
        let shader = Shader::from_wgsl(OVERRIDABLE)
            .unwrap()
            .with_constant("LEVELS", 2.0);

        assert_eq!(shader.constant_overrides(), vec![("LEVELS", 2.0)]);
    }

    #[test]
    fn setting_the_same_constant_twice_keeps_the_last_value() {
        let shader = Shader::from_wgsl(OVERRIDABLE)
            .unwrap()
            .with_constant("LEVELS", 2.0)
            .with_constant("LEVELS", 8.0);

        assert_eq!(shader.constant_overrides(), vec![("LEVELS", 8.0)]);
    }

    #[test]
    fn a_different_constant_means_a_different_pipeline() {
        // 这是整条路的要害：id 不变的话两个变体会共用一条管线。
        let base = Shader::from_wgsl(OVERRIDABLE).unwrap();
        let two = base.clone().with_constant("LEVELS", 2.0);
        let eight = base.clone().with_constant("LEVELS", 8.0);

        assert_ne!(two.id(), eight.id());
        assert_ne!(two.id(), base.id());
    }

    #[test]
    fn a_shader_without_constants_reports_none() {
        assert!(Shader::from_wgsl(VALID).unwrap().constants().is_empty());
    }
}
