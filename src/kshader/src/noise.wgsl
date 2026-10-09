// MaterialX 标准库的程序噪声（three.js TSL 的 `mx_noise_float` 那一族用的就是这套）。
//
// 按 MaterialX 的 `mx_noise.glsl` 移植：Bob Jenkins 的 lookup3 整数哈希 + 经典 Perlin 梯度噪声，
// 所以同样的输入和 three.js 算出来的是同一个值（浮点误差以内），照抄 TSL 的参数能得到同样的图案。
//
// 名字和 TSL 一致；WGSL 没有重载，二维 / 三维、标量 / 向量各起一个名字：
//
// | 函数 | 值域 | TSL |
// |---|---|---|
// | `mx_noise_float(p: vec3)` / `mx_noise_float_2d(p: vec2)` | 约 [-1, 1] | `mx_noise_float` |
// | `mx_noise_vec3(p: vec3)` | 每分量约 [-1, 1] | `mx_noise_vec3` |
// | `mx_fractal_noise_float(p, octaves, lacunarity, diminish)` | 叠加后约 [-2, 2] | `mx_fractal_noise_float` |
// | `mx_fractal_noise_vec3(p, octaves, lacunarity, diminish)` | | `mx_fractal_noise_vec3` |
// | `mx_cell_noise_float(p: vec3)` | [0, 1]，逐格常量 | `mx_cell_noise_float` |
// | `mx_worley_noise_float(p: vec3, jitter)` / `mx_worley_noise_float_2d` | 到最近特征点的距离 | `mx_worley_noise_float` |
//
// TSL 版本的 `amplitude` / `pivot` 参数就是 `* amplitude + pivot`，调用处自己乘。
// 另外附带 TSL 里常用的几个小工具：`tsl_hash(u32) -> f32`、`tsl_remap`、`tsl_tri_noise3d(p, speed, time)`。

fn mx_rotl32(x: u32, k: u32) -> u32 {
    return (x << k) | (x >> (32u - k));
}

fn mx_bjfinal(a_in: u32, b_in: u32, c_in: u32) -> u32 {
    var a = a_in;
    var b = b_in;
    var c = c_in;
    c ^= b; c -= mx_rotl32(b, 14u);
    a ^= c; a -= mx_rotl32(c, 11u);
    b ^= a; b -= mx_rotl32(a, 25u);
    c ^= b; c -= mx_rotl32(b, 16u);
    a ^= c; a -= mx_rotl32(c, 4u);
    b ^= a; b -= mx_rotl32(a, 14u);
    c ^= b; c -= mx_rotl32(b, 24u);
    return c;
}

fn mx_hash_int_2d(x: i32, y: i32) -> u32 {
    let seed = 0xdeadbeefu + (2u << 2u) + 13u;
    return mx_bjfinal(seed + bitcast<u32>(x), seed + bitcast<u32>(y), seed);
}

fn mx_hash_int_3d(x: i32, y: i32, z: i32) -> u32 {
    let seed = 0xdeadbeefu + (3u << 2u) + 13u;
    return mx_bjfinal(seed + bitcast<u32>(x), seed + bitcast<u32>(y), seed + bitcast<u32>(z));
}

fn mx_hash_int_4d(x: i32, y: i32, z: i32, w: i32) -> u32 {
    let seed = 0xdeadbeefu + (4u << 2u) + 13u;
    var a = seed + bitcast<u32>(x);
    var b = seed + bitcast<u32>(y);
    var c = seed + bitcast<u32>(z);
    // mx_bjmix
    a -= c; a ^= mx_rotl32(c, 4u); c += b;
    b -= a; b ^= mx_rotl32(a, 6u); a += c;
    c -= b; c ^= mx_rotl32(b, 8u); b += a;
    a -= c; a ^= mx_rotl32(c, 16u); c += b;
    b -= a; b ^= mx_rotl32(a, 19u); a += c;
    c -= b; c ^= mx_rotl32(b, 4u); b += a;
    a += bitcast<u32>(w);
    return mx_bjfinal(a, b, c);
}

fn mx_bits_to_01(bits: u32) -> f32 {
    return f32(bits) / 4294967295.0;
}

fn mx_fade(t: f32) -> f32 {
    return t * t * t * (t * (t * 6.0 - 15.0) + 10.0);
}

fn mx_negate_if(value: f32, flag: bool) -> f32 {
    return select(value, -value, flag);
}

