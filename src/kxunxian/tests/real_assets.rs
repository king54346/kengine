//! 用真实的解包资源跑一遍（`assets/unpack2` 不在就跳过）：书生穿一套衣服、右手拿剑、背上背法宝，
//! 读两段动作。

use std::path::PathBuf;

use kxunxian::{Library, Outfit};

fn library() -> Option<Library> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/unpack2");
    root.join("cha/special")
        .is_dir()
        .then(|| Library::new(root))
}

#[test]
fn builds_dressed_and_armed_character() {
    let Some(mut library) = library() else { return };
    let character = library.character("zj_shusheng_007").unwrap();
    assert!(character.bones.len() > 50, "骨架没读全");
    assert!(character.bone("Bip01 R Hand").is_some());

    let outfit = Outfit {
        parts: [
            "M_hd001_hd_001",
            "m_yf001_st_001",
            "m_yf001_qz_001",
            "m_yf001_gl_003",
            "m_yf001_gr_003",
            "M_kz001_kz_001",
            "M_xz001_xz_001",
            "m_mz001_mz_001",
        ]
        .map(String::from)
        .to_vec(),
        equips: ["e_wqa_101_2", "e_fba_101"].map(String::from).to_vec(),
        markers: vec!["ride_tun".into()],
    };
    let model = library.build(&character, &outfit);
    // 每个部件一块网格，武器一块、法宝一块。
    assert_eq!(
        model.meshes().len(),
        outfit.parts.len() + 2,
        "有部件或装备没读出来"
    );
    for (index, mesh) in model.meshes().iter().enumerate() {
        assert!(!mesh.vertices().is_empty(), "第 {index} 块网格是空的");
    }
    // 身体部件都是蒙皮的，武器和法宝不是。
    let skinned = model
        .nodes()
        .iter()
        .filter(|node| node.skin.is_some())
        .count();
    assert_eq!(skinned, outfit.parts.len());
    // 武器挂在右手骨骼下面。
    let hand = character.bone("Bip01 R Hand").unwrap();
    let mount = model.find_node("e_wqa_101_2").unwrap();
    assert!(model.nodes()[hand].children.contains(&mount));
    // 挂点标记是空节点，挂在骨盆下面（`ride_tun` 的骨骼号是 1）。
    let marker = model.find_node("ride_tun").unwrap();
    assert!(model.nodes()[marker].parts.is_empty());
    assert!(
        model
            .nodes()
            .iter()
            .any(|node| node.children.contains(&marker))
    );
    // 每块网格都拿到了贴图。
    for material in model.materials() {
        assert!(material.base_color_texture().is_some(), "有材质没贴图");
    }

    for code in ["zl01", "pb01", "gj01"] {
        let clip = library.animation(&character, code).unwrap();
        assert!(clip.duration() > 0.1, "{code} 时长不对");
        // 上下半身合并之后，根骨骼和右手都有轨道。
        assert!(
            clip.tracks().iter().any(|track| track.target == 0),
            "{code} 没有根骨骼轨道"
        );
        assert!(
            clip.tracks().iter().any(|track| track.target == hand),
            "{code} 没有右手轨道"
        );
    }

    // 骑乘放技能：上半身攻击、下半身坐姿。下半身（左大腿）和坐姿一样，上半身（右手）和攻击一样。
    let layered = library
        .layered_animation(&character, "gj01", "qczz01", "ride_gj01")
        .unwrap();
    let ride = library.animation(&character, "qczz01").unwrap();
    let attack = library.animation(&character, "gj01").unwrap();
    let thigh = character.bone("Bip01 L Thigh").unwrap();
    let rotation = |clip: &kanim::AnimationClip, target: usize| {
        clip.tracks()
            .iter()
            .find(|track| {
                track.target == target && matches!(track.channel, kanim::Channel::Rotation(_))
            })
            .map(|track| format!("{:?}", track.channel))
    };
    assert!(rotation(&layered, thigh).is_some());
    assert_eq!(rotation(&layered, thigh), rotation(&ride, thigh));
    assert_eq!(rotation(&layered, hand), rotation(&attack, hand));

    // 坐骑：怪物骨架，动作是单文件。
    let mount = library.character("gw_cwbailang_919").unwrap();
    let run = library.animation(&mount, "pb01").unwrap();
    assert!(run.duration() > 0.1);
    assert!(mount.def.hinges.contains_key("ride_0"));
}
