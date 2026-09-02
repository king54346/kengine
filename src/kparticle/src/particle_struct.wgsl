// 一个粒子在显存里的样子。**CPU 侧的 `GpuParticle` 和这里必须逐字节一致。**
//
// 这份声明单独一个文件，是为了让**计算着色器也能用同一份**：
// GPU 粒子由用户自己的 compute 写进一块 storage buffer，然后直接交给
// 粒子管线画。两边各抄一遍的话，字段顺序或填充一旦对不上，
// wgpu **不会报错**（绑定只校验总长度），画出来是一堆乱飞的方片。
//
// Rust 侧拼在自己的源码前面即可：
//
// ```ignore
// let source = format!("{}\n{}", kparticle::PARTICLE_STRUCT_WGSL, my_compute_source);
// ```

struct Particle {
    position: vec3<f32>,
    size: f32,
    color: vec4<f32>,
    rotation: f32,
    // 填充写成三个标量而不是一个 vec3：vec3 的对齐要求是 16 字节，
    // 会把自己推到偏移 48，整个结构体因此涨到 64 字节，
    // 与 CPU 侧紧凑排布的 48 字节对不上，绑定时就会被 wgpu 打回。
    padding_x: f32,
    padding_y: f32,
    padding_z: f32,
};
