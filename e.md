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
>
> **同日再追加**：动手做了 P0 第一项「地形能存能画」，过程中发现**这一项的判断
> 又错了一半**——序列化和权重纹理的接线其实早就在代码里，只是没人写完最后一块
> （层贴图数组 + 着色器）也没有测试证实过整条链路真的工作。详见「零」第 3 条。
>
> **再追加**：顺手跑了一次 `cargo test --workspace`（这个仓库看起来很久没人真正
> 跑全过一次——见第 4 条），结果 `kmesh` 两条 `raycast` 测试直接失败。查下去发现
> **P0 第二项「网格拾取」也早就写完了**（`Mesh::raycast` + `Scene::pick`，
> 连文档注释都写好了），只是里面的 Möller–Trumbore 求交算法有一个符号错误，
> 导致绝大多数命中被误判成「不在三角形内」。这是这次调查里**唯一一个不只是
> 「没测试」、而是真的在生产代码里跑不对的 bug**，已定位并修复。至此 P0 清单
> 上的四项这次全部核实/补完，**P0 已清空**。
>
> **再追加**：P0 清空之后，按用户要求移植了 18 个 three.js
> `webgpu_materials_*` 例子里的 11 个（`examples/kengine/new/materials_*`），
> 全部复用已有的材质钩子体系、没有新引擎能力。结论见「四」：钩子系统接得住
> 外观定制，接不住顶点变形和第二套 UV——这两条从「以后要做」变成了
> 「这次实地验证过确实还没有」。

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
3. **「地形能存能画」已完成，且上一版对现状的描述本身就不准确。** 动手前重新
   翻代码发现：
   - `kterrain::Terrain`/`Heightmap`/`SplatMap` **早就实现了 `Visit`**，
     `kscene::serialize` 的 `impl Visit for Node` 里**早就**有
     `visit_optional("Terrain", &mut self.terrain, ...)`——序列化这一半根本
     不是空的。上一版判断依据是「`grep terrain serialize.rs` 无匹配」，
     这次重新 grep 同一个文件却能搜到，大概率是我当时的检索本身有问题
     （和音频流那次同一类错误：检索范围/方式不对，不是代码真的没有）。
     只是这条路径**从来没有回归测试**，实际补的是
     `serialize.rs` 里的 `terrain_survives_a_roundtrip_heightmap_and_splat_alike`。
   - `kscene/src/terrain.rs` 的 `update_terrain` 里**早就有**「splat 权重变了就
     把 `custom_texture0` 重新打进材质、广播给 `__chunk*` 子节点」的逻辑——
     格式潦草（尾随空白、无说明注释）、**同样没有测试**，看起来像是没写完就
     搁置的半成品。这次顺手清理了格式、补了注释，也补了
     `painting_a_splat_layer_refreshes_the_chunk_material_weight_texture` 测试。
   - 真正缺的、这次新写的，只有：`examples/terrain_splat.wgsl`（`material_surface`
     钩子，按权重混合 4 层 `custom_texture_array`）、`examples/terrain.rs` 里
     建纹理数组 + 挂新着色器 + `1`~`4` 选层、Shift+左键涂层的交互，以及
     `tests/example_shaders.rs` 里对新着色器的编译校验。
   - **教训**：这个代码库里「用 grep 判断某功能存在与否」至少已经翻车两次
     （音频流、地形）。両次都是漏查了一个文件而不是功能真的缺。下次再做这类
     盘点，findings 要么用多个不同角度的 grep 交叉验证，要么直接读目标模块的
     完整目录列表，不能只信一次搜索的"无匹配"。
4. **P0「网格拾取」原来也早就实现了，而且是本次改动里唯一一个真正的生产代码
   bug（不只是缺测试）。** `kmesh::Mesh::raycast` + `kscene::Scene::pick`
   （连"不依赖碰撞体""没有 BVH 加速""编辑器拾取用"这些文档注释都写好了）
   一直都在，但 `cargo test --workspace` 从来没人真正跑过——一跑就发现
   `kmesh` 的两条 raycast 测试失败（`ray_hits_a_cube_from_directly_above`、
   `the_closest_triangle_wins`）。

   根因是 `ray_triangle`（Möller–Trumbore 射线三角形求交）的行列式算反了：
   写的是 `det = dir.dot(edge1.cross(edge2))`，标准算法要的是
   `det = edge1.dot(dir.cross(edge2))`——两者互为相反数（标量三重积对调
   前两个参数就反号），这个符号会经 `inv_det` 一路带进重心坐标 `u`、`v`
   和距离 `t`，让三者全部反号。多数真实命中因此会被「重心坐标必须落在
   [0,1]」的检查误判成「在三角形外」，只有极少数边界情形巧合还落在范围内
   ——这解释了为什么另外三条判断「不命中」的测试反而一直是绿的：
   一个把该命中的也判成不命中的 bug，天然更容易蒙对「不该命中」的用例。

   已修复（改成标准公式，复用 `h = dir.cross(edge2)` 而不是重复计算），
   `kmesh` 89 项测试全绿。顺手给一直没有测试的 `Scene::pick` 补了三条：
   命中没挂碰撞体的网格、忽略没网格的节点、两个网格叠在射线上时取更近的那个。

