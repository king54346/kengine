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
    /// 自己到相机的视空间距离。
    ///
    /// 和 `scene_depth(uv)` 是同一个尺度，相减就是「我和背后的东西
    /// 隔多远」——水的分层、玻璃的厚度、软边缘全靠这个差值。
    view_depth: f32,
    /// 自定义材质参数，四个 `vec4` 槽位。
    ///
    /// Rust 侧是 `material.set_param(i, ...)`，标量与 `vec2`/`vec3`
    /// 补零升到 `vec4`。**逐对象**，所以同一批实例可以各带各的值而
    /// 不打断合批。
    ///
    /// 自定义贴图不在这里——那是两个全局变量 `custom_texture0` /
    /// `custom_texture1`，钩子里直接
    /// `textureSample(custom_texture0, base_color_sampler, surface.uv)`。
    params: array<vec4<f32>, 4>,

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

// ── 第二个钩子：光照模型 ──
//
// `material_surface` 覆盖的是「这个表面长什么样」，光照那一整套仍是
// 引擎的。想自己写一套光照模型（卡通、Phong、色阶化、描边）时，
// 覆盖 `material_lighting` 即可：
//
// ```wgsl
// fn material_lighting(surface: ptr<function, Surface>, input: LightingInput) -> vec3<f32> {
//     // 三档色阶的卡通着色
//     let n_dot_l = max(dot((*surface).normal, input.light_direction), 0.0);
//     let banded = floor(n_dot_l * 3.0) / 3.0;
//     return (*surface).base_color.rgb * input.radiance * banded;
// }
// ```
//
// # 引擎还替你做的事
//
// 这正是它和「借道自发光自己算光照」的区别——那条路把下面这些全绕过去了：
//
// | 仍然是引擎的 | |
// |---|---|
// | 阴影 | 你返回之后引擎乘上可见度，**忘不掉** |
// | 光照分层 | 掩码不匹配的灯根本不会调到你 |
// | 聚簇 | 只遍历自己那个簇的名单 |
// | 距离 / 锥衰减、cookie | 已经乘进 `radiance` |
// | IBL、雾、色调映射 | 在你之后照常进行 |
//
// # 半球光不走这里
//
// 它没有方向也不产生高光，是个**环境**项，和 IBL 归在一起由引擎处理。
// 钩子覆盖的是方向光、点光源、聚光灯、矩形面光源这四种直射光。
//
// # 表面为什么是个指针
//
// `surface` 是 `ptr<function, Surface>` 而不是按值传的一份，所以要写
// `(*surface).normal`。丑，但这不是风格问题——`Surface` 有四十多个浮点，
// 而这个函数**每盏灯调一次**。
//
// 第一版是按值传的，实测（256 盏点光源，六次 512² 全渲染，取多次的最小值）：
//
// | | 耗时 |
// |---|---|
// | 钩子化之前 | 267 ms |
// | 按值传 `Surface` | 407 ms |
// | 传指针 | 259 ms |
//
// 也就是按值传要多花一半以上的时间，而传指针和「根本没有这个钩子」
// 一样快。
//
// 结构体里也不能放指针（WGSL 不允许），所以它只能是个单独的参数，
// 没法塞进 `LightingInput`。
//
// 想省事就在函数开头写一句 `let s = *surface;`——那等于把那份拷贝
// 明明白白地要回来，代价你自己知道。
//
// # 两个钩子都是可选的
//
// 只写其中一个就行，没写的那个引擎会补上默认实现。

struct LightingInput {
    /// 这盏灯的原始数据。想自己再取颜色、范围、锥角时用。
    light: Light,
    /// 表面指向光源的单位向量。
    light_direction: vec3<f32>,
    /// 到达这一点的辐射亮度。
    ///
    /// 已经含了：灯的颜色与强度、距离衰减、聚光锥衰减、cookie 图案。
    /// **不含阴影**——阴影由引擎在你返回之后乘上，所以你不必（也不该）
    /// 自己处理。
    radiance: vec3<f32>,
    /// 矩形面光源对这个着色点张成的形状因子，其余光源类型恒为 0。
    ///
    /// 面光源的漫反射该用它代替 `n·l`：贴着板子的表面有半个天空是光源，
    /// 余弦积分接近 1，而指向中心的 `n·l` 可能很小。
    form_factor: f32,
};
