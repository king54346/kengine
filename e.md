# e.md —— 现状核实后的下一步计划

> 依据：读完 `PLAN.md`（951 行，八个阶段全部 ✅）、`next.md`（1023 行，历次「查漏补缺」的记录）、
> `a.md`/`c.md`/`d.md`，并**对着当前代码逐条核实**了 `next.md` 里标的几个「未做」是否还成立。
> 结论：`next.md` 的 P0 清单**已经过期**——它写「角色控制器一个字都没接」时是对的，
> 但代码里已经有完整的 `kphysics::CharacterController`（3D + 2D，各带自动上台阶/斜坡限制/
> 沿墙滑动，`character_tests.rs` 十几条测试）和跑起来的例子 `physics_character`。
> 这份文档只列**核实过、现在仍然成立**的缺口，按「挡不挡着做游戏」排序。
>
> **2026-09-07 追加一轮**：又跑了三路代码级调查（clippy 全量扫描 / 热路径性能审查 /
> 健壮性与文档一致性检查），结论合并进了本文档，并**修正了上一版一处判断错误**
> ——见下面「零、这一轮的修正与已处理项」。

## 零、这一轮的修正与已处理项

1. **上一版说"音频流仍未做"是错的，我漏查了一个文件。** `kaudio/src/source.rs`
   已经有完整的 `AudioSource` trait + `BufferedSource`/`StreamingSource` 两套实现
   （`lib.rs` 也导出了），按文件长度自动选整段解码还是流式。**这条从 P0 划掉**，
   只剩一个小尾巴：`StreamingSource::seek` 目前只支持 seek 到 0（非 0 目标帧会被
   静默忽略，解码器从头重来），注释里写着待办——降级进 P1。
2. **修了一个真 bug（已修复并加回归测试）**：`kscene/src/lib.rs` 的
   `NodeIndex::clear()` 漏清 `terrains` 字段，而 `Scene::update` 每帧的树遍历
   都会把地形节点句柄再 `push` 一次（`terrain.rs:41` 又是逐帧 `.clone()` 整个
   `Vec` 去遍历）。带地形的场景因此逐帧无界增长：索引越滚越大、
   `update_terrain` 对同一块地形被重复调用，是个 O(帧数²) 的隐藏开销。
   已加 `terrains.clear()` 一行修复 + `terrain.rs` 里
   `the_terrain_index_does_not_grow_across_frames` 回归测试，`cargo test -p kscene` 已过。

## 一、核实结果（避免重复劳动）

| next.md 的说法 | 现在还成立吗 | 依据 |
|---|---|---|
| 角色控制器「一个字都没接」 | ❌ 过期，**已完成** | `kphysics/src/character.rs` + `d2/character.rs`，`examples/kengine/new/physics_character.rs` |
| 反射探针「不能把周围几何采下来」 | ❌ 过期，**已完成** | `Renderer::capture_environment`（7g-2 节已经记录，只是没回头划掉这条） |
| 音频流「整段解码进内存」 | ❌ **本轮核实是错的** | `kaudio/src/source.rs` 已有 `AudioSource`/`BufferedSource`/`StreamingSource`，`lib.rs` 已导出；只有 `seek` 非 0 目标还没做 |
| 地形「存不下来」 | ✅ 仍然成立 | `kscene/src/serialize.rs` 里搜不到 `Terrain`，场景存档会跳过地形 |
| 地形 splat 多层材质「渲染仍是单材质」 | ✅ 仍然成立 | `krender` 里搜不到 `SplatMap` / 多层地形材质，数据结构在 `kterrain`，没接到着色器 |
| 拾取「确认没有」 | ✅ 仍然成立 | `kscene` 只有物理射线 `cast_ray`，没有纯网格求交 |
| 渲染层 + 多相机多趟绘制「做不到」 | ✅ 仍然成立 | 全仓库搜不到 `RenderLayer` / 相机位掩码；`capture_environment` 顺手做出了「渲染到任意目标」的参数化，但没人拿它接渲染层 |

## 二、P0 —— 挡着做游戏（现在真缺的只有两样）

### 1. 地形能存能画

两件事互相独立但都卡在「地形现在只是个能跑的技术验证」：

