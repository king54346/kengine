//! `render_frame` 的编码阶段：一个 pass 一个方法。
//!
//! 准备阶段（剔除、合批、上传）在 `lib.rs` 里把这一帧要画的东西攒好，
//! 这里只往命令编码器里录命令，不再碰场景。每个方法的参数就是它真正用到的那几样，
//! 读签名就知道这个 pass 依赖什么——拆之前它们是同一个一千八百行函数里的局部变量。

use super::*;

/// 绑好一个网格的顶点 / 索引缓冲，画一批实例。
///
/// 蒙皮批次要第二路顶点缓冲（骨骼下标与权重）；网格缺了这一路时整批跳过，
/// 而不是用错的布局去画。
fn draw_batch(pass: &mut wgpu::RenderPass<'_>, gpu_mesh: &GpuMesh, batch: &Batch) {
    pass.set_vertex_buffer(0, gpu_mesh.vertex_buffer.slice(..));
    if batch.skinned {
        let Some(skin) = gpu_mesh.skin_buffer.as_ref() else {
            return;
        };
        pass.set_vertex_buffer(1, skin.slice(..));
    }
    pass.set_index_buffer(gpu_mesh.index_buffer.slice(..), wgpu::IndexFormat::Uint32);
    // 实例范围的起点即 `@builtin(instance_index)` 的起始值。
    pass.draw_indexed(
        batch.index_range.0..batch.index_range.0 + batch.index_range.1,
        0,
        batch.first..batch.first + batch.count,
    );
}

/// 阴影 pass 要的东西。
pub(crate) struct ShadowInputs<'a> {
    pub batches: &'a [Batch],
    /// 每个绘制项的对象数据。
    pub objects: &'a [ObjectUniforms],
    /// 每个 GPU 实例一个槽，批次按它数。
    pub slots: &'a [InstanceSlot],
    pub instance_bounds: &'a [kmath::Aabb],
    /// 主投影光源的各层（级联 / 立方体六面 / 聚光一面）。
    pub cascades: &'a [klight::cascade::Cascade],
    /// 额外投影光源的各层矩阵。
    pub local_faces: &'a [Mat4],
    /// 不透明绘制项个数（对象缓冲的容量按它定）。
    pub draw_count: usize,
    pub joint_count: usize,
    pub morph_weight_count: usize,
    /// 着色器时间（秒），带顶点钩子的材质在阴影里按它位移。
    pub time: f32,
}

