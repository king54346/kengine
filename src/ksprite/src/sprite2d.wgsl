// 2D 精灵着色器。
//
// 几何在顶点着色器里长出来：CPU 每帧只上传实例数组，不重建顶点缓冲。
// 一个精灵六个顶点（两个三角形），四个角由 `vertex_index` 推出来。
//
// 为什么不复用 3D 管线：3D 那条路每个精灵是一个带材质的网格，
// 逐精灵要绑一次材质、提一次绘制。几万个精灵时瓶颈全在提交上。
// 这里一批（同一张纹理）只提交一次。

struct Globals {
    // 世界坐标到裁剪空间。2D 相机是正交的。
    view_proj: mat4x4<f32>,
};

@group(0) @binding(0)
var<uniform> globals: Globals;

// 一个精灵实例。字段顺序与 CPU 侧的 `GpuSprite` 一一对应。
struct Sprite {
    // 左下角的世界坐标。
    position: vec2<f32>,
    // 世界尺寸。
    size: vec2<f32>,
    // 图集区域：[u0, v0, u1, v1]。
    uv: vec4<f32>,
    // 顶点色。
    color: vec4<f32>,
    // 绕中心旋转的弧度。
    rotation: f32,
    // 补齐到 64 字节。
    //
    // **写成三个 f32 而不是一个 vec3**：WGSL 里 vec3 按 16 字节对齐，
    // 会把 rotation 之后的偏移推到 64，整个结构体撑到 80——
    // 而 Rust 侧是 64。对不上就是满屏乱码，且不报任何错。
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
};

@group(1) @binding(0)
var<storage, read> sprites: array<Sprite>;

@group(2) @binding(0)
var sprite_texture: texture_2d<f32>;
@group(2) @binding(1)
var sprite_sampler: sampler;

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
};

@vertex
fn sprite_vs(
    @builtin(vertex_index) vertex_index: u32,
    @builtin(instance_index) instance_index: u32,
) -> VertexOutput {
    let sprite = sprites[instance_index];

    // 两个三角形的六个顶点，按 0-1-2 / 0-2-3 展开成四个角。
    // 角的顺序：左下、右下、右上、左上（逆时针，正面朝 +Z）。
    var corner_index = array<u32, 6>(0u, 1u, 2u, 0u, 2u, 3u);
    let corner = corner_index[vertex_index];

    var offsets = array<vec2<f32>, 4>(
        vec2<f32>(-0.5, -0.5),
        vec2<f32>( 0.5, -0.5),
        vec2<f32>( 0.5,  0.5),
        vec2<f32>(-0.5,  0.5),
    );
    let local = offsets[corner] * sprite.size;

    // 绕**中心**旋转。绕左下角转的话，改朝向会让精灵整个甩出去。
    let s = sin(sprite.rotation);
    let c = cos(sprite.rotation);
    let rotated = vec2<f32>(
        local.x * c - local.y * s,
        local.x * s + local.y * c,
    );
    let center = sprite.position + sprite.size * 0.5;
    let world = center + rotated;

    // UV 的 V 要翻转：贴图坐标原点在左上，世界 Y 向上。
    // 不翻的话每个精灵都上下颠倒。
    var uvs = array<vec2<f32>, 4>(
        vec2<f32>(sprite.uv.x, sprite.uv.w),
        vec2<f32>(sprite.uv.z, sprite.uv.w),
        vec2<f32>(sprite.uv.z, sprite.uv.y),
        vec2<f32>(sprite.uv.x, sprite.uv.y),
    );

    var out: VertexOutput;
    out.clip_position = globals.view_proj * vec4<f32>(world, 0.0, 1.0);
    out.uv = uvs[corner];
    out.color = sprite.color;
    return out;
}

@fragment
fn sprite_fs(in: VertexOutput) -> @location(0) vec4<f32> {
    let sampled = textureSample(sprite_texture, sprite_sampler, in.uv) * in.color;
    // 预乘 alpha 输出，与混合状态一致。
    return vec4<f32>(sampled.rgb * sampled.a, sampled.a);
}
