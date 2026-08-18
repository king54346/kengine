//! 声源组件：把 `kaudio` 的声音挂到场景节点上。
//!
//! # 谁驱动谁
//!
//! 与物理相反，音频是**单向**的：场景图驱动音频，音频不反过来改场景。
//! 声源的世界位置来自它所在节点，听者的位姿来自启用的相机。
//! 于是「声音跟着物体走」不需要任何额外代码——挂上去就对了。
//!
//! # 资源是异步的
//!
//! 声源持有的是 [`Resource<AudioBuffer>`]，请求时未必已经解码完。
//! 组件因此每帧检查一次：资源就绪的**那一帧**才真正把声音交给混音器。
//! 这样调用方不必等待、也不必轮询，挂上就行。

use crate::{Node, Scene};
use kasset::Resource;
use kaudio::{AudioBuffer, AudioDevice, Listener, Sound, Spatial, Status};
use kcore::pool::Handle;
use kmath::Vec3;

/// 挂在节点上的声源。
#[derive(Debug, Clone)]
pub struct SoundSource {
    buffer: Resource<AudioBuffer>,
    /// 音量倍率。
    pub gain: f32,
    /// 播放速度，同时改变音高。
    pub pitch: f32,
    /// 是否循环。
    pub looping: bool,
    /// 空间参数。为 [`None`] 时是 2D 声音（背景音乐、旁白）。
    pub spatial: Option<Spatial>,
    /// 加进场景后是否立刻播放。
    pub autoplay: bool,
    /// 期望的播放状态。同步时推给混音器。
    desired: Status,
    /// 混音器里对应的声音；还没交出去时为 [`None`]。
    native: Option<Handle<Sound>>,
    /// 非循环的声音播完后混音器会回收它，这里记一笔免得反复重播。
    finished: bool,
}

impl SoundSource {
    /// 用一份音频资源创建一个 2D 声源，默认自动播放、不循环。
    pub fn new(buffer: Resource<AudioBuffer>) -> Self {
        Self {
            buffer,
            gain: 1.0,
            pitch: 1.0,
            looping: false,
            spatial: None,
            autoplay: true,
            desired: Status::Playing,
            native: None,
            finished: false,
        }
    }

    /// 3D 声源：音量随距离衰减、按方向摆声像。
    pub fn spatial(buffer: Resource<AudioBuffer>, spatial: Spatial) -> Self {
        Self {
            spatial: Some(spatial),
            ..Self::new(buffer)
        }
    }

    /// 指定音量。
    pub fn with_gain(mut self, gain: f32) -> Self {
        self.gain = gain.max(0.0);
        self
    }

    /// 指定播放速度。
    pub fn with_pitch(mut self, pitch: f32) -> Self {
        self.pitch = pitch.max(0.0);
        self
    }

    /// 设为循环。
    pub fn looping(mut self) -> Self {
        self.looping = true;
        self
    }

    /// 建好后不自动播放，等显式调 [`play`](Self::play)。
    pub fn paused(mut self) -> Self {
        self.autoplay = false;
        self.desired = Status::Paused;
        self
    }

    /// 音频资源。
    pub fn buffer(&self) -> &Resource<AudioBuffer> {
        &self.buffer
    }

    /// 换一份音频。会把当前正在播的那个停掉。
    pub fn set_buffer(&mut self, buffer: Resource<AudioBuffer>) {
        self.buffer = buffer;
        self.native = None;
        self.finished = false;
    }

    /// 期望的播放状态。
    pub fn status(&self) -> Status {
        self.desired
    }

    /// 播放（或从暂停处继续）。
    pub fn play(&mut self) {
        self.desired = Status::Playing;
    }

    /// 暂停。
    pub fn pause(&mut self) {
        self.desired = Status::Paused;
    }

    /// 停止并倒回开头。
    pub fn stop(&mut self) {
        self.desired = Status::Stopped;
    }

    /// 从头重放。非循环的声音播完之后要靠它再来一次。
    pub fn restart(&mut self) {
        self.native = None;
        self.finished = false;
        self.desired = Status::Playing;
    }

    /// 声音是否已经播完（非循环声源专用）。
    pub fn is_finished(&self) -> bool {
        self.finished
    }

    /// 混音器里对应的声音句柄。
    pub fn native(&self) -> Option<Handle<Sound>> {
        self.native
    }
}

