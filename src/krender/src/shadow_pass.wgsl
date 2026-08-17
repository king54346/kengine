// 阴影贴图的深度 pass 与采样。

struct ShadowGlobals {
    // 光空间矩阵（投影 × 视图）
    light_view_proj: mat4x4<f32>,
    // x = 深度偏移，y = 法线偏移，z = 阴影贴图边长，w = 是否启用（0/1）
    params: vec4<f32>,
};

@group(0) @binding(0) var<uniform> shadow_globals: ShadowGlobals;

struct ShadowObject {
    model: mat4x4<f32>,
};

// 与主 pass 一样按实例寻址，好让深度 pass 也能一次画完一整批。
@group(1) @binding(0) var<storage, read> shadow_objects: array<ShadowObject>;

// ── 深度 pass：只需要把顶点变换到光空间 ──

@vertex
fn shadow_vs(
    @location(0) position: vec3<f32>,
    @builtin(instance_index) instance: u32,
) -> @builtin(position) vec4<f32> {
    return shadow_globals.light_view_proj * shadow_objects[instance].model * vec4<f32>(position, 1.0);
}
