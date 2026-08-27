//! Rust 与 JS 之间的桥：一组扁平的原生函数。
//!
//! 这里刻意**只做最原始的事**——取一个数、写一个数、打一条射线。
//! `Node`、`Vector3` 那些手感全在 `prelude.js` 里包出来：
//! getter/setter、类、链式方法在 JS 是母语，用 boa 的对象 API 拼同样的东西
//! 要多写十倍代码，还更难读。
//!
//! 所有函数都遵守同一条纪律：**先把参数从 VM 里取干净，再借场景**。
//! 取参数可能触发用户的 `toString()`，那会回调进 JS；此时若还持着场景借用，
//! 就是重入。顺序反过来就结构上不可能发生（见 [`crate::host`]）。

use crate::host::{MAX_SIGNALS, MAX_SPAWNS, handle_of, id_of, with_host, with_input, with_scene};
use boa_engine::{
    Context, JsResult, JsValue, NativeFunction, js_string, object::ObjectInitializer,
    property::Attribute,
};
use kcore::pool::Handle;
use kinput::MouseButton;
use kmath::{Quat, Vec3};
use kphysics::RayCastOptions;
use kscene::Node;

/// 向量字段的编号，与 `prelude.js` 里的 `FIELD_*` 一一对应。
const FIELD_POSITION: u32 = 0;
const FIELD_SCALE: u32 = 1;

/// 取一个数值参数，缺省当 0。
fn number(args: &[JsValue], index: usize, context: &mut Context) -> JsResult<f64> {
    match args.get(index) {
        Some(value) => value.to_number(context),
        None => Ok(0.0),
    }
}

/// 取三个数值参数当向量。
fn vec3(args: &[JsValue], start: usize, context: &mut Context) -> JsResult<Vec3> {
    Ok(Vec3::new(
        number(args, start, context)? as f32,
        number(args, start + 1, context)? as f32,
        number(args, start + 2, context)? as f32,
    ))
}

/// 取节点句柄参数。
fn node_arg(
    args: &[JsValue],
    index: usize,
    context: &mut Context,
) -> JsResult<Option<Handle<Node>>> {
    Ok(handle_of(number(args, index, context)?))
}

/// 非有限的数值一律拦下。
///
/// NaN 写进变换，世界矩阵会变 NaN，包围盒随之变 NaN，剔除把它判成不可见——
/// **物体无声无息地消失**，日志里什么都没有。在边界上拦掉最便宜。
fn finite(value: Vec3) -> Option<Vec3> {
    value.is_finite().then_some(value)
}

/// 把一个 `[f64; 3]` 变成 JS 数组。
fn array3(value: Vec3, context: &mut Context) -> JsValue {
    let array = boa_engine::object::builtins::JsArray::new(context);
    let _ = array.push(JsValue::from(value.x as f64), context);
    let _ = array.push(JsValue::from(value.y as f64), context);
    let _ = array.push(JsValue::from(value.z as f64), context);
    array.into()
}

/// 把一个二维向量变成 JS 数组。
fn array2(value: kmath::Vec2, context: &mut Context) -> JsValue {
    let array = boa_engine::object::builtins::JsArray::new(context);
    let _ = array.push(JsValue::from(value.x as f64), context);
    let _ = array.push(JsValue::from(value.y as f64), context);
    array.into()
}

/// 取一个字符串参数，缺省当空串。
///
/// 与整个模块同一条纪律：这一步可能触发用户的 `toString()`，
/// 所以必须在借场景**之前**做完。
fn text(args: &[JsValue], index: usize, context: &mut Context) -> JsResult<String> {
    Ok(match args.get(index) {
        Some(value) => value.to_string(context)?.to_std_string_escaped(),
        None => String::new(),
    })
}

