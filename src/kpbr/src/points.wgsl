// 点精灵：把退化的正方形面片在顶点阶段张成一个正对相机的方块。
//
// 几何由 `kmesh::Mesh::point_sprites` 生成——每个点四个**位置完全相同**
// 的顶点，四个角的区别只写在 `uv` 里（(0,0)/(1,0)/(1,1)/(0,1)）。
// 不这样做的话，点云要么每帧在 CPU 上重建朝向（几万个点就是几万次
// 三角函数），要么干脆不朝向相机、侧看时变成一条线。
//
// # 相机位置为什么要 CPU 传进来
//
// `material_vertex` 拿到的 `position` 是**模型空间**的，而
// `globals.camera_position` 是世界空间的。两者不在一个坐标系里，直接
// 相减得到的朝向在节点带旋转/平移时是错的。模型矩阵的逆在着色器里
// 拿不到（对象 uniform 只存了 model 和法线矩阵），所以由
// `PointMaterial::set_camera` 每帧把相机换算到局部空间后写进 params[1]。
//
// # 点是方的不是圆的
//
// 圆点要在片元里按到中心的距离 `discard`，而引擎的钩子体系没有暴露
// `discard`（它会影响提前深度测试，是管线级的决定）。three.js 的
// `PointsMaterial` 不带贴图时画出来的也是方块，一致。

fn material_vertex(vertex: VertexSurface) -> VertexSurface {
    var out = vertex;
    // params[0].x = 点的世界尺寸（边长的一半）
    let radius = vertex.params[0].x;
    // uv 的四个角 → [-1, 1] 的偏移
    let corner = vertex.uv * 2.0 - vec2<f32>(1.0);
    let to_camera = normalize_or_fallback(
        vertex.params[1].xyz - vertex.position,
        vec3<f32>(0.0, 0.0, 1.0),
    );
    // 参考向量和视线共线时叉乘退化，换一根轴。
    let reference = select(
        vec3<f32>(0.0, 1.0, 0.0),
        vec3<f32>(1.0, 0.0, 0.0),
        abs(to_camera.y) > 0.99,
    );
    let right = normalize_or_fallback(cross(reference, to_camera), vec3<f32>(1.0, 0.0, 0.0));
    let up = cross(to_camera, right);
    out.position = vertex.position + (right * corner.x + up * corner.y) * radius;
    out.normal = to_camera;
    return out;
}

// 点云不参与光照：几何上它根本没有表面，法线是编出来的。
// 把颜色整个搬到自发光上，直射光和环境光都返回 0。
fn material_surface(surface: Surface) -> Surface {
    var out = surface;
    out.emissive = surface.base_color.rgb;
    out.base_color = vec4<f32>(0.0, 0.0, 0.0, surface.base_color.a);
    return out;
}

fn material_lighting(surface: ptr<function, Surface>, input: LightingInput) -> vec3<f32> {
    return vec3<f32>(0.0);
}

fn material_ambient(surface: ptr<function, Surface>, input: AmbientInput) -> vec3<f32> {
    return vec3<f32>(0.0);
}
