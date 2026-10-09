// 形态学抗锯齿：SMAA 1x 的三步，面积用解析式代替查找表。
//
//   edges     找边：亮度差超过阈值、且不比邻近的其他差小太多（局部对比度自适应）
//   weights   对每段边沿着边搜两头，看两端往哪边拐，算出「真实的边」穿过这个
//             像素时切掉了多少面积
//   blend     按面积和上下左右的邻居混合
//
// # 和原版 SMAA 的差别
//
// 原版第二步查两张预计算的表（AreaTex、SearchTex），能认出更多形状
// （对角线、拐角锐化）。这里只处理横平竖直的 L / Z / U 形——也就是 MLAA
// 的那一套——面积用直线方程在像素中心处的高度直接算。对「楼梯状」的
// 斜边这已经是主要的收益；对角线细节和文字的拐角会比原版略糊。

const SMAA_THRESHOLD: f32 = 0.08;
const SMAA_MAX_SEARCH: i32 = 16;

fn smaa_luma(pixel: vec2<i32>) -> f32 {
    let size = vec2<i32>(textureDimensions(input_texture));
    let p = clamp(pixel, vec2<i32>(0), size - vec2<i32>(1));
    // 在感知空间里比：链上存的是线性值，暗部一点点差在线性里微不足道，
    // 在屏幕上却是看得见的台阶。原版 SMAA 也是在 gamma 空间找边。
    return luminance(linear_to_srgb(textureLoad(input_texture, p, 0).rgb));
}

// ── 第一步：边 ──
// r = 和左边之间有边，g = 和上面之间有边。

@fragment
fn edges(in: PostVertex) -> @location(0) vec4<f32> {
    let p = vec2<i32>(in.position.xy);
    let l = smaa_luma(p);
    let left = abs(l - smaa_luma(p + vec2<i32>(-1, 0)));
    let top = abs(l - smaa_luma(p + vec2<i32>(0, -1)));
    var edge = step(vec2<f32>(SMAA_THRESHOLD), vec2<f32>(left, top));
    if (edge.x + edge.y == 0.0) {
        return vec4<f32>(0.0);
    }

    // 局部对比度自适应：这条边要是明显比周围别的差小，就是个次要的边
    // （比如一道亮边旁边的暗纹），丢掉。原版 SMAA 的系数是 2。
    let right = abs(l - smaa_luma(p + vec2<i32>(1, 0)));
    let bottom = abs(l - smaa_luma(p + vec2<i32>(0, 1)));
    let left_left = abs(smaa_luma(p + vec2<i32>(-1, 0)) - smaa_luma(p + vec2<i32>(-2, 0)));
    let top_top = abs(smaa_luma(p + vec2<i32>(0, -1)) - smaa_luma(p + vec2<i32>(0, -2)));
    let local_max = max(max(max(left, top), max(right, bottom)), max(left_left, top_top));
    edge *= step(vec2<f32>(local_max * 0.5), vec2<f32>(left, top));
    return vec4<f32>(edge, 0.0, 1.0);
}

fn edge_at(pixel: vec2<i32>) -> vec2<f32> {
    let size = vec2<i32>(textureDimensions(t0));
    if (any(pixel < vec2<i32>(0)) || any(pixel >= size)) {
        return vec2<f32>(0.0);
    }
    return textureLoad(t0, pixel, 0).rg;
}

// 一段线的解析面积：线从左端 (x0, h0) 到右端 (x1, h1)，按 MLAA 的规则：
// 两头往**相反**方向拐（Z 形）是一条斜线；往**同一**方向拐（U 形）是
// 折到中点的两段；只有一头拐（L 形）是从那一头斜到另一头。
// 返回像素中心 x = c 处线的高度：正 = 边在像素**外侧**（上/左）一侧。
fn line_height(c: f32, x0: f32, h0: f32, x1: f32, h1: f32) -> f32 {
    if (h0 == 0.0 && h1 == 0.0) {
        return 0.0;
    }
    if (h0 != 0.0 && h1 != 0.0 && sign(h0) == sign(h1)) {
        let middle = 0.5 * (x0 + x1);
        if (c < middle) {
            return mix(h0, 0.0, (c - x0) / max(middle - x0, 1e-4));
        }
        return mix(0.0, h1, (c - middle) / max(x1 - middle, 1e-4));
    }
    return mix(h0, h1, (c - x0) / max(x1 - x0, 1e-4));
}

// ── 第二步：面积 ──
// xy = 这个像素上边那条边：x = 本像素往上混多少，y = 上面那个像素往下混多少；
// zw = 左边那条边：z = 本像素往左混多少，w = 左边那个像素往右混多少。

