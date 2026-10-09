# kengine

纯 Rust 的 3D / 2D 游戏引擎：wgpu 渲染、rapier 物理、cpal 音频、boa 脚本（GDScript 式接口）、taffy 界面。
场景图路线，不做 ECS、不做编辑器。

```bash
cargo run --example demo                 # 脚本驱动的小游戏
cargo run --example postprocessing_ssr   # 任意一个例子，完整列表见 Cargo.toml / examples/README.md
cargo test --workspace                   # 全部测试
```

| 文档 | 内容 |
|---|---|
| [`docs/ROADMAP.md`](docs/ROADMAP.md) | 现状、各 crate 的职责、**还没做的事**、定下来的取舍 |
| [`docs/ITERATION_PLAN.md`](docs/ITERATION_PLAN.md) | 下一轮迭代方案：先做什么、怎么做、验收标准（附实测依据） |
| [`docs/HISTORY.md`](docs/HISTORY.md) | 开发记录：每一轮做了什么、为什么、踩过的坑 |
| [`examples/README.md`](examples/README.md) | 例子导览 |
| [`benches/README.md`](benches/README.md) | 基准怎么跑、数字怎么读 |

Windows 上用脚本的游戏要把主线程栈开大，见 [`kapp`](src/kapp/src/lib.rs) 文档开头。
