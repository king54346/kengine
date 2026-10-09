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
    // x = 不参与聚簇的光源数（方向光、半球光，排在数组最前面），
    // y = 光源总数，zw 保留
    light_count: vec4<u32>,
    // 各级级联的光空间矩阵。用不满的级填单位阵。
    // 方向光用前几层（级联），点光六层都用（立方体的六个面），聚光只用第 0 层。
    light_view_proj: array<mat4x4<f32>, 6>,
    // x/y/z = 前三级的远距离，w = 实际级数
    cascade_splits: vec4<f32>,
    // x = 深度偏移，y = 法线偏移，z = 阴影贴图边长，
    // w = 投射者类型：0 没有阴影，1 方向光级联，2 点光立方体，3 聚光
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
    // 聚簇网格：x/y = 屏幕分块数，z = 深度切片数，w = 是否启用
    cluster_grid: vec4<u32>,
    // x = 近平面，y = 远平面，z = 1 / ln(far / near)，w 保留
    cluster_depth: vec4<f32>,
    // **不带抖动**的本帧视图投影。
    //
    // `view_proj` 在开了 TAA 时每帧挪亚像素——光栅化要的就是这个。
    // 运动向量却必须用不抖的那份：不然静止的画面也有一层随帧跳动的
    // 「运动」，TAA 会把它当真去重投影，整个画面糊掉。
    clip_view_proj: mat4x4<f32>,
    // 上一帧（不带抖动）的视图投影。运动向量 = 本帧 NDC − 上一帧 NDC。
    prev_view_proj: mat4x4<f32>,
    // xy = 本帧抖动（NDC 单位），zw = 上一帧的
    jitter: vec4<f32>,
    environment: Environment,
};

// 自定义材质参数：kmaterial::standard::PARAM_SLOTS 个 vec4。钩子里写这个别名，别写死数组长度——
// 槽位数变了，写死长度的函数签名会直接编译失败。
alias MaterialParams = array<vec4<f32>, 16>;

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
    // x = 接受哪些层的光照（位掩码），y = 后处理遮罩位，
    // z = 上一帧骨骼矩阵的起点（和本帧的拼在同一个数组里），w 保留
    flags: vec4<u32>,
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
    // 探针过渡：x = 次探针的层号，y = 次探针占的权重，z = 次探针的强度，w 保留。
    //
    // 逐对象只选一个探针的话，物体跨过盒子边界的那一刻环境光会**跳一下**。
    // 这一组让它在盒子边缘那一圈里平滑过渡到「外面那个」——罩住它的更大
    // 的探针，没有就是全局环境（层 0）。
    probe_blend: vec4<f32>,
    // 自定义材质参数，钩子里是 `surface.params[i]`。
    //
    // 放在**逐对象**的数据里而不是单独一个 uniform：那样同一个网格的
    // 多个实例各带各的参数仍然是一次绘制。给每个材质单开一条绑定的话，
    // 「每个方块颜色不同」就等于「每个方块一次 draw call」。
    params: MaterialParams,
    // 上一帧的模型矩阵。运动向量要它；没有上一帧（刚出现的物体）时
    // 和 `model` 相同，于是速度为零。
    prev_model: mat4x4<f32>,
};

// 一个顶点在某个形变目标下的增量。两个 vec3 各自补齐到 16 字节。
struct MorphDelta {
    position: vec3<f32>,
    padding0: f32,
    normal: vec3<f32>,
    padding1: f32,
};

@group(0) @binding(0) var<uniform> globals: Globals;
// 光源数组。全局光（方向光、半球光）在前，可聚簇的（点、聚光）在后。
//
// 从 uniform 搬到存储缓冲，是为了让上限从十几盏提到几百盏——
// uniform 的大小要在管线里写死，而存储缓冲是变长的。
@group(0) @binding(1) var<storage, read> lights: array<Light>;
// 每个簇的名单区间：x = 起点，y = 长度。
@group(0) @binding(2) var<storage, read> cluster_ranges: array<vec2<u32>>;
// 所有簇的名单首尾相接。存的是**可聚簇那一段**里的下标。
@group(0) @binding(3) var<storage, read> cluster_indices: array<u32>;

