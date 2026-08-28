//! 2D 包围体：相交、射线、扫掠。
//!
//! 三维那边的 [`Aabb`](crate::Aabb) 早就有了，这里补的是二维的一套，外加
//! 三维那边也没有的两样东西：**射线检测**与**扫掠**。
//!
//! # 和 `kphysics` 的射线是两回事
//!
//! `kphysics` 也能打射线，但那问的是「物理世界里有什么挡着」——目标必须
//! 是注册过的刚体或碰撞体。这里的一套是**纯几何**：给两个数字算一个数字，
//! 不需要物理世界，也不需要任何东西登记在案。
//!
//! 用途不一样：物理射线用来问「玩家瞄的是哪个敌人」；这里的用来做
//! 粗筛（在建物理体之前先排掉八成）、UI 命中、以及所有「有一组框，
//! 想知道哪些碰上了」的场合。
//!
//! # 扫掠是干什么的
//!
//! 「这个框沿这个方向走，多远会撞上那个框」。逐帧做相交测试会**漏掉快速
//! 运动**：一发子弹这一帧在墙前、下一帧在墙后，两帧都不相交，于是穿墙而过。
//! 扫掠问的是整段路径，所以不会漏。
//!
//! 实现上用的是闵可夫斯基和那个技巧：把目标按运动体的尺寸「膨胀」一圈，
//! 问题就退化成「一条射线打一个膨胀后的静态形状」——两个盒子的扫掠因此
//! 不必真的沿路径采样。

use crate::Vec2;

/// 二维轴对齐包围盒。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Aabb2d {
    /// 各轴的最小值。
    pub min: Vec2,
    /// 各轴的最大值。
    pub max: Vec2,
}

impl Aabb2d {
    /// 由中心与半尺寸构造。半尺寸取绝对值，传负数不会得到一个反向的盒子。
    pub fn new(center: Vec2, half_size: Vec2) -> Self {
        let half_size = half_size.abs();
        Self {
            min: center - half_size,
            max: center + half_size,
        }
    }

    /// 由两个角点构造，顺序随意。
    pub fn from_corners(a: Vec2, b: Vec2) -> Self {
        Self {
            min: a.min(b),
            max: a.max(b),
        }
    }

    /// 刚好包住一组点。点集为空时返回一个退化在原点的盒子。
    pub fn from_points(points: &[Vec2]) -> Self {
        let Some((first, rest)) = points.split_first() else {
            return Self {
                min: Vec2::ZERO,
                max: Vec2::ZERO,
            };
        };
        let mut aabb = Self {
            min: *first,
            max: *first,
        };
        for point in rest {
            aabb.min = aabb.min.min(*point);
            aabb.max = aabb.max.max(*point);
        }
        aabb
    }

    /// 中心。
    pub fn center(&self) -> Vec2 {
        (self.min + self.max) * 0.5
    }

    /// 半尺寸（中心到面的距离）。
    pub fn half_size(&self) -> Vec2 {
        (self.max - self.min) * 0.5
    }

    /// 整体尺寸。
    pub fn size(&self) -> Vec2 {
        self.max - self.min
    }

    /// 面积。
    pub fn area(&self) -> f32 {
        let size = self.size();
        size.x * size.y
    }

    /// 点在盒内（含边界）。
    pub fn contains_point(&self, point: Vec2) -> bool {
        point.cmpge(self.min).all() && point.cmple(self.max).all()
    }

    /// 完全包住另一个盒子。
    pub fn contains(&self, other: &Self) -> bool {
        other.min.cmpge(self.min).all() && other.max.cmple(self.max).all()
    }

    /// 同时包住两者的最小盒子。
    pub fn merged(&self, other: &Self) -> Self {
        Self {
            min: self.min.min(other.min),
            max: self.max.max(other.max),
        }
    }

    /// 四周各外扩一圈。传负数即收缩。
    pub fn grown(&self, amount: Vec2) -> Self {
        Self {
            min: self.min - amount,
            max: self.max + amount,
        }
    }

    /// 盒内离给定点最近的点。点在盒内时就是它自己。
    pub fn closest_point(&self, point: Vec2) -> Vec2 {
        point.clamp(self.min, self.max)
    }

