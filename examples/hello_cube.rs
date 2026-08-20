//! 最小的一个 kengine 程序：一个转动的方块。
//!
//! ```bash
//! cargo run --example hello_cube
//! ```
//!
//! 引擎要跑起来只需要三样东西：一台相机、一盏灯、一个带网格的节点。
//! 三者都是**普通场景节点**——相机不是特殊对象，光源也不是，
//! 它们只是挂了不同组件的节点，同样能设父子、同样跟着父节点动。

use kengine::prelude::*;

struct HelloCube {
    cube: Handle<Node>,
}

impl Plugin for HelloCube {
    fn init(&mut self, ctx: &mut Context) {
        // 相机。朝向取节点的 -Z 轴，所以「看向哪里」是摆节点，不是设参数。
        ctx.scene.add_node(
            Node::new("Camera")
                .with_camera(Camera::default())
                .with_transform(Transform::looking_at(
                    Vec3::new(0.0, 1.5, 4.0),
                    Vec3::ZERO,
                    Vec3::Y,
                )),
        );

        // 方向光。同样是 -Z 照出去。
        ctx.scene.add_node(
            Node::new("Sun")
                .with_light(Light::directional().with_intensity(3.0))
                .with_transform(Transform::looking_at(
                    Vec3::new(2.0, 4.0, 3.0),
                    Vec3::ZERO,
                    Vec3::Y,
                )),
        );

        self.cube = ctx.scene.add_node(
            Node::new("Cube")
                .with_mesh(Mesh::cube())
                .with_material(PbrMaterial::metal(Vec3::new(0.9, 0.6, 0.2), 0.35)),
        );

        klog::info!("转动的方块。Esc 退出。");
    }

    fn update(&mut self, ctx: &mut Context) {
        // 改的是**局部**变换。引擎随后会沿树把世界变换算出来，
        // 所以这里不必关心父节点在哪。
        if let Some(node) = ctx.scene.try_get_mut(self.cube) {
            node.transform.rotate_y(ctx.dt);
            node.transform.rotate_x(ctx.dt * 0.5);
        }

        if ctx.input.key_just_pressed(KeyCode::Escape) {
            ctx.request_exit();
        }
    }
}

fn main() {
    klog::init(None);
    App::new()
        .with_title("kengine — hello cube")
        .add_plugin(HelloCube { cube: Handle::NONE })
        .run();
}
