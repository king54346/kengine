//! MDD 顶点缓存（Blender / LightWave 的「点缓存」导出格式）。
//!
//! 格式本身极简：**大端**的帧数、点数、每帧的时间戳，然后是
//! `帧数 × 点数 × 3` 个大端 float。大端是这个格式最容易读错的地方——
//! 按小端读出来的坐标是天文数字或者接近零的噪声，不会报错。
//!
//! # MDD 里没有拓扑
//!
//! 它只有「第 i 个点在第 f 帧在哪」，没有三角形、没有法线、没有 UV。
//! 一份 MDD 必须配上它**当初导出时那份**几何才有意义。
//!
//! three.js 的例子直接假定「这份 cube.mdd 的 24 个点正好等于
//! `BoxGeometry` 的 24 个顶点、顺序也一样」。那个假设在别的引擎里不成立
//! ——立方体顶点的排列顺序是每个引擎自己的约定。
//!
//! 这里改成按**第一帧的位置最近邻**做一次重定向（[`PointCache::retarget`]）：
//! 网格的每个顶点找缓存里离它最近的那个点，之后逐帧跟着那个点走。
//! 立方体的一个角上有三个顶点（法线不同）而缓存里也有三个重合的点，
//! 谁配谁无所谓——它们在每一帧的位置本来就相同。
//!
//! 代价是重定向是 O(网格顶点数 × 缓存点数)，只在加载时做一次。

use crate::{bad, limits};
use kasset::LoadError;
use kmath::Vec3;
use kmesh::Mesh;

/// 一份顶点缓存。
#[derive(Debug, Clone, Default)]
pub struct PointCache {
    /// 每帧的时间戳，单位秒。
    pub times: Vec<f32>,
    /// `frames[f][p]` 是第 `f` 帧第 `p` 个点的位置。
    pub frames: Vec<Vec<Vec3>>,
}

impl PointCache {
    /// 帧数。
    pub fn len(&self) -> usize {
        self.frames.len()
    }

    /// 是否为空。
    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    /// 总时长，单位秒。
    pub fn duration(&self) -> f32 {
        self.times.last().copied().unwrap_or(0.0)
    }

    /// 把网格的每个顶点绑到缓存里最近的那个点上，见模块文档。
    ///
    /// 返回的映射长度等于网格顶点数。缓存为空时返回空映射，
    /// [`apply`](Self::apply) 遇到空映射什么也不做。
    pub fn retarget(&self, mesh: &Mesh) -> Vec<u32> {
        let Some(reference) = self.frames.first() else {
            return Vec::new();
        };
        mesh.vertices()
            .iter()
            .map(|vertex| {
                let position = vertex.position();
                reference
                    .iter()
                    .enumerate()
                    .min_by(|(_, a), (_, b)| {
                        a.distance_squared(position)
                            .total_cmp(&b.distance_squared(position))
                    })
                    .map_or(0, |(index, _)| index as u32)
            })
            .collect()
    }

    /// 按时间取姿态写进网格。超出时长时停在最后一帧。
    pub fn apply(&self, mesh: &mut Mesh, map: &[u32], time: f32) {
        if map.is_empty() || self.frames.is_empty() {
            return;
        }
        let (a, b, t) = self.sample(time);
        let (first, second) = (&self.frames[a], &self.frames[b]);
        for (vertex, &source) in mesh.vertices_mut().iter_mut().zip(map) {
            let source = source as usize;
            vertex.position = first[source].lerp(second[source], t).to_array();
        }
        // 顶点动了，法线就不再是原来那套了。逐帧重算是 O(三角形数)，
        // 对 MDD 这种规模（几十到几千个点）完全不是问题，而不重算的话
        // 形变过程中的明暗会整个是错的。
        mesh.recompute_normals();
    }

    /// 给定时间落在哪两帧之间，以及插值系数。
    fn sample(&self, time: f32) -> (usize, usize, f32) {
        let last = self.frames.len() - 1;
        if last == 0 || !time.is_finite() {
            return (0, 0, 0.0);
        }
        let duration = self.duration();
        let time = time.clamp(0.0, duration);
        // 时间戳是升序的，线性扫一遍就够——MDD 的帧数通常是几十到几百。
        let index = self
            .times
            .iter()
            .rposition(|&stamp| stamp <= time)
            .unwrap_or(0)
            .min(last);
        if index == last {
            return (last, last, 0.0);
        }
        let span = self.times[index + 1] - self.times[index];
        let t = if span > 1e-8 {
            (time - self.times[index]) / span
        } else {
            0.0
        };
        (index, index + 1, t)
    }
}

