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
    // 各级级联的光空间矩阵。用不满的级填单位阵。
    light_view_proj: array<mat4x4<f32>, 4>,
    // x/y/z = 前三级的远距离，w = 实际级数
    cascade_splits: vec4<f32>,
    // x = 深度偏移，y = 法线偏移，z = 阴影贴图边长，w = 是否启用
    shadow_params: vec4<f32>,
    // x = 预滤波环境图的 mip 数（0 表示没有 HDR），其余保留
    ibl_params: vec4<f32>,
    // x/y = 投影矩阵的深度系数，用于把深度缓冲还原成视空间距离
    depth_params: vec4<f32>,
    // x = 启动至今的秒数，y = 上一帧的间隔，zw = 视口宽高（像素）
    //
    // 时间和视口尺寸是自定义材质最常要的两样东西：没有时间做不了流动，
    // 没有视口尺寸算不出屏幕 UV。
    frame_params: vec4<f32>,
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
    // 纹理坐标变换：xy = 缩放，zw = 偏移。图集里取一格子图就靠它。
    uv_transform: vec4<f32>,
    // 反射探针：xyz = 采集点，w = 纹理数组的层号。
    //
    // w = 0 表示这个对象没有探针，用第 0 层（全局环境）且不做视差。
    // 探针是**逐对象**选的，所以一个横跨两个房间的大物体只能用一个
    // 探针——这是前向渲染的常规取舍，办法是把大物体拆开。
    probe_position: vec4<f32>,
    // xyz = 视差盒最小角，w = 是否做视差校正（>0.5 为是）
    probe_min: vec4<f32>,
    // xyz = 视差盒最大角，w = 强度
    probe_max: vec4<f32>,
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
@group(3) @binding(2) var shadow_map: texture_depth_2d_array;
@group(3) @binding(3) var shadow_sampler: sampler_comparison;
// 预滤波的 HDR 环境图（等距柱状投影，带 mip 链）。
// 没有 HDR 时这里绑的是一张 1×1 的占位图，靠 shadow_params 之外的
// 标志位跳过采样——绑空的绑定组在 wgpu 里是非法的。
@group(3) @binding(4) var prefiltered_env: texture_2d_array<f32>;
@group(3) @binding(5) var prefiltered_sampler: sampler;
// 不透明几何与天空画完之后拷出来的一份颜色。
//
// 只对**半透明**物体有意义：不透明 pass 自己还没画完，读到的是上一帧
// 的残留。引擎不拦这件事——拦的话就得为两条路各编译一套着色器。
@group(3) @binding(6) var scene_color_texture: texture_2d<f32>;
// 不透明几何的深度。半透明 pass 用只读深度附件，所以同一张纹理
// 既当深度测试的对象又当采样源。
@group(3) @binding(7) var scene_depth_texture: texture_depth_2d;

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

// ── 给材质钩子用的工具函数 ──

// 采样场景颜色（半透明物体背后的样子）。
//
// `uv` 是屏幕空间坐标，左上 (0,0)、右下 (1,1)。把它偏移一点就是
// **屏幕空间折射**：水面、玻璃看到的背后景物随之扭曲。
//
// 用 `textureLoad` 而不是 `textureSample`：这张图和屏幕是 1:1 的，
// 不需要过滤，而且 `textureSample` 在有分支的代码里会因为求不出
// 屏幕导数而报错。
fn scene_color(uv: vec2<f32>) -> vec3<f32> {
    let size = vec2<f32>(textureDimensions(scene_color_texture));
    let coord = vec2<i32>(clamp(uv, vec2<f32>(0.0), vec2<f32>(1.0)) * size);
    return textureLoad(scene_color_texture, coord, 0).rgb;
}

// 场景深度还原成视空间距离（正数，越远越大）。
//
// 拿它减去自己的深度就是「我和背后的东西隔多远」——水的分层、
// 玻璃的厚度、软边缘都靠这个差值。
fn scene_depth(uv: vec2<f32>) -> f32 {
    let size = vec2<f32>(textureDimensions(scene_depth_texture));
    let coord = vec2<i32>(clamp(uv, vec2<f32>(0.0), vec2<f32>(1.0)) * size);
    return linearize_depth(textureLoad(scene_depth_texture, coord, 0));
}

