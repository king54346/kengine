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

struct PrepassOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) world_normal: vec3<f32>,
};

@vertex
fn vs_main(
    in: VertexInput,
    @builtin(vertex_index) vertex_index: u32,
    @builtin(instance_index) instance: u32,
) -> PrepassOutput {
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

    var out: PrepassOutput;
    out.clip_position = globals.view_proj * world_position;
    out.world_normal = (object.normal_matrix * vec4<f32>(normal, 0.0)).xyz;
    return out;
}

@vertex
fn vs_skinned(
    in: VertexInput,
    skin: SkinInput,
    @builtin(vertex_index) vertex_index: u32,
    @builtin(instance_index) instance: u32,
) -> PrepassOutput {
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

    // 和主 pass 同一个公式，包括那句「蒙皮网格的 model 是单位阵但仍然乘上」。
    // 两边写法不同的话，蒙皮物体的法线会和它自己的着色对不上。
    let model = object.model * skin_matrix(skin.joints, skin.weights, object.skin.x);
    let world_position = model * vec4<f32>(position, 1.0);

    var out: PrepassOutput;
    out.clip_position = globals.view_proj * world_position;
    out.world_normal = (model * vec4<f32>(normal, 0.0)).xyz;
    return out;
}

@fragment
fn fs_main(in: PrepassOutput) -> @location(0) vec4<f32> {
    // 存**世界**法线，范围 [-1, 1]，所以目标格式必须是浮点的
    // （Rgba16Float）。压进 Unorm 要先编码到 [0,1] 再解开，
    // 8 位精度下 SSAO 的半球采样会在平面上抖出一圈圈条纹。
    //
    // 这里不归一化：光栅化的插值会让法线变短，但 SSAO 那边自己会归一。
    // 在这里归一等于每个像素多一次平方根，而下游反正还要再算一次。
    return vec4<f32>(in.world_normal, 1.0);
}
