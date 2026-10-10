# kengine 路线图

> 现状、还没做的事、定下来的取舍。**核实于 2026-10-05**——每一条「还没做」
> 都对着当时的代码查过（不止一次 grep：读模块、跑测试、看截图）。
>
> 接下来先做什么、怎么做，见 [`ITERATION_PLAN.md`](ITERATION_PLAN.md)。
> 怎么一步步做到今天、路上踩过的坑，见 [`HISTORY.md`](HISTORY.md)。
> 那份是当时的记录，其中写的「还没做」以这份为准。

### 一句话结论

渲染和导入的功能面已经很宽（three.js 的 webgpu / webgl 例子移植了 179 个）；玩法层的硬缺口（手柄、根运动、寻路）
2026-10 也补上了，剩下的是「更好」：导航网格、避让、逐实例剔除。**眼下最该做的是工程侧**：CI 一次都没在 remote 上跑过
（Linux 上编不编得过不知道），`krender/src/lib.rs` 和 `kscene` 都太大。渲染侧剩下的多是「更好」，不急。建议的先后见 [二、还没做的](#二还没做的) 开头。

## 一、现状

| | |
|---|---|
| crate | 35 个（`src/k*`），约 17 万行 Rust + WGSL + JS |
| 测试 | `cargo test --workspace`：**2968 项全绿**（含文档测试，2026-10-05）；另有 34 项 `#[ignore]`（截图回归、要显卡的），`-- --ignored` 跑也全绿 |
| 例子 | 226 个（`examples/`，其中 179 个是 three.js 移植，在 `examples/kengine/new/`；`ocean`、`water_island`、`meadow` 是 kcomponents 的完整场景；`vfx` 是技能特效沙盒（ThreeJSVFX-Demo / LinearAbilityCastingThreeJS 的移植，六个技能已完成）；`xunxian` 是寻仙角色换装（zj + 武器 + 法宝，资源在 `assets/unpack2`）） |
| 基准 | 6 组（`benches/`，说明见 `benches/README.md`） |
| 集成测试 | 7 个（`tests/`：例子着色器编译校验、例子截图回归、无头截图、demo 脚本、导入器样本……） |
| CI | `.github/workflows/ci.yml`（fmt → clippy `-D warnings` → Linux + Windows test → bench --no-run → `video` 特性能编）。**还没在 remote 上真正跑过**；2026-10-05 发现这个文件其实不在仓库里（文档一直写着有），按 HISTORY 的记录重写了一份。照现状 fmt 那关必红（267 个文件没格式化） |

### 各 crate 管什么

每个第三方库只出现在一个 crate 里（括号里是它）。

| crate | 职责 |
|---|---|
| kcore / kcore-derive | 对象池与句柄、`Visit` 序列化（及其派生宏）、字节读写工具 |
| kmath | 数学：向量矩阵（glam）、包围盒、射线、BVH、样条、二维 / 三维图元 |
| klog | 日志（tracing） |
| ktask | 任务池、并行迭代 |
| kasset | 异步资源加载、热重载、资源包（逐项 deflate） |
| kinput | 键鼠 + 手柄（gilrs）采集、动作 / 轴映射 |
| kwinit | 窗口与事件循环（winit） |
| kapp | 应用生命周期、插件、阶段调度、定长物理步 |
| kcamera | 相机（透视 / 正交、渲染层、离屏目标）、视锥剔除、轨道 / 飞行 / 平移相机 |
| kmesh | 网格、内置图元（含茶壶、圆角盒）、子网格材质组、两套 UV、射线求交 |
| ktexture | 纹理（二维、数组、三维）、mip 链、各向异性、图片解码（含 GIF / AVIF / DDS / KTX / KTX2 数组 / PVR）、3D LUT、原地 / 局部更新（`with_pixels` / `with_region`） |
| kshader | WGSL 着色器资源与校验（naga）、MaterialX / TSL 噪声库（`noise`）、GLSL / ShaderToy → WGSL（`glsl`） |
| kmaterial | 材质：标准参数 + 自定义贴图 / 参数槽 + 着色器钩子 |
| kpbr | PBR、IBL、预滤波、探针、HDR / Ultra HDR 解码、Phong / 不受光 / 线框 / 扩展物理材质 |
| klight | 光源（点 / 聚 / 方向 / 半球 / 面）、聚簇、阴影级联、cookie、IES、按流明 / 勒克斯给强度 |
| krender | 渲染（wgpu）：前向 + 预通道、阴影、SSAO、可编程后处理链（TAA / SMAA / SSR / SSGI / DOF / bloom…）、UI 合成、截图 |
| kscene | 场景图：节点、层级变换、物理 / 音频 / 动画同步、LOD、贴花、地形接线、序列化、拾取 |
| kphysics | 刚体与碰撞（rapier 3D + 2D）、角色控制器、布料 / 软体、载具、布娃娃、碎裂 |
| kanim | 动画曲线、剪辑、混合、状态机 / 混合树、IK、交叉淡化、循环 / 结束事件 |
| kparticle | CPU 粒子与 GPU 粒子 |
| kaudio | 音频（cpal + symphonia）：整段 / 流式来源（可 seek）、3D 空间音频 |
| kgltf | glTF 2.0 导入（含 Draco、meshopt、KTX2、各 KHR 材质扩展、变体） |
| kimport | 其余格式：OBJ STL PLY FBX Collada 3MF AMF 3DS VOX KMZ USD/USDZ LDraw IFC 3DM VRML SVG Lottie MaterialX TTF 立体字…… |
| kxunxian | 寻仙解包资源直读：`.cct` 角色配置 / `.cmf` 材质表 / `.pmf` 网格 / `.psf` 骨架 / `.paf` 动作；按装扮拼玩家角色（zj 衣柜），武器 wqa、法宝 fba 挂到挂点骨骼上，上下半身动作合并 |
| kfont | 字体加载、光栅化、排版、矢量轮廓（ab_glyph） |
| klocale | 本地化：Fluent（`.ftl`）子集的字符串表、当前语言 + 回退链、`tr!` / 脚本 `tr()`（零依赖，解析器自写） |
| knav | 导航：2.5D 格子烘焙（边缘按代理半径腐蚀）、A* + 视线拉直、动态挡区 / 代价（零依赖） |
| kui / kui_widgets | 界面核心（taffy 布局、绘制、命中、标记语言）/ 控件 |
| ksprite | 2D 精灵、图集、帧动画 |
| kterrain | 高度图地形、分块、笔刷、splat 多层材质 |
| kgizmo | 即时模式调试绘制 |
| kvideo | 视频贴图：MP4 拆包（mp4）+ H.264 解码（OpenH264，源码随 crate 编译） |
| kscript | JavaScript 脚本（boa）：GDScript 式接口、async / 计时器、信号、热重载带状态 |
| kcomponents | 可复用的场景组件。**海面**：JONSWAP 谱 + FFT 三级联（涌浪 / 风浪 / 涟漪）、极坐标连续 LOD、按像素覆盖淡出各级（防远处摩尔纹）、尖浪、水深图（浅水浪变矮、拍岸浪）、按水深吸收、折射、次表面散射、屏幕空间反射、三层独立泡沫（白浪 / 表面薄沫 / 岸边，各自纹理、颜色、覆盖量）、波光、大气透视。**浮力**：多点（船：俯仰横摇）/ 单点（浮标、漂浮物：上下浮动 + 扶正），阻尼按相对水的速度。**尾迹**：波动方程网格，任何物体都能发尾迹，水面起伏、白沫、互相干涉，漂浮物也被推着晃。**天空**：程序化（瑞利 / 米氏大气、日盘、会飘会变形的体积云、星空、地平线雾带）或全景照片，同一份烘成环境光。**水下**：逐像素水线、按方向分级的雾、光柱、焦散。**草地**（照 three-stylized）：噪声起伏的地块 + 泥地斑块、按面积撒的上万根草叶（风是顶点钩子里的两道正弦波，逆光透射、影子里留底的风格化光照走光照钩子，吃场景里真的灯）、三种程序化画出来的野花（遮罩图集 + 四套配色，镂空连影子一起挖）。八套环境预设、画质档位 |

## 二、还没做的

按子系统排。角色控制器、音频流、拾取、地形存档、多相机这些以前的硬缺口已经补上了；
**还剩的硬缺口在「游戏玩法」一节**，其余各节都是「更好」，不是「能不能做」。

**建议的先后**（按「挡不挡着做游戏 × 成本」排）：

| 优先 | 项 | 理由 |
|---|---|---|
| P0 | CI 在 remote 上跑一次 | 成本最低；Linux 能不能编过从没验证过。先 `cargo fmt --all`（267 个文件）再推 |
| P1 | 导航网格 + 避让 | 格子寻路已经有了（`knav`）；多层地形（桥）、大地图、成群的 NPC 才需要 |
| P2 | 逐实例剔除 / 间接绘制 | 实例化已经有了（整组剔除）；一组铺满整张地图、或者要 GPU 自己生成实例时才需要 |
| P2 | 拆 `krender/src/lib.rs`、`kscene` | 纯重构，改动开始互相绊脚时再做 |
| P2 | 其余渲染 / 导入缺口 | 按具体游戏需要挑 |

### 游戏玩法

| 缺口 | 说明 |
|---|---|
| 导航网格 / 避让 | 有格子寻路（见三），没有导航网格：一个 (x, z) 只有一层，桥下面那层没有；大地图格子多（1 km² / 0.25 m = 1600 万格）。代理之间也不互相避让 |
| 存档槽位 | 机制是全的：场景序列化（`Visit`）+ 脚本的 `_save()` / `_load()`（`ScriptRuntime::save_states`）。缺的是上面一层：存档槽位 / 文件管理、只存「会变的」节点而不是整个场景、版本迁移的约定；也没有一个完整的存读档例子 |

### 渲染

| 缺口 | 说明 |
|---|---|
| 覆盖层的深度效果 | 覆盖层相机（`CameraTarget::Overlay`）在后处理之前叠上，雾 / 景深 / SSAO 对覆盖层用的是主画面的深度；要完全不受影响得让开它们的遮罩位 |
| 顶点钩子不能读 `globals` | 带顶点钩子的材质的阴影管线 group 0 是光空间矩阵，和 `globals` 同号；钩子读了 `globals` 的话这套管线建不出来，退回普通阴影（影子没位移，记一条警告）。时间用 `vertex.time`，别的全局量（相机位置）要用得另想办法 |
| 自定义材质改管线状态 | 钩子改不了深度测试、光栅化状态——体积雾、深度技巧会撞上（钩子式设计的代价） |
| 2D 自定义材质 | `ksprite` 是独立的批处理管线，没有钩子 |
| 异步读回 | GPU 读回是同步等待的 |
| 计算结果当顶点 | 粒子能直接用存储缓冲，存储纹理能直接当材质贴图（`StorageTexture::texture()`）；网格的顶点缓冲还不行——要么塞进 `Rgba16Float` 纹理在顶点钩子里 `textureLoad`（`tsl_vfx_linkedparticles` 就这么做），要么等这一项 |
| 逐实例剔除 / 间接绘制 | 实例化（见三）按**整组**剔除，看见一个就画全部——大组要自己按区域切成几个节点；没有 `draw_indirect`，计算着色器生成的实例还得读回再塞（`struct_drawindirect`、`tsl_galaxy` 仍是拼大网格的写法）。实例矩阵变了没有运动向量；拾取、物理不认实例 |
| 材质绑不了存储缓冲 | 钩子里读不了 `array<T>`，只能经存储纹理 |
| 彩色 / 透射阴影 | 阴影图只有深度。半透明物体能投（`with_blended_shadows`）但影子是实的；焦散这类由接收面在 `Surface.transmitted` 里自己画（`volume_caustics`） |
| TAAU 的快速运动 | 高斯加权 + 包围盒夹历史，快速运动和大面积遮挡变化时会糊 / 有轻微残影；没有反应遮罩 |
| 面光源阴影 | 用的是平行投影，影子形状比真实的收敛 |
| 探针过渡 | 只混两个探针，三个交叠的角落会跳一下 |
| 环境捕获 | 跨面取样是最近邻；多次弹射要自己连捕几遍 |
| IES A/B 型 | 只有几何不变量守着，没有厂商真文件对照过 |
| 后处理的几处近似 | 没有 MRT 自发光通道（bloom 遮罩代替）、预通道材质只有系数没有贴图 |
| 海洋的 FFT 在 CPU 上 | 三个级联三条线程，128²（中 / 高画质）一帧约 3.5 ms（release；debug 下 3.9 ms——kcomponents 在 dev 下也开了 opt-level 3）。256²（超高）按 N² log N 估要四倍多，没实测过。「计算结果当贴图」已经有了（`StorageTexture::texture()`），挪到计算着色器只差做 |
| 海洋网格跟着相机连续平移 | 远处稀疏顶点会轻微「游」（短波在远处已淡出，基本看不出）；要彻底消除得换成按格对齐的几何 clipmap |
| 海面的常量还在调色板贴图里 | 参数槽现在有 16 个，放得下了，但 16 个 RGBA8 常量挪进参数槽是纯重构，没做 |
| 水线不贴浪 | 水下后处理的水线按相机处的一个平面切（法线取那一点的水面法线 + 一点抖动），镜头半截在水里时水线是直的，不跟着身边的浪起伏。要贴浪得让后处理效果能绑海面的波浪纹理（后处理没有材质那样的贴图槽） |
| 尾迹只有一块 | 尾迹图（256 格 × 320 米）跟着一个目标走（海岛场景跟着船），离它太远的物体不留尾迹。波动方程不带色散：V 字张角随船速变，不是开尔文尾迹恒定的 19.5° |
| 水下光柱和水面无关 | 光柱的明暗是程序化噪声沿阳光投到水面，不是真浪的焦散；看起来对，但和头顶的浪对不上 |
| 屏幕空间反射的老毛病 | 只反射屏幕里有的东西：船身一出屏幕，倒影跟着消失（贴边淡出了，但会看出来）；28 步 + 4 次二分，远处细物体（桅杆、缆绳）会漏 |
| 全景天空是静止的 | 程序化天空的云会飘会变形，全景照片模式的云不动 |
| 额外投影光源的阴影 | 主光源之外的聚光 / 点光共用 12 层、半分辨率的阴影图，按离相机远近分；没有按屏幕占比挑，也没有接触阴影和体积光（那两样只给主光源） |

### 物理

| 缺口 | 说明 |
|---|---|
| 软体双向耦合 | 布料推不动刚体；软体之间不互相碰 |
| 碎裂 | 八等分而不是按撞击点凸切割 |
| 多体关节 | ✅ 3D/2D 世界层四件套 + 场景同步（`Joint::multibody`，失败回落普通关节） |
| 多线程求解 | rapier 支持，但要接 ktask 而不是它自带的 rayon |

### 动画 / 粒子 / 音频

| 缺口 | 说明 |
|---|---|
| 形变的切线增量 | glTF 的 `TANGENT` 形变目标没读 |
| 粒子 | 粒子间碰撞、对刚体的反作用力、拖尾 / 网格粒子 |
| 音频 | 混音总线与效果器（混响 / 滤波 / 压限）、HRTF、多普勒、遮挡（射线是现成的） |

### 资源与导入

| 缺口 | 说明 |
|---|---|
| 资源依赖图 | 删资源不检查谁还在引用 |
| USD | 只读文本层（`.usda` / USDZ 里的 usda）；二进制 `.usdc`、`references` / `payload` 组合、UsdSkel 不支持 |
| Lottie | 渐变按首色标、没有蒙版 / 文字 / 图片层 / 表达式 / 位置的空间贝塞尔 |
| SVG | 没有 `<text>`、真渐变、虚线、裁剪 / 蒙版 |
| MaterialX | `specular` / `specular_color` 不支持（F0 按 IOR）、导入器没接噪声节点（噪声函数本身有了：`kshader::noise`）、只认 Standard Surface |
| LDraw | 条件边线（只在轮廓处出现的那种）没画 |
| VRML | 灯光、Viewpoint、Text、Inline、PROTO 实例化、ROUTE 动画 |
| GIF | 只取首帧 |
| 视频 | 要 `--features video`（C++ 解码器，见五、6）；`kvideo` 只认 MP4 + H.264；OpenH264 对 High 档支持不全（解不了的帧跳到下一个关键帧）；没有 WebM / Ogg、没有声音、解码在调用线程上同步做 |

### 界面与窗口

| 缺口 | 说明 |
|---|---|
| 文字整形（shaping） | 阿拉伯文、天城文这类要整形的文字排版错误。需要 rustybuzz（本机 cargo 缓存里没有，要联网加依赖） |
| 嵌套滚动区 | 有意不做：滚轮归属、scroll chaining 的规则真实游戏界面用不上 |
| 多行文本编辑 | 只有单行文本框。`tsl_editor` / `tsl_transpiler` 的代码放在文件里、保存热重载 |

### 结构（不加功能）

1. **`kscene` 1.3 万行**，塞了场景图 + 物理 / 音频同步 + 流式加载 + 序列化 + 地形 / 贴花 / 探针接线。
   可以学 Fyrox 拆 `fyrox-graph` 那样拆。纯重构，只有在改动开始互相绊脚时才值得做。
2. **`krender/src/lib.rs` 6600 行**：`render_frame` 已经拆过，但材质管线、贴图上传、绑定组、计算、放大这些还挤在一个文件里。
   按「材质管线 / 资源上传 / 帧编码」拆成模块，和拆 kscene 同理，改动互相绊脚时再做。
3. **CI 在 remote 上跑一次**。本机只有 Windows，Linux 上编不编得过没验证过。成本最低、价值最高的下一步。
   推之前先 `cargo fmt --all`：仓库里从没有过 rustfmt.toml，267 个文件和默认格式不一致（大半是 `examples/kengine/new`）。

## 三、最近核实已经完成的（别再列成缺口）

旧文档里写着「没有」、核实后已经有了的：

| 以前的说法 | 现在 | 在哪 |
|---|---|---|
| 没有角色控制器 | ✅ 3D + 2D，自动上台阶 / 斜坡限制 / 沿墙滑动 | `kphysics::CharacterController`、`physics_character` |
| 音频整段解码进内存 | ✅ 流式来源；**seek 到任意帧**（2026-10 修：以前只重建解码器不回绕读包器，循环播放的 BGM 放完一遍就没声） | `kaudio::StreamingSource` |
| 拾取没有 | ✅ 网格射线拾取（修过一个 Möller–Trumbore 符号 bug） | `Scene::pick` |
| 地形存不下来 / 单材质 | ✅ 序列化 + splat 多层 | `kterrain`、`terrain_splat.wgsl` |
| 渲染层 / 多相机做不到 | ✅ 渲染层、离屏相机、覆盖层相机（第一人称手臂不扎墙、FOV 独立） | `Camera::layers`、`CameraTarget::View` / `Overlay` |
| 一帧只有一盏灯能投影 | ✅ 主光源之外的聚光 / 点光也投影（独立阴影图集，按距离分层） | `Renderer::set_local_shadow_layers` |
| 没有锁定光标 | ✅ `Input::set_cursor_locked`，失焦自动放开；脚本 `Input.lockCursor()` | `kinput`、`free_camera_controller` |
| 没有补间 | ✅ `kanim::Tween` + 21 条缓动；`Scene::tween_*`；脚本 `await self.tween(...)` | `kanim`、`kscene/src/tween.rs` |
| 没有帧耗时数据 / 关不掉垂直同步 | ✅ F3 剖析面板（CPU 分段 + GPU 时间戳）；`KENGINE_PRESENT=immediate` | `klog::profile`、`kapp::Profiler` |
| GPU 资源只进不出 | ✅ 网格 / 贴图 / 绑定组 300 帧没用就回收 | `Renderer::set_eviction_frames` |
| 刚体 `add_force` 一直叠加 | ✅ 2026-10 修：rapier 的力是持续的，引擎从来没清，每步加一次浮力的船几秒后飞上天。现在每步之后清零（2D、3D 都修了），兑现文档里「只作用一步」 | `kphysics::PhysicsWorld::step` |
| 画面 bug 只能靠人眼 | ✅ 截图回归测试（无头渲染器，分块比对） | `tests/screenshots.rs` |
| 没有运动向量 / TAA / SSR | ✅ 运动向量、TAA、SMAA、SSAA、SSR、SSGI | `krender::effects` |
| 没有顶点钩子 | ✅ `material_vertex` | `krender/src/surface.wgsl` |
| 顶点钩子只进主通道（位移出来的形状没影子、SSAO 看不到） | ✅ 2026-10 修：写了 `material_vertex` 的材质，预通道和阴影的入口拼在它自己的着色器后面编，四条管线（阴影 / 预通道 × 静态 / 蒙皮）先过同一个钩子；阴影 pass 和主 pass 实例顺序一致，直接读主 pass 的逐对象数据。无头测试 `a_vertex_hook_moves_the_shadow_too`（关掉这套就挂，A/B 过）；`materials_displacementmap` 打开了阴影 | `krender/src/shadow_hooked.wgsl`、`prepass.wgsl` |
| 材质只有 4 个参数槽、2 个自定义贴图槽 | ✅ 2026-10 先扩到 8 个，同月再扩到 **16 个 `vec4`**（程序化木纹 7 个叠在物理材质的 5 个上）、4 张二维贴图 + 纹理数组 + 三维纹理。逐对象数据随之变大（没量对帧时的影响）；主着色器片元阶段的采样纹理超过 WebGPU 基线，建设备时按适配器要上限（`SAMPLED_TEXTURES_PER_STAGE`）。WGSL 里用别名 `MaterialParams`，两条测试守着别名长度和纹理数 | `kmaterial::standard`、`krender::required_limits` |
| 只有一套 UV | ✅ `Vertex::uv1` | `kmesh` |
| 没有子网格材质组 | ✅ `MeshGroup` + `Node::set_materials` | `kmesh`、`kscene` |
| 点光 / 聚光的影子按方向光算 | ✅ 立方体阴影 / 透视阴影（2026-09 修的真 bug） | `klight::cascade` |
| 没有立方体相机 | ✅ 场景捕获成环境图 | `Renderer::capture_environment`、`lightprobe_cubecamera` |
| 背景不能用 HDR | ✅ HDR 背景，可模糊（`set_background_blurriness`） | `krender/src/sky.wgsl` |
| 没有真 `.hdr` 文件被解码过 | ✅ 十几个 three.js 样本 HDR 在例子里天天用 | `examples/threejs/textures/equirectangular` |
| UI 在 DPI ≠ 1 上没验过 | ✅ 2026-10 验了也修了：字形按物理像素光栅化、对齐像素格 | `kui` |
| 动画循环事件要自己探测 | ✅ `Animator::events()`：`Looped { count }` / `Finished` | `kanim` |
| 资源包不压缩 | ✅ `PackWriter::compressed()`，逐项 deflate，旧包照读 | `kasset::pack` |
| JS 没有 async / 计时器 / 信号 | ✅ `await wait()`、`setTimeout`、`node.connect / toSignal`、`console`、旋转 / 树 API | `kscript` |
| 没有本地化 | ✅ 2026-10：`klocale`——`.ftl` 字符串表（键、`{ $变量 }`、注释、续行、属性）、`zh-CN` → `zh` → 回退语言的查找链、找不到显示键名；`tr!("key", name = v)`，脚本 `tr("key", {name: v})` / `language()`；换语言下一帧界面就变（即时模式每帧重新布局）。例子 `localization`（中 / 英 / 日，日文表故意缺条目演示回退），进了例子截图回归 | `klocale`、`kscript` |
| 不带 `--release` 跑海洋只有 20 帧 | ✅ 2026-10 修：工作区成员不吃 `[profile.dev.package."*"]`，海面 FFT 在 opt-level 0 下一帧 37 ms。kcomponents、krender、kscene、kanim 单独开 opt-level 3 后 debug 下高画质 78 帧 | 根 `Cargo.toml` |
| 海面远处一片发颤的细花纹 | ✅ 2026-10 修：按像素在海面上的覆盖（`dpdx`/`dpdy`）淡出各级浪，不按距离；远处暗纹是把浪的背坡误判成「从水下看」，改成整帧按相机在不在水里判断 | `ocean.wgsl` |
| 海面没有物体倒影 / 尾迹只是白沫 / 浮标只能多点 | ✅ 屏幕空间反射、波动方程尾迹（`WakeMap::emit`）、单点浮力（`Buoyancy::point`） | `kcomponents::ocean` |
| 根运动只在 `kanim` 里 | ✅ 2026-10：`Scene::enable_root_motion(模型, 骨骼名, 竖直?)` / `root_motion_delta`（换到世界空间，含模型缩放）/ `apply_root_motion`；`Plugin::post_animate` 里调。有角色控制器时把增量喂给 `move_character`。测试 2 条 + 例子 `root_motion` | `kscene/src/root_motion.rs` |
| 没有手柄 | ✅ 2026-10：`kinput` 接 gilrs（纯 Rust）。`Gamepads` 存按键 / 模拟量（摇杆径向死区 0.15 并重新拉伸到 0..1，扳机 0.05），`Binding::Gamepad(按键)` 进动作映射，`bind_axis_analog` 让左摇杆和 A / D 驱动同一个轴（键盘按着以键盘为准，`axis_vector` 半推就是半速）；kapp 每帧 `GamepadPoller::poll`。`physics_character` 能用手柄玩。测试 4 条（没插手柄验真机——见「只能靠人看的」） | `kinput/src/gamepad.rs` |
| 没有导航寻路 | ✅ 2026-10：新 crate `knav`（零依赖）。2.5D 格子：每格往下打射线取地面，坡度、台阶高度按角色控制器的参数判；离边缘不到代理半径的格子腐蚀掉（倒角距离变换）；八方向 A*（不切墙角，代价含高差和区域倍数）+ 视线拉直；`block` / `set_cost` 动态改。`Scene::bake_nav_grid` 只认静态碰撞体和地形（角色自己、动态箱子不算）。120 × 120 格烘 3 ms，一次寻路 0.3 ms。测试 10 条 + 例子 `navigation` | `knav`、`kscene/src/nav.rs` |
| 环境 HDR 里的太阳在粗糙面上反出亮斑 | ✅ 2026-10 修：预滤波改成过滤的重要性采样（Křivánek & Colbert 2008）——源图先做一串 2×2 平均的 mip，每条 GGX 采样线按概率密度算出它代表的立体角，到对应粗细的 mip 去采。一个 2×2 像素、十万倍亮的太阳，相邻像素亮度比从 1999 倍降到 7.5 倍（测试 `a_tiny_blinding_sun_does_not_leave_hot_spots`） | `kpbr/src/prefilter.rs` |
| Mixamo 式「一个动作一个文件」用不了 | ✅ 2026-10：`Model::retarget_animations_from(&动作模型)` 按节点名把剪辑换到角色身上，`Scene::add_animations(角色, 剪辑)` 追加进已实例化角色的播放器 | `kgltf`、`kscene` |
| 没有实例化 | ✅ 2026-10：`Node::with_instances(Vec<Instance>)`，一个实例 = 局部矩阵 + 颜色（乘进基础色）+ 一个自定义 `vec4`（钩子里的 `instance_data`）。渲染器一个节点一个绘制项、600 字节的对象数据存一份；每个 GPU 实例一个 8 字节的槽（对象下标 + 实例下标），实例数组从场景原样拷进显存（`Instance` 是 `Pod`，96 字节）。主 pass、预通道、遮罩、带钩子和不带钩子的阴影都按槽展开；片元阶段不读实例缓冲（顶点阶段平着传下去）。同一片方块：一万个 0.54 ms（逐节点 3.43）、五万个 1.36 ms（逐节点 5.79），`benches/render.rs` 的 `render/instanced/*`。测试：无头渲染（位置 / 颜色 / 数据各自生效、一次绘制）、实例化方块和普通方块的画面与影子逐像素一致、阴影逐级剔除按对象判一次；例子 `instancing`（十万个方块、25 个节点、2 次绘制），`meadow` 的草也换成了实例 | `kscene/src/instance.rs`、`krender`（`geometry.wgsl` 的 `instance_object`） |
| 没有风格化草地 | ✅ 2026-10：`kcomponents::meadow::Meadow`，照 three-stylized 移植（地形噪声逐式一致）。草叶是实例：一份 9 个顶点的叶片网格，按 10 米切块、每块一个实例化节点（按块剔除），风的相位取实例矩阵的原点（和原版的 `instanceMatrix` 同一个写法）；默认那块地 1.5 万根。`blade_mesh` + `grass_instances` + `SurfaceSampler` 能把草撒在任意网格上。测试 12 条（含着色器对引擎编译）+ 例子 `meadow` | `kcomponents/src/meadow/` |
| 没有三维纹理 | ✅ 2026-10：`Texture::volume` + `custom_texture_3d`（层间三线性插值）。无头测试 `a_volume_texture_interpolates_between_layers` | `ktexture`、`krender` |
| 计算结果读回才能当贴图 | ✅ `StorageTexture::texture()`：渲染器直接绑计算着色器写的那块显存 | `krender::compute` |
| 只有不透明 / alpha 两种混合 | ✅ `BlendMode::Additive`（预乘管线，不多编）；半透明写深度、半透明投影都可以按材质开 | `kmaterial`、`krender` |
| 光照算完不能再改 | ✅ 第五个钩子 `material_output`（three.js 的 `outputNode`） | `krender/src/surface.wgsl` |
| 材质贴图没有 mip | ✅ CPU 上生成 mip 链（sRGB 先换线性），各向异性 1–16 | `ktexture::generate_mips`、`Sampler::anisotropy` |
| 渲染分辨率等于窗口 | ✅ `PostSettings::render_scale` + FSR1 / TAAU / 双线性放大，UI 保持窗口分辨率 | `krender/src/fsr.wgsl`、`taau.wgsl` |
| 没有噪声函数 / 不能用 GLSL | ✅ `kshader::noise`（和 TSL 的 MaterialX 噪声逐值一致）、`kshader::glsl` | `kshader` |
| 没有视频 | ✅ `kvideo`（见上，限 MP4 / H.264） | `kvideo` |
| 球面贴图东西颠倒 | ✅ 2026-10 修：`Mesh::sphere` 的参数化改成和 three.js 一样（u = 0 在 −X） | `kmesh` |

## 四、只能靠人看的

所有「完成」都是测试过 + 校验过 + 数值对得上 + 截图和参考图比过。截图比不出来的：

- 阴影级联的接缝、贴花边缘的 z-fighting、探针间移动时反射的跳变
- 2D / 3D 物理的手感（堆得稳不稳、角色贴墙走顺不顺）
- 音频的空间感、流式播放在卡顿时会不会断音
- 文字在各种字号下的观感（现在截图里清楚了，但只看过雅黑）
- 视频播放是否卡顿 / 掉帧（解码在主线程同步做），TAAU 在快速转镜头时的拖影，FSR1 / TAAU 和双线性的清晰度差别
- 后处理在 `render_scale < 1` 时的表现（SSAO、SSR、景深按渲染分辨率跑，没逐个在低分辨率下看过）
- 手柄真机：本机没插手柄，测试都是直接往 `Gamepads` 里喂事件。gilrs 的按键 / 轴名到我们的映射、Xbox / PS / Switch 手柄的南键是不是都对上 `South`、摇杆 y 的正负号，都要插一个真手柄跑 `physics_character` 才知道

截图工具现在**拷的是交换链**（UI 画完之后），以前拷的是后处理链，截图里没有 UI——
UI 的毛病因此长期没被发现。不支持 `COPY_SRC` 的后端会退回旧路径。

## 五、定下来的取舍

写下来免得反复纠结：

1. **不做 ECS。** 场景图路线已定。
2. **不做编辑器。** 连带：UI 只做 HUD / 菜单 / 调试浮层，反射系统已删。
3. **不做多后端图形抽象。** wgpu 已经是抽象层。
4. **UI 布局不自己写。** 接 taffy。
5. **每个第三方引擎关在一个 crate 里**（见上表）。
6. **纯 Rust 工具链。** `cargo build` 不需要外部工具链——选 rapier 不选 Jolt、cpal 不选 OpenAL、boa 不选 QuickJS 的唯一理由。
   **唯一的例外关在特性后面：视频解码**。OpenH264 是 C++（随 crate 用 `cc` 编译，要 MSVC / gcc），没有成熟的纯 Rust
   H.264 解码器。所以 `kvideo` 的 `h264` 特性默认关（关着时 MP4 拆包照常编译，`VideoPlayer::open` 报错），
   根 crate 的 `video` 特性打开它；两个视频例子 `required-features = ["video"]`。`cargo build --workspace` 依赖树里没有 openh264。
7. **2D 不走节点组件。** 立即模式精灵。
8. **绘制顺序固定**：3D → 粒子 → 精灵 → 调试线 → UI。要画在界面上面就当界面画（`Ui::image`）。
9. **性能数字必须有 benches 撑着**，而且是同一轮里和不受改动影响的那条 bench 比出来的比值（这台机器两次运行之间能差 2.6 倍）。
10. **Windows 主线程栈开到 8 MB**（根目录 `build.rs`）。boa 解析 JS 递归极深，1 MB 不够；用脚本的游戏要在自己的 `build.rs` 里照抄（见 `kapp` 文档）。

## 六、做事的规矩（都是踩出来的）

- **「没有」要交叉验证。** 用一次 grep 判断功能不存在，翻车过四次（音频流、地形、拾取、seek）。读模块目录、跑测试、写个探针程序，至少两种。
- **装上了不等于生效了。** 级联阴影三级用同一个矩阵、地形不投影、点光影子按方向光算、截图里没有 UI——测试全绿，画面不对。能读回像素就读回像素，能截图就截图，截图要和参考比。
- **CPU / WGSL 结构体大小对不上不报错**，只出垃圾。`uniform_sizes_match_wgsl_layout` 拦住过很多次，新加字段先跑它。
- **颜色空间要写明。** UI 主题曾把 sRGB 的数当线性用，面板成了中灰；引擎里颜色一律线性，从设计稿来的用 `srgb(0x……)` 换。
- **调试构建的栈很浅。** taffy 递归 20 层、boa 解析三层嵌套调用都能顶破 Windows 的 1 MB——表现是进程直接退出，没有 panic。
- **调试构建也要量帧率。** 用户多半是 `cargo run`（不带 `--release`）。工作区成员不吃 `"*"` 的优化级别，数值密集的成员（FFT、渲染器、场景遍历）要单独列进 `[profile.dev.package.*]`。
- **截图要用真窗口分辨率看一遍。** `KENGINE_SCREENSHOT_WIDTH` 缩小后的图把走样、摩尔纹、细线全抹掉了——海面远处的花纹在 960 宽的截图里看不出来，用户 2400 宽的窗口里一眼就是。
- **细节按像素大小淡出，不按距离。** 同样 500 米，俯视时一个像素盖住几十厘米，平视时盖住十几米；只按距离淡出，相机一低就走样。
- **参考截图是最好的测试。** three.js 的每个例子都带一张参考图（`examples/threejs/screenshots/`）。地球的经度、地形的形状、
  TSL `a.step(b)` 的参数顺序，全是对着参考图比出来的；单测和「看着像」都抓不到。
- **组合起来才出错的常量最难查。** Neutral 色调映射和「纯色背景反算」各自都对，碰上白背景反算出 256，进 Bloom 整屏发糊。
  看到「整体糊 / 整体偏色」先二分开关（色调映射、Bloom、抗锯齿），别先怀疑几何。
- **重编译会把 H: 盘拖掉。** 两次掉盘都发生在大规模并行编译时（一次满盘，一次还有 143 GB）。报 `os error 433` /
  `STATUS_IN_PAGE_ERROR` 时停下来，别重试；编译用 `-j 4`。
- **一帧只有一个答案的事别逐像素猜。** 「从水下还是水上看海面」逐像素按法线朝向判断，远处浪的背坡全被当成水下；CPU 算一次传进去就对了。
