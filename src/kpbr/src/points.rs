//! 点精灵材质：把点云画成正对相机的小方块。
//!
//! 和 [`physical`](crate::physical) 同一个定位——引擎自带的一份材质钩子，
//! 由导入器和应用代码共用。放在 kpbr 而不是某个导入器里，是因为
//! 「点云怎么画」是渲染侧的决定：PCD、PDB、LiDAR、粒子调试视图都会用到
//! 同一套几何与着色器，各自抄一份的话改一次就得改好几处。
//!
//! ```no_run
//! use kmath::{Mat4, Vec3};
//! use kpbr::points::PointMaterial;
//!
//! let mut material = PointMaterial::new(0.01);
//! // 每帧：把相机换算到点云节点的局部空间。
//! PointMaterial::set_camera(&mut material, Vec3::new(0.0, 1.0, 3.0), Mat4::IDENTITY);
//! ```

use kasset::Resource;
use kmaterial::Material;
use kmath::{Mat4, Vec3, Vec4};
use kshader::Shader;
use std::sync::OnceLock;

/// 点精灵材质的构造与每帧更新。
#[derive(Debug, Clone, Copy)]
pub struct PointMaterial;

impl PointMaterial {
    /// 建一个点精灵材质。`radius` 是每个点在世界里的半边长。
    ///
    /// 配套的几何必须由 [`kmesh::Mesh::point_sprites`] 生成——顶点的
    /// `uv` 编码了四个角的位置，普通网格挂上这个材质会整个塌成一个点。
    pub fn new(radius: f32) -> Material {
        static SHADER: OnceLock<Resource<Shader>> = OnceLock::new();
        Material::standard()
            .with_name("points")
            .with_shader(
                SHADER
                    .get_or_init(|| {
                        Resource::new_ok(
                            "builtin/points.wgsl",
                            Shader::snippet(include_str!("points.wgsl")),
                        )
                    })
                    .clone(),
            )
            .with_base_color(Vec4::ONE)
            .with_param(0, Vec4::new(radius.max(0.0), 0.0, 0.0, 0.0))
            // 相机位置的初值：摆在很远的地方，第一帧还没调 set_camera 时
            // 点也不至于塌成零面积。
            .with_param(1, Vec4::new(0.0, 0.0, 1e4, 0.0))
    }

    /// 每帧把相机位置换算到点云节点的局部空间写进材质。
    ///
    /// `node_world` 是点云那个节点的世界变换矩阵。矩阵不可逆时（缩放为零）
    /// 保持上一帧的值——不更新比写进一堆 NaN 好，NaN 会让整片点消失。
    pub fn set_camera(material: &mut Material, camera_world: Vec3, node_world: Mat4) {
        let inverse = node_world.inverse();
        let local = inverse.transform_point3(camera_world);
        if local.is_finite() {
            material.set_param(1, local.extend(0.0));
        }
    }

    /// 改点的大小。
    pub fn set_radius(material: &mut Material, radius: f32) {
        let existing = material
            .param(0)
            .and_then(|value| value.as_vec4())
            .unwrap_or(Vec4::ZERO);
        material.set_param(0, Vec4::new(radius.max(0.0), existing.y, existing.z, existing.w));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_hook_compiles() {
        // 着色器片段单独编不过——它引用 `VertexSurface` 等引擎结构体。
        // 完整拼接后的校验在 `krender` 的 `material_shader_tests` 里。
        assert!(include_str!("points.wgsl").contains("fn material_vertex"));
    }

    #[test]
    fn the_camera_lands_in_node_space() {
        let mut material = PointMaterial::new(0.5);
        let node = Mat4::from_translation(Vec3::new(10.0, 0.0, 0.0));
        PointMaterial::set_camera(&mut material, Vec3::new(11.0, 0.0, 0.0), node);
        let local = material.param(1).unwrap().as_vec4().unwrap();
        assert_eq!(local.truncate(), Vec3::new(1.0, 0.0, 0.0));
    }

    #[test]
    fn a_singular_node_transform_leaves_the_old_value_alone() {
        let mut material = PointMaterial::new(0.5);
        PointMaterial::set_camera(&mut material, Vec3::ONE, Mat4::ZERO);
        let value = material.param(1).unwrap().as_vec4().unwrap();
        assert!(value.is_finite(), "不该把 NaN 写进材质，实际是 {value:?}");
    }
}
