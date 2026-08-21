//! 脚本 API 覆盖面的测试：动画、粒子、音频。
//!
//! 这些参数以前一个都没暴露给脚本——想让一个特效的发射率随剧情变化，
//! 只能在 Rust 侧写死。

use crate::{Script, ScriptRuntime};
use kasset::{MemoryResourceIo, ResourceManager};
use kmath::Vec3;
use kscene::{Node, Scene};
use std::sync::Arc;

fn resources(source: &str) -> ResourceManager {
    let io = MemoryResourceIo::new().with("s.js", source.as_bytes().to_vec());
    let manager = ResourceManager::with_io(Arc::new(io));
    manager.add_loader(crate::ScriptLoader);
    // 不预热的话资源还在异步解码，脚本一个都建不起来。
    let _ = manager.request_blocking::<Script>("s.js");
    manager
}

fn tick(runtime: &mut ScriptRuntime, scene: &mut Scene, manager: &ResourceManager) -> Vec<String> {
    scene.update();
    runtime
        .process(scene, manager, 1.0 / 60.0, 0.0)
        .into_iter()
        .map(|s| s.name)
        .collect()
}

/// 一个带粒子系统的节点。
fn particle_node() -> Node {
    let mut system = kscene::ParticleSystem::new(kparticle::Emitter {
        rate: 10.0,
        ..Default::default()
    });
    system.playing = true;
    Node::new("n").with_particles(system).with_script("s.js")
}

// ── 粒子 ──

#[test]
fn a_script_can_stop_and_start_particles() {
    let manager = resources("return { _ready() { self.stopParticles(); } };");
    let mut scene = Scene::new();
    let handle = scene.add_node(particle_node());

    let mut runtime = ScriptRuntime::new();
    tick(&mut runtime, &mut scene, &manager);

    assert!(
        !scene.try_get(handle).unwrap().particles().unwrap().playing,
        "脚本没能停掉粒子"
    );
}

#[test]
fn a_script_can_change_the_emission_rate() {
    let manager = resources("return { _ready() { self.emissionRate = 250; } };");
    let mut scene = Scene::new();
    let handle = scene.add_node(particle_node());

    let mut runtime = ScriptRuntime::new();
    tick(&mut runtime, &mut scene, &manager);

    let rate = scene
        .try_get(handle)
        .unwrap()
        .particles()
        .unwrap()
        .emitter
        .rate;
    assert_eq!(rate, 250.0);
}

#[test]
fn a_negative_emission_rate_is_clamped() {
    // 负的发射率会让生成计数往回走，攒出一个巨大的负债，
    // 之后调正了也很久不出粒子。
    let manager = resources("return { _ready() { self.emissionRate = -100; } };");
    let mut scene = Scene::new();
    let handle = scene.add_node(particle_node());

    let mut runtime = ScriptRuntime::new();
    tick(&mut runtime, &mut scene, &manager);

    assert_eq!(
        scene
            .try_get(handle)
            .unwrap()
            .particles()
            .unwrap()
            .emitter
            .rate,
        0.0
    );
}

#[test]
fn a_script_can_burst_particles() {
    // 一次性喷发：爆炸、受击特效都靠它，而且必须由游戏逻辑触发。
    let manager = resources("return { _ready() { self.burst(50); } };");
    let mut scene = Scene::new();
    let mut node = particle_node();
    // 关掉持续发射，这样计数只可能来自 burst。
    node.particles_mut().unwrap().emitter.rate = 0.0;
    let handle = scene.add_node(node);

    let mut runtime = ScriptRuntime::new();
    tick(&mut runtime, &mut scene, &manager);

    let alive = scene.try_get(handle).unwrap().particles().unwrap().alive();
    assert_eq!(alive, 50, "喷发出来的粒子数不对");
}

#[test]
fn a_script_can_read_the_particle_count() {
    let manager = resources(
        r#"
        return {
            _ready() {
                self.burst(7);
                emit("n" + self.particleCount);
            },
        };
        "#,
    );
    let mut scene = Scene::new();
    let mut node = particle_node();
    node.particles_mut().unwrap().emitter.rate = 0.0;
    scene.add_node(node);

    let mut runtime = ScriptRuntime::new();
    assert_eq!(tick(&mut runtime, &mut scene, &manager), vec!["n7"]);
}

#[test]
fn particle_calls_on_a_node_without_particles_do_nothing() {
    // 挂错节点是很常见的手误。不该崩，也不该让后面的代码不执行。
    let manager = resources(
        r#"
        return {
            _ready() {
                self.burst(10);
                self.stopParticles();
                self.emissionRate = 5;
                emit("survived:" + self.particleCount);
            },
        };
        "#,
    );
    let mut scene = Scene::new();
    scene.add_node(Node::new("n").with_script("s.js"));

    let mut runtime = ScriptRuntime::new();
    assert_eq!(
        tick(&mut runtime, &mut scene, &manager),
        vec!["survived:0"]
    );
}

// ── 音频 ──

/// 一个带声源的节点。用一段静音缓冲，测试里不需要真的出声。
fn sound_node() -> Node {
    let buffer = kscene::AudioBuffer::new(vec![0.0; 128], 2, 44_100);
    let resource = kasset::Resource::new_ok("silent.wav", buffer);
    Node::new("n")
        .with_sound(kscene::SoundSource::new(resource))
        .with_script("s.js")
}

