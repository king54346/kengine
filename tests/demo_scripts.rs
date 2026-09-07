//! `examples/kengine/demo` 那套脚本的集成测试。
//!
//! 例子本身要开窗口，没法在 CI 里跑；但它的**玩法**全在 JS 里，而 JS 只需要
//! 场景与脚本运行时——两样都不碰 GPU。于是这里把 demo 的场景摆一遍，
//! 用真实的脚本文件跑上几百帧，验证那条链是通的：
//!
//! 刷怪 → 追人 → 打死 → 掉金币 → 进背包。
//!
//! 脚本改坏了（拼错方法名、少个 `null` 判断）会在这里当场露馅，
//! 而不是等到有人打开例子才发现。

use kengine::prelude::*;

/// 脚本目录，和 `main.rs` 里那份保持一致。
const SCRIPTS: &str = "examples/kengine/demo/scripts";

const FILES: [&str; 7] = [
    "player.js",
    "enemy.js",
    "bullet.js",
    "coin.js",
    "inventory.js",
    "spawner.js",
    "camera.js",
];

/// 装好脚本加载器、并且把七个脚本都读进来的资源管理器。
///
/// 必须**预热**：脚本是异步加载的，不等它读完的话，几百帧跑过去
/// 一个实例都建不起来，测试会以「什么都没发生」的方式假通过。
fn resources() -> ResourceManager {
    let manager = ResourceManager::new();
    manager.add_loader(ScriptLoader);
    for file in FILES {
        let path = format!("{SCRIPTS}/{file}");
        let script = manager
            .request_blocking::<Script>(&path)
            .unwrap_or_else(|error| panic!("读不到脚本 {path}（测试要在仓库根目录下跑）：{error}"));
        assert!(script.data_ref().is_some(), "{path} 加载失败");
    }
    manager
}

/// 摆一个和例子里一样的场景（去掉纯装饰的部分）。
fn scene() -> Scene {
    let mut scene = Scene::new();
    // 顺序同 main.rs：背包与刷怪器排在玩家前面。
    scene.add_node(Node::new("Bag").with_script(format!("{SCRIPTS}/inventory.js")));
    scene.add_node(Node::new("Spawner").with_script(format!("{SCRIPTS}/spawner.js")));
    scene.add_node(
        Node::new("Player")
            .with_position(Vec3::new(0.0, 0.45, 0.0))
            .with_script(format!("{SCRIPTS}/player.js")),
    );
    scene
}

/// 装好三个原型的脚本运行时。
fn runtime() -> ScriptRuntime {
    let mut runtime = ScriptRuntime::new();
    runtime.register_prototype("Enemy", || {
        Node::new("Enemy").with_script(format!("{SCRIPTS}/enemy.js"))
    });
    runtime.register_prototype("Bullet", || {
        Node::new("Bullet").with_script(format!("{SCRIPTS}/bullet.js"))
    });
    runtime.register_prototype("Coin", || {
        Node::new("Coin").with_script(format!("{SCRIPTS}/coin.js"))
    });
    runtime
}

/// 跑若干帧，返回这期间的全部信号。
fn run(
    runtime: &mut ScriptRuntime,
    scene: &mut Scene,
    input: &mut Input,
    resources: &ResourceManager,
    frames: usize,
) -> Vec<Signal> {
    let dt = 1.0 / 60.0;
    let mut all = Vec::new();
    for frame in 0..frames {
        scene.update();
        all.extend(runtime.process(scene, input, resources, dt, frame as f32 * dt));
        input.end_frame();
    }
    all
}

/// 场景里叫这个名字的节点有几个。
fn count(scene: &Scene, name: &str) -> usize {
    scene
        .nodes()
        .iter()
        .filter(|node| node.name == name)
        .count()
}

fn find_all(scene: &Scene, name: &str) -> Vec<Handle<Node>> {
    scene
        .nodes()
        .pair_iter()
        .filter(|(_, node)| node.name == name)
        .map(|(handle, _)| handle)
        .collect()
}

#[test]
fn every_script_loads_without_errors() {
    let resources = resources();
    let mut scene = scene();
    let mut runtime = runtime();
    let mut input = Input::new();

    run(&mut runtime, &mut scene, &mut input, &resources, 120);

    let errors: Vec<String> = runtime
        .errors()
        .iter()
        .map(|(_, error)| error.to_string())
        .collect();
    assert!(errors.is_empty(), "脚本抛异常了：{errors:?}");
}

#[test]
fn the_spawner_keeps_sending_waves() {
    let resources = resources();
    let mut scene = scene();
    let mut runtime = runtime();
    let mut input = Input::new();

    let signals = run(&mut runtime, &mut scene, &mut input, &resources, 300);

    let waves = signals
        .iter()
        .filter(|signal| signal.name == "wave")
        .count();
    assert!(waves >= 2, "五秒里只放了 {waves} 波");
    assert!(count(&scene, "Enemy") > 0, "一个敌人都没生成");
}

