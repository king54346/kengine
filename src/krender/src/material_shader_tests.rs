//! 自定义材质着色器的测试。
//!
//! 这些测试全部围绕**拼装**：钩子和引擎的标准着色器拼起来之后能不能
//! 通过 naga 的校验。真正的管线创建需要 GPU，headless 起不来，
//! 所以那部分靠例子实机验证（见 `examples/kengine/backdrop_water.rs`）。

use super::*;

/// 拼一份带钩子的着色器并校验。
fn compile(hook: &str) -> Result<Shader, kshader::ShaderError> {
    Shader::from_wgsl(material_shader_source(hook))
}

#[test]
fn the_default_hook_compiles() {
    // 默认钩子是恒等函数，编译器会把它整个消掉。
    compile(DEFAULT_SURFACE_HOOK).expect("默认钩子应当通过校验");
}

#[test]
fn the_default_hook_is_what_the_standard_shader_uses() {
    // 标准着色器就是「默认钩子 + 其余部分」，两者必须是同一份源码——
    // 分成两条路的话，改了一处忘了另一处，自定义材质和标准材质
    // 会在光照上出现说不清的差异。
    assert_eq!(
        standard_shader_source(),
        material_shader_source(DEFAULT_SURFACE_HOOK)
    );
}

#[test]
fn a_hook_can_change_the_base_color() {
    compile(
        r#"
        fn material_surface(surface: Surface) -> Surface {
            var out = surface;
            out.base_color = vec4<f32>(1.0, 0.0, 0.0, 1.0);
            return out;
        }
        "#,
    )
    .expect("改颜色的钩子应当通过校验");
}

#[test]
fn a_hook_can_read_every_input_field() {
    // 逐个字段读一遍。少写一个字段、或者拼错名字，这条会报错——
    // 而在实际项目里那表现为「某个材质突然编译不过」，很难定位到
    // 是引擎改了 `Surface`。
    compile(
        r#"
        fn material_surface(surface: Surface) -> Surface {
            var out = surface;
            let sum = surface.world_position
                + surface.geometric_normal
                + vec3<f32>(surface.uv, 0.0)
                + surface.view_direction
                + vec3<f32>(surface.screen_uv, surface.time)
                + surface.normal
                + surface.emissive
                + vec3<f32>(surface.metallic, surface.roughness, surface.occlusion)
                + surface.base_color.rgb;
            out.base_color = vec4<f32>(sum, 1.0);
            return out;
        }
        "#,
    )
    .expect("读全部字段的钩子应当通过校验");
}

#[test]
fn a_hook_can_write_every_output_field() {
    compile(
        r#"
        fn material_surface(surface: Surface) -> Surface {
            var out = surface;
            out.base_color = vec4<f32>(0.5);
            out.normal = vec3<f32>(0.0, 1.0, 0.0);
            out.metallic = 1.0;
            out.roughness = 0.3;
            out.occlusion = 0.8;
            out.emissive = vec3<f32>(1.0, 0.5, 0.0);
            return out;
        }
        "#,
    )
    .expect("写全部输出字段的钩子应当通过校验");
}

#[test]
fn a_hook_can_use_time_for_animation() {
    // 时间是自定义材质最常要的东西。`frame_params.x` 没接上的话
    // 这里编译得过、但画面是静止的——所以还有下面那条布局测试。
    compile(
        r#"
        fn material_surface(surface: Surface) -> Surface {
            var out = surface;
            out.base_color = vec4<f32>(vec3<f32>(sin(surface.time)), 1.0);
            return out;
        }
        "#,
    )
    .expect("用时间的钩子应当通过校验");
}

#[test]
fn a_hook_can_use_screen_uv() {
    compile(
        r#"
        fn material_surface(surface: Surface) -> Surface {
            var out = surface;
            out.base_color = vec4<f32>(surface.screen_uv, 0.0, 1.0);
            return out;
        }
        "#,
    )
    .expect("用屏幕 UV 的钩子应当通过校验");
}

#[test]
fn a_hook_can_call_engine_helpers() {
    // 钩子拼在 klight / kpbr 之后，所以引擎的工具函数都能用。
    // 这是钩子式设计相对「整份替换」的主要好处之一。
    compile(
        r#"
        fn material_surface(surface: Surface) -> Surface {
            var out = surface;
            out.base_color = vec4<f32>(
                ibl_sky(globals.environment, surface.geometric_normal),
                1.0,
            );
            return out;
        }
        "#,
    )
    .expect("调用引擎函数的钩子应当通过校验");
}

#[test]
fn a_broken_hook_is_rejected_rather_than_reaching_wgpu() {
    // 这一条是安全网：wgpu 的着色器校验失败会**直接 panic 掉整个进程**，
    // 而用户写错着色器是常态。渲染器必须先自己校验一遍，
    // 失败就退回标准管线。
    assert!(compile("fn material_surface() { 这不是 WGSL }").is_err());
    assert!(compile("fn material_surface(s: Surface) -> Surface { return 1; }").is_err());
    // 签名不对（少了返回值）也要被拦下。
    assert!(compile("fn material_surface(s: Surface) { }").is_err());
}

#[test]
fn a_hook_that_forgets_to_return_the_others_still_compiles() {
    // 只填几个字段、其余留空的钩子**能编译**——这是 `var out: Surface;`
    // 的合法用法。画面上表现为颜色和法线全黑。
    //
    // 记在这里是为了说明：输入输出共用一个结构体的设计，
    // 让「照搬其余字段」变成 `var out = surface;` 一句话，
    // 但用户仍然可以选择不这么做。
    compile(
        r#"
        fn material_surface(surface: Surface) -> Surface {
            var out: Surface;
            out.base_color = vec4<f32>(1.0);
            return out;
        }
        "#,
    )
    .expect("只填部分字段也该编译得过");
}

#[test]
fn the_frame_params_slot_exists_in_globals() {
    // 钩子拿时间和视口尺寸走的是 `globals.frame_params`。
    // 这个字段被删掉或改名的话，上面那些测试仍然会过（它们读的是
    // `surface.time`），但画面会静止——所以单独盯一下源码。
    let source = standard_shader_source();
    assert!(
        source.contains("frame_params"),
        "Globals 里没有 frame_params"
    );
    assert!(
        source.contains("surface.time = globals.frame_params.x"),
        "时间没有从 frame_params 接到 Surface 上"
    );
    assert!(
        source.contains("globals.frame_params.zw"),
        "屏幕 UV 没有用上视口尺寸"
    );
}

#[test]
fn the_hook_is_spliced_before_the_shader_body() {
    // 顺序错了的话 `material_surface` 在被调用处还没定义。
    // WGSL 允许前向引用，所以这不会立刻报错，但 `Surface` 的定义
    // 必须在钩子之前——那个是会报错的。
    // 钩子里放一个独特的标记来定位。直接找 `fn material_surface` 会
    // 命中 `surface.wgsl` 文档注释里的那段示例代码——那在 `struct Surface`
    // 之前，测出来的顺序是反的。
    let source = material_shader_source(
        "fn material_surface(s: Surface) -> Surface { let marker_9f3a = 1; return s; }",
    );
    let surface_definition = source.find("struct Surface {").expect("没有 Surface 定义");
    let hook = source.find("marker_9f3a").expect("没有钩子");
    let call = source
        .find("surface = material_surface(surface)")
        .expect("没有调用点");

    assert!(surface_definition < hook, "Surface 的定义排在钩子后面了");
    assert!(hook < call, "钩子排在调用点后面了");
}