impl Renderer {
    /// 阴影深度 pass：主光源的每层、额外投影光源的每层各画一遍，只写深度。
    pub(crate) fn encode_shadow_pass(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        input: ShadowInputs<'_>,
    ) {
        let settings = self.shadow.settings;
        let ShadowInputs {
            batches,
            objects,
            slots,
            instance_bounds,
            cascades,
            local_faces,
            draw_count,
            joint_count,
            morph_weight_count,
            time,
        } = input;
        // 深度 pass 有自己的一份骨骼矩阵缓冲：它与主 pass 分属不同的绑定组布局，
        // 共用一个缓冲反而要多传一层引用。数据是同一份，写两遍。
        let shadow_joints_grew = joint_count as u64 > self.shadow.joint_capacity;
        if shadow_joints_grew {
            let capacity = (joint_count as u64).next_power_of_two();
            self.shadow.joint_buffer = create_joint_storage(&self.device, capacity);
            self.shadow.joint_capacity = capacity;
        }
        // 深度 pass 有自己的一份形变权重缓冲，数据同主 pass。
        let shadow_weights_grew = morph_weight_count as u64 > self.shadow.morph_weight_capacity;
        if shadow_weights_grew {
            let capacity = (morph_weight_count as u64).next_power_of_two();
            self.shadow.morph_weight_buffer = create_morph_weight_storage(&self.device, capacity);
            self.shadow.morph_weight_capacity = capacity;
        }
        // 形变增量是静态数据，主 pass 那边可能已经扩过容，这里跟上。
        let shadow_morph_stale = self.shadow.morph_capacity != self.morph_capacity;
        if shadow_morph_stale {
            self.shadow.morph_buffer = create_morph_storage(&self.device, self.morph_capacity);
            self.shadow.morph_capacity = self.morph_capacity;
        }

        // 主 pass 的槽 / 实例缓冲换过的话（阴影的对象绑定组也引用它们），一样要重建。
        let instances_stale = self.shadow.instance_generation != self.instance_generation;
        if draw_count as u64 > self.shadow.object_capacity
            || shadow_joints_grew
            || shadow_weights_grew
            || shadow_morph_stale
            || instances_stale
        {
            let capacity = (draw_count as u64)
                .next_power_of_two()
                .max(self.shadow.object_capacity);
            let (buffer, bind_group) = create_shadow_object_storage(
                &self.device,
                &self.shadow.object_layout,
                capacity,
                &self.shadow.joint_buffer,
                &self.shadow.morph_buffer,
                &self.shadow.morph_weight_buffer,
                &self.slot_buffer,
                &self.instance_buffer,
            );
            self.shadow.object_buffer = buffer;
            self.shadow.object_bind_group = bind_group;
            self.shadow.object_capacity = capacity;
            self.shadow.instance_generation = self.instance_generation;
        }
        if joint_count > 0 {
            self.queue.write_buffer(
                &self.shadow.joint_buffer,
                0,
                bytemuck::cast_slice(&self.joint_scratch),
            );
        }
        if morph_weight_count > 0 {
            self.queue.write_buffer(
                &self.shadow.morph_weight_buffer,
                0,
                bytemuck::cast_slice(&self.morph_weight_scratch),
            );
        }
        // 形变增量只在网格新上传时变，用一次拷贝把主 pass 的那份同步过来。
        if self.morph_used > 0 {
            encoder.copy_buffer_to_buffer(
                &self.morph_buffer,
                0,
                &self.shadow.morph_buffer,
                0,
                self.morph_used * size_of::<MorphDelta>() as u64,
            );
        }

        // 深度 pass 只要模型矩阵，一个对象一份；实例化节点靠主 pass 的槽和实例数据在着色器里展开。
        let shadow_objects: Vec<ShadowObject> = objects
            .iter()
            .map(|object| ShadowObject {
                model: object.model,
                skin: object.skin,
            })
            .collect();
        if !shadow_objects.is_empty() {
            self.queue.write_buffer(
                &self.shadow.object_buffer,
                0,
                bytemuck::cast_slice(&shadow_objects),
            );
        }

        // 每层跑一遍：一次 render pass 只能挂一层当深度附件。
        //
        // 这曾经是级联最主要的代价——N 级就是 N 次**完整**的场景遍历。
        // 现在每级先剔一遍：范围外的不画，投影小于两个纹素的也不画
        // （小物件在几百米外投的影子还不到一个像素）。
        // 所有层的全局量**一次写完**，各占一段。
        //
        // 见 `has_dynamic_offset` 那里的注释：分开写会被 wgpu 的
        // 写入时序合并成最后一次。
        // 主光源的级联在前，额外投影光源的各层接在后面：(矩阵, 是否额外层, 层号, 分辨率)。
        let local_resolution = self.shadow.local_resolution;
        let shadow_layers: Vec<(Mat4, bool, usize, u32)> = cascades
            .iter()
            .enumerate()
            .map(|(index, cascade)| (cascade.matrix, false, index, settings.resolution.max(256)))
            .chain(
                local_faces
                    .iter()
                    .enumerate()
                    .map(|(index, matrix)| (*matrix, true, index, local_resolution)),
            )
            .collect();
        if !local_faces.is_empty() {
            let matrices: Vec<[[f32; 4]; 4]> =
                local_faces.iter().map(Mat4::to_cols_array_2d).collect();
            self.queue.write_buffer(
                &self.shadow.local_matrices,
                0,
                bytemuck::cast_slice(&matrices),
            );
        }
        let mut blob = vec![0u8; SHADOW_GLOBALS_STRIDE as usize * shadow_layers.len().max(1)];
        for (index, &(matrix, _, _, resolution)) in shadow_layers.iter().enumerate() {
            let globals = ShadowGlobals {
                light_view_proj: matrix.to_cols_array_2d(),
                params: [
                    settings.depth_bias,
                    settings.normal_bias,
                    resolution as f32,
                    1.0,
                ],
                frame: [time, 0.0, 0.0, 0.0],
            };
            let start = index * SHADOW_GLOBALS_STRIDE as usize;
            blob[start..start + size_of::<ShadowGlobals>()]
                .copy_from_slice(bytemuck::bytes_of(&globals));
        }
        self.queue
            .write_buffer(&self.shadow.globals_buffer, 0, &blob);

        for (index, &(matrix, local, layer, resolution)) in shadow_layers.iter().enumerate() {
            let layer_batches = cascade_batches(
                batches,
                slots,
                instance_bounds,
                matrix,
                resolution,
                settings.min_shadow_texels,
            );
            // 写 `self.stats`：统计在准备阶段末尾就定格搬进 self 了。
            self.stats.shadow_draw_calls += layer_batches.len() as u32;
            let layer_view = if local {
                &self.shadow.local_layer_views[layer]
            } else {
                &self.shadow.layer_views[layer]
            };

            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("kengine shadow pass"),
                color_attachments: &[],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: layer_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            // 动态偏移选中本层那一段。
            pass.set_bind_group(
                0,
                &self.shadow.globals_bind_group,
                &[index as u32 * SHADOW_GLOBALS_STRIDE as u32],
            );
            pass.set_bind_group(1, &self.shadow.object_bind_group, &[]);

            // 深度 pass 与贴图无关，本可以按网格合并得更狠，
            // 但沿用主 pass 的分批能保证两边的实例下标一一对应——
            // 带顶点钩子的材质也靠这一点直接读主 pass 的逐对象数据。
            //
            // 管线键：(蒙皮, 钩子 id)。普通物体钩子 id 记 nil。
            let mut current: Option<(bool, Uuid)> = None;
            for batch in &layer_batches {
                let Some(gpu_mesh) = self.meshes.get(&batch.mesh_id) else {
                    continue;
                };
                let skinned = usize::from(batch.skinned);
                let hooked = self.hooked_passes_for(batch).and_then(|passes| {
                    self.material_bind_groups
                        .get(&batch.texture_key)
                        .map(|(textures, _)| (passes, textures))
                });
                let key = (
                    batch.skinned,
                    if hooked.is_some() {
                        batch.shader_id
                    } else {
                        Uuid::nil()
                    },
                );
                if let Some((passes, textures)) = hooked {
                    if current != Some(key) {
                        // 从普通管线切过来：group 1 换成主 pass 的逐对象数据（布局不同）。
                        if current.is_none_or(|(_, id)| id.is_nil()) {
                            pass.set_bind_group(1, &self.object_bind_group, &[]);
                        }
                        pass.set_pipeline(&passes.shadow[skinned]);
                        current = Some(key);
                    }
                    pass.set_bind_group(2, textures, &[]);
                } else if current != Some(key) {
                    if current.is_some_and(|(_, id)| !id.is_nil()) {
                        pass.set_bind_group(1, &self.shadow.object_bind_group, &[]);
                    }
                    pass.set_pipeline(if batch.skinned {
                        &self.shadow.skinned_pipeline
                    } else {
                        &self.shadow.pipeline
                    });
                    current = Some(key);
                }
                draw_batch(&mut pass, gpu_mesh, batch);
            }
        }
    }

