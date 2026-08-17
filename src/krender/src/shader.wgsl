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
};

@group(0) @binding(0) var<uniform> globals: Globals;
// 每个实例一份，用 instance_index 寻址。存储缓冲而非 uniform：
// 一次 draw 就能画完一批同网格同贴图的对象，不必逐个切换动态偏移。
@group(1) @binding(0) var<storage, read> objects: array<ObjectUniforms>;
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
fn vs_main(in: VertexInput, @builtin(instance_index) instance: u32) -> VertexOutput {
    let object = objects[instance];
    let world_position = object.model * vec4<f32>(in.position, 1.0);

    var out: VertexOutput;
    out.instance = instance;
    out.clip_position = globals.view_proj * world_position;
    out.world_position = world_position.xyz;
    out.world_normal = (object.normal_matrix * vec4<f32>(in.normal, 0.0)).xyz;
    // 切线随模型矩阵变换即可，不需要逆转置——它是切向而非法向。
    out.world_tangent = (object.model * vec4<f32>(in.tangent.xyz, 0.0)).xyz;
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