    /// 和另一个盒子有重叠（含仅接触）。
    pub fn intersects(&self, other: &Self) -> bool {
        self.min.cmple(other.max).all() && self.max.cmpge(other.min).all()
    }

    /// 和一个圆有重叠。
    pub fn intersects_circle(&self, circle: &BoundingCircle) -> bool {
        // 盒上离圆心最近的点在圆内 ⟺ 两者相交。这个判据对
        // 「圆心在盒内」也成立：那时最近点就是圆心，距离为 0。
        let closest = self.closest_point(circle.center);
        closest.distance_squared(circle.center) <= circle.radius * circle.radius
    }
}

/// 二维包围圆。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BoundingCircle {
    /// 圆心。
    pub center: Vec2,
    /// 半径。
    pub radius: f32,
}

impl BoundingCircle {
    /// 由圆心与半径构造。半径取绝对值。
    pub fn new(center: Vec2, radius: f32) -> Self {
        Self {
            center,
            radius: radius.abs(),
        }
    }

    /// 包住一组点的圆。
    ///
    /// 用的是「包围盒的外接圆」，**不是最小外接圆**。真正的最小外接圆要跑
    /// Welzl，而包围体的用途是快速粗筛：算得快比裹得紧重要，何况这条路径
    /// 经常每帧重算。
    pub fn from_points(points: &[Vec2]) -> Self {
        let aabb = Aabb2d::from_points(points);
        let center = aabb.center();
        let radius = points
            .iter()
            .map(|point| center.distance_squared(*point))
            .fold(0.0_f32, f32::max)
            .sqrt();
        Self { center, radius }
    }

    /// 面积。
    pub fn area(&self) -> f32 {
        std::f32::consts::PI * self.radius * self.radius
    }

    /// 点在圆内（含边界）。
    pub fn contains_point(&self, point: Vec2) -> bool {
        self.center.distance_squared(point) <= self.radius * self.radius
    }

    /// 完全包住另一个圆。
    pub fn contains(&self, other: &Self) -> bool {
        if other.radius > self.radius {
            return false;
        }
        let gap = self.radius - other.radius;
        self.center.distance_squared(other.center) <= gap * gap
    }

    /// 同时包住两者的圆。
    pub fn merged(&self, other: &Self) -> Self {
        let offset = other.center - self.center;
        let distance = offset.length();

        // 一个已经装下另一个时，直接返回大的那个——按下面的通式算会得到
        // 一个比它还小的圆，反而漏掉一部分。
        if distance + other.radius <= self.radius {
            return *self;
        }
        if distance + self.radius <= other.radius {
            return *other;
        }

        let radius = (distance + self.radius + other.radius) * 0.5;
        // 圆心落在两心连线上，偏向半径小的那边。
        let direction = if distance > f32::EPSILON {
            offset / distance
        } else {
            Vec2::ZERO
        };
        Self {
            center: self.center + direction * (radius - self.radius),
            radius,
        }
    }

    /// 半径外扩。传负数即收缩，但不会小于 0。
    pub fn grown(&self, amount: f32) -> Self {
        Self {
            center: self.center,
            radius: (self.radius + amount).max(0.0),
        }
    }

    /// 圆上（或圆内）离给定点最近的点。
    pub fn closest_point(&self, point: Vec2) -> Vec2 {
        let offset = point - self.center;
        let distance = offset.length();
        if distance <= self.radius || distance < f32::EPSILON {
            return point;
        }
        self.center + offset / distance * self.radius
    }

    /// 和另一个圆有重叠。
    pub fn intersects(&self, other: &Self) -> bool {
        let reach = self.radius + other.radius;
        self.center.distance_squared(other.center) <= reach * reach
    }

    /// 和一个盒子有重叠。
    pub fn intersects_aabb(&self, aabb: &Aabb2d) -> bool {
        aabb.intersects_circle(self)
    }

    /// 外接的轴对齐盒子。
    pub fn aabb(&self) -> Aabb2d {
        Aabb2d::new(self.center, Vec2::splat(self.radius))
    }
}

/// 二维射线：一个起点加一个**归一化**的方向。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Ray2d {
    /// 起点。
    pub origin: Vec2,
    /// 方向，构造时已归一化。
    pub direction: Vec2,
}

