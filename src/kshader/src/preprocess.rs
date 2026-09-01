//! 着色器的条件编译。
//!
//! WGSL 本身没有预处理器——`#ifdef` 这类东西是各家引擎自己加的一层。
//! 这里实现的是最小的那一份：`#ifdef` / `#ifndef` / `#else` / `#endif`，
//! 可以嵌套，别的一概不认。
//!
//! # 为什么不做 `#define` 和宏
//!
//! `#define` 写在源码里，而**开关必须从 Rust 侧给**：管线是按
//! [`Shader::id`](crate::Shader::id) 缓存的，源码里自己定义的开关对缓存
//! 是不可见的，同一份源码会得到同一个 id 却编出不同的代码。
//!
//! 带参数的宏则是另一回事：那等于在 WGSL 之上再造一门语言，
//! 报错会指向展开后的代码，而人看的是展开前的。WGSL 已经有 `const`
//! 和普通函数，宏能干的事它基本都能干。
//!
//! # 行号必须对得上
//!
//! 被删掉的行**换成空行**而不是真的删掉。naga 报错时给的行号是拼接后的，
//! 本来就不好对；再让预处理把行号挪一遍，写着色器的人就只能靠猜了。

use crate::ShaderError;

/// 按一组开关处理条件编译指令。
///
/// `defs` 里的名字算「已定义」，其余都算未定义。返回的源码行数与输入
/// **完全一致**——删掉的行换成空行，好让报错的行号还能用。
///
/// ```
/// let source = "#ifdef A\nyes\n#else\nno\n#endif";
///
/// // 五行进、五行出：指令行和没选中的行都换成了空行。
/// assert_eq!(kshader::preprocess(source, &["A"]).unwrap(), "\nyes\n\n\n\n");
/// assert_eq!(kshader::preprocess(source, &[]).unwrap(), "\n\n\nno\n\n");
/// ```
///
/// # Errors
///
/// 指令不配对（少 `#endif`、多 `#endif`、`#else` 没有对应的 `#ifdef`）
/// 或者出现不认识的 `#` 指令时返回 [`ShaderError::Preprocess`]。
///
/// 不认识的指令**不放行**：静默留在源码里的话，naga 会报一句
/// 「意外的字符 `#`」，而真正的问题是拼错了指令名。
pub fn preprocess(source: &str, defs: &[&str]) -> Result<String, ShaderError> {
    /// 一层 `#ifdef` 的状态。
    struct Frame {
        /// 这一层自己的条件成不成立。
        taken: bool,
        /// 已经进过 `#else` 了——再来一个就是写错了。
        in_else: bool,
        /// 出错时指回源码。
        line: usize,
    }

    let mut out = String::with_capacity(source.len());
    let mut stack: Vec<Frame> = Vec::new();

    for (index, line) in source.lines().enumerate() {
        let number = index + 1;
        let trimmed = line.trim_start();

        // 「外层全都成立」才轮得到这一行。任何一层没成立，
        // 里面的分支连指令带代码一起当不存在——但 `#endif` 仍要配对，
        // 所以指令还是要解析。
        let active = stack.iter().all(|frame| frame.taken);

        if let Some(rest) = trimmed.strip_prefix('#') {
            let mut parts = rest.split_whitespace();
            let directive = parts.next().unwrap_or("");
            let argument = parts.next();

            match directive {
                "ifdef" | "ifndef" => {
                    let Some(name) = argument else {
                        return Err(ShaderError::Preprocess(format!(
                            "第 {number} 行：`#{directive}` 后面没有名字"
                        )));
                    };
                    let defined = defs.contains(&name);
                    stack.push(Frame {
                        taken: if directive == "ifdef" { defined } else { !defined },
                        in_else: false,
                        line: number,
                    });
                }
                "else" => {
                    let Some(frame) = stack.last_mut() else {
                        return Err(ShaderError::Preprocess(format!(
                            "第 {number} 行：`#else` 没有对应的 `#ifdef`"
                        )));
                    };
                    if frame.in_else {
                        return Err(ShaderError::Preprocess(format!(
                            "第 {number} 行：同一个 `#ifdef` 出现了两次 `#else`"
                        )));
                    }
                    frame.in_else = true;
                    frame.taken = !frame.taken;
                }
                "endif" => {
                    if stack.pop().is_none() {
                        return Err(ShaderError::Preprocess(format!(
                            "第 {number} 行：`#endif` 没有对应的 `#ifdef`"
                        )));
                    }
                }
                other => {
                    return Err(ShaderError::Preprocess(format!(
                        "第 {number} 行：不认识的预处理指令 `#{other}`"
                    )));
                }
            }
            // 指令行本身永远不出现在结果里，但要留个空行占位。
            out.push('\n');
            continue;
        }

        if active {
            out.push_str(line);
        }
        out.push('\n');
    }

    if let Some(frame) = stack.last() {
        return Err(ShaderError::Preprocess(format!(
            "第 {} 行的 `#ifdef` 没有 `#endif`",
            frame.line
        )));
    }

    Ok(out)
}

