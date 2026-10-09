//! 异步与信号：`await wait()`、计时器、`async` 生命周期方法、脚本之间的信号、console。

use crate::{ConsoleLevel, Script, ScriptRuntime};
use kasset::{MemoryResourceIo, ResourceManager};
use kscene::{Node, Scene};
use std::sync::Arc;

/// 一个测试舞台：若干脚本文件、一个场景、一个运行时，时间手动往前推。
struct Stage {
    manager: ResourceManager,
    scene: Scene,
    runtime: ScriptRuntime,
    elapsed: f32,
}

impl Stage {
    fn new(files: &[(&str, &str)]) -> Self {
        let mut io = MemoryResourceIo::new();
        for (name, source) in files {
            io = io.with(*name, source.as_bytes().to_vec());
        }
        let manager = ResourceManager::with_io(Arc::new(io));
        manager.add_loader(crate::ScriptLoader);
        for (name, _) in files {
            let _ = manager.request_blocking::<Script>(*name);
        }
        Self {
            manager,
            scene: Scene::new(),
            runtime: ScriptRuntime::new(),
            elapsed: 0.0,
        }
    }

    fn add(&mut self, name: &str, script: &str) -> kcore::pool::Handle<Node> {
        self.scene.add_node(Node::new(name).with_script(script))
    }

    /// 推进 `dt` 秒跑一帧，返回这一帧的信号 `(名字, 值)`。
    fn step(&mut self, dt: f32) -> Vec<(String, f64)> {
        self.elapsed += dt;
        self.scene.update();
        let mut input = kinput::Input::new();
        self.runtime
            .process(&mut self.scene, &mut input, &self.manager, dt, self.elapsed)
            .into_iter()
            .map(|s| (s.name, s.value))
            .collect()
    }

    fn names(&mut self, dt: f32) -> Vec<String> {
        self.step(dt).into_iter().map(|(n, _)| n).collect()
    }
}

// ── await / 计时器 ──

#[test]
fn await_wait_resumes_after_the_game_time_has_passed() {
    let mut stage = Stage::new(&[(
        "s.js",
        r#"return { async _ready() { emit("start"); await wait(0.5); emit("done"); } };"#,
    )]);
    stage.add("n", "s.js");
    assert_eq!(stage.names(0.1), ["start"]);
    assert!(stage.names(0.1).is_empty(), "0.2 秒时还不该醒");
    assert!(stage.names(0.2).is_empty(), "0.4 秒时还不该醒");
    assert_eq!(stage.names(0.2), ["done"], "0.6 秒时该醒了");
    assert!(stage.names(0.2).is_empty(), "只醒一次");
}

#[test]
fn next_frame_waits_exactly_one_frame() {
    let mut stage = Stage::new(&[(
        "s.js",
        r#"return { async _ready() { for (let i = 0; i < 3; i++) { emit("frame", i); await nextFrame(); } } };"#,
    )]);
    stage.add("n", "s.js");
    // 每帧走一圈，不会在一帧里把循环跑完。
    assert_eq!(stage.step(0.016), [("frame".to_string(), 0.0)]);
    assert_eq!(stage.step(0.016), [("frame".to_string(), 1.0)]);
    assert_eq!(stage.step(0.016), [("frame".to_string(), 2.0)]);
    assert!(stage.step(0.016).is_empty());
}

#[test]
fn promise_continuations_run_in_the_same_frame() {
    // 不涉及计时器的 Promise 当帧就该兑现——以前根本不跑任务队列，`then` 永远不回来。
    let mut stage = Stage::new(&[(
        "s.js",
        r#"return { _ready() { Promise.resolve(7).then(v => emit("then", v)); } };"#,
    )]);
    stage.add("n", "s.js");
    assert_eq!(stage.step(0.016), [("then".to_string(), 7.0)]);
}

#[test]
fn self_stays_bound_across_await_for_every_instance() {
    let mut stage = Stage::new(&[(
        "s.js",
        r#"return { async _ready() { await wait(0.1); self.position.y = self.name === "a" ? 1 : 2; } };"#,
    )]);
    let a = stage.add("a", "s.js");
    let b = stage.add("b", "s.js");
    stage.step(0.05);
    // 留点余量：0.05 + 0.1 在 f32 里可能刚好比 0.15 小一丝，那样要再等一帧。
    stage.step(0.2);
    assert_eq!(stage.scene.try_get(a).unwrap().transform.position.y, 1.0);
    assert_eq!(stage.scene.try_get(b).unwrap().transform.position.y, 2.0);
}

