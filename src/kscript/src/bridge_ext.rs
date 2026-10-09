//! 桥的第二部分：变换（旋转、四元数、全局坐标）、节点树、异步支持、日志级别。
//!
//! 和 [`crate::bridge`] 同一条纪律：**先把参数从 VM 里取干净，再借场景**。
//! 这里的函数挂到同一个 `__k` 对象上，`prelude.js` 不需要知道它们来自哪个文件。
//!
//! # 旋转的约定
//!
//! 欧拉角和 Godot 一样：弧度，**YXZ** 顺序（先绕 Z、再绕 X、最后绕 Y）。
//! 这是第三人称 / 第一人称控制器最顺手的顺序——偏航（Y）在最外层，
//! 改俯仰（X）不会把偏航带歪。

use crate::host::{handle_of, id_of, with_host, with_scene};
use boa_engine::{
    Context, JsResult, JsValue, NativeFunction, js_string,
    object::{FunctionObjectBuilder, builtins::JsArray},
};
use kmath::{EulerRot, Mat4, Quat, Vec3};
use kscene::Node;

fn number(args: &[JsValue], index: usize, context: &mut Context) -> JsResult<f64> {
    match args.get(index) {
        Some(value) => value.to_number(context),
        None => Ok(0.0),
    }
}

fn vec3(args: &[JsValue], start: usize, context: &mut Context) -> JsResult<Vec3> {
    Ok(Vec3::new(
        number(args, start, context)? as f32,
        number(args, start + 1, context)? as f32,
        number(args, start + 2, context)? as f32,
    ))
}

fn text(args: &[JsValue], index: usize, context: &mut Context) -> JsResult<String> {
    Ok(match args.get(index) {
        Some(value) => value.to_string(context)?.to_std_string_escaped(),
        None => String::new(),
    })
}

fn floats(values: &[f32], context: &mut Context) -> JsValue {
    let array = JsArray::new(context);
    for v in values {
        let _ = array.push(JsValue::from(*v as f64), context);
    }
    array.into()
}

fn ids(values: &[f64], context: &mut Context) -> JsValue {
    let array = JsArray::new(context);
    for v in values {
        let _ = array.push(JsValue::from(*v), context);
    }
    array.into()
}

/// 节点的父节点在世界里的矩阵（根节点下的节点是单位阵）。
fn parent_world(scene: &kscene::Scene, node: &Node) -> Mat4 {
    if node.parent().is_none() {
        Mat4::IDENTITY
    } else {
        scene.world_matrix(node.parent())
    }
}

type Native = fn(&JsValue, &[JsValue], &mut Context) -> JsResult<JsValue>;

