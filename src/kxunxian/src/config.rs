//! 角色配置 `.cct` 与材质表 `.cmf`。
//!
//! 一个 `.cct` 描述一个「角色」：骨架、材质表、一堆 `<Model>`（网格 + 材质名）、挂点 `<HingePoint>`、
//! 装备 `<Equip>`（引用另一个模板 `.cct` 里的 `<Model>`，挂到某个挂点上）、动作 `<Animation>`。
//! zj（玩家角色）的 `.cct` 是整座衣柜：几千个部件，游戏按装备挑几件穿上。

use std::collections::HashMap;

use kmath::{Quat, Vec3, Vec4};

use crate::xml::{self, Element};

/// 一个可以穿戴 / 显示的网格部件。
#[derive(Debug, Clone, PartialEq)]
pub struct ModelDef {
    pub name: String,
    /// 原始路径（`$(res)\...`）。
    pub mesh: String,
    pub material: String,
    /// `MeshDual="Yes"`：双面。
    pub double_sided: bool,
}

/// 挂点：某根骨骼上的一个局部坐标系。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Hinge {
    /// 骨骼序号；-1 表示角色根。
    pub bone: i32,
    pub rotation: Quat,
    pub translation: Vec3,
}

/// 一件装备：把模板 `.cct` 里的一个或几个条目（`*` 分隔）挂到挂点上。
#[derive(Debug, Clone, PartialEq)]
pub struct EquipDef {
    pub name: String,
    /// 模板里的条目名（`<Model>` / `<Particle>` / `<Cloth>` 的 `Name`）。
    pub items: Vec<String>,
    pub hinge: String,
    pub scaling: f32,
    /// 模板 `.cct` 的原始路径。
    pub template: String,
}

/// 一个 `.cct`。
#[derive(Debug, Clone, Default)]
pub struct CharacterDef {
    pub scaling: f32,
    pub skeleton: Option<String>,
    pub materials: Vec<String>,
    /// 按出现顺序。同名的只留第一个。
    pub models: Vec<ModelDef>,
    pub hinges: HashMap<String, Hinge>,
    pub equips: HashMap<String, EquipDef>,
    /// 动作名 → `.paf` 原始路径。
    pub animations: HashMap<String, String>,
    model_index: HashMap<String, usize>,
}

impl CharacterDef {
    pub fn parse(bytes: &[u8]) -> Self {
        let text = xml::decode(bytes);
        let mut def = CharacterDef {
            scaling: 1.0,
            ..Default::default()
        };
        for element in xml::elements(&text) {
            if element.closing {
                continue;
            }
            match element.tag.as_str() {
                "Scaling" => def.scaling = element.text.parse().unwrap_or(1.0),
                "Skeleton" if !element.text.is_empty() => def.skeleton = Some(element.text.clone()),
                "Material" if !element.text.is_empty() => def.materials.push(element.text.clone()),
                // `<Cloth>` 是布料模拟的网格（披风、飘带），先当普通部件显示。
                "Model" | "Cloth" => {
                    let (Some(name), Some(mesh)) = (element.attr("Name"), element.attr("MeshFile"))
                    else {
                        continue;
                    };
                    // 名字不分大小写：装备里写 `M_wqa_101`，模板里叫 `m_wqa_101`，原版游戏照样认。
                    let key = name.to_lowercase();
                    if def.model_index.contains_key(&key) {
                        continue;
                    }
                    def.model_index.insert(key, def.models.len());
                    def.models.push(ModelDef {
                        name: name.to_string(),
                        mesh: mesh.to_string(),
                        material: element.attr("MatName").unwrap_or_default().to_string(),
                        double_sided: element.attr("MeshDual") == Some("Yes"),
                    });
                }
                "HingePoint" => {
                    let Some(name) = element.attr("Name") else {
                        continue;
                    };
                    def.hinges.insert(name.to_string(), hinge(&element));
                }
                "Equip" => {
                    let Some(name) = element.attr("Name") else {
                        continue;
                    };
                    def.equips.insert(
                        name.to_string(),
                        EquipDef {
                            name: name.to_string(),
                            items: element
                                .attr("Equip")
                                .unwrap_or_default()
                                .split('*')
                                .map(str::to_string)
                                .collect(),
                            hinge: element
                                .attr("HingePointName")
                                .unwrap_or_default()
                                .to_string(),
                            scaling: element
                                .attr("Scaling")
                                .and_then(|s| s.parse().ok())
                                .unwrap_or(1.0),
                            template: element.attr("TemplateFile").unwrap_or_default().to_string(),
                        },
                    );
                }
                "Animation" => {
                    if let (Some(name), Some(file)) =
                        (element.attr("Name"), element.attr("AnimationFile"))
                    {
                        def.animations.insert(name.to_string(), file.to_string());
                    }
                }
                _ => {}
            }
        }
        def
    }

    /// 按名找部件（不分大小写）。
    pub fn model(&self, name: &str) -> Option<&ModelDef> {
        self.model_index
            .get(&name.to_lowercase())
            .map(|&index| &self.models[index])
    }
}

