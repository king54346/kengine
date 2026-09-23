//! OGC 3D Tiles（1.0 / 1.1）：分层级、按需流式加载的大场景。
//!
//! 一个瓦片集（`tileset.json`）是一棵树：每个瓦片有包围体、**几何误差**
//! （「用这一级代替完整模型会差多少米」）、可选的内容（`.glb` / `.b3dm` /
//! 另一个 `tileset.json`）和子瓦片。渲染时按**屏幕空间误差**选择：
//!
//! ```text
//! sse = 几何误差 × 视口高度 / (距离 × 2 × tan(fov/2))
//! ```
//!
//! 误差大于阈值（默认 16 像素）就往下细化，否则就用这一级。
//!
//! # 分工
//!
//! 这个模块只做**与引擎场景无关**的部分：解析瓦片集、算选择结果。
//! 真正的加载与卸载由调用方按 [`Selection`] 去做（请求资源、实例化、
//! 删掉不再需要的节点）——这样选择算法可以脱离 GPU 和场景单独测试。
//!
//! # 支持
//!
//! | 部分 | |
//! |---|---|
//! | 包围体 | `box`、`sphere`、`region`（经纬度区域，换算成 ECEF 包围球） |
//! | 细化 | `REPLACE`（子级就绪前保留父级，不闪洞）与 `ADD` |
//! | 变换 | 逐级 `transform` 累乘 |
//! | 内容 | `.glb` / `.gltf`（经 [`GltfLoader`](kgltf::GltfLoader)）、`.b3dm`（见 [`B3dmLoader`]）、外部 `tileset.json`（展开进同一棵树） |
//! | 1.1 | `content` / `contents`（取第一个）、`uri` 与旧的 `url` |
//!
//! 不支持：隐式瓦片（`implicitTiling`）、`.i3dm` / `.pnts` / `.cmpt`、元数据、
//! 按视锥之外的请求优先级排序（这里按屏幕误差从大到小排）。

use crate::{bad, loader};
use kasset::{LoadError, ResourceData, ResourceIo};
use kcore::uuid::{Uuid, uuid};
use kgltf::{MODEL_TYPE_UUID, Model};
use kmath::{Mat4, Vec3, Vec4};
use serde_json::Value;
use std::{path::PathBuf, sync::Arc};

/// [`Tileset`] 的资源类型标识。
pub const TILESET_TYPE_UUID: Uuid = uuid!("7e1c9a4f-3b62-4d58-a0e7-5f2c8b1d9e34");

/// 细化方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refine {
    /// 子级替换父级。
    Replace,
    /// 子级叠加在父级之上。
    Add,
}

/// 一个瓦片。
#[derive(Debug, Clone)]
pub struct Tile {
    /// 包围球球心（瓦片集坐标系，已乘上逐级变换）。
    pub center: Vec3,
    /// 包围球半径。
    pub radius: f32,
    /// 几何误差（米）。
    pub geometric_error: f32,
    /// 细化方式。
    pub refine: Refine,
    /// 内容文件（绝对路径），没有内容时为 `None`。
    pub content: Option<PathBuf>,
    /// 子瓦片（[`Tileset::tiles`] 的下标）。
    pub children: Vec<usize>,
    /// 累乘后的变换：内容模型要按它摆放。
    pub transform: Mat4,
    /// 在树里的深度。
    pub depth: usize,
}

/// 解析好的瓦片集。
#[derive(Debug, Clone, Default)]
pub struct Tileset {
    /// 全部瓦片，`tiles[0]` 是根。
    pub tiles: Vec<Tile>,
    /// 顶层几何误差。
    pub geometric_error: f32,
}

impl ResourceData for Tileset {
    fn type_uuid(&self) -> Uuid {
        TILESET_TYPE_UUID
    }
}

/// 视点参数。
#[derive(Debug, Clone, Copy)]
pub struct View {
    /// 相机位置（瓦片集坐标系）。
    pub position: Vec3,
    /// `投影 × 视图`（瓦片集坐标系 → 裁剪空间），用来做视锥剔除。
    pub view_proj: Mat4,
    /// 垂直视场角（弧度）。
    pub fov_y: f32,
    /// 视口高度（像素）。
    pub viewport_height: f32,
    /// 允许的最大屏幕空间误差（像素）。
    pub max_error: f32,
}

/// 一次选择的结果。
#[derive(Debug, Clone, Default)]
pub struct Selection {
    /// 这一帧该显示的瓦片（都已就绪）。
    pub visible: Vec<usize>,
    /// 应当开始加载的瓦片，按优先级（屏幕误差从大到小）排好。
    pub requests: Vec<usize>,
}