    /// 深度／法线预通道 + SSAO + 接触阴影。必须排在主 pass **之前**：主 pass 要采那张遮蔽图。
    ///
    /// `first_light` 是 0 号光源（接触阴影朝它走），格式见 `render_frame` 里的说明。
    pub(crate) fn encode_prepass(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        batches: &[Batch],
        view_proj: Mat4,
        camera_position: Vec3,
        first_light: Option<[f32; 4]>,
    ) {
        {
            let mut pass = self.ssao.begin_prepass(encoder);
            pass.set_bind_group(0, &self.globals_bind_group, &[]);
            pass.set_bind_group(1, &self.object_bind_group, &[]);

            // 预通道只关心几何，不关心材质——换管线的判据是「蒙皮与否」，
            // 外加写了顶点钩子的材质有自己的一套（位移后的形状才是 SSAO 该看的）。
            let mut current: Option<(bool, Uuid)> = None;
            for batch in batches {
                let Some(gpu_mesh) = self.meshes.get(&batch.mesh_id) else {
                    continue;
                };
                let hooked = self.hooked_passes_for(batch).and_then(|passes| {
                    self.material_bind_groups
                        .get(&batch.texture_key)
                        .map(|(textures, _)| (passes, textures))
                });
                let key = (
                    batch.skinned,
                    if hooked.is_some() {
                        batch.shader_id
                    } else {
                        Uuid::nil()
                    },
                );
                if current != Some(key) {
                    match hooked {
                        Some((passes, _)) => {
                            pass.set_pipeline(&passes.prepass[usize::from(batch.skinned)])
                        }
                        None => pass.set_pipeline(self.ssao.prepass_pipeline(batch.skinned)),
                    }
                    current = Some(key);
                }
                if let Some((_, textures)) = hooked {
                    pass.set_bind_group(2, textures, &[]);
                }
                draw_batch(&mut pass, gpu_mesh, batch);
            }
        }
        let ssao_on = self.ssao.settings.enabled;
        if ssao_on {
            self.ssao
                .run(&self.queue, encoder, view_proj, camera_position);
        }
        if self.ssao.settings.contact.enabled {
            // 没有能投影的光（只有半球光）时强度给 0，等于把绿通道清成 1。
            self.ssao.run_contact(
                &self.queue,
                encoder,
                view_proj,
                first_light.unwrap_or([0.0, 1.0, 0.0, 0.0]),
                first_light.is_some(),
                ssao_on,
            );
        }
        self.stats.draw_calls += batches.len() as u32 + 1;
    }

