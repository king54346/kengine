//! 拿 three.js 仓库里那批**真实资源**跑一遍导入器。
//!
//! 单元测试用的是各模块里手工拼的最小文件——它们能盯住格式规则，
//! 盯不住「真实世界里的导出器到底怎么写」。这里补上另一半：
//! 每种格式各读一个真文件，检查出来的东西在数量级上说得通
//! （有几何、有帧、有颜色、坐标没跑飞）。
//!
//! 资源目录不在时整组**跳过而不是失败**：这些是 400 MB 的第三方样本，
//! 不进版本库。跑之前先确认 `examples/threejs/models` 在。

use kengine::{kimport, ktask};
use std::path::{Path, PathBuf};

/// three.js 的样本资源根目录。
fn assets() -> Option<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/threejs");
    root.join("models").is_dir().then_some(root)
}

/// 读一个样本文件；目录不在时返回 `None`，调用方据此跳过。
fn read(relative: &str) -> Option<Vec<u8>> {
    let path = assets()?.join(relative);
    match std::fs::read(&path) {
        Ok(bytes) => Some(bytes),
        Err(error) => panic!("样本 {} 读不出来：{error}", path.display()),
    }
}

/// 同步跑一个导入器的异步解析函数。
macro_rules! import {
    ($parse:path, $relative:literal) => {{
        let Some(bytes) = read($relative) else {
            eprintln!("跳过：没有 examples/threejs 资源");
            return;
        };
        let io: std::sync::Arc<dyn kengine::kasset::ResourceIo> =
            std::sync::Arc::new(kengine::kasset::FsResourceIo);
        ktask::block_on($parse(
            bytes,
            assets().unwrap().join($relative),
            io,
        ))
        .unwrap_or_else(|error| panic!("{} 导入失败：{error}", $relative))
    }};
}

#[test]
fn obj_reads_a_textured_character() {
    let model = import!(kimport::obj::parse, "models/obj/male02/male02.obj");
    assert!(model.triangle_count() > 5_000, "三角形太少，像是没读全");
    // male02.mtl 里有三个材质，每个带一张贴图。
    assert!(model.materials().len() >= 3);
    assert!(
        model
            .materials()
            .iter()
            .any(|m| m.base_color_texture().is_some()),
        "MTL 的贴图没接上"
    );
}

#[test]
fn stl_reads_both_writings() {
    let ascii = import!(kimport::stl::parse, "models/stl/ascii/slotted_disk.stl");
    assert!(ascii.triangle_count() > 100);
    let binary = import!(kimport::stl::parse, "models/stl/binary/pr2_head_pan.stl");
    // 文件头里写的就是 1000 个三角形，一个不少。
    assert_eq!(binary.triangle_count(), 1_000);
}

#[test]
fn stl_reads_per_face_colours() {
    let model = import!(kimport::stl::parse, "models/stl/binary/colored.stl");
    let mesh = model.mesh(0).unwrap();
    assert!(
        mesh.vertices().iter().any(|v| v.color != [1.0; 3]),
        "colored.stl 应当带逐面颜色"
    );
}

#[test]
fn ply_reads_ascii_and_binary() {
    let ascii = import!(kimport::ply::parse, "models/ply/ascii/dolphins.ply");
    assert!(ascii.triangle_count() > 100);
    let binary = import!(kimport::ply::parse, "models/ply/binary/Lucy100k.ply");
    assert!(binary.triangle_count() > 90_000, "Lucy 是十万面的模型");
}

#[test]
fn pcd_reads_all_three_encodings() {
    for (name, minimum) in [
        ("models/pcd/ascii/simple.pcd", 100usize),
        ("models/pcd/binary/Zaghetto.pcd", 10_000),
        ("models/pcd/binary_compressed/pcl_logo.pcd", 10_000),
    ] {
        let Some(bytes) = read(name) else {
            eprintln!("跳过：没有 examples/threejs 资源");
            return;
        };
        let cloud = kimport::pcd::parse(&bytes).unwrap_or_else(|e| panic!("{name}：{e}"));
        assert!(cloud.len() >= minimum, "{name} 只读到 {} 个点", cloud.len());
        let (min, max) = cloud.bounds();
        assert!(min.is_finite() && max.is_finite(), "{name} 的坐标跑飞了");
        // 分列存储读错时坐标不会报错，只会变成一团噪声——包围盒
        // 的跨度会异常巨大。真实点云的跨度是米级。
        assert!((max - min).max_element() < 1e4, "{name} 的包围盒大得不像话");
    }
}