#[test]
fn a_script_can_set_volume_and_pitch() {
    let manager = resources("return { _ready() { self.volume = 0.25; self.pitch = 1.5; } };");
    let mut scene = Scene::new();
    let handle = scene.add_node(sound_node());

    let mut runtime = ScriptRuntime::new();
    tick(&mut runtime, &mut scene, &manager);

    let sound = scene.try_get(handle).unwrap().sound().unwrap();
    assert_eq!(sound.gain, 0.25);
    assert_eq!(sound.pitch, 1.5);
}

#[test]
fn a_zero_pitch_is_clamped() {
    // 音高为 0 会让播放头永远不前进：声音既不响也不结束，
    // 那个声源就永远占着混音器的一路。
    let manager = resources("return { _ready() { self.pitch = 0; } };");
    let mut scene = Scene::new();
    let handle = scene.add_node(sound_node());

    let mut runtime = ScriptRuntime::new();
    tick(&mut runtime, &mut scene, &manager);

    assert!(scene.try_get(handle).unwrap().sound().unwrap().pitch > 0.0);
}

#[test]
fn a_script_can_set_looping() {
    let manager = resources("return { _ready() { self.soundLooping = true; } };");
    let mut scene = Scene::new();
    let handle = scene.add_node(sound_node());

    let mut runtime = ScriptRuntime::new();
    tick(&mut runtime, &mut scene, &manager);

    assert!(scene.try_get(handle).unwrap().sound().unwrap().looping);
}

#[test]
fn a_negative_volume_is_clamped() {
    // 负增益在混音器里会变成反相，两个声源叠加时互相抵消——
    // 表现为「加了一个音效，别的声音反而没了」。
    let manager = resources("return { _ready() { self.volume = -2; } };");
    let mut scene = Scene::new();
    let handle = scene.add_node(sound_node());

    let mut runtime = ScriptRuntime::new();
    tick(&mut runtime, &mut scene, &manager);

    assert_eq!(scene.try_get(handle).unwrap().sound().unwrap().gain, 0.0);
}

// ── 动画 ──

#[test]
fn playing_a_missing_clip_returns_false_instead_of_throwing() {
    // 美术改个剪辑名不该让整个脚本停掉。
    let manager = resources(
        r#"
        return {
            _ready() {
                const ok = self.playAnimation("不存在的剪辑");
                emit(ok === false ? "false" : "other");
            },
        };
        "#,
    );
    let mut scene = Scene::new();
    scene.add_node(Node::new("n").with_script("s.js"));

    let mut runtime = ScriptRuntime::new();
    assert_eq!(tick(&mut runtime, &mut scene, &manager), vec!["false"]);
}

#[test]
fn animation_calls_on_a_node_without_an_animator_do_nothing() {
    let manager = resources(
        r#"
        return {
            _ready() {
                self.stopAnimation();
                self.animationSpeed = 2;
                emit("playing:" + self.animationPlaying);
            },
        };
        "#,
    );
    let mut scene = Scene::new();
    scene.add_node(Node::new("n").with_script("s.js"));

    let mut runtime = ScriptRuntime::new();
    assert_eq!(
        tick(&mut runtime, &mut scene, &manager),
        vec!["playing:false"]
    );
}

// ── 组合 ──

#[test]
fn the_new_api_works_on_other_nodes_not_just_self() {
    // `getNode` 拿到的节点也该能用这些方法——特效节点通常是主角的
    // 兄弟或子节点，不是它自己。
    let manager = resources(
        r#"
        return {
            _ready() {
                const fx = getNode("fx");
                fx.burst(12);
                emit("n" + fx.particleCount);
            },
        };
        "#,
    );
    let mut scene = Scene::new();
    scene.add_node(Node::new("n").with_script("s.js"));
    // 特效节点自己不挂脚本——从零建一个，别用带脚本的辅助函数。
    let mut system = kscene::ParticleSystem::new(kparticle::Emitter {
        rate: 0.0,
        ..Default::default()
    });
    system.playing = true;
    scene.add_node(Node::new("fx").with_particles(system));

    let mut runtime = ScriptRuntime::new();
    assert_eq!(tick(&mut runtime, &mut scene, &manager), vec!["n12"]);
}

#[test]
fn bursting_uses_the_world_transform() {
    // 喷发发生在世界空间。用局部变换的话，挂在移动物体上的特效
    // 会全部从原点喷出来。
    let manager = resources("return { _ready() { self.burst(20); } };");
    let mut scene = Scene::new();
    let mut node = particle_node();
    node.particles_mut().unwrap().emitter.rate = 0.0;
    node.transform.position = Vec3::new(100.0, 0.0, 0.0);
    let handle = scene.add_node(node);

    let mut runtime = ScriptRuntime::new();
    tick(&mut runtime, &mut scene, &manager);

    let system = scene.try_get(handle).unwrap().particles().unwrap();
    assert_eq!(system.alive(), 20);
    // 世界空间的粒子应当在 x=100 附近，不是原点。
    let positions = system.positions();
    assert!(
        positions.iter().all(|p| p.x > 50.0),
        "粒子从原点喷出来了，用的是局部变换"
    );
}
