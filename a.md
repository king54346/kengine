渲染系统（Rendering） — 图形渲染管线、光照、材质/着色器、后处理、粒子特效。是引擎最核心也最消耗资源的部分。

System提供内存管理（ ）、线程（NiThread）和文件 I/O（NiFile）的基础抽象层
Mesh 

物理系统（Physics） — 刚体动力学、碰撞检测、碰撞响应、布娃娃、射线检测。用Jolt。

动画系统（Animation） — 骨骼动画、蒙皮、状态机、动画混合（blend tree）、IK、Morph Target。

音频系统（Audio） — 音效播放、3D 空间音频、混音、音频流。常见底层如 FMOD、Wwise、OpenAL。

输入系统（Input） — 键鼠的事件采集与映射。

脚本/逻辑系统（Scripting） — 游戏逻辑层，如js脚本语言的集成，提供游戏对象行为定义和事件处理。

场景管理（Scene / Scene Graph） — 场景树、实体组织、空间划分（八叉树、BVH）、可见性剔除。

资源管理（Asset / Resource Management） — 资源加载、序列化、打包、内存管理、异步流式加载。


多任务执行：

底层执行器：多线程任务池
ktask 提供任务池（基于 async-executor + 类似 work-stealing 的调度），每个"就绪" System 被包装成一个 async task 丢进线程池，线程从队列里认领任务执行,执行完毕后通过完成通道（completion channel）通知调度器，再触发下游依赖的 System 变为就绪状态。

kmesh 提供 Mesh 资源的加载、序列化、打包、内存管理、异步流式加载。Mesh 资源包括顶点数据、索引数据、法线、UV 坐标等。

ksprite 提供 Sprite 资源的加载、序列化、打包、内存管理、异步流式加载。Sprite 资源包括纹理、动画帧、碰撞体等。

kmaterial 提供 Material 资源的加载、序列化、打包、内存管理、异步流式加载。Material 资源包括着色器、纹理、参数等。

klight 提供 Light 资源的加载、序列化、打包、内存管理、异步流式加载。Light 资源包括点光源、聚光灯、环境光等。

kinput 提供 Input 资源的加载、序列化、打包、内存管理、异步流式加载。Input 资源包括键盘映射、手柄映射、触屏映射等。

kgltf 提供 GLTF 资源的加载、序列化、打包、内存管理、异步流式加载。GLTF 资源包括模型、材质、动画等。

kpbr 提供 PBR 资源的加载、序列化、打包、内存管理、异步流式加载。PBR 资源包括金属度、粗糙度、环境贴图等。

kapp App 生命周期和插件系统。App、Plugin、Schedule 阶段

kutils — 通用工具集

kasset — 资源加载与追踪（AssetServer、Handle<T>）。异步加载各种资源

kcamera — 相机与可见性管理（View、culling）

kshader — Shader 加载与处理（依赖 naga）

krender — 核心渲染抽象（RenderApp、RenderGraph、提取/准备/队列的渲染阶段）。

kwinit — 基于 winit 的窗口后端实现（WinitPlugin），负责事件循环]()



┌──────────────────────────────┬───────────────────────────────────┐
│            已完成            │               空缺                │
├──────────────────────────────┼───────────────────────────────────┤
│ kcore / kmath / klog / ktask │ kpbr、klight、ksprite             │
├──────────────────────────────┼───────────────────────────────────┤
│ kasset / kinput / ktexture   │ kapp（Schedule 阶段）             │
├──────────────────────────────┼───────────────────────────────────┤
│ kshader / kmaterial          │ 物理（Jolt）、动画、音频、JS 脚本 │
├──────────────────────────────┼───────────────────────────────────┤
│ kmesh / kgltf / kcamera      │ 空间划分（八叉树/BVH）            │
└──────────────────────────────┴───────────────────────────────────┘
