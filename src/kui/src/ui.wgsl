// UI 着色器。
//
// 一条管线画完所有 UI 图元：纯色矩形、圆角、描边、文字、贴图。
// 区别全在顶点带的参数里，没有分支之外的开销。

struct Globals {
    // 屏幕尺寸（逻辑像素）。顶点着色器靠它把像素坐标换算成裁剪空间。
    screen: vec2<f32>,
    _padding: vec2<f32>,
};

@group(0) @binding(0)
var<uniform> globals: Globals;

@group(1) @binding(0)
var atlas_texture: texture_2d<f32>;
@group(1) @binding(1)
var atlas_sampler: sampler;

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
    // 片元自己的屏幕坐标。SDF 要用它算到矩形边缘的距离。
    @location(2) position: vec2<f32>,
    @location(3) rect: vec4<f32>,
    @location(4) params: vec2<f32>,
};

@vertex
fn ui_vs(
    @location(0) position: vec2<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) color: vec4<f32>,
    @location(3) rect: vec4<f32>,
    @location(4) params: vec2<f32>,
) -> VertexOutput {
    var out: VertexOutput;

    // 像素坐标（原点左上、y 向下）换成裁剪空间（原点居中、y 向上）。
    // y 不取反的话整个界面会上下颠倒。
    let ndc = vec2<f32>(
        position.x / globals.screen.x * 2.0 - 1.0,
        1.0 - position.y / globals.screen.y * 2.0,
    );
    out.clip_position = vec4<f32>(ndc, 0.0, 1.0);
    out.uv = uv;
    out.color = color;
    out.position = position;
    out.rect = rect;
    out.params = params;
    return out;
}

// 圆角矩形的有符号距离场。
//
// `p` 是相对矩形中心的偏移，`half_size` 是半尺寸，`radius` 是圆角半径。
// 返回值为负表示在内部。
fn rounded_box_sdf(p: vec2<f32>, half_size: vec2<f32>, radius: f32) -> f32 {
    let q = abs(p) - half_size + vec2<f32>(radius);
    return length(max(q, vec2<f32>(0.0))) + min(max(q.x, q.y), 0.0) - radius;
}

@fragment
fn ui_fs(in: VertexOutput) -> @location(0) vec4<f32> {
    var color = in.color * textureSample(atlas_texture, atlas_sampler, in.uv);

    let radius = in.params.x;
    let border = in.params.y;

    // 半径和描边都为零时是普通矩形，不必走 SDF。
    // 文字走的就是这条路——字形的形状来自图集，不是 SDF。
    if (radius > 0.0 || border > 0.0) {
        let center = in.rect.xy;
        let half_size = in.rect.zw;
        let distance = rounded_box_sdf(in.position - center, half_size, radius);

        // 用一个像素宽的过渡带做抗锯齿。`fwidth` 拿到的是屏幕空间的
        // 变化率，所以这条边在任何缩放下都是一像素宽，不会随尺寸变粗。
        let aa = fwidth(distance);
        var coverage = 1.0 - smoothstep(-aa, aa, distance);

        if (border > 0.0) {
            // 描边 = 外轮廓减去内轮廓。内轮廓整体往里缩一个描边宽度。
            let inner = distance + border;
            coverage = coverage - (1.0 - smoothstep(-aa, aa, inner));
        }
        color.a = color.a * clamp(coverage, 0.0, 1.0);
    }

    // 预乘 alpha 输出，与混合状态一致。
    return vec4<f32>(color.rgb * color.a, color.a);
}
