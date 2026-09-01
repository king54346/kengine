// kengine 的几何声明：`Globals`、`ObjectUniforms`、顶点属性、蒙皮与形变。
//
// 单独拆出来是因为**不止一条通道要用它们**。标准着色器要，深度／法线
// 预通道（prepass）也要——两边各抄一份的话，`Globals` 里加一个字段就得
// 记得改两个地方，而漏改的症状是着色器照常编译、画面莫名其妙地错位：
// WGSL 结构体只要总大小对得上就不报错。
//
// 这个文件不含任何入口函数，由渲染器拼到各条通道的着色器前面。
// group(0) 是每帧全局量，group(1) 是每实例数据；纹理（group 2/3）
// 只有标准着色器要，留在 `shader.wgsl` 里。

struct Globals {
    view_proj: mat4x4<f32>,
    camera_position: vec4<f32>,
    // rgb = 环境光贡献，a 未使用
    ambient: vec4<f32>,
    // x = 生效的光源数量，其余保留
    light_count: vec4<u32>,
    // 各级级联的光空间矩阵。用不满的级填单位阵。
    light_view_proj: array<mat4x4<f32>, 4>,
    // x/y/z = 前三级的远距离，w = 实际级数
    cascade_splits: vec4<f32>,
    // x = 深度偏移，y = 法线偏移，z = 阴影贴图边长，w = 是否启用
    shadow_params: vec4<f32>,
    // x = 预滤波环境图的 mip 数（0 表示没有 HDR），其余保留
    ibl_params: vec4<f32>,
    // x/y = 投影矩阵的深度系数，用于把深度缓冲还原成视空间距离
    depth_params: vec4<f32>,
    // x = 启动至今的秒数，y = 上一帧的间隔，zw = 视口宽高（像素）
    //
    // 时间和视口尺寸是自定义材质最常要的两样东西：没有时间做不了流动，
    // 没有视口尺寸算不出屏幕 UV。
    frame_params: vec4<f32>,
    environment: Environment,
    lights: array<Light, 16>,
};

struct ObjectUniforms {
    model: mat4x4<f32>,
    // 法线矩阵：model 的逆转置，保证非均匀缩放下法线仍然正确
    normal_matrix: mat4x4<f32>,
    base_color: vec4<f32>,
    metallic: f32,
    roughness: f32,
    // 法线贴图强度：0 表示完全忽略贴图
    normal_scale: f32,
    // 环境光遮蔽强度
    occlusion_strength: f32,
    // rgb = 自发光颜色，a 保留
    emissive: vec4<f32>,
    // x = 骨骼矩阵起点，y = 形变增量起点，z = 形变目标数，w = 形变权重起点
    skin: vec4<u32>,
    // 纹理坐标变换：xy = 缩放，zw = 偏移。图集里取一格子图就靠它。
    uv_transform: vec4<f32>,
    // 反射探针：xyz = 采集点，w = 纹理数组的层号。
    //
    // w = 0 表示这个对象没有探针，用第 0 层（全局环境）且不做视差。
    // 探针是**逐对象**选的，所以一个横跨两个房间的大物体只能用一个
    // 探针——这是前向渲染的常规取舍，办法是把大物体拆开。
    probe_position: vec4<f32>,
    // xyz = 视差盒最小角，w = 是否做视差校正（>0.5 为是）
    probe_min: vec4<f32>,
    // xyz = 视差盒最大角，w = 强度
    probe_max: vec4<f32>,
    // 自定义材质参数，钩子里是 `surface.params[i]`。
    //
    // 放在**逐对象**的数据里而不是单独一个 uniform：那样同一个网格的
    // 多个实例各带各的参数仍然是一次绘制。给每个材质单开一条绑定的话，
    // 「每个方块颜色不同」就等于「每个方块一次 draw call」。
    params: array<vec4<f32>, 4>,
};

// 一个顶点在某个形变目标下的增量。两个 vec3 各自补齐到 16 字节。
struct MorphDelta {
    position: vec3<f32>,
    padding0: f32,
    normal: vec3<f32>,
    padding1: f32,
};

@group(0) @binding(0) var<uniform> globals: Globals;
// 每个实例一份，用 instance_index 寻址。存储缓冲而非 uniform：
// 一次 draw 就能画完一批同网格同贴图的对象，不必逐个切换动态偏移。
@group(1) @binding(0) var<storage, read> objects: array<ObjectUniforms>;
// 所有蒙皮实例的骨骼矩阵拼在一起，各实例按自己的偏移取用。
// 静态渲染时这里绑的是一个占位缓冲，谁也不会去读。
@group(1) @binding(1) var<storage, read> joint_matrices: array<mat4x4<f32>>;
// 所有带形变的网格的增量拼在一起，按「顶点优先」排列：
// 同一顶点的各个目标相邻，读一个顶点的全部形变只碰一段连续内存。
@group(1) @binding(2) var<storage, read> morph_deltas: array<MorphDelta>;
// 每个实例一段形变权重，实例自己记着起点。
@group(1) @binding(3) var<storage, read> morph_weights: array<f32>;

struct VertexInput {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) color: vec3<f32>,
    // xyz = 切线，w = 副切线手性
    @location(4) tangent: vec4<f32>,
};

// 蒙皮顶点属性，作为第二个顶点缓冲送进来。只有蒙皮管线声明它。
struct SkinInput {
    @location(5) joints: vec4<u32>,
    @location(6) weights: vec4<f32>,
};

// 线性混合蒙皮：顶点的最终变换是四个关节矩阵的加权和。
// 权重在导入时已经归一化，这里直接相加即可。
fn skin_matrix(joints: vec4<u32>, weights: vec4<f32>, offset: u32) -> mat4x4<f32> {
    return weights.x * joint_matrices[offset + joints.x]
        + weights.y * joint_matrices[offset + joints.y]
        + weights.z * joint_matrices[offset + joints.z]
        + weights.w * joint_matrices[offset + joints.w];
}

// 把形变增量叠加到顶点上。没有形变目标时（count = 0）整个循环不执行。
//
// 形变发生在蒙皮之前：形变改的是绑定姿态下的网格形状，
// 骨骼再把这个形状带到世界里——顺序反了，张嘴的幅度会被骨骼的缩放放大。
fn apply_morph(
    vertex_index: u32,
    offset: u32,
    count: u32,
    weight_offset: u32,
    position: ptr<function, vec3<f32>>,
    normal: ptr<function, vec3<f32>>,
) {
    if (count == 0u) {
        return;
    }

    let base = offset + vertex_index * count;
    for (var i = 0u; i < count; i = i + 1u) {
        let weight = morph_weights[weight_offset + i];
        // 权重为 0 的目标占多数（一张脸几十个表情通常只有几个在起作用），
        // 跳过它们能省下大量无用的读取。
        if (weight == 0.0) {
            continue;
        }
        let delta = morph_deltas[base + i];
        *position = *position + delta.position * weight;
        *normal = *normal + delta.normal * weight;
    }
}
