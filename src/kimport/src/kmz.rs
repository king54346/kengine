//! KMZ（Google Earth 的模型包）：一个 ZIP，里面是 `doc.kml` + Collada 模型 + 贴图。
//!
//! KML 里 `<Model><Link><href>` 指向 `.dae`；没写（或找不到）时取包里第一个
//! `.dae`。Collada 引用的贴图按 **`.dae` 所在目录** 在包内解析，找不到再按
//! 文件名在整个包里找——KMZ 里的相对路径经常和实际打包的层级对不上。
//!
//! KML 本身的地理信息（经纬度、朝向、比例）不读：引擎没有地理坐标系，
//! 模型按 Collada 自己的坐标摆放。

use crate::{bad, collada, loader, xml, zip};
use kasset::{LoadError, ResourceIo};
use kgltf::{MODEL_TYPE_UUID, Model};
use std::{path::PathBuf, sync::Arc};

loader! {
    /// 读 `.kmz`。
    KmzLoader -> Model : ["kmz"] = MODEL_TYPE_UUID, parse
}

/// 解析 KMZ。
pub async fn parse(bytes: Vec<u8>, path: PathBuf, _io: Arc<dyn ResourceIo>) -> Result<Model, LoadError> {
    let archive = zip::Archive::open(&bytes)?;
    let from_kml = archive
        .find_extension("kml")
        .and_then(|e| archive.read(e).ok())
        .and_then(|kml| xml::parse(&kml).ok())
        .and_then(|kml| {
            let mut links = Vec::new();
            kml.descendants("Model", &mut links);
            links
                .into_iter()
                .find_map(|model| model.find("Link/href").map(|h| h.text.trim().to_string()))
        })
        .filter(|href| archive.find(href).is_some());
    let dae = match from_kml {
        Some(href) => href,
        None => archive
            .entries()
            .iter()
            // macOS 打包会塞进 `__MACOSX/._xxx` 这种资源分叉文件，跳过。
            .find(|e| e.name.to_ascii_lowercase().ends_with(".dae") && !e.name.starts_with("__MACOSX"))
            .map(|e| e.name.clone())
            .ok_or_else(|| bad("KMZ 里没有 .dae 模型"))?,
    };
    let data = archive.read_named(&dae).ok_or_else(|| bad(format!("KMZ 里的 {dae} 读不出来")))?;
    let directory = dae.rsplit_once('/').map_or(String::new(), |(d, _)| format!("{d}/"));
    let name = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "KMZ".into());
    let collada = collada::build_with(&data, &name, |file| {
        let file = file.replace('\\', "/");
        let file = file.trim_start_matches("./");
        archive.read_named(&format!("{directory}{file}")).or_else(|| archive.read_named(file)).or_else(|| {
            let base = file.rsplit('/').next().unwrap_or(file);
            archive
                .entries()
                .iter()
                .find(|e| e.name.rsplit('/').next().is_some_and(|n| n.eq_ignore_ascii_case(base)))
                .and_then(|e| archive.read(e).ok())
        })
    })?;
    Ok(collada.model)
}
