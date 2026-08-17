// kengine 标准着色器：Cook-Torrance PBR + 基础色贴图。
//
// BRDF 函数（pbr_* 前缀）由 kpbr 提供，光源求值（light_* 前缀）由 klight 提供，
// 渲染器在创建管线时把它们拼接到本文件前面。

struct Globals {
    view_proj: mat4x4<f32>,
    camera_position: vec4<f32>,
    // rgb = 环境光贡献，a 未使用
    ambient: vec4<f32>,
    // x = 生效的光源数量，其余保留
    light_count: vec4<u32>,
    // 阴影光源的光空间矩阵
    light_view_proj: mat4x4<f32>,
    // x = 深度偏移，y = 法线偏移，z = 阴影贴图边长，w = 是否启用
    shadow_params: vec4<f32>,
    environment: Environment,
    lights: array<Light, 16>,
};

struct ObjectUniforms {
    model: mat4x4<f32>,
    // 法线矩阵：model 的逆转置，保证非均匀缩放下法线仍然正确
    normal_matrix: mat4x4<f32>,
    base_color: vec4<f32>,
    metallic: f32,
    roughness: f32,
    // 法线贴图强度：0 表示完全忽略贴图
    normal_scale: f32,
    // 环境光遮蔽强度
    occlusion_strength: f32,
    // rgb = 自发光颜色，a 保留
    emissive: vec4<f32>,
    // x = 骨骼矩阵起点，y = 形变增量起点，z = 形变目标数，w = 形变权重起点
    skin: vec4<u32>,
};

// 一个顶点在某个形变目标下的增量。两个 vec3 各自补齐到 16 字节。
struct MorphDelta {
    position: vec3<f32>,
    padding0: f32,
    normal: vec3<f32>,
    padding1: f32,
};

@group(0) @binding(0) var<uniform> globals: Globals;
// 每个实例一份，用 instance_index 寻址。存储缓冲而非 uniform：
// 一次 draw 就能画完一批同网格同贴图的对象，不必逐个切换动态偏移。
@group(1) @binding(0) var<storage, read> objects: array<ObjectUniforms>;
// 所有蒙皮实例的骨骼矩阵拼在一起，各实例按自己的偏移取用。
// 静态渲染时这里绑的是一个占位缓冲，谁也不会去读。
@group(1) @binding(1) var<storage, read> joint_matrices: array<mat4x4<f32>>;
// 所有带形变的网格的增量拼在一起，按「顶点优先」排列：
// 同一顶点的各个目标相邻，读一个顶点的全部形变只碰一段连续内存。
@group(1) @binding(2) var<storage, read> morph_deltas: array<MorphDelta>;
// 每个实例一段形变权重，实例自己记着起点。
@group(1) @binding(3) var<storage, read> morph_weights: array<f32>;
@group(2) @binding(0) var base_color_texture: texture_2d<f32>;
@group(2) @binding(1) var base_color_sampler: sampler;
@group(2) @binding(2) var normal_texture: texture_2d<f32>;
@group(2) @binding(3) var metallic_roughness_texture: texture_2d<f32>;
@group(2) @binding(4) var occlusion_texture: texture_2d<f32>;
@group(2) @binding(5) var emissive_texture: texture_2d<f32>;
// 环境 BRDF 查找表：u = n·v，v = 粗糙度。
@group(3) @binding(0) var brdf_lut: texture_2d<f32>;
@group(3) @binding(1) var brdf_sampler: sampler;
@group(3) @binding(2) var shadow_map: texture_depth_2d;
@group(3) @binding(3) var shadow_sampler: sampler_comparison;

struct VertexInput {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) color: vec3<f32>,
    // xyz = 切线，w = 副切线手性
    @location(4) tangent: vec4<f32>,
};

// 蒙皮顶点属性，作为第二个顶点缓冲送进来。只有蒙皮管线声明它。
struct SkinInput {
    @location(5) joints: vec4<u32>,
    @location(6) weights: vec4<f32>,
};