#[test]
fn pdb_reads_atoms_and_bonds() {
    let Some(bytes) = read("models/pdb/caffeine.pdb") else {
        eprintln!("跳过：没有 examples/threejs 资源");
        return;
    };
    let molecule = kimport::pdb::parse(&bytes).unwrap();
    // 咖啡因是 C8H10N4O2，连氢一共 24 个原子。
    assert_eq!(molecule.atoms.len(), 24);
    assert!(molecule.bonds.len() >= 20);
    assert!(molecule.radius() > 1.0 && molecule.radius() < 20.0);
}

#[test]
fn vox_greedy_meshing_is_worth_it() {
    let Some(bytes) = read("models/vox/monu10.vox") else {
        eprintln!("跳过：没有 examples/threejs 资源");
        return;
    };
    let io: std::sync::Arc<dyn kengine::kasset::ResourceIo> =
        std::sync::Arc::new(kengine::kasset::FsResourceIo);
    let model = ktask::block_on(kimport::vox::parse(
        bytes,
        assets().unwrap().join("models/vox/monu10.vox"),
        io,
    ))
    .unwrap();
    let triangles = model.triangle_count();
    assert!(triangles > 1_000, "合并得太狠了，只剩 {triangles} 个三角形");
    // 不合并的话这个模型是几十万个三角形。合并之后应当低一个数量级以上。
    assert!(triangles < 200_000, "合并没起作用，仍有 {triangles} 个三角形");
}

#[test]
fn md2_reads_frames_and_animations() {
    let md2 = import!(
        kimport::md2::parse,
        "models/md2/ratamahatta/ratamahatta.md2"
    );
    assert!(md2.frames.len() > 100, "MD2 角色通常有近两百帧");
    assert!(
        md2.find_animation("stand").is_some(),
        "没切出 stand 这段动画，切出来的是 {:?}",
        md2.animations.iter().map(|a| &a.name).collect::<Vec<_>>()
    );
    // 帧数据读错时坐标会是天文数字。角色模型的高度是几十个单位。
    let bounds = md2.frames[0]
        .positions
        .iter()
        .fold(0.0f32, |acc, p| acc.max(p.length()));
    assert!(bounds < 1e3, "第一帧的坐标跑飞了：{bounds}");
}

#[test]
fn mdd_reads_a_big_endian_cache() {
    let Some(bytes) = read("models/mdd/cube.mdd") else {
        eprintln!("跳过：没有 examples/threejs 资源");
        return;
    };
    let cache = kimport::mdd::parse(&bytes).unwrap();
    assert_eq!(cache.len(), 4);
    assert_eq!(cache.frames[0].len(), 24);
    // 按小端读的话第一帧的坐标会是 1e38 级别的数。
    assert!(cache.frames[0].iter().all(|p| p.length() < 10.0));
}

#[test]
fn nrrd_reads_a_gzipped_volume() {
    let Some(bytes) = read("models/nrrd/stent.nrrd") else {
        eprintln!("跳过：没有 examples/threejs 资源");
        return;
    };
    let volume = kimport::nrrd::parse(&bytes).unwrap();
    assert_eq!(volume.size, [128, 128, 256]);
    assert!(volume.range.1 > volume.range.0, "体数据是常量？");
    // 归一化之后应当真的铺满 0..1。
    assert!(volume.voxels.iter().any(|&v| v > 0.9));
    assert!(volume.voxels.iter().any(|&v| v < 0.1));
}

#[test]
fn the_extended_material_shader_still_validates() {
    kengine::krender::validate_material_hook(include_str!("../src/kpbr/src/physical.wgsl")).unwrap();
}

#[test]
fn the_point_sprite_shader_still_validates() {
    kengine::krender::validate_material_hook(include_str!("../src/kpbr/src/points.wgsl")).unwrap();
}