fn mx_gradient_float_2d(hash: u32, x: f32, y: f32) -> f32 {
    let h = hash & 7u;
    let u = select(y, x, h < 4u);
    let v = 2.0 * select(x, y, h < 4u);
    return mx_negate_if(u, (h & 1u) != 0u) + mx_negate_if(v, (h & 2u) != 0u);
}

fn mx_gradient_float_3d(hash: u32, x: f32, y: f32, z: f32) -> f32 {
    let h = hash & 15u;
    let u = select(y, x, h < 8u);
    let v = select(select(z, x, h == 12u || h == 14u), y, h < 4u);
    return mx_negate_if(u, (h & 1u) != 0u) + mx_negate_if(v, (h & 2u) != 0u);
}

fn mx_bilerp(v0: f32, v1: f32, v2: f32, v3: f32, s: f32, t: f32) -> f32 {
    let s1 = 1.0 - s;
    let t1 = 1.0 - t;
    return t1 * (v0 * s1 + v1 * s) + t * (v2 * s1 + v3 * s);
}

fn mx_trilerp(v0: f32, v1: f32, v2: f32, v3: f32, v4: f32, v5: f32, v6: f32, v7: f32, s: f32, t: f32, r: f32) -> f32 {
    let s1 = 1.0 - s;
    let t1 = 1.0 - t;
    let r1 = 1.0 - r;
    return r1 * (t1 * (v0 * s1 + v1 * s) + t * (v2 * s1 + v3 * s)) + r * (t1 * (v4 * s1 + v5 * s) + t * (v6 * s1 + v7 * s));
}

fn mx_noise_float_2d(p: vec2<f32>) -> f32 {
    let cell = floor(p);
    let ix = i32(cell.x);
    let iy = i32(cell.y);
    let f = p - cell;
    let u = mx_fade(f.x);
    let v = mx_fade(f.y);
    let result = mx_bilerp(
        mx_gradient_float_2d(mx_hash_int_2d(ix, iy), f.x, f.y),
        mx_gradient_float_2d(mx_hash_int_2d(ix + 1, iy), f.x - 1.0, f.y),
        mx_gradient_float_2d(mx_hash_int_2d(ix, iy + 1), f.x, f.y - 1.0),
        mx_gradient_float_2d(mx_hash_int_2d(ix + 1, iy + 1), f.x - 1.0, f.y - 1.0),
        u,
        v,
    );
    return 0.6616 * result;
}

// 三维 Perlin。`whole` 为真时梯度用整个哈希（标量版）；否则取哈希右移 `shift` 位后的低 8 位（向量版每个分量各取一段）。
fn mx_perlin_3d(p: vec3<f32>, shift: u32, whole: bool) -> f32 {
    let cell = floor(p);
    let i = vec3<i32>(cell);
    let f = p - cell;
    let u = mx_fade(f.x);
    let v = mx_fade(f.y);
    let w = mx_fade(f.z);
    var g: array<f32, 8>;
    for (var corner = 0u; corner < 8u; corner++) {
        let o = vec3<i32>(i32(corner & 1u), i32((corner >> 1u) & 1u), i32((corner >> 2u) & 1u));
        var hash = mx_hash_int_3d(i.x + o.x, i.y + o.y, i.z + o.z);
        if (!whole) {
            hash = (hash >> shift) & 0xFFu;
        }
        let d = f - vec3<f32>(o);
        g[corner] = mx_gradient_float_3d(hash, d.x, d.y, d.z);
    }
    return 0.982 * mx_trilerp(g[0], g[1], g[2], g[3], g[4], g[5], g[6], g[7], u, v, w);
}

fn mx_noise_float(p: vec3<f32>) -> f32 {
    return mx_perlin_3d(p, 0u, true);
}

fn mx_noise_vec3(p: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(mx_perlin_3d(p, 0u, false), mx_perlin_3d(p, 8u, false), mx_perlin_3d(p, 16u, false));
}

fn mx_fractal_noise_float(p_in: vec3<f32>, octaves: i32, lacunarity: f32, diminish: f32) -> f32 {
    var p = p_in;
    var result = 0.0;
    var amplitude = 1.0;
    for (var i = 0; i < octaves; i++) {
        result += amplitude * mx_noise_float(p);
        amplitude *= diminish;
        p *= lacunarity;
    }
    return result;
}

fn mx_fractal_noise_vec3(p_in: vec3<f32>, octaves: i32, lacunarity: f32, diminish: f32) -> vec3<f32> {
    var p = p_in;
    var result = vec3<f32>(0.0);
    var amplitude = 1.0;
    for (var i = 0; i < octaves; i++) {
        result += amplitude * mx_noise_vec3(p);
        amplitude *= diminish;
        p *= lacunarity;
    }
    return result;
}

