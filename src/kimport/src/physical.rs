use crate::Surface;
use kmaterial::{BlendMode, Material};
use kmath::{Vec3, Vec4};
/// Build a native kengine material from cooked material metadata.
pub fn physical_material(s:&Surface)->Material {
    let mut material=Material::standard().with_name(&s.name).with_base_color(Vec4::from_array(s.color))
        .with_metallic(s.metallic).with_roughness(s.roughness).with("emissive",Vec3::from_array(s.emissive));
    if s.double_sided {material.set_double_sided(true);}
    if s.transparent {material.set_blend_mode(BlendMode::Alpha);}
    if s.transmission>0.0 || s.iridescence>0.0 || s.sheen.iter().any(|&v|v>0.0) || s.unlit || s.alpha_test>0.0 {
        kpbr::physical::Physical{transmission:s.transmission,ior:s.ior,thickness:s.thickness,dispersion:s.dispersion,
            iridescence:s.iridescence,film_thickness:s.iridescence_thickness,sheen:Vec3::from_array(s.sheen),
            sheen_roughness:s.sheen_roughness,alpha_cutoff:s.alpha_test,unlit:s.unlit}.apply(&mut material);
    }
    material
}