#[test]
fn set_timeout_set_interval_and_clear() {
    let mut stage = Stage::new(&[(
        "s.js",
        r#"
        let ticks = 0;
        return {
            _ready() {
                setTimeout(() => emit("timeout"), 250);
                const id = setInterval(() => { ticks++; emit("interval", ticks); if (ticks === 3) clearInterval(id); }, 100);
                const never = setTimeout(() => emit("never"), 50);
                clearTimeout(never);
            },
        };
        "#,
    )]);
    stage.add("n", "s.js");
    let mut all = Vec::new();
    for _ in 0..10 {
        all.extend(stage.step(0.1));
    }
    let names: Vec<&str> = all.iter().map(|(n, _)| n.as_str()).collect();
    assert!(!names.contains(&"never"), "{names:?}");
    assert_eq!(
        names.iter().filter(|n| **n == "timeout").count(),
        1,
        "{names:?}"
    );
    let intervals: Vec<f64> = all
        .iter()
        .filter(|(n, _)| n == "interval")
        .map(|(_, v)| *v)
        .collect();
    assert_eq!(intervals, [1.0, 2.0, 3.0], "清掉之后不再触发");
}

#[test]
fn a_rejected_async_ready_disables_the_script() {
    let mut stage = Stage::new(&[(
        "s.js",
        r#"return { async _ready() { await wait(0.1); throw new Error("坏了"); }, _process() { emit("alive"); } };"#,
    )]);
    stage.add("n", "s.js");
    assert_eq!(stage.names(0.05), ["alive"]);
    stage.step(0.1);
    assert!(stage.names(0.1).is_empty(), "异步异常之后脚本该被停掉");
    let errors = stage.runtime.errors();
    assert_eq!(errors.len(), 1);
    assert!(errors[0].1.message.contains("坏了"), "{:?}", errors[0].1);
}

