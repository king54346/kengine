// 深度／法线预通道。
//
// 在主 pass **之前**把整个场景的世界法线渲进一张离屏纹理，同时填一份
// 自己的深度。SSAO 要的就是这一对——它得知道「屏幕上这一点的表面朝哪、
// 离相机多远」，而那是主 pass 画自己时才有的信息，别人拿不到。
//
// # 为什么不复用主 pass 的深度
//
// 顺序上做得到（预通道在前），但那意味着主 pass 得从 `Less` 改成
// `LessEqual` 并且 `LoadOp::Load`——而两条 pass 的顶点变换必须
// **逐位相同**才安全。同一段 WGSL 编进两个模块，驱动的优化不保证一致，
// 差一个 ULP 就会在物体表面上抠出一片洞。
//
// 所以预通道自己带一份深度，代价是几何走两遍。**只有开了 SSAO 才跑**，
// 关着的时候一分钱不花。将来真要省这一遍，那是一次独立的、有回归风险的
// 改动，不该和「把 SSAO 做出来」混在一起。
//
// # 绑定只用 group(0) 和 group(1)
//
// `Globals` 和 `ObjectUniforms` 来自 `geometry.wgsl`，渲染器把它拼在
// 这段前面——和主着色器、阴影 pass 用的是同一份声明。
// 贴图那两组（group 2/3）这里根本用不到，所以预通道有自己的一条更窄的
// 管线布局：wgpu 要求管线布局里的每个组在绘制时都被 set，
// 沿用主 pass 的布局就得为它准备一套用不上的贴图绑定组。
//
// # 三张输出
//
// | 附件 | 格式 | 内容 |
// |---|---|---|
// | 0 | Rgba16Float | xyz = 世界法线，w = 金属度 |
// | 1 | Rg16Float | 运动向量：本帧 NDC − 上一帧 NDC，都不带抖动 |
// | 2 | Rgba8Unorm | rgb = 基础色，a = 粗糙度 |
//
// 后两张是给后处理的：TAA、运动模糊要运动向量，屏幕空间反射要知道
// 哪儿是镜面（金属度、粗糙度），屏幕空间全局光照要知道反弹出去的光
// 被染成什么颜色（基础色）。
//
// # 顶点钩子
//
// 两个顶点入口和主 pass 一样经过 `material_vertex`：位移出来的形状（海浪、置换贴图、
// 风吹的草）在预通道里也是那个形状，SSAO、运动向量才对得上。没写顶点钩子的材质拼进来的是
// 恒等的默认钩子；写了的，这段源码拼在那个材质自己的着色器后面编（见 `material_shader_source`），
// 入口名带 `prepass_` 前缀就是为了不和主着色器的 `vs_main` / `fs_main` 撞名。
//
// 基础色和粗糙度取的是**材质参数**，不采贴图：采贴图得把 group(2) 也
// 绑进来，预通道就不再是「只有几何」的窄管线了。带贴图的物体在这张图
// 里是它的因子色——对 SSGI 的「反弹染色」来说够用，对需要逐像素粗糙度
// 的 SSR 就是个近似，文档里写明了。

struct PrepassOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) world_normal: vec3<f32>,
    // 不带抖动的本帧 / 上一帧裁剪坐标。在片元里才做透视除法：
    // 插值裁剪坐标再除是对的，插值除过的 NDC 在透视下是错的。
    @location(1) current_clip: vec4<f32>,
    @location(2) previous_clip: vec4<f32>,
    @location(3) @interpolate(flat) instance: u32,
    // 实例颜色（乘进材质目标里的基础色）。
    @location(4) @interpolate(flat) instance_color: vec4<f32>,
};

struct PrepassTargets {
    @location(0) normal: vec4<f32>,
    @location(1) velocity: vec2<f32>,
    @location(2) material: vec4<f32>,
};

