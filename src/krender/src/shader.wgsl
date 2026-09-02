// kengine 标准着色器：Cook-Torrance PBR + 基础色贴图。
//
// BRDF 函数（pbr_* 前缀）由 kpbr 提供，光源求值（light_* 前缀）由 klight 提供，
// 渲染器在创建管线时把它们拼接到本文件前面。

@group(2) @binding(0) var base_color_texture: texture_2d<f32>;
@group(2) @binding(1) var base_color_sampler: sampler;
@group(2) @binding(2) var normal_texture: texture_2d<f32>;
@group(2) @binding(3) var metallic_roughness_texture: texture_2d<f32>;
@group(2) @binding(4) var occlusion_texture: texture_2d<f32>;
@group(2) @binding(5) var emissive_texture: texture_2d<f32>;
// 自定义材质贴图。没设的时候绑的是那张 1×1 的白图——
// 「没给贴图」于是等价于「乘 1」，钩子不必为缺图写分支。
// 这正是 bevy 那边 `FallbackImage` 在做的事。
@group(2) @binding(6) var custom_texture0: texture_2d<f32>;
@group(2) @binding(7) var custom_texture1: texture_2d<f32>;
// 自定义纹理数组：一个槽位装很多张同尺寸的图，用整数层号选。
// 没设的时候绑的是那张 1×1 白图的一层数组视图。
//
// 采样时**层号是第四个参数**，不是 UV 的第三个分量：
// `textureSample(custom_texture_array, base_color_sampler, uv, layer)`。
@group(2) @binding(8) var custom_texture_array: texture_2d_array<f32>;
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
// 屏幕空间环境光遮蔽。1 = 完全不遮，0 = 全黑。
//
// 关掉 SSAO 时这里绑的是一张 1×1 的白图——「没有 SSAO」于是等价于
// 「乘 1」，着色器不必为它写分支。和缺贴图时绑白图是同一个套路。
@group(3) @binding(8) var ssao_texture: texture_2d<f32>;


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


// 片元所在的簇。
//
// 真正的公式在 `klight::cluster.wgsl` 里（`cluster_index`），
// 和 CPU 侧的 `ClusterGrid` 是**同一份数学**，两边有一条真跑 GPU 的
// 对拍测试守着。这里只负责把 `Globals` 里那几个字段拆出来喂进去。
//
// 不在这儿重写一遍：重写的那份一旦和 CPU 那份漂移，片元读到的就是
// 别的簇的名单——光照在屏幕上整体错位一块，而且不越界、不报错、不掉帧。
fn cluster_of(pixel: vec2<f32>, view_depth: f32) -> u32 {
    return cluster_index(
        pixel,
        globals.frame_params.zw,
        view_depth,
        globals.cluster_grid.xy,
        globals.cluster_grid.z,
        globals.cluster_depth.x,
        globals.cluster_depth.z,
    );
}

// 一盏光对这个片元的贡献。
//
// 抽成函数是因为它有三个调用点（全局段、簇内、聚簇关着时的全遍历），
// 三处各抄一遍的话，改一处忘两处是迟早的事。
fn shade_light(
    light: Light,
    index: u32,
    n: vec3<f32>,
    v: vec3<f32>,
    albedo: vec3<f32>,
    metallic: f32,
    roughness: f32,
    occlusion: f32,
    world_position: vec3<f32>,
    object_mask: u32,
) -> vec3<f32> {
    // 光照分层：灯和物体两边都得同意。
    if (!light_affects(light, object_mask)) {
        return vec3<f32>(0.0);
    }

    // 半球光是环境项，不走「入射方向 + BRDF」那条路：它没有方向，
    // 也不产生高光。金属没有漫反射，所以按金属度衰减。
    if (light.position.w == LIGHT_HEMISPHERE) {
        return light_hemisphere(light, n) * albedo * (1.0 - metallic) * occlusion;
    }

    let sample = light_sample_direction(light, world_position);
    // 衰减为 0 说明超出作用范围或在聚光锥外。
    if (sample.w <= 0.0) {
        return vec3<f32>(0.0);
    }

    var visibility = 1.0;
    // 只有第一盏光（阴影投射者）参与阴影计算。
    if (index == 0u && globals.shadow_params.w > 0.5) {
        let n_dot_l = max(dot(n, sample.xyz), 0.0);
        // 沿法线推开一点再采样，比纯深度偏移更不容易漏光。
        let offset_position = world_position + n * globals.shadow_params.y;
        // 按到相机的距离选级联。用世界空间距离而不是视空间 z：
        // 视空间 z 在视野边缘会偏小，导致边缘用了过细的级联，
        // 而那一级根本没覆盖到那里——表现为屏幕四角的阴影消失。
        let view_depth = distance(world_position, globals.camera_position.xyz);
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

    return pbr_direct_lighting(
        n, v, sample.xyz,
        albedo,
        metallic,
        roughness,
        light_radiance(light, sample.w),
    ) * visibility;
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
    // 逐对象的自定义参数。整块搬过去，钩子按下标取。
    surface.params = object.params;

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
    // SSAO 乘进 `occlusion` 而不是乘在最终颜色上。
    //
    // 这一条很要紧：`occlusion` 只削弱**环境光**（`pbr_ambient`、
    // `ibl_diffuse`、`ibl_specular` 都乘了它），直射光不受影响。
    // 把 AO 直接乘在最终颜色上——很多引擎图省事的做法——会把太阳照亮
    // 的地方也一起压暗，看着像整个画面蒙了一层灰。
    //
    // 按屏幕坐标取：`position.xy` 就是像素坐标，AO 图和帧缓冲同分辨率，
    // 所以直接 `textureLoad`，不必采样也不必算 UV。
    let ssao = textureLoad(ssao_texture, vec2<i32>(in.clip_position.xy), 0).r;
    let occlusion = clamp(surface.occlusion * ssao, 0.0, 1.0);
    let v = surface.view_direction;

    // ── 逐光源累加 ──
    //
    // 光源数组分成两段：**全局光**（方向光、半球光——没有位置也没有范围）
    // 无条件全遍历；**可聚簇的**（点光源、聚光灯）只遍历自己那个簇的名单。
    //
    // 分段的意义：全局光塞进簇里等于每个簇都有它们，白白占名单；
    // 而点光源不分簇的话，几百盏灯就是每个片元几百次距离计算，
    // 其中绝大多数离这个片元十万八千里。
    var color = vec3<f32>(0.0);
    let object_mask = object.flags.x;
    let global_count = min(globals.light_count.x, globals.light_count.y);

    for (var i = 0u; i < global_count; i = i + 1u) {
        color += shade_light(
            lights[i], i, n, v, albedo, metallic, roughness, occlusion,
            in.world_position, object_mask,
        );
    }

    // 可聚簇的那一段。
    let cluster = cluster_of(in.clip_position.xy, surface.view_depth);
    if (globals.cluster_grid.w > 0u) {
        let range = cluster_ranges[cluster];
        for (var slot = 0u; slot < range.y; slot = slot + 1u) {
            // 名单里存的是「可聚簇那一段」里的下标，要加上全局段的长度。
            let index = global_count + cluster_indices[range.x + slot];
            color += shade_light(
                lights[index], index, n, v, albedo, metallic, roughness, occlusion,
                in.world_position, object_mask,
            );
        }
    } else {
        // 聚簇关着（正交相机）：老老实实全遍历。
        for (var i = global_count; i < globals.light_count.y; i = i + 1u) {
            color += shade_light(
                lights[i], i, n, v, albedo, metallic, roughness, occlusion,
                in.world_position, object_mask,
            );
        }
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
