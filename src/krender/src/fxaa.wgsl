// FXAA（Fast Approximate Anti-Aliasing），Timothy Lottes 的 FXAA 3.11 质量版。
//
// # 为什么选 FXAA
//
// - **MSAA** 只对几何边缘有效，对着色产生的锯齿（高光、法线贴图、alpha 测试）
//   无能为力，而且要在整条管线上开多重采样目标，改动很大。
// - **TAA** 效果最好，但需要历史帧、速度缓冲和一整套重投影，
//   还会带来鬼影，是另一个量级的工作。
// - **FXAA** 是纯后处理：一张 LDR 图进，一张图出。接进现有的后处理链
//   只要多一个 pass。代价是细小的文字和纹理细节会略糊。
//
// # 必须在色调映射之后
//
// FXAA 靠**亮度差**找边缘。HDR 里一个高光可能是 100，色调映射之后是 0.95，
// 直接在 HDR 上跑的话，几乎所有像素对都会被判成边缘。

struct Params {
    // 一个纹素多大：1 / 分辨率。
    texel: vec2<f32>,
    // 边缘判定的绝对阈值。低于它的对比度一律不处理（暗部的噪声）。
    threshold_min: f32,
    // 相对阈值：对比度低于「最亮值 × 它」时不处理。
    threshold_max: f32,
};

@group(0) @binding(0)
var<uniform> params: Params;
@group(0) @binding(1)
var source: texture_2d<f32>;
@group(0) @binding(2)
var source_sampler: sampler;

struct FullscreenOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

// 一个覆盖全屏的三角形。用三角形而不是两个三角形拼的方片：
// 方片的对角线上像素会被光栅化两次。
@vertex
fn fxaa_vs(@builtin(vertex_index) index: u32) -> FullscreenOutput {
    var out: FullscreenOutput;
    let x = f32((index << 1u) & 2u);
    let y = f32(index & 2u);
    out.uv = vec2<f32>(x, y);
    out.clip_position = vec4<f32>(x * 2.0 - 1.0, 1.0 - y * 2.0, 0.0, 1.0);
    return out;
}

// 感知亮度。用 sRGB 的系数而不是简单平均：人眼对绿色最敏感，
// 平均的话绿色边缘会被低估、蓝色边缘会被高估。
fn luma(color: vec3<f32>) -> f32 {
    return dot(color, vec3<f32>(0.299, 0.587, 0.114));
}

fn sample_luma(uv: vec2<f32>, offset: vec2<f32>) -> f32 {
    return luma(textureSample(source, source_sampler, uv + offset * params.texel).rgb);
}

// 沿边缘走多远去找端点。每一步的步长逐渐拉大——
// 近处要精确，远处只是为了确认边缘还在延伸。
const STEPS: i32 = 12;

