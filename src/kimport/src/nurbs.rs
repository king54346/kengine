//! NURBS 曲线求值，FBX 与 3DM 共用。

/// 有理 B 样条求值（齐次坐标上做 de Boor，最后除以 w）。
pub(crate) fn de_boor(degree: usize, knots: &[f64], points: &[[f64; 4]], u: f64) -> [f64; 3] {
    let n = points.len() - 1;
    // 找 u 所在的结点区间 [k_span, k_span+1)。
    let mut span = degree;
    while span < n && u >= knots[span + 1] {
        span += 1;
    }
    let mut d: Vec<[f64; 4]> = (0..=degree)
        .map(|j| {
            let p = points[(span + j).saturating_sub(degree).min(n)];
            [p[0] * p[3], p[1] * p[3], p[2] * p[3], p[3]]
        })
        .collect();
    for r in 1..=degree {
        for j in (r..=degree).rev() {
            let i = span + j - degree;
            let left = knots[i];
            let right = knots.get(i + degree + 1 - r).copied().unwrap_or(left);
            let alpha = if right > left { (u - left) / (right - left) } else { 0.0 };
            for c in 0..4 {
                d[j][c] = (1.0 - alpha) * d[j - 1][c] + alpha * d[j][c];
            }
        }
    }
    let p = d[degree];
    let w = if p[3].abs() > 1e-12 { p[3] } else { 1.0 };
    [p[0] / w, p[1] / w, p[2] / w]
}

