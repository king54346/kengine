//! 用真实资源读小桑村（`scn/fb_xiaosangcun`，`area_info` 第 2107 条 → `scn_info` 80~89）。资源不在就跳过。

use std::path::PathBuf;

use kxunxian::Library;

#[test]
fn reads_xiaosangcun() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/unpack2");
    if !root.join("scn/fb_xiaosangcun").is_dir() {
        return;
    }
    let mut library = Library::new(&root);
    let scene = library.scene("fb_xiaosangcun").unwrap();
    assert_eq!(scene.units.len(), 11);
    assert!(
        scene.units.iter().all(|u| u.preblend.is_some()),
        "有单元没有预混贴图"
    );
    // 村长站的地方：标记的高度和地形高度对得上（误差在一两米内——他站在台阶上）。
    let chief = scene.mark("nhx_xsc_luocunzhang").expect("找不到罗村长");
    let ground = scene.height_at(chief.position).expect("村长不在地形上");
    assert!(
        (ground - chief.position.y).abs() < 1.5,
        "地面 {ground}，村长 {}",
        chief.position.y
    );
    // 三层物件都展开了：小物件三千多个，加上树和建筑群。
    println!("摆放 {} 个", scene.placements.len());
    assert!(
        scene.placements.len() > 3200,
        "物件只有 {} 个",
        scene.placements.len()
    );
    let groups = kxunxian::group_placements(&scene.placements);
    let buildings = groups
        .iter()
        .filter(|(model, _)| model.contains("xiaosangcun") || model.contains("jzxsc"))
        .count();
    assert!(buildings > 3, "小桑村的房子只有 {buildings} 种");
    // 除了纯粒子特效的物件（蝴蝶、火把，`.gfx` 还不支持），每种物件都能做出网格。
    let mut empty = Vec::new();
    for (model, _) in &groups {
        if !library.static_object(model).is_empty() {
            continue;
        }
        let path = library.resolve(model).expect("物件文件找不到");
        let text = kxunxian::xml::decode(&std::fs::read(path).unwrap());
        if !text.contains("<Particle") {
            empty.push(model.clone());
        }
    }
    assert!(
        empty.is_empty(),
        "{} 种物件没有网格：{:?}",
        empty.len(),
        empty
    );

    // 大海是 0 号水面（haishui，-3 米）；(8,3) 那块整个是海，没有高度图。
    assert_eq!(scene.sea, Some(0));
    assert!(scene.sea_mesh(100.0).is_some());
    assert!(scene.units.iter().any(|u| !u.terrain));
    // 地表分层：有地形的单元都有明暗，几乎都有分层权重（(8,6) 的层掩码是 0，退回预混贴图），
    // 每个单元一份材质。
    assert!(scene.layers.iter().flatten().count() >= 10);
    let terrain: Vec<_> = scene.units.iter().filter(|u| u.terrain).collect();
    assert!(
        terrain.iter().all(|u| u.colors.is_some()),
        "有地形却没有明暗"
    );
    let blended: Vec<_> = terrain.iter().filter_map(|u| u.blend.as_ref()).collect();
    assert!(
        blended.len() + 1 >= terrain.len(),
        "只有 {} 个单元有分层",
        blended.len()
    );
    // 底层在陆地上铺满；海底（(9,3)、(10,x) 那几块）的底层权重不满——游戏在那里让地面透明，
    // 露出水。村子所在的 (9,5) 至少 99% 是满的。
    let village = scene.unit(9, 5).and_then(|u| u.blend.as_ref()).unwrap();
    let full = village.weights[0].iter().filter(|&&w| w == 255).count();
    assert!(
        full * 100 >= village.weights[0].len() * 99,
        "村子的底层权重不满"
    );
    assert_eq!(library.terrain_materials(&scene).len(), scene.units.len());
    // 碰撞：房子、台阶、树干都有碰撞网格，而且都读得出来。
    println!("碰撞 {} 块", scene.colliders.len());
    assert!(
        scene.colliders.len() > 500,
        "碰撞只有 {} 块",
        scene.colliders.len()
    );
    let mut meshes: Vec<_> = scene
        .colliders
        .iter()
        .map(|c| c.mesh.to_lowercase())
        .collect();
    meshes.sort();
    meshes.dedup();
    let broken: Vec<_> = meshes
        .iter()
        .filter(|m| library.collision_mesh(m).is_none())
        .collect();
    assert!(
        broken.len() * 20 < meshes.len(),
        "{} / {} 种碰撞网格读不出来：{:?}",
        broken.len(),
        meshes.len(),
        broken
    );
}
