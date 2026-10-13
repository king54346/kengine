// 带钩子的材质的阴影深度 pass。
//
// 普通物体走 `shadow_pass.wgsl`：只要模型矩阵，绑定也窄，没有片元阶段。两种材质要走这里：
//
// - 写了 `material_vertex` 的（海浪、置换贴图、风吹的草）：位移出来的形状得进阴影，
//   否则影子还是没位移的那个。
// - 写了 `material_surface` 而且里面 `discard` 的（镂空、噪声挖洞、按 alpha 剪裁）：
//   挖掉的地方影子也得透光。片元阶段把材质的表面钩子跑一遍，钩子 discard 了这一点就不写深度。
//
// 这段拼在那个材质自己的着色器后面编（见 `material_shader_source`），于是钩子、钩子要采的
// 材质贴图（group 2）都在。
//
// 绑定：
//   group(0) binding(0)  本层的光空间矩阵（和普通阴影 pass 同一个缓冲、同一个动态偏移）
//   group(1)             主 pass 的逐对象数据（`objects`、骨骼、形变）：阴影 pass 的实例顺序
//                        和主 pass 完全一致，下标直接通用，还带着钩子要的 `params`
//   group(2)             材质贴图
//
// `hooked_shadow_globals` 和 `geometry.wgsl` 的 `globals` 声明在同一个 (group, binding) 上。
// WGSL 只要求**同一个入口**用到的资源不重号——这里的入口只碰前者。代价是：
// 钩子不能读 `globals`、`scene_color` 这类主 pass 才有的东西（时间从 `vertex.time` / `surface.time` 拿）；
// 读了的话这套管线建不出来，渲染器会退回普通阴影（影子没位移、不镂空，但不会崩）。

struct HookedShadowGlobals {
    light_view_proj: mat4x4<f32>,
    params: vec4<f32>,
    // x = 秒（和主 pass 的 `globals.frame_params.x` 同一个时钟）
    frame: vec4<f32>,
};

@group(0) @binding(0) var<uniform> hooked_shadow_globals: HookedShadowGlobals;

struct HookedShadowOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) world_position: vec3<f32>,
    @location(1) world_normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) uv1: vec2<f32>,
    @location(4) @interpolate(flat) instance: u32,
    // 实例的颜色和自定义数据（实例化节点；普通物体是白色和 0）。片元阶段不读实例缓冲，靠这两项。
    @location(5) @interpolate(flat) instance_color: vec4<f32>,
    @location(6) @interpolate(flat) instance_data: vec4<f32>,
    // 顶点色：主 pass 的底色是 `材质底色 × 底色贴图 × 顶点色`，这里照乘。
    @location(7) color: vec3<f32>,
};

// 钩子改过的 uv 写回 `in`，镂空钩子在片元里看到的和主画面同一套 uv。
fn hooked_shadow_vertex(in: ptr<function, VertexInput>, object: ObjectUniforms, instance: u32, position: ptr<function, vec3<f32>>, normal: ptr<function, vec3<f32>>) {
    var vertex_surface: VertexSurface;
    vertex_surface.uv = (*in).uv;
    vertex_surface.uv1 = (*in).uv1;
    vertex_surface.tangent = (*in).tangent;
    vertex_surface.color = (*in).color;
    vertex_surface.time = hooked_shadow_globals.frame.x;
    vertex_surface.params = object.params;
    vertex_surface.model = object.model;
    vertex_surface.instance_data = instance_of(instance).data;
    vertex_surface.position = *position;
    vertex_surface.normal = *normal;
    vertex_surface = material_vertex(vertex_surface);
    *position = vertex_surface.position;
    *normal = vertex_surface.normal;
    (*in).uv = vertex_surface.uv;
    (*in).uv1 = vertex_surface.uv1;
}

fn hooked_shadow_output(in: VertexInput, model: mat4x4<f32>, position: vec3<f32>, normal: vec3<f32>, instance: u32) -> HookedShadowOutput {
    let world = model * vec4<f32>(position, 1.0);
    var out: HookedShadowOutput;
    out.clip_position = hooked_shadow_globals.light_view_proj * world;
    out.world_position = world.xyz;
    out.world_normal = (model * vec4<f32>(normal, 0.0)).xyz;
    out.uv = in.uv;
    out.uv1 = in.uv1;
    out.instance = instance_object_index(instance);
    let extra = instance_of(instance);
    out.instance_color = extra.color;
    out.instance_data = extra.data;
    out.color = in.color;
    return out;
}

@vertex
fn shadow_hooked_vs(
    in: VertexInput,
    @builtin(vertex_index) vertex_index: u32,
    @builtin(instance_index) instance: u32,
) -> HookedShadowOutput {
    let object = instance_object(instance);
    var position = in.position;
    var normal = in.normal;
    apply_morph(vertex_index, object.skin.y, object.skin.z, object.skin.w, &position, &normal);
    var vertex = in;
    hooked_shadow_vertex(&vertex, object, instance, &position, &normal);
    return hooked_shadow_output(vertex, object.model, position, normal, instance);
}