// 顶点钩子：和主 pass 的 `vs_main` 同样的填法。
fn prepass_vertex_hook(in: VertexInput, object: ObjectUniforms, instance: u32, position: ptr<function, vec3<f32>>, normal: ptr<function, vec3<f32>>) {
    var vertex_surface: VertexSurface;
    vertex_surface.uv = in.uv;
    vertex_surface.uv1 = in.uv1;
    vertex_surface.tangent = in.tangent;
    vertex_surface.color = in.color;
    vertex_surface.time = globals.frame_params.x;
    vertex_surface.params = object.params;
    vertex_surface.model = object.model;
    vertex_surface.instance_data = instance_of(instance).data;
    vertex_surface.position = *position;
    vertex_surface.normal = *normal;
    vertex_surface = material_vertex(vertex_surface);
    *position = vertex_surface.position;
    *normal = vertex_surface.normal;
}

@vertex
fn prepass_vs(
    in: VertexInput,
    @builtin(vertex_index) vertex_index: u32,
    @builtin(instance_index) instance: u32,
) -> PrepassOutput {
    let object = instance_object(instance);

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
    prepass_vertex_hook(in, object, instance, &position, &normal);

    let world_position = object.model * vec4<f32>(position, 1.0);
    // 形变权重没有上一帧的那份，用本帧的——表情动画的运动向量因此是零。
    // 要做准得多存一份权重，而表情的位移通常小到 TAA 自己兜得住。
    let previous_position = object.prev_model * vec4<f32>(position, 1.0);

    var out: PrepassOutput;
    out.clip_position = globals.view_proj * world_position;
    out.world_normal = (object.normal_matrix * vec4<f32>(normal, 0.0)).xyz;
    out.current_clip = globals.clip_view_proj * world_position;
    out.previous_clip = globals.prev_view_proj * previous_position;
    out.instance = instance_object_index(instance);
    out.instance_color = instance_of(instance).color;
    return out;
}

@vertex
fn prepass_vs_skinned(
    in: VertexInput,
    skin: SkinInput,
    @builtin(vertex_index) vertex_index: u32,
    @builtin(instance_index) instance: u32,
) -> PrepassOutput {
    let object = instance_object(instance);

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
    prepass_vertex_hook(in, object, instance, &position, &normal);

    // 和主 pass 同一个公式，包括那句「蒙皮网格的 model 是单位阵但仍然乘上」。
    // 两边写法不同的话，蒙皮物体的法线会和它自己的着色对不上。
    let model = object.model * skin_matrix(skin.joints, skin.weights, object.skin.x);
    let world_position = model * vec4<f32>(position, 1.0);
    // 上一帧的骨骼矩阵拼在同一个缓冲里，起点在 `flags.z`。
    let previous_model = object.prev_model * skin_matrix(skin.joints, skin.weights, object.flags.z);
    let previous_position = previous_model * vec4<f32>(position, 1.0);

    var out: PrepassOutput;
    out.clip_position = globals.view_proj * world_position;
    out.world_normal = (model * vec4<f32>(normal, 0.0)).xyz;
    out.current_clip = globals.clip_view_proj * world_position;
    out.previous_clip = globals.prev_view_proj * previous_position;
    out.instance = instance_object_index(instance);
    out.instance_color = instance_of(instance).color;
    return out;
}

@fragment
fn prepass_fs(in: PrepassOutput) -> PrepassTargets {
    let object = tinted_object(in.instance, in.instance_color);
    var out: PrepassTargets;
    // 存**世界**法线，范围 [-1, 1]，所以目标格式必须是浮点的
    // （Rgba16Float）。压进 Unorm 要先编码到 [0,1] 再解开，
    // 8 位精度下 SSAO 的半球采样会在平面上抖出一圈圈条纹。
    //
    // 这里不归一化：光栅化的插值会让法线变短，但下游自己会归一。
    // 在这里归一等于每个像素多一次平方根，而下游反正还要再算一次。
    out.normal = vec4<f32>(in.world_normal, clamp(object.metallic, 0.0, 1.0));

    // w 可能是 0 或负（贴着近平面的顶点插值出来的），除之前兜一下。
    let current = in.current_clip.xy / max(in.current_clip.w, 1e-6);
    let previous = in.previous_clip.xy / max(in.previous_clip.w, 1e-6);
    out.velocity = current - previous;

    out.material = vec4<f32>(
        clamp(object.base_color.rgb, vec3<f32>(0.0), vec3<f32>(1.0)),
        clamp(object.roughness, 0.0, 1.0),
    );
    return out;
}
