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

#![warn(missing_docs)]

mod loader;

pub use loader::ShaderLoader;

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
    /// WGSL 语法解析失败。
    Parse(String),
    /// 通过了语法解析，但类型/资源校验失败。
    Validation(String),
}

impl fmt::Display for ShaderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
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
        })
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
        }
    }

    /// 着色器源码，交给 wgpu 创建 `ShaderModule`。
    pub fn source(&self) -> &str {
        &self.source
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
}