- **高度图 + splat 权重图序列化**：`Terrain` / `Heightmap` / `SplatMap` 补 `Visit`，
  `kscene::serialize` 把地形节点纳入存盘/读档，仿照网格走「内联 vs 出处引用」
  两条路（程序化生成的内联，从图片导入的存路径）。
- **splat 多层材质着色器**：`kterrain` 早就在算逐顶点/逐像素的层权重
  （`SplatMap`），`krender` 也已经有纹理数组（`Texture::from_layers` + `D2Array`，
  阶段 7b 为这个铺过路），两块拼起来就是标准的「按权重贴图混合」——**不需要
  新技术，是两块已有的东西没接线**。

### 2. 网格拾取（不依赖碰撞体）

现在只能靠物理射线，意味着没挂碰撞体的东西点不到。做 UI 拾取、编辑器式的
「点选高亮」都需要。参照 `bevy_picking` 但只做「射线对网格三角形求交」这一半，
不需要它的事件冒泡系统：在 `kmesh` 上加一个 `raycast(ray) -> Option<(f32, Vec3)>`
（用现成的 BVH 加速），`kscene::Scene::pick` 包一层坐标变换。

## 三、P1 —— 已有功能的窟窿

按子系统分组，条目内部不再分优先级（这些是「更好」而非「能不能」）：

| 子系统 | 缺口 |
|---|---|
| kaudio | `StreamingSource::seek` 只支持 seek 到 0（非 0 目标被静默忽略，`source.rs:463` 注释里已写明待办，可以用 symphonia 的 `seek_track` 近似到关键帧）；混音总线与效果器（混响/滤波/压限）、HRTF、多普勒、遮挡（物理已就位，射线是现成的，只是没接） |
| kphysics | 软体只能单向耦合（推不动刚体）、软体之间不互相碰撞、碎裂是八等分不是凸切割、没有多体关节链、多线程求解（rapier 支持但要接 ktask 而非它自带的 rayon） |
| kparticle | 粒子间碰撞、粒子对刚体的反作用力、拖尾/网格粒子 |
| kanim | 形变的切线增量（`TANGENT` target）、循环事件（现在只能轮询时间回卷自己探测） |
| kasset | 打包压缩、资源依赖图（删资源不检查谁还在引用） |
| kfont | 整形（shaping）——阿拉伯文/天城文等复杂文字排版错误，需要接 rustybuzz |
| 渲染 | 渲染层 + 多相机多趟绘制（第一人称手臂、小地图这类需要两台相机各画各的场景才需要） |
| kui | 嵌套滚动区（先定滚轮该归哪层、scroll chaining 的规则，真实游戏 UI 很少嵌套，优先级最低） |
| 光照 | A/B 型 IES 只有几何不变量守着，没有厂商真文件对照过；面光源阴影仍是平行投影，形状比真实的收敛 |
| 材质钩子 | 只能覆盖表面属性和光照贡献，改不了管线状态（顶点变形、深度测试）——做体积雾、贴花式深度技巧会撞上 |

## 四、代码级发现（clippy / 热路径性能 / 健壮性，三路后台调查的结论）

结论先说：**这个代码库的 clippy 卫生状况和健壮性都非常好**，没有一大批能立项的问题，
下面是三路调查里筛出来、确认值得动手的那一小撮（已完成的用 ✅ 标注）。

### 已修复

- ✅ **`kscene::NodeIndex::clear()` 漏清 `terrains`**（见「零」）—— 带地形场景的
  隐藏性能/内存回归，一行修复 + 一条回归测试，已提交本次改动。

### 值得顺手做（都是小改动，风险低）

1. **`kcore/src/visitor/impls.rs:686`，`char` 的 `Visit` 反序列化会 panic**——
   `char::from_u32(bytes).unwrap()`，存档里的 `u32` 若落在代理对/超出范围
   （损坏或手改的存档），直接崩溃。当前没有任何 `#[derive(Visit)]` 结构体
   真的带 `char` 字段，所以暂时走不到，但这是共享反序列化基础设施里的一个地雷，
   和项目"存档读到坏数据要报错不要崩"的一贯原则不符。建议改成
   `unwrap_or(char::REPLACEMENT_CHARACTER)` 或返回 `VisitError`。
