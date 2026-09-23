//! BVH（Biovision Hierarchy）：动作捕捉数据。只有骨架和逐帧通道，没有网格。
//!
//! 产物是一个没有几何的 [`Model`]：每个关节一个节点（`End Site` 也建成
//! 叶子节点，画骨头要用到它的位置），外加一段动画剪辑。于是实例化之后
//! 引擎现成的 [`AnimationPlayer`](kscene) 就会驱动它，不需要为 BVH 单开
//! 一套播放逻辑。
//!
//! # 旋转顺序
//!
//! BVH 的每个关节自己声明通道顺序，例如 `Zrotation Xrotation Yrotation`。
//! 旋转按**声明顺序**依次右乘（`R = Rz · Rx · Ry`），这是 BVH 的约定，
//! 也是 three.js `BVHLoader` 的做法。写死成 XYZ 的话，大部分文件的
//! 手臂会扭成麻花。
//!
//! # 位置通道
//!
//! 带位置通道的关节（通常只有根），通道值**替换**偏移量里对应的分量，
//! 而不是加在它上面——这是 BVH 事实上的约定（Blender、MotionBuilder
//! 都这么读）。没出现的轴保持 `OFFSET` 里的值。

use crate::{bad, limits, loader};
use kanim::{AnimationClip, Channel, Curve, Interpolation, Track};
use kasset::{LoadError, ResourceIo};
use kgltf::{MODEL_TYPE_UUID, Model, ModelNode, NodeTransform};
use kmath::{Quat, Vec3};
use std::{path::PathBuf, sync::Arc};

loader! {
    /// 读 `.bvh`。
    BvhLoader -> Model : ["bvh"] = MODEL_TYPE_UUID, parse
}

#[derive(Clone, Copy, PartialEq)]
enum Axis {
    PositionX,
    PositionY,
    PositionZ,
    RotationX,
    RotationY,
    RotationZ,
}

struct Joint {
    name: String,
    offset: Vec3,
    channels: Vec<Axis>,
    children: Vec<usize>,
}

/// 解析 BVH。
pub async fn parse(bytes: Vec<u8>, path: PathBuf, _io: Arc<dyn ResourceIo>) -> Result<Model, LoadError> {
    let name = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "BVH".into());
    parse_text(&String::from_utf8_lossy(&bytes), &name)
}

