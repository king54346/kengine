// 材质钩子的契约。
//
// # 为什么是「钩子」而不是「整份着色器」
//
// 让用户交一整份着色器的话，他得连 PBR、级联阴影、IBL、雾一起重写一遍，
// 而且引擎这边任何一次 `Globals` 布局改动都会把所有自定义着色器打挂。
//
// 钩子只让用户覆盖**表面属性**——颜色、法线、金属度、粗糙度、自发光。
// 光照那一整套照旧，引擎改它不影响任何自定义材质。
//
// # 输入输出用同一个结构体
//
// 引擎先把标准的采样结果填进 `Surface`，再交给 `material_surface`。
// 用户改自己关心的那几项、原样返回其余的：
//
// ```wgsl
// fn material_surface(surface: Surface) -> Surface {
//     var out = surface;
//     out.base_color = vec4<f32>(fract(surface.world_position), 1.0);
//     return out;
// }
// ```
//
// 分成 in / out 两个结构体的话，用户每写一个材质都得手抄一遍
// 「其余字段照搬」，抄漏一个就是那项功能静默失效。

struct Surface {
    // ── 只读：几何与环境 ──
    //
    // 改这些不会有效果，引擎用的是自己那份。

    /// 世界空间位置。
    world_position: vec3<f32>,
    /// 插值后的几何法线（世界空间，已归一化）。
    geometric_normal: vec3<f32>,
    /// 已经过 `uv_transform` 的纹理坐标。
    uv: vec2<f32>,
    /// 从表面指向相机的单位向量。
    view_direction: vec3<f32>,
    /// 屏幕空间坐标，左上角 (0,0)、右下角 (1,1)。
    screen_uv: vec2<f32>,
    /// 引擎启动至今的秒数。做流动、闪烁、波纹都要它。
    time: f32,

    // ── 可写：表面属性 ──

    /// 基础色，a 是不透明度。
    base_color: vec4<f32>,
    /// 最终法线（世界空间）。默认已经算好了法线贴图。
    normal: vec3<f32>,
    /// 金属度。
    metallic: f32,
    /// 粗糙度。
    roughness: f32,
    /// 环境光遮蔽。
    occlusion: f32,
    /// 自发光，不受光照影响。
    emissive: vec3<f32>,
};