impl Ray2d {
    /// 构造一条射线，方向会被归一化。
    ///
    /// 传零向量会得到 `+X` 方向：归一化零向量是 NaN，而 NaN 一旦进了
    /// 相交测试，返回的既不是「撞上」也不是「没撞上」——所有比较都是 false，
    /// 表现为「这条射线什么都打不中」，排查起来毫无线索。
    pub fn new(origin: Vec2, direction: Vec2) -> Self {
        Self {
            origin,
            direction: direction.try_normalize().unwrap_or(Vec2::X),
        }
    }

    /// 射线上距起点 `distance` 处的点。
    pub fn at(&self, distance: f32) -> Vec2 {
        self.origin + self.direction * distance
    }

    /// 打一个盒子，返回命中距离；`max` 之外或打空时为 [`None`]。
    ///
    /// 起点在盒内时返回 `0.0`。
    pub fn hit_aabb(&self, aabb: &Aabb2d, max: f32) -> Option<f32> {
        // 板块法（slab method）：把盒子看成两对平行线，分别求射线进入、
        // 离开每一对的参数区间，两个区间的交集非空就是命中。
        //
        // 方向某个分量为 0 时这里会得到 ±inf，那正是想要的——「永远不会
        // 穿过这一对平行线」，交集运算会自然处理掉。所以不必特判轴对齐，
        // 只要别让 0/0 出现（那是 NaN）。
        let inverse = Vec2::new(1.0 / self.direction.x, 1.0 / self.direction.y);

        let t1 = (aabb.min - self.origin) * inverse;
        let t2 = (aabb.max - self.origin) * inverse;

        let near = t1.min(t2);
        let far = t1.max(t2);

        let enter = near.x.max(near.y).max(0.0);
        let exit = far.x.min(far.y);

        if enter > exit || exit < 0.0 || enter > max {
            return None;
        }
        Some(enter)
    }

    /// 打一个圆，返回命中距离；`max` 之外或打空时为 [`None`]。
    ///
    /// 起点在圆内时返回 `0.0`。
    pub fn hit_circle(&self, circle: &BoundingCircle, max: f32) -> Option<f32> {
        let offset = circle.center - self.origin;

        // 起点已经在里面，谈不上「打进去」，距离就是 0。
        if offset.length_squared() <= circle.radius * circle.radius {
            return Some(0.0);
        }

        // 圆心在射线上的投影。为负说明圆在身后。
        let projection = offset.dot(self.direction);
        if projection < 0.0 {
            return None;
        }

        // 圆心到射线的垂距平方。
        let gap_squared = offset.length_squared() - projection * projection;
        let radius_squared = circle.radius * circle.radius;
        if gap_squared > radius_squared {
            return None;
        }

        let half_chord = (radius_squared - gap_squared).sqrt();
        let distance = projection - half_chord;
        (distance <= max).then_some(distance)
    }
}

impl Aabb2d {
    /// 这个盒子沿 `direction` 移动，最多走 `max`，撞上 `target` 的距离。
    ///
    /// 没撞上返回 [`None`]；出发时就已经重叠则返回 `0.0`。
    ///
    /// 这是**连续**检测：逐帧做相交测试会漏掉快速运动——子弹这一帧在墙前、
    /// 下一帧在墙后，两帧都不相交，于是穿墙而过。
    pub fn sweep_to(&self, direction: Vec2, max: f32, target: &Aabb2d) -> Option<f32> {
        // 闵可夫斯基和：把目标按自己的半尺寸膨胀一圈，问题就退化成
        // 「一条射线（自己的中心）打一个静止的大盒子」。
        // 沿路径一步步试探是另一种做法，但那既慢又会漏掉步长之间的碰撞。
        let expanded = target.grown(self.half_size());
        Ray2d::new(self.center(), direction).hit_aabb(&expanded, max)
    }
}

