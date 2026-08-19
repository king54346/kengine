//! kparticle —— 粒子系统。
//!
//! 一个粒子系统 = 一个[发射器](Emitter) + 一池粒子 + 两条随寿命变化的[曲线](Gradient)。
//! 本 crate **不依赖 wgpu**：模拟在 CPU 上跑完，导出一份中立的 [`GpuParticle`] 数组，
//! 由渲染器上传显存；着色器源码作为常量 [`PARTICLE_WGSL`] 一并提供。
//!
//! # 存储用「列」而不是「行」
//!
//! 粒子不是 `Vec<Particle>`，而是每个属性各占一个数组（SoA）——
//! 这正是 Bevy 的 ECS `Table` 的做法：*列式连续内存，为快速迭代优化*。
//! 好处有三：
//!
//! - 更新只碰用得到的列，不把整个粒子结构拖进缓存；
//! - 每列都是连续的 `f32`/`Vec3`，编译器能自动向量化；
//! - 分片并行时切的是切片，不需要任何同步。
//!
//! 死亡粒子用 `swap_remove` 就地填补（同样是 ECS `Table` 的做法），
//! 池子因此永远紧凑，迭代时不需要判断「这个还活着吗」。
//! 代价是粒子顺序会变——但粒子本来就要按到相机的距离重新排序，无所谓。
//!
//! 没有引入全局 ECS `World`：引擎已经是场景图架构，为一个子系统再引入一套
//! 实体存储只会多出一层需要同步的状态。ECS 在这里有价值的是**存储布局**，不是调度器。
//!
//! ```
//! use kparticle::{Emitter, ParticleSystem};
//! use kmath::{Mat4, Vec3};
//!
//! let mut system = ParticleSystem::new(Emitter::cone(20.0).with_rate(100.0))
//!     .with_acceleration(Vec3::new(0.0, -9.8, 0.0));
//!
//! // 推进半秒：100 个/秒 → 50 个粒子。
//! for _ in 0..30 {
//!     system.tick(1.0 / 60.0, Mat4::IDENTITY);
//! }
//!
//! assert_eq!(system.alive(), 50);
//! ```

#![warn(missing_docs)]

mod collision;
mod emitter;
mod gradient;
mod rng;

pub use collision::{Collision, CollisionResponse, Resolved, SurfaceHit, resolve_at_surface};
pub use emitter::{Emitter, EmitterShape, Spawn, SpawnClock};
pub use gradient::{ColorGradient, Curve, Gradient};
pub use rng::{Lerp, Rng, Span};

use bytemuck::{Pod, Zeroable};
use kasset::Resource;
use kmath::{Aabb, Mat4, Vec3, Vec4};
use ktask::{ComputeTaskPool, TaskPool};
use ktexture::Texture;

/// 粒子数低于这个值就单线程模拟：分片、唤醒线程、汇合本身也要开销，
/// 少量粒子时这些固定成本比模拟本身还贵。
const PARALLEL_THRESHOLD: usize = 2048;

/// 常用类型的集中导出。
pub mod prelude {
    pub use crate::{
        BlendMode, ColorGradient, Curve, Emitter, EmitterShape, ParticleSystem, Space, Span,
    };
}

/// 粒子着色器源码。
///
/// 自带绑定声明，可以直接编译成模块：
/// `group(0)` 全局量、`group(1)` 粒子数组、`group(2)` 贴图。
pub const PARTICLE_WGSL: &str = include_str!("particle.wgsl");

/// 粒子在哪个空间里演化。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Space {
    /// 世界空间：粒子出生后与发射器脱钩。
    ///
    /// 发射器移动时会拉出一条尾迹——火箭尾焰、脚步扬尘都要这个。
    #[default]
    World,
    /// 局部空间：粒子跟着节点一起走。
    ///
    /// 整团粒子被当作一个整体搬运，适合手持火把、护盾这类附着效果。
    Local,
}

/// 粒子与背景的混合方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BlendMode {
    /// 普通半透明叠加，适合烟、雾、碎片。
    #[default]
    Alpha,
    /// 相加，越叠越亮，适合火花、能量、魔法。
    Additive,
}

/// 一个粒子的 GPU 数据。
///
/// 字段顺序与 `particle.wgsl` 的 `Particle` 结构一致，改动需同步两边。
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Pod, Zeroable)]
pub struct GpuParticle {
    /// 世界空间位置。
    pub position: [f32; 3],
    /// 边长。
    pub size: f32,
    /// 线性空间 RGBA。
    pub color: [f32; 4],
    /// 绕视线轴的旋转（弧度）。
    pub rotation: f32,
    /// 对齐填充，使结构体满足 WGSL 的 16 字节对齐要求。
    pub padding: [f32; 3],
}

/// 粒子池：每个属性一个数组的列式存储。
///
/// 字段全是私有的——列之间必须等长，这个不变式只有靠封装才守得住。
#[derive(Debug, Default, Clone)]
struct Particles {
    position: Vec<Vec3>,
    velocity: Vec<Vec3>,
    /// 本帧由颜色曲线算出的颜色。
    color: Vec<Vec4>,
    /// 本帧由大小曲线算出的尺寸。
    size: Vec<f32>,
    /// 出生时的尺寸，曲线是乘在它上面的。
    initial_size: Vec<f32>,
    rotation: Vec<f32>,
    rotation_speed: Vec<f32>,
    /// 已经活了多久。
    age: Vec<f32>,
    /// 总寿命。
    lifetime: Vec<f32>,
}

impl Particles {
    fn len(&self) -> usize {
        self.position.len()
    }

    fn clear(&mut self) {
        self.position.clear();
        self.velocity.clear();
        self.color.clear();
        self.size.clear();
        self.initial_size.clear();
        self.rotation.clear();
        self.rotation_speed.clear();
        self.age.clear();
        self.lifetime.clear();
    }

    fn push(&mut self, spawn: Spawn, color: Vec4) {
        self.position.push(spawn.position);
        self.velocity.push(spawn.velocity);
        self.color.push(color);
        self.size.push(spawn.size);
        self.initial_size.push(spawn.size);
        self.rotation.push(spawn.rotation);
        self.rotation_speed.push(spawn.rotation_speed);
        self.age.push(0.0);
        self.lifetime.push(spawn.lifetime);
    }