/// 解析 MDD。
pub fn parse(bytes: &[u8]) -> Result<PointCache, LoadError> {
    if bytes.len() < 8 {
        return Err(bad("MDD 太短"));
    }
    // 大端，见模块文档。
    let frames = u32::from_be_bytes(bytes[0..4].try_into().unwrap()) as usize;
    let points = u32::from_be_bytes(bytes[4..8].try_into().unwrap()) as usize;
    if frames == 0 || points == 0 {
        return Err(bad("MDD 里没有帧或点"));
    }
    if frames > 100_000 || points > limits::VERTICES / 3 {
        return Err(bad("MDD 的帧数或点数超过上限"));
    }
    let expected = 8 + frames * 4 + frames * points * 12;
    if bytes.len() < expected {
        return Err(bad("MDD 被截断"));
    }

    let be_f32 = |at: usize| f32::from_be_bytes(bytes[at..at + 4].try_into().unwrap());
    let times: Vec<f32> = (0..frames).map(|index| be_f32(8 + index * 4)).collect();
    if times.iter().any(|t| !t.is_finite()) {
        return Err(bad("MDD 的时间戳里有非有限值"));
    }
    let base = 8 + frames * 4;
    let positions = (0..frames)
        .map(|frame| {
            (0..points)
                .map(|point| {
                    let at = base + (frame * points + point) * 12;
                    Vec3::new(be_f32(at), be_f32(at + 4), be_f32(at + 8))
                })
                .collect()
        })
        .collect();
    Ok(PointCache {
        times,
        frames: positions,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 拼一份两帧、两点的 MDD：点沿 X 从 0 走到 10。
    fn file() -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&2u32.to_be_bytes());
        bytes.extend_from_slice(&2u32.to_be_bytes());
        for time in [0.0f32, 1.0] {
            bytes.extend_from_slice(&time.to_be_bytes());
        }
        for frame in [0.0f32, 10.0] {
            for point in [0.0f32, 1.0] {
                for value in [frame + point, 0.0, 0.0] {
                    bytes.extend_from_slice(&value.to_be_bytes());
                }
            }
        }
        bytes
    }

    #[test]
    fn reads_big_endian_frames() {
        let cache = parse(&file()).unwrap();
        assert_eq!(cache.len(), 2);
        assert_eq!(cache.duration(), 1.0);
        assert_eq!(cache.frames[1][0], Vec3::new(10.0, 0.0, 0.0));
    }

    /// 按小端读的话第一个坐标会是个天文数字，而且不会报任何错。
    #[test]
    fn little_endian_would_give_nonsense_so_the_first_frame_is_checked() {
        let cache = parse(&file()).unwrap();
        assert!(
            cache.frames[0][0].length() < 1.0,
            "第一帧应当在原点附近，实际是 {:?}",
            cache.frames[0][0]
        );
    }

    #[test]
    fn sampling_interpolates_and_clamps() {
        let cache = parse(&file()).unwrap();
        assert_eq!(cache.sample(0.5), (0, 1, 0.5));
        assert_eq!(cache.sample(99.0), (1, 1, 0.0));
        assert_eq!(cache.sample(-5.0), (0, 1, 0.0));
    }

    #[test]
    fn retargeting_picks_the_nearest_point_in_the_first_frame() {
        let cache = parse(&file()).unwrap();
        let mesh = Mesh::point_sprites(&[Vec3::new(0.9, 0.0, 0.0)], &[]);
        let map = cache.retarget(&mesh);
        assert!(map.iter().all(|&index| index == 1), "该绑到 x=1 那个点");
    }

    #[test]
    fn a_truncated_file_is_rejected() {
        let mut bytes = file();
        bytes.truncate(bytes.len() - 4);
        assert!(parse(&bytes).is_err());
    }
}