#[test]
fn enemies_close_in_on_the_player() {
    let resources = resources();
    let mut scene = scene();
    let mut runtime = runtime();
    let mut input = Input::new();

    // 先等第一只敌人出场。
    run(&mut runtime, &mut scene, &mut input, &resources, 90);
    let enemy = *find_all(&scene, "Enemy").first().expect("没有敌人");
    let before = scene[enemy].transform.position.length();

    run(&mut runtime, &mut scene, &mut input, &resources, 60);

    let after = scene[enemy].transform.position.length();
    assert!(
        after < before - 1.0,
        "敌人没有向玩家靠拢：{before} → {after}"
    );
}

#[test]
fn holding_fire_produces_bullets() {
    let resources = resources();
    let mut scene = scene();
    let mut runtime = runtime();

    let mut input = Input::new();
    input.bindings_mut().bind_action("attack", KeyCode::Space);
    input.press_key(KeyCode::Space);

    // 开火间隔 0.22 秒，30 帧（0.5 秒）里该出好几发。
    run(&mut runtime, &mut scene, &mut input, &resources, 30);

    assert!(count(&scene, "Bullet") > 0, "按住开火却一发都没有");
}

#[test]
fn the_player_walks_where_the_axes_point() {
    let resources = resources();
    let mut scene = scene();
    let mut runtime = runtime();

    let mut input = Input::new();
    input
        .bindings_mut()
        .bind_axis("move_x", KeyCode::KeyD, KeyCode::KeyA);
    input.press_key(KeyCode::KeyD);

    run(&mut runtime, &mut scene, &mut input, &resources, 30);

    let player = scene.find_by_name("Player").expect("玩家没了");
    assert!(scene[player].transform.position.x > 1.0, "按住右键没往右走");
}

#[test]
fn killing_an_enemy_drops_a_coin_that_lands_in_the_bag() {
    // 这条把整条链走完：打死 → 掉金币 → 玩家捡 → 进背包 → HUD 收到信号。
    //
    // 不指望子弹自己打中——敌人从随机角度过来，靠运气的测试早晚会闪。
    // 直接对敌人的脚本实例调 `hit`，走的是子弹调的那个方法。
    let resources = resources();
    let mut scene = scene();
    let mut runtime = runtime();
    let mut input = Input::new();

    run(&mut runtime, &mut scene, &mut input, &resources, 90);
    let enemy = *find_all(&scene, "Enemy").first().expect("没有敌人");
    let where_it_died = scene[enemy].transform.position;

    let instance = scene[enemy].script().expect("敌人没挂脚本").instance;
    runtime
        .eval_in(kscript::InstanceId(instance), &mut scene, "this.hit(999)")
        .expect("hit 调不通");

    assert_eq!(count(&scene, "Enemy"), 0, "打死之后节点该没了");
    assert_eq!(count(&scene, "Coin"), 1, "没掉金币");

    // 把玩家挪到金币上，下一帧金币脚本就该把它收走。
    let player = scene.find_by_name("Player").expect("玩家没了");
    scene[player].transform.position = where_it_died;

    let signals = run(&mut runtime, &mut scene, &mut input, &resources, 30);

    assert_eq!(count(&scene, "Coin"), 0, "金币没被捡走");
    let coins = signals
        .iter()
        .find(|signal| signal.name == "coins")
        .expect("背包没报出金币数");
    assert_eq!(coins.value, 1.0);

    // 击杀数不在这批信号里：`reportKill` 是在上面 `eval_in` 那次求值里
    // 发出的，而帧外求值攒下的信号会被下一次 tick 清掉（见 `eval_in` 的文档）。
    // 真正跑起来时它由子弹在正常的 `_process` 里触发，那条路径有 HUD 作证。
}

#[test]
fn touching_the_player_costs_health() {
    let resources = resources();
    let mut scene = scene();
    let mut runtime = runtime();
    let mut input = Input::new();

    run(&mut runtime, &mut scene, &mut input, &resources, 90);
    let enemy = *find_all(&scene, "Enemy").first().expect("没有敌人");

    // 把敌人直接贴到玩家身上，省得等它走过来。
    let player = scene.find_by_name("Player").expect("玩家没了");
    let beside = scene[player].transform.position + Vec3::new(0.5, 0.0, 0.0);
    scene[enemy].transform.position = beside;

    let signals = run(&mut runtime, &mut scene, &mut input, &resources, 30);

    // 取最后一条：血量是每次挨打都报一次的，中间那些还没掉够。
    let hp = signals
        .iter()
        .rfind(|signal| signal.name == "hp")
        .expect("玩家没报血量");
    assert!(hp.value < 100.0, "贴身了却没掉血");
}
