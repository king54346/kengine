//! `render_frame` 的准备阶段里能独立出来的几块：选相机、收集光源、同步环境资源、
//! 主光源阴影分层、收集绘制项、上传每帧的缓冲。
//!
//! 和 `passes.rs`（编码阶段）一样，拆出来的目的是让每一块的输入输出写在签名上。

use super::*;

/// 这一帧的光源：着色器要的数组（全局光在前、可聚簇的在后）和派生出来的几样。
pub(crate) struct FrameLights<'a> {
    pub lights: Vec<GpuLight>,
    /// 前面几盏是全局光（方向光、半球光）。
    pub global_count: usize,
    /// 可聚簇光源的包围球，顺序和 `lights[global_count..]` 一致。
    pub cluster_spheres: Vec<klight::cluster::ClusterLight>,
    /// 额外投影光源各层的光空间矩阵。
    pub local_faces: Vec<Mat4>,
    /// 主投影光源（占 0 号）。
    pub shadow_caster: Option<(&'a klight::Light, Mat4)>,
}

/// 收集好的绘制项和每帧要上传的数组。
pub(crate) struct CollectedDraws<'a> {
    pub draws: Vec<DrawCall>,
    pub transparent_draws: Vec<DrawCall>,
    pub mask_draws: Vec<DrawCall>,
    pub joints: Vec<[[f32; 4]; 4]>,
    pub morph_weights: Vec<f32>,
    /// 这台相机实际画的对象数（不含只进遮罩的）。
    pub drawn: u32,
    pub triangles: u32,
    /// 实例化节点的实例（场景里的切片），见 [`FrameInstances::lists`](crate::FrameInstances)。
    pub instance_lists: Vec<&'a [kscene::Instance]>,
}

/// 后处理链要的这一帧的量。
pub(crate) struct PostFrameParams<'a> {
    pub view: Mat4,
    pub projection: Mat4,
    /// 不带亚像素抖动的投影。
    pub clean_projection: Mat4,
    pub view_proj: Mat4,
    pub clip_view_proj: Mat4,
    pub prev_view_proj: Mat4,
    pub camera_position: Vec3,
    pub camera: Camera,
    /// 这一帧的抖动（NDC）。
    pub jitter: Vec2,
    pub shader_time: f32,
    pub frame_delta: f32,
    pub post_settings: PostSettings,
    /// 0 号光源 `(方向或位置, 颜色)`，体积光、接触阴影朝它走。
    pub first_light: Option<([f32; 4], [f32; 4])>,
    pub cascades: &'a [klight::cascade::Cascade],
    pub cascade_splits: [f32; 4],
    /// 阴影开着时是主光源阴影的类型。
    pub shadow_kind: Option<klight::cascade::ShadowKind>,
    pub effects: &'a PostStack,
    pub taa: bool,
    pub prepass_needed: bool,
    pub mask_needed: bool,
}

/// 一份材质这一帧解析出来的东西：贴图绑定组键、管线，以及逐实例要抄进对象缓冲的参数。
#[derive(Clone, Copy)]
pub(crate) struct ResolvedMaterial {
    pub texture_key: [Uuid; TEXTURE_KEY_SLOTS],
    pub shader_id: Uuid,
    pub double_sided: bool,
    pub transparent: bool,
    /// 半透明但写深度（`Material::depth_write`）。
    pub depth_write: bool,
    /// 半透明也投影（`Material::blended_shadows`）。
    pub blended_shadows: bool,
    pub base_color: [f32; 4],
    pub metallic: f32,
    pub roughness: f32,
    pub normal_scale: f32,
    pub occlusion_strength: f32,
    pub emissive: [f32; 4],
    pub uv_transform: [f32; 4],
    pub params: [[f32; 4]; PARAM_SLOTS],
}

/// 主投影光源的阴影分层：方向光走级联，点光立方体六面，聚光 / 面光一面。
pub(crate) fn primary_shadow_faces(
    shadow_caster: Option<(&klight::Light, Mat4)>,
    clip_view_proj: Mat4,
    visible_bounds: kmath::Aabb,
    settings: klight::cascade::CascadeSettings,
) -> (Vec<klight::cascade::Cascade>, klight::cascade::ShadowKind) {
    match shadow_caster {
        Some((light, transform)) => {
            let position = transform.w_axis.truncate();
            let range = light.kind.range().clamp(0.5, 1.0e4);
            // 近平面取范围的千分之一：太小的话深度精度都挤在灯跟前。
            let near = (range * 0.001).max(0.02);
            match light.kind {
                klight::LightKind::Point { .. } => (
                    klight::cascade::point_faces(position, near, range),
                    klight::cascade::ShadowKind::Cube,
                ),
                klight::LightKind::Spot { outer_angle, .. } => (
                    klight::cascade::spot_face(
                        position,
                        light.direction(transform),
                        outer_angle,
                        near,
                        range,
                    ),
                    klight::cascade::ShadowKind::Spot,
                ),
                // 面光源往半个空间发光，当一盏很宽的聚光。
                klight::LightKind::Rect { .. } => (
                    klight::cascade::spot_face(
                        position,
                        light.direction(transform),
                        80.0,
                        near,
                        range,
                    ),
                    klight::cascade::ShadowKind::Spot,
                ),
                _ => (
                    klight::cascade::compute(
                        clip_view_proj,
                        light.direction(transform),
                        visible_bounds,
                        settings,
                    ),
                    klight::cascade::ShadowKind::Cascades,
                ),
            }
        }
        None => (Vec::new(), klight::cascade::ShadowKind::Cascades),
    }
}