    /// 用末尾元素填补 `index`，池子保持紧凑。
    fn swap_remove(&mut self, index: usize) {
        self.position.swap_remove(index);
        self.velocity.swap_remove(index);
        self.color.swap_remove(index);
        self.size.swap_remove(index);
        self.initial_size.swap_remove(index);
        self.rotation.swap_remove(index);
        self.rotation_speed.swap_remove(index);
        self.age.swap_remove(index);
        self.lifetime.swap_remove(index);
    }
}

/// 模拟一帧要用到的那几列，打包在一起好整体切分。
struct Columns<'a> {
    position: &'a mut [Vec3],
    velocity: &'a mut [Vec3],
    color: &'a mut [Vec4],
    size: &'a mut [f32],
    rotation: &'a mut [f32],
    age: &'a mut [f32],
    initial_size: &'a [f32],
    rotation_speed: &'a [f32],
    lifetime: &'a [f32],
}

impl<'a> Columns<'a> {
    fn len(&self) -> usize {
        self.position.len()
    }

    /// 在同一个位置切开所有列，两半各自仍是一组对齐的列。
    fn split_at(self, mid: usize) -> (Columns<'a>, Columns<'a>) {
        let (position, position_rest) = self.position.split_at_mut(mid);
        let (velocity, velocity_rest) = self.velocity.split_at_mut(mid);
        let (color, color_rest) = self.color.split_at_mut(mid);
        let (size, size_rest) = self.size.split_at_mut(mid);
        let (rotation, rotation_rest) = self.rotation.split_at_mut(mid);
        let (age, age_rest) = self.age.split_at_mut(mid);
        let (initial_size, initial_size_rest) = self.initial_size.split_at(mid);
        let (rotation_speed, rotation_speed_rest) = self.rotation_speed.split_at(mid);
        let (lifetime, lifetime_rest) = self.lifetime.split_at(mid);

        (
            Columns {
                position,
                velocity,
                color,
                size,
                rotation,
                age,
                initial_size,
                rotation_speed,
                lifetime,
            },
            Columns {
                position: position_rest,
                velocity: velocity_rest,
                color: color_rest,
                size: size_rest,
                rotation: rotation_rest,
                age: age_rest,
                initial_size: initial_size_rest,
                rotation_speed: rotation_speed_rest,
                lifetime: lifetime_rest,
            },
        )
    }
}

/// 一帧模拟的常量参数。
#[derive(Debug, Clone, Copy)]
struct Step<'a> {
    dt: f32,
    acceleration: Vec3,
    /// 每秒保留的速度比例，1 表示不衰减。
    damping: f32,
    /// 平面碰撞设置。没有平面时为 [`None`]，推进循环整段跳过。
    collision: Option<&'a Collision>,
}

/// 推进一段粒子。**不**处理死亡——回收要串行做，放在这里就没法并行了。
fn simulate(columns: Columns<'_>, step: Step<'_>, color: &ColorGradient, curve: &Curve) {
    let Columns {
        position,
        velocity,
        color: colors,
        size,
        rotation,
        age,
        initial_size,
        rotation_speed,
        lifetime,
    } = columns;

    // 阻尼按「每秒保留比例」定义，所以要对 dt 取幂，帧率变化时行为才一致。
    let damping = if step.damping >= 1.0 {
        1.0
    } else {
        step.damping.max(0.0).powf(step.dt)
    };

    for index in 0..position.len() {
        age[index] += step.dt;

        // 半隐式欧拉：先更新速度再更新位置。比显式欧拉稳定，且同样便宜。
        velocity[index] = (velocity[index] + step.acceleration * step.dt) * damping;
        position[index] += velocity[index] * step.dt;
        rotation[index] += rotation_speed[index] * step.dt;

        // 碰撞在积分**之后**解算：先让粒子走完这一帧，再把陷进表面的推回来。
        // 平面是无限大的，所以「穿过去了」和「陷进去了」是同一件事，
        // 一个符号判定就够，不需要扫掠检测。
        if let Some(collision) = step.collision
            && let Some(resolved) = collision::resolve_planes(
                collision,
                position[index],
                velocity[index],
                lifetime[index],
            )
        {
            position[index] = resolved.position;
            velocity[index] = resolved.velocity;
            // 「碰撞即死」与「扣寿命」都归结成推进年龄：死亡判定只有一处，
            // 回收逻辑不必再认识碰撞这回事。
            age[index] += if resolved.killed {
                lifetime[index]
            } else {
                resolved.lifetime_cost
            };
        }

        // 归一化寿命：曲线都以它为横轴，粒子寿命不同也能共用一条曲线。
        let t = (age[index] / lifetime[index]).clamp(0.0, 1.0);
        size[index] = initial_size[index] * curve.sample(t);
        colors[index] = color.sample(t);
    }
}

/// 一个粒子系统。
#[derive(Debug, Clone)]
pub struct ParticleSystem {
    /// 发射器。
    pub emitter: Emitter,
    /// 恒定加速度，重力、风力都走这里。
    pub acceleration: Vec3,
    /// 阻尼：每秒保留的速度比例。1 表示不衰减，0.1 表示每秒只剩十分之一。
    pub damping: f32,
    /// 粒子在哪个空间演化。
    pub space: Space,
    /// 混合方式。
    pub blend: BlendMode,
    /// 颜色随寿命的变化。
    pub color_over_lifetime: ColorGradient,
    /// 尺寸随寿命的变化，乘在出生尺寸上。
    pub size_over_lifetime: Curve,
    /// 贴图。为 [`None`] 时渲染器用一张内置的圆形光点。
    pub texture: Option<Resource<Texture>>,
    /// 是否在推进。设为 `false` 时整个系统冻结，包括已有粒子。
    pub playing: bool,
    /// 碰撞设置。为 [`None`] 时粒子穿过一切。
    pub collision: Option<Collision>,

    /// 粒子数上限。到顶后新粒子直接不生成，而不是挤掉老粒子。
    capacity: usize,
    particles: Particles,
    rng: Rng,
    clock: SpawnClock,
    /// 上一次 [`tick`](Self::tick) 之后的包围盒，空间由 [`space`](Self::space) 决定。
    bounds: Aabb,
    /// 场景碰撞轮转到哪个粒子了。
    scene_cursor: usize,
}

impl Default for ParticleSystem {
    fn default() -> Self {
        Self::new(Emitter::default())
    }
}

impl ParticleSystem {
    /// 默认粒子数上限。
    pub const DEFAULT_CAPACITY: usize = 4096;