// 线性混合蒙皮：顶点的最终变换是四个关节矩阵的加权和。
// 权重在导入时已经归一化，这里直接相加即可。
fn skin_matrix(joints: vec4<u32>, weights: vec4<f32>, offset: u32) -> mat4x4<f32> {
    return weights.x * joint_matrices[offset + joints.x]
        + weights.y * joint_matrices[offset + joints.y]
        + weights.z * joint_matrices[offset + joints.z]
        + weights.w * joint_matrices[offset + joints.w];
}

// 把形变增量叠加到顶点上。没有形变目标时（count = 0）整个循环不执行。
//
// 形变发生在蒙皮之前：形变改的是绑定姿态下的网格形状，
// 骨骼再把这个形状带到世界里——顺序反了，张嘴的幅度会被骨骼的缩放放大。
fn apply_morph(
    vertex_index: u32,
    offset: u32,
    count: u32,
    weight_offset: u32,
    position: ptr<function, vec3<f32>>,
    normal: ptr<function, vec3<f32>>,
) {
    if (count == 0u) {
        return;
    }

    let base = offset + vertex_index * count;
    for (var i = 0u; i < count; i = i + 1u) {
        let weight = morph_weights[weight_offset + i];
        // 权重为 0 的目标占多数（一张脸几十个表情通常只有几个在起作用），
        // 跳过它们能省下大量无用的读取。
        if (weight == 0.0) {
            continue;
        }
        let delta = morph_deltas[base + i];
        *position = *position + delta.position * weight;
        *normal = *normal + delta.normal * weight;
    }
}

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) world_position: vec3<f32>,
    @location(1) world_normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) color: vec3<f32>,
    @location(4) world_tangent: vec3<f32>,
    @location(5) tangent_handedness: f32,
    // 片元着色器要拿它回存储缓冲里取材质参数。同一实例内是常量，故 flat。
    @location(6) @interpolate(flat) instance: u32,
};

@vertex
fn vs_main(
    in: VertexInput,
    @builtin(vertex_index) vertex_index: u32,
    @builtin(instance_index) instance: u32,
) -> VertexOutput {
    let object = objects[instance];

    var position = in.position;
    var normal = in.normal;
    apply_morph(
        vertex_index,
        object.skin.y,
        object.skin.z,
        object.skin.w,
        &position,
        &normal,
    );

    let world_position = object.model * vec4<f32>(position, 1.0);

    var out: VertexOutput;
    out.instance = instance;
    out.clip_position = globals.view_proj * world_position;
    out.world_position = world_position.xyz;
    out.world_normal = (object.normal_matrix * vec4<f32>(normal, 0.0)).xyz;
    // 切线随模型矩阵变换即可，不需要逆转置——它是切向而非法向。
    out.world_tangent = (object.model * vec4<f32>(in.tangent.xyz, 0.0)).xyz;
    out.tangent_handedness = in.tangent.w;
    out.uv = in.uv;
    out.color = in.color;
    return out;
}