@vertex
fn shadow_hooked_skinned_vs(
    in: VertexInput,
    skin: SkinInput,
    @builtin(vertex_index) vertex_index: u32,
    @builtin(instance_index) instance: u32,
) -> HookedShadowOutput {
    let object = instance_object(instance);
    var position = in.position;
    var normal = in.normal;
    apply_morph(vertex_index, object.skin.y, object.skin.z, object.skin.w, &position, &normal);
    var vertex = in;
    hooked_shadow_vertex(&vertex, object, instance, &position, &normal);
    let model = object.model * skin_matrix(skin.joints, skin.weights, object.skin.x);
    return hooked_shadow_output(vertex, model, position, normal, instance);
}

// 镂空：把材质的表面钩子跑一遍，钩子里 discard 了就不写深度。不写颜色（深度 pass 没有颜色附件）。
// 和主 pass 不一样的字段（视线方向、屏幕坐标、视空间深度）这里给的是近似值——光源视角下它们没有意义，
// 剪裁一般也不看它们。
@fragment
fn shadow_hooked_fs(in: HookedShadowOutput) {
    let object = tinted_object(in.instance, in.instance_color);
    var surface: Surface;
    surface.world_position = in.world_position;
    surface.geometric_normal = normalize(in.world_normal);
    surface.front_facing = true;
    surface.uv = in.uv * object.uv_transform.xy + object.uv_transform.zw;
    surface.uv1 = in.uv1;
    surface.view_direction = surface.geometric_normal;
    surface.screen_uv = vec2<f32>(0.0);
    surface.time = hooked_shadow_globals.frame.x;
    surface.view_depth = 0.0;
    surface.tangent = vec3<f32>(1.0, 0.0, 0.0);
    surface.bitangent = vec3<f32>(0.0, 0.0, 1.0);
    surface.params = object.params;
    surface.instance_data = in.instance_data;
    // 底色要和主 pass 一样乘上底色贴图：按 alpha 剪裁的钩子（`Physical::alpha_cutoff`、头发、羽毛、
    // 树叶）判断的就是 `base_color.a`。只给材质常量的话 alpha 恒为 1，影子一个像素都裁不掉——
    // 镂空的翅膀投下来的是整块实心的影子。
    let sampled = textureSample(base_color_texture, base_color_sampler, surface.uv);
    surface.base_color = object.base_color * sampled * vec4<f32>(in.color, 1.0);
    surface.normal = surface.geometric_normal;
    surface.metallic = object.metallic;
    surface.roughness = object.roughness;
    surface.occlusion = 1.0;
    surface.emissive = vec3<f32>(0.0);
    surface = material_surface(surface);
}

// 镂空材质的预通道：和阴影一样先把表面钩子跑一遍，钩子 discard 了这一点就不写深度、法线、运动向量。
// 不跑的话预通道里是整块面片：树叶透明的那一圈挡住了后面的地面，TAA 在那里拿叶子的运动向量去对齐
// 历史，镜头一平移，叶子、布帘的边缘就拖影、抖动；SSAO 也会在空处算出一圈遮蔽。
// 这里在主 pass 的相机下，视线方向、屏幕坐标都是真的。
@fragment
fn prepass_cutout_fs(in: PrepassOutput, @builtin(front_facing) front_facing: bool) -> PrepassTargets {
    let object = tinted_object(in.instance, in.instance_color);
    var surface: Surface;
    surface.world_position = in.world_position;
    let normal = normalize(in.world_normal);
    surface.geometric_normal = select(-normal, normal, front_facing);
    surface.front_facing = front_facing;
    surface.uv = in.uv * object.uv_transform.xy + object.uv_transform.zw;
    surface.uv1 = in.uv1;
    surface.view_direction = normalize(globals.camera_position.xyz - in.world_position);
    surface.screen_uv = in.clip_position.xy / max(globals.frame_params.zw, vec2<f32>(1.0));
    surface.time = globals.frame_params.x;
    surface.view_depth = linearize_depth(in.clip_position.z);
    surface.tangent = vec3<f32>(1.0, 0.0, 0.0);
    surface.bitangent = vec3<f32>(0.0, 0.0, 1.0);
    surface.params = object.params;
    surface.instance_data = in.instance_data;
    // 和阴影入口同理：按 alpha 裁的钩子看的是乘过底色贴图的 `base_color.a`。
    let sampled = textureSample(base_color_texture, base_color_sampler, surface.uv);
    surface.base_color = object.base_color * sampled * vec4<f32>(in.color, 1.0);
    surface.normal = surface.geometric_normal;
    surface.metallic = object.metallic;
    surface.roughness = object.roughness;
    surface.occlusion = 1.0;
    surface.emissive = vec3<f32>(0.0);
    surface = material_surface(surface);
    return prepass_targets(in, front_facing);
}