impl Scene {
    /// 把场景里的声源与听者同步给音频设备。
    ///
    /// 每帧调用，排在 [`update`](Self::update) **之后**——声源的世界位置
    /// 取自节点的世界变换，排在前面的话声音会比画面慢一帧。
    ///
    /// 听者取自启用的相机；场景里没有相机时听者留在原地。
    pub fn tick_audio(&mut self, device: &AudioDevice) {
        // 先在锁外把要用的数据备齐，锁内只做赋值——音频回调正在另一条线程上
        // 等这把锁，持锁期间做任何耗时的事都会直接变成断音。
        let listener = self
            .active_camera()
            .map(|(world, _)| Listener::from_matrix(world));

        let mut pending: Vec<(Handle<Node>, Vec3, SoundSnapshot)> = Vec::new();
        for position in 0..self.index.sounds.len() {
            let handle = self.index.sounds[position];
            let Some(node) = self.try_get(handle) else {
                continue;
            };
            let Some(source) = node.sound() else {
                continue;
            };
            pending.push((
                handle,
                node.global_transform.w_axis.truncate(),
                SoundSnapshot {
                    native: source.native(),
                    desired: source.desired,
                    gain: source.gain,
                    pitch: source.pitch,
                    looping: source.looping,
                    spatial: source.spatial,
                    // 资源没解码完时拿不到缓冲，这一帧先跳过，下一帧再看。
                    buffer: source
                        .native()
                        .is_none()
                        .then(|| source.buffer.data_ref().map(|data| data.clone()))
                        .flatten(),
                    finished: source.finished,
                },
            ));
        }

        let mut updates: Vec<(Handle<Node>, Option<Handle<Sound>>, bool)> = Vec::new();
        {
            let mut mixer = device.mixer().lock();
            if let Some(listener) = listener {
                mixer.listener = listener;
            }

            for (handle, world_position, snapshot) in pending {
                match snapshot.native {
                    // 已经在混音器里：推同步，并检查是不是已经播完被回收了。
                    Some(native) => match mixer.sound_mut(native) {
                        Some(sound) => {
                            sound.position = world_position;
                            sound.gain = snapshot.gain;
                            sound.pitch = snapshot.pitch;
                            sound.looping = snapshot.looping;
                            sound.spatial = snapshot.spatial;
                            match snapshot.desired {
                                Status::Playing => sound.play(),
                                Status::Paused => sound.pause(),
                                Status::Stopped => sound.stop(),
                            }
                        }
                        // 混音器把它回收了，说明非循环的声音播完了。
                        None => updates.push((handle, None, true)),
                    },
                    None => {
                        // 播完过、或者不想播、或者资源还没就绪，都不新建。
                        if snapshot.finished || snapshot.desired == Status::Stopped {
                            continue;
                        }
                        let Some(buffer) = snapshot.buffer else {
                            continue;
                        };

                        let mut sound = Sound::new(buffer);
                        sound.position = world_position;
                        sound.gain = snapshot.gain;
                        sound.pitch = snapshot.pitch;
                        sound.looping = snapshot.looping;
                        sound.spatial = snapshot.spatial;
                        if snapshot.desired == Status::Paused {
                            sound.pause();
                        }

                        let native = mixer.add(sound);
                        updates.push((handle, Some(native), false));
                    }
                }
            }
        }

        for (handle, native, finished) in updates {
            if let Some(source) = self.try_get_mut(handle).and_then(Node::sound_mut) {
                source.native = native;
                source.finished = finished;
            }
        }
    }

    /// 把场景里所有声源从混音器里摘掉。
    ///
    /// 切场景时要调，否则上一张地图的声音会一直响着——混音器不认识场景，
    /// 没人告诉它这些声音已经没有主人了。
    pub fn stop_all_sounds(&mut self, device: &AudioDevice) {
        let mut mixer = device.mixer().lock();
        for position in 0..self.index.sounds.len() {
            let handle = self.index.sounds[position];
            let Some(source) = self.nodes.try_borrow_mut(handle).ok().and_then(|n| n.sound.as_deref_mut())
            else {
                continue;
            };
            if let Some(native) = source.native.take() {
                mixer.remove(native);
            }
            source.finished = false;
        }
    }
}

/// 锁外备好的一份声源快照。
struct SoundSnapshot {
    native: Option<Handle<Sound>>,
    desired: Status,
    gain: f32,
    pitch: f32,
    looping: bool,
    spatial: Option<Spatial>,
    buffer: Option<AudioBuffer>,
    finished: bool,
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::{Camera, Transform};
    use kasset::{MemoryResourceIo, ResourceManager};
    use kaudio::{AudioLoader, encode_wav};
    use std::sync::Arc;

    /// 一个装好音频加载器、内存里放着一段 0.5 秒正弦的资源管理器。
    fn manager() -> ResourceManager {
        let wav = encode_wav(&AudioBuffer::tone(440.0, 0.5, 48_000));
        let looping = encode_wav(&AudioBuffer::tone(220.0, 0.05, 48_000));
        let io = MemoryResourceIo::new()
            .with("beep.wav", wav)
            .with("hum.wav", looping);

        let manager = ResourceManager::with_io(Arc::new(io));
        manager.add_loader(AudioLoader);
        manager
    }

