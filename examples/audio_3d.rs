//! 空间音频：一个绕着你转圈的声源。
//!
//! ```bash
//! cargo run --example audio_3d
//! ```
//!
//! **戴耳机听**。绿色小球绕圈时能听出左右与远近；WASD 走动会改变相对位置。
//!
//! # 听者是相机
//!
//! 声源的位置取自节点的世界变换，听者的位置取自**活动相机**的世界变换。
//! 所以音频同步必须排在世界变换重算之后——排在前面的话，声音会比画面慢一帧。
//! 引擎的帧循环已经排好了。
//!
//! # 等功率声像
//!
//! 左右声道满足 `L² + R² = 1`，而不是 `L + R = 1`。线性声像在正中间时
//! 两边各 0.5，**总功率只有两侧的一半**——声音扫过中间会有个明显的凹陷。

use kengine::prelude::*;

/// 采样率。程序化生成的音频直接按这个率写。
const RATE: u32 = 48_000;

#[derive(Default)]
struct AudioDemo {
    orbit: Handle<Node>,
    camera: Handle<Node>,
    beep: Option<Resource<AudioBuffer>>,
    /// 正在播的一次性音效，播完就删。
    beeps: Vec<Handle<Node>>,
    muted: bool,
}

impl AudioDemo {
    /// 一段能循环得上的低频嗡鸣。
    fn hum(frequency: f32) -> AudioBuffer {
        // 帧数取成整周期数，首尾才接得上——差一点点就会每循环一次「哒」一声。
        let periods = 20.0;
        let frames = (periods * RATE as f32 / frequency).round() as usize;
        let samples = (0..frames)
            .map(|i| {
                let phase = std::f32::consts::TAU * frequency * i as f32 / RATE as f32;
                (phase.sin() * 0.5 + (phase * 3.0).sin() * 0.15) * 0.35
            })
            .collect();
        AudioBuffer::new(samples, 1, RATE)
    }

    /// 一声短促的提示音，带指数衰减包络。
    fn beep(frequency: f32, seconds: f32) -> AudioBuffer {
        let frames = (seconds * RATE as f32) as usize;
        let samples = (0..frames)
            .map(|i| {
                let t = i as f32 / RATE as f32;
                // 没有包络的话首尾会「啪」地一下，那是波形突变，不是音色。
                let envelope = (-4.0 * t / seconds.max(1e-3)).exp();
                (std::f32::consts::TAU * frequency * t).sin() * envelope * 0.4
            })
            .collect();
        AudioBuffer::new(samples, 1, RATE)
    }
}

impl Plugin for AudioDemo {
    fn init(&mut self, ctx: &mut Context) {
        let b = ctx.input.bindings_mut();
        b.bind_action("beep", KeyCode::KeyB);
        b.bind_action("mute", KeyCode::KeyN);
        b.bind_axis("horizontal", KeyCode::KeyD, KeyCode::KeyA);
        b.bind_axis("forward", KeyCode::KeyW, KeyCode::KeyS);

        ctx.resources.add_loader(AudioLoader);
        // 程序化生成的音频直接登记为资源，不需要外部文件。
        let hum = ctx.resources.register("builtin/hum", Self::hum(110.0));
        self.beep = Some(
            ctx.resources
                .register("builtin/beep", Self::beep(660.0, 0.35)),
        );

        self.camera = ctx.scene.add_node(
            Node::new("Camera")
                .with_camera(Camera::default())
                .with_transform(Transform::looking_at(
                    Vec3::new(0.0, 1.6, 0.0),
                    Vec3::NEG_Z,
                    Vec3::Y,
                )),
        );
        ctx.scene.add_node(
            Node::new("Sun")
                .with_light(Light::directional().with_intensity(2.0))
                .with_transform(Transform::looking_at(
                    Vec3::new(3.0, 6.0, 4.0),
                    Vec3::ZERO,
                    Vec3::Y,
                )),
        );
        ctx.scene.add_node(
            Node::new("Ground")
                .with_mesh(Mesh::cube())
                .with_material(PbrMaterial::metal(Vec3::splat(0.3), 0.9))
                .with_scale(Vec3::new(30.0, 0.2, 30.0))
                .with_position(Vec3::new(0.0, -0.1, 0.0)),
        );

        self.orbit = ctx.scene.add_node(
            Node::new("Emitter")
                .with_mesh(Mesh::sphere(12, 16))
                .with_material(PbrMaterial::emissive(
                    Vec3::new(0.2, 0.9, 0.4),
                    Vec3::new(0.3, 2.0, 0.8),
                ))
                .with_scale(Vec3::splat(0.25))
                .with_sound(
                    SoundSource::spatial(
                        hum,
                        // 反比衰减、5 米参考距离：走近明显变响，走远迅速淡出。
                        // 参考距离给大了会听不出远近，给小了会一走开就没声。
                        Spatial::default()
                            .with_range(5.0, 60.0)
                            .with_model(Attenuation::Inverse, 1.2),
                    )
                    .looping()
                    .with_gain(0.8),
                ),
        );

        match ctx.audio.name() {
            Some(name) => klog::info!("音频输出：{name}"),
            None => klog::warn!("没有可用的音频输出，会静默运行"),
        }
        klog::info!("戴耳机听。WASD 走动，B 提示音，N 静音，Esc 退出");
    }

    fn update(&mut self, ctx: &mut Context) {
        // 声源绕圈。它是普通节点，改的是局部变换，音频自动跟上。
        let angle = ctx.elapsed * 0.6;
        if let Some(node) = ctx.scene.try_get_mut(self.orbit) {
            node.transform.position = Vec3::new(angle.cos() * 6.0, 1.2, angle.sin() * 6.0 - 4.0);
        }

        // 走动改变听者位置——空间音频的另一半靠这个才听得出来。
        let strafe = ctx.input.axis("horizontal");
        let forward = ctx.input.axis("forward");
        if let Some(node) = ctx.scene.try_get_mut(self.camera) {
            let speed = 4.0 * ctx.dt;
            node.transform.position += Vec3::new(strafe * speed, 0.0, -forward * speed);
        }

        if ctx.input.action_just_pressed("beep")
            && let Some(beep) = self.beep.clone()
        {
            // 一次性音效也是挂在节点上的——声音的位置来自节点变换，
            // 没有节点就没有位置。这里用 `SoundSource::new`（非空间）
            // 所以摆在哪都一样响。
            let node = ctx.scene.add_node(
                Node::new("Beep")
                    .with_position(Vec3::new(0.0, 1.0, 0.0))
                    .with_sound(SoundSource::new(beep).with_gain(0.7)),
            );
            self.beeps.push(node);
        }

        // 播完的一次性音效连节点一起清掉，不然按一次多一个节点。
        self.beeps.retain(|handle| {
            let finished = ctx
                .scene
                .try_get(*handle)
                .and_then(Node::sound)
                .is_some_and(SoundSource::is_finished);
            if finished {
                ctx.scene.remove_node(*handle);
            }
            !finished
        });

        if ctx.input.action_just_pressed("mute") {
            self.muted = !self.muted;
            // 总音量在混音器上，不在设备上：设备只负责把混好的块推给声卡。
            ctx.audio.mixer().lock().master_gain = if self.muted { 0.0 } else { 1.0 };
            klog::info!("{}", if self.muted { "静音" } else { "取消静音" });
        }

        if ctx.input.key_just_pressed(KeyCode::Escape) {
            ctx.request_exit();
        }
    }
}

fn main() {
    klog::init(None);
    App::new()
        .with_title("kengine — 3D audio")
        .add_plugin(AudioDemo::default())
        .run();
}