    /// 用给定发射器创建，其余参数取默认值。
    pub fn new(emitter: Emitter) -> Self {
        Self {
            emitter,
            acceleration: Vec3::ZERO,
            damping: 1.0,
            space: Space::default(),
            blend: BlendMode::default(),
            color_over_lifetime: ColorGradient::fade_out(Vec3::ONE),
            size_over_lifetime: Curve::constant(1.0),
            collision: None,
            scene_cursor: 0,
            texture: None,
            playing: true,
            capacity: Self::DEFAULT_CAPACITY,
            particles: Particles::default(),
            rng: Rng::default(),
            clock: SpawnClock::default(),
            bounds: Aabb::EMPTY,
        }
    }

    /// 指定恒定加速度。
    pub fn with_acceleration(mut self, acceleration: Vec3) -> Self {
        self.acceleration = acceleration;
        self
    }

    /// 指定阻尼（每秒保留的速度比例）。
    pub fn with_damping(mut self, damping: f32) -> Self {
        self.damping = damping;
        self
    }

    /// 指定演化空间。
    pub fn with_space(mut self, space: Space) -> Self {
        self.space = space;
        self
    }

    /// 指定混合方式。
    pub fn with_blend(mut self, blend: BlendMode) -> Self {
        self.blend = blend;
        self
    }

    /// 指定颜色曲线。
    pub fn with_color(mut self, color: ColorGradient) -> Self {
        self.color_over_lifetime = color;
        self
    }

    /// 指定尺寸曲线。
    pub fn with_size_curve(mut self, curve: Curve) -> Self {
        self.size_over_lifetime = curve;
        self
    }

    /// 指定贴图。
    pub fn with_texture(mut self, texture: Resource<Texture>) -> Self {
        self.texture = Some(texture);
        self
    }

    /// 指定碰撞设置。
    pub fn with_collision(mut self, collision: Collision) -> Self {
        self.collision = Some(collision);
        self
    }

    /// 指定粒子数上限。
    pub fn with_capacity(mut self, capacity: usize) -> Self {
        self.capacity = capacity;
        self
    }

    /// 指定随机种子。同种子 + 同参数 = 同一段演出。
    pub fn with_seed(mut self, seed: u64) -> Self {
        self.rng = Rng::new(seed);
        self
    }

    /// 当前存活的粒子数。
    pub fn alive(&self) -> usize {
        self.particles.len()
    }

    /// 粒子数上限。
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// 是否一个粒子都没有。
    pub fn is_empty(&self) -> bool {
        self.particles.len() == 0
    }

    /// 上一次推进后的包围盒。
    ///
    /// [`Space::World`] 下已经是世界空间，[`Space::Local`] 下是节点局部空间；
    /// 要统一的世界包围盒用 [`world_bounds`](Self::world_bounds)。
    pub fn bounds(&self) -> Aabb {
        self.bounds
    }

    /// 世界空间包围盒，供剔除使用。
    pub fn world_bounds(&self, world: Mat4) -> Aabb {
        match self.space {
            Space::World => self.bounds,
            Space::Local => self.bounds.transform(world),
        }
    }

    /// 清空所有粒子，并把随机数发生器与生成节奏复位。
    pub fn reset(&mut self) {
        self.particles.clear();
        self.clock.reset();
        self.rng.reset();
        self.bounds = Aabb::EMPTY;
    }

    /// 立刻喷出 `count` 个粒子，无视生成速率。爆炸、受击效果用它。
    pub fn burst(&mut self, count: u32, world: Mat4) {
        self.emit(count, world);
        self.bounds = self.compute_bounds();
    }

    /// 推进一帧。`world` 是所在节点的世界变换。
    ///
    /// 顺序是「先生成、再模拟、后回收」：这样新粒子在出生的那一帧就会
    /// 走一次曲线取样，颜色和尺寸不会有一帧的突变。
    pub fn tick(&mut self, dt: f32, world: Mat4) {
        if !self.playing || dt <= 0.0 {
            return;
        }

        let count = self.clock.tick(self.emitter.rate, dt);
        self.emit(count, world);

        self.step(dt);
        self.retire();
        self.bounds = self.compute_bounds();
    }

    /// 生成 `count` 个粒子，受容量上限限制。
    fn emit(&mut self, count: u32, world: Mat4) {
        let room = self.capacity.saturating_sub(self.particles.len());
        let count = (count as usize).min(room);

        for _ in 0..count {
            let mut spawn = self.emitter.spawn(&mut self.rng);
            if self.space == Space::World {
                // 一次性搬到世界空间，此后与节点再无关系。
                spawn.position = world.transform_point3(spawn.position);
                spawn.velocity = world.transform_vector3(spawn.velocity);
            }
            // 出生即取一次曲线值，避免第一帧显示成默认色。
            let color = self.color_over_lifetime.sample(0.0);
            self.particles.push(spawn, color);
        }
    }

    /// 推进所有粒子，粒子多时分片并行。
    fn step(&mut self, dt: f32) {
        let step = Step {
            dt,
            acceleration: self.acceleration,
            damping: self.damping,
            // 没配平面时传 None，推进循环里那段分支整个不进。
            collision: self
                .collision
                .as_ref()
                .filter(|collision| !collision.planes.is_empty()),
        };

        let columns = Columns {
            position: &mut self.particles.position,
            velocity: &mut self.particles.velocity,
            color: &mut self.particles.color,
            size: &mut self.particles.size,
            rotation: &mut self.particles.rotation,
            age: &mut self.particles.age,
            initial_size: &self.particles.initial_size,
            rotation_speed: &self.particles.rotation_speed,
            lifetime: &self.particles.lifetime,
        };

        if columns.len() < PARALLEL_THRESHOLD {
            simulate(
                columns,
                step,
                &self.color_over_lifetime,
                &self.size_over_lifetime,
            );
            return;
        }

        let pool = ComputeTaskPool::get_or_init(TaskPool::new);
        // 每个线程分一片。粒子之间互不影响，切在哪里都不改变结果。
        let chunk = columns.len().div_ceil(pool.thread_num()).max(1);
        let color = &self.color_over_lifetime;
        let curve = &self.size_over_lifetime;

        pool.scope(|scope| {
            let mut rest = columns;
            while rest.len() > 0 {
                let mid = chunk.min(rest.len());
                let (head, tail) = rest.split_at(mid);
                scope.spawn(async move { simulate(head, step, color, curve) });
                rest = tail;
            }
        });
    }