fn mx_cell_noise_float(p: vec3<f32>) -> f32 {
    let i = vec3<i32>(floor(p));
    return mx_bits_to_01(mx_hash_int_3d(i.x, i.y, i.z));
}

fn mx_cell_noise_vec3_at(i: vec3<i32>) -> vec3<f32> {
    return vec3<f32>(
        mx_bits_to_01(mx_hash_int_4d(i.x, i.y, i.z, 0)),
        mx_bits_to_01(mx_hash_int_4d(i.x, i.y, i.z, 1)),
        mx_bits_to_01(mx_hash_int_4d(i.x, i.y, i.z, 2)),
    );
}

fn mx_cell_noise_vec2_at(i: vec2<i32>) -> vec2<f32> {
    let seed = 0xdeadbeefu + (3u << 2u) + 13u;
    return vec2<f32>(
        mx_bits_to_01(mx_bjfinal(seed + bitcast<u32>(i.x), seed + bitcast<u32>(i.y), seed)),
        mx_bits_to_01(mx_bjfinal(seed + bitcast<u32>(i.x), seed + bitcast<u32>(i.y), seed + 1u)),
    );
}

// 欧氏距离（开方后）到最近特征点。`jitter` = 1 时特征点在格子里完全随机，0 时就在格点上。
fn mx_worley_noise_float(p: vec3<f32>, jitter: f32) -> f32 {
    let cell = floor(p);
    let i = vec3<i32>(cell);
    let local = p - cell;
    var best = 1e6;
    for (var z = -1; z <= 1; z++) {
        for (var y = -1; y <= 1; y++) {
            for (var x = -1; x <= 1; x++) {
                let o = vec3<i32>(x, y, z);
                let offset = (mx_cell_noise_vec3_at(i + o) - 0.5) * jitter + 0.5;
                let diff = vec3<f32>(o) + offset - local;
                best = min(best, dot(diff, diff));
            }
        }
    }
    return sqrt(best);
}

fn mx_worley_noise_float_2d(p: vec2<f32>, jitter: f32) -> f32 {
    let cell = floor(p);
    let i = vec2<i32>(cell);
    let local = p - cell;
    var best = 1e6;
    for (var y = -1; y <= 1; y++) {
        for (var x = -1; x <= 1; x++) {
            let o = vec2<i32>(x, y);
            let offset = (mx_cell_noise_vec2_at(i + o) - 0.5) * jitter + 0.5;
            let diff = vec2<f32>(o) + offset - local;
            best = min(best, dot(diff, diff));
        }
    }
    return sqrt(best);
}

// TSL 的 `hash(seed)`：PCG 风格的整数哈希 → [0, 1)。按实例号、粒子号撒随机数用。
fn tsl_hash(seed: u32) -> f32 {
    let state = seed * 747796405u + 2891336453u;
    let word = ((state >> ((state >> 28u) + 4u)) ^ state) * 277803737u;
    return f32((word >> 22u) ^ word) / 4294967295.0;
}

// TSL 的 `remap(x, inLow, inHigh, outLow = 0, outHigh = 1)`（不夹紧）。
fn tsl_remap(x: f32, in_low: f32, in_high: f32, out_low: f32, out_high: f32) -> f32 {
    return out_low + (x - in_low) * (out_high - out_low) / (in_high - in_low);
}

// TSL 的 triNoise3D：三角波叠出来的便宜「噪声」（四层扭曲），值约在 [0, 1]。three.js 的雨、雪、烟例子常用。
fn tsl_tri(x: f32) -> f32 {
    return abs(fract(x) - 0.5);
}

fn tsl_tri3(p: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(tsl_tri(p.z + tsl_tri(p.y)), tsl_tri(p.z + tsl_tri(p.x)), tsl_tri(p.y + tsl_tri(p.x)));
}

fn tsl_tri_noise3d(position: vec3<f32>, speed: f32, time: f32) -> f32 {
    var p = position;
    var z = 1.4;
    var rz = 0.0;
    var bp = position;
    for (var i = 0; i <= 3; i++) {
        let dg = tsl_tri3(bp * 2.0);
        p += dg + time * 0.1 * speed;
        bp *= 1.8;
        z *= 1.5;
        p *= 1.2;
        rz += tsl_tri(p.z + tsl_tri(p.x + tsl_tri(p.y))) / z;
        bp += 0.14;
    }
    return rz;
}