impl Renderer {
    /// 这一遍用哪台相机。离屏视图 / 覆盖层那台不存在时返回 `None`（这一遍不画）。
    pub(crate) fn frame_camera(
        &self,
        scene: &Scene,
        mode: &FrameMode<'_>,
    ) -> Option<(Mat4, Camera)> {
        // 相机：捕获时由调用方指定那一面的朝向；离屏视图取那个编号的相机；
        // 否则取场景里第一个启用的屏幕相机，没有就用一个看向原点的默认视角。
        Some(match *mode {
            FrameMode::Capture(face) => (face.camera_to_world, face.camera),
            FrameMode::View(slot) => {
                let (_, transform, camera) = scene
                    .view_cameras()
                    .into_iter()
                    .find(|(candidate, _, _)| *candidate == slot)?;
                (transform, camera)
            }
            FrameMode::Overlay => scene.overlay_camera()?,
            _ => scene.active_camera().unwrap_or_else(|| {
                let eye = Vec3::new(0.0, 1.5, 3.0);
                (
                    Mat4::look_at_rh(eye, Vec3::ZERO, Vec3::Y).inverse(),
                    Camera::default(),
                )
            }),
        })
    }

    /// 收集光源，超出容量的部分丢弃并告警；顺带给额外投影光源分阴影层。
    pub(crate) fn gather_lights<'a>(
        &self,
        scene: &'a Scene,
        camera_position: Vec3,
    ) -> FrameLights<'a> {
        // 收集光源，超出容量的部分丢弃并告警。
        //
        // 投射阴影的光源必须占据 index 0——着色器只对首个光源做阴影判定，
        // 顺序错了会导致阴影套在错误的光源上。
        // 光源分成两段：**前面是全局光**（方向光、半球光——没有位置也没有
        // 范围，照亮一切），**后面是可聚簇的**（点光源、聚光灯）。
        //
        // 分段是聚簇的前提：全局光塞进簇里等于每个簇都有它们，白白占名单。
        // 着色器无条件遍历前一段，按簇遍历后一段。
        //
        // 投射阴影的那盏必须占据 index 0——着色器只对首个光源做阴影判定。
        let shadow_caster = scene.shadow_caster();
        let mut global_lights: Vec<GpuLight> = Vec::new();
        let mut clustered_lights: Vec<GpuLight> = Vec::new();
        let mut cluster_spheres: Vec<klight::cluster::ClusterLight> = Vec::new();

        if let Some((light, transform)) = shadow_caster {
            global_lights.push(light.to_gpu(transform));
        }

        // ── 额外的投影光源 ──
        // 主投影光源之外还标了 `cast_shadows` 的聚光 / 点光，按离相机多近排队分层：
        // 聚光一层、点光六层，分完为止。分到的在 `extra.z` 里记「起始层 + 1」。
        let (local_assign, local_faces) = self.assign_local_shadows(scene, camera_position);

        let mut caster_skipped = false;
        let mut overflowed = false;
        for (light_index, (light, transform)) in scene.visible_lights().enumerate() {
            // 跳过已放在首位的那一盏；后续同样标记了投影的光源按普通光源处理。
            if light.cast_shadows && shadow_caster.is_some() && !caster_skipped {
                caster_skipped = true;
                continue;
            }
            if global_lights.len() + clustered_lights.len() >= MAX_LIGHTS {
                overflowed = true;
                break;
            }

            let mut gpu = light.to_gpu(transform);
            if let Some(&(_, base)) = local_assign.iter().find(|(index, _)| *index == light_index) {
                gpu.extra[2] = base + 1;
            }
            match light.kind {
                klight::LightKind::Directional | klight::LightKind::Hemisphere { .. } => {
                    global_lights.push(gpu)
                }
                _ => {
                    cluster_spheres.push(klight::cluster::ClusterLight {
                        position: transform.w_axis.truncate(),
                        radius: light.kind.range(),
                    });
                    clustered_lights.push(gpu);
                }
            }
        }
        if overflowed {
            klog::once!(klog::warn!("场景光源超过上限 {MAX_LIGHTS}，多余的已被忽略"));
        }

        // 全局光排在前面，可聚簇的接在后面。簇名单里存的是**后一段里的下标**，
        // 着色器取用时要加上全局段的长度。
        let global_count = global_lights.len();
        let mut lights = global_lights;
        lights.extend_from_slice(&clustered_lights);
        FrameLights {
            lights,
            global_count,
            cluster_spheres,
            local_faces,
            shadow_caster,
        }
    }

    /// cookie 图集与 HDR 环境图：换了才重传，并重建引用它们的绑定组。
    pub(crate) fn sync_environment(&mut self, scene: &Scene) {
        // ── cookie 图集 ──
        //
        // 换了才重传。图集是长期资源，每帧重传一张多层纹理是实打实的浪费。
        // 换了之后 group(3) 要重建——旧的绑定组还指着已经没人用的那块显存。
        let atlas_id = scene.cookie_atlas().map(ktexture::Texture::id);
        if atlas_id != self.cookie_id {
            self.cookie = scene
                .cookie_atlas()
                .map(|texture| upload_texture(&self.device, &self.queue, texture));
            self.cookie_id = atlas_id;
            self.rebuild_scene_bind_groups();
        }

        // ── HDR 环境图 ──
        // 只在版本号变了时重传：一条 256×128 的 mip 链是几兆的浮点数据，
        // 每帧重传纯属浪费，而它只在换环境图时才变。
        if scene.environment_version() != self.environment_version {
            self.environment_version = scene.environment_version();
            let probe_levels: Vec<&[kpbr::prefilter::PrefilteredLevel]> = scene
                .reflection_probes()
                .iter()
                .map(|entry| entry.levels.as_slice())
                .collect();
            let uploaded = scene.prefiltered_environment().and_then(|levels| {
                upload_prefiltered_environment(&self.device, &self.queue, levels, &probe_levels)
                    .map(|view| (view, levels.len()))
            });

            let (view, mips) = match uploaded {
                Some((view, mips)) => (view, mips),
                // 换回程序化天空：绑占位图，着色器靠 `ibl_params.x == 0`
                // 跳过采样。
                None => (create_placeholder_environment(&self.device), 0),
            };
            self.environment_mips = mips;
            self.environment_view = view.clone();
            self.rebuild_scene_bind_groups();
            // 天空 pass 也要跟着换：不换的话反射来自新 HDR、
            // 天上还是旧的那张，两者对不上。
            //
            // 背景用原分辨率的那张而不是预滤波链：链的第 0 级只有 256 宽，
            // 铺满屏幕会糊成马赛克。没有原图（比如只有探针）时才退回链。
            let background = scene
                .environment_background()
                .filter(|_| mips > 0)
                .map(|image| upload_environment_background(&self.device, &self.queue, image))
                .unwrap_or_else(|| view.clone());
            self.sky_bind_group = create_sky_bind_group(
                &self.device,
                &self.sky_layout,
                &self.sky_buffer,
                &background,
                &view,
            );
        }
    }

    /// 把可见的绘制项转成 `DrawCall`，顺便把没上传过的网格与贴图传到显存、
    /// 攒下骨骼矩阵和形变权重。`visible` 会被清空（分配留给下一帧）。
    pub(crate) fn collect_draws<'a>(
        &mut self,
        scene: &'a Scene,
        visible: &mut Vec<kscene::RenderItem<'a>>,
        camera_layers: u32,
        camera_position: Vec3,
        mask_needed: bool,
        velocity_needed: bool,
    ) -> CollectedDraws<'a> {
        // 收集绘制项，顺便把没上传过的网格与贴图传到显存。
        // 标准材质建一次就够：它内部是带 String 键的哈希表，
        // 放在循环里等于每个对象都重新分配一遍。
        let default_material = Material::standard();
        let mut materials: FxHashMap<kmaterial::MaterialKey, ResolvedMaterial> =
            FxHashMap::default();
        // 探针参数拿出来一份：`select` 要一个连续切片，而场景里
        // 存的是带像素的条目。
        let probe_params: Vec<kpbr::probe::ReflectionProbe> = scene
            .reflection_probes()
            .iter()
            .map(|entry| entry.probe)
            .collect();
        let mut draws = Vec::with_capacity(visible.len());
        // 半透明的单独收：它们要按距离排序，混不进不透明的批次里。
        let mut transparent_draws: Vec<DrawCall> = Vec::new();
        // 要进后处理遮罩的，各复制一份：遮罩 pass 只画它们，
        // 而其中有些根本不在这台相机的层里（「只当模板」的物体）。
        let mut mask_draws: Vec<DrawCall> = Vec::new();
        // 上一帧的骨骼矩阵，先攒在这里，最后接到本帧的后面。
        let mut prev_joints: Vec<[[f32; 4]; 4]> = Vec::new();
        let mut drawn = 0u32;
        let mut triangles = 0u32;
        let mut instance_lists: Vec<&'a [kscene::Instance]> = Vec::new();
        // 所有蒙皮实例的骨骼矩阵拼进同一个数组，各实例记下自己的起点。
        let mut joints = std::mem::take(&mut self.joint_scratch);
        joints.clear();
        let mut morph_weights = std::mem::take(&mut self.morph_weight_scratch);
        morph_weights.clear();
        for item in visible.drain(..) {
            let in_view = item.layers & camera_layers != 0;
            let masked = mask_needed && item.post_mask != 0;
            if !in_view && !masked {
                continue;
            }
            // 一个实例都没有的实例化节点：什么也不画。
            let copies = item.instances.map_or(1, <[_]>::len) as u32;
            if copies == 0 {
                continue;
            }
            if in_view {
                drawn += copies;
                triangles = triangles
                    .saturating_add((item.mesh.triangle_count() as u32).saturating_mul(copies));
            }

            let mesh = item.mesh;
            // 显存里那份是不是这一版。版本对不上说明顶点被改过
            // （顶点动画每帧都会），要么原地覆写、要么重建。
            //
            // 绝大多数帧、绝大多数物体走第一条：一次查表拿到形变参数、顺手记下用到了。
            let frame = self.frame_index;
            let cached = self.meshes.get_mut(&mesh.id()).and_then(|gpu| {
                (gpu.version == mesh.version()).then(|| {
                    gpu.last_used = frame;
                    (gpu.morph_offset, gpu.morph_count)
                })
            });
            let stale = cached.is_none() && self.meshes.contains_key(&mesh.id());
            if stale {
                self.refresh_mesh(mesh);
            }

            if cached.is_none() && !self.meshes.contains_key(&mesh.id()) {
                // 形变增量是随网格一次性上传的静态数据，追加到全局缓冲末尾。
                let (morph_offset, morph_count) = self.upload_morph_targets(mesh);
                let gpu_mesh = GpuMesh {
                    vertex_buffer: self.device.create_buffer_init(
                        &wgpu::util::BufferInitDescriptor {
                            label: Some("kengine vertex buffer"),
                            contents: bytemuck::cast_slice(mesh.vertices()),
                            // COPY_DST 是给顶点动画留的：几何改了之后
                            // `refresh_mesh` 要原地覆写这块缓冲，而不是
                            // 每帧重新分配一个。不带这个标志 wgpu 会拒绝
                            // `write_buffer`。
                            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                        },
                    ),
                    // 蒙皮属性单独一路顶点缓冲，静态网格没有这一路。
                    skin_buffer: mesh.skin().map(|skin| {
                        self.device
                            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                                label: Some("kengine skin buffer"),
                                contents: bytemuck::cast_slice(skin),
                                usage: wgpu::BufferUsages::VERTEX,
                            })
                    }),
                    index_buffer: self.device.create_buffer_init(
                        &wgpu::util::BufferInitDescriptor {
                            label: Some("kengine index buffer"),
                            contents: bytemuck::cast_slice(mesh.indices()),
                            usage: wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST,
                        },
                    ),
                    index_count: mesh.index_count(),
                    version: mesh.version(),
                    morph_offset,
                    morph_count,
                    last_used: self.frame_index,
                };
                self.meshes.insert(mesh.id(), gpu_mesh);
            }

            let material = item.material.unwrap_or(&default_material);
            // 同一份材质（克隆出来的也算，内容键相同）这一帧只解析一次：
            // 贴图键、管线、十几个按名字查的参数。一千个物体八种材质时就是八次而不是一千次。
            let resolved = match materials.get(&material.cache_key()) {
                Some(resolved) => *resolved,
                None => {
                    let resolved = self.resolve_material(material);
                    materials.insert(material.cache_key(), resolved);
                    resolved
                }
            };
            let ResolvedMaterial {
                texture_key,
                shader_id,
                ..
            } = resolved;

            // 形变权重逐实例写进权重缓冲：同一个网格的两个实例可以有不同的表情。
            let morph = cached.unwrap_or_else(|| {
                self.meshes
                    .get_mut(&mesh.id())
                    .map(|gpu| {
                        gpu.last_used = frame;
                        (gpu.morph_offset, gpu.morph_count)
                    })
                    .unwrap_or((0, 0))
            });
            let weight_offset = morph_weights.len() as u32;
            if morph.1 > 0 {
                morph_weights.extend(
                    (0..morph.1 as usize)
                        .map(|index| item.morph_weights.get(index).copied().unwrap_or(0.0)),
                );
            }

            // 只有网格自己也带蒙皮属性时才走蒙皮管线：
            // 骨架挂在没有蒙皮顶点的网格上是导入出的错，按静态画至少不会崩。
            let skin_offset = match item.skin.filter(|_| mesh.is_skinned()) {
                Some(matrices) => {
                    let offset = joints.len() as u32;
                    joints.extend(matrices.iter().map(|m| m.to_cols_array_2d()));
                    Some(offset)
                }
                None => None,
            };
            // 上一帧的骨骼矩阵：只有要运动向量时才记。偏移先按 `prev_joints`
            // 的相对位置记，收集完再统一加上本帧矩阵的总数。
            let prev_skin_offset = match (skin_offset, item.skin) {
                (Some(_), Some(matrices)) if velocity_needed => {
                    let relative = prev_joints.len() as u32;
                    match self.motion.previous_joints.get(&item.node) {
                        Some(previous) if previous.len() == matrices.len() => {
                            prev_joints.extend(previous.iter().map(|m| m.to_cols_array_2d()));
                        }
                        _ => prev_joints.extend(matrices.iter().map(|m| m.to_cols_array_2d())),
                    }
                    self.motion
                        .current_joints
                        .insert(item.node, matrices.to_vec());
                    relative
                }
                (Some(current), _) => current,
                _ => 0,
            };

            // 逐对象选探针，用包围盒中心。横跨两个房间的大物体只能
            // 用一个探针——前向渲染的常规取舍，办法是把大物体拆开。
            let (primary, secondary, blend_weight) =
                kpbr::probe::select_blend(&probe_params, item.aabb.center());
            let (probe_position, probe_min, probe_max) = match primary {
                Some(index) => {
                    let probe = &probe_params[index];
                    (
                        // 层号 +1：第 0 层是全局环境。
                        probe.position.extend((index + 1) as f32).to_array(),
                        probe
                            .bounds
                            .min
                            .extend(if probe.parallax { 1.0 } else { 0.0 })
                            .to_array(),
                        probe.bounds.max.extend(probe.intensity).to_array(),
                    )
                }
                // 没探针管它：层号 0（全局环境）、不做视差、强度 1。
                None => ([0.0; 4], [0.0, 0.0, 0.0, 0.0], [0.0, 0.0, 0.0, 1.0]),
            };
            // 过渡的那一半：次探针是罩住同一个点、盒子次小的那个；
            // 没有就是全局环境（层 0，强度 1）。
            let probe_blend = match (primary, secondary) {
                // 压根没进任何探针，无处可过渡。
                (None, _) => [0.0; 4],
                (Some(_), Some(index)) => [
                    (index + 1) as f32,
                    blend_weight,
                    probe_params[index].intensity,
                    0.0,
                ],
                (Some(_), None) => [0.0, blend_weight, 1.0, 0.0],
            };

            let model = item.transform;
            // 上一帧的模型矩阵。第一次见到的物体没有上一帧，用本帧的——速度为零。
            let prev_model = if velocity_needed {
                let previous = self
                    .motion
                    .previous
                    .get(&item.node)
                    .copied()
                    .unwrap_or(model);
                self.motion.current.insert(item.node, model);
                previous
            } else {
                model
            };
            // 用包围盒中心而不是变换的平移：蒙皮网格的变换是单位阵，
            // 拿平移排序的话所有角色都会被当成在原点。
            let depth = (item.aabb.center() - camera_position).length_squared();
            let call = DrawCall {
                mesh_id: mesh.id(),
                shader_id,
                texture_key,
                skinned: skin_offset.is_some(),
                double_sided: resolved.double_sided,
                depth_write: resolved.depth_write,
                depth,
                // 不投影的物体给一个空包围盒：阴影 pass 的逐级剔除会把它剔掉，
                // 而这个包围盒别处用不上（探针选择在上面已经做完了）。
                // 半透明的只有材质开了 `blended_shadows` 才投影（阴影 pass 也画半透明的批次，靠这个空盒子剔掉）。
                aabb: if item.casts_shadows && (!resolved.transparent || resolved.blended_shadows) {
                    item.aabb
                } else {
                    kmath::Aabb::EMPTY
                },
                // 没有子网格材质组时画整份网格——用 `mesh.index_count()`
                // 而不是缓存里的 `gpu.index_count`：两者理应相等，但网格
                // 缓存刚好在上面才建好或刷新过，直接用来源数据更直接。
                index_range: item.index_range.unwrap_or((0, mesh.index_count())),
                instances: item.instances.map(|list| {
                    instance_lists.push(list);
                    instance_lists.len() as u32 - 1
                }),
                uniforms: ObjectUniforms {
                    model: model.to_cols_array_2d(),
                    // 逆转置，保证非均匀缩放下法线方向仍然正确。
                    normal_matrix: model.inverse().transpose().to_cols_array_2d(),
                    base_color: resolved.base_color,
                    metallic: resolved.metallic,
                    roughness: resolved.roughness,
                    normal_scale: resolved.normal_scale,
                    occlusion_strength: resolved.occlusion_strength,
                    emissive: resolved.emissive,
                    skin: [skin_offset.unwrap_or(0), morph.0, morph.1, weight_offset],
                    flags: [item.light_mask, item.post_mask, prev_skin_offset, 0],
                    uv_transform: resolved.uv_transform,
                    probe_position,
                    probe_blend,
                    probe_min,
                    probe_max,
                    params: resolved.params,
                    prev_model: prev_model.to_cols_array_2d(),
                },
            };
            if masked {
                mask_draws.push(call.clone());
            }
            if in_view {
                if resolved.transparent {
                    transparent_draws.push(call);
                } else {
                    draws.push(call);
                }
            }
        }

        // 上一帧的骨骼矩阵接到本帧的后面，偏移从相对改成绝对。
        if velocity_needed && !prev_joints.is_empty() {
            let base = joints.len() as u32;
            joints.extend_from_slice(&prev_joints);
            for call in draws
                .iter_mut()
                .chain(transparent_draws.iter_mut())
                .chain(mask_draws.iter_mut())
                .filter(|call| call.skinned)
            {
                call.uniforms.flags[2] += base;
            }
        }

        CollectedDraws {
            draws,
            transparent_draws,
            mask_draws,
            joints,
            morph_weights,
            drawn,
            triangles,
            instance_lists,
        }
    }

    /// 骨骼矩阵、形变权重、实例数组写进各自的存储缓冲；不够就翻倍，并重建引用它们的绑定组。
    pub(crate) fn upload_frame_buffers(
        &mut self,
        joints: &[[[f32; 4]; 4]],
        morph_weights: &[f32],
        frame: &FrameInstances<'_>,
    ) {
        let (objects, slots) = (&frame.objects, &frame.slots);
        let total_draws = objects.len();
        // 槽、实例缓冲不够就翻倍，换了缓冲要重建对象绑定组（下面几处重建都会带上新的这块），
        // 阴影 pass 那边看 `instance_generation` 变了也跟着重建。
        let mut slots_grew = false;
        if slots.len() as u64 > self.slot_capacity {
            let capacity = (slots.len() as u64).next_power_of_two();
            self.slot_buffer = create_slot_storage(&self.device, capacity);
            self.slot_capacity = capacity;
            slots_grew = true;
        }
        if frame.instance_count as u64 > self.instance_capacity {
            let capacity = (frame.instance_count as u64).next_power_of_two();
            self.instance_buffer = create_instance_storage(&self.device, capacity);
            self.instance_capacity = capacity;
            slots_grew = true;
        }
        if slots_grew {
            self.instance_generation += 1;
        }
        // 骨骼矩阵超出容量时翻倍。它排在对象缓冲之前，
        // 因为对象绑定组引用了骨骼缓冲，换了缓冲就得重建绑定组。
        let joint_grew = joints.len() as u64 > self.joint_capacity;
        if joint_grew {
            let capacity = (joints.len() as u64).next_power_of_two();
            self.joint_buffer = create_joint_storage(&self.device, capacity);
            self.joint_capacity = capacity;
        }

        // 对象数超出缓冲容量时翻倍扩容。
        if total_draws as u64 > self.object_capacity {
            let capacity = (total_draws as u64).next_power_of_two();
            let (buffer, bind_group) = Self::create_object_storage(
                &self.device,
                &self.object_layout,
                capacity,
                &self.joint_buffer,
                &self.morph_buffer,
                &self.morph_weight_buffer,
                &self.slot_buffer,
                &self.instance_buffer,
            );
            self.object_buffer = buffer;
            self.object_bind_group = bind_group;
            self.object_capacity = capacity;
        } else if joint_grew || slots_grew {
            // 对象缓冲没换但骨骼缓冲或槽缓冲换了，绑定组仍然指着旧的，得重建。
            self.object_bind_group = create_object_bind_group(
                &self.device,
                &self.object_layout,
                &self.object_buffer,
                &self.joint_buffer,
                &self.morph_buffer,
                &self.morph_weight_buffer,
                &self.slot_buffer,
                &self.instance_buffer,
            );
        }

        if !joints.is_empty() {
            self.queue
                .write_buffer(&self.joint_buffer, 0, bytemuck::cast_slice(joints));
        }

        // 形变权重每帧重写；缓冲不够就翻倍，并重建引用它的绑定组。
        if morph_weights.len() as u64 > self.morph_weight_capacity {
            let capacity = (morph_weights.len() as u64).next_power_of_two();
            self.morph_weight_buffer = create_morph_weight_storage(&self.device, capacity);
            self.morph_weight_capacity = capacity;
            self.object_bind_group = create_object_bind_group(
                &self.device,
                &self.object_layout,
                &self.object_buffer,
                &self.joint_buffer,
                &self.morph_buffer,
                &self.morph_weight_buffer,
                &self.slot_buffer,
                &self.instance_buffer,
            );
        }
        if !morph_weights.is_empty() {
            self.queue.write_buffer(
                &self.morph_weight_buffer,
                0,
                bytemuck::cast_slice(morph_weights),
            );
        }

        // 一次写完整个数组。逐对象写在上万实例时，光是写入调用本身就很可观。
        if !objects.is_empty() {
            self.queue
                .write_buffer(&self.object_buffer, 0, bytemuck::cast_slice(objects));
        }
        if !slots.is_empty() {
            self.queue
                .write_buffer(&self.slot_buffer, 0, bytemuck::cast_slice(slots));
        }
        // 实例数据：场景里的切片直接拷，一个实例化节点一次。
        for &(base, instances) in &frame.uploads {
            self.queue.write_buffer(
                &self.instance_buffer,
                base as u64 * size_of::<kscene::Instance>() as u64,
                bytemuck::cast_slice(instances),
            );
        }
    }

    /// 后处理：场景 HDR ─→ [HDR 阶段的效果（+ TAA）] ─→ Bloom + 色调映射
    /// ─→ [LDR 阶段的效果（+ SMAA）] ─→ FXAA 或原样拷贝 ─→ `target`。
    ///
    /// 返回 LDR 链最后写到第几张（截图在交换链不能拷时从那里拷）。
    pub(crate) fn encode_post(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        params: PostFrameParams<'_>,
    ) -> usize {
        let PostFrameParams {
            view,
            projection,
            clean_projection,
            view_proj,
            clip_view_proj,
            prev_view_proj,
            camera_position,
            camera,
            jitter,
            shader_time,
            frame_delta,
            post_settings,
            first_light,
            cascades,
            cascade_splits,
            shadow_kind,
            effects,
            taa,
            prepass_needed,
            mask_needed,
        } = params;
        let (light, light_color) = first_light.unwrap_or(([0.0, 0.0, 0.0, -1.0], [0.0; 4]));
        let mut light_view_proj_all =
            [Mat4::IDENTITY.to_cols_array_2d(); klight::cascade::MAX_SHADOW_LAYERS];
        for (index, cascade) in cascades
            .iter()
            .take(klight::cascade::MAX_SHADOW_LAYERS)
            .enumerate()
        {
            light_view_proj_all[index] = cascade.matrix.to_cols_array_2d();
        }
        let is_perspective = clean_projection.w_axis.w == 0.0;
        self.postfx.write_frame(
            &self.queue,
            &postfx::PostFrame {
                view_proj: view_proj.to_cols_array_2d(),
                inverse_view_proj: view_proj.inverse().to_cols_array_2d(),
                clip_view_proj: clip_view_proj.to_cols_array_2d(),
                prev_view_proj: prev_view_proj.to_cols_array_2d(),
                view: view.to_cols_array_2d(),
                projection: projection.to_cols_array_2d(),
                inverse_projection: projection.inverse().to_cols_array_2d(),
                camera_position: camera_position.extend(1.0).to_array(),
                resolution: [
                    self.config.width.max(1) as f32,
                    self.config.height.max(1) as f32,
                    1.0 / self.config.width.max(1) as f32,
                    1.0 / self.config.height.max(1) as f32,
                ],
                time: [shader_time, frame_delta, self.frame_index as f32, 0.0],
                camera: [
                    camera.z_near,
                    camera.z_far,
                    if is_perspective { 1.0 } else { 0.0 },
                    post_settings.exposure,
                ],
                // UV 的 y 朝下，NDC 的 y 朝上。
                jitter: [
                    jitter.x * 0.5,
                    -jitter.y * 0.5,
                    self.prev_jitter[0] * 0.5,
                    -self.prev_jitter[1] * 0.5,
                ],
                light,
                light_color,
                light_view_proj: light_view_proj_all,
                cascade_splits,
                shadow_params: [
                    shadow_kind.map_or(0.0, |kind| kind as u32 as f32),
                    0.0015,
                    self.shadow.settings.depth_bias.max(0.01),
                    0.0,
                ],
            },
        );

        let mut chain: Vec<&PostEffect> = effects.iter().filter(|effect| effect.enabled).collect();
        // 引擎自带的抗锯齿排在各自那一段的最后：TAA 要在辉光之前（辉光会把
        // 抖动放大成闪烁），SMAA 要在所有 LDR 效果之后（描边之类会造出新的边）。
        if taa {
            chain.push(&self.builtin_taa);
        }
        if post_settings.anti_alias == AntiAlias::Smaa {
            chain.push(&self.builtin_smaa);
        }
        let inputs = postfx::FrameInputs {
            depth: &self.depth_view,
            normal: prepass_needed.then(|| self.ssao.normal_view()),
            velocity: prepass_needed.then(|| self.ssao.velocity_view()),
            material: prepass_needed.then(|| self.ssao.material_view()),
            mask: mask_needed.then(|| self.mask.view()),
            ao: self
                .ssao
                .occlusion_active()
                .then(|| self.ssao.raw_occlusion_view()),
            scene: self.post.hdr_target(),
            views: [
                self.views[0].as_ref().map(|(_, view)| view),
                self.views[1].as_ref().map(|(_, view)| view),
            ],
            shadow: &self.shadow.depth_view,
        };

        let hdr_result = self.postfx.run_stage(
            &self.device,
            &self.queue,
            encoder,
            &chain,
            PostStage::Hdr,
            post::HDR_FORMAT,
            Some(self.post.hdr_target()),
            None,
            &inputs,
        );
        let ldr_format = self.post.ldr_format();
        let has_ldr_effects = chain.iter().any(|effect| effect.stage() == PostStage::Ldr);
        // 低分辨率渲染 + FSR1 放大：最后一步换成 EASU + RCAS（FXAA 让位）。
        let fsr =
            post_settings.upscaling == crate::post::Upscaling::Fsr1 && self.render_scale < 1.0;
        // TAAU 要运动向量：预通道没跑（比如不是屏幕帧）就退回双线性。
        let taau = self.taau_active(&post_settings) && prepass_needed;
        let fxaa = post_settings.anti_alias == AntiAlias::Fxaa && !fsr && !taau;
        // 截图要从 LDR 链上拷，所以这一帧强制走那条路。
        let ldr_needed = has_ldr_effects || fxaa || fsr || taau || self.screenshot.is_some();

        if ldr_needed {
            self.postfx
                .ensure_chain(&self.device, PostStage::Ldr, ldr_format);
        }
        {
            let hdr_source = hdr_result
                .and_then(|index| self.postfx.chain_view(PostStage::Hdr, index))
                .unwrap_or(self.post.hdr_target());
            let composite_target = if ldr_needed {
                self.postfx
                    .chain_view(PostStage::Ldr, 0)
                    .expect("上面刚建好")
            } else {
                target
            };
            self.post.run(
                &self.device,
                &self.queue,
                encoder,
                hdr_source,
                inputs.mask,
                composite_target,
            );
        }
        let mut ldr_final_index = 0;
        if ldr_needed {
            let ldr_result = self.postfx.run_stage(
                &self.device,
                &self.queue,
                encoder,
                &chain,
                PostStage::Ldr,
                ldr_format,
                None,
                Some(0),
                &inputs,
            );
            let final_index = ldr_result.unwrap_or(0);
            ldr_final_index = final_index;
            if !fxaa {
                self.postfx.prepare_blit(&self.device, ldr_format, false);
            }
            let final_view = self
                .postfx
                .chain_view(PostStage::Ldr, final_index)
                .expect("上面刚建好");
            if taau {
                let output = (self.size.width, self.size.height);
                // 投影的抖动是 NDC 平移，换回低分辨率像素。
                let jitter_pixels = [
                    jitter.x * self.config.width as f32 * 0.5,
                    jitter.y * self.config.height as f32 * 0.5,
                ];
                let sharpness = post_settings
                    .upscale_sharpening
                    .then_some(post_settings.upscale_sharpness);
                self.post.run_taau(
                    &self.device,
                    &self.queue,
                    encoder,
                    final_view,
                    self.ssao.velocity_view(),
                    target,
                    output,
                    jitter_pixels,
                    sharpness,
                );
            } else if fsr {
                let output = (self.size.width, self.size.height);
                self.post.run_fsr(
                    &self.device,
                    &self.queue,
                    encoder,
                    final_view,
                    target,
                    output,
                    post_settings.upscale_sharpness,
                );
            } else if fxaa {
                self.post
                    .run_fxaa(&self.device, &self.queue, encoder, final_view, target);
            } else {
                self.postfx
                    .blit(&self.device, encoder, final_view, target, ldr_format, None);
            }
        }
        if !taau {
            self.post.skip_taau();
        }
        ldr_final_index
    }

    /// 解析一份材质：确保贴图已上传、管线已建好，把逐实例要的参数一次查完。
    ///
    /// 每帧每份材质调一次（见 `collect_draws`）——贴图可能是这一帧刚加载完的，
    /// 或者被原地改过像素，所以贴图那部分不能跨帧缓存。
    fn resolve_material(&mut self, material: &Material) -> ResolvedMaterial {
        let texture_key = self.ensure_material_textures(material);
        let shader_id = self.ensure_material_pipelines(material);
        ResolvedMaterial {
            texture_key,
            shader_id,
            double_sided: material.double_sided(),
            transparent: material.blend_mode().is_blended(),
            depth_write: material.blend_mode().is_blended() && material.depth_write(),
            blended_shadows: material.blended_shadows(),
            base_color: material.base_color().to_array(),
            metallic: material.metallic(),
            roughness: material.roughness(),
            // 没挂法线贴图时置 0，着色器据此完全跳过切线空间计算。
            normal_scale: if material.get(kpbr::standard::NORMAL_TEXTURE).is_some() {
                material
                    .get("normal_scale")
                    .and_then(kmaterial::MaterialValue::as_float)
                    .unwrap_or(1.0)
            } else {
                0.0
            },
            occlusion_strength: material
                .get(kpbr::standard::OCCLUSION)
                .and_then(kmaterial::MaterialValue::as_float)
                .unwrap_or(1.0),
            // w = 混合方式：0 不透明，1 alpha，2 叠加，3 已预乘。着色器按它决定输出要不要预乘、alpha 写不写。
            emissive: material
                .get(kpbr::standard::EMISSIVE)
                .and_then(kmaterial::MaterialValue::as_vec3)
                .unwrap_or(Vec3::ZERO)
                .extend(match material.blend_mode() {
                    kmaterial::BlendMode::Opaque => 0.0,
                    kmaterial::BlendMode::Alpha => 1.0,
                    kmaterial::BlendMode::Additive => 2.0,
                    kmaterial::BlendMode::Premultiplied => 3.0,
                })
                .to_array(),
            uv_transform: uv_transform_of(material),
            params: custom_params_of(material),
        }
    }
}
