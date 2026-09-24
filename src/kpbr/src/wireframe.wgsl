// 线框：只留下三角形的边，照常受光。
//
// 几何由 `kmesh::Mesh::wireframe` 生成——三角形全部拆开，三个角的 `uv1`
// 分别是 (1,0)、(0,1)、(0,0)，插值之后第三个重心坐标就是 1 - u - v。
// 一个片元离某条边多近，就看它对应的那个重心坐标有多小。
//
// # 线宽按屏幕像素算
//
// 重心坐标是「三角形内的比例」，直接拿它和一个常数比，线宽会随三角形
// 在屏幕上的大小变——远处的球线条糊成一片，近处的细得看不见。
// 除以 `fwidth`（这个量每跨一个像素变多少）就换算成了像素单位，
// 于是远近线宽一致，和 three.js 的 1 像素线框观感相同。
//
// `fwidth` 必须在丢片元之前算：导数要求同一个 2×2 像素块里的四个片元
// 都还活着，先 discard 再求导的结果是未定义的。
//
// # 已知局限
//
// - 阴影贴图和 SSAO 预通道只画几何、不跑表面钩子，线框在那两处是实心的。
//   three.js 的线框同样会投实心的影子，一致。
// - 四边形拆成的两个三角形之间那条对角线也会画出来——three.js 一样。

fn material_surface(surface: Surface) -> Surface {
    var out = surface;
    // params[0].x = 线宽（像素）
    let width = max(surface.params[0].x, 0.01);
    let bary = vec3<f32>(surface.uv1, 1.0 - surface.uv1.x - surface.uv1.y);
    let pixels = bary / max(fwidth(bary), vec3<f32>(1e-6));
    let edge = min(min(pixels.x, pixels.y), pixels.z);
    if (edge > width) {
        discard;
    }
    return out;
}
