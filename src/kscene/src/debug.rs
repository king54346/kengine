//! 把场景状态画成调试线。
//!
//! kgizmo 提供的是「怎么画一个球」，这里提供的是「场景里有哪些球该画」。
//! 两件事分开，是因为前者与引擎无关，后者要认识节点、骨架、物理世界。
//!
//! 这一层还负责一件事：rapier 的调试渲染给的是 **HSLA** 颜色，
//! 转成 RGBA 的地方就在这里——kphysics 不认识颜色类型，kgizmo 不认识物理。

use crate::Scene;
use kgizmo::Color;
use kmath::{Aabb, Vec3};
use kphysics::PhysicsDebugOptions;

/// 画哪些场景信息。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SceneDebugOptions {
    /// 每个可绘制节点的世界包围盒。
    pub bounds: bool,
    /// 剔除用的 BVH 的内部节点。
    pub bvh: bool,
    /// 骨架：关节之间连线。
    pub skeletons: bool,
    /// 每个节点的局部坐标系。
    pub node_axes: bool,
    /// 光源的位置与朝向。
    pub lights: bool,
    /// 相机的视锥。
    pub cameras: bool,
}

impl SceneDebugOptions {
    /// 一项都没开。
    pub fn is_empty(&self) -> bool {
        !(self.bounds
            || self.bvh
            || self.skeletons
            || self.node_axes
            || self.lights
            || self.cameras)
    }
}

/// 包围盒的颜色。
const BOUNDS_COLOR: Color = Color::rgba(0.2, 0.9, 0.3, 0.5);
/// 骨骼连线的颜色。
const BONE_COLOR: Color = Color::rgb(1.0, 0.6, 0.1);
/// 光源指示物的颜色。
const LIGHT_COLOR: Color = Color::rgb(1.0, 0.95, 0.5);
/// 相机视锥的颜色。
const CAMERA_COLOR: Color = Color::rgba(0.6, 0.4, 1.0, 0.8);

impl Scene {
    /// 把物理世界画进调试线缓冲。
    ///
    /// 碰撞体线框、刚体坐标轴、关节、接触点——具体画哪些由 `options` 决定。
    /// 调试绘制没开时直接返回，不会去问物理世界要数据。
    pub fn debug_draw_physics(&mut self, options: PhysicsDebugOptions) {
        if !self.gizmos().enabled() || options.is_empty() {
            return;
        }

        // 物理世界和 gizmo 缓冲都挂在 self 上，同时可变借用会撞车。
        // 把缓冲取出来用完再放回去——比给 Scene 加一层内部可变性划算。
        let mut gizmos = std::mem::take(self.gizmos_mut());
        self.physics().debug_render(options, &mut |a, b, hsla| {
            // rapier 给的是 HSLA，直接当 RGBA 用会得到一片惨白。
            gizmos.line(a, b, Color::from_hsla(hsla[0], hsla[1], hsla[2], hsla[3]));
        });
        *self.gizmos_mut() = gizmos;
    }

    /// 把场景结构画进调试线缓冲。
    pub fn debug_draw(&mut self, options: SceneDebugOptions) {
        if !self.gizmos().enabled() || options.is_empty() {
            return;
        }

        if options.bounds {
            self.draw_bounds();
        }
        if options.bvh {
            self.draw_bvh();
        }
        if options.skeletons {
            self.draw_skeletons();
        }
        if options.node_axes {
            self.draw_node_axes();
        }
        if options.lights {
            self.draw_lights();
        }
        if options.cameras {
            self.draw_cameras();
        }
    }

    /// 每个可绘制节点的世界包围盒。
    fn draw_bounds(&mut self) {
        let boxes: Vec<Aabb> = self
            .drawable_nodes()
            .iter()
            .filter_map(|h| self.try_get(*h).map(|n| n.global_aabb()))
            .collect();
        let gizmos = self.gizmos_mut();
        for aabb in boxes {
            gizmos.aabb(aabb, BOUNDS_COLOR);
        }
    }

    /// 剔除 BVH 的每个节点，按深度换色。
    ///
    /// 换色是有用的：同一层的盒子重叠得厉害，说明这一层的划分差，
    /// 查询会同时往两边走。全画一个颜色就看不出这件事。
    fn draw_bvh(&mut self) {
        let mut boxes = Vec::new();
        self.culling.bvh().visit_nodes(|aabb, depth, is_leaf| {
            boxes.push((aabb, depth, is_leaf));
        });

        let gizmos = self.gizmos_mut();
        for (aabb, depth, is_leaf) in boxes {
            // 每深一层转 55 度色相：相邻层次的颜色一定不一样。
            let color = if is_leaf {
                Color::WHITE.with_alpha(0.35)
            } else {
                Color::from_hsla(depth as f32 * 55.0, 0.9, 0.55, 0.35)
            };
            gizmos.aabb(aabb, color);
        }
    }

