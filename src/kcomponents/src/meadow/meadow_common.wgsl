// 草地（kcomponents::meadow）草叶和野花共用的两段：风、风格化光照。
// 照 three-stylized 的 `shaders.ts`：
//
// - 风：沿风向一道正弦波 + 垂直方向一道小一点的「湍流」波，相位按根部的世界坐标走，
//   所以整片草一起起伏、一波一波地过去；位移乘 高度²——根不动、梢动得最多。
// - 光照：漫反射按「法线 = 正上方」算（不然几万条朝向随机的叶片会闪成一片噪点），
//   影子里留一层底（`shadow_floor`），再加一项逆光透射：顺着太阳看过去、叶片侧对着光、
//   越往梢越薄，就越透亮。

// 世界空间的风位移（还没乘梢部遮罩）。`base` 是根部的世界 xz。
fn meadow_wind(base: vec2<f32>, direction: vec2<f32>, strength: f32, speed: f32, frequency: f32,
               turbulence: f32, lean: f32, time: f32) -> vec3<f32> {
    let primary = sin(dot(base, direction) * frequency + time * speed);
    let perpendicular = vec2<f32>(-direction.y, direction.x);
    let secondary = sin(dot(base, perpendicular) * frequency * 1.7 + time * speed * 0.73) * turbulence;
    return vec3<f32>(direction.x, 0.0, direction.y) * ((primary + secondary) * strength + lean);
}

// 世界空间的位移换回模型空间：投影到模型矩阵的三根轴上、除以轴长的平方
// （节点有旋转、有不等比缩放都对，只要三根轴互相垂直）。
fn meadow_to_model(world: vec3<f32>, model: mat4x4<f32>) -> vec3<f32> {
    let x = model[0].xyz;
    let y = model[1].xyz;
    let z = model[2].xyz;
    return vec3<f32>(
        dot(world, x) / max(dot(x, x), 1e-5),
        dot(world, y) / max(dot(y, y), 1e-5),
        dot(world, z) / max(dot(z, z), 1e-5),
    );
}

// 一盏直射光的贡献。影子里那一层底（`shadow_floor` 那部分）记进 `transmitted`，
// 引擎不会再乘可见度；其余的照常被影子挡。
fn meadow_direct(surface: ptr<function, Surface>, input: LightingInput, shadow_floor: f32,
                 diffuse_floor: f32) -> vec3<f32> {
    let diffuse = diffuse_floor + (1.0 - diffuse_floor) * max(input.light_direction.y, 0.0);
    let direct = (*surface).base_color.rgb * input.radiance * diffuse;
    (*surface).transmitted += direct * shadow_floor;
    return direct * (1.0 - shadow_floor);
}

// 逆光透射。`tip` 是 0（根）..1（梢）。
fn meadow_backlight(surface: ptr<function, Surface>, input: LightingInput, tip: f32, strength: f32,
                    power: f32, tip_weight: f32) -> f32 {
    let view_to_sun = pow(max(dot((*surface).view_direction, -input.light_direction), 0.0), power);
    let edge_on = 1.0 - abs(dot((*surface).geometric_normal, input.light_direction));
    let thin_tip = mix(1.0, tip, tip_weight);
    return view_to_sun * edge_on * thin_tip * strength;
}
