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

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
};

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
    let color = in.color * texel;

    // 输出预乘 alpha 的颜色：这样「普通半透明」与「相加」两种混合
    // 只差一个混合状态，片元着色器可以完全共用。
    return vec4<f32>(color.rgb * color.a, color.a);
}