    /// 骨架：每个关节连到它在骨架里的父关节。
    fn draw_skeletons(&mut self) {
        let mut bones: Vec<(Vec3, Vec3)> = Vec::new();
        let mut joints: Vec<Vec3> = Vec::new();

        for handle in self.skinned_nodes().to_vec() {
            let Some(node) = self.try_get(handle) else {
                continue;
            };
            let Some(skin) = node.skin() else {
                continue;
            };

            for joint in skin.joints() {
                let Some(joint_node) = self.try_get(*joint) else {
                    continue;
                };
                let position = joint_node.global_transform.w_axis.truncate();
                joints.push(position);

                // 连到父节点——但只连到**同属这个骨架**的父节点，
                // 不然骨架根会甩出一条线连到场景根上。
                let parent = joint_node.parent();
                if skin.joints().contains(&parent)
                    && let Some(parent_node) = self.try_get(parent)
                {
                    bones.push((parent_node.global_transform.w_axis.truncate(), position));
                }
            }
        }

        let gizmos = self.gizmos_mut();
        // 骨架埋在模型里面，画在覆盖层才看得见。
        gizmos.on_top(|g| {
            for (from, to) in bones {
                g.line(from, to, BONE_COLOR);
            }
            for joint in joints {
                g.point(joint, 0.04, Color::WHITE);
            }
        });
    }

    /// 每个节点的局部坐标系。
    fn draw_node_axes(&mut self) {
        let transforms: Vec<_> = self
            .drawable_nodes()
            .iter()
            .filter_map(|h| self.try_get(*h).map(|n| n.global_transform))
            .collect();
        let gizmos = self.gizmos_mut();
        for matrix in transforms {
            gizmos.transform(matrix, 0.25);
        }
    }

    /// 光源：位置画个小球，方向光再补一根箭头。
    fn draw_lights(&mut self) {
        let lights: Vec<_> = self
            .light_nodes()
            .iter()
            .filter_map(|h| {
                let node = self.try_get(*h)?;
                Some((node.global_transform, *node.light()?))
            })
            .collect();

        let gizmos = self.gizmos_mut();
        for (matrix, light) in lights {
            let position = matrix.w_axis.truncate();
            let color = LIGHT_COLOR;
            gizmos.sphere(position, 0.15, color);

            // 本引擎的约定：光沿节点的 -Z 照出去。
            let direction = -matrix.z_axis.truncate().normalize_or_zero();
            if direction != Vec3::ZERO {
                gizmos.arrow(position, position + direction, color);
            }

            // 有作用范围的光把范围也画出来——调「为什么这盏灯照不到」时最有用。
            let range = light.kind.range();
            if range > 0.0 && range.is_finite() {
                gizmos.sphere(position, range, color.with_alpha(0.25));
            }
        }
    }