#[test]
fn a_throwing_timer_callback_only_stops_its_owner() {
    let mut stage = Stage::new(&[
        (
            "bad.js",
            r#"return { _ready() { setTimeout(() => { throw new Error("x"); }, 10); }, _process() { emit("bad"); } };"#,
        ),
        ("good.js", r#"return { _process() { emit("good"); } };"#),
    ]);
    stage.add("bad", "bad.js");
    stage.add("good", "good.js");
    stage.step(0.016);
    stage.step(0.016);
    let names = stage.names(0.016);
    assert_eq!(names, ["good"]);
}

#[test]
fn timers_of_a_removed_node_never_fire() {
    let mut stage = Stage::new(&[(
        "s.js",
        r#"return { _ready() { setTimeout(() => emit("ghost"), 100); } };"#,
    )]);
    let handle = stage.add("n", "s.js");
    stage.step(0.016);
    stage.scene.remove_node(handle);
    for _ in 0..10 {
        assert!(stage.names(0.05).is_empty());
    }
    assert!(stage.runtime.errors().is_empty());
}

// ── 信号 ──

const LISTENER: &str = r#"
return {
    _ready() {
        getNode("button").connect("pressed", (a, b) => {
            emit("heard", a + b);
            // 回调里的 self 是订阅者自己。
            self.position.x = 5;
        });
    },
};
"#;

const BUTTON: &str = r#"
let count = 0;
return {
    _process() {
        count++;
        if (count === 2) emit("delivered", self.emitSignal("pressed", 2, 3));
    },
};
"#;

#[test]
fn signals_reach_listeners_with_their_own_self() {
    let mut stage = Stage::new(&[("listener.js", LISTENER), ("button.js", BUTTON)]);
    let listener = stage.add("listener", "listener.js");
    let button = stage.add("button", "button.js");
    stage.step(0.016);
    let signals = stage.step(0.016);
    assert!(signals.contains(&("heard".to_string(), 5.0)), "{signals:?}");
    assert!(
        signals.contains(&("delivered".to_string(), 1.0)),
        "{signals:?}"
    );
    assert_eq!(
        stage.scene.try_get(listener).unwrap().transform.position.x,
        5.0
    );
    assert_eq!(
        stage.scene.try_get(button).unwrap().transform.position.x,
        0.0
    );
}

#[test]
fn await_to_signal_resumes_with_the_arguments() {
    let mut stage = Stage::new(&[
        (
            "waiter.js",
            r#"return { async _ready() { const v = await getNode("emitter").toSignal("go"); emit("got", v); } };"#,
        ),
        (
            "emitter.js",
            r#"let n = 0; return { _process() { if (++n === 3) self.emitSignal("go", 42); } };"#,
        ),
    ]);
    stage.add("waiter", "waiter.js");
    stage.add("emitter", "emitter.js");
    assert!(stage.names(0.016).is_empty());
    assert!(stage.names(0.016).is_empty());
    assert_eq!(stage.step(0.016), [("got".to_string(), 42.0)]);
}

#[test]
fn once_and_disconnect() {
    let mut stage = Stage::new(&[(
        "s.js",
        r#"
        let hits = 0;
        const counter = () => { hits++; };
        return {
            _ready() {
                self.connect("ping", () => emit("once"), true);
                self.connect("ping", counter);
                self.emitSignal("ping");
                self.emitSignal("ping");
                self.disconnect("ping", counter);
                self.emitSignal("ping");
                emit("hits", hits);
            },
        };
        "#,
    )]);
    stage.add("n", "s.js");
    let signals = stage.step(0.016);
    assert_eq!(signals.iter().filter(|(n, _)| n == "once").count(), 1);
    assert!(signals.contains(&("hits".to_string(), 2.0)), "{signals:?}");
}

#[test]
fn a_throwing_listener_does_not_stop_the_emitter() {
    let mut stage = Stage::new(&[
        (
            "listener.js",
            r#"return { _ready() { getNode("e").connect("x", () => { throw new Error("boom"); }); }, _process() { emit("listener"); } };"#,
        ),
        (
            "emitter.js",
            r#"let n = 0; return { _process() { if (++n === 2) self.emitSignal("x"); emit("emitter"); } };"#,
        ),
    ]);
    stage.add("l", "listener.js");
    stage.add("e", "emitter.js");
    stage.step(0.016);
    stage.step(0.016);
    let names = stage.names(0.016);
    assert_eq!(names, ["emitter"], "订阅者停了，发信号的照常");
}

// ── console ──

#[test]
fn console_levels_and_object_formatting() {
    let mut stage = Stage::new(&[(
        "s.js",
        r#"return { _ready() {
            console.log("位置", new Vector3(1, 2, 3), { hp: 10, tags: ["a", "b"] });
            console.warn("小心");
            console.error(new Error("糟了"));
            console.assert(1 === 2, "一不等于二");
            console.assert(true, "不该出现");
        } };"#,
    )]);
    stage.add("n", "s.js");
    stage.step(0.016);
    let log = stage.runtime.take_console();
    let levels: Vec<ConsoleLevel> = log.iter().map(|m| m.level).collect();
    assert_eq!(
        levels,
        [
            ConsoleLevel::Info,
            ConsoleLevel::Warn,
            ConsoleLevel::Error,
            ConsoleLevel::Error
        ]
    );
    assert_eq!(
        log[0].text,
        r#"位置 (1, 2, 3) { hp: 10, tags: ["a", "b"] }"#
    );
    assert!(log[2].text.starts_with("Error: 糟了"), "{}", log[2].text);
    assert!(log[3].text.contains("一不等于二"));
    assert!(stage.runtime.take_console().is_empty(), "取走就清");
}

#[test]
fn print_expands_plain_objects() {
    // 以前打出来的是 `[object Object]`。
    let mut stage = Stage::new(&[(
        "s.js",
        r#"return { _ready() { print({ a: 1 }); console.log([1, [2, [3]]]); } };"#,
    )]);
    stage.add("n", "s.js");
    stage.step(0.016);
    let log = stage.runtime.take_console();
    assert_eq!(log[0].text, "[1, [2, [3]]]");
}

// ── 数学 ──

#[test]
fn seeded_random_is_reproducible_and_in_range() {
    let mut stage = Stage::new(&[(
        "s.js",
        r#"return { _ready() {
            const a = new RandomNumberGenerator(123);
            const b = new RandomNumberGenerator(123);
            let same = true, inRange = true;
            for (let i = 0; i < 1000; i++) {
                const x = a.randf();
                if (x !== b.randf()) same = false;
                if (!(x >= 0 && x < 1)) inRange = false;
                const k = a.randiRange(3, 5); b.randiRange(3, 5);
                if (k < 3 || k > 5 || k !== Math.floor(k)) inRange = false;
            }
            emit("same", same ? 1 : 0);
            emit("range", inRange ? 1 : 0);
            emit("differs", new RandomNumberGenerator(1).randi() !== new RandomNumberGenerator(2).randi() ? 1 : 0);
        } };"#,
    )]);
    stage.add("n", "s.js");
    let signals = stage.step(0.016);
    assert_eq!(
        signals,
        [
            ("same".into(), 1.0),
            ("range".into(), 1.0),
            ("differs".into(), 1.0)
        ]
    );
}