@fragment
fn fxaa_fs(in: FullscreenOutput) -> @location(0) vec4<f32> {
    let uv = in.uv;
    let center = textureSample(source, source_sampler, uv);

    // ── 1. 取十字邻域的亮度 ──
    let luma_center = luma(center.rgb);
    let luma_down = sample_luma(uv, vec2<f32>(0.0, 1.0));
    let luma_up = sample_luma(uv, vec2<f32>(0.0, -1.0));
    let luma_left = sample_luma(uv, vec2<f32>(-1.0, 0.0));
    let luma_right = sample_luma(uv, vec2<f32>(1.0, 0.0));

    let luma_min = min(luma_center, min(min(luma_down, luma_up), min(luma_left, luma_right)));
    let luma_max = max(luma_center, max(max(luma_down, luma_up), max(luma_left, luma_right)));
    let range = luma_max - luma_min;

    // ── 2. 对比度太低就原样返回 ──
    //
    // 两个阈值缺一不可：只有绝对阈值的话，亮部里很轻微的渐变也会被
    // 当成边缘去糊；只有相对阈值的话，暗部的噪声会被无限放大。
    if (range < max(params.threshold_min, luma_max * params.threshold_max)) {
        return center;
    }

    // ── 3. 取四个角，判断边缘是横的还是竖的 ──
    let luma_dl = sample_luma(uv, vec2<f32>(-1.0, 1.0));
    let luma_ur = sample_luma(uv, vec2<f32>(1.0, -1.0));
    let luma_ul = sample_luma(uv, vec2<f32>(-1.0, -1.0));
    let luma_dr = sample_luma(uv, vec2<f32>(1.0, 1.0));

    let luma_down_up = luma_down + luma_up;
    let luma_left_right = luma_left + luma_right;
    let luma_left_corners = luma_dl + luma_ul;
    let luma_down_corners = luma_dl + luma_dr;
    let luma_right_corners = luma_dr + luma_ur;
    let luma_up_corners = luma_ur + luma_ul;

    let edge_horizontal =
        abs(-2.0 * luma_left + luma_left_corners)
        + abs(-2.0 * luma_center + luma_down_up) * 2.0
        + abs(-2.0 * luma_right + luma_right_corners);
    let edge_vertical =
        abs(-2.0 * luma_up + luma_up_corners)
        + abs(-2.0 * luma_center + luma_left_right) * 2.0
        + abs(-2.0 * luma_down + luma_down_corners);

    let is_horizontal = edge_horizontal >= edge_vertical;

    // ── 4. 挑梯度更陡的那一侧 ──
    var luma1 = select(luma_left, luma_down, is_horizontal);
    var luma2 = select(luma_right, luma_up, is_horizontal);
    let gradient1 = luma1 - luma_center;
    let gradient2 = luma2 - luma_center;
    let is_steepest_1 = abs(gradient1) >= abs(gradient2);
    let gradient_scaled = 0.25 * max(abs(gradient1), abs(gradient2));

    // 沿边缘法线走半个像素。
    var step_length = select(params.texel.x, params.texel.y, is_horizontal);
    var luma_local_average = 0.0;
    if (is_steepest_1) {
        step_length = -step_length;
        luma_local_average = 0.5 * (luma1 + luma_center);
    } else {
        luma_local_average = 0.5 * (luma2 + luma_center);
    }

    var current_uv = uv;
    if (is_horizontal) {
        current_uv.y = current_uv.y + step_length * 0.5;
    } else {
        current_uv.x = current_uv.x + step_length * 0.5;
    }

    // ── 5. 沿边缘往两头走，找边缘的端点 ──
    let offset = select(
        vec2<f32>(0.0, params.texel.y),
        vec2<f32>(params.texel.x, 0.0),
        is_horizontal,
    );

    var uv1 = current_uv - offset;
    var uv2 = current_uv + offset;
    var luma_end1 = luma(textureSample(source, source_sampler, uv1).rgb) - luma_local_average;
    var luma_end2 = luma(textureSample(source, source_sampler, uv2).rgb) - luma_local_average;
    var reached1 = abs(luma_end1) >= gradient_scaled;
    var reached2 = abs(luma_end2) >= gradient_scaled;

    if (!reached1) { uv1 = uv1 - offset; }
    if (!reached2) { uv2 = uv2 + offset; }

    // 步长逐渐拉大：近处要精确，远处只是确认边缘还在延伸。
    // 全用步长 1 的话，一条很长的边要走几十次采样。
    var quality = array<f32, 12>(
        1.0, 1.0, 1.0, 1.0, 1.0, 1.5, 2.0, 2.0, 2.0, 2.0, 4.0, 8.0
    );

    if (!(reached1 && reached2)) {
        for (var i: i32 = 2; i < STEPS; i = i + 1) {
            if (!reached1) {
                luma_end1 = luma(textureSample(source, source_sampler, uv1).rgb)
                    - luma_local_average;
            }
            if (!reached2) {
                luma_end2 = luma(textureSample(source, source_sampler, uv2).rgb)
                    - luma_local_average;
            }
            reached1 = abs(luma_end1) >= gradient_scaled;
            reached2 = abs(luma_end2) >= gradient_scaled;

            if (!reached1) { uv1 = uv1 - offset * quality[i]; }
            if (!reached2) { uv2 = uv2 + offset * quality[i]; }
            if (reached1 && reached2) { break; }
        }
    }

    // ── 6. 按到两端的距离算偏移量 ──
    let distance1 = select(uv.y - uv1.y, uv.x - uv1.x, is_horizontal);
    let distance2 = select(uv2.y - uv.y, uv2.x - uv.x, is_horizontal);
    let is_direction1 = distance1 < distance2;
    let distance_final = min(distance1, distance2);
    let edge_thickness = distance1 + distance2;
    let pixel_offset = -distance_final / max(edge_thickness, 1e-6) + 0.5;

    // 端点那侧的亮度变化方向要和中心一致，否则这不是同一条边。
    let is_luma_center_smaller = luma_center < luma_local_average;
    let correct_variation =
        (select(luma_end2, luma_end1, is_direction1) < 0.0) != is_luma_center_smaller;
    let final_offset = select(0.0, pixel_offset, correct_variation);

    // ── 7. 子像素抖动：处理边缘之外的细节锯齿 ──
    //
    // 光靠边缘检测处理不了「一个像素宽的亮点」——它四周都是边缘。
    // 用邻域的加权平均再补一点偏移。
    let luma_average = (1.0 / 12.0) * (
        2.0 * (luma_down_up + luma_left_right) + luma_left_corners + luma_right_corners
    );
    let sub_pixel_offset1 = clamp(abs(luma_average - luma_center) / max(range, 1e-6), 0.0, 1.0);
    let sub_pixel_offset2 = (-2.0 * sub_pixel_offset1 + 3.0) * sub_pixel_offset1 * sub_pixel_offset1;
    let sub_pixel_offset = sub_pixel_offset2 * sub_pixel_offset2 * 0.75;

    let offset_final = max(final_offset, sub_pixel_offset);

    var final_uv = uv;
    if (is_horizontal) {
        final_uv.y = final_uv.y + offset_final * step_length;
    } else {
        final_uv.x = final_uv.x + offset_final * step_length;
    }
    return textureSample(source, source_sampler, final_uv);
}