    /// 相机的视锥。
    ///
    /// 画的是**非活动**相机——活动相机的视锥就是屏幕本身，画出来只会
    /// 贴着画面边缘糊一圈。
    fn draw_cameras(&mut self) {
        let active = self.active_camera_node();
        let cameras: Vec<_> = self
            .camera_nodes()
            .iter()
            .filter(|h| Some(**h) != active)
            .filter_map(|h| {
                let node = self.try_get(*h)?;
                let camera = node.camera()?;
                // 视锥形状与屏幕宽高比无关的那部分才是相机自己的属性；
                // 这里用 16:9 画个示意，够看出朝向和远近了。
                Some((node.global_transform, camera.projection_matrix(16.0 / 9.0)))
            })
            .collect();

        let gizmos = self.gizmos_mut();
        for (to_world, projection) in cameras {
            gizmos.frustum(projection * to_world.inverse(), CAMERA_COLOR);
            gizmos.transform(to_world, 0.3);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Camera, Collider, Layer, Node, RigidBody, Skin, Transform};
    use kmath::{Mat4, Quat};
    use kmesh::Mesh;

    fn scene_with_gizmos() -> Scene {
        let mut scene = Scene::new();
        scene.gizmos_mut().set_enabled(true);
        scene
    }

    fn line_count(scene: &Scene) -> usize {
        scene.gizmos().len() / 2
    }

    fn all_options() -> SceneDebugOptions {
        SceneDebugOptions {
            bounds: true,
            bvh: true,
            skeletons: true,
            node_axes: true,
            lights: true,
            cameras: true,
        }
    }

    #[test]
    fn nothing_is_drawn_while_gizmos_are_off() {
        let mut scene = Scene::new();
        scene.add_node(Node::new("box").with_mesh(Mesh::cube()));
        scene.update();

        scene.debug_draw(all_options());
        scene.debug_draw_physics(PhysicsDebugOptions::all());

        assert!(scene.gizmos().is_empty());
    }

    #[test]
    fn empty_options_draw_nothing() {
        let mut scene = scene_with_gizmos();
        scene.add_node(Node::new("box").with_mesh(Mesh::cube()));
        scene.update();

        scene.debug_draw(SceneDebugOptions::default());
        assert!(scene.gizmos().is_empty());
    }

    #[test]
    fn bounds_are_drawn_once_per_drawable() {
        let mut scene = scene_with_gizmos();
        for i in 0..3 {
            scene.add_node(
                Node::new(format!("box{i}"))
                    .with_mesh(Mesh::cube())
                    .with_position(Vec3::X * i as f32 * 3.0),
            );
        }
        scene.update();

        scene.debug_draw(SceneDebugOptions {
            bounds: true,
            ..Default::default()
        });

        // 每个盒子十二条棱。
        assert_eq!(line_count(&scene), 3 * 12);
    }

    #[test]
    fn the_gizmo_buffer_survives_the_physics_swap() {
        // `debug_draw_physics` 会把缓冲 `mem::take` 出去再放回来；
        // 之前画的东西不能在这一进一出里丢掉。
        let mut scene = scene_with_gizmos();
        scene.gizmos_mut().line(Vec3::ZERO, Vec3::X, Color::RED);
        let before = scene.gizmos().len();

        scene.add_node(
            Node::new("body")
                .with_rigid_body(RigidBody::dynamic())
                .with_collider(Collider::cuboid(Vec3::splat(0.5))),
        );
        scene.update();
        // 碰撞体是在 step_physics 里才建进物理世界的，光 update 还没有。
        scene.step_physics(1.0 / 60.0);

        scene.debug_draw_physics(PhysicsDebugOptions::shapes_only());

        assert!(scene.gizmos().len() > before, "物理线段应当追加在后面");
        let first = scene.gizmos().vertices(Layer::Depth)[0];
        assert_eq!(first.color, Color::RED.to_array(), "先画的线不能被冲掉");
    }

    #[test]
    fn physics_colors_are_converted_out_of_hsla() {
        // rapier 给的色相能到 340；不转换的话所有分量都会被当成 RGB，
        // 结果是一片过曝的白。转换后每个分量都该落在 [0, 1]。
        let mut scene = scene_with_gizmos();
        scene.add_node(Node::new("body").with_collider(Collider::cuboid(Vec3::splat(0.5))));
        scene.update();
        scene.step_physics(1.0 / 60.0);

        scene.debug_draw_physics(PhysicsDebugOptions::shapes_only());

        assert!(!scene.gizmos().is_empty());
        for vertex in scene.gizmos().vertices(Layer::Depth) {
            assert!(
                vertex.color.iter().all(|c| (0.0..=1.0).contains(c)),
                "颜色分量越界：{:?}",
                vertex.color
            );
        }
    }

    #[test]
    fn the_bvh_is_drawn_layer_by_layer() {
        let mut scene = scene_with_gizmos();
        for i in 0..16 {
            scene.add_node(
                Node::new(format!("box{i}"))
                    .with_mesh(Mesh::cube())
                    .with_position(Vec3::new((i % 4) as f32 * 3.0, 0.0, (i / 4) as f32 * 3.0)),
            );
        }
        scene.update();

        scene.debug_draw(SceneDebugOptions {
            bvh: true,
            ..Default::default()
        });

        // 16 个物体的 BVH 至少有根加两个子节点。
        assert!(line_count(&scene) >= 3 * 12);
        assert!(
            scene
                .gizmos()
                .vertices(Layer::Depth)
                .iter()
                .all(|v| v.position.iter().all(|c| c.is_finite()))
        );
    }

    #[test]
    fn skeleton_lines_go_on_the_overlay() {
        // 骨架埋在模型里面，画在深度层等于看不见。
        let mut scene = scene_with_gizmos();

        let hip = scene.add_node(Node::new("hip"));
        let knee = scene.add_node_with_parent(Node::new("knee").with_position(Vec3::Y), hip);
        scene.add_node(
            Node::new("body")
                .with_mesh(Mesh::cube())
                .with_skin(Skin::new(vec![hip, knee], vec![Mat4::IDENTITY; 2])),
        );
        scene.update();

        scene.debug_draw(SceneDebugOptions {
            skeletons: true,
            ..Default::default()
        });

        assert!(scene.gizmos().vertices(Layer::Depth).is_empty());
        assert!(!scene.gizmos().vertices(Layer::Overlay).is_empty());
    }

    #[test]
    fn a_skeleton_root_does_not_connect_to_the_scene_root() {
        // 骨架根的父节点在骨架之外，连过去会甩出一条穿过半个场景的线。
        let mut scene = scene_with_gizmos();

        let anchor = scene.add_node(Node::new("anchor").with_position(Vec3::X * 100.0));
        let hip = scene.add_node_with_parent(Node::new("hip"), anchor);
        let knee = scene.add_node_with_parent(Node::new("knee").with_position(Vec3::Y), hip);
        scene.add_node(
            Node::new("body")
                .with_mesh(Mesh::cube())
                .with_skin(Skin::new(vec![hip, knee], vec![Mat4::IDENTITY; 2])),
        );
        scene.update();

        scene.debug_draw(SceneDebugOptions {
            skeletons: true,
            ..Default::default()
        });

        // 骨头只有一根（hip→knee），长度是 1；没有指向 anchor 的那根长线。
        let longest = scene
            .gizmos()
            .vertices(Layer::Overlay)
            .chunks(2)
            .map(|s| (Vec3::from_array(s[1].position) - Vec3::from_array(s[0].position)).length())
            .fold(0.0f32, f32::max);
        assert!(longest < 2.0, "最长的一段有 {longest}，骨架连错了");
    }

    #[test]
    fn the_active_camera_frustum_is_skipped() {
        // 活动相机的视锥就是屏幕本身，画出来只会贴着画面边缘糊一圈。
        let mut scene = scene_with_gizmos();
        scene.add_node(Node::new("active").with_camera(Camera::default()));
        scene.update();

        scene.debug_draw(SceneDebugOptions {
            cameras: true,
            ..Default::default()
        });
        assert!(scene.gizmos().is_empty());

        // 再加一个停用的相机，它不是活动的，应当被画出来。
        let disabled = Camera {
            enabled: false,
            ..Default::default()
        };
        scene.add_node(
            Node::new("other")
                .with_camera(disabled)
                .with_position(Vec3::Z * 5.0),
        );
        scene.update();

        scene.debug_draw(SceneDebugOptions {
            cameras: true,
            ..Default::default()
        });
        assert!(!scene.gizmos().is_empty());
    }

    #[test]
    fn a_light_draws_a_marker_and_a_direction() {
        let mut scene = scene_with_gizmos();
        scene.add_node(
            Node::new("sun")
                .with_light(klight::Light::directional())
                .with_transform(Transform {
                    rotation: Quat::from_rotation_x(-0.5),
                    ..Default::default()
                }),
        );
        scene.update();

        scene.debug_draw(SceneDebugOptions {
            lights: true,
            ..Default::default()
        });

        assert!(!scene.gizmos().is_empty());
        assert!(
            scene
                .gizmos()
                .vertices(Layer::Depth)
                .iter()
                .all(|v| v.position.iter().all(|c| c.is_finite()))
        );
    }

    #[test]
    fn a_point_light_also_draws_its_range() {
        // 方向光没有作用范围，点光有——多出来的那个球就是范围。
        let mut scene = scene_with_gizmos();
        scene.add_node(Node::new("sun").with_light(klight::Light::directional()));
        scene.update();
        scene.debug_draw(SceneDebugOptions {
            lights: true,
            ..Default::default()
        });
        let directional = line_count(&scene);

        let mut scene = scene_with_gizmos();
        scene.add_node(Node::new("bulb").with_light(klight::Light::point(8.0)));
        scene.update();
        scene.debug_draw(SceneDebugOptions {
            lights: true,
            ..Default::default()
        });
        let point = line_count(&scene);

        assert!(
            point > directional,
            "点光 {point} 段，方向光 {directional} 段"
        );
    }
}
