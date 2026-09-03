//! 软体：布料和可压的闭合体积。
//!
//! # 为什么不是弹簧
//!
//! 最直觉的写法是「质点 + 弹簧 + 显式积分」。它的问题不在于不对，
//! 而在于**刚度和步长挂钩**：布要想不像橡皮筋一样抻长，弹簧就得很硬；
//! 弹簧一硬，显式积分就要极小的步长才不炸。想在 60 Hz 下模拟一块
//! 不怎么抻的布，光靠调弹簧系数是调不出来的——不是软得像橡皮，
//! 就是直接飞了。
//!
//! 这里用的是**基于位置的动力学**（PBD）：不算力，直接把粒子挪到满足
//! 约束的地方，再由「挪了多少」反推速度。它无条件稳定，刚度变成了
//! 「解算几次」——迭代越多越硬，而且**再多也不会炸**。
//!
//! 代价是刚度和帧率有关：同样的迭代次数，步长变了硬度会变。所以这里
//! 走**子步**（substep）而不是靠加迭代次数——子步在数学上比多迭代更接近
//! 真解，这一点是 XPBD 那篇论文的主要结论之一。
//!
//! # 和刚体世界是单向耦合
//!
//! 布会被刚体推开，但**不会反过来推刚体**。
//!
//! 双向耦合要把粒子受到的冲量加回刚体上，而一块布有几千个粒子、
//! 每个质量都比箱子小两三个数量级——质量比一悬殊，加回去的冲量要么
//! 没有可见效果，要么在几帧里把箱子弹飞。真要做得可靠得给软体一个
//! 整体质量再分配，那是另一套东西。
//!
//! 所以：布落在箱子上会披下来，但推不动箱子。这条限制写在这儿而不是
//! 让人自己去发现。
//!
//! # 顶点要焊接
//!
//! 从网格造软体时，**位置相同的顶点必须合成一个粒子**。球和立方体这类
//! 图元在接缝处（UV 缝、硬边）有重复顶点，不焊的话那些顶点各走各的，
//! 球会从缝上裂开——而且不报任何错，看起来像「模型坏了」。

use crate::{InteractionGroups, PhysicsWorld};
use kmath::{Mat4, Vec3};
use kmesh::{Mesh, Vertex};

/// 软体的解算参数。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SoftBodySettings {
    /// 一帧切成几个子步。
    ///
    /// 提硬度优先加这个而不是加 `iterations`：同样的总解算量下，
    /// 子步比多迭代更接近真解（XPBD 论文的主要结论）。
    /// 代价是每个子步都要重跑一遍碰撞查询。
    pub substeps: u32,
    /// 每个子步里约束解算几遍。
    pub iterations: u32,
    /// 结构约束的刚度，`0..1`。1 是每次解算都完全满足。
    pub stiffness: f32,
    /// 抗弯约束的刚度，`0..1`。
    ///
    /// 这一项决定布「有多想保持平整」。给 0 就是纯悬垂的布，
    /// 给大了会像纸板。
    pub bend_stiffness: f32,
    /// 速度阻尼，每秒衰减的比例。
    ///
    /// 布没有阻尼的话会一直抖——PBD 本身不耗散能量。
    pub damping: f32,
    /// 每个粒子当多大的球去和刚体碰。
    ///
    /// 给 0 的话布会正好贴着表面，而渲染出来的三角面会有一半穿进去。
    /// 一般给布厚度的一半。
    pub particle_radius: f32,
    /// 体积保持的强度，`0` 表示不做（布料就该是 0）。
    ///
    /// 只对**闭合**网格有意义。开的话软体会努力维持初始体积，
    /// 表现为「捏下去会鼓回来」。
    pub pressure: f32,
}

impl Default for SoftBodySettings {
    fn default() -> Self {
        Self {
            substeps: 4,
            iterations: 2,
            stiffness: 1.0,
            bend_stiffness: 0.2,
            damping: 0.5,
            particle_radius: 0.02,
            pressure: 0.0,
        }
    }
}

/// 一个粒子。
#[derive(Debug, Clone, Copy)]
struct Particle {
    position: Vec3,
    /// 上一个子步开始时的位置。速度是由它和当前位置反推的。
    previous: Vec3,
    velocity: Vec3,
    /// 质量的倒数。0 表示钉死（无限重）。
    inverse_mass: f32,
}