#[cfg(test)]
mod test {
    use super::*;

    /// 行数必须守恒，否则报错的行号就废了。
    fn assert_same_line_count(source: &str, output: &str) {
        assert_eq!(
            source.lines().count(),
            output.lines().count(),
            "预处理挪动了行号"
        );
    }

    #[test]
    fn a_source_without_directives_is_untouched() {
        let source = "let a = 1;\nlet b = 2;\n";
        assert_eq!(preprocess(source, &["X"]).unwrap(), source);
    }

    #[test]
    fn ifdef_keeps_the_branch_when_defined() {
        let source = "#ifdef GREEN\ngreen\n#endif\ntail";
        let out = preprocess(source, &["GREEN"]).unwrap();

        assert!(out.contains("green"));
        assert!(out.contains("tail"));
        assert_same_line_count(source, &out);
    }

    #[test]
    fn ifdef_drops_the_branch_when_undefined() {
        let source = "#ifdef GREEN\ngreen\n#endif\ntail";
        let out = preprocess(source, &[]).unwrap();

        assert!(!out.contains("green"));
        assert!(out.contains("tail"));
        assert_same_line_count(source, &out);
    }

    #[test]
    fn ifndef_is_the_mirror_image() {
        let source = "#ifndef GREEN\nfallback\n#endif";

        assert!(preprocess(source, &[]).unwrap().contains("fallback"));
        assert!(!preprocess(source, &["GREEN"]).unwrap().contains("fallback"));
    }

    #[test]
    fn else_takes_exactly_one_side() {
        let source = "#ifdef A\nyes\n#else\nno\n#endif";

        let on = preprocess(source, &["A"]).unwrap();
        assert!(on.contains("yes") && !on.contains("no"));

        let off = preprocess(source, &[]).unwrap();
        assert!(off.contains("no") && !off.contains("yes"));
    }

    #[test]
    fn nesting_works_both_ways() {
        let source = "#ifdef OUTER\n#ifdef INNER\nboth\n#else\nouter_only\n#endif\n#endif";

        let both = preprocess(source, &["OUTER", "INNER"]).unwrap();
        assert!(both.contains("both") && !both.contains("outer_only"));

        let outer = preprocess(source, &["OUTER"]).unwrap();
        assert!(outer.contains("outer_only") && !outer.contains("both"));

        // 外层不成立时，里层无论真假都不该留下任何东西。
        let none = preprocess(source, &["INNER"]).unwrap();
        assert!(!none.contains("both") && !none.contains("outer_only"));
    }

    #[test]
    fn an_inactive_branch_still_has_to_balance() {
        // 外层没成立，里层的指令仍然要解析——否则 `#endif` 会数错，
        // 后面的代码会被莫名其妙地删掉。
        let source = "#ifdef OFF\n#ifdef X\na\n#endif\n#endif\nkeep";
        assert!(preprocess(source, &[]).unwrap().contains("keep"));
    }

    #[test]
    fn indented_directives_are_recognized() {
        // WGSL 里 `#ifdef` 常常缩在函数体内，跟着周围的缩进走。
        let source = "    #ifdef A\n    a\n    #endif";
        assert!(preprocess(source, &["A"]).unwrap().contains("a"));
        assert!(!preprocess(source, &[]).unwrap().contains("a"));
    }

    #[test]
    fn a_missing_endif_is_an_error() {
        let error = preprocess("#ifdef A\nbody", &["A"]).unwrap_err();
        assert!(error.to_string().contains("没有 `#endif`"), "{error}");
    }

    #[test]
    fn a_stray_endif_is_an_error() {
        assert!(preprocess("body\n#endif", &[]).is_err());
    }

    #[test]
    fn a_stray_else_is_an_error() {
        assert!(preprocess("#else\nbody", &[]).is_err());
    }

    #[test]
    fn two_elses_are_an_error() {
        assert!(preprocess("#ifdef A\na\n#else\nb\n#else\nc\n#endif", &["A"]).is_err());
    }

    #[test]
    fn an_unknown_directive_is_refused_rather_than_passed_through() {
        // 放行的话 naga 只会说「意外的字符 `#`」，而真正的问题是名字拼错了。
        let error = preprocess("#ifdefined A\na\n#endif", &["A"]).unwrap_err();
        assert!(error.to_string().contains("ifdefined"), "{error}");
    }

    #[test]
    fn ifdef_without_a_name_is_an_error() {
        assert!(preprocess("#ifdef\nbody\n#endif", &[]).is_err());
    }

    #[test]
    fn the_error_message_points_at_the_right_line() {
        let error = preprocess("a\nb\n#endif", &[]).unwrap_err();
        assert!(error.to_string().contains("第 3 行"), "{error}");
    }

    #[test]
    fn a_hash_inside_a_line_is_not_a_directive() {
        // 只有**行首**（去掉缩进后）的 `#` 才算指令。
        let source = "let x = 1; // #endif 写在注释里\n";
        assert_eq!(preprocess(source, &[]).unwrap(), source);
    }
}