## 一、核实结果（避免重复劳动）

| next.md 的说法 | 现在还成立吗 | 依据 |
|---|---|---|
| 角色控制器「一个字都没接」 | ❌ 过期，**已完成** | `kphysics/src/character.rs` + `d2/character.rs`，`examples/kengine/new/physics_character.rs` |
| 反射探针「不能把周围几何采下来」 | ❌ 过期，**已完成** | `Renderer::capture_environment`（7g-2 节已经记录，只是没回头划掉这条） |
| 音频流「整段解码进内存」 | ❌ **本轮核实是错的** | `kaudio/src/source.rs` 已有 `AudioSource`/`BufferedSource`/`StreamingSource`，`lib.rs` 已导出；只有 `seek` 非 0 目标还没做 |
| 地形「存不下来」 | ❌ **本轮核实也是错的** | `Terrain`/`Heightmap`/`SplatMap` 早已实现 `Visit`，`kscene::serialize` 早已接线；缺的只是回归测试，已补 |
| 地形 splat 多层材质「渲染仍是单材质」 | ⚠️ **一半错**，本次已补完 | 权重纹理的接线（`custom_texture0`）早已在 `kscene/src/terrain.rs` 里，只是没测试；真缺的层贴图数组 + 着色器本次已写（`terrain_splat.wgsl` + `examples/terrain.rs`） |
| 拾取「确认没有」 | ❌ **本轮核实也是错的** | `Mesh::raycast` + `Scene::pick` 早已实现，只是有个符号 bug 导致几乎命中不了任何东西，已修复 |
| 渲染层 + 多相机多趟绘制「做不到」 | ✅ 仍然成立 | 全仓库搜不到 `RenderLayer` / 相机位掩码；`capture_environment` 顺手做出了「渲染到任意目标」的参数化，但没人拿它接渲染层 |

## 二、P0 —— 挡着做游戏（本轮核实/补完下来，已清空）

### ~~地形能存能画~~ ✅ 已完成（本次改动）

序列化和权重纹理广播其实早就在代码里（见「零」第 3 条），本次补完了缺的那块
（层贴图数组 + `material_surface` 混合钩子）并把整条链路第一次装上测试：

- `examples/terrain_splat.wgsl` —— 按 `custom_texture0`（`SplatMap::to_texture()`
  产出的权重图）混合 `custom_texture_array` 的最多 4 层地表贴图。
- `examples/terrain.rs` —— 建 4 层地表贴图数组（草/岩/土/雪）、挂上面这个着色器，
  加 `1`~`4` 选层、`Shift`+左键涂层的交互。
- 测试：`kscene::serialize::terrain_survives_a_roundtrip_heightmap_and_splat_alike`
  （高度图 + splat 权重存读一致）、
  `kscene::terrain::painting_a_splat_layer_refreshes_the_chunk_material_weight_texture`
  （涂图之后权重纹理确实被广播到块子节点的材质上）、
  `tests/example_shaders.rs` 里新着色器的编译校验。
- 顺手清理了 `kscene/src/terrain.rs` 里那段接线代码的格式（尾随空白、补了
  说明注释），逻辑没动。

**未做（有意留到真的需要时再补）**：层数固定为 4（`SplatMap::to_texture()` 的
硬限制，够绝大多数地形用）；splat 权重只有笔刷涂改，没有从图片导入权重图的路径。

### ~~网格拾取~~ ✅ 已完成（本次修复，见「零」第 4 条）

`Mesh::raycast` + `Scene::pick` 早就写好了，这次只是修掉了让它几乎命中不了
任何东西的一个符号 bug，并补了三条 `Scene::pick` 测试。**没有 BVH 加速**
——文档注释里写明是有意的（O(三角形数)，几万三角形以内一次拾取几微秒，
够用；真到需要加速的规模，`kscene` 已有的 BVH 剔除结构可以复用）。

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
| 材质钩子 | 只能覆盖表面属性和光照贡献，改不了管线状态（顶点变形、深度测试）——做体积雾、贴花式深度技巧会撞上；确认没有顶点着色器钩子，见「四」 |
| kmesh | 没有「子网格材质组」（一个 `Mesh` 按三角形区间分材质），glTF 多 primitive 和这次的 `materials_arrays` 例子都是靠拆成多个 `Node` 绕过去的，物体多、每个都要求「每面材质不同」时会掉合批 |
| kmesh | 只有一套 UV，lightmap 需要独立于材质 UV 的第二套（`TEXCOORD_1`），见「四」 |

