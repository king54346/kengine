// klight —— 聚簇下标的 WGSL 实现。
//
// 和 `cluster.rs` 里的 `ClusterGrid` **必须算出同一个下标**。对不上的话
// 片元读到的是别的簇的名单——光照在屏幕上整体错位一块，而且不越界、
// 不报错、不掉帧。所以两边都在这个 crate 里，而且有一条测试拿真 GPU
// 跑这一份、拿 CPU 跑那一份，逐个对拍。
//
// 单独一个文件而不是写在渲染器的着色器里，就是为了让那条测试能
// **只编译这一段**：塞在主着色器里的话，测它就得把整套光照、PBR、
// 阴影一起拖进来。

// 视空间深度落在第几片。
//
// 指数划分：近处切得细、远处切得粗——近处才是光源密度最高的地方。
// 均匀切的话近处几片挤在一起，而远处一片能横跨几十米。
//
// `inv_log_ratio` 是 `1 / ln(far / near)`，由 CPU 侧倒好传进来：
// 分母是常数，每个片元再算一次对数纯属浪费。
fn cluster_slice(view_depth: f32, near: f32, inv_log_ratio: f32, slices: u32) -> u32 {
    if (slices == 0u) {
        return 0u;
    }
    let safe_near = max(near, 1e-4);
    // 夹而不是丢：丢掉的话紧贴近平面的那一层会没有光。
    let depth = max(view_depth, safe_near);
    let ratio = log(depth / safe_near) * inv_log_ratio;
    return u32(clamp(ratio * f32(slices), 0.0, f32(slices) - 1.0));
}

// 屏幕坐标 + 视空间深度 → 簇的一维下标。
//
// x 变化最快，和 `ClusterGrid::index` 一致：同一行相邻的像素多半落在
// 相邻的簇里，这样它们读到的名单在内存上也相邻。
fn cluster_index(
    pixel: vec2<f32>,
    viewport: vec2<f32>,
    view_depth: f32,
    tiles: vec2<u32>,
    slices: u32,
    near: f32,
    inv_log_ratio: f32,
) -> u32 {
    if (tiles.x == 0u || tiles.y == 0u || slices == 0u) {
        return 0u;
    }

    // **先乘后除**，不是先除后乘。
    //
    // `pixel / size * tiles` 在分块边界上会差一格：驱动把除法换成
    // 「乘以倒数」之后，`960 * (1/1920)` 是 0.49999997 而不是 0.5，
    // 乘 16 得 7.9999995，取整成 7 而 CPU 那边是 8。
    // 一像素宽的错位不显眼，但那一列的光会少一盏。
    //
    // 先乘的话 `960 * 16 = 15360` 是精确的，再除 1920 也精确得 8。
    let size = max(viewport, vec2<f32>(1.0));
    let tile = vec2<u32>(clamp(
        floor(pixel * vec2<f32>(tiles) / size),
        vec2<f32>(0.0),
        vec2<f32>(tiles) - vec2<f32>(1.0),
    ));

    let slice = cluster_slice(view_depth, near, inv_log_ratio, slices);
    return (slice * tiles.y + tile.y) * tiles.x + tile.x;
}