// 把深度缓冲里的非线性值还原成视空间距离。
//
// 透视投影下 `clip.z = a*z + b`、`clip.w = -z`，所以
// `depth = (a*z+b)/(-z)`，解出 `z = -b/(depth+a)`。
// 系数从投影矩阵里取，正交投影下这个公式不成立（`clip.w` 恒为 1）。
fn linearize_depth(depth: f32) -> f32 {
    let a = globals.depth_params.x;
    let b = globals.depth_params.y;
    let denominator = depth + a;
    // 退化时返回一个很大的值（「背后无穷远」），不能返回 NaN——
    // NaN 会让那个像素变成黑洞，还会顺着 Bloom 扩散开。
    if (abs(denominator) < 1e-9) {
        return 1e9;
    }
    return abs(b / denominator);
}

// 归一化，长度为零时退回一个已知可用的方向。
//
// 钩子返回零向量时 `normalize` 给出 NaN，那个像素连同它周围被 Bloom
// 波及的一片都会变成黑洞。
fn normalize_or_fallback(value: vec3<f32>, fallback: vec3<f32>) -> vec3<f32> {
    let length_squared = dot(value, value);
    if (length_squared < 1e-12) {
        return fallback;
    }
    return value * inverseSqrt(length_squared);
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let object = objects[in.instance];
    // 图集取格：整张图的 UV 是 0..1，缩放到格子大小再偏移到格子位置，
    // 就等于「只采样这一格」。所有贴图槽用同一套变换，否则法线贴图会错位。
    let uv = in.uv * object.uv_transform.xy + object.uv_transform.zw;
    let sampled = textureSample(base_color_texture, base_color_sampler, uv);

    // ── 切线空间法线 ──
    let geometric_normal = normalize(in.world_normal);
    var mapped_normal = geometric_normal;
    if (object.normal_scale > 0.0) {
        // Gram-Schmidt 重新正交化：插值后的切线未必还垂直于法线。
        let t = normalize(in.world_tangent - geometric_normal * dot(geometric_normal, in.world_tangent));
        let b = cross(geometric_normal, t) * in.tangent_handedness;
        let tbn = mat3x3<f32>(t, b, geometric_normal);

        // 贴图存的是 [0,1]，解回 [-1,1]。
        var tangent_normal = textureSample(normal_texture, base_color_sampler, uv).xyz * 2.0 - 1.0;
        tangent_normal = vec3<f32>(tangent_normal.xy * object.normal_scale, tangent_normal.z);
        mapped_normal = normalize(tbn * tangent_normal);
    }

    // ── 金属度粗糙度贴图（glTF 约定：G 通道粗糙度、B 通道金属度）──
    let mr = textureSample(metallic_roughness_texture, base_color_sampler, uv);

    // ── 交给材质钩子 ──
    //
    // 到这里为止是引擎的标准采样结果。钩子可以整个改掉，也可以原样返回。
    // 没有自定义着色器时，`material_surface` 是一个恒等函数，
    // 编译器会把它整个消掉，不产生任何开销。
    var surface: Surface;
    surface.world_position = in.world_position;
    surface.geometric_normal = geometric_normal;
    surface.uv = uv;
    surface.view_direction = normalize(globals.camera_position.xyz - in.world_position);
    // `clip_position` 在片元阶段已经是像素坐标，除以视口尺寸得到 0..1。
    surface.screen_uv = in.clip_position.xy / max(globals.frame_params.zw, vec2<f32>(1.0));
    surface.time = globals.frame_params.x;
    // 和 `scene_depth()` 用同一个还原函数，两者才能直接相减。
    surface.view_depth = linearize_depth(in.clip_position.z);

    surface.base_color = object.base_color * sampled * vec4<f32>(in.color, 1.0);
    surface.normal = mapped_normal;
    surface.metallic = clamp(object.metallic * mr.b, 0.0, 1.0);
    surface.roughness = clamp(object.roughness * mr.g, 0.02, 1.0);
    surface.occlusion = mix(
        1.0,
        textureSample(occlusion_texture, base_color_sampler, uv).r,
        object.occlusion_strength,
    );
    surface.emissive = object.emissive.rgb
        * textureSample(emissive_texture, base_color_sampler, uv).rgb;

    surface = material_surface(surface);

    let base = surface.base_color;
    let albedo = base.rgb;
    // 钩子可能返回没归一化的法线（比如手写的程序化法线）。不归一化的话
    // 光照会整体偏亮或偏暗，而且不报任何错。
    let n = normalize_or_fallback(surface.normal, geometric_normal);
    let roughness = clamp(surface.roughness, 0.02, 1.0);
    let metallic = clamp(surface.metallic, 0.0, 1.0);
    let occlusion = clamp(surface.occlusion, 0.0, 1.0);
    let v = surface.view_direction;

    // 逐光源累加。光源数量由 CPU 侧截断到数组容量，这里再夹一次以防越界。
    var color = vec3<f32>(0.0);
    let count = min(globals.light_count.x, 16u);
    for (var i = 0u; i < count; i = i + 1u) {
        let light = globals.lights[i];

        // 半球光是环境项，不走「入射方向 + BRDF」那条路：它没有方向，
        // 也不产生高光。金属没有漫反射，所以按金属度衰减。
        if (light.position.w == LIGHT_HEMISPHERE) {
            color += light_hemisphere(light, n) * albedo * (1.0 - metallic) * occlusion;
            continue;
        }

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
            // 按到相机的距离选级联。用世界空间距离而不是视空间 z：
            // 视空间 z 在视野边缘会偏小，导致边缘用了过细的级联，
            // 而那一级根本没覆盖到那里——表现为屏幕四角的阴影消失。
            let view_depth = distance(in.world_position, globals.camera_position.xyz);
            let layer = pick_cascade(view_depth, globals.cascade_splits);
            visibility = shadow_factor_cascade(
                shadow_map,
                shadow_sampler,
                globals.light_view_proj[layer],
                layer,
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

    // 有 HDR 就走预滤波的 mip 链，否则退回程序化天空的近似。
    // 保留退路是必要的：不是每个场景都会配 HDR，而没有镜面反射的
    // 金属会变成纯黑。
    var reflection = reflect(-v, n);
    if (globals.ibl_params.x > 0.5) {
        // 视差校正：把反射射线和探针盒求交，用「从采集点看向交点」
        // 的方向去采样。不做的话环境被当成无穷远，室内的金属球
        // 会反射出天空而不是墙。
        if (object.probe_min.w > 0.5) {
            reflection = parallax_correct(
                in.world_position,
                reflection,
                object.probe_min.xyz,
                object.probe_max.xyz,
                object.probe_position.xyz,
            );
        }
        color += ibl_specular_prefiltered(
            prefiltered_env,
            prefiltered_sampler,
            // w = 0 时用第 0 层，也就是全局环境。
            object.probe_position.w,
            globals.ibl_params.x,
            reflection,
            roughness,
            f0,
            brdf,
            globals.environment.sun_color.a * object.probe_max.w,
        ) * occlusion;
    } else {
        color += ibl_specular(globals.environment, reflection, roughness, f0, brdf) * occlusion;
    }

    // ── 雾 ──
    //
    // 放在最后、自发光之前：雾是眼睛和物体之间的介质，它该盖住
    // 一切从物体来的光，包括自发光。放在自发光之后的话，
    // 远处的灯会穿透浓雾清晰可见。
    color = apply_fog(globals.environment, color, length(in.world_position - globals.camera_position.xyz));

    // 自发光不受光照影响，直接叠加。
    // 自发光同样要被雾衰减，所以乘上清澈度。
    let clarity = 1.0 - fog_density(globals.environment, length(in.world_position - globals.camera_position.xyz));
    color += surface.emissive * clarity;

    // 输出线性 HDR，不做色调映射也不做 gamma——
    // 那些交给后处理链，Bloom 需要未经压缩的高光才能提取出来。
    return vec4<f32>(color, base.a);
}
