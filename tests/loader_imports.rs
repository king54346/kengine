//! Offline fixture checks and native material shader validation.
use kengine::{kimport,krender};

#[test]
fn imported_physical_material_shader_validates(){
    krender::validate_material_hook(include_str!("../src/kpbr/src/physical.wgsl")).unwrap();
}

#[test]
fn texture_views_do_not_alias_in_the_gpu_cache(){
    use kengine::ktexture::{Texture,TextureFormat,Sampler};
    let color=Texture::white();
    let data=color.clone().with_format(TextureFormat::Linear);
    let nearest=color.clone().with_sampler(Sampler::pixelated());
    assert_ne!(color.id(),data.id());assert_ne!(color.id(),nearest.id());
    assert_eq!(color.id(),color.clone().id());
    assert_eq!(color.data().as_ptr(),data.data().as_ptr(),"pixel storage should be shared");
}

#[test]
#[ignore="run import_tool/cook.mjs first; requires official upstream sample assets"]
fn all_cooked_examples_decode_and_validate(){
    let names="gltf_compressed gltf_dispersion gltf_instancing gltf_iridescence gltf_progressive_lod gltf_sheen gltf_transmission gltf_variants ifc imagebitmap kmz ldraw md2 md2_control mdd nrrd obj pcd pdb ply stl svg texture_dds texture_exr texture_hdr texture_ktx texture_ktx2 texture_lottie texture_pvrtc texture_tga texture_tiff texture_ultrahdr ttf usdz vox";
    for name in names.split_whitespace(){
        let path=std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("examples/kengine/new/loader_assets/{name}"));
        let json=std::fs::read(path.with_extension("kmodel")).unwrap_or_else(|e|panic!("{name}: {e}"));
        let binary=std::fs::read(path.with_extension("kbin")).unwrap();
        let scene=kimport::decode(&json,&binary).unwrap_or_else(|e|panic!("{name}: {e}"));
        assert!(scene.model.triangle_count()>0,"{name} is empty");
        if name=="gltf_instancing"{assert!(scene.model.nodes().len()>scene.model.meshes().len()*4,"instances were lost");}
        if ["md2","md2_control","mdd"].contains(&name){assert!(!scene.model.animations().is_empty(),"{name} lost animation");}
        if ["gltf_variants","nrrd","texture_lottie"].contains(&name){assert!(scene.variants.len()>1,"{name} lost variants");}
        if ["texture_hdr","texture_exr","texture_ultrahdr"].contains(&name){
            let env=scene.environment.as_ref().expect("HDR environment must not be flattened to RGBA8");
            assert!(env.pixels().iter().any(|&v|v>1.0),"{name} lost HDR range");
        }
    }
}
