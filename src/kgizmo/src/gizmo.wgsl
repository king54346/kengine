// 调试线着色器。
//
// 简单到几乎没有内容：把世界坐标变到裁剪空间，颜色原样传下去。
// 没有光照、没有贴图、没有雾——调试线要的是「一眼看清」，
// 任何着色都只会让它更难认。

struct Globals {
    view_proj: mat4x4<f32>,
};

@group(0) @binding(0)
var<uniform> globals: Globals;

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) color: vec4<f32>,
};

@vertex
fn gizmo_vs(
    @location(0) position: vec3<f32>,
    @location(1) color: vec4<f32>,
) -> VertexOutput {
    var out: VertexOutput;
    out.clip_position = globals.view_proj * vec4<f32>(position, 1.0);
    out.color = color;
    return out;
}

@fragment
fn gizmo_fs(in: VertexOutput) -> @location(0) vec4<f32> {
    // 颜色已经是线性值，直接输出到 HDR 目标；后处理会统一做色调映射。
    // alpha 走的是常规混合，所以这里要预乘。
    return vec4<f32>(in.color.rgb * in.color.a, in.color.a);
}