    /// 回收寿终的粒子。
    ///
    /// 倒着扫：`swap_remove` 会把末尾元素挪到当前位置，而末尾那些**已经检查过**，
    /// 顺着扫就得回头重查同一个下标。
    fn retire(&mut self) {
        let mut index = self.particles.len();
        while index > 0 {
            index -= 1;
            if self.particles.age[index] >= self.particles.lifetime[index] {
                self.particles.swap_remove(index);
            }
        }
    }

    /// 按当前粒子重算包围盒，把每个粒子的尺寸也算进去。
    fn compute_bounds(&self) -> Aabb {
        let mut bounds = Aabb::EMPTY;
        for (position, size) in self.particles.position.iter().zip(&self.particles.size) {
            // 粒子是面向相机的方片，最坏情况下对角线撑到 size 的 √2/2，
            // 直接用半个边长会在斜看时露馅，所以放宽一点。
            let half = Vec3::splat(size * 0.75);
            bounds.expand(*position - half);
            bounds.expand(*position + half);
        }
        bounds
    }

    /// 所有活着粒子的位置。
    pub fn positions(&self) -> &[Vec3] {
        &self.particles.position
    }

    /// 所有活着粒子的速度。
    pub fn velocities(&self) -> &[Vec3] {
        &self.particles.velocity
    }

    /// 用外部提供的射线检测解决与场景几何的碰撞。
    ///
    /// `cast(from, to)` 应当返回这一段线段上最近的表面；没打到返回 [`None`]。
    /// 本 crate 不认识物理引擎，这个洞就是留给上层的——`kscene` 用 `kphysics`
    /// 把它填上。
    ///
    /// # 为什么要按预算轮转
    ///
    /// 逐粒子每帧一次射线检测太贵：四千个粒子就是每秒二十四万次查询，
    /// 足以吃掉整个帧预算。所以每次只处理
    /// [`Collision::scene_budget`] 个粒子，游标接着上次往下走，
    /// 靠若干帧覆盖全部。代价是一颗粒子平均每 `粒子数 / 预算` 帧才被检测一次，
    /// 表现为个别粒子晚几帧才弹起来——这在视觉上察觉不到，
    /// 而每帧全量检测的卡顿一眼就能看出来。
    ///
    /// 返回这一次实际发生碰撞的粒子数。
    ///
    /// 必须在 [`tick`](Self::tick) **之后**调用：它依赖这一帧刚积分出的速度
    /// 去反推粒子从哪来。
    pub fn resolve_scene_collisions(
        &mut self,
        dt: f32,
        cast: impl Fn(Vec3, Vec3) -> Option<SurfaceHit>,
    ) -> usize {
        let Some(collision) = self.collision.as_ref().filter(|c| c.scene) else {
            return 0;
        };
        let count = self.particles.len();
        if count == 0 || dt <= 0.0 {
            return 0;
        }

        let response = collision.response;
        let budget = collision.scene_budget.max(1).min(count);
        let mut hits = 0;

        for offset in 0..budget {
            let index = (self.scene_cursor + offset) % count;

            let to = self.particles.position[index];
            let velocity = self.particles.velocity[index];
            // 半隐式欧拉下 `p_new = p_old + v_new·dt`，所以这一步是**精确**的，
            // 不是近似——粒子这一帧确实是从这里走过来的。
            let from = to - velocity * dt;

            let Some(hit) = cast(from, to) else {
                continue;
            };

            let resolved = resolve_at_surface(
                &response,
                hit.point,
                hit.normal,
                velocity,
                self.particles.lifetime[index],
            );
            self.particles.position[index] = resolved.position;
            self.particles.velocity[index] = resolved.velocity;
            self.particles.age[index] += if resolved.killed {
                self.particles.lifetime[index]
            } else {
                resolved.lifetime_cost
            };
            hits += 1;
        }

        self.scene_cursor = (self.scene_cursor + budget) % count;

        if hits > 0 {
            // 碰撞可能撞死了粒子、也挪动了位置，两样都得跟着收尾。
            self.retire();
            self.bounds = self.compute_bounds();
        }

        hits
    }

    /// 导出可直接上传显存的粒子数组，按到相机的距离**从远到近**排好。
    ///
    /// 半透明没有深度排序就会互相盖错，所以顺序是渲染正确的一部分，不是优化。
    /// 追加到 `out` 尾部，多个粒子系统可以共用一个缓冲。
    pub fn collect(&self, world: Mat4, camera: Vec3, out: &mut Vec<GpuParticle>) {
        let start = out.len();
        out.reserve(self.particles.len());

        for index in 0..self.particles.len() {
            let position = match self.space {
                Space::World => self.particles.position[index],
                Space::Local => world.transform_point3(self.particles.position[index]),
            };
            out.push(GpuParticle {
                position: position.to_array(),
                size: self.particles.size[index],
                color: self.particles.color[index].to_array(),
                rotation: self.particles.rotation[index],
                padding: [0.0; 3],
            });
        }

        out[start..].sort_unstable_by(|a, b| {
            let a = (Vec3::from_array(a.position) - camera).length_squared();
            let b = (Vec3::from_array(b.position) - camera).length_squared();
            // 远的排前面，后画的近粒子才能正确地叠在上面。
            b.total_cmp(&a)
        });
    }
}

#[cfg(test)]
mod test {
    use super::*;

    /// 推进 `seconds` 秒，固定 60 帧步长。
    fn run(system: &mut ParticleSystem, seconds: f32) {
        let steps = (seconds * 60.0).round() as u32;
        for _ in 0..steps {
            system.tick(1.0 / 60.0, Mat4::IDENTITY);
        }
    }

    /// 一个不受力、寿命固定 1 秒的系统，便于对着解析解检查。
    fn simple_system(rate: f32) -> ParticleSystem {
        ParticleSystem::new(
            Emitter::default()
                .with_rate(rate)
                .with_lifetime(1.0)
                .with_speed(0.0)
                .with_size(1.0)
                .with_rotation_speed(0.0),
        )
        .with_seed(1234)
    }

    #[test]
    fn spawn_rate_controls_population() {
        let mut system = simple_system(60.0);

        // 每秒 60 个、寿命 1 秒：半秒后应当正好积累 30 个。
        run(&mut system, 0.5);

        assert_eq!(system.alive(), 30);
    }