    /// 主 pass：不透明几何 + 天空，画到 HDR 目标。覆盖层清成全透明、不画天空。
    pub(crate) fn encode_main_pass(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        batches: &[Batch],
        overlay: bool,
    ) {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("kengine render pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: self.post.hdr_target(),
                resolve_target: None,
                depth_slice: None,
                ops: wgpu::Operations {
                    // 覆盖层清成全透明：没画到的地方合成时透出主画面。
                    load: wgpu::LoadOp::Clear(if overlay {
                        wgpu::Color::TRANSPARENT
                    } else {
                        wgpu::Color {
                            r: 0.05,
                            g: 0.05,
                            b: 0.08,
                            a: 1.0,
                        }
                    }),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &self.depth_view,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(1.0),
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });

        pass.set_pipeline(self.standard_pipelines.pick(false, false));
        pass.set_bind_group(0, &self.globals_bind_group, &[]);
        // 整个实例数组绑一次就够，着色器按实例号自己寻址。
        pass.set_bind_group(1, &self.object_bind_group, &[]);
        pass.set_bind_group(3, &self.brdf_bind_group, &[]);

        let mut current_pipeline: Option<(bool, bool, Uuid)> = None;
        for batch in batches {
            let Some(gpu_mesh) = self.meshes.get(&batch.mesh_id) else {
                continue;
            };
            let Some((texture_bind_group, _)) = self.material_bind_groups.get(&batch.texture_key)
            else {
                continue;
            };
            // 换管线的判据是「蒙皮与否 + 单双面 + 着色器」。只看蒙皮的话，
            // 相邻两个自定义材质会共用前一个的着色器；漏了单双面的话，
            // 标准材质的双面物体（布料）会沿用前一个单面物体的管线——
            // 背面被剔掉，布从一侧看是透明的，而阴影照样有，很难想到是这里。
            let key = (batch.skinned, batch.double_sided, batch.shader_id);
            if current_pipeline != Some(key) {
                pass.set_pipeline(self.pipeline_for(batch, false));
                // 换管线不影响已绑定的组，它们的布局是同一个。
                current_pipeline = Some(key);
            }
            pass.set_bind_group(2, texture_bind_group, &[]);
            draw_batch(&mut pass, gpu_mesh, batch);
        }

        // 天空放在最后画：此时深度缓冲已填好，只有空白像素能通过 LessEqual 测试，
        // 被物体挡住的部分直接被剔除，省下大片无用的着色。
        if !overlay {
            pass.set_pipeline(&self.sky_pipeline);
            pass.set_bind_group(0, &self.sky_bind_group, &[]);
            pass.draw(0..3, 0..1);
        }
    }

