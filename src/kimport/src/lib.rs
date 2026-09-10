//! Versioned, offline-cooked scene assets. Runtime decoding is CPU-only Rust.
//!
//! Complex authoring formats are cooked by `examples/kengine/new/import_tool`.
//! The runtime never starts a browser, executes JavaScript, or downloads assets.
use kasset::{BoxedLoaderFuture, LoadError, Resource, ResourceData, ResourceIo, ResourceLoader};
use kcore::uuid::{Uuid, uuid};
use kgltf::{MeshPart, Model, ModelNode, NodeTransform};
use kmath::{Mat4, Vec3};
use kmesh::{Mesh, MorphDelta, MorphTarget, Vertex};
use ktexture::{Texture, TextureFormat};
use serde::Deserialize;
use std::{path::PathBuf, sync::Arc};

mod physical;
pub use physical::physical_material;

const IMPORT_UUID: Uuid = uuid!("9d87050d-955a-425e-96cf-b7860fca9269");

/// Portable imported scene plus authoring camera and environment.
#[derive(Debug)]
pub struct ImportedScene {
    pub model: Model,
    pub camera: Mat4,
    pub fov: f32,
    pub near: f32,
    pub far: f32,
    pub target: Vec3,
    pub environment: Option<kpbr::hdr::HdrImage>,
    pub variants: Vec<(String, Model)>,
    pub provenance: String,
}
impl ResourceData for ImportedScene {
    fn type_uuid(&self) -> Uuid { IMPORT_UUID }
}

/// Register with ResourceManager to load `.kmodel` + its sibling `.kbin`.
#[derive(Debug, Default, Clone, Copy)]
pub struct ImportedSceneLoader;
impl ResourceLoader for ImportedSceneLoader {
    fn extensions(&self) -> &[&str] { &["kmodel"] }
    fn data_type_uuid(&self) -> Uuid { IMPORT_UUID }
    fn load(&self, path: PathBuf, io: Arc<dyn ResourceIo>) -> BoxedLoaderFuture {
        Box::pin(async move {
            let json = io.load_file(&path).await?;
            let bytes = io.load_file(&path.with_extension("kbin")).await?;
            Ok(Box::new(decode(&json, &bytes)?) as Box<dyn ResourceData>)
        })
    }
}