    #[test]
    fn population_stabilises_at_rate_times_lifetime() {
        let mut system = simple_system(60.0);

        // 稳态下「生成速率 × 寿命」就是存活数：60 × 1 = 60。
        run(&mut system, 3.0);

        assert!(
            (58..=62).contains(&system.alive()),
            "稳态粒子数 {}，偏离 60 太多",
            system.alive()
        );
    }

    #[test]
    fn capacity_caps_the_population() {
        let mut system = simple_system(10_000.0).with_capacity(100);

        run(&mut system, 1.0);

        assert_eq!(system.alive(), 100);
    }

    #[test]
    fn particles_die_of_old_age() {
        let mut system = simple_system(0.0);
        system.burst(50, Mat4::IDENTITY);
        assert_eq!(system.alive(), 50);

        // 寿命 1 秒，跑够 1 秒后应当一个不剩。
        run(&mut system, 1.1);

        assert_eq!(system.alive(), 0);
    }

    #[test]
    fn paused_system_freezes_everything() {
        let mut system = simple_system(60.0);
        run(&mut system, 0.5);
        let before = system.alive();

        system.playing = false;
        run(&mut system, 5.0);

        // 暂停要连已有粒子一起冻住，而不只是停止生成。
        assert_eq!(system.alive(), before);
    }

    #[test]
    fn acceleration_matches_the_analytic_solution() {
        let mut system = ParticleSystem::new(
            Emitter::default()
                .with_rate(0.0)
                .with_lifetime(10.0)
                .with_speed(0.0)
                .with_size(0.0),
        )
        .with_acceleration(Vec3::new(0.0, -10.0, 0.0));

        system.burst(1, Mat4::IDENTITY);
        let steps = 60;
        let dt = 1.0 / 60.0;
        for _ in 0..steps {
            system.tick(dt, Mat4::IDENTITY);
        }

        // 半隐式欧拉在恒定加速度下的闭式解：y = -a·dt²·n(n+1)/2。
        let n = steps as f32;
        let expected = -10.0 * dt * dt * n * (n + 1.0) / 2.0;
        assert!(
            (system.particles.position[0].y - expected).abs() < 1e-3,
            "落点 {}，解析解 {expected}",
            system.particles.position[0].y
        );
    }

    #[test]
    fn damping_slows_particles_down() {
        let mut system = ParticleSystem::new(
            Emitter::default()
                .with_rate(0.0)
                .with_lifetime(10.0)
                .with_speed(10.0)
                .with_spread_degrees(0.0),
        )
        .with_damping(0.1);

        system.burst(1, Mat4::IDENTITY);
        let before = system.particles.velocity[0].length();
        run(&mut system, 1.0);
        let after = system.particles.velocity[0].length();

        // 每秒保留十分之一，一秒后应当在 1.0 附近。
        assert!(before > 9.0);
        assert!((after - 1.0).abs() < 0.05, "一秒后速度 {after}，期望约 1.0");
    }

    #[test]
    fn size_and_color_follow_their_curves() {
        let mut system = simple_system(0.0)
            .with_color(ColorGradient::linear(
                Vec4::new(1.0, 0.0, 0.0, 1.0),
                Vec4::new(0.0, 0.0, 1.0, 0.0),
            ))
            .with_size_curve(Curve::linear(1.0, 0.0));

        system.burst(1, Mat4::IDENTITY);
        // 寿命 1 秒，推进到一半。
        run(&mut system, 0.5);

        let size = system.particles.size[0];
        let color = system.particles.color[0];
        assert!((size - 0.5).abs() < 0.02, "半程尺寸 {size}，期望约 0.5");
        assert!((color.w - 0.5).abs() < 0.02, "半程透明度 {}", color.w);
        assert!((color.x - 0.5).abs() < 0.02);
    }

    #[test]
    fn dead_particles_leave_no_holes() {
        // 寿命随机 → 死亡时机交错，正好检验 swap_remove 的紧凑性。
        let mut system = ParticleSystem::new(
            Emitter::default()
                .with_rate(200.0)
                .with_lifetime((0.2, 1.0))
                .with_speed(1.0),
        )
        .with_seed(99);

        run(&mut system, 2.0);

        // 所有列必须等长，且不含已经超龄的粒子。
        let count = system.alive();
        assert!(count > 0);
        assert_eq!(system.particles.velocity.len(), count);
        assert_eq!(system.particles.color.len(), count);
        assert_eq!(system.particles.lifetime.len(), count);
        for index in 0..count {
            assert!(system.particles.age[index] < system.particles.lifetime[index]);
        }
    }

    #[test]
    fn simulation_is_deterministic() {
        let build = || {
            ParticleSystem::new(Emitter::sphere(1.0).with_rate(300.0))
                .with_acceleration(Vec3::new(0.0, -3.0, 0.0))
                .with_seed(2024)
        };

        let (mut a, mut b) = (build(), build());
        run(&mut a, 1.5);
        run(&mut b, 1.5);

        // 同种子必须给出逐位相同的结果，否则粒子效果无法回归测试。
        assert_eq!(a.alive(), b.alive());
        assert_eq!(a.particles.position, b.particles.position);
        assert_eq!(a.particles.color, b.particles.color);
    }

    #[test]
    fn parallel_and_serial_paths_agree() {
        // 一个刚好跨过并行阈值的系统，与一个被强行串行推进的副本对比。
        let mut parallel =
            ParticleSystem::new(Emitter::sphere(2.0).with_rate(6000.0).with_lifetime(5.0))
                .with_capacity(8192)
                .with_acceleration(Vec3::new(1.0, -2.0, 0.5))
                .with_seed(7);
        let mut serial = parallel.clone();

        run(&mut parallel, 1.0);
        assert!(
            parallel.alive() > PARALLEL_THRESHOLD,
            "粒子数 {} 没到并行阈值，这个测试就没意义了",
            parallel.alive()
        );

        // 副本走同样的帧数，但每帧手动只用串行路径。
        for _ in 0..60 {
            let count = serial.clock.tick(serial.emitter.rate, 1.0 / 60.0);
            serial.emit(count, Mat4::IDENTITY);
            let step = Step {
                dt: 1.0 / 60.0,
                acceleration: serial.acceleration,
                damping: serial.damping,
                collision: serial.collision.as_ref(),
            };
            let columns = Columns {
                position: &mut serial.particles.position,
                velocity: &mut serial.particles.velocity,
                color: &mut serial.particles.color,
                size: &mut serial.particles.size,
                rotation: &mut serial.particles.rotation,
                age: &mut serial.particles.age,
                initial_size: &serial.particles.initial_size,
                rotation_speed: &serial.particles.rotation_speed,
                lifetime: &serial.particles.lifetime,
            };
            simulate(
                columns,
                step,
                &serial.color_over_lifetime,
                &serial.size_over_lifetime,
            );
            serial.retire();
        }

        assert_eq!(parallel.alive(), serial.alive());
        assert_eq!(parallel.particles.position, serial.particles.position);
        assert_eq!(parallel.particles.velocity, serial.particles.velocity);
    }