/// 从裁剪矩阵取出六个平面（Gribb–Hartmann），法线朝内。
fn frustum_planes(m: Mat4) -> [Vec4; 6] {
    let r = |i: usize| m.row(i);
    [r(3) + r(0), r(3) - r(0), r(3) + r(1), r(3) - r(1), r(3) + r(2), r(3) - r(2)].map(|p| {
        let length = p.truncate().length().max(1e-12);
        p / length
    })
}

impl Tileset {
    /// 屏幕空间误差（像素）。相机在包围球里时视为无穷大。
    pub fn screen_error(&self, tile: usize, view: &View) -> f32 {
        let tile = &self.tiles[tile];
        let distance = (tile.center.distance(view.position) - tile.radius).max(1e-3);
        tile.geometric_error * view.viewport_height / (distance * 2.0 * (view.fov_y * 0.5).tan())
    }

    /// 按屏幕空间误差选瓦片。
    ///
    /// `ready(tile)` 告诉算法某个瓦片的内容是否已经加载好。`REPLACE` 细化时，
    /// 子级**全部**就绪之前继续显示父级——否则相机一动就会露出一块块的洞。
    pub fn select(&self, view: &View, ready: impl Fn(usize) -> bool) -> Selection {
        let planes = frustum_planes(view.view_proj);
        let mut selection = Selection::default();
        if self.tiles.is_empty() {
            return selection;
        }
        let mut requests: Vec<(f32, usize)> = Vec::new();
        self.visit(0, view, &planes, &ready, &mut selection.visible, &mut requests);
        requests.sort_by(|a, b| b.0.total_cmp(&a.0));
        requests.dedup_by_key(|r| r.1);
        selection.requests = requests.into_iter().map(|r| r.1).collect();
        selection
    }

    fn in_frustum(&self, tile: usize, planes: &[Vec4; 6]) -> bool {
        let t = &self.tiles[tile];
        planes.iter().all(|p| p.truncate().dot(t.center) + p.w >= -t.radius)
    }

    /// 返回这个子树是否有东西可画（给父级判断「子级是否就绪」用）。
    fn visit(
        &self,
        index: usize,
        view: &View,
        planes: &[Vec4; 6],
        ready: &impl Fn(usize) -> bool,
        visible: &mut Vec<usize>,
        requests: &mut Vec<(f32, usize)>,
    ) -> bool {
        let tile = &self.tiles[index];
        if !self.in_frustum(index, planes) {
            // 视锥外的瓦片不画，但不算「没就绪」：否则父级永远退不掉。
            return true;
        }
        let error = self.screen_error(index, view);
        let has_content = tile.content.is_some();
        let own_ready = !has_content || ready(index);
        if has_content && !own_ready {
            requests.push((error, index));
        }
        let refine = error > view.max_error && !tile.children.is_empty();
        if !refine {
            if has_content && own_ready {
                visible.push(index);
            }
            return own_ready;
        }
        match tile.refine {
            Refine::Add => {
                if has_content && own_ready {
                    visible.push(index);
                }
                for &child in &tile.children {
                    self.visit(child, view, planes, ready, visible, requests);
                }
                own_ready
            }
            Refine::Replace => {
                let mark = visible.len();
                let mut all = true;
                for &child in &tile.children {
                    all &= self.visit(child, view, planes, ready, visible, requests);
                }
                if all {
                    return true;
                }
                // 子级没就绪：撤掉子级已选的，退回显示自己。
                visible.truncate(mark);
                if has_content && own_ready {
                    visible.push(index);
                }
                own_ready
            }
        }
    }
}

loader! {
    /// 读 `tileset.json`（连带展开外部子瓦片集）。扩展名是 `json`，要和其它
    /// JSON 加载器区分时把瓦片集文件改名成 `.tileset` 也认。
    TilesetLoader -> Tileset : ["json", "tileset"] = TILESET_TYPE_UUID, parse
}