#[derive(Deserialize, Clone, Copy, Debug)]
struct View { offset: usize, count: usize }
impl View {
    fn bytes<'a>(&self, blob: &'a [u8], stride: usize) -> Result<&'a [u8], LoadError> {
        let end = self.count.checked_mul(stride).and_then(|n| self.offset.checked_add(n))
            .ok_or_else(|| LoadError::message("import buffer size overflow"))?;
        blob.get(self.offset..end).ok_or_else(|| LoadError::message("import buffer out of bounds"))
    }
    fn floats(&self, blob: &[u8]) -> Result<Vec<f32>, LoadError> {
        let data: Vec<_> = self.bytes(blob, 4)?.chunks_exact(4)
            .map(|v| f32::from_le_bytes(v.try_into().unwrap())).collect();
        if data.iter().any(|v| !v.is_finite()) { return Err(LoadError::message("non-finite import attribute")); }
        Ok(data)
    }
}
#[derive(Deserialize)]
struct Document {
    version: u32,
    provenance: String,
    meshes: Vec<Geometry>,
    textures: Vec<Image>,
    materials: Vec<Surface>,
    nodes: Vec<Object>,
    roots: Vec<usize>,
    camera: Camera,
    #[serde(default)] environment: Option<Environment>,
    #[serde(default)] clips: Vec<Clip>,
    #[serde(default)] variants: Vec<Variant>,
}
#[derive(Deserialize)]
struct Geometry {
    position: View,
    normal: Option<View>, uv: Option<View>, color: Option<View>,
    index: View,
    #[serde(default)] morphs: Vec<Morph>,
}
#[derive(Deserialize)]
struct Morph { name: String, position: View }
#[derive(Deserialize)]
struct Image { width: u32, height: u32, data: View, #[serde(default)] linear: bool }
#[derive(Deserialize, Clone)]
pub struct Surface {
    pub name: String,
    pub color: [f32; 4],
    pub metallic: f32, pub roughness: f32,
    pub emissive: [f32; 3],
    #[serde(default)] pub double_sided: bool,
    #[serde(default)] pub transparent: bool,
    #[serde(default)] pub unlit: bool,
    #[serde(default)] pub alpha_test: f32,
    pub map: Option<usize>, pub normal_map: Option<usize>, pub mr_map: Option<usize>,
    pub emissive_map: Option<usize>, pub ao_map: Option<usize>,
    #[serde(default)] pub transmission: f32,
    #[serde(default = "default_ior")] pub ior: f32,
    #[serde(default)] pub thickness: f32,
    #[serde(default)] pub dispersion: f32,
    #[serde(default)] pub iridescence: f32,
    #[serde(default = "default_film")] pub iridescence_thickness: f32,
    #[serde(default)] pub sheen: [f32; 3],
    #[serde(default)] pub sheen_roughness: f32,
}
fn default_ior() -> f32 { 1.5 }
fn default_film() -> f32 { 400.0 }
#[derive(Deserialize)]
struct Object { name: String, matrix: [f32;16], children: Vec<usize>, mesh: Option<usize>, material: Option<usize> }
#[derive(Deserialize)]
struct Camera { matrix: [f32;16], fov: f32, near: f32, far: f32, target: [f32;3] }
#[derive(Deserialize)]
struct Environment { width: usize, height: usize, data: View }
#[derive(Deserialize)]
struct Clip { name: String, tracks: Vec<AnimationTrack> }
#[derive(Deserialize)]
struct AnimationTrack { node: usize, slot: usize, times: Vec<f32>, values: Vec<f32> }
#[derive(Deserialize)]
struct Variant { name: String, materials: Vec<Option<usize>> }

/// Decode a cooked scene. Invalid ranges, topology and cyclic hierarchies fail before instantiation.
pub fn decode(json: &[u8], blob: &[u8]) -> Result<ImportedScene, LoadError> {
    if json.len() > 32 * 1024 * 1024 || blob.len() > 512 * 1024 * 1024 {
        return Err(LoadError::message("import exceeds 32 MiB metadata / 512 MiB buffer limit"));
    }
    let doc: Document = serde_json::from_slice(json).map_err(LoadError::custom)?;
    if doc.version != 1 { return Err(LoadError::message("unsupported kmodel version")); }
    if doc.nodes.len() > 100_000 { return Err(LoadError::message("too many imported nodes")); }
    validate_tree(&doc.nodes, &doc.roots)?;
    let textures: Vec<_> = doc.textures.iter().enumerate().map(|(i, image)| {
        let expected = (image.width as usize).checked_mul(image.height as usize).and_then(|n| n.checked_mul(4));
        if image.width == 0 || image.height == 0 || image.width > 16384 || image.height > 16384 || expected != Some(image.data.count) {
            return Err(LoadError::message("invalid imported image dimensions"));
        }
        Ok(Resource::new_ok(format!("import#image{i}"), Texture::new(image.width, image.height, image.data.bytes(blob,1)?.to_vec())
            .with_format(if image.linear { TextureFormat::Linear } else { TextureFormat::Srgb })))
    }).collect::<Result<_,_>>()?;
    let materials: Vec<_> = doc.materials.iter().map(|s| {
        let mut m = physical_material(s);
        for (slot, index) in [("base_color_texture",s.map),("normal_texture",s.normal_map),("metallic_roughness_texture",s.mr_map),("emissive_texture",s.emissive_map),("occlusion_texture",s.ao_map)] {
            if let Some(index) = index {
                let tex = textures.get(index).ok_or_else(|| LoadError::message("invalid imported texture reference"))?;
                m.set(slot,tex.clone());
            }
        }
        Ok(m)
    }).collect::<Result<_,LoadError>>()?;
    let meshes = doc.meshes.iter().map(|g| {
        let p = g.position.floats(blob)?;
        if p.is_empty() || p.len()%3 != 0 { return Err(LoadError::message("invalid POSITION length")); }
        let n = g.normal.map(|v| v.floats(blob)).transpose()?;
        let uv = g.uv.map(|v| v.floats(blob)).transpose()?;
        let color = g.color.map(|v| v.floats(blob)).transpose()?;
        for (a, size) in [(&n,3),(&uv,2),(&color,3)] {
            if a.as_ref().is_some_and(|a| a.len()!=p.len()/3*size) { return Err(LoadError::message("attribute vertex count mismatch")); }
        }
        let vertices = p.chunks_exact(3).enumerate().map(|(i,p)| Vertex {
            position: p.try_into().unwrap(),
            normal: n.as_ref().map_or([0.0;3],|a| a[i*3..i*3+3].try_into().unwrap()),
            uv: uv.as_ref().map_or([0.0;2],|a| a[i*2..i*2+2].try_into().unwrap()),
            color: color.as_ref().map_or([1.0;3],|a| a[i*3..i*3+3].try_into().unwrap()),
            ..Default::default()
        }).collect();
        let indices: Vec<_> = g.index.bytes(blob,4)?.chunks_exact(4).map(|a| u32::from_le_bytes(a.try_into().unwrap())).collect();
        if indices.is_empty() || indices.len()%3!=0 || indices.iter().any(|&i| i as usize>=p.len()/3) { return Err(LoadError::message("invalid triangle indices")); }
        let mut mesh = Mesh::new(vertices, indices);
        if n.is_none() { mesh.recompute_normals(); }
        if uv.is_some() { mesh.recompute_tangents(); }
        let morphs = g.morphs.iter().map(|m| {
            let values = m.position.floats(blob)?;
            if values.len()!=p.len() { return Err(LoadError::message("morph vertex count mismatch")); }
            Ok(MorphTarget::new(m.name.clone(), values.chunks_exact(3).map(|v| MorphDelta { position: v.try_into().unwrap(), ..Default::default() }).collect()))
        }).collect::<Result<Vec<_>,LoadError>>()?;
        if !morphs.is_empty() { let count=morphs.len(); mesh=mesh.with_morph_targets(morphs,vec![0.0;count]); }
        Ok(mesh)
    }).collect::<Result<Vec<_>,LoadError>>()?;
    let nodes = doc.nodes.iter().map(|node| {
        if node.matrix.iter().any(|n| !n.is_finite()) { return Err(LoadError::message("invalid node transform")); }
        let (scale,rotation,position) = Mat4::from_cols_array(&node.matrix).to_scale_rotation_translation();
        let parts = if let Some(mesh) = node.mesh {
            if mesh>=meshes.len() || node.material.is_some_and(|m| m>=materials.len()) { return Err(LoadError::message("invalid mesh/material reference")); }
            vec![MeshPart{mesh,material:node.material}]
        } else { Vec::new() };
        Ok(ModelNode{name:node.name.clone(), transform:NodeTransform{scale,rotation,position}, children:node.children.clone(), parts, skin:None})
    }).collect::<Result<Vec<_>,LoadError>>()?;
    let mut clips = Vec::new();
    for clip in &doc.clips {
        let mut tracks=Vec::new();
        for t in &clip.tracks {
            let mesh=doc.nodes.get(t.node).and_then(|n|n.mesh).and_then(|i|doc.meshes.get(i));
            if mesh.is_none_or(|m|t.slot>=m.morphs.len()) || t.times.iter().chain(&t.values).any(|v|!v.is_finite()) {
                return Err(LoadError::message("invalid morph animation target"));
            }
            let curve=kanim::Curve::new(t.times.clone(),t.values.clone(),kanim::Interpolation::Linear).ok_or_else(||LoadError::message("invalid animation curve"))?;
            tracks.push(kanim::Track{target:t.node, channel:kanim::Channel::MorphWeight{index:t.slot,curve}});
        }
        clips.push(kanim::AnimationClip::new(clip.name.clone(),tracks));
    }
    let mut variants=Vec::new();
    for v in doc.variants {
        if v.materials.len()!=nodes.len() {return Err(LoadError::message("variant node count mismatch"));}
        let mut vn=nodes.clone();
        for (node,mat) in vn.iter_mut().zip(v.materials) {
            if mat.is_some_and(|m|m>=materials.len()) {return Err(LoadError::message("invalid variant material"));}
            for part in &mut node.parts {part.material=mat;}
        }
        variants.push((v.name,Model::new(meshes.clone(),materials.clone(),vn,doc.roots.clone()).with_animations(clips.clone())));
    }
    let environment=doc.environment.map(|e| {
        let pixels=e.data.floats(blob)?;
        if e.width==0 || e.height==0 || e.width.checked_mul(e.height).and_then(|n|n.checked_mul(3))!=Some(pixels.len()) {return Err(LoadError::message("invalid environment dimensions"));}
        Ok(kpbr::hdr::HdrImage::from_pixels(e.width,e.height,pixels))
    }).transpose()?;
    Ok(ImportedScene{model:Model::new(meshes,materials,nodes,doc.roots).with_animations(clips),camera:Mat4::from_cols_array(&doc.camera.matrix),
        fov:doc.camera.fov,near:doc.camera.near,far:doc.camera.far,target:Vec3::from_array(doc.camera.target),environment,variants,provenance:doc.provenance})
}

fn validate_tree(nodes:&[Object],roots:&[usize])->Result<(),LoadError>{
    let mut seen=vec![false;nodes.len()];
    let mut stack:Vec<_>=roots.iter().map(|&i|(i,0)).collect();
    while let Some((i,depth))=stack.pop(){
        if i>=nodes.len() || seen[i] || depth>256 {return Err(LoadError::message("cyclic, shared, out-of-range or excessively deep scene hierarchy"));}
        seen[i]=true;
        stack.extend(nodes[i].children.iter().map(|&c|(c,depth+1)));
    }
    if seen.iter().any(|v|!*v){return Err(LoadError::message("unreachable imported node"));}
    Ok(())
}