#[test]
fn mathf_helpers() {
    let mut stage = Stage::new(&[(
        "s.js",
        r#"return { _ready() {
            emit("wrap", Mathf.wrap(-1, 0, 5));
            emit("lerpAngle", Mathf.lerpAngle(Mathf.degToRad(350), Mathf.degToRad(10), 0.5));
            emit("moveToward", Mathf.moveToward(0, 10, 3));
            emit("remap", Mathf.remap(5, 0, 10, 100, 200));
            emit("pingPong", Mathf.pingPong(7, 5));
            emit("vecMove", new Vector3(0, 0, 0).moveToward(new Vector3(10, 0, 0), 4).x);
            emit("angle", new Vector3(1, 0, 0).angleTo(new Vector3(0, 1, 0)));
        } };"#,
    )]);
    stage.add("n", "s.js");
    let signals = stage.step(0.016);
    let get = |name: &str| signals.iter().find(|(n, _)| n == name).unwrap().1;
    assert_eq!(get("wrap"), 4.0);
    assert!(
        get("lerpAngle").abs() < 1e-9 || (get("lerpAngle") - std::f64::consts::TAU).abs() < 1e-9,
        "{}",
        get("lerpAngle")
    );
    assert_eq!(get("moveToward"), 3.0);
    assert_eq!(get("remap"), 150.0);
    assert_eq!(get("pingPong"), 3.0);
    assert_eq!(get("vecMove"), 4.0);
    assert!((get("angle") - std::f64::consts::FRAC_PI_2).abs() < 1e-9);
}

#[test]
fn clear_drops_pending_timers() {
    let mut stage = Stage::new(&[(
        "s.js",
        r#"return { _ready() { setTimeout(() => emit("late"), 100); } };"#,
    )]);
    stage.add("n", "s.js");
    stage.step(0.016);
    stage.runtime.clear();
    stage.scene = Scene::new();
    for _ in 0..5 {
        assert!(stage.names(0.1).is_empty());
    }
}

// ── 报错位置 ──

#[test]
fn errors_name_the_script_file_and_line() {
    let mut stage = Stage::new(&[(
        "enemy.js",
        "let hp = 3;\nreturn {\n    _ready() {\n        undefinedFunction();\n    },\n};\n",
    )]);
    stage.add("n", "enemy.js");
    stage.step(0.016);
    let errors = stage.runtime.errors();
    assert_eq!(errors.len(), 1);
    let message = &errors[0].1.message;
    assert!(
        message.contains("enemy.js:4:"),
        "报错要指到 enemy.js 的第 4 行：{message}"
    );
}

// ── 补间 ──

#[test]
fn awaiting_a_tween_resumes_after_it_lands() {
    let mut stage = Stage::new(&[(
        "s.js",
        r#"return {
            async _ready() {
                const ok = await self.tween("position", new Vector3(0, 2, 0), 0.3, "easeOutCubic");
                emit(ok ? "landed" : "cut", self.position.y);
                // 被顶替的那段兑现成 false。
                const first = self.tween("scale", 3, 1.0);
                self.tween("scale", 1, 0.1);
                emit((await first) ? "first-landed" : "first-replaced");
            },
        };"#,
    )]);
    let node = stage.add("n", "s.js");
    let mut seen = Vec::new();
    for _ in 0..40 {
        // kapp 里补间在脚本之后、变换重算之前推进；这里照那个顺序。
        let signals = stage.step(0.05);
        stage.scene.tick_animations(0.05);
        seen.extend(signals);
    }
    let landed = seen
        .iter()
        .find(|(n, _)| n == "landed")
        .expect("等到补间走完");
    assert!(
        (landed.1 - 2.0).abs() < 1e-4,
        "走完时正好在终点：{}",
        landed.1
    );
    assert!(seen.iter().any(|(n, _)| n == "first-replaced"), "{seen:?}");
    assert_eq!(stage.scene[node].transform.scale, kmath::Vec3::ONE);
}

#[test]
fn unknown_tween_property_or_ease_resolves_false() {
    let mut stage = Stage::new(&[(
        "s.js",
        r#"return { async _ready() {
            emit((await self.tween("colour", 1, 0.1)) ? "yes" : "no");
            emit((await self.tween("position", new Vector3(1, 0, 0), 0.1, "wobbly")) ? "yes" : "no");
        } };"#,
    )]);
    stage.add("n", "s.js");
    let mut names = Vec::new();
    for _ in 0..5 {
        names.extend(stage.names(0.05));
    }
    assert_eq!(names, ["no", "no"]);
}