/// 解析瓦片集。
pub async fn parse(bytes: Vec<u8>, path: PathBuf, io: Arc<dyn ResourceIo>) -> Result<Tileset, LoadError> {
    let mut tileset = Tileset::default();
    let json: Value = serde_json::from_slice(&bytes).map_err(|e| bad(format!("tileset.json 不是合法 JSON：{e}")))?;
    if json.get("root").is_none() {
        return Err(bad("tileset.json 缺少 root"));
    }
    tileset.geometric_error = json.get("geometricError").and_then(Value::as_f64).unwrap_or(0.0) as f32;
    // 外部瓦片集递归展开：栈里放 `(JSON, 基目录, 父变换, 父瓦片, 深度)`。
    let mut pending = vec![(json["root"].clone(), crate::base_dir(&path), Mat4::IDENTITY, None::<usize>, 0usize, Refine::Replace)];
    let mut external_budget = 256;
    while let Some((node, base, parent_transform, parent, depth, inherited)) = pending.pop() {
        if tileset.tiles.len() > 1_000_000 || depth > 64 {
            return Err(bad("瓦片集太大或太深"));
        }
        let transform = parent_transform * node.get("transform").map_or(Mat4::IDENTITY, |t| {
            let v: Vec<f32> = t.as_array().map(|a| a.iter().filter_map(Value::as_f64).map(|f| f as f32).collect()).unwrap_or_default();
            if v.len() == 16 { Mat4::from_cols_array(&v.try_into().expect("16")) } else { Mat4::IDENTITY }
        });
        let (center, radius) = bounding_sphere(node.get("boundingVolume"), transform);
        let refine = match node.get("refine").and_then(Value::as_str) {
            Some(r) if r.eq_ignore_ascii_case("ADD") => Refine::Add,
            Some(_) => Refine::Replace,
            None => inherited,
        };
        let content = node
            .get("content")
            .or_else(|| node.get("contents").and_then(|c| c.get(0)))
            .and_then(|c| c.get("uri").or_else(|| c.get("url")))
            .and_then(Value::as_str)
            .map(|uri| base.join(uri.split(['?', '#']).next().unwrap_or(uri)));
        let index = tileset.tiles.len();
        tileset.tiles.push(Tile {
            center,
            radius,
            geometric_error: node.get("geometricError").and_then(Value::as_f64).unwrap_or(0.0) as f32,
            refine,
            content: None,
            children: Vec::new(),
            transform,
            depth,
        });
        if let Some(parent) = parent {
            tileset.tiles[parent].children.push(index);
        }
        match content {
            // 外部瓦片集：把它的根当成这个瓦片的子级。
            Some(file) if file.extension().is_some_and(|e| e.eq_ignore_ascii_case("json")) => {
                external_budget -= 1;
                if external_budget < 0 {
                    return Err(bad("外部瓦片集过多（多半是循环引用）"));
                }
                match io.load_file(&file).await {
                    Ok(bytes) => {
                        let child: Value = serde_json::from_slice(&bytes).map_err(|e| bad(format!("{} 不是合法 JSON：{e}", file.display())))?;
                        if let Some(root) = child.get("root") {
                            pending.push((root.clone(), crate::base_dir(&file), transform, Some(index), depth + 1, refine));
                        }
                    }
                    Err(error) => klog::warn!("外部瓦片集 {} 读不出来：{error}", file.display()),
                }
            }
            other => tileset.tiles[index].content = other,
        }
        if let Some(children) = node.get("children").and_then(Value::as_array) {
            // 倒序压栈，出栈时就是文件里的顺序。
            for child in children.iter().rev() {
                pending.push((child.clone(), base.clone(), transform, Some(index), depth + 1, refine));
            }
        }
    }
    Ok(tileset)
}

/// 包围体 → 包围球（已变换）。
fn bounding_sphere(volume: Option<&Value>, transform: Mat4) -> (Vec3, f32) {
    let numbers = |key: &str| -> Option<Vec<f64>> {
        Some(volume?.get(key)?.as_array()?.iter().filter_map(Value::as_f64).collect())
    };
    let scale = transform.to_scale_rotation_translation().0.max_element();
    if let Some(b) = numbers("box").filter(|b| b.len() >= 12) {
        let center = Vec3::new(b[0] as f32, b[1] as f32, b[2] as f32);
        let half = [3, 6, 9].map(|i| Vec3::new(b[i] as f32, b[i + 1] as f32, b[i + 2] as f32));
        let radius = (half[0] + half[1] + half[2]).length().max((half[0] - half[1] + half[2]).length()).max((half[0] + half[1] - half[2]).length()).max((-half[0] + half[1] + half[2]).length());
        return (transform.transform_point3(center), radius * scale);
    }
    if let Some(s) = numbers("sphere").filter(|s| s.len() >= 4) {
        return (transform.transform_point3(Vec3::new(s[0] as f32, s[1] as f32, s[2] as f32)), s[3] as f32 * scale);
    }
    if let Some(r) = numbers("region").filter(|r| r.len() >= 6) {
        // [西, 南, 东, 北, 最低, 最高]（弧度、米），WGS84 → ECEF。区域本身就在 ECEF 里，不乘变换。
        let ecef = |lon: f64, lat: f64, h: f64| {
            let a = 6_378_137.0f64;
            let e2 = 6.694_379_990_14e-3;
            let n = a / (1.0 - e2 * lat.sin().powi(2)).sqrt();
            Vec3::new(((n + h) * lat.cos() * lon.cos()) as f32, ((n + h) * lat.cos() * lon.sin()) as f32, ((n * (1.0 - e2) + h) * lat.sin()) as f32)
        };
        let mut corners = vec![ecef((r[0] + r[2]) / 2.0, (r[1] + r[3]) / 2.0, r[5])];
        for lon in [r[0], r[2]] {
            for lat in [r[1], r[3]] {
                for h in [r[4], r[5]] {
                    corners.push(ecef(lon, lat, h));
                }
            }
        }
        let center = corners.iter().copied().sum::<Vec3>() / corners.len() as f32;
        let radius = corners.iter().map(|c| c.distance(center)).fold(0.0, f32::max);
        return (center, radius);
    }
    (transform.transform_point3(Vec3::ZERO), f32::MAX / 4.0)
}