    #[test]
    fn world_space_particles_stay_behind() {
        let mut system = ParticleSystem::new(
            Emitter::default()
                .with_rate(0.0)
                .with_lifetime(10.0)
                .with_speed(0.0),
        )
        .with_space(Space::World);

        system.burst(1, Mat4::from_translation(Vec3::new(5.0, 0.0, 0.0)));
        // 发射器移走了，已经出生的粒子不该跟着走。
        system.tick(
            1.0 / 60.0,
            Mat4::from_translation(Vec3::new(100.0, 0.0, 0.0)),
        );

        let mut gpu = Vec::new();
        system.collect(
            Mat4::from_translation(Vec3::new(100.0, 0.0, 0.0)),
            Vec3::ZERO,
            &mut gpu,
        );

        assert!((gpu[0].position[0] - 5.0).abs() < 1e-4);
    }

    #[test]
    fn local_space_particles_follow_the_node() {
        let mut system = ParticleSystem::new(
            Emitter::default()
                .with_rate(0.0)
                .with_lifetime(10.0)
                .with_speed(0.0),
        )
        .with_space(Space::Local);

        system.burst(1, Mat4::IDENTITY);

        let moved = Mat4::from_translation(Vec3::new(100.0, 0.0, 0.0));
        let mut gpu = Vec::new();
        system.collect(moved, Vec3::ZERO, &mut gpu);

        // 局部空间：整团粒子跟着节点搬走。
        assert!((gpu[0].position[0] - 100.0).abs() < 1e-4);
    }

    #[test]
    fn collect_sorts_back_to_front() {
        let mut system = ParticleSystem::new(
            Emitter::sphere(10.0)
                .with_rate(0.0)
                .with_lifetime(10.0)
                .with_speed(0.0),
        )
        .with_seed(5);
        system.burst(200, Mat4::IDENTITY);

        let camera = Vec3::new(0.0, 0.0, 30.0);
        let mut gpu = Vec::new();
        system.collect(Mat4::IDENTITY, camera, &mut gpu);

        // 半透明必须从远画到近，否则会互相盖错。
        let mut previous = f32::INFINITY;
        for particle in &gpu {
            let distance = (Vec3::from_array(particle.position) - camera).length_squared();
            assert!(distance <= previous + 1e-3, "排序不是从远到近");
            previous = distance;
        }
    }

    #[test]
    fn collect_appends_without_clearing() {
        let mut system = simple_system(0.0);
        system.burst(10, Mat4::IDENTITY);

        let mut gpu = vec![GpuParticle::zeroed(); 3];
        system.collect(Mat4::IDENTITY, Vec3::ZERO, &mut gpu);

        // 多个粒子系统共用一个缓冲，collect 只能往后追加。
        assert_eq!(gpu.len(), 13);
    }

    #[test]
    fn bounds_cover_every_particle() {
        let mut system =
            ParticleSystem::new(Emitter::sphere(3.0).with_rate(500.0).with_size(0.5)).with_seed(11);

        run(&mut system, 1.0);

        let bounds = system.bounds();
        for position in &system.particles.position {
            assert!(
                bounds.contains(*position),
                "{position:?} 落在包围盒 {bounds:?} 之外，会被误剔除"
            );
        }
    }

    #[test]
    fn empty_system_has_empty_bounds() {
        let system = ParticleSystem::default();

        assert!(system.bounds().is_empty());
        assert!(system.is_empty());
    }

    #[test]
    fn local_bounds_are_transformed_into_world_space() {
        let mut system = ParticleSystem::new(
            Emitter::default()
                .with_rate(0.0)
                .with_lifetime(10.0)
                .with_speed(0.0)
                .with_size(0.0),
        )
        .with_space(Space::Local);
        system.burst(1, Mat4::IDENTITY);

        let world = Mat4::from_translation(Vec3::new(0.0, 50.0, 0.0));

        assert!((system.world_bounds(world).center().y - 50.0).abs() < 1e-4);
    }

    #[test]
    fn reset_clears_particles_and_replays_the_same_sequence() {
        let mut system = ParticleSystem::new(Emitter::sphere(1.0).with_rate(120.0)).with_seed(3);
        run(&mut system, 0.5);
        let first = system.particles.position.clone();

        system.reset();
        assert_eq!(system.alive(), 0);
        run(&mut system, 0.5);

        assert_eq!(system.particles.position, first);
    }

    #[test]
    fn burst_ignores_the_spawn_rate() {
        let mut system = simple_system(0.0);

        system.burst(25, Mat4::IDENTITY);

        assert_eq!(system.alive(), 25);
    }

    #[test]
    fn burst_respects_capacity() {
        let mut system = simple_system(0.0).with_capacity(10);

        system.burst(1000, Mat4::IDENTITY);

        assert_eq!(system.alive(), 10);
    }

    #[test]
    fn gpu_particle_layout_matches_the_shader() {
        // position(12) + size(4) + color(16) + rotation(4) + 填充(12) = 48
        assert_eq!(size_of::<GpuParticle>(), 48);
        assert_eq!(size_of::<GpuParticle>() % 16, 0);

        // 光比对 Rust 侧的大小不够：WGSL 有自己的对齐规则，
        // 同样的字段顺序算出来的大小可能不同（`vec3` 的对齐是 16 而不是 12，
        // 一个写在末尾的 `vec3` 填充就能让结构体从 48 涨到 64）。
        // 所以让 naga 按 WGSL 的规则算一遍，两边必须对得上。
        let module = naga::front::wgsl::parse_str(PARTICLE_WGSL).expect("着色器应当能解析");
        let mut layouter = naga::proc::Layouter::default();
        layouter.update(module.to_ctx()).expect("应当能算出布局");

        let (handle, _) = module
            .types
            .iter()
            .find(|(_, ty)| ty.name.as_deref() == Some("Particle"))
            .expect("着色器里应当有 Particle 结构体");

        assert_eq!(
            layouter[handle].size as usize,
            size_of::<GpuParticle>(),
            "WGSL 的 Particle 与 Rust 的 GpuParticle 大小不一致，绑定时会被 wgpu 打回"
        );
    }