/// 一条「两点之间距离要保持」的约束。
#[derive(Debug, Clone, Copy)]
struct Distance {
    a: u32,
    b: u32,
    rest: f32,
    /// 结构约束还是抗弯约束——两者用不同的刚度。
    bend: bool,
}

/// 一块布或一个可压的闭合体积。
///
/// 它**不在** [`PhysicsWorld`] 里，是个独立的东西：自己 `step`，
/// 步进时把物理世界借进去做碰撞查询。这样刚体那一套完全不受影响，
/// 而且不用软体的项目一点代价都不付。
#[derive(Debug, Clone)]
pub struct SoftBody {
    particles: Vec<Particle>,
    constraints: Vec<Distance>,
    settings: SoftBodySettings,
    /// 三角形的顶点下标，按**粒子**编号。算法线和体积要用。
    triangles: Vec<[u32; 3]>,
    /// 原始网格的顶点 → 粒子下标。焊接之后是多对一。
    vertex_to_particle: Vec<u32>,
    /// 初始体积，体积约束的目标。
    rest_volume: f32,
}

impl SoftBody {
    /// 造一块矩形的布。
    ///
    /// `columns` × `rows` 是**粒子**的个数（不是格子数），至少 2×2。
    /// 布铺在局部空间的 XY 平面上，中心在原点，再乘 `transform` 摆到世界里。
    ///
    /// 约束分三种，缺一种都有明显的症状：
    ///
    /// | 约束 | 连谁 | 缺了会怎样 |
    /// |---|---|---|
    /// | 结构 | 上下左右相邻 | 布会像橡皮一样无限抻长 |
    /// | 剪切 | 对角线 | 方格会塌成菱形，布「歪」着垂下来 |
    /// | 抗弯 | 隔一个 | 布会折出锐利的死褶，不像织物 |
    pub fn cloth(columns: usize, rows: usize, width: f32, height: f32, transform: Mat4) -> Self {
        let columns = columns.max(2);
        let rows = rows.max(2);
        let index = |x: usize, y: usize| (y * columns + x) as u32;

        let mut particles = Vec::with_capacity(columns * rows);
        for y in 0..rows {
            for x in 0..columns {
                let u = x as f32 / (columns - 1) as f32;
                let v = y as f32 / (rows - 1) as f32;
                let local = Vec3::new((u - 0.5) * width, (0.5 - v) * height, 0.0);
                let position = transform.transform_point3(local);
                particles.push(Particle {
                    position,
                    previous: position,
                    velocity: Vec3::ZERO,
                    inverse_mass: 1.0,
                });
            }
        }

        let mut constraints = Vec::new();
        let mut add = |a: u32, b: u32, bend: bool, particles: &[Particle]| {
            let rest = (particles[a as usize].position - particles[b as usize].position).length();
            constraints.push(Distance { a, b, rest, bend });
        };
        for y in 0..rows {
            for x in 0..columns {
                // 结构：右、下。
                if x + 1 < columns {
                    add(index(x, y), index(x + 1, y), false, &particles);
                }
                if y + 1 < rows {
                    add(index(x, y), index(x, y + 1), false, &particles);
                }
                // 剪切：两条对角线。
                if x + 1 < columns && y + 1 < rows {
                    add(index(x, y), index(x + 1, y + 1), false, &particles);
                    add(index(x + 1, y), index(x, y + 1), false, &particles);
                }
                // 抗弯：隔一个连一条。
                if x + 2 < columns {
                    add(index(x, y), index(x + 2, y), true, &particles);
                }
                if y + 2 < rows {
                    add(index(x, y), index(x, y + 2), true, &particles);
                }
            }
        }

        let mut triangles = Vec::with_capacity((columns - 1) * (rows - 1) * 2);
        for y in 0..rows - 1 {
            for x in 0..columns - 1 {
                let (a, b, c, d) = (
                    index(x, y),
                    index(x + 1, y),
                    index(x + 1, y + 1),
                    index(x, y + 1),
                );
                triangles.push([a, b, c]);
                triangles.push([a, c, d]);
            }
        }

        let vertex_to_particle = (0..particles.len() as u32).collect();
        let mut body = Self {
            particles,
            constraints,
            settings: SoftBodySettings::default(),
            triangles,
            vertex_to_particle,
            rest_volume: 0.0,
        };
        body.rest_volume = body.volume();
        body
    }