// 每个光照探针的漫反射球谐，9 个 vec4 一组。
// 第 0 组是全局环境；物体属于哪一组由 `object.probe_position.w` 说了算，
// 和镜面反射取哪一层用的是同一个层号。
@group(0) @binding(4) var<storage, read> probe_irradiance: array<vec4<f32>>;
// 每个实例一份，用 instance_index 寻址。存储缓冲而非 uniform：
// 一次 draw 就能画完一批同网格同贴图的对象，不必逐个切换动态偏移。
@group(1) @binding(0) var<storage, read> objects: array<ObjectUniforms>;
// 每个 GPU 实例一个（`instance_index` 寻址）：x = 对象在 `objects` 里的下标，
// y = 实例数据在 `instance_data` 里的下标，`NO_INSTANCE` 表示普通物体（单位阵、白色、0）。
//
// 普通物体一个对象一个槽；实例化的节点（`Node::with_instances`）一个对象 N 个槽，共用一份
// 600 字节的对象数据。槽只有 8 字节，实例数据（矩阵 + 颜色 + 自定义 vec4）从场景里整块拷上来。
// 只在顶点阶段读：片元阶段要的对象下标、实例颜色和数据由顶点着色器平着传下去。
@group(1) @binding(4) var<storage, read> instance_slots: array<vec2<u32>>;

// 和 `kscene::Instance` 逐字节一致。
struct InstanceData {
    // 实例在节点里的局部变换：最终的 model = 节点的 model × 它
    transform: mat4x4<f32>,
    // 乘到基础色上（rgba）
    color: vec4<f32>,
    // 自定义数据，钩子里是 `vertex.instance_data` / `surface.instance_data`
    data: vec4<f32>,
};
@group(1) @binding(5) var<storage, read> instance_data: array<InstanceData>;

const NO_INSTANCE: u32 = 0xffffffffu;

// 3×3 部分的逆转置（余子式 / 行列式），法线用。比 4×4 求逆便宜得多，不等比缩放也对。
fn instance_normal_matrix(m: mat4x4<f32>) -> mat4x4<f32> {
    let a = m[0].xyz;
    let b = m[1].xyz;
    let c = m[2].xyz;
    let r0 = cross(b, c);
    let r1 = cross(c, a);
    let r2 = cross(a, b);
    let det = dot(a, r0);
    let s = select(1.0 / det, 1.0, abs(det) < 1e-12);
    return mat4x4<f32>(vec4<f32>(r0 * s, 0.0), vec4<f32>(r1 * s, 0.0), vec4<f32>(r2 * s, 0.0), vec4<f32>(0.0, 0.0, 0.0, 1.0));
}

// 这个 GPU 实例的实例数据；普通物体是单位阵、白色、0。
fn instance_of(instance: u32) -> InstanceData {
    let index = instance_slots[instance].y;
    if (index == NO_INSTANCE) {
        return InstanceData(
            mat4x4<f32>(vec4<f32>(1.0, 0.0, 0.0, 0.0), vec4<f32>(0.0, 1.0, 0.0, 0.0), vec4<f32>(0.0, 0.0, 1.0, 0.0), vec4<f32>(0.0, 0.0, 0.0, 1.0)),
            vec4<f32>(1.0),
            vec4<f32>(0.0),
        );
    }
    return instance_data[index];
}

// 这个 GPU 实例属于哪个对象。顶点着色器把它传给片元（`out.instance`）。
fn instance_object_index(instance: u32) -> u32 {
    return instance_slots[instance].x;
}

// 这个 GPU 实例的对象数据，实例的矩阵、颜色已经并进去了（顶点阶段用）。
fn instance_object(instance: u32) -> ObjectUniforms {
    let slot = instance_slots[instance];
    var object = objects[slot.x];
    if (slot.y != NO_INSTANCE) {
        let extra = instance_data[slot.y];
        object.model = object.model * extra.transform;
        object.prev_model = object.prev_model * extra.transform;
        object.normal_matrix = object.normal_matrix * instance_normal_matrix(extra.transform);
        object.base_color = object.base_color * extra.color;
    }
    return object;
}

// 片元阶段用：对象下标和实例颜色是顶点阶段平着传下来的。
fn tinted_object(object_index: u32, color: vec4<f32>) -> ObjectUniforms {
    var object = objects[object_index];
    object.base_color = object.base_color * color;
    return object;
}

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
    // 第二套 UV（lightmap 用）。5/6 留给 `SkinInput`，见 `VERTEX_ATTRIBUTES`
    // 的注释。
    @location(7) uv1: vec2<f32>,
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