## 四、three.js `webgpu_materials_*` 移植批次（18 个里做了 11 个）

放在 `examples/kengine/new/materials_*.rs` + 配套 `.wgsl`，全部走已有的
`material_surface` / `material_lighting` / `material_ambient` 三钩子体系，
**没有新增任何引擎能力**——这批例子本身就是在验证"钩子系统够不够用"，
结论是：材质外观层面的定制（含屏幕空间折射、次表面散射近似、matcap、
alpha hashing）钩子系统全接得住，接不住的是顶点变形和第二套 UV，
这两条以前就在 `next.md`/`e.md` 的已知清单里，这次算是又实地确认了一遍。

**已做**：`materials_toon`、`materials_matcap`、`materials_basic`、
`materialx_noise`、`materials_alphahash`、`materials_arrays`、
`materials_envmaps`、`materials_cubemap_mipmaps`、`materials_envmaps_bpcem`、
`materials_transmission`、`materials_sss`。全部有对应 `.wgsl` 且已进
`tests/example_shaders.rs` 的编译校验名单，`cargo build`/`clippy`/
`test --test example_shaders` 均过。

**没做，原因分三类**：

1. **需要新引擎能力，工作量不小，本批次没做**：
   - `materials_displacementmap`——顶点位移需要**顶点着色器钩子**，
     现在的三个钩子全在片元阶段。这是个真实的架构缺口（材质钩子文档
     早就写明"改不了顶点变形"），加一个 `material_vertex` 钩子涉及
     顶点缓冲布局、管线常量、和现有五种管线变体（含蒙皮/形变）的组合
     爆炸，值得单独立项而不是顺手做。
   - `materials_lightmap`——需要第二套 UV（`Vertex` 现在只有一个 `uv`
     字段），涉及顶点布局、glTF 导入 `TEXCOORD_1`、序列化格式，
     同样是个独立工作量。
2. **niche，性价比低，跳过**：`materials_envmaps_groundprojected`
   （地面投影天空盒，一个很窄的专用技巧，且依赖上面的 lightmap 同款
   UV/几何工作）。
3. **在原生桌面引擎里没有直接对应物，不该硬造一个假的去凑数**：
   - `materials_texture_html`——把一段 HTML DOM 渲染到 3D 表面上，
     纯浏览器技巧（`three-html-render` 那个 polyfill），原生引擎没有
     DOM 可渲染。硬凑一个"kui 面板贴到 3D 表面"会文不对题。
   - `materials_texture_manualmipmap`——三.js 那版其实是"往 HTML canvas
     上画画再当纹理用"，不是字面意思的"手动选 mip"；这台引擎已经在
     `terrain_splat.wgsl`/`materials_matcap.rs` 里多次演示过"CPU 生成
     纹理、每帧可重传"这个能力，再照抄一遍没有新信息量。
   - `materials_video`——视频解码不在这台引擎的依赖范围内
     （`kaudio` 只解音频，没有视频编解码器），造一个"假视频"纹理会
     误导人以为引擎真支持视频播放。

## 五、代码级发现（clippy / 热路径性能 / 健壮性，三路后台调查的结论）

结论先说：**这个代码库的 clippy 卫生状况和健壮性都非常好**，没有一大批能立项的问题，
下面是三路调查里筛出来、确认值得动手的那一小撮（已完成的用 ✅ 标注）。

### 已修复

- ✅ **`kscene::NodeIndex::clear()` 漏清 `terrains`**（见「零」）—— 带地形场景的
  隐藏性能/内存回归，一行修复 + 一条回归测试，已提交本次改动。
- ✅ **`kscene/src/lib.rs` 的 `best.map_or(true, ...)` 和 `kterrain/src/brush.rs`
  的 `to_texture()` 里那处 `needless_range_loop`**——顺着地形改动路过这两处，
  各改一行（`map_or(true,..)` → `is_none_or(..)`，`weights[c]` 越界判断改
  `weights.get(c)`），`cargo clippy -p kscene -p kterrain --all-targets` 现在零警告。

### 「值得顺手做」清单 ✅ 三条全部已修（原先这里错写成了"全是误报"，见下方更正）