    /// 从一个网格造软体。位置相同的顶点会被**焊接**成同一个粒子。
    ///
    /// 每条三角形的边成为一条结构约束（同一条边被两个三角形共用时只加一次）。
    ///
    /// 不焊接的话，图元在 UV 缝和硬边处的重复顶点会各走各的，
    /// 球会从缝上裂开——不报任何错，看起来像模型坏了。
    pub fn from_mesh(mesh: &Mesh, transform: Mat4) -> Self {
        // 按量化后的位置分桶。用 1e-4 的格子：比任何合理的建模精度都粗，
        // 又远小于最小的有意义特征。
        let quantize = |p: [f32; 3]| {
            [
                (p[0] * 10_000.0).round() as i64,
                (p[1] * 10_000.0).round() as i64,
                (p[2] * 10_000.0).round() as i64,
            ]
        };

        let mut lookup: std::collections::HashMap<[i64; 3], u32> = std::collections::HashMap::new();
        let mut particles: Vec<Particle> = Vec::new();
        let mut vertex_to_particle = Vec::with_capacity(mesh.vertices().len());

        for vertex in mesh.vertices() {
            let key = quantize(vertex.position);
            let index = *lookup.entry(key).or_insert_with(|| {
                let local = Vec3::from_array(vertex.position);
                let position = transform.transform_point3(local);
                particles.push(Particle {
                    position,
                    previous: position,
                    velocity: Vec3::ZERO,
                    inverse_mass: 1.0,
                });
                particles.len() as u32 - 1
            });
            vertex_to_particle.push(index);
        }

        let mut triangles = Vec::with_capacity(mesh.indices().len() / 3);
        let mut edges: std::collections::HashSet<(u32, u32)> = std::collections::HashSet::new();
        let mut constraints = Vec::new();
        for face in mesh.indices().chunks_exact(3) {
            let tri = [
                vertex_to_particle[face[0] as usize],
                vertex_to_particle[face[1] as usize],
                vertex_to_particle[face[2] as usize],
            ];
            // 焊接之后可能出现退化三角形（原本三个顶点里有两个同位置）。
            // 留着的话法线是 NaN，整块软体会消失。
            if tri[0] == tri[1] || tri[1] == tri[2] || tri[0] == tri[2] {
                continue;
            }
            triangles.push(tri);
            for (a, b) in [(tri[0], tri[1]), (tri[1], tri[2]), (tri[2], tri[0])] {
                let key = (a.min(b), a.max(b));
                if edges.insert(key) {
                    let rest =
                        (particles[a as usize].position - particles[b as usize].position).length();
                    constraints.push(Distance {
                        a,
                        b,
                        rest,
                        bend: false,
                    });
                }
            }
        }

        let mut body = Self {
            particles,
            constraints,
            settings: SoftBodySettings::default(),
            triangles,
            vertex_to_particle,
            rest_volume: 0.0,
        };
        body.rest_volume = body.volume();
        body
    }

    /// 当前的解算参数。
    pub fn settings(&self) -> SoftBodySettings {
        self.settings
    }

    /// 改解算参数。
    pub fn set_settings(&mut self, settings: SoftBodySettings) {
        self.settings = settings;
    }

    /// 粒子个数。**焊接之后**的数量，可能比网格顶点少。
    pub fn particle_count(&self) -> usize {
        self.particles.len()
    }

