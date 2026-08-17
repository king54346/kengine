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
    // x = 骨骼矩阵起点，y = 形变增量起点，z = 形变目标数，w = 形变权重起点
    skin: vec4<u32>,
};

struct MorphDelta {
    position: vec3<f32>,
    padding0: f32,
    normal: vec3<f32>,
    padding1: f32,
};

// 与主 pass 一样按实例寻址，好让深度 pass 也能一次画完一整批。
@group(1) @binding(0) var<storage, read> shadow_objects: array<ShadowObject>;
@group(1) @binding(1) var<storage, read> shadow_joints: array<mat4x4<f32>>;
@group(1) @binding(2) var<storage, read> shadow_morph_deltas: array<MorphDelta>;
@group(1) @binding(3) var<storage, read> shadow_morph_weights: array<f32>;

// 深度 pass 只关心位置，法线增量用不上。
fn shadow_morph_position(vertex_index: u32, object: ShadowObject, position: vec3<f32>) -> vec3<f32> {
    let count = object.skin.z;
    if (count == 0u) {
        return position;
    }

    var result = position;
    let base = object.skin.y + vertex_index * count;
    for (var i = 0u; i < count; i = i + 1u) {
        let weight = shadow_morph_weights[object.skin.w + i];
        if (weight == 0.0) {
            continue;
        }
        result = result + shadow_morph_deltas[base + i].position * weight;
    }
    return result;
}

// ── 深度 pass：只需要把顶点变换到光空间 ──

@vertex
fn shadow_vs(
    @location(0) position: vec3<f32>,
    @builtin(vertex_index) vertex_index: u32,
    @builtin(instance_index) instance: u32,
) -> @builtin(position) vec4<f32> {
    let object = shadow_objects[instance];
    // 形变也要参与投影，否则张开的嘴投出来的影子还是闭着的。
    let morphed = shadow_morph_position(vertex_index, object, position);
    return shadow_globals.light_view_proj * object.model * vec4<f32>(morphed, 1.0);
}

// 蒙皮物体的深度也要按骨骼变形，否则角色动起来了、影子还是绑定姿态。
@vertex
fn shadow_skinned_vs(
    @location(0) position: vec3<f32>,
    @location(5) joints: vec4<u32>,
    @location(6) weights: vec4<f32>,
    @builtin(vertex_index) vertex_index: u32,
    @builtin(instance_index) instance: u32,
) -> @builtin(position) vec4<f32> {
    let object = shadow_objects[instance];
    let morphed = shadow_morph_position(vertex_index, object, position);
    let offset = object.skin.x;
    let skin = weights.x * shadow_joints[offset + joints.x]
        + weights.y * shadow_joints[offset + joints.y]
        + weights.z * shadow_joints[offset + joints.z]
        + weights.w * shadow_joints[offset + joints.w];
    return shadow_globals.light_view_proj * object.model * skin * vec4<f32>(morphed, 1.0);
}