2. **`kscene/src/lib.rs:733`，`Scene::update()` 树遍历栈每帧新分配一个 `Vec`**——
   大场景（万级节点）下栈会扩容多次。可以提升成 `Scene` 的常驻 scratch 字段，
   `clear()` 后复用容量，做法和 `index`（`NodeIndex`）已经在用的模式一致。
3. **clippy 挑出的几处顺手清理**：
   - `kmesh/src/lib.rs:759-790`（网格求交附近）—— collapsible_if /
     unnecessary_map_or / manual_range_contains 三连。**恰好是上面"网格拾取"
     P0 要新写代码的地方**，等做那一项时顺手一起改，不必现在单独动。
   - `kaudio/src/source.rs:286` —— needless_range_loop，在混音的逐样本循环里，
     属于热路径，改成迭代器写法能省一次边界检查，值得改。
   - `kscene/src/lib.rs:1904`、`kterrain/src/terrain.rs:103,115` —— 场景图/
     地形代码里的 unnecessary_map_or / redundant_closure / collapsible_if，
     无性能影响，纯整理。

### 大概率不值得现在动

- `Scene::cull()` 里 `indices = Vec::new()` 每次调用新分配——每帧只调用几次
  （主视图 + 阴影级联），收益有限，不如先做上面的树遍历栈。
- `kmaterial` 的 `FxHashMap<String, MaterialValue>` 字符串键查表——阶段 2/3 已经
  用内容版本号解决了**缓存**层面的问题，字符串查表本身仍在，但改成 slot/index
  式访问是较大改动，收益不明确，不建议现在动。
- `ktask/src/iter/mod.rs` 的 4 处 TODO（68/86/200/219/305 行）—— 都是
  `size_hint`/减少拷贝这类性能微调的既有标记，非功能缺陷，不紧急。
- `kui_widgets` 的 3 条历史遗留 clippy 警告（`bool_assert_comparison` +
  两处 `unused_mut`）——文档里早就记过，维持现状。

## 五、P2 —— 结构性，不加功能

1. **`kscene` 已是最大 crate（10.6k 行）**，塞了场景图 + 物理同步 + 2D 物理 +
   音频同步 + 布娃娃 + 流式加载 + 序列化 + 脚本槽位 + 地形接线 + 贴花 + 探针。
   参照 Fyrox 拆 `fyrox-graph` 的做法，可以把物理同步/音频同步/流式加载拆成
   独立 crate。**纯重构，没有功能收益，只有在改动时开始互相绊脚才值得做。**
2. **CI 从没在 remote 上真正跑过**。`.github/workflows/ci.yml` 三道关只在本机
   验过等价命令，Linux 上编不编得过完全没验证过（本机只有 Windows）。push 一次
   到 `github.com/king54346/kengine` 就能验证，成本很低，应该尽快做一次。

## 六、建议执行顺序

```
地形序列化 + splat 着色器（kterrain/kscene/krender）  ← 数据结构都在，只是没接线
   ↓
网格拾取（kmesh/kscene）            ← 解锁交互类功能，顺手清掉 kmesh 那 3 条 clippy
   ↓
CI 在 remote 上真正跑一次           ← 成本极低，一直没验证是个隐患
   ↓
顺手做的小改动：char Visit 防御性修复 / update() 树遍历栈复用 / kaudio 混音循环
   ↓
按兴趣从 P1 里挑：kaudio 效果器 / 软体双向耦合 / 渲染层多相机 / IES A-B 真文件验证
   ↓
kscene 拆分（P2）                   ← 只在开始互相绊脚时才做
```

前两项的共同点和 next.md 当初判断的一致：**不是「更好看」，是「能不能」**。
角色控制器、反射探针场景烘焙、音频流这三件事原来都排在最前面，
现在核实下来全部已经做完，可以从执行顺序里划掉。

## 七、不重复的取舍（已在 next.md/PLAN.md 定过，继续遵守）

不做 ECS、不做多后端图形抽象、UI 布局不自己写（接 taffy）、不做编辑器、
每个第三方引擎关在一个 crate 里（wgpu→krender、rapier→kphysics、
cpal/symphonia→kaudio、boa→kscript、ab_glyph→kfont、taffy→kui）、
纯 Rust 工具链（这也是选 rapier 不选 Jolt、选 cpal 不选 OpenAL、选 boa 不选
rquickjs/deno_core 的唯一理由）、2D 不走节点组件（立即模式精灵）。
