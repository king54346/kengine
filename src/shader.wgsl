// kengine 标准着色器：方向光 + Blinn-Phong 高光 + 基础色贴图。

struct Globals {
    view_proj: mat4x4<f32>,
    camera_position: vec4<f32>,
    // xyz = 指向光源的方向（已归一化），w 未使用
    light_direction: vec4<f32>,
    // rgb = 光照颜色，a = 环境光强度
    light_color: vec4<f32>,
};

struct ObjectUniforms {
    model: mat4x4<f32>,
    // 法线矩阵：model 的逆转置，保证非均匀缩放下法线仍然正确
    normal_matrix: mat4x4<f32>,
    base_color: vec4<f32>,
    metallic: f32,
    roughness: f32,
    _padding: vec2<f32>,
};

@group(0) @binding(0) var<uniform> globals: Globals;
@group(1) @binding(0) var<uniform> object: ObjectUniforms;
@group(2) @binding(0) var base_color_texture: texture_2d<f32>;
@group(2) @binding(1) var base_color_sampler: sampler;

struct VertexInput {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) color: vec3<f32>,
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) world_position: vec3<f32>,
    @location(1) world_normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) color: vec3<f32>,
};

@vertex
fn vs_main(in: VertexInput) -> VertexOutput {
    let world_position = object.model * vec4<f32>(in.position, 1.0);

    var out: VertexOutput;
    out.clip_position = globals.view_proj * world_position;
    out.world_position = world_position.xyz;
    out.world_normal = (object.normal_matrix * vec4<f32>(in.normal, 0.0)).xyz;
    out.uv = in.uv;
    out.color = in.color;
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let sampled = textureSample(base_color_texture, base_color_sampler, in.uv);
    let albedo = object.base_color * sampled * vec4<f32>(in.color, 1.0);

    let normal = normalize(in.world_normal);
    let to_light = normalize(globals.light_direction.xyz);
    let to_camera = normalize(globals.camera_position.xyz - in.world_position);

    // 漫反射
    let diffuse = max(dot(normal, to_light), 0.0);

    // Blinn-Phong 高光：粗糙度越低高光越锐利
    let halfway = normalize(to_light + to_camera);
    let shininess = mix(128.0, 4.0, clamp(object.roughness, 0.0, 1.0));
    let specular_strength = mix(0.04, 1.0, clamp(object.metallic, 0.0, 1.0));
    var specular = pow(max(dot(normal, halfway), 0.0), shininess) * specular_strength;
    // 背光面不该有高光
    specular = select(0.0, specular, diffuse > 0.0);

    let ambient = globals.light_color.a;
    let lighting = globals.light_color.rgb * (diffuse + specular) + vec3<f32>(ambient);

    return vec4<f32>(albedo.rgb * lighting, albedo.a);
}