@fragment
fn weights(in: PostVertex) -> @location(0) vec4<f32> {
    let p = vec2<i32>(in.position.xy);
    let e = edge_at(p);
    var out = vec4<f32>(0.0);

    // 上边那条（水平边）：沿 x 搜。
    if (e.y > 0.0) {
        var left = 0;
        for (var i = 1; i <= SMAA_MAX_SEARCH; i = i + 1) {
            if (edge_at(p + vec2<i32>(-i, 0)).y == 0.0) {
                break;
            }
            left = i;
        }
        var right = 0;
        for (var i = 1; i <= SMAA_MAX_SEARCH; i = i + 1) {
            if (edge_at(p + vec2<i32>(i, 0)).y == 0.0) {
                break;
            }
            right = i;
        }
        // 两端往哪边拐：端点那一列上，竖边在上一行（往上拐）还是本行（往下拐）。
        let left_end = p + vec2<i32>(-left, 0);
        let right_end = p + vec2<i32>(right + 1, 0);
        let h0 = select(0.0, 0.5, edge_at(left_end + vec2<i32>(0, -1)).x > 0.0)
            - select(0.0, 0.5, edge_at(left_end).x > 0.0);
        let h1 = select(0.0, 0.5, edge_at(right_end + vec2<i32>(0, -1)).x > 0.0)
            - select(0.0, 0.5, edge_at(right_end).x > 0.0);
        let h = line_height(f32(left) + 0.5, 0.0, h0, f32(left + right + 1), h1);
        out.x = max(-h, 0.0);
        out.y = max(h, 0.0);
    }

    // 左边那条（竖直边）：沿 y 搜。
    if (e.x > 0.0) {
        var up = 0;
        for (var i = 1; i <= SMAA_MAX_SEARCH; i = i + 1) {
            if (edge_at(p + vec2<i32>(0, -i)).x == 0.0) {
                break;
            }
            up = i;
        }
        var down = 0;
        for (var i = 1; i <= SMAA_MAX_SEARCH; i = i + 1) {
            if (edge_at(p + vec2<i32>(0, i)).x == 0.0) {
                break;
            }
            down = i;
        }
        let top_end = p + vec2<i32>(0, -up);
        let bottom_end = p + vec2<i32>(0, down + 1);
        let h0 = select(0.0, 0.5, edge_at(top_end + vec2<i32>(-1, 0)).y > 0.0)
            - select(0.0, 0.5, edge_at(top_end).y > 0.0);
        let h1 = select(0.0, 0.5, edge_at(bottom_end + vec2<i32>(-1, 0)).y > 0.0)
            - select(0.0, 0.5, edge_at(bottom_end).y > 0.0);
        let h = line_height(f32(up) + 0.5, 0.0, h0, f32(up + down + 1), h1);
        out.z = max(-h, 0.0);
        out.w = max(h, 0.0);
    }
    return out;
}

// ── 第三步：混合 ──

fn weight_at(pixel: vec2<i32>) -> vec4<f32> {
    let size = vec2<i32>(textureDimensions(t1));
    if (any(pixel < vec2<i32>(0)) || any(pixel >= size)) {
        return vec4<f32>(0.0);
    }
    return textureLoad(t1, pixel, 0);
}

fn color_at(pixel: vec2<i32>) -> vec4<f32> {
    let size = vec2<i32>(textureDimensions(input_texture));
    return textureLoad(input_texture, clamp(pixel, vec2<i32>(0), size - vec2<i32>(1)), 0);
}

@fragment
fn blend(in: PostVertex) -> @location(0) vec4<f32> {
    let p = vec2<i32>(in.position.xy);
    let own = weight_at(p);
    let up = own.x;
    let down = weight_at(p + vec2<i32>(0, 1)).y;
    let left = own.z;
    let right = weight_at(p + vec2<i32>(1, 0)).w;
    let color = color_at(p);

    // 只沿占优的那个方向混：同时两个方向都混会把拐角糊成一团。
    if (max(up, down) >= max(left, right)) {
        let total = min(up + down, 1.0);
        if (total <= 0.0) {
            return color;
        }
        let neighbour = (color_at(p + vec2<i32>(0, -1)) * up + color_at(p + vec2<i32>(0, 1)) * down)
            / max(up + down, 1e-5);
        return mix(color, neighbour, total);
    }
    let total = min(left + right, 1.0);
    let neighbour = (color_at(p + vec2<i32>(-1, 0)) * left + color_at(p + vec2<i32>(1, 0)) * right)
        / max(left + right, 1e-5);
    return mix(color, neighbour, total);
}