1. ✅ **`kcore/src/visitor/impls.rs:686`，`char` 的 `Visit` 反序列化会 panic**——
   `char::from_u32(bytes).unwrap()` 改成了
   `.unwrap_or(char::REPLACEMENT_CHARACTER)`。
2. ✅ **`kscene/src/lib.rs`，`Scene::update()` 树遍历栈每帧新分配**——加了
   `Scene::scratch_traversal_stack` 字段，`update()` 里
   `std::mem::take` 取出来用、函数末尾存回去，容量跨帧复用。
3. ✅ **`kaudio/src/source.rs:286`，混音循环的 `needless_range_loop`**——
   `for i in 0..can_write*ch { out[i] = .. }` 改成
   `for dst in &mut out[..can_write*ch] { *dst = .. }`。

三条都在 `git log` 的 `aadc583 chore: small fixes and clippy cleanups` 里，
`cargo clippy -p kaudio -p kscene -p kcore --all-targets` 现在零警告。

**更正一次自己犯的错**：写到这里时我一度把这三条错判成"从来就是对的、
fork 报告是误报"——原因是核对时看到的已经是修完之后的代码，而
`git diff`（不带 `HEAD` 之外的比较对象）在改动已提交的情况下自然显示
"无差异"，被我当成了"从未改过"的证据。真正的判据应该是翻 `git log`
看这些文件最近一次改动的提交，而不是只看当前工作区有没有未提交的 diff
——工作区干净既可能是"从来没坏过"，也可能是"刚修好并提交了"，
两者不能只凭一次 `git diff` 区分。这次纠错留在这里，是因为它和「零」
那几条"grep 说没有其实有"的教训是同一类问题的另一个变种：**单一信号
（一次 grep、一次 diff）不构成结论，结论要靠交叉验证**。

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

## 六、P2 —— 结构性，不加功能

1. **`kscene` 已是最大 crate（10.6k 行）**，塞了场景图 + 物理同步 + 2D 物理 +
   音频同步 + 布娃娃 + 流式加载 + 序列化 + 脚本槽位 + 地形接线 + 贴花 + 探针。
   参照 Fyrox 拆 `fyrox-graph` 的做法，可以把物理同步/音频同步/流式加载拆成
   独立 crate。**纯重构，没有功能收益，只有在改动时开始互相绊脚才值得做。**
2. **CI 从没在 remote 上真正跑过**。`.github/workflows/ci.yml` 三道关只在本机
   验过等价命令，Linux 上编不编得过完全没验证过（本机只有 Windows）。push 一次
   到 `github.com/king54346/kengine` 就能验证，成本很低，应该尽快做一次。

## 七、建议执行顺序

P0 已经清空，剩下的都是「更好」而非「能不能」：

```
CI 在 remote 上真正跑一次           ← 成本极低、价值最高的下一步——这次的经历
                                      说明本地也很久没人跑过完整 `cargo test
                                      --workspace`，remote CI 能兜住这类漏网之鱼
   ↓
按兴趣从 P1 里挑：kaudio 效果器 / 软体双向耦合 / 渲染层多相机 / IES A-B 真文件验证
   ↓
kscene 拆分（P2）                   ← 只在开始互相绊脚时才做
```

原来这里还有一项"顺手做的小改动"（char Visit 防御 / update() 栈复用 /
kaudio 循环写法）——这三条已经在 `aadc583` 提交里修完了，详见「四、代码级发现」。

角色控制器、反射探针场景烘焙、音频流、地形能存能画、网格拾取——这五件事
原来都排在 P0，核实/补完下来现在全部已经做完（其中网格拾取还修掉了一个
真实的生产代码 bug），已从执行顺序里划掉。

**这次盘点最大的收获不是找到的缺口，而是发现"缺口"本身经常是假的**：
五项 P0 里有四项其实早就实现了，只是没测试、没文档、没人跑过完整测试套件去
验证。真正值得做的下一步动作，可能不是继续找新功能缺口，而是把 CI 接到
remote 上、把这类"写了但没验证过"的角落找出来跑一遍。

## 八、不重复的取舍（已在 next.md/PLAN.md 定过，继续遵守）

不做 ECS、不做多后端图形抽象、UI 布局不自己写（接 taffy）、不做编辑器、
每个第三方引擎关在一个 crate 里（wgpu→krender、rapier→kphysics、
cpal/symphonia→kaudio、boa→kscript、ab_glyph→kfont、taffy→kui）、
纯 Rust 工具链（这也是选 rapier 不选 Jolt、选 cpal 不选 OpenAL、选 boa 不选
rquickjs/deno_core 的唯一理由）、2D 不走节点组件（立即模式精灵）。