/// 往已有的 `__k` 上加函数。
pub(crate) fn register(context: &mut Context) {
    let functions: &[(&str, usize, Native)] = &[
        // ── 补间 ──
        // tween(节点, 属性, x, y, z, 时长, 缓动名) → 补间编号；节点无效、属性或缓动名不认识时 0。
        // 旋转给的是欧拉角（弧度，和 `rotation` 同一套 YXZ 顺序）。
        ("tween", 7, |_, args, context| {
            let Some(handle) = handle_of(number(args, 0, context)?) else {
                return Ok(JsValue::from(0));
            };
            let property = text(args, 1, context)?;
            let target = vec3(args, 2, context)?;
            let duration = number(args, 5, context)? as f32;
            let ease_name = text(args, 6, context)?;
            let Some(ease) = kanim::Ease::from_name(if ease_name.is_empty() {
                "linear"
            } else {
                &ease_name
            }) else {
                klog::warn!("tween：不认识的缓动「{ease_name}」");
                return Ok(JsValue::from(0));
            };
            if !target.is_finite() || !duration.is_finite() {
                return Ok(JsValue::from(0));
            }
            let id = with_scene(|scene| match property.as_str() {
                "position" => scene.tween_position(handle, target, duration, ease).0,
                "scale" => scene.tween_scale(handle, target, duration, ease).0,
                "rotation" => {
                    let to = Quat::from_euler(EulerRot::YXZ, target.y, target.x, target.z);
                    scene.tween_rotation(handle, to, duration, ease).0
                }
                other => {
                    klog::warn!(
                        "tween：不支持的属性「{other}」（只有 position / rotation / scale）"
                    );
                    0
                }
            })
            .unwrap_or(0);
            Ok(JsValue::from(id as f64))
        }),
        // `KENGINE_SEED` 设了就返回它（让 `Math.random` 可复现），没设返回 null。
        ("randomSeed", 0, |_, _, _| {
            Ok(std::env::var("KENGINE_SEED")
                .ok()
                .and_then(|text| text.trim().parse::<u32>().ok())
                .map_or(JsValue::null(), JsValue::from))
        }),
        ("tweenActive", 1, |_, args, context| {
            let id = kscene::TweenId(number(args, 0, context)? as u64);
            Ok(JsValue::from(
                with_scene(|scene| scene.tween_active(id)).unwrap_or(false),
            ))
        }),
        ("tweenFinished", 1, |_, args, context| {
            let id = kscene::TweenId(number(args, 0, context)? as u64);
            Ok(JsValue::from(
                with_scene(|scene| scene.tween_finished(id)).unwrap_or(false),
            ))
        }),
        ("cancelTween", 1, |_, args, context| {
            let id = kscene::TweenId(number(args, 0, context)? as u64);
            with_scene(|scene| scene.cancel_tween(id));
            Ok(JsValue::undefined())
        }),
        // ── 旋转 ──
        ("getRotation", 1, |_, args, context| {
            let handle = handle_of(number(args, 0, context)?);
            let rotation = handle
                .and_then(|h| {
                    with_scene(|scene| scene.try_get(h).map(|n| n.transform.rotation)).flatten()
                })
                .unwrap_or(Quat::IDENTITY);
            let (y, x, z) = rotation.to_euler(EulerRot::YXZ);
            Ok(floats(&[x, y, z], context))
        }),
        ("setRotation", 4, |_, args, context| {
            let handle = handle_of(number(args, 0, context)?);
            let euler = vec3(args, 1, context)?;
            if !euler.is_finite() {
                return Ok(JsValue::undefined());
            }
            if let Some(h) = handle {
                with_scene(|scene| {
                    if let Some(node) = scene.try_get_mut(h) {
                        node.transform.rotation =
                            Quat::from_euler(EulerRot::YXZ, euler.y, euler.x, euler.z);
                    }
                });
            }
            Ok(JsValue::undefined())
        }),
        ("getQuat", 1, |_, args, context| {
            let handle = handle_of(number(args, 0, context)?);
            let q = handle
                .and_then(|h| {
                    with_scene(|scene| scene.try_get(h).map(|n| n.transform.rotation)).flatten()
                })
                .unwrap_or(Quat::IDENTITY);
            Ok(floats(&[q.x, q.y, q.z, q.w], context))
        }),
        ("setQuat", 5, |_, args, context| {
            let handle = handle_of(number(args, 0, context)?);
            let q = Quat::from_xyzw(
                number(args, 1, context)? as f32,
                number(args, 2, context)? as f32,
                number(args, 3, context)? as f32,
                number(args, 4, context)? as f32,
            );
            // 零四元数或 NaN 归一化不了，写进去整棵子树都会消失。
            if !q.is_finite() || q.length_squared() < 1e-12 {
                return Ok(JsValue::undefined());
            }
            if let Some(h) = handle {
                with_scene(|scene| {
                    if let Some(node) = scene.try_get_mut(h) {
                        node.transform.rotation = q.normalize();
                    }
                });
            }
            Ok(JsValue::undefined())
        }),
        // rotateAxis(id, ax, ay, az, angle, local)：local 为真绕自身的轴，否则绕父空间的轴。
        ("rotateAxis", 6, |_, args, context| {
            let handle = handle_of(number(args, 0, context)?);
            let axis = vec3(args, 1, context)?;
            let angle = number(args, 4, context)? as f32;
            let local = args.get(5).map(JsValue::to_boolean).unwrap_or(true);
            if !axis.is_finite() || !angle.is_finite() || axis.length_squared() < 1e-12 {
                return Ok(JsValue::undefined());
            }
            let delta = Quat::from_axis_angle(axis.normalize(), angle);
            if let Some(h) = handle {
                with_scene(|scene| {
                    if let Some(node) = scene.try_get_mut(h) {
                        let r = node.transform.rotation;
                        node.transform.rotation =
                            if local { r * delta } else { delta * r }.normalize();
                    }
                });
            }
            Ok(JsValue::undefined())
        }),
        // 世界空间的三根轴：[右 xyz, 上 xyz, 后 xyz]（列向量，已归一化）。
        ("getAxes", 1, |_, args, context| {
            let handle = handle_of(number(args, 0, context)?);
            let matrix = handle
                .and_then(|h| with_scene(|scene| scene.world_matrix(h)))
                .unwrap_or(Mat4::IDENTITY);
            let axis = |v: Vec3| v.normalize_or_zero();
            let (x, y, z) = (
                axis(matrix.x_axis.truncate()),
                axis(matrix.y_axis.truncate()),
                axis(matrix.z_axis.truncate()),
            );
            Ok(floats(
                &[x.x, x.y, x.z, y.x, y.y, y.z, z.x, z.y, z.z],
                context,
            ))
        }),
        // ── 全局坐标 ──
        ("setGlobalPosition", 4, |_, args, context| {
            let handle = handle_of(number(args, 0, context)?);
            let target = vec3(args, 1, context)?;
            if !target.is_finite() {
                return Ok(JsValue::undefined());
            }
            if let Some(h) = handle {
                with_scene(|scene| {
                    let Some(node) = scene.try_get(h) else { return };
                    let local = parent_world(scene, node).inverse().transform_point3(target);
                    if let Some(node) = scene.try_get_mut(h) {
                        node.transform.position = local;
                    }
                });
            }
            Ok(JsValue::undefined())
        }),
        // toGlobal(id, x, y, z, isPoint)：节点局部 → 世界。isPoint 为假时只转方向（不平移）。
        ("toGlobal", 5, |_, args, context| {
            let handle = handle_of(number(args, 0, context)?);
            let v = vec3(args, 1, context)?;
            let point = args.get(4).map(JsValue::to_boolean).unwrap_or(true);
            let matrix = handle
                .and_then(|h| with_scene(|scene| scene.world_matrix(h)))
                .unwrap_or(Mat4::IDENTITY);
            let out = if point {
                matrix.transform_point3(v)
            } else {
                matrix.transform_vector3(v)
            };
            Ok(floats(&[out.x, out.y, out.z], context))
        }),
        ("toLocal", 5, |_, args, context| {
            let handle = handle_of(number(args, 0, context)?);
            let v = vec3(args, 1, context)?;
            let point = args.get(4).map(JsValue::to_boolean).unwrap_or(true);
            let matrix = handle
                .and_then(|h| with_scene(|scene| scene.world_matrix(h)))
                .unwrap_or(Mat4::IDENTITY)
                .inverse();
            let out = if point {
                matrix.transform_point3(v)
            } else {
                matrix.transform_vector3(v)
            };
            Ok(floats(&[out.x, out.y, out.z], context))
        }),
        // ── 节点树 ──
        ("getParent", 1, |_, args, context| {
            let handle = handle_of(number(args, 0, context)?);
            let parent = handle.and_then(|h| {
                with_scene(|scene| {
                    let parent = scene.try_get(h)?.parent();
                    // 场景根对脚本来说不是一个「节点」：它没有名字，也不该被挪动。
                    (parent.is_some() && parent != scene.root()).then_some(parent)
                })
                .flatten()
            });
            Ok(JsValue::from(parent.map_or(-1.0, id_of)))
        }),
        ("getChildren", 1, |_, args, context| {
            let handle = handle_of(number(args, 0, context)?);
            let children: Vec<kcore::pool::Handle<Node>> = handle
                .and_then(|h| {
                    with_scene(|scene| scene.try_get(h).map(|n| n.children().to_vec())).flatten()
                })
                .unwrap_or_default();
            let list: Vec<f64> = children.into_iter().map(id_of).collect();
            Ok(ids(&list, context))
        }),
        // findChild(id, name, recursive)：按名字找子节点，id < 0 时从场景根找。
        ("findChild", 3, |_, args, context| {
            let raw = number(args, 0, context)?;
            let name = text(args, 1, context)?;
            let recursive = args.get(2).map(JsValue::to_boolean).unwrap_or(false);
            // 下标先换成句柄（要借宿主），再借场景。
            let given = if raw < 0.0 {
                None
            } else {
                Some(handle_of(raw))
            };
            if given == Some(None) {
                return Ok(JsValue::from(-1.0));
            }
            let found = with_scene(|scene| {
                let start = given.flatten().unwrap_or(scene.root());
                if recursive {
                    scene
                        .descendants(start)
                        .into_iter()
                        .find(|&h| h != start && scene.try_get(h).is_some_and(|n| n.name == name))
                } else {
                    scene
                        .try_get(start)?
                        .children()
                        .iter()
                        .copied()
                        .find(|&h| scene.try_get(h).is_some_and(|n| n.name == name))
                }
            })
            .flatten();
            Ok(JsValue::from(found.map_or(-1.0, id_of)))
        }),
        // reparent(child, parent, keepGlobal)：parent < 0 挂回场景根。
        ("reparent", 3, |_, args, context| {
            let child = handle_of(number(args, 0, context)?);
            let raw_parent = number(args, 1, context)?;
            let parent = handle_of(raw_parent);
            let keep = args.get(2).map(JsValue::to_boolean).unwrap_or(true);
            let Some(child) = child else {
                return Ok(JsValue::from(false));
            };
            let ok = with_scene(|scene| {
                let parent = if raw_parent < 0.0 {
                    scene.root()
                } else {
                    parent?
                };
                let world = scene.world_matrix(child);
                scene.link_nodes(child, parent);
                let linked = scene.try_get(child)?.parent() == parent;
                if linked && keep {
                    let parent_world = if parent == scene.root() {
                        Mat4::IDENTITY
                    } else {
                        scene.world_matrix(parent)
                    };
                    let local = parent_world.inverse() * world;
                    let (scale, rotation, position) = local.to_scale_rotation_translation();
                    if let Some(node) = scene.try_get_mut(child)
                        && position.is_finite()
                        && rotation.is_finite()
                    {
                        node.transform.position = position;
                        node.transform.rotation = rotation;
                        node.transform.scale = scale;
                    }
                }
                Some(linked)
            })
            .flatten()
            .unwrap_or(false);
            Ok(JsValue::from(ok))
        }),
        // ── 异步支持 ──
        //
        // 计时器到点、信号回调时要把「当前节点」换成订阅者自己的，返回原来的。
        ("setSelf", 1, |_, args, context| {
            let handle = handle_of(number(args, 0, context)?).unwrap_or(kcore::pool::Handle::NONE);
            let previous = with_host(|host| std::mem::replace(&mut host.current, handle))
                .unwrap_or(kcore::pool::Handle::NONE);
            Ok(JsValue::from(if previous.is_none() {
                -1.0
            } else {
                id_of(previous)
            }))
        }),
        // asyncError(ownerId, message)：异步回调（计时器、await 之后、信号）里的异常，
        // 记在宿主上，运行时 tick 末尾按它停掉对应的脚本。
        ("asyncError", 2, |_, args, context| {
            let owner = handle_of(number(args, 0, context)?);
            let message = text(args, 1, context)?;
            with_host(|host| {
                if host.async_errors.len() < 256 {
                    host.async_errors
                        .push((owner.unwrap_or(kcore::pool::Handle::NONE), message));
                }
            });
            Ok(JsValue::undefined())
        }),
        // ── 日志 ──
        ("logLevel", 2, |_, args, context| {
            let level = number(args, 0, context)? as i32;
            let message = text(args, 1, context)?;
            match level {
                0 => klog::debug!("[脚本] {message}"),
                2 => klog::warn!("[脚本] {message}"),
                3 => klog::error!("[脚本] {message}"),
                _ => klog::info!("[脚本] {message}"),
            }
            with_host(|host| {
                if host.log.len() < 1024 {
                    host.log.push((level, message));
                }
            });
            Ok(JsValue::undefined())
        }),
    ];

    let bridge = context
        .global_object()
        .get(js_string!("__k"), context)
        .ok()
        .and_then(|v| v.as_object())
        .expect("bridge::register 要先跑");
    let realm = context.realm().clone();
    for (name, length, function) in functions {
        let object = FunctionObjectBuilder::new(&realm, NativeFunction::from_fn_ptr(*function))
            .name(js_string!(*name))
            .length(*length)
            .build();
        bridge
            .set(js_string!(*name), object, false, context)
            .expect("__k 是普通对象，加属性不会失败");
    }
}