// ── b3dm ──

loader! {
    /// 读 `.b3dm`（Batched 3D Model）：剥掉 28 字节头与特征表 / 批次表，里面的 GLB 交给 kgltf。
    B3dmLoader -> Model : ["b3dm"] = MODEL_TYPE_UUID, parse_b3dm
}

/// 解析 `.b3dm`。
pub async fn parse_b3dm(bytes: Vec<u8>, path: PathBuf, io: Arc<dyn ResourceIo>) -> Result<Model, LoadError> {
    let glb = b3dm_payload(&bytes)?.to_vec();
    kgltf::import_bytes(glb, path, io).await
}

/// `.b3dm` 里 GLB 的那一段。
pub fn b3dm_payload(bytes: &[u8]) -> Result<&[u8], LoadError> {
    if bytes.len() < 28 || &bytes[0..4] != b"b3dm" {
        return Err(bad("不是 b3dm 文件"));
    }
    let word = |i: usize| u32::from_le_bytes(bytes[i..i + 4].try_into().expect("4")) as usize;
    let mut start = 28 + word(12) + word(16) + word(20) + word(24);
    // 老版本（1.0 之前）的头只有 24 字节，头部偏移对不上时退回去试。
    if bytes.get(start..start + 4) != Some(b"glTF") {
        start = 24 + word(12) + word(16) + word(20);
    }
    bytes.get(start..).filter(|b| b.starts_with(b"glTF")).ok_or_else(|| bad("b3dm 里找不到 GLB"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use kasset::MemoryResourceIo;
    use std::path::Path;

    fn tileset() -> Tileset {
        let json = br#"{
            "asset": {"version": "1.1"}, "geometricError": 100,
            "root": {
                "boundingVolume": {"sphere": [0, 0, 0, 100]}, "geometricError": 50, "refine": "REPLACE",
                "content": {"uri": "root.glb"},
                "children": [
                    {"boundingVolume": {"sphere": [-50, 0, 0, 50]}, "geometricError": 0, "content": {"uri": "a.glb"}},
                    {"boundingVolume": {"sphere": [50, 0, 0, 50]}, "geometricError": 0, "content": {"uri": "b.glb"}}
                ]
            }
        }"#;
        let io: Arc<dyn ResourceIo> = Arc::new(MemoryResourceIo::new());
        ktask::block_on(parse(json.to_vec(), PathBuf::from("tiles/tileset.json"), io)).unwrap()
    }

    fn view(distance: f32) -> View {
        let position = Vec3::new(0.0, 0.0, distance);
        let view = Mat4::look_at_rh(position, Vec3::ZERO, Vec3::Y);
        View {
            position,
            view_proj: Mat4::perspective_rh(1.0, 1.0, 0.1, 1.0e6) * view,
            fov_y: 1.0,
            viewport_height: 1000.0,
            max_error: 16.0,
        }
    }

    #[test]
    fn parses_tree_and_paths() {
        let t = tileset();
        assert_eq!(t.tiles.len(), 3);
        assert_eq!(t.tiles[0].children, [1, 2]);
        assert_eq!(t.tiles[1].content.as_deref(), Some(Path::new("tiles/a.glb")));
        assert_eq!(t.tiles[1].refine, Refine::Replace, "细化方式要继承父级");
    }

    #[test]
    fn far_away_uses_the_root_and_close_up_refines() {
        let t = tileset();
        let far = t.select(&view(100_000.0), |_| true);
        assert_eq!(far.visible, [0]);
        let near = t.select(&view(300.0), |_| true);
        assert_eq!(near.visible, [1, 2]);
    }

    #[test]
    fn replace_keeps_the_parent_until_children_are_ready() {
        let t = tileset();
        let s = t.select(&view(300.0), |i| i != 2);
        assert_eq!(s.visible, [0], "一个子级没就绪就不能换掉父级");
        assert_eq!(s.requests, [2]);
    }

    #[test]
    fn b3dm_header_is_stripped() {
        let mut bytes = b"b3dm".to_vec();
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(&[0u8; 16]);
        bytes.extend_from_slice(b"glTF....");
        assert!(b3dm_payload(&bytes).unwrap().starts_with(b"glTF"));
    }
}