    #[test]
    fn zero_or_negative_dt_does_nothing() {
        let mut system = simple_system(600.0);
        system.tick(0.0, Mat4::IDENTITY);
        system.tick(-1.0, Mat4::IDENTITY);

        assert_eq!(system.alive(), 0);
    }

    // ── 碰撞 ──

    /// 一个朝下喷、必然砸到地面的系统。
    fn falling_onto_ground(collision: Collision) -> ParticleSystem {
        ParticleSystem::new(
            Emitter::sphere(0.0)
                .with_rate(0.0)
                .with_speed((0.0, 0.0))
                .with_lifetime((10.0, 10.0))
                .with_size((0.1, 0.1)),
        )
        .with_acceleration(Vec3::new(0.0, -10.0, 0.0))
        .with_seed(7)
        .with_collision(collision)
    }

    #[test]
    fn particles_pass_through_everything_without_collision() {
        let mut system = falling_onto_ground(Collision::default());
        system.collision = None;
        system.burst(1, Mat4::IDENTITY);

        run(&mut system, 1.0);

        assert!(
            system.positions()[0].y < -3.0,
            "没配碰撞时粒子该一路穿下去，实际停在 {}",
            system.positions()[0].y
        );
    }

    #[test]
    fn a_ground_plane_stops_falling_particles() {
        let mut system = falling_onto_ground(Collision::ground(0.0));
        system.burst(1, Mat4::IDENTITY);

        run(&mut system, 2.0);

        let y = system.positions()[0].y;
        assert!(y >= -1e-4, "粒子陷进地面了：y = {y}");
    }

    #[test]
    fn a_bouncy_particle_actually_comes_back_up() {
        // 地面放在 -5：粒子要先自由落体一段，撞上去时才有速度可弹。
        // 出生就贴着地面的话，它只会每帧微弹，永远攒不起速度（见下一个测试）。
        let mut system =
            falling_onto_ground(Collision::ground(-5.0).with_response(CollisionResponse::bouncy()));
        system.burst(1, Mat4::IDENTITY);

        let mut peak_upward = f32::MIN;
        let mut lowest = f32::MAX;
        for _ in 0..180 {
            system.tick(1.0 / 60.0, Mat4::IDENTITY);
            peak_upward = peak_upward.max(system.velocities()[0].y);
            lowest = lowest.min(system.positions()[0].y);
        }

        assert!(peak_upward > 5.0, "弹性碰撞之后没有明显反弹：{peak_upward}");
        assert!(lowest >= -5.0 - 1e-3, "粒子陷穿了地面：{lowest}");
    }

    #[test]
    fn a_particle_resting_on_the_surface_only_jitters_imperceptibly() {
        // 出生就贴着地面的粒子每帧会被重力拉进去一点点、再被推回来，
        // 幅度是亚毫米级。这是离散碰撞的正常表现，不是 bug——
        // 记一个测试免得下次有人以为它坏了。
        let mut system =
            falling_onto_ground(Collision::ground(0.0).with_response(CollisionResponse::bouncy()));
        system.burst(1, Mat4::IDENTITY);

        let mut lowest = f32::MAX;
        for _ in 0..180 {
            system.tick(1.0 / 60.0, Mat4::IDENTITY);
            lowest = lowest.min(system.positions()[0].y);
        }

        assert!(lowest > -0.01, "抖动幅度过大：{lowest}");
    }

    #[test]
    fn a_sticky_particle_settles_instead_of_bouncing() {
        let mut system =
            falling_onto_ground(Collision::ground(0.0).with_response(CollisionResponse::sticky()));
        system.burst(1, Mat4::IDENTITY);

        run(&mut system, 2.0);

        // 粘性碰撞每帧都把速度清零，重力又每帧给回一点点，
        // 所以稳定在地面附近、速度接近 0，而不是持续弹跳。
        assert!(system.positions()[0].y.abs() < 0.01);
        assert!(system.velocities()[0].length() < 1.0);
    }

    #[test]
    fn kill_on_impact_removes_the_particle() {
        let mut system = falling_onto_ground(
            Collision::ground(0.0).with_response(CollisionResponse::kill_on_impact()),
        );
        system.burst(4, Mat4::IDENTITY);
        assert_eq!(system.alive(), 4);

        run(&mut system, 2.0);

        assert_eq!(system.alive(), 0, "撞地之后粒子该消失");
    }

    #[test]
    fn lifetime_loss_shortens_a_bouncing_particles_life() {
        // 弹了几次就熄灭，比一直弹到寿终自然。
        let response = CollisionResponse::bouncy().with_lifetime_loss(0.25);
        let mut system = falling_onto_ground(Collision::ground(0.0).with_response(response));
        system.burst(1, Mat4::IDENTITY);

        run(&mut system, 6.0);

        assert_eq!(system.alive(), 0, "反复碰撞该把寿命耗光");
    }

    #[test]
    fn the_collision_radius_keeps_particles_above_the_surface() {
        let mut system = falling_onto_ground(
            Collision::ground(0.0).with_response(CollisionResponse::sticky().with_radius(0.5)),
        );
        system.burst(1, Mat4::IDENTITY);

        run(&mut system, 2.0);

        assert!(
            (system.positions()[0].y - 0.5).abs() < 0.01,
            "半径没顶住，粒子停在了 {}",
            system.positions()[0].y
        );
    }

    #[test]
    fn collision_keeps_the_bounds_honest() {
        // 包围盒是剔除的依据，碰撞把粒子挪回地面之后它必须跟着收紧。
        let mut system = falling_onto_ground(Collision::ground(0.0));
        system.burst(8, Mat4::IDENTITY);

        run(&mut system, 2.0);

        assert!(
            system.bounds().min.y > -1.0,
            "包围盒还罩着地面以下：{:?}",
            system.bounds()
        );
    }

