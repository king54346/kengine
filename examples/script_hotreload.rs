//! 脚本：JS 驱动场景，改完存盘立刻生效。
//!
//! ```bash
//! cargo run --example script_hotreload
//! ```
//!
//! 跑起来之后**别关窗口**，去编辑 `assets/scripts/spinner.js`，
//! 比如把 `delta * 1.5` 改成 `delta * 5.0`，存盘——方块立刻转得更快。
//!
//! # 脚本能直接读写场景
//!
//! 脚本回调跑在引擎线程上，场景在回调期间是**活的**：
//!
//! ```js
//! self.position.y += delta;              // 立刻写进场景
//! const hit = raycast(from, dir, 10.0);  // 当场拿到结果
//! if (hit) self.lookAt(hit.position);    // 据此决定下一步
//! ```
//!
//! 「快照进、命令出」那套架构做不到第二行：查询只能排到下一帧。
//!
//! # 脚本文件是一个函数体
//!
//! 不是对象字面量，是**函数体**，`return` 一个带生命周期方法的对象。
//! 这样每个实例有自己的闭包变量，而不是所有实例共享一份全局状态。

use kengine::prelude::*;

#[derive(Default)]
struct ScriptDemo {
    spinner: Handle<Node>,
}

impl Plugin for ScriptDemo {
    fn init(&mut self, ctx: &mut Context) {
        ctx.input
            .bindings_mut()
            .bind_action("toggle", KeyCode::KeyJ);

        ctx.scene.add_node(
            Node::new("Camera")
                .with_camera(Camera::default())
                .with_transform(Transform::looking_at(
                    Vec3::new(0.0, 2.5, 6.0),
                    Vec3::new(0.0, 1.0, 0.0),
                    Vec3::Y,
                )),
        );
        ctx.scene.add_node(
            Node::new("Sun")
                .with_light(Light::directional().with_intensity(2.5))
                .with_transform(Transform::looking_at(
                    Vec3::new(3.0, 5.0, 4.0),
                    Vec3::ZERO,
                    Vec3::Y,
                )),
        );

        // 地面给 follower 的射线一个落点。
        ctx.scene.add_node(
            Node::new("Ground")
                .with_mesh(Mesh::cube())
                .with_material(PbrMaterial::metal(Vec3::splat(0.3), 0.9))
                .with_scale(Vec3::new(12.0, 0.2, 12.0))
                .with_collider(Collider::cuboid(Vec3::new(6.0, 0.1, 6.0)))
                .with_position(Vec3::new(0.0, -0.1, 0.0)),
        );

        // 脚本路径就是资源路径。文件改动由引擎的热重载看门人监听，
        // 变更时脚本实例会被重建——注意实例里的闭包变量会跟着重置。
        self.spinner = ctx.scene.add_node(
            Node::new("ScriptSpinner")
                .with_mesh(Mesh::cube())
                .with_material(PbrMaterial::emissive(
                    Vec3::new(1.0, 0.6, 0.2),
                    Vec3::new(2.0, 0.9, 0.2),
                ))
                .with_position(Vec3::Y)
                .with_script("assets/scripts/spinner.js"),
        );

        ctx.scene.add_node(
            Node::new("ScriptFollower")
                .with_mesh(Mesh::cube())
                .with_material(PbrMaterial::emissive(
                    Vec3::new(0.3, 0.5, 1.0),
                    Vec3::new(0.4, 0.8, 2.5),
                ))
                .with_scale(Vec3::splat(0.25))
                .with_script("assets/scripts/follower.js"),
        );

        klog::info!("改 assets/scripts/spinner.js 存盘即热重载；J 键停用/恢复脚本，Esc 退出");
    }

    fn update(&mut self, ctx: &mut Context) {
        // 脚本排在插件的 update 之前跑，所以这里读到的是**本帧**的信号。
        for signal in ctx.script_events {
            klog::info!("信号 {} = {:.2}", signal.name, signal.value);
        }

        if ctx.input.action_just_pressed("toggle")
            && let Some(slot) = ctx
                .scene
                .try_get_mut(self.spinner)
                .and_then(Node::script_mut)
        {
            slot.enabled = !slot.enabled;
            klog::info!(
                "脚本{}",
                if slot.enabled {
                    "已恢复"
                } else {
                    "已停用"
                }
            );
        }

        if ctx.input.key_just_pressed(KeyCode::Escape) {
            ctx.request_exit();
        }
    }
}

fn main() {
    klog::init(None);
    App::new()
        .with_title("kengine — script hot reload")
        // 热重载默认就是开的，这里写出来是为了让它显眼。
        .with_hot_reload(true)
        .add_plugin(ScriptDemo::default())
        .run();
}