@vertex
fn vs_skinned(
    in: VertexInput,
    skin: SkinInput,
    @builtin(vertex_index) vertex_index: u32,
    @builtin(instance_index) instance: u32,
) -> VertexOutput {
    let object = objects[instance];

    // 先形变再蒙皮：形变改的是绑定姿态下的形状，骨骼再把它带到世界里。
    var position = in.position;
    var normal = in.normal;
    apply_morph(
        vertex_index,
        object.skin.y,
        object.skin.z,
        object.skin.w,
        &position,
        &normal,
    );

    // 骨骼矩阵已经包含了模型在世界里的位姿，所以蒙皮网格的 model 是单位阵；
    // 这里仍然乘上它，是为了让两条路径保持同一个公式。
    let model = object.model * skin_matrix(skin.joints, skin.weights, object.skin.x);
    let world_position = model * vec4<f32>(position, 1.0);

    var out: VertexOutput;
    out.instance = instance;
    out.clip_position = globals.view_proj * world_position;
    // 骨骼变换是刚体的（旋转加平移），逆转置等于它自己的 3×3 部分，
    // 所以法线直接乘 model 即可。骨骼带非均匀缩放时这里会有偏差。
    out.world_normal = (model * vec4<f32>(normal, 0.0)).xyz;
    out.world_tangent = (model * vec4<f32>(in.tangent.xyz, 0.0)).xyz;
    out.world_position = world_position.xyz;
    out.tangent_handedness = in.tangent.w;
    out.uv = in.uv;
    out.color = in.color;
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let object = objects[in.instance];
    let sampled = textureSample(base_color_texture, base_color_sampler, in.uv);
    let base = object.base_color * sampled * vec4<f32>(in.color, 1.0);
    let albedo = base.rgb;

    // ── 切线空间法线 ──
    let geometric_normal = normalize(in.world_normal);
    var n = geometric_normal;
    if (object.normal_scale > 0.0) {
        // Gram-Schmidt 重新正交化：插值后的切线未必还垂直于法线。
        let t = normalize(in.world_tangent - geometric_normal * dot(geometric_normal, in.world_tangent));
        let b = cross(geometric_normal, t) * in.tangent_handedness;
        let tbn = mat3x3<f32>(t, b, geometric_normal);

        // 贴图存的是 [0,1]，解回 [-1,1]。
        var tangent_normal = textureSample(normal_texture, base_color_sampler, in.uv).xyz * 2.0 - 1.0;
        tangent_normal = vec3<f32>(tangent_normal.xy * object.normal_scale, tangent_normal.z);
        n = normalize(tbn * tangent_normal);
    }

    // ── 金属度粗糙度贴图（glTF 约定：G 通道粗糙度、B 通道金属度）──
    let mr = textureSample(metallic_roughness_texture, base_color_sampler, in.uv);
    let roughness = clamp(object.roughness * mr.g, 0.02, 1.0);
    let metallic = clamp(object.metallic * mr.b, 0.0, 1.0);

    let occlusion = mix(1.0, textureSample(occlusion_texture, base_color_sampler, in.uv).r, object.occlusion_strength);
    let v = normalize(globals.camera_position.xyz - in.world_position);

    // 逐光源累加。光源数量由 CPU 侧截断到数组容量，这里再夹一次以防越界。
    var color = vec3<f32>(0.0);
    let count = min(globals.light_count.x, 16u);
    for (var i = 0u; i < count; i = i + 1u) {
        let light = globals.lights[i];
        let sample = light_sample_direction(light, in.world_position);
        // 衰减为 0 说明超出作用范围或在聚光锥外，跳过。
        if (sample.w <= 0.0) {
            continue;
        }

        var visibility = 1.0;
        // 只有第一盏光（阴影投射者）参与阴影计算。
        if (i == 0u && globals.shadow_params.w > 0.5) {
            let n_dot_l = max(dot(n, sample.xyz), 0.0);
            // 沿法线推开一点再采样，比纯深度偏移更不容易漏光。
            let offset_position = in.world_position + n * globals.shadow_params.y;
            visibility = shadow_factor(
                shadow_map,
                shadow_sampler,
                globals.light_view_proj,
                offset_position,
                n_dot_l,
                globals.shadow_params.x,
                globals.shadow_params.z,
            );
        }

        color += pbr_direct_lighting(
            n, v, sample.xyz,
            albedo,
            metallic,
            roughness,
            light_radiance(light, sample.w),
        ) * visibility;
    }

    // ── 环境光（IBL）──
    let n_dot_v = max(dot(n, v), 1e-4);
    let f0 = pbr_f0(albedo, metallic);
    let brdf = textureSample(brdf_lut, brdf_sampler, vec2<f32>(n_dot_v, roughness)).rg;

    color += ibl_diffuse(globals.environment, n, albedo, metallic, occlusion);
    color += ibl_specular(
        globals.environment,
        reflect(-v, n),
        roughness,
        f0,
        brdf,
    ) * occlusion;

    // 自发光不受光照影响，直接叠加。
    color += object.emissive.rgb * textureSample(emissive_texture, base_color_sampler, in.uv).rgb;

    // 输出线性 HDR，不做色调映射也不做 gamma——
    // 那些交给后处理链，Bloom 需要未经压缩的高光才能提取出来。
    return vec4<f32>(color, base.a);
}