fn floats<const N: usize>(text: Option<&str>) -> Option<[f32; N]> {
    let values: Vec<f32> = text?
        .split(',')
        .filter_map(|part| part.trim().parse().ok())
        .collect();
    values.try_into().ok()
}

fn hinge(element: &Element) -> Hinge {
    let rotation = floats::<4>(element.attr("RelRot"))
        .map(|[x, y, z, w]| Quat::from_xyzw(x, y, z, w))
        .filter(|q| q.length_squared() > 1e-6)
        .map(Quat::normalize)
        .unwrap_or(Quat::IDENTITY);
    Hinge {
        bone: element
            .attr("BoneId")
            .and_then(|s| s.parse().ok())
            .unwrap_or(-1),
        rotation,
        translation: floats::<3>(element.attr("RelTrans"))
            .map(Vec3::from_array)
            .unwrap_or(Vec3::ZERO),
    }
}

/// `.cmf` 里的一个材质。
#[derive(Debug, Clone, PartialEq)]
pub struct MaterialDef {
    /// 底图原始路径。
    pub base_map: String,
    pub diffuse: Vec4,
    pub emissive: Vec4,
}

/// 解析 `.cmf`：材质名 → 定义。
pub fn parse_materials(bytes: &[u8]) -> HashMap<String, MaterialDef> {
    let text = xml::decode(bytes);
    let mut out = HashMap::new();
    let mut current: Option<(String, MaterialDef)> = None;
    let vec4 = |text: &str| floats::<4>(Some(text)).map(Vec4::from_array);
    for element in xml::elements(&text) {
        match (element.tag.as_str(), element.closing) {
            ("Material", false) => {
                current = element.attr("Name").map(|name| {
                    (
                        name.to_string(),
                        MaterialDef {
                            base_map: String::new(),
                            diffuse: Vec4::ONE,
                            emissive: Vec4::ZERO,
                        },
                    )
                });
            }
            ("Material", true) => {
                if let Some((name, def)) = current.take() {
                    out.insert(name, def);
                }
            }
            (tag, false) => {
                let Some((_, def)) = current.as_mut() else {
                    continue;
                };
                match tag {
                    "BaseMap0" => def.base_map = element.text.clone(),
                    "Diffuse" => def.diffuse = vec4(&element.text).unwrap_or(Vec4::ONE),
                    "Emissive" => def.emissive = vec4(&element.text).unwrap_or(Vec4::ZERO),
                    _ => {}
                }
            }
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_character_parts() {
        let text = r#"<Character>
  <Scaling>1.0000</Scaling>
  <Skeleton>$(res)\cha\a.psf</Skeleton>
  <Material>$(res)\cha\a.cmf</Material>
  <Model ColorSign="Yes" MatName="c_x" MeshDual="Yes" MeshFile="$(res)\m\x.pmf" Name="m_x"/>
  <HingePoint BoneId="61" Name="equip_weapen" RelRot="0.0000, 0.0000, 0.0000, 1.0000" RelTrans="0.1, 0.2, 0.3"/>
  <HingePoint BoneId="41" Name="zero" RelRot="0.0000, 0.0000, 0.0000, 0.0000" RelTrans="0, 0, 0"/>
  <Equip Equip="c_fba_101a*m_fba_101" HingePointName="equip_bei_2" Name="e_fba_101" Scaling="1.0000" TemplateFile="$(res)\fb.cct"/>
  <Animation AnimationFile="$(res)\an\zl01_tk.paf" Name="zl01_tk" Priority="0"/>
</Character>"#;
        let def = CharacterDef::parse(text.as_bytes());
        assert_eq!(def.skeleton.as_deref(), Some("$(res)\\cha\\a.psf"));
        assert_eq!(def.materials, ["$(res)\\cha\\a.cmf"]);
        let model = def.model("M_X").unwrap();
        assert!(model.double_sided);
        assert_eq!(model.material, "c_x");
        let hinge = def.hinges["equip_weapen"];
        assert_eq!(hinge.bone, 61);
        assert_eq!(hinge.translation, Vec3::new(0.1, 0.2, 0.3));
        // 全零的四元数（原数据里真有）当单位旋转。
        assert_eq!(def.hinges["zero"].rotation, Quat::IDENTITY);
        assert_eq!(def.equips["e_fba_101"].items, ["c_fba_101a", "m_fba_101"]);
        assert_eq!(def.animations["zl01_tk"], "$(res)\\an\\zl01_tk.paf");
    }

    #[test]
    fn reads_material_table() {
        let text = r#"<Root>
  <Material Name="c_a">
    <Layer>0</Layer>
    <BaseMap0>$(res)\t\a.dds</BaseMap0>
    <Emissive>0.2980, 0.2980, 0.2980, 0.0000</Emissive>
  </Material>
</Root>"#;
        let table = parse_materials(text.as_bytes());
        assert_eq!(table["c_a"].base_map, "$(res)\\t\\a.dds");
        assert!((table["c_a"].emissive.x - 0.298).abs() < 1e-4);
    }
}