    /// 半透明、精灵、粒子、调试线：只读深度的第二个 pass。
    ///
    /// 合成一个 pass 的前提是这里**没人写深度**：四者的管线都是只测不写。
    /// 只读深度换来两件事：软粒子能把深度当纹理采样；自定义材质能同时
    /// 读场景颜色和场景深度，做出按水深分层的效果。
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn encode_transparent_pass(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        transparent_batches: &[Batch],
        sprite_batches: &[ksprite::Batch],
        particle_batches: &[particle::ParticleBatch],
        line_draws: &[gizmo::RetainedDraw],
        gizmo_draw: &gizmo::GizmoDraw,
        overlay: bool,
    ) {
        // 写深度的半透明（`Material::depth_write`）先单独画一趟：只读深度附件的那一趟里不能写深度，
        // 而那一趟又要把同一张深度当采样源（软粒子、按水深着色）。代价：这一批里的材质采不到场景深度
        // （绑的是占位图），而且它们整体排在普通半透明之前，彼此之间仍然按远近排。
        let (writers, readers): (Vec<Batch>, Vec<Batch>) = transparent_batches
            .iter()
            .partition(|batch| batch.depth_write);
        if !writers.is_empty() {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("kengine transparent depth-write pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: self.post.hdr_target(),
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &self.depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_bind_group(0, &self.globals_bind_group, &[]);
            pass.set_bind_group(1, &self.object_bind_group, &[]);
            pass.set_bind_group(3, &self.brdf_bind_group, &[]);
            for batch in &writers {
                let Some(gpu_mesh) = self.meshes.get(&batch.mesh_id) else {
                    continue;
                };
                let Some((texture_bind_group, _)) =
                    self.material_bind_groups.get(&batch.texture_key)
                else {
                    continue;
                };
                pass.set_pipeline(self.pipeline_for(batch, true));
                pass.set_bind_group(2, texture_bind_group, &[]);
                draw_batch(&mut pass, gpu_mesh, batch);
            }
        }
        let transparent_batches = readers.as_slice();

        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("kengine transparent pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: self.post.hdr_target(),
                resolve_target: None,
                depth_slice: None,
                // 接着上一个 pass 画，不能清。
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &self.depth_view,
                // `None` = 只读。这一条就是软粒子和折射能成立的原因。
                depth_ops: None,
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });

        // 半透明必须在天空之后：它要和背后的东西混合，而天空就是最远的那个「背后」。
        if !transparent_batches.is_empty() {
            pass.set_bind_group(0, &self.globals_bind_group, &[]);
            pass.set_bind_group(1, &self.object_bind_group, &[]);
            // 换成带真实场景深度的那份：这个 pass 用只读深度附件，
            // 允许同一张纹理既当附件又当采样源。
            pass.set_bind_group(3, &self.brdf_bind_group_transparent, &[]);
            let mut current: Option<(bool, bool, Uuid)> = None;
            for batch in transparent_batches {
                let Some(gpu_mesh) = self.meshes.get(&batch.mesh_id) else {
                    continue;
                };
                let Some((texture_bind_group, _)) =
                    self.material_bind_groups.get(&batch.texture_key)
                else {
                    continue;
                };
                // 和不透明那边同一个判据，见那里的注释。
                let key = (batch.skinned, batch.double_sided, batch.shader_id);
                if current != Some(key) {
                    pass.set_pipeline(self.pipeline_for(batch, true));
                    current = Some(key);
                }
                pass.set_bind_group(2, texture_bind_group, &[]);
                draw_batch(&mut pass, gpu_mesh, batch);
            }
        }

        // 覆盖层只画网格：精灵、粒子、调试线属于主画面，再画一遍就重影了。
        if overlay {
            return;
        }
        // 2D 精灵画在半透明之后、粒子之前：精灵该被粒子盖住（粒子通常是特效）。
        self.sprites.draw(&mut pass, sprite_batches);
        // 粒子在精灵之后：它们半透明且不写深度，任何在它们之后画的
        // 不透明物体都会把它们盖掉——包括天空。
        self.particles.draw(&mut pass, particle_batches);
        // 调试线放在最后：它要盖在所有东西上面，而且不写深度，
        // 所以画在哪一步都不会影响别人，唯独顺序决定了它自己可不可见。
        self.gizmos.draw_retained(&mut pass, line_draws);
        self.gizmos.draw(&mut pass, gizmo_draw);
    }