/// 把鼠标键的名字翻成 winit 的枚举。
///
/// 认不出的名字返回 [`None`]，调用方一律当成「没按」——脚本里把
/// `"Left"` 写成 `"leftt"` 不该让整个脚本停掉，但也不该悄悄变成左键。
fn mouse_button(name: &str) -> Option<MouseButton> {
    match name.to_ascii_lowercase().as_str() {
        "left" => Some(MouseButton::Left),
        "right" => Some(MouseButton::Right),
        "middle" => Some(MouseButton::Middle),
        _ => None,
    }
}

/// 往全局装上 `__k` 桥对象。`prelude.js` 会把它包成 `Node` / `Vector3`。
pub(crate) fn register(context: &mut Context) {
    let bridge = ObjectInitializer::new(context)
        // ── 时间 ──
        .function(
            NativeFunction::from_fn_ptr(|_, _, _| {
                Ok(JsValue::from(
                    with_host(|host| host.elapsed).unwrap_or(0.0) as f64
                ))
            }),
            js_string!("time"),
            0,
        )
        .function(
            NativeFunction::from_fn_ptr(|_, _, _| {
                Ok(JsValue::from(
                    with_host(|host| host.dt).unwrap_or(0.0) as f64
                ))
            }),
            js_string!("delta"),
            0,
        )
        // ── 查找 ──
        .function(
            NativeFunction::from_fn_ptr(|_, _, _| {
                let handle = with_host(|host| host.current).unwrap_or(Handle::NONE);
                Ok(JsValue::from(id_of(handle)))
            }),
            js_string!("selfId"),
            0,
        )
        .function(
            NativeFunction::from_fn_ptr(|_, args, context| {
                // 先把字符串取干净（可能触发 JS 的 toString），再借场景。
                let name = match args.first() {
                    Some(value) => value.to_string(context)?.to_std_string_escaped(),
                    None => String::new(),
                };
                let found = with_scene(|scene| scene.find_by_name(&name)).flatten();
                Ok(JsValue::from(match found {
                    Some(handle) => id_of(handle),
                    None => -1.0,
                }))
            }),
            js_string!("find"),
            1,
        )
        .function(
            NativeFunction::from_fn_ptr(|_, args, context| {
                let handle = node_arg(args, 0, context)?;
                let valid = handle
                    .and_then(|handle| with_scene(|scene| scene.try_get(handle).is_some()))
                    .unwrap_or(false);
                Ok(JsValue::from(valid))
            }),
            js_string!("isValid"),
            1,
        )
        // ── 读 ──
        .function(
            NativeFunction::from_fn_ptr(|_, args, context| {
                let handle = node_arg(args, 0, context)?;
                let name = handle
                    .and_then(|handle| {
                        with_scene(|scene| scene.try_get(handle).map(|node| node.name.clone()))
                    })
                    .flatten();
                Ok(match name {
                    Some(name) => JsValue::from(js_string!(name.as_str())),
                    None => JsValue::null(),
                })
            }),
            js_string!("getName"),
            1,
        )
        .function(
            NativeFunction::from_fn_ptr(|_, args, context| {
                let handle = node_arg(args, 0, context)?;
                let field = number(args, 1, context)? as u32;
                let axis = number(args, 2, context)? as usize;

                let value = handle
                    .and_then(|handle| {
                        with_scene(|scene| {
                            let node = scene.try_get(handle)?;
                            let vector = match field {
                                FIELD_SCALE => node.transform.scale,
                                FIELD_POSITION => node.transform.position,
                                // 认不出的字段号只可能是前奏与桥对不上了，
                                // 退回位置比返回垃圾值好排查。
                                _ => node.transform.position,
                            };
                            Some(match axis {
                                1 => vector.y,
                                2 => vector.z,
                                _ => vector.x,
                            })
                        })
                    })
                    .flatten();
                Ok(JsValue::from(value.unwrap_or(0.0) as f64))
            }),
            js_string!("getComponent"),
            3,
        )
        .function(
            NativeFunction::from_fn_ptr(|_, args, context| {
                let handle = node_arg(args, 0, context)?;
                let field = number(args, 1, context)? as u32;
                let axis = number(args, 2, context)? as usize;
                let value = number(args, 3, context)? as f32;

                if !value.is_finite() {
                    return Ok(JsValue::undefined());
                }
                if let Some(handle) = handle {
                    with_scene(|scene| {
                        let Some(node) = scene.try_get_mut(handle) else {
                            return;
                        };
                        let vector = match field {
                            FIELD_SCALE => &mut node.transform.scale,
                            FIELD_POSITION => &mut node.transform.position,
                            _ => &mut node.transform.position,
                        };
                        match axis {
                            1 => vector.y = value,
                            2 => vector.z = value,
                            _ => vector.x = value,
                        }
                    });
                }
                Ok(JsValue::undefined())
            }),
            js_string!("setComponent"),
            4,
        )
        .function(
            NativeFunction::from_fn_ptr(|_, args, context| {
                let handle = node_arg(args, 0, context)?;
                let field = number(args, 1, context)? as u32;
                let Some(value) = finite(vec3(args, 2, context)?) else {
                    return Ok(JsValue::undefined());
                };

                if let Some(handle) = handle {
                    with_scene(|scene| {
                        if let Some(node) = scene.try_get_mut(handle) {
                            match field {
                                FIELD_SCALE => node.transform.scale = value,
                                FIELD_POSITION => node.transform.position = value,
                                _ => node.transform.position = value,
                            }
                        }
                    });
                }
                Ok(JsValue::undefined())
            }),
            js_string!("setVec"),
            5,
        )
        .function(
            NativeFunction::from_fn_ptr(|_, args, context| {
                let handle = node_arg(args, 0, context)?;
                let position = handle
                    .and_then(|handle| {
                        with_scene(|scene| {
                            scene
                                .try_get(handle)
                                .map(|node| node.global_transform().w_axis.truncate())
                        })
                    })
                    .flatten()
                    .unwrap_or(Vec3::ZERO);
                Ok(array3(position, context))
            }),
            js_string!("getGlobalPosition"),
            1,
        )
        .function(
            NativeFunction::from_fn_ptr(|_, args, context| {
                // 本引擎（和 glTF）的约定：前方是 -Z。取世界矩阵的那一列，
                // 所以父节点转了也算数。
                let handle = node_arg(args, 0, context)?;
                let forward = handle
                    .and_then(|handle| {
                        with_scene(|scene| {
                            scene.try_get(handle).map(|node| {
                                (-node.global_transform().z_axis.truncate()).normalize_or_zero()
                            })
                        })
                    })
                    .flatten()
                    .unwrap_or(Vec3::NEG_Z);
                Ok(array3(forward, context))
            }),
            js_string!("getForward"),
            1,
        )
        .function(
            NativeFunction::from_fn_ptr(|_, args, context| {
                let handle = node_arg(args, 0, context)?;
                let visible = handle
                    .and_then(|handle| with_scene(|scene| scene.try_get(handle).map(|n| n.visible)))
                    .flatten()
                    .unwrap_or(false);
                Ok(JsValue::from(visible))
            }),
            js_string!("getVisible"),
            1,
        )
        .function(
            NativeFunction::from_fn_ptr(|_, args, context| {
                let handle = node_arg(args, 0, context)?;
                let visible = args.get(1).map(JsValue::to_boolean).unwrap_or(false);
                if let Some(handle) = handle {
                    with_scene(|scene| {
                        if let Some(node) = scene.try_get_mut(handle) {
                            node.visible = visible;
                        }
                    });
                }
                Ok(JsValue::undefined())
            }),
            js_string!("setVisible"),
            2,
        )
        .function(
            NativeFunction::from_fn_ptr(|_, args, context| {
                let handle = node_arg(args, 0, context)?;
                let velocity = handle
                    .and_then(|handle| {
                        with_scene(|scene| {
                            scene
                                .try_get(handle)
                                .and_then(|node| node.rigid_body())
                                .map(|body| body.linvel())
                        })
                    })
                    .flatten()
                    .unwrap_or(Vec3::ZERO);
                Ok(array3(velocity, context))
            }),
            js_string!("getLinvel"),
            1,
        )
        // ── 写 ──
        .function(
            NativeFunction::from_fn_ptr(|_, args, context| {
                let handle = node_arg(args, 0, context)?;
                let Some(offset) = finite(vec3(args, 1, context)?) else {
                    return Ok(JsValue::undefined());
                };
                if let Some(handle) = handle {
                    with_scene(|scene| {
                        if let Some(node) = scene.try_get_mut(handle) {
                            node.transform.position += offset;
                        }
                    });
                }
                Ok(JsValue::undefined())
            }),
            js_string!("translate"),
            4,
        )
        .function(
            NativeFunction::from_fn_ptr(|_, args, context| {
                let handle = node_arg(args, 0, context)?;
                let angle = number(args, 1, context)? as f32;
                if !angle.is_finite() {
                    return Ok(JsValue::undefined());
                }
                if let Some(handle) = handle {
                    with_scene(|scene| {
                        if let Some(node) = scene.try_get_mut(handle) {
                            node.transform.rotation *= Quat::from_rotation_y(angle);
                        }
                    });
                }
                Ok(JsValue::undefined())
            }),
            js_string!("rotateY"),
            2,
        )
        .function(
            NativeFunction::from_fn_ptr(|_, args, context| {
                let handle = node_arg(args, 0, context)?;
                let Some(target) = finite(vec3(args, 1, context)?) else {
                    return Ok(JsValue::undefined());
                };
                if let Some(handle) = handle {
                    with_scene(|scene| {
                        let Some(node) = scene.try_get_mut(handle) else {
                            return;
                        };
                        let offset = target - node.transform.position;
                        // 目标就在脚下时朝向无从谈起，保持原样比转成 NaN 强。
                        if offset.length_squared() > 1e-12 {
                            let matrix =
                                kmath::Mat4::look_at_rh(Vec3::ZERO, offset, Vec3::Y).inverse();
                            let (_, rotation, _) = matrix.to_scale_rotation_translation();
                            node.transform.rotation = rotation;
                        }
                    });
                }
                Ok(JsValue::undefined())
            }),
            js_string!("lookAt"),
            4,
        )
        .function(
            NativeFunction::from_fn_ptr(|_, args, context| {
                let handle = node_arg(args, 0, context)?;
                let Some(impulse) = finite(vec3(args, 1, context)?) else {
                    return Ok(JsValue::undefined());
                };
                if let Some(handle) = handle {
                    with_scene(|scene| {
                        if let Some(body) = scene
                            .try_get_mut(handle)
                            .and_then(kscene::Node::rigid_body_mut)
                        {
                            body.apply_impulse(impulse);
                        }
                    });
                }
                Ok(JsValue::undefined())
            }),
            js_string!("applyImpulse"),
            4,
        )
        .function(
            NativeFunction::from_fn_ptr(|_, args, context| {
                let handle = node_arg(args, 0, context)?;
                let Some(velocity) = finite(vec3(args, 1, context)?) else {
                    return Ok(JsValue::undefined());
                };
                if let Some(handle) = handle {
                    with_scene(|scene| {
                        if let Some(body) = scene
                            .try_get_mut(handle)
                            .and_then(kscene::Node::rigid_body_mut)
                        {
                            body.set_linvel(velocity);
                        }
                    });
                }
                Ok(JsValue::undefined())
            }),
            js_string!("setLinvel"),
            4,
        )
        .function(
            NativeFunction::from_fn_ptr(|_, args, context| {
                let handle = node_arg(args, 0, context)?;
                if let Some(handle) = handle {
                    with_scene(|scene| {
                        if let Some(sound) =
                            scene.try_get_mut(handle).and_then(kscene::Node::sound_mut)
                        {
                            sound.restart();
                        }
                    });
                }
                Ok(JsValue::undefined())
            }),
            js_string!("playSound"),
            1,
        )
        // ── 动画 ──
        .function(
            NativeFunction::from_fn_ptr(|_, args, context| {
                // 先把字符串取干净再借场景：转字符串可能触发用户的
                // toString()，那会回调进 JS，此时若还持着借用就是重入。
                let handle = node_arg(args, 0, context)?;
                let name = match args.get(1) {
                    Some(value) => value.to_string(context)?.to_std_string_lossy(),
                    None => String::new(),
                };
                let Some(handle) = handle else {
                    return Ok(JsValue::from(false));
                };
                let played = with_scene(|scene| {
                    scene
                        .try_get_mut(handle)
                        .and_then(kscene::Node::animator_mut)
                        .and_then(|player| player.animator_mut().play_by_name(&name))
                        .is_some()
                })
                .unwrap_or(false);
                Ok(JsValue::from(played))
            }),
            js_string!("playAnimation"),
            2,
        )
        .function(
            NativeFunction::from_fn_ptr(|_, args, context| {
                let handle = node_arg(args, 0, context)?;
                let playing = args.get(1).map(JsValue::to_boolean).unwrap_or(false);
                if let Some(handle) = handle {
                    with_scene(|scene| {
                        if let Some(player) = scene
                            .try_get_mut(handle)
                            .and_then(kscene::Node::animator_mut)
                        {
                            player.animator_mut().set_playing(playing);
                        }
                    });
                }
                Ok(JsValue::undefined())
            }),
            js_string!("setAnimationPlaying"),
            2,
        )
        .function(
            NativeFunction::from_fn_ptr(|_, args, context| {
                let handle = node_arg(args, 0, context)?;
                let speed = number(args, 1, context)? as f32;
                if let Some(handle) = handle {
                    with_scene(|scene| {
                        if let Some(player) = scene
                            .try_get_mut(handle)
                            .and_then(kscene::Node::animator_mut)
                        {
                            player.animator_mut().set_speed(speed);
                        }
                    });
                }
                Ok(JsValue::undefined())
            }),
            js_string!("setAnimationSpeed"),
            2,
        )
        .function(
            NativeFunction::from_fn_ptr(|_, args, context| {
                let handle = node_arg(args, 0, context)?;
                let Some(handle) = handle else {
                    return Ok(JsValue::from(false));
                };
                let playing = with_scene(|scene| {
                    scene
                        .try_get(handle)
                        .and_then(kscene::Node::animator)
                        .is_some_and(|player| player.animator().is_playing())
                })
                .unwrap_or(false);
                Ok(JsValue::from(playing))
            }),
            js_string!("isAnimationPlaying"),
            1,
        )
        // ── 粒子 ──
        .function(
            NativeFunction::from_fn_ptr(|_, args, context| {
                let handle = node_arg(args, 0, context)?;
                let playing = args.get(1).map(JsValue::to_boolean).unwrap_or(false);
                if let Some(handle) = handle {
                    with_scene(|scene| {
                        if let Some(system) = scene
                            .try_get_mut(handle)
                            .and_then(kscene::Node::particles_mut)
                        {
                            system.playing = playing;
                        }
                    });
                }
                Ok(JsValue::undefined())
            }),
            js_string!("setParticlesPlaying"),
            2,
        )
        .function(
            NativeFunction::from_fn_ptr(|_, args, context| {
                let handle = node_arg(args, 0, context)?;
                let rate = number(args, 1, context)? as f32;
                if let Some(handle) = handle {
                    with_scene(|scene| {
                        if let Some(system) = scene
                            .try_get_mut(handle)
                            .and_then(kscene::Node::particles_mut)
                        {
                            system.emitter.rate = rate.max(0.0);
                        }
                    });
                }
                Ok(JsValue::undefined())
            }),
            js_string!("setEmissionRate"),
            2,
        )
        .function(
            NativeFunction::from_fn_ptr(|_, args, context| {
                let handle = node_arg(args, 0, context)?;
                let count = number(args, 1, context)?.max(0.0) as u32;
                if let Some(handle) = handle {
                    with_scene(|scene| {
                        // 喷发发生在世界空间，所以要先拿到世界变换。
                        // 借用是嵌套的：先算矩阵，再改粒子。
                        let world = scene.world_matrix(handle);
                        if let Some(system) = scene
                            .try_get_mut(handle)
                            .and_then(kscene::Node::particles_mut)
                        {
                            system.burst(count, world);
                        }
                    });
                }
                Ok(JsValue::undefined())
            }),
            js_string!("burstParticles"),
            2,
        )
        .function(
            NativeFunction::from_fn_ptr(|_, args, context| {
                let handle = node_arg(args, 0, context)?;
                let Some(handle) = handle else {
                    return Ok(JsValue::from(0.0));
                };
                let count = with_scene(|scene| {
                    scene
                        .try_get(handle)
                        .and_then(kscene::Node::particles)
                        .map_or(0.0, |system| system.alive() as f64)
                })
                .unwrap_or(0.0);
                Ok(JsValue::from(count))
            }),
            js_string!("particleCount"),
            1,
        )
        // ── 音频 ──
        .function(
            NativeFunction::from_fn_ptr(|_, args, context| {
                let handle = node_arg(args, 0, context)?;
                if let Some(handle) = handle {
                    with_scene(|scene| {
                        if let Some(sound) =
                            scene.try_get_mut(handle).and_then(kscene::Node::sound_mut)
                        {
                            sound.stop();
                        }
                    });
                }
                Ok(JsValue::undefined())
            }),
            js_string!("stopSound"),
            1,
        )
        .function(
            NativeFunction::from_fn_ptr(|_, args, context| {
                let handle = node_arg(args, 0, context)?;
                let gain = number(args, 1, context)? as f32;
                if let Some(handle) = handle {
                    with_scene(|scene| {
                        if let Some(sound) =
                            scene.try_get_mut(handle).and_then(kscene::Node::sound_mut)
                        {
                            sound.gain = gain.max(0.0);
                        }
                    });
                }
                Ok(JsValue::undefined())
            }),
            js_string!("setSoundGain"),
            2,
        )
        .function(
            NativeFunction::from_fn_ptr(|_, args, context| {
                let handle = node_arg(args, 0, context)?;
                let pitch = number(args, 1, context)? as f32;
                if let Some(handle) = handle {
                    with_scene(|scene| {
                        if let Some(sound) =
                            scene.try_get_mut(handle).and_then(kscene::Node::sound_mut)
                        {
                            // 音高为 0 会让播放头永远不前进：声音既不响
                            // 也不结束，那个声源就永远占着混音器的一路。
                            sound.pitch = pitch.max(0.01);
                        }
                    });
                }
                Ok(JsValue::undefined())
            }),
            js_string!("setSoundPitch"),
            2,
        )
        .function(
            NativeFunction::from_fn_ptr(|_, args, context| {
                let handle = node_arg(args, 0, context)?;
                let looping = args.get(1).map(JsValue::to_boolean).unwrap_or(false);
                if let Some(handle) = handle {
                    with_scene(|scene| {
                        if let Some(sound) =
                            scene.try_get_mut(handle).and_then(kscene::Node::sound_mut)
                        {
                            sound.looping = looping;
                        }
                    });
                }
                Ok(JsValue::undefined())
            }),
            js_string!("setSoundLooping"),
            2,
        )
        .function(
            NativeFunction::from_fn_ptr(|_, args, context| {
                let handle = node_arg(args, 0, context)?;
                if let Some(handle) = handle {
                    with_scene(|scene| scene.remove_node(handle));
                }
                Ok(JsValue::undefined())
            }),
            js_string!("queueFree"),
            1,
        )
        // ── 即时查询：旧架构做不到的那一类 ──
        .function(
            NativeFunction::from_fn_ptr(|_, args, context| {
                let origin = vec3(args, 0, context)?;
                let direction = vec3(args, 3, context)?;
                let max_distance = number(args, 6, context)? as f32;

                if !origin.is_finite() || !direction.is_finite() {
                    return Ok(JsValue::null());
                }

                let hit = with_scene(|scene| {
                    scene.cast_ray(&RayCastOptions::new(origin, direction, max_distance))
                })
                .flatten();

                let Some(hit) = hit else {
                    return Ok(JsValue::null());
                };
                let node = hit
                    .body_node
                    .or(hit.collider_node)
                    .map(id_of)
                    .unwrap_or(-1.0);

                Ok(ObjectInitializer::new(context)
                    .property(js_string!("node"), node, Attribute::all())
                    .property(js_string!("px"), hit.point.x as f64, Attribute::all())
                    .property(js_string!("py"), hit.point.y as f64, Attribute::all())
                    .property(js_string!("pz"), hit.point.z as f64, Attribute::all())
                    .property(js_string!("nx"), hit.normal.x as f64, Attribute::all())
                    .property(js_string!("ny"), hit.normal.y as f64, Attribute::all())
                    .property(js_string!("nz"), hit.normal.z as f64, Attribute::all())
                    .property(
                        js_string!("distance"),
                        hit.distance as f64,
                        Attribute::all(),
                    )
                    .build()
                    .into())
            }),
            js_string!("raycast"),
            7,
        )
        // ── 输入 ──
        //
        // 只认**动作与轴**，不认具体键位。键位绑定留在 Rust 侧的 `Bindings`：
        // 脚本里写死 `KeyCode::KeyW`，改键功能就永远做不了了，而 kinput 那套
        // 映射表正是为了避免这件事。
        .function(
            NativeFunction::from_fn_ptr(|_, args, context| {
                let name = text(args, 0, context)?;
                let pressed = with_input(|input| input.action_pressed(&name)).unwrap_or(false);
                Ok(JsValue::from(pressed))
            }),
            js_string!("actionPressed"),
            1,
        )
        .function(
            NativeFunction::from_fn_ptr(|_, args, context| {
                let name = text(args, 0, context)?;
                let pressed = with_input(|input| input.action_just_pressed(&name)).unwrap_or(false);
                Ok(JsValue::from(pressed))
            }),
            js_string!("actionJustPressed"),
            1,
        )
        .function(
            NativeFunction::from_fn_ptr(|_, args, context| {
                let name = text(args, 0, context)?;
                let released =
                    with_input(|input| input.action_just_released(&name)).unwrap_or(false);
                Ok(JsValue::from(released))
            }),
            js_string!("actionJustReleased"),
            1,
        )
        .function(
            NativeFunction::from_fn_ptr(|_, args, context| {
                let name = text(args, 0, context)?;
                let value = with_input(|input| input.axis(&name)).unwrap_or(0.0);
                Ok(JsValue::from(value as f64))
            }),
            js_string!("axis"),
            1,
        )
        .function(
            NativeFunction::from_fn_ptr(|_, args, context| {
                let name = text(args, 0, context)?;
                let Some(button) = mouse_button(&name) else {
                    return Ok(JsValue::from(false));
                };
                let pressed = with_input(|input| input.mouse_pressed(button)).unwrap_or(false);
                Ok(JsValue::from(pressed))
            }),
            js_string!("mousePressed"),
            1,
        )
        .function(
            NativeFunction::from_fn_ptr(|_, args, context| {
                let name = text(args, 0, context)?;
                let Some(button) = mouse_button(&name) else {
                    return Ok(JsValue::from(false));
                };
                let pressed = with_input(|input| input.mouse_just_pressed(button)).unwrap_or(false);
                Ok(JsValue::from(pressed))
            }),
            js_string!("mouseJustPressed"),
            1,
        )
        .function(
            NativeFunction::from_fn_ptr(|_, _, context| {
                // 光标还没进过窗口时没有位置，那时返回 null 而不是 (0,0)——
                // 原点是个合法坐标，混在一起脚本没法区分。
                let position = with_input(|input| input.cursor_position()).flatten();
                Ok(match position {
                    Some(position) => array2(position, context),
                    None => JsValue::null(),
                })
            }),
            js_string!("mousePosition"),
            0,
        )
        .function(
            NativeFunction::from_fn_ptr(|_, _, context| {
                let delta = with_input(|input| input.mouse_delta()).unwrap_or_default();
                Ok(array2(delta, context))
            }),
            js_string!("mouseDelta"),
            0,
        )
        .function(
            NativeFunction::from_fn_ptr(|_, _, context| {
                let delta = with_input(|input| input.scroll_delta()).unwrap_or_default();
                Ok(array2(delta, context))
            }),
            js_string!("scrollDelta"),
            0,
        )
        // ── 生成 ──
        //
        // 脚本按**名字**生成，原型由游戏侧用 `register_prototype` 登记。
        // 让脚本直接拼网格与材质的话，等于把整个渲染栈拖进 JS 层，
        // 而且换一套美术资源就要改脚本。
        .function(
            NativeFunction::from_fn_ptr(|_, args, context| {
                let name = text(args, 0, context)?;
                let position = vec3(args, 1, context)?;
                let Some(position) = finite(position) else {
                    return Ok(JsValue::from(-1.0));
                };

                let id = with_host(|host| {
                    if host.spawned >= MAX_SPAWNS {
                        return -1.0;
                    }
                    let Some(prototype) = host.prototypes.get(&name) else {
                        // 名字拼错是最常见的错误，而生成失败的表现是
                        // 「什么都没发生」——不记一条日志根本无从查起。
                        klog::error!("脚本想生成「{name}」，但没有登记过这个原型");
                        return -1.0;
                    };

                    let mut node = prototype();
                    node.transform.position = position;

                    let Some(scene) = host.scene.as_mut() else {
                        return -1.0;
                    };
                    let handle = scene.add_node(node);

                    host.spawned += 1;
                    host.registry.id_of(handle) as f64
                });

                Ok(JsValue::from(id.unwrap_or(-1.0)))
            }),
            js_string!("spawn"),
            4,
        )
        // ── 与 Rust 侧通信 ──
        .function(
            NativeFunction::from_fn_ptr(|_, args, context| {
                let text = match args.first() {
                    Some(value) => value.to_string(context)?.to_std_string_escaped(),
                    None => String::new(),
                };
                klog::info!("[脚本] {text}");
                Ok(JsValue::undefined())
            }),
            js_string!("log"),
            1,
        )
        .function(
            NativeFunction::from_fn_ptr(|_, args, context| {
                let name = match args.first() {
                    Some(value) => value.to_string(context)?.to_std_string_escaped(),
                    None => String::new(),
                };
                let value = number(args, 1, context)?;
                with_host(|host| {
                    if host.signals.len() < MAX_SIGNALS {
                        let source = host.current;
                        host.signals.push(crate::runtime::Signal {
                            name,
                            value,
                            source,
                        });
                    }
                });
                Ok(JsValue::undefined())
            }),
            js_string!("emit"),
            2,
        )
        .build();

    context
        .register_global_property(js_string!("__k"), bridge, Attribute::all())
        .expect("__k 是新建运行时里第一个全局属性，不该冲突");
}