impl BoundingCircle {
    /// 这个圆沿 `direction` 移动，最多走 `max`，撞上 `target` 的距离。
    ///
    /// 道理同 [`Aabb2d::sweep_to`]，只是膨胀量变成半径相加。
    pub fn sweep_to(&self, direction: Vec2, max: f32, target: &BoundingCircle) -> Option<f32> {
        let expanded = target.grown(self.radius);
        Ray2d::new(self.center, direction).hit_circle(&expanded, max)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn aabb(cx: f32, cy: f32, hx: f32, hy: f32) -> Aabb2d {
        Aabb2d::new(Vec2::new(cx, cy), Vec2::new(hx, hy))
    }

    #[test]
    fn an_aabb_knows_its_shape() {
        let a = aabb(1.0, 2.0, 3.0, 4.0);

        assert_eq!(a.center(), Vec2::new(1.0, 2.0));
        assert_eq!(a.half_size(), Vec2::new(3.0, 4.0));
        assert_eq!(a.size(), Vec2::new(6.0, 8.0));
        assert_eq!(a.area(), 48.0);
    }

    #[test]
    fn negative_half_sizes_do_not_produce_an_inside_out_box() {
        // 反过来的盒子会让所有相交测试静默地永远返回 false。
        let a = Aabb2d::new(Vec2::ZERO, Vec2::new(-2.0, -3.0));

        assert!(a.min.cmple(a.max).all());
        assert_eq!(a.half_size(), Vec2::new(2.0, 3.0));
    }

    #[test]
    fn boxes_touching_at_an_edge_count_as_intersecting() {
        // 边界算相交：物理上「刚好贴住」通常就该判定为接触了。
        let a = aabb(0.0, 0.0, 1.0, 1.0);
        let b = aabb(2.0, 0.0, 1.0, 1.0);

        assert!(a.intersects(&b));
        assert!(!a.intersects(&aabb(2.01, 0.0, 1.0, 1.0)));
    }

    #[test]
    fn a_box_and_a_circle_meet_at_the_closest_point() {
        let a = aabb(0.0, 0.0, 1.0, 1.0);

        // 圆心在角外，但半径够长够到角上。
        assert!(a.intersects_circle(&BoundingCircle::new(Vec2::new(1.5, 1.5), 0.8)));
        // 差一点：到角的距离是 √2/2·… 这里取一个明显够不着的半径。
        assert!(!a.intersects_circle(&BoundingCircle::new(Vec2::new(1.5, 1.5), 0.5)));
        // 圆心在盒内，无论多小都算。
        assert!(a.intersects_circle(&BoundingCircle::new(Vec2::ZERO, 0.01)));
    }

    #[test]
    fn merging_circles_keeps_both_inside() {
        let a = BoundingCircle::new(Vec2::new(-2.0, 0.0), 1.0);
        let b = BoundingCircle::new(Vec2::new(3.0, 0.0), 2.0);
        let merged = a.merged(&b);

        assert!(merged.contains(&a), "合并后没装下 a");
        assert!(merged.contains(&b), "合并后没装下 b");
    }

    #[test]
    fn merging_a_contained_circle_changes_nothing() {
        // 通式在这种情形下会算出一个比原来还小的圆，必须特判。
        let big = BoundingCircle::new(Vec2::ZERO, 5.0);
        let small = BoundingCircle::new(Vec2::new(1.0, 0.0), 1.0);

        assert_eq!(big.merged(&small), big);
        assert_eq!(small.merged(&big), big);
    }

    #[test]
    fn a_ray_hits_a_box_from_outside_and_reports_the_entry_distance() {
        let ray = Ray2d::new(Vec2::new(-5.0, 0.0), Vec2::X);
        let hit = ray.hit_aabb(&aabb(0.0, 0.0, 1.0, 1.0), 100.0);

        assert_eq!(hit, Some(4.0));
    }

    #[test]
    fn a_ray_starting_inside_a_box_hits_at_zero() {
        let ray = Ray2d::new(Vec2::ZERO, Vec2::X);
        assert_eq!(ray.hit_aabb(&aabb(0.0, 0.0, 1.0, 1.0), 100.0), Some(0.0));
    }

    #[test]
    fn a_ray_pointing_away_misses() {
        let ray = Ray2d::new(Vec2::new(-5.0, 0.0), Vec2::NEG_X);
        assert_eq!(ray.hit_aabb(&aabb(0.0, 0.0, 1.0, 1.0), 100.0), None);
    }

    #[test]
    fn the_max_distance_is_respected() {
        let ray = Ray2d::new(Vec2::new(-5.0, 0.0), Vec2::X);
        let target = aabb(0.0, 0.0, 1.0, 1.0);

        assert_eq!(ray.hit_aabb(&target, 3.0), None, "还没走到就该算没撞上");
        assert_eq!(ray.hit_aabb(&target, 4.0), Some(4.0));
    }

    #[test]
    fn an_axis_aligned_ray_does_not_produce_nan() {
        // 方向某个分量为 0 时，板块法里会出现 1/0 = inf。inf 是对的
        // （「永远穿不过这对平行线」），0/0 才是 NaN——而 NaN 会让所有
        // 比较都返回 false，表现成「这条射线什么都打不中」。
        let ray = Ray2d::new(Vec2::new(0.0, -5.0), Vec2::Y);
        assert_eq!(ray.hit_aabb(&aabb(0.0, 0.0, 1.0, 1.0), 100.0), Some(4.0));

        let miss = Ray2d::new(Vec2::new(9.0, -5.0), Vec2::Y);
        assert_eq!(miss.hit_aabb(&aabb(0.0, 0.0, 1.0, 1.0), 100.0), None);
    }

    #[test]
    fn a_zero_direction_ray_does_not_poison_everything_with_nan() {
        let ray = Ray2d::new(Vec2::ZERO, Vec2::ZERO);
        assert!(ray.direction.is_finite());
        assert!((ray.direction.length() - 1.0).abs() < 1e-6);
    }

    #[test]
    fn a_ray_hits_a_circle_at_the_near_side() {
        let ray = Ray2d::new(Vec2::new(-5.0, 0.0), Vec2::X);
        let hit = ray.hit_circle(&BoundingCircle::new(Vec2::ZERO, 2.0), 100.0);

        assert_eq!(hit, Some(3.0));
    }

    #[test]
    fn a_ray_grazing_past_a_circle_misses() {
        let ray = Ray2d::new(Vec2::new(-5.0, 3.0), Vec2::X);
        assert_eq!(
            ray.hit_circle(&BoundingCircle::new(Vec2::ZERO, 2.0), 100.0),
            None
        );
    }

    #[test]
    fn sweeping_a_box_catches_what_frame_by_frame_tests_would_miss() {
        // 这才是扫掠存在的理由：起点和终点都不重叠，中间却穿过去了。
        // 逐帧做相交测试的话，这一帧在墙左、下一帧在墙右，两帧都「没碰上」。
        let bullet = aabb(-10.0, 0.0, 0.5, 0.5);
        let wall = aabb(0.0, 0.0, 1.0, 5.0);

        let far_end = aabb(10.0, 0.0, 0.5, 0.5);
        assert!(!bullet.intersects(&wall), "出发时不该重叠");
        assert!(!far_end.intersects(&wall), "落点也不重叠");

        let hit = bullet.sweep_to(Vec2::X, 20.0, &wall).expect("该撞上墙");
        // 子弹半宽 0.5，墙左面在 -1，所以中心走到 -1.5 时贴上。
        assert!((hit - 8.5).abs() < 1e-4, "撞上的距离是 {hit}");
    }

    #[test]
    fn sweeping_reports_zero_when_already_overlapping() {
        let a = aabb(0.0, 0.0, 1.0, 1.0);
        let b = aabb(0.5, 0.0, 1.0, 1.0);

        assert_eq!(a.sweep_to(Vec2::X, 10.0, &b), Some(0.0));
    }

    #[test]
    fn sweeping_a_circle_uses_the_summed_radius() {
        let moving = BoundingCircle::new(Vec2::new(-10.0, 0.0), 1.0);
        let target = BoundingCircle::new(Vec2::ZERO, 2.0);

        let hit = moving.sweep_to(Vec2::X, 20.0, &target).expect("该撞上");

        // 两半径之和是 3，所以圆心走到 -3 时贴上：10 - 3 = 7。
        assert!((hit - 7.0).abs() < 1e-4, "撞上的距离是 {hit}");
    }

    #[test]
    fn a_sweep_that_falls_short_reports_nothing() {
        let bullet = aabb(-10.0, 0.0, 0.5, 0.5);
        let wall = aabb(0.0, 0.0, 1.0, 5.0);

        assert_eq!(bullet.sweep_to(Vec2::X, 5.0, &wall), None);
    }
}
