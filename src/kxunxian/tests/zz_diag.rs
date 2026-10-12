use std::path::PathBuf;
use kxunxian::Library;
use kmath::Vec3;

fn inside(p: Vec3, a: Vec3, b: Vec3, c: Vec3, n: Vec3) -> bool {
    let e = 1e-4;
    (b - a).cross(p - a).dot(n) > -e && (c - b).cross(p - b).dot(n) > -e && (a - c).cross(p - c).dot(n) > -e
}

#[test]
fn diag() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/unpack2");
    let mut library = Library::new(&root);
    let scene = library.scene("fb_xiaosangcun").unwrap();
    let groups = kxunxian::group_placements(&scene.placements);
    let (mut dbl_opp, mut same_all, mut parts) = (0, 0, 0);
    for (model, _) in &groups {
        for (pi, part) in library.static_object(model).iter().enumerate() {
            parts += 1;
            let v = part.mesh.vertices();
            let tris: Vec<[Vec3; 3]> = part.mesh.indices().chunks_exact(3).map(|t| [v[t[0] as usize].position(), v[t[1] as usize].position(), v[t[2] as usize].position()]).collect();
            let (mut opp, mut same) = (0, 0);
            for (i, a) in tris.iter().enumerate() {
                let Some(na) = (a[1] - a[0]).cross(a[2] - a[0]).try_normalize() else { continue };
                let ca = (a[0] + a[1] + a[2]) / 3.0;
                for (j, b) in tris.iter().enumerate() {
                    if i == j { continue; }
                    let Some(nb) = (b[1] - b[0]).cross(b[2] - b[0]).try_normalize() else { continue };
                    let d = na.dot(nb);
                    if d.abs() < 0.999 || (ca - b[0]).dot(nb).abs() > 0.003 || !inside(ca, b[0], b[1], b[2], nb) { continue; }
                    if d < 0.0 { opp += 1 } else { same += 1 }
                    break;
                }
            }
            if part.material.double_sided() { dbl_opp += opp; }
            same_all += same;
            if model.contains("fangzhigongju_003") { println!("{model}#{pi} dbl={} tris {} opp {opp} same {same}", part.material.double_sided(), tris.len()); }
        }
    }
    println!("parts {parts} 双面里的背靠背 {dbl_opp} 同向共面重叠 {same_all}");
}