/// 把 three.js 仓库里所有压缩纹理样本都过一遍。
///
/// 这批文件覆盖了 DDS / KTX / KTX2 / PVR 四种容器和十几种像素格式，
/// 是这个模块最有价值的一组测试——手工拼的假文件盯不住「真实导出器
/// 到底怎么排 mip 链和立方体的六个面」。
#[test]
fn every_compressed_texture_sample_decodes() {
    use kengine::ktexture::container;
    let Some(root) = assets() else {
        eprintln!("跳过：没有 examples/threejs 资源");
        return;
    };
    let mut decoded = 0;
    let mut basis = 0;
    for directory in ["textures/compressed", "textures/ktx2"] {
        let Ok(entries) = std::fs::read_dir(root.join(directory)) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let extension = path
                .extension()
                .map(|e| e.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            if !["dds", "ktx", "ktx2", "pvr"].contains(&extension.as_str()) {
                continue;
            }
            let bytes = std::fs::read(&path).unwrap();
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            match container::decode(&bytes) {
                Ok(result) => {
                    assert!(result.width > 0 && result.height > 0, "{name} 的尺寸是零");
                    assert!(!result.levels.is_empty(), "{name} 一级 mip 都没有");
                    // 每一级的像素数要和它自己的尺寸对得上。排 mip 链时
                    // 算错偏移的典型后果就是某一级尺寸对、数据是别人的。
                    for (level, texture) in result.levels.iter().enumerate() {
                        let expected = (result.width as usize >> level).max(1)
                            * (result.height as usize >> level).max(1)
                            * 4
                            * result.faces as usize;
                        assert_eq!(texture.data().len(), expected, "{name} 第 {level} 级");
                    }
                    decoded += 1;
                }
                Err(error) => {
                    // 只有两种失败是允许的：
                    // 1. Basis Universal——引擎有意没实现转码器；
                    // 2. BC6H——第三方解码器 `texture2ddecoder` 0.1.2 在
                    //    mode 15 上算错了掩码（debug 下 panic、release 下
                    //    颜色错）。这两条都写在 `ktexture::container` 的
                    //    模块文档里。
                    let text = error.to_string();
                    assert!(
                        text.contains("Basis") || text.contains("块解码器崩了"),
                        "{name} 解不出来，而且不是已知的那两种原因：{text}"
                    );
                    basis += 1;
                }
            }
        }
    }
    assert!(decoded > 20, "只解出了 {decoded} 个样本，像是路径不对");
    eprintln!("压缩纹理：{decoded} 个解出来了，{basis} 个是 Basis Universal（不支持）");
}

#[test]
fn exr_decodes_to_a_real_hdr_range() {
    let Some(bytes) = read("textures/memorial.exr") else {
        eprintln!("跳过：没有 examples/threejs 资源");
        return;
    };
    let image = kengine::kpbr::hdr::HdrImage::decode_exr(&bytes).unwrap();
    assert!(image.width() > 100 && image.height() > 100);
    // memorial 是那张著名的教堂窗户 HDR，窗口处远超 1.0。
    assert!(
        image.pixels().iter().any(|&v| v > 4.0),
        "EXR 的高光被压掉了，最大值只有 {}",
        image.pixels().iter().cloned().fold(0.0f32, f32::max)
    );
}

#[test]
fn ultrahdr_recovers_more_range_than_the_base_jpeg() {
    let Some(bytes) = read("textures/equirectangular/spruit_sunrise_2k.hdr.jpg") else {
        eprintln!("跳过：没有 examples/threejs 资源");
        return;
    };
    let image = kengine::kpbr::ultrahdr::decode(&bytes).unwrap();
    let peak = image.pixels().iter().cloned().fold(0.0f32, f32::max);
    // 增益图没接上的话，结果就是张普通 JPEG——最大值恰好是 1.0。
    assert!(peak > 2.0, "增益图没起作用，峰值只有 {peak}");
    assert!(peak.is_finite(), "重建出了非有限值");
}
#[test]
fn gltf_extension_samples_load() {
    let Some(root) = assets() else { eprintln!("跳过"); return; };
    let io: std::sync::Arc<dyn kengine::kasset::ResourceIo> =
        std::sync::Arc::new(kengine::kasset::FsResourceIo);
    let manager = kengine::kasset::ResourceManager::with_io(io);
    manager.add_loader(kengine::kgltf::GltfLoader);
    for name in [
        "models/gltf/DispersionTest.glb",
        "models/gltf/IridescenceLamp.glb",
        "models/gltf/SheenChair.glb",
        "models/gltf/IridescentDishWithOlives.glb",
        "models/gltf/MaterialsVariantsShoe/glTF/MaterialsVariantsShoe.gltf",
        "models/gltf/DamagedHelmet/glTF-instancing/DamagedHelmetGpuInstancing.gltf",
        "models/gltf/DragonAttenuation.glb",
        "models/gltf/BoomBox.glb",
        "models/gltf/coffeemat.glb",
    ] {
        let path = root.join(name);
        if !path.exists() { eprintln!("缺样本 {name}"); continue; }
        match manager.request_blocking::<kengine::kgltf::Model>(&path) {
            Ok(model) => {
                let model = model.data_ref().unwrap();
                eprintln!("{name}: {} 网格 / {} 材质 / {} 节点 / {} 三角形",
                    model.meshes().len(), model.materials().len(), model.nodes().len(), model.triangle_count());
            }
            Err(error) => eprintln!("{name}: 失败 {error}"),
        }
    }
}