    fn beep(manager: &ResourceManager) -> Resource<AudioBuffer> {
        manager.request_blocking::<AudioBuffer>("beep.wav").unwrap()
    }

    #[test]
    fn a_sound_source_reaches_the_mixer_once_its_resource_is_ready() {
        let device = AudioDevice::silent();
        let manager = manager();
        let mut scene = Scene::new();
        let node = scene.add_node(Node::new("beeper").with_sound(SoundSource::new(beep(&manager))));

        scene.update();
        scene.tick_audio(&device);

        assert_eq!(device.mixer().lock().len(), 1);
        assert!(scene[node].sound().unwrap().native().is_some());
    }

    #[test]
    fn a_source_whose_resource_is_still_loading_waits_instead_of_failing() {
        // 资源是异步解码的，挂上去的那一帧未必已经就绪。
        let device = AudioDevice::silent();
        let manager = manager();
        let mut scene = Scene::new();
        // 请求一个根本不存在的文件：永远不会就绪。
        let missing = manager.request::<AudioBuffer>("nope.wav");
        scene.add_node(Node::new("silent").with_sound(SoundSource::new(missing)));

        scene.update();
        for _ in 0..5 {
            scene.tick_audio(&device);
        }

        assert_eq!(device.mixer().lock().len(), 0, "没就绪的资源不该进混音器");
    }

    #[test]
    fn the_sound_follows_its_node() {
        let device = AudioDevice::silent();
        let manager = manager();
        let mut scene = Scene::new();
        let node = scene.add_node(
            Node::new("mover")
                .with_sound(SoundSource::spatial(beep(&manager), Spatial::default())),
        );

        scene.update();
        scene.tick_audio(&device);

        scene[node].transform.position = Vec3::new(7.0, 0.0, -3.0);
        scene.update();
        scene.tick_audio(&device);

        let native = scene[node].sound().unwrap().native().unwrap();
        let mixer = device.mixer().lock();
        assert_eq!(mixer.sound(native).unwrap().position, Vec3::new(7.0, 0.0, -3.0));
    }

    #[test]
    fn the_listener_follows_the_active_camera() {
        let device = AudioDevice::silent();
        let mut scene = Scene::new();
        scene.add_node(
            Node::new("camera")
                .with_camera(Camera::default())
                .with_transform(Transform::looking_at(
                    Vec3::new(0.0, 2.0, 10.0),
                    Vec3::ZERO,
                    Vec3::Y,
                )),
        );

        scene.update();
        scene.tick_audio(&device);

        let listener = device.mixer().lock().listener;
        assert!((listener.position - Vec3::new(0.0, 2.0, 10.0)).length() < 1e-4);
        // 相机看向原点，听者的前方也该指向原点。
        assert!(listener.forward.z < 0.0, "听者朝向不对：{:?}", listener.forward);
    }

    #[test]
    fn a_scene_without_a_camera_leaves_the_listener_alone() {
        let device = AudioDevice::silent();
        device.mixer().lock().listener.position = Vec3::splat(5.0);

        let mut scene = Scene::new();
        scene.update();
        scene.tick_audio(&device);

        assert_eq!(device.mixer().lock().listener.position, Vec3::splat(5.0));
    }

    #[test]
    fn changing_the_gain_reaches_the_mixer() {
        let device = AudioDevice::silent();
        let manager = manager();
        let mut scene = Scene::new();
        let node = scene.add_node(Node::new("s").with_sound(SoundSource::new(beep(&manager))));

        scene.update();
        scene.tick_audio(&device);
        scene[node].sound_mut().unwrap().gain = 0.25;
        scene.tick_audio(&device);

        let native = scene[node].sound().unwrap().native().unwrap();
        assert_eq!(device.mixer().lock().sound(native).unwrap().gain, 0.25);
    }

    #[test]
    fn pausing_and_resuming_go_through() {
        let device = AudioDevice::silent();
        let manager = manager();
        let mut scene = Scene::new();
        let node = scene.add_node(Node::new("s").with_sound(SoundSource::new(beep(&manager))));

        scene.update();
        scene.tick_audio(&device);
        let native = scene[node].sound().unwrap().native().unwrap();

        scene[node].sound_mut().unwrap().pause();
        scene.tick_audio(&device);
        assert_eq!(device.mixer().lock().sound(native).unwrap().status(), Status::Paused);

        scene[node].sound_mut().unwrap().play();
        scene.tick_audio(&device);
        assert_eq!(device.mixer().lock().sound(native).unwrap().status(), Status::Playing);
    }

