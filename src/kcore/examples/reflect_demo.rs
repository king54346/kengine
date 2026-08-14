//! `reflect` 模块用途演示。
//!
//! 运行：`cargo run -p kcore --example reflect_demo`
//!
//! Rust 是静态语言，编译后字段名、字段类型这些信息就消失了。
//! `Reflect` 通过 derive 宏把它们**保留到运行时**，于是可以用字符串
//! 来遍历、读取、修改一个对象的任意深层字段——这正是游戏引擎编辑器
//! 的属性面板、动画系统、撤销/重做、预制体继承所依赖的能力。

use kcore::reflect::prelude::*;
use kcore::variable::InheritableVariable;

#[derive(Reflect, Clone, Debug, Default, PartialEq)]
#[reflect(type_uuid = "1f4c3a10-8b2e-4a55-9d31-6e0a7c9b1d20")]
struct Light {
    /// 光照强度。
    #[reflect(
        display_name = "光照强度",
        min_value = "0.0",
        max_value = "10.0",
        step = "0.1"
    )]
    intensity: f32,

    /// 是否投射阴影。
    #[reflect(display_name = "投射阴影")]
    cast_shadows: bool,

    /// 内部缓存，不希望暴露给编辑器。
    #[reflect(hidden)]
    _cached_shadow_map_id: u32,
}

#[derive(Reflect, Clone, Debug, Default, PartialEq)]
#[reflect(type_uuid = "2c8d5b71-3f6a-4e18-b7c2-95a1e4d80f37")]
struct Node {
    #[reflect(display_name = "节点名称")]
    name: String,

    #[reflect(display_name = "缩放", min_value = "0.01", precision = "3")]
    scale: f32,

    #[reflect(display_name = "光源")]
    light: InheritableVariable<Light>,

    #[reflect(display_name = "子节点")]
    children: Vec<Node>,
}

fn main() {
    let mut node = Node {
        name: "SpotLight".to_string(),
        scale: 1.0,
        light: Light {
            intensity: 2.0,
            cast_shadows: true,
            _cached_shadow_map_id: 42,
        }
        .into(),
        children: vec![Node {
            name: "Child".to_string(),
            scale: 0.5,
            ..Default::default()
        }],
    };

    demo_1_inspector(&node);
    demo_2_all_paths(&node);
    demo_3_set_by_path(&mut node);
    demo_4_prefab_inheritance();
}

/// 用途 1：自动生成编辑器属性面板。
///
/// 编辑器完全不认识 `Node` 这个类型，却能列出它的每个字段、
/// 显示名、数值范围，从而决定该渲染成滑条还是复选框。
fn demo_1_inspector(node: &Node) {
    println!("=== 1. 自动生成属性面板（编辑器不需要知道 Node 是什么）===");
    node.fields_ref(&mut |fields| {
        for field in fields {
            let widget = match (field.min_value, field.max_value) {
                (Some(min), Some(max)) => format!("滑条 [{min} ..= {max}]"),
                _ => "输入框".to_string(),
            };
            let mut value = format!("{:?}", field.value);
            if value.chars().count() > 40 {
                value = value.chars().take(40).collect::<String>() + "...";
            }
            println!(
                "  {:<8} 显示名={:<10} 控件={:<20} 当前值={value}",
                field.name, field.display_name, widget
            );
        }
    });
    println!("  注意：Light 里的 _cached_shadow_map_id 带 #[reflect(hidden)]，下面不会出现\n");
}

/// 用途 2：递归枚举出对象里每一个属性的「路径」。
///
/// 这些字符串路径就是动画曲线的绑定目标、撤销/重做记录的操作对象、
/// 以及场景差分（diff）的键。
fn demo_2_all_paths(node: &Node) {
    println!("=== 2. 递归枚举所有字段路径 ===");
    (node as &dyn Reflect).enumerate_fields_recursively(
        &mut |path, _field, _value| {
            if !path.is_empty() {
                println!("  {path}");
            }
        },
        &[],
    );
    println!();
}

/// 用途 3：用一个字符串修改任意深度的字段。
///
/// 这是反射最核心的价值：调用方只有一个 `&mut dyn Reflect` 和一个字符串，
/// 没有任何 `Node`/`Light` 的静态类型信息，却能精确写入。
/// 动画播放器、脚本绑定、编辑器 UI 全都走这条路。
fn demo_3_set_by_path(node: &mut Node) {
    println!("=== 3. 按字符串路径读写深层字段 ===");

    // 注意：`light` 被 `InheritableVariable` 包着，穿过它要多写一层 `Content`。
    // （`enumerate_fields_recursively` 报告的路径省略了 Content，两者并不通用。）
    node.resolve_path("light.Content.intensity", &mut |result| {
        println!("  修改前 light.intensity = {:?}", result.unwrap());
    });

    (node as &mut dyn Reflect).set_field_by_path(
        "light.Content.intensity",
        Box::new(7.5f32),
        &mut |result| match result {
            Ok(old) => println!("  写入成功，旧值 = {old:?}"),
            Err(err) => println!("  写入失败：{err:?}"),
        },
    );
    println!("  修改后 light.intensity = {}", node.light.intensity);

    // 集合同样支持，索引用方括号。
    (node as &mut dyn Reflect).set_field_by_path(
        "children[0].scale",
        Box::new(2.0f32),
        &mut |result| println!("  写入 children[0].scale：{:?}", result.is_ok()),
    );
    println!("  修改后 children[0].scale = {}", node.children[0].scale);

    // 路径写错不会 panic，而是返回错误——编辑器可以直接把它显示给用户。
    (node as &mut dyn Reflect).set_field_by_path(
        "light.no_such_field",
        Box::new(1.0f32),
        &mut |result| {
            if result.is_err() {
                println!("  写入不存在的字段：如预期地失败了");
            }
        },
    );
    println!();
}

/// 用途 4：预制体（prefab）属性继承。
///
/// `InheritableVariable` 记录一个值「有没有被用户手动改过」。
/// 场景实例从预制体继承属性时，**只覆盖用户没动过的字段**，
/// 用户手动调过的值保持不变。这是 Unity 预制体 override 的同款机制，
/// 而它的实现依赖 `Reflect` 递归遍历整棵对象树。
fn demo_4_prefab_inheritance() {
    println!("=== 4. 预制体属性继承（InheritableVariable）===");

    let mut instance = InheritableVariable::new_non_modified(1.0f32);
    println!("  实例初始值 = {}，被改过吗 = {}", *instance, instance.is_modified());

    let parent = InheritableVariable::new_non_modified(9.0f32);
    instance
        .try_inherit(&parent, &[])
        .expect("继承应当成功");
    println!("  未被手动修改 → 继承预制体的值 = {}", *instance);

    // 用户在编辑器里手动拖动了这个属性。
    instance.set_value_and_mark_modified(3.0);
    println!("  用户手动改成 = {}，被改过吗 = {}", *instance, instance.is_modified());

    let parent = InheritableVariable::new_non_modified(100.0f32);
    instance.try_inherit(&parent, &[]).expect("继承应当成功");
    println!(
        "  预制体改成 100，但实例已被手动修改 → 保持 = {}（不被覆盖）",
        *instance
    );
}
