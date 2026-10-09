// 后处理遮罩：把 `post_mask` 非零的物体画进一张 Rgba8 图。
//
// 四个通道对应遮罩的四位。每个通道写：
//
// | 值 | 意思 |
// |---|---|
// | 1   | 这个像素上能看见它 |
// | 0.5 | 它在这儿，但被别的东西挡住了 |
// | 0   | 不在这儿 |
//
// 分「看得见 / 被挡住」是描边要的：three.js 的 OutlinePass 给两种边
// 两个颜色，被挡住的那段通常画得暗一些，玩家才知道「墙后面有个东西」。
//
// 深度测试是**手动**做的：不挂深度附件，在片元里拿自己的深度和主 pass
// 的深度比。挂附件的话被挡住的片元直接被硬件丢掉，0.5 就无从写起。
//
// 混合用 Max：同一个像素上同一层有好几个面（物体的前后两面），
// 取最「看得见」的那个。

@group(2) @binding(0) var scene_depth: texture_depth_2d;

struct MaskOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) @interpolate(flat) instance: u32,
};

@vertex
fn vs_main(
    in: VertexInput,
    @builtin(vertex_index) vertex_index: u32,
    @builtin(instance_index) instance: u32,
) -> MaskOutput {
    let object = instance_object(instance);
    var position = in.position;
    var normal = in.normal;
    apply_morph(vertex_index, object.skin.y, object.skin.z, object.skin.w, &position, &normal);

    var out: MaskOutput;
    out.clip_position = globals.view_proj * object.model * vec4<f32>(position, 1.0);
    out.instance = instance_object_index(instance);
    return out;
}

@vertex
fn vs_skinned(
    in: VertexInput,
    skin: SkinInput,
    @builtin(vertex_index) vertex_index: u32,
    @builtin(instance_index) instance: u32,
) -> MaskOutput {
    let object = instance_object(instance);
    var position = in.position;
    var normal = in.normal;
    apply_morph(vertex_index, object.skin.y, object.skin.z, object.skin.w, &position, &normal);

    let model = object.model * skin_matrix(skin.joints, skin.weights, object.skin.x);
    var out: MaskOutput;
    out.clip_position = globals.view_proj * model * vec4<f32>(position, 1.0);
    out.instance = instance_object_index(instance);
    return out;
}

@fragment
fn fs_main(in: MaskOutput) -> @location(0) vec4<f32> {
    let bits = objects[in.instance].flags.y;
    let size = vec2<i32>(textureDimensions(scene_depth));
    let pixel = clamp(vec2<i32>(in.clip_position.xy), vec2<i32>(0), size - vec2<i32>(1));
    let depth = textureLoad(scene_depth, pixel, 0);
    // 一点点容差：同一个面在主 pass 和这里各光栅化一次，深度不保证逐位相同。
    let visible = in.clip_position.z <= depth + 1e-5;
    let value = select(0.5, 1.0, visible);
    return vec4<f32>(
        select(0.0, value, (bits & 1u) != 0u),
        select(0.0, value, (bits & 2u) != 0u),
        select(0.0, value, (bits & 4u) != 0u),
        select(0.0, value, (bits & 8u) != 0u),
    );
}