    /// 后处理遮罩。深度这时已经齐了（不透明写的），遮罩 pass 拿它判断「看得见还是被挡住」。
    /// 一个带遮罩位的物体都没有时也得跑一次：清零。
    pub(crate) fn encode_mask_pass(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        mask_batches: &[Batch],
    ) {
        let mut pass = self.mask.begin(encoder);
        if mask_batches.is_empty() {
            return;
        }
        pass.set_bind_group(0, &self.globals_bind_group, &[]);
        pass.set_bind_group(1, &self.object_bind_group, &[]);
        pass.set_bind_group(2, self.mask.depth_bind_group(), &[]);
        let mut current_skinned: Option<bool> = None;
        for batch in mask_batches {
            let Some(gpu_mesh) = self.meshes.get(&batch.mesh_id) else {
                continue;
            };
            if current_skinned != Some(batch.skinned) {
                pass.set_pipeline(self.mask.pipeline(batch.skinned));
                current_skinned = Some(batch.skinned);
            }
            draw_batch(&mut pass, gpu_mesh, batch);
        }
    }

    /// UI：画在后处理之后——UI 的颜色是设计好的，过一遍色调映射会被整体压暗，
    /// 白色不再是白色。代价是 UI 拿不到 bloom。
    pub(crate) fn encode_ui_pass(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        list: &kui::DrawList,
        scale: f32,
    ) {
        if list.is_empty() {
            return;
        }
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("kengine ui pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                resolve_target: None,
                depth_slice: None,
                // 保留后处理的输出，UI 叠在上面。
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        // UI 画在交换链上，按窗口尺寸（低分辨率渲染时离屏目标更小，UI 不跟着糊）。
        self.ui
            .draw(&mut pass, list, [self.size.width, self.size.height], scale);
    }

    /// 排一次截图：把 `source` 拷进一块可映射的缓冲。提交之后用 [`save_screenshot`] 写盘。
    ///
    /// 返回 `(缓冲, 路径, 宽, 高, 每行字节, 是不是 BGRA)`。没有排队的截图时返回 `None`。
    pub(crate) fn encode_screenshot(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        source: Option<&wgpu::Texture>,
    ) -> Option<(wgpu::Buffer, std::path::PathBuf, u32, u32, u32, bool)> {
        let texture = source?;
        let path = self.screenshot.take()?;
        let width = texture.width();
        let height = texture.height();
        let bytes_per_row = (width * 4).div_ceil(256) * 256;
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("kengine screenshot"),
            size: u64::from(bytes_per_row) * u64::from(height),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        encoder.copy_texture_to_buffer(
            texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(bytes_per_row),
                    rows_per_image: Some(height),
                },
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        let bgra = matches!(
            texture.format(),
            wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Bgra8UnormSrgb
        );
        Some((buffer, path, width, height, bytes_per_row, bgra))
    }
}