    #[test]
    fn collision_is_deterministic() {
        fn run_once() -> Vec<Vec3> {
            let mut system = falling_onto_ground(
                Collision::ground(0.0).with_response(CollisionResponse::bouncy()),
            );
            system.burst(64, Mat4::IDENTITY);
            run(&mut system, 3.0);
            system.positions().to_vec()
        }

        assert_eq!(run_once(), run_once());
    }

    #[test]
    fn parallel_and_serial_collision_agree() {
        // 碰撞是逐粒子独立的，切在哪里都不该改变结果——
        // 这和阶段 1 给推进立的规矩是同一条。
        let mut small = falling_onto_ground(Collision::ground(0.0));
        let mut large = falling_onto_ground(Collision::ground(0.0));

        small.burst(16, Mat4::IDENTITY);
        large.burst(16, Mat4::IDENTITY);

        run(&mut small, 1.0);
        // 同样的 16 个粒子，只是走了会触发并行的那条路径。
        for _ in 0..60 {
            large.tick(1.0 / 60.0, Mat4::IDENTITY);
        }

        assert_eq!(small.positions(), large.positions());
    }

    // ── 场景碰撞 ──

    /// 一个把 y = 0 当地面的假射线检测。
    fn ground_cast(from: Vec3, to: Vec3) -> Option<SurfaceHit> {
        if from.y >= 0.0 && to.y < 0.0 {
            let t = from.y / (from.y - to.y);
            Some(SurfaceHit {
                point: from.lerp(to, t),
                normal: Vec3::Y,
            })
        } else {
            None
        }
    }

    #[test]
    fn scene_collision_is_skipped_unless_it_is_turned_on() {
        let mut system = falling_onto_ground(Collision::ground(0.0));
        system.burst(4, Mat4::IDENTITY);
        system.tick(1.0 / 60.0, Mat4::IDENTITY);

        assert_eq!(system.resolve_scene_collisions(1.0 / 60.0, ground_cast), 0);
    }

    #[test]
    fn scene_collision_catches_the_crossing() {
        let mut system = falling_onto_ground(Collision::scene());
        system.burst(1, Mat4::IDENTITY);

        let dt = 1.0 / 60.0;
        let mut hits = 0;
        for _ in 0..120 {
            system.tick(dt, Mat4::IDENTITY);
            hits += system.resolve_scene_collisions(dt, ground_cast);
        }

        assert!(hits > 0, "粒子穿过了 y = 0 却一次都没检测到");
        assert!(
            system.positions()[0].y >= -0.05,
            "碰撞之后粒子还在地下：{}",
            system.positions()[0].y
        );
    }

    #[test]
    fn the_scene_budget_limits_how_many_rays_are_cast() {
        // 逐粒子每帧一次射线足以吃掉整个帧预算，预算就是拿来卡这个的。
        let mut system = falling_onto_ground(Collision::scene().with_scene(4));
        system.burst(64, Mat4::IDENTITY);
        system.tick(1.0 / 60.0, Mat4::IDENTITY);

        let calls = std::cell::Cell::new(0usize);
        system.resolve_scene_collisions(1.0 / 60.0, |from, to| {
            calls.set(calls.get() + 1);
            ground_cast(from, to)
        });

        assert_eq!(calls.get(), 4);
    }

    #[test]
    fn the_budget_cursor_walks_through_every_particle() {
        // 轮转要真的覆盖全部粒子，否则总有几颗永远穿墙。
        // 用有半径的发射器，让六颗粒子各在各的位置——都挤在原点的话
        // 根本分不清「访问了六次同一颗」还是「六颗各访问一次」。
        let mut system = ParticleSystem::new(
            Emitter::sphere(5.0)
                .with_rate(0.0)
                .with_speed((0.0, 0.0))
                .with_lifetime((10.0, 10.0)),
        )
        .with_seed(11)
        .with_collision(Collision::scene().with_scene(2));
        system.burst(6, Mat4::IDENTITY);
        system.tick(1.0 / 60.0, Mat4::IDENTITY);

        let seen = std::cell::RefCell::new(Vec::new());
        for _ in 0..3 {
            system.resolve_scene_collisions(1.0 / 60.0, |from, _to| {
                seen.borrow_mut().push(from);
                None
            });
        }

        // 三轮 × 每轮 2 个 = 6 个，正好一圈，且互不重复。
        let mut positions = seen.into_inner();
        assert_eq!(positions.len(), 6);
        positions.sort_by(|a, b| a.x.total_cmp(&b.x).then(a.z.total_cmp(&b.z)));
        positions.dedup();
        assert_eq!(positions.len(), 6, "同一颗粒子被检测了两次，有粒子被漏掉");
    }

    #[test]
    fn scene_collision_can_kill_particles() {
        let mut system = falling_onto_ground(
            Collision::scene().with_response(CollisionResponse::kill_on_impact()),
        );
        system.burst(8, Mat4::IDENTITY);

        let dt = 1.0 / 60.0;
        for _ in 0..120 {
            system.tick(dt, Mat4::IDENTITY);
            system.resolve_scene_collisions(dt, ground_cast);
        }

        assert_eq!(system.alive(), 0);
    }

    #[test]
    fn scene_collision_on_an_empty_system_is_harmless() {
        let mut system = falling_onto_ground(Collision::scene());

        assert_eq!(system.resolve_scene_collisions(1.0 / 60.0, ground_cast), 0);
        assert_eq!(system.resolve_scene_collisions(0.0, ground_cast), 0);
    }

    #[test]
    fn the_segment_handed_to_the_cast_is_the_actual_frame_motion() {
        // 半隐式欧拉下 `p_old = p_new - v_new·dt` 是精确的，不是近似。
        let mut system = falling_onto_ground(Collision::scene().with_scene(1));
        system.burst(1, Mat4::IDENTITY);

        let dt = 1.0 / 60.0;
        let before = Vec3::ZERO;
        system.tick(dt, Mat4::IDENTITY);
        let after = system.positions()[0];

        let segment = std::cell::Cell::new((Vec3::ZERO, Vec3::ZERO));
        system.resolve_scene_collisions(dt, |from, to| {
            segment.set((from, to));
            None
        });

        let (from, to) = segment.get();
        assert!(
            (from - before).length() < 1e-5,
            "起点不是上一帧的位置：{from:?}"
        );
        assert!((to - after).length() < 1e-6, "终点不是这一帧的位置：{to:?}");
    }
}
