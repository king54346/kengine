// 粒子着色器：把每个粒子展开成一个面向相机的方片（billboard）。
//
// 方片不在 CPU 上生成：顶点着色器按 vertex_index 取四个角，
// 按 instance_index 取粒子数据，几何完全在 GPU 上长出来。
// CPU 每帧只需要上传一个粒子数组，省掉了顶点缓冲的重建。

struct ParticleGlobals {
    view_proj: mat4x4<f32>,
    // 相机的右向量与上向量（世界空间）。方片沿这两个方向张开，
    // 于是无论相机怎么转，它始终正对镜头。
    camera_right: vec4<f32>,
    camera_up: vec4<f32>,
    // 软粒子参数：
    //   x = 淡出距离（世界单位），0 表示关闭
    //   y = 投影矩阵的 [2][2]，z = 投影矩阵的 [3][2]
    //   w = 保留
    //
    // y 和 z 用来把深度缓冲里的非线性值还原成视空间距离。直接传这两个
    // 系数而不是 near/far：正交投影和透视投影的公式不同，传系数两边通用。
    soft_params: vec4<f32>,
};

struct Particle {
    position: vec3<f32>,
    size: f32,
    color: vec4<f32>,
    rotation: f32,
    // 填充写成三个标量而不是一个 vec3：vec3 的对齐要求是 16 字节，
    // 会把自己推到偏移 48，整个结构体因此涨到 64 字节，
    // 与 CPU 侧紧凑排布的 48 字节对不上，绑定时就会被 wgpu 打回。
    padding_x: f32,
    padding_y: f32,
    padding_z: f32,
};

@group(0) @binding(0) var<uniform> particle_globals: ParticleGlobals;
@group(1) @binding(0) var<storage, read> particles: array<Particle>;
@group(2) @binding(0) var particle_texture: texture_2d<f32>;
@group(2) @binding(1) var particle_sampler: sampler;
// 不透明几何的深度。粒子 pass 用**只读**深度附件，所以同一张纹理
// 既当深度测试的对象又当采样源——WebGPU 明确允许这种用法。
@group(3) @binding(0) var scene_depth: texture_depth_2d;

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
};

// 把深度缓冲里的值还原成视空间的距离（正数，越远越大）。
//
// 透视投影下：`clip.z = a * z + b`、`clip.w = -z`，所以
// `depth = (a * z + b) / (-z)`。解出 z：
//
//     depth * (-z) = a * z + b
//     z * (-depth - a) = b
//     z = -b / (depth + a)
//
// 传进来的 a 是 `projection[2][2]`，b 是 `projection[3][2]`。
//
// **只对透视投影成立。** 正交投影下 `clip.w` 恒为 1，公式完全不同——
// 那种情况由 CPU 侧把淡出距离置 0 关掉，不走到这里。
fn linear_depth(depth: f32, a: f32, b: f32) -> f32 {
    let denominator = depth + a;
    // 退化时返回一个很大的值，效果是「背景无穷远」，粒子不淡出——
    // 比返回 NaN 强，NaN 会让整个粒子变成黑洞，而且顺着 Bloom
    // 扩散到整个画面。
    if (abs(denominator) < 1e-9) {
        return 1e9;
    }
    return abs(b / denominator);
}

@vertex
fn particle_vs(
    @builtin(vertex_index) vertex: u32,
    @builtin(instance_index) instance: u32,
) -> VertexOutput {
    // 两个三角形拼成一个方片。六个顶点直接写死，不需要索引缓冲。
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(-0.5, -0.5),
        vec2<f32>(0.5, -0.5),
        vec2<f32>(0.5, 0.5),
        vec2<f32>(-0.5, -0.5),
        vec2<f32>(0.5, 0.5),
        vec2<f32>(-0.5, 0.5),
    );

    let particle = particles[instance];
    let corner = corners[vertex];

    // 绕视线轴自转。旋转的是顶点位置而不是 uv，贴图因此跟着一起转。
    let sin_r = sin(particle.rotation);
    let cos_r = cos(particle.rotation);
    let rotated = vec2<f32>(
        corner.x * cos_r - corner.y * sin_r,
        corner.x * sin_r + corner.y * cos_r,
    );

    let offset = (particle_globals.camera_right.xyz * rotated.x
        + particle_globals.camera_up.xyz * rotated.y) * particle.size;

    var out: VertexOutput;
    out.clip_position = particle_globals.view_proj * vec4<f32>(particle.position + offset, 1.0);
    out.uv = corner + vec2<f32>(0.5, 0.5);
    out.color = particle.color;
    return out;
}

@fragment
fn particle_fs(in: VertexOutput) -> @location(0) vec4<f32> {
    let texel = textureSample(particle_texture, particle_sampler, in.uv);
    var color = in.color * texel;

    // ── 软粒子 ──
    //
    // 粒子是个方片，插进地面时会露出一条**笔直的交线**——一眼就能
    // 看出它是张纸。做法是：比较粒子自身的深度和它背后不透明几何的
    // 深度，两者越接近就越透明，交线因此被抹掉。
    let fade_distance = particle_globals.soft_params.x;
    if (fade_distance > 0.0) {
        // `clip_position` 在片元阶段已经是屏幕坐标（像素），
        // 直接当纹素坐标用，不需要再做一次投影除法。
        let coord = vec2<i32>(i32(in.clip_position.x), i32(in.clip_position.y));
        let scene = linear_depth(
            textureLoad(scene_depth, coord, 0),
            particle_globals.soft_params.y,
            particle_globals.soft_params.z,
        );
        let particle = linear_depth(
            in.clip_position.z,
            particle_globals.soft_params.y,
            particle_globals.soft_params.z,
        );

        // 粒子在几何**前面**多远。为负说明粒子在几何后面，
        // 那本来就会被深度测试剔掉，这里夹到 0 不影响。
        let gap = scene - particle;
        color.a = color.a * clamp(gap / fade_distance, 0.0, 1.0);
    }

    // 输出预乘 alpha 的颜色：这样「普通半透明」与「相加」两种混合
    // 只差一个混合状态，片元着色器可以完全共用。
    return vec4<f32>(color.rgb * color.a, color.a);
}