    #[test]
    fn a_source_created_paused_does_not_start_on_its_own() {
        let device = AudioDevice::silent();
        let manager = manager();
        let mut scene = Scene::new();
        let node = scene
            .add_node(Node::new("s").with_sound(SoundSource::new(beep(&manager)).paused()));

        scene.update();
        scene.tick_audio(&device);

        let native = scene[node].sound().unwrap().native().unwrap();
        assert_eq!(device.mixer().lock().sound(native).unwrap().status(), Status::Paused);
    }

    #[test]
    fn a_one_shot_sound_is_marked_finished_and_not_restarted() {
        // 播完了还自动重来的话，一个脚步声会变成永动机。
        let device = AudioDevice::silent();
        let manager = manager();
        let mut scene = Scene::new();
        let node = scene.add_node(Node::new("s").with_sound(SoundSource::new(beep(&manager))));

        scene.update();
        scene.tick_audio(&device);

        // 把它渲染完：0.5 秒的素材，一次渲染 48000 帧足够。
        let mut out = vec![0.0; 48_000 * 2 * 2];
        device.mixer().lock().render(&mut out, 2);

        scene.tick_audio(&device);
        assert!(scene[node].sound().unwrap().is_finished());
        assert_eq!(device.mixer().lock().len(), 0);

        // 再同步几帧也不该复活。
        for _ in 0..5 {
            scene.tick_audio(&device);
        }
        assert_eq!(device.mixer().lock().len(), 0);
    }

    #[test]
    fn restart_replays_a_finished_sound() {
        let device = AudioDevice::silent();
        let manager = manager();
        let mut scene = Scene::new();
        let node = scene.add_node(Node::new("s").with_sound(SoundSource::new(beep(&manager))));

        scene.update();
        scene.tick_audio(&device);
        let mut out = vec![0.0; 48_000 * 2 * 2];
        device.mixer().lock().render(&mut out, 2);
        scene.tick_audio(&device);
        assert!(scene[node].sound().unwrap().is_finished());

        scene[node].sound_mut().unwrap().restart();
        scene.tick_audio(&device);

        assert_eq!(device.mixer().lock().len(), 1);
        assert!(!scene[node].sound().unwrap().is_finished());
    }

    #[test]
    fn a_looping_sound_never_gets_marked_finished() {
        let device = AudioDevice::silent();
        let manager = manager();
        let mut scene = Scene::new();
        let node = scene.add_node(
            Node::new("hum").with_sound(
                SoundSource::new(manager.request_blocking::<AudioBuffer>("hum.wav").unwrap())
                    .looping(),
            ),
        );

        scene.update();
        scene.tick_audio(&device);
        let mut out = vec![0.0; 48_000 * 2];
        for _ in 0..4 {
            device.mixer().lock().render(&mut out, 2);
            scene.tick_audio(&device);
        }

        assert!(!scene[node].sound().unwrap().is_finished());
        assert_eq!(device.mixer().lock().len(), 1);
    }

    #[test]
    fn stopping_all_sounds_clears_the_mixer() {
        // 切场景时不清的话，上一张地图的声音会一直响着。
        let device = AudioDevice::silent();
        let manager = manager();
        let mut scene = Scene::new();
        for index in 0..3 {
            scene.add_node(
                Node::new(format!("s{index}")).with_sound(SoundSource::new(beep(&manager))),
            );
        }

        scene.update();
        scene.tick_audio(&device);
        assert_eq!(device.mixer().lock().len(), 3);

        scene.stop_all_sounds(&device);

        assert_eq!(device.mixer().lock().len(), 0);
    }

    #[test]
    fn a_spatial_source_is_quieter_when_the_listener_walks_away() {
        // 端到端：场景里挪一下相机，混出来的样本就该变小。
        let device = AudioDevice::silent();
        let manager = manager();
        let mut scene = Scene::new();

        let camera = scene.add_node(
            Node::new("camera")
                .with_camera(Camera::default())
                .with_position(Vec3::ZERO),
        );
        scene.add_node(
            Node::new("emitter")
                .with_position(Vec3::new(0.0, 0.0, -2.0))
                .with_sound(
                    SoundSource::new(manager.request_blocking::<AudioBuffer>("hum.wav").unwrap())
                        .looping(),
                )
                .with_sound_spatial(Spatial::default().with_range(1.0, 200.0)),
        );

        scene.update();
        scene.tick_audio(&device);

        let mut out = vec![0.0; 512];
        device.mixer().lock().render(&mut out, 2);
        device.mixer().lock().render(&mut out, 2);
        let near: f32 = out.iter().map(|s| s.abs()).sum();

        scene[camera].transform.position = Vec3::new(0.0, 0.0, 100.0);
        scene.update();
        scene.tick_audio(&device);
        device.mixer().lock().render(&mut out, 2);
        device.mixer().lock().render(&mut out, 2);
        let far: f32 = out.iter().map(|s| s.abs()).sum();

        assert!(far < near * 0.2, "走远之后没变小：近 {near} 远 {far}");
    }
}