    /// 所有粒子的当前位置。
    pub fn positions(&self) -> impl Iterator<Item = Vec3> + '_ {
        self.particles.iter().map(|p| p.position)
    }

    /// 某个粒子的位置。
    pub fn position(&self, index: usize) -> Option<Vec3> {
        self.particles.get(index).map(|p| p.position)
    }

    /// 把一个粒子钉死。钉死的粒子不受任何力和约束影响。
    pub fn pin(&mut self, index: usize) {
        if let Some(p) = self.particles.get_mut(index) {
            p.inverse_mass = 0.0;
            p.velocity = Vec3::ZERO;
        }
    }

    /// 解开一个钉死的粒子。
    pub fn unpin(&mut self, index: usize) {
        if let Some(p) = self.particles.get_mut(index) {
            p.inverse_mass = 1.0;
        }
    }

    /// 某个粒子是不是钉死的。
    pub fn is_pinned(&self, index: usize) -> bool {
        self.particles
            .get(index)
            .is_some_and(|p| p.inverse_mass == 0.0)
    }

    /// 直接把一个粒子挪到某处，并清掉它的速度。
    ///
    /// 拿来做「拖着布的一角走」：每帧设位置，配合 [`pin`](Self::pin)。
    pub fn set_position(&mut self, index: usize, position: Vec3) {
        if let Some(p) = self.particles.get_mut(index) {
            p.position = position;
            p.previous = position;
            p.velocity = Vec3::ZERO;
        }
    }

    /// 离某个点最近的粒子。用来做鼠标抓取。
    pub fn nearest_particle(&self, point: Vec3) -> Option<usize> {
        self.particles
            .iter()
            .enumerate()
            .map(|(index, p)| (index, (p.position - point).length_squared()))
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(index, _)| index)
    }

    /// 给所有粒子加一个速度。做爆炸、风这类整体效果。
    pub fn add_velocity(&mut self, velocity: Vec3) {
        for particle in &mut self.particles {
            if particle.inverse_mass > 0.0 {
                particle.velocity += velocity;
            }
        }
    }

    /// 当前包围的体积（闭合网格才有意义）。
    ///
    /// 用散度定理：闭合曲面上 `∑ (a × b) · c / 6` 就是体积。
    /// 开放的网格（比如布）算出来是个没有意义的数，所以体积约束
    /// 只该用在闭合网格上。
    pub fn volume(&self) -> f32 {
        let mut total = 0.0;
        for tri in &self.triangles {
            let a = self.particles[tri[0] as usize].position;
            let b = self.particles[tri[1] as usize].position;
            let c = self.particles[tri[2] as usize].position;
            total += a.cross(b).dot(c);
        }
        total / 6.0
    }

    /// 初始体积，也就是体积约束的目标。
    pub fn rest_volume(&self) -> f32 {
        self.rest_volume
    }

    /// 步进一帧。
    ///
    /// `world` 给了就和里面的刚体做碰撞（**单向**：布被推开，刚体不动）。
    /// 给 `None` 就只有内部约束和重力。
    pub fn step(&mut self, dt: f32, gravity: Vec3, world: Option<&PhysicsWorld>) {
        if dt <= 0.0 || self.particles.is_empty() {
            return;
        }
        let substeps = self.settings.substeps.max(1);
        let h = dt / substeps as f32;
        // 阻尼是**每秒**衰减掉的比例，所以指数是子步的时长（秒），
        // 不是 `1/子步数`。
        //
        // 第一版写成了 `powf(1/substeps)`，那等于「每帧」衰减一次——
        // 60 Hz 下 0.5 的阻尼变成了每秒衰减到 0.5⁶⁰，布几乎落不下去
        // （半秒只掉 10 厘米）。而它看起来只是「布有点飘」，
        // 很容易被当成参数没调好。
        let damping = (1.0 - self.settings.damping.clamp(0.0, 1.0)).powf(h);

        for _ in 0..substeps {
            for particle in &mut self.particles {
                if particle.inverse_mass == 0.0 {
                    particle.previous = particle.position;
                    continue;
                }
                particle.velocity = (particle.velocity + gravity * h) * damping;
                particle.previous = particle.position;
                particle.position += particle.velocity * h;
            }

            for _ in 0..self.settings.iterations.max(1) {
                self.solve_distances();
                if self.settings.pressure > 0.0 {
                    self.solve_volume();
                }
            }

            if let Some(world) = world {
                self.solve_collisions(world);
            }

            // 速度由「实际挪了多少」反推。这是 PBD 的关键一步：
            // 约束把粒子拉回去的那部分位移会**变成**速度的修正，
            // 于是碰撞和约束天然就有正确的反弹与减速。
            for particle in &mut self.particles {
                if particle.inverse_mass == 0.0 {
                    continue;
                }
                particle.velocity = (particle.position - particle.previous) / h;
            }
        }
    }

    /// 距离约束：把每条边拉回它的静止长度。
    fn solve_distances(&mut self) {
        for constraint in &self.constraints {
            let stiffness = if constraint.bend {
                self.settings.bend_stiffness
            } else {
                self.settings.stiffness
            }
            .clamp(0.0, 1.0);
            if stiffness <= 0.0 {
                continue;
            }

            let (a, b) = (constraint.a as usize, constraint.b as usize);
            let (wa, wb) = (
                self.particles[a].inverse_mass,
                self.particles[b].inverse_mass,
            );
            let total = wa + wb;
            // 两端都钉死：这条约束没有任何自由度可动。
            if total <= 0.0 {
                continue;
            }

            let delta = self.particles[b].position - self.particles[a].position;
            let length = delta.length();
            // 两个粒子重合时方向没有定义。跳过而不是随便挑个方向——
            // 随便挑会让重合的粒子每帧朝不同方向弹开，抖个不停。
            if length < 1e-9 {
                continue;
            }

            let correction = delta * ((length - constraint.rest) / length / total) * stiffness;
            self.particles[a].position += correction * wa;
            self.particles[b].position -= correction * wb;
        }
    }

    /// 体积约束：把当前体积拉回 `rest_volume × pressure`。
    ///
    /// 按每个顶点的法线（相邻三角形面积加权）方向推。面积加权而不是
    /// 等权：不加权的话密集的地方会被推得更多，球会长出疙瘩。
    fn solve_volume(&mut self) {
        let target = self.rest_volume * self.settings.pressure;
        if target.abs() < 1e-9 {
            return;
        }
        let current = self.volume();
        let error = current - target;
        if error.abs() < 1e-9 {
            return;
        }

        // 梯度：每个顶点对体积的偏导，正是它所在三角形的法线之和 / 6。
        let mut gradients = vec![Vec3::ZERO; self.particles.len()];
        for tri in &self.triangles {
            let a = self.particles[tri[0] as usize].position;
            let b = self.particles[tri[1] as usize].position;
            let c = self.particles[tri[2] as usize].position;
            gradients[tri[0] as usize] += b.cross(c);
            gradients[tri[1] as usize] += c.cross(a);
            gradients[tri[2] as usize] += a.cross(b);
        }

        let mut denominator = 0.0;
        for (index, gradient) in gradients.iter().enumerate() {
            denominator += self.particles[index].inverse_mass * gradient.length_squared();
        }
        if denominator < 1e-9 {
            return;
        }

        // 除以 6 是因为上面的梯度没除；两边都省掉这个常数会让
        // 拉格朗日乘子差 6 倍，表现为体积恢复得过冲、然后来回跳。
        let lambda = -error * 6.0 / denominator;
        for (index, gradient) in gradients.iter().enumerate() {
            let w = self.particles[index].inverse_mass;
            if w > 0.0 {
                self.particles[index].position += *gradient * (lambda * w);
            }
        }
    }

    /// 和刚体世界碰撞：把陷进碰撞体里的粒子推到表面外。
    fn solve_collisions(&mut self, world: &PhysicsWorld) {
        let radius = self.settings.particle_radius.max(0.0);
        for particle in &mut self.particles {
            if particle.inverse_mass == 0.0 {
                continue;
            }
            // `solid = false`：要的是**表面**上最近的点。给 true 的话
            // 陷在内部的粒子会把自己的位置当成投影点返回，一动不动。
            let Some(projection) = world.project_point(
                particle.position,
                radius.max(1e-3),
                false,
                InteractionGroups::ALL,
            ) else {
                continue;
            };

            let to_surface = projection.point - particle.position;
            let distance = to_surface.length();
            if projection.is_inside {
                // 陷进去了：推到表面外面再留出半径。
                let normal = if distance > 1e-6 {
                    to_surface / distance
                } else {
                    Vec3::Y
                };
                particle.position = projection.point + normal * radius;
            } else if distance < radius {
                // 还在外面但贴得太近：沿表面法线顶开。
                let normal = if distance > 1e-6 {
                    -to_surface / distance
                } else {
                    Vec3::Y
                };
                particle.position = projection.point + normal * radius;
            }
        }
    }

    /// 造一个和这个软体拓扑一致的网格，顶点在当前位置。
    ///
    /// 布料用这个建初始网格；之后每帧走
    /// [`write_positions`](Self::write_positions) 更新，
    /// 那条不重建索引，便宜得多。
    pub fn build_mesh(&self) -> Mesh {
        let vertices: Vec<Vertex> = self
            .particles
            .iter()
            .map(|particle| Vertex::new(particle.position, Vec3::Y, [0.0, 0.0]))
            .collect();
        let indices: Vec<u32> = self.triangles.iter().flatten().copied().collect();
        let mut mesh = Mesh::new(vertices, indices);
        mesh.recompute_normals();
        mesh.recompute_tangents();
        mesh
    }

    /// 造一块带 UV 的布网格。`columns`/`rows` 要和造它时给的一致。
    pub fn build_cloth_mesh(&self, columns: usize, rows: usize) -> Mesh {
        let columns = columns.max(2);
        let rows = rows.max(2);
        let vertices: Vec<Vertex> = self
            .particles
            .iter()
            .enumerate()
            .map(|(index, particle)| {
                let x = index % columns;
                let y = index / columns;
                Vertex::new(
                    particle.position,
                    Vec3::Y,
                    [
                        x as f32 / (columns - 1) as f32,
                        y as f32 / (rows - 1) as f32,
                    ],
                )
            })
            .collect();
        let indices: Vec<u32> = self.triangles.iter().flatten().copied().collect();
        let mut mesh = Mesh::new(vertices, indices);
        mesh.recompute_normals();
        mesh.recompute_tangents();
        mesh
    }

    /// 把当前位置写回一个网格，并重算法线。
    ///
    /// 网格的顶点数必须和造这个软体时的一致——对不上就什么都不做，
    /// 因为写进去的会是一堆错位的坐标，而那看起来像模型炸了。
    pub fn write_positions(&self, mesh: &mut Mesh) -> bool {
        if mesh.vertices().len() != self.vertex_to_particle.len() {
            return false;
        }
        for (vertex, &particle) in mesh.vertices_mut().iter_mut().zip(&self.vertex_to_particle) {
            vertex.position = self.particles[particle as usize].position.to_array();
        }
        // 法线必须重算：软体每帧都在变形，用旧法线的话光照会像
        // 贴在一张不动的布上，形状变了明暗不变。
        mesh.recompute_normals();
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ColliderDesc, RigidBodyDesc};

    fn cloth() -> SoftBody {
        SoftBody::cloth(8, 8, 2.0, 2.0, Mat4::IDENTITY)
    }

    fn step_for(body: &mut SoftBody, world: Option<&PhysicsWorld>, seconds: f32) {
        let dt = 1.0 / 60.0;
        for _ in 0..(seconds / dt) as usize {
            body.step(dt, Vec3::new(0.0, -9.81, 0.0), world);
        }
    }

    #[test]
    fn a_free_cloth_falls_under_gravity() {
        let mut body = cloth();
        let before = body.position(0).unwrap();
        step_for(&mut body, None, 0.5);
        let after = body.position(0).unwrap();
        assert!(
            after.y < before.y - 0.5,
            "自由下落了 {} 米",
            before.y - after.y
        );
    }

    #[test]
    fn a_pinned_particle_does_not_move() {
        // 钉死是「无限质量」，不是「每帧摆回去」。约束求解时必须
        // 完全不动它，否则布会把钉子拽下来一点点，日积月累整块布下沉。
        let mut body = cloth();
        body.pin(0);
        let pinned = body.position(0).unwrap();
        step_for(&mut body, None, 2.0);
        assert_eq!(body.position(0).unwrap(), pinned, "钉死的粒子动了");
    }

    #[test]
    fn a_cloth_hung_from_two_corners_does_not_stretch_without_bound() {
        // 这正是「弹簧 + 显式积分」做不到的事：60 Hz 下想让布不抻长，
        // 弹簧就得硬到炸。PBD 的刚度是「解算几次」，所以能做到。
        let mut body = cloth();
        body.pin(0);
        body.pin(7);
        let span = (body.position(0).unwrap() - body.position(7).unwrap()).length();

        step_for(&mut body, None, 3.0);

        // 最底下那一行的中点离顶边有多远。布长 2 米，垂下来不该超过太多。
        let bottom = body.position(8 * 7 + 4).unwrap();
        let drop = body.position(0).unwrap().y - bottom.y;
        assert!(
            drop < 2.6,
            "布垂下去 {drop} 米，而它总共才 2 米 —— 抻得太厉害了"
        );
        assert!(drop > 1.0, "布只垂下去 {drop} 米，硬得不像布");
        // 顶边两个钉子之间的距离一点没变。
        let span_after = (body.position(0).unwrap() - body.position(7).unwrap()).length();
        assert!((span_after - span).abs() < 1e-4);
    }

    #[test]
    fn every_position_stays_finite() {
        // PBD 号称无条件稳定。真炸了的话是 NaN 而不是「看起来抖」——
        // 而 NaN 会顺着法线传进渲染，整块布消失。
        let mut body = cloth();
        body.pin(0);
        // 故意给一个离谱的步长和一记猛推。
        body.add_velocity(Vec3::new(50.0, 80.0, -50.0));
        for _ in 0..200 {
            body.step(0.2, Vec3::new(0.0, -9.81, 0.0), None);
        }
        for position in body.positions() {
            assert!(position.is_finite(), "出现了 NaN：{position:?}");
        }
    }

    #[test]
    fn duplicate_mesh_vertices_are_welded_into_one_particle() {
        // 球在 UV 缝上有重复顶点。不焊的话那些顶点各走各的，
        // 球会从缝上裂开——不报任何错，看起来像模型坏了。
        let mesh = Mesh::sphere(12, 16);
        let body = SoftBody::from_mesh(&mesh, Mat4::IDENTITY);
        assert!(
            body.particle_count() < mesh.vertices().len(),
            "球的 {} 个顶点一个都没焊上",
            mesh.vertices().len()
        );
        // 每个原始顶点都得映射到一个合法的粒子。
        assert_eq!(body.vertex_to_particle.len(), mesh.vertices().len());
        assert!(
            body.vertex_to_particle
                .iter()
                .all(|&i| (i as usize) < body.particle_count())
        );
    }

    #[test]
    fn a_welded_sphere_does_not_split_at_the_seam() {
        // 焊接的实际效果：缝两侧原本重合的顶点，运动之后仍然重合。
        let mesh = Mesh::sphere(12, 16);
        let mut body = SoftBody::from_mesh(&mesh, Mat4::IDENTITY);
        body.set_settings(SoftBodySettings {
            pressure: 1.0,
            ..SoftBodySettings::default()
        });
        step_for(&mut body, None, 1.0);

        let mut updated = mesh.clone();
        assert!(body.write_positions(&mut updated));

        // 原来位置相同的顶点，现在必须还相同。
        for (a, va) in mesh.vertices().iter().enumerate() {
            for (b, vb) in mesh.vertices().iter().enumerate().skip(a + 1) {
                if Vec3::from_array(va.position).distance(Vec3::from_array(vb.position)) < 1e-5 {
                    let pa = Vec3::from_array(updated.vertices()[a].position);
                    let pb = Vec3::from_array(updated.vertices()[b].position);
                    assert!(pa.distance(pb) < 1e-5, "缝上的两个顶点裂开了");
                }
            }
        }
    }

    #[test]
    fn pressure_inflates_a_closed_mesh_beyond_its_rest_volume() {
        // 压力给到 2 就是「鼓成静止体积的两倍」。结构约束会抗，
        // 所以调软一点让压力说了算——这条验的是体积约束本身在不在起作用。
        //
        // 不能靠「给所有粒子加同一个速度」去压它：那是纯平移，
        // 一点形变都没有，两组跑出来会一模一样（第一版就是这么错的）。
        let mesh = Mesh::sphere(12, 16);
        let rest = SoftBody::from_mesh(&mesh, Mat4::IDENTITY).volume();

        let run = |pressure: f32| {
            let mut body = SoftBody::from_mesh(&mesh, Mat4::IDENTITY);
            body.set_settings(SoftBodySettings {
                pressure,
                stiffness: 0.05,
                bend_stiffness: 0.0,
                ..SoftBodySettings::default()
            });
            // 没有重力：只看体积约束干了什么。
            for _ in 0..300 {
                body.step(1.0 / 60.0, Vec3::ZERO, None);
            }
            body.volume()
        };

        let idle = run(0.0);
        let inflated = run(2.0);

        assert!(
            (idle - rest).abs() < rest * 0.05,
            "没开压力体积从 {rest} 变成了 {idle} —— 不该有变化"
        );
        assert!(
            inflated > rest * 1.3,
            "压力给到 2 倍，体积只从 {rest} 涨到 {inflated}"
        );
    }

    #[test]
    fn pressure_pushes_a_squashed_mesh_back_out() {
        // 「捏下去会鼓回来」才是体积约束的实际用途。压扁之后，
        // 开压力的那个要比不开的恢复得多。
        let mesh = Mesh::sphere(12, 16);

        let run = |pressure: f32| {
            let mut body = SoftBody::from_mesh(&mesh, Mat4::IDENTITY);
            body.set_settings(SoftBodySettings {
                pressure,
                stiffness: 0.02,
                bend_stiffness: 0.0,
                ..SoftBodySettings::default()
            });
            // 沿 Y 压扁一半——这是真的形变，不是平移。
            let squashed: Vec<Vec3> = body
                .positions()
                .map(|p| Vec3::new(p.x, p.y * 0.35, p.z))
                .collect();
            for (index, position) in squashed.into_iter().enumerate() {
                body.set_position(index, position);
            }
            let start = body.volume();
            for _ in 0..300 {
                body.step(1.0 / 60.0, Vec3::ZERO, None);
            }
            (start, body.volume())
        };

        let (squashed, without) = run(0.0);
        let (_, with) = run(1.0);

        assert!(
            with > without * 1.2,
            "压扁到 {squashed} 之后，开压力恢复到 {with}，不开恢复到 {without}"
        );
    }

    #[test]
    fn the_volume_of_a_unit_cube_is_one() {
        // 散度定理那个公式的符号和系数都容易写错，而错了只会表现为
        // 「压力调不出想要的效果」。拿一个已知答案钉死它。
        let body = SoftBody::from_mesh(&Mesh::cube(), Mat4::IDENTITY);
        assert!(
            (body.volume() - 1.0).abs() < 1e-4,
            "单位立方体的体积算成了 {}",
            body.volume()
        );
    }

    #[test]
    fn a_cloth_lands_on_a_rigid_body_instead_of_falling_through() {
        // 碰撞是这个模块最容易「装了没生效」的一处：`project_point`
        // 的 `solid` 参数给反的话，陷进去的粒子会把自己当成投影点，
        // 一动不动地穿过去。
        let mut world = PhysicsWorld::new();
        let ground = world.add_body(&RigidBodyDesc::fixed(), 0);
        world
            .add_collider(
                &ColliderDesc::cuboid(Vec3::new(10.0, 0.5, 10.0)),
                Some(ground),
                0,
            )
            .unwrap();
        world.update_query_structures();

        let mut body = SoftBody::cloth(
            10,
            10,
            3.0,
            3.0,
            // 平铺在地面上方，绕 X 转 90° 让它水平。
            Mat4::from_rotation_translation(
                kmath::Quat::from_rotation_x(-std::f32::consts::FRAC_PI_2),
                Vec3::new(0.0, 3.0, 0.0),
            ),
        );
        body.set_settings(SoftBodySettings {
            particle_radius: 0.05,
            ..SoftBodySettings::default()
        });

        step_for(&mut body, Some(&world), 3.0);

        let lowest = body.positions().map(|p| p.y).fold(f32::INFINITY, f32::min);
        assert!(
            lowest > 0.4,
            "布最低点到了 y = {lowest}，地面在 0.5 —— 它穿过去了"
        );
        assert!(
            lowest < 0.7,
            "布停在 y = {lowest}，离地面太远，没真的落下来"
        );
    }

    #[test]
    fn write_positions_rejects_a_mismatched_mesh() {
        // 顶点数对不上还照写的话，会写进一堆错位的坐标，
        // 而那看起来像「模型炸了」而不是「用错了 API」。
        let body = cloth();
        let mut wrong = Mesh::cube();
        assert!(!body.write_positions(&mut wrong));
    }

    #[test]
    fn substeps_do_not_change_how_damped_the_cloth_feels() {
        // 阻尼是按整帧给的，要分摊到子步上。不分摊的话调子步数会
        // 顺带改变布的「黏度」，而那不该是子步数的副作用。
        let settle = |substeps: u32| {
            let mut body = SoftBody::cloth(8, 8, 2.0, 2.0, Mat4::IDENTITY);
            body.set_settings(SoftBodySettings {
                substeps,
                damping: 0.9,
                ..SoftBodySettings::default()
            });
            body.pin(0);
            body.pin(7);
            step_for(&mut body, None, 1.0);
            body.position(60).unwrap().y
        };
        let few = settle(1);
        let many = settle(8);
        assert!(
            (few - many).abs() < 0.2,
            "1 个子步落到 {few}，8 个子步落到 {many} —— 阻尼跟着子步数变了"
        );
    }
}