/// 从文本解析。
pub fn parse_text(text: &str, clip_name: &str) -> Result<Model, LoadError> {
    let mut tokens = text.split_ascii_whitespace().peekable();
    let mut joints: Vec<Joint> = Vec::new();
    let mut stack: Vec<usize> = Vec::new();
    let mut roots = Vec::new();

    let expect = |token: Option<&str>, what: &str| -> Result<(), LoadError> {
        if token.is_some_and(|t| t.eq_ignore_ascii_case(what)) {
            Ok(())
        } else {
            Err(bad(format!("BVH：这里应当是 {what}，实际是 {token:?}")))
        }
    };
    let number = |token: Option<&str>| -> Result<f32, LoadError> {
        token
            .and_then(|t| t.parse().ok())
            .ok_or_else(|| bad(format!("BVH：这里应当是数，实际是 {token:?}")))
    };

    expect(tokens.next(), "HIERARCHY")?;
    loop {
        let Some(token) = tokens.next() else {
            return Err(bad("BVH：缺少 MOTION 段"));
        };
        match token.to_ascii_uppercase().as_str() {
            "ROOT" | "JOINT" | "END" => {
                let end = token.eq_ignore_ascii_case("END");
                let name = match tokens.next() {
                    // `End Site`：名字取父关节名加后缀，三维软件的惯例。
                    Some(site) if end => {
                        let _ = site;
                        let parent = stack.last().map_or("Root", |&p| joints[p].name.as_str());
                        format!("{parent}_End")
                    }
                    Some(name) => name.to_string(),
                    None => return Err(bad("BVH：关节没有名字")),
                };
                expect(tokens.next(), "{")?;
                expect(tokens.next(), "OFFSET")?;
                let offset = Vec3::new(number(tokens.next())?, number(tokens.next())?, number(tokens.next())?);
                let mut channels = Vec::new();
                if !end {
                    expect(tokens.next(), "CHANNELS")?;
                    let count = number(tokens.next())? as usize;
                    if count > 6 {
                        return Err(bad("BVH：一个关节最多 6 个通道"));
                    }
                    for _ in 0..count {
                        channels.push(match tokens.next().unwrap_or("").to_ascii_lowercase().as_str() {
                            "xposition" => Axis::PositionX,
                            "yposition" => Axis::PositionY,
                            "zposition" => Axis::PositionZ,
                            "xrotation" => Axis::RotationX,
                            "yrotation" => Axis::RotationY,
                            "zrotation" => Axis::RotationZ,
                            other => return Err(bad(format!("BVH：未知的通道 {other}"))),
                        });
                    }
                }
                let index = joints.len();
                if index > limits::NODES {
                    return Err(bad("BVH：关节数超过上限"));
                }
                joints.push(Joint {
                    name,
                    offset,
                    channels,
                    children: Vec::new(),
                });
                match stack.last() {
                    Some(&parent) => joints[parent].children.push(index),
                    None => roots.push(index),
                }
                stack.push(index);
            }
            "}" => {
                stack.pop();
            }
            "MOTION" => break,
            other => return Err(bad(format!("BVH：层级段里出现了意外的 {other}"))),
        }
    }

    expect(tokens.next(), "Frames:")?;
    let frames = number(tokens.next())? as usize;
    expect(tokens.next(), "Frame")?;
    expect(tokens.next(), "Time:")?;
    let frame_time = number(tokens.next())?.max(1e-4);
    let channel_count: usize = joints.iter().map(|j| j.channels.len()).sum();
    if frames.saturating_mul(channel_count) > limits::VERTICES * 4 {
        return Err(bad("BVH：帧数据超过上限"));
    }

    let mut positions: Vec<Vec<Vec3>> = joints.iter().map(|_| Vec::with_capacity(frames)).collect();
    let mut rotations: Vec<Vec<Quat>> = joints.iter().map(|_| Vec::with_capacity(frames)).collect();
    for _ in 0..frames {
        for (index, joint) in joints.iter().enumerate() {
            let mut position = joint.offset;
            let mut rotation = Quat::IDENTITY;
            for axis in &joint.channels {
                let value = number(tokens.next())?;
                match axis {
                    Axis::PositionX => position.x = value,
                    Axis::PositionY => position.y = value,
                    Axis::PositionZ => position.z = value,
                    Axis::RotationX => rotation *= Quat::from_rotation_x(value.to_radians()),
                    Axis::RotationY => rotation *= Quat::from_rotation_y(value.to_radians()),
                    Axis::RotationZ => rotation *= Quat::from_rotation_z(value.to_radians()),
                }
            }
            positions[index].push(position);
            rotations[index].push(rotation.normalize());
        }
    }

    let times: Vec<f32> = (0..frames).map(|f| f as f32 * frame_time).collect();
    let mut tracks = Vec::new();
    for (index, joint) in joints.iter().enumerate() {
        let has_position = joint.channels.iter().any(|a| matches!(a, Axis::PositionX | Axis::PositionY | Axis::PositionZ));
        let has_rotation = joint.channels.iter().any(|a| matches!(a, Axis::RotationX | Axis::RotationY | Axis::RotationZ));
        if frames == 0 {
            continue;
        }
        if has_position
            && let Some(curve) = Curve::new(times.clone(), std::mem::take(&mut positions[index]), Interpolation::Linear)
        {
            tracks.push(Track { target: index, channel: Channel::Position(curve) });
        }
        if has_rotation
            && let Some(curve) = Curve::new(times.clone(), std::mem::take(&mut rotations[index]), Interpolation::Linear)
        {
            tracks.push(Track { target: index, channel: Channel::Rotation(curve) });
        }
    }

    let nodes = joints
        .iter()
        .map(|joint| ModelNode {
            name: joint.name.clone(),
            transform: NodeTransform {
                position: joint.offset,
                ..Default::default()
            },
            children: joint.children.clone(),
            parts: Vec::new(),
            skin: None,
        })
        .collect();
    let mut model = Model::new(Vec::new(), Vec::new(), nodes, roots);
    if !tracks.is_empty() {
        model = model.with_animations(vec![AnimationClip::new(clip_name, tracks)]);
    }
    Ok(model)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "HIERARCHY
ROOT Hips
{
  OFFSET 0 0 0
  CHANNELS 6 Xposition Yposition Zposition Zrotation Xrotation Yrotation
  JOINT Spine
  {
    OFFSET 0 10 0
    CHANNELS 3 Zrotation Xrotation Yrotation
    End Site
    {
      OFFSET 0 5 0
    }
  }
}
MOTION
Frames: 2
Frame Time: 0.5
1 2 3 0 0 0 0 0 0
4 5 6 90 0 0 0 0 90
";

    #[test]
    fn hierarchy_and_motion() {
        let model = parse_text(SAMPLE, "take").unwrap();
        assert_eq!(model.nodes().len(), 3);
        assert_eq!(model.nodes()[2].name, "Spine_End");
        assert_eq!(model.nodes()[1].transform.position, Vec3::new(0.0, 10.0, 0.0));
        let clip = &model.animations()[0];
        assert!((clip.duration() - 0.5).abs() < 1e-6);
        let pose = clip.sample(0.5);
        assert_eq!(pose.entry(0).unwrap().position, Some(Vec3::new(4.0, 5.0, 6.0)));
        // 根：Z 转 90°。脊柱：Y 转 90°。
        let spine = pose.entry(1).unwrap().rotation.unwrap();
        assert!(spine.angle_between(Quat::from_rotation_y(std::f32::consts::FRAC_PI_2)) < 1e-4);
    }

    #[test]
    fn rotation_order_follows_the_declared_channels() {
        // 同样的数值，ZXY 与 XYZ 的顺序得到不同的旋转。
        let zxy = Quat::from_rotation_z(0.5) * Quat::from_rotation_x(0.3) * Quat::from_rotation_y(0.2);
        let text = SAMPLE.replace("90 0 0 0 0 90", &format!("{} {} {} 0 0 0", 0.5f32.to_degrees(), 0.3f32.to_degrees(), 0.2f32.to_degrees()));
        let model = parse_text(&text, "t").unwrap();
        let root = model.animations()[0].sample(0.5).entry(0).unwrap().rotation.unwrap();
        assert!(root.angle_between(zxy) < 1e-4);
    }
}
