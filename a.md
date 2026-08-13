渲染系统（Rendering） — 图形渲染管线、光照、材质/着色器、后处理、粒子特效。是引擎最核心也最消耗资源的部分。
渲染管线

System提供内存管理（ ）、线程（NiThread）和文件 I/O（NiFile）的基础抽象层
Mesh  利用基于数据流的架构NiDataStream进行灵活顶点和索引管理的几何系统

物理系统（Physics） — 刚体动力学、碰撞检测、碰撞响应、布娃娃、射线检测。用Jolt。




动画系统（Animation） — 骨骼动画、蒙皮、状态机、动画混合（blend tree）、IK、Morph Target。

音频系统（Audio） — 音效播放、3D 空间音频、混音、音频流。常见底层如 FMOD、Wwise、OpenAL。

输入系统（Input） — 键鼠、手柄、触屏、体感设备的事件采集与映射。

脚本/逻辑系统（Scripting） — 游戏逻辑层，如js脚本语言的集成，提供游戏对象行为定义和事件处理。

场景管理（Scene / Scene Graph） — 场景树、实体组织、空间划分（八叉树、BVH）、可见性剔除。

资源管理（Asset / Resource Management） — 资源加载、序列化、打包、内存管理、异步流式加载。


多任务执行：

底层执行器：多线程任务池
bevy_tasks 提供任务池（基于 async-executor + 类似 work-stealing 的调度），每个"就绪" System 被包装成一个 async task 丢进线程池，线程从队列里认领任务执行,执行完毕后通过完成通道（completion channel）通知调度器，再触发下游依赖的 System 变为就绪状态。



GNg:: 包含了2D图形与23D都通用的相关功能
最常用的 sIMAGE, cVIEW2D, cTEXT_ART 和 ImageLoad, ImageDraw等

GN3d:: 包含了3D图形,渲染,3D编辑控制等相关功能
最常用的 cSCENE cMODEL cLIGHT cCAMERA cOBJ3D

GNu:: 包含了界面 布局等相关功能
最常用的 cSCROLL_RECTEX, cSELECT_LIST_TEXT, cTEXT_INPUT, cSIM_DIALOG NearCx, NearTy, Near???等

GN:: 包含了常规的通用处理计算所用到的相关功能 例如: 时间 多线程 操作控制 输入输出 运行记录等
最常用的 Key[GK_A] 按键状态 Mouse[GM_LEFT] 鼠标按键状态 RunRec 运行记录 TimeGet??????时间相关函数等

GNio:: 包含了一些特殊不通用的信号输入输出功能等
最常用的 InFv[GI_WX] 重力感应器

GNf:: 包含了文件操作相关的功能
最常用的 sFILE Open Close Read Write 等类以C语言的函数 但这个能方便地定位到读取压缩包内文件或网上服务器文件

GNn:: 包含了网络通讯等相关的功能
最常用的 cCLIENT_HTTP cDTP cSOC_TCP cSOC_UDP

GNa:: 包含了声音处理播放等相关的功能
最常用的 sSOUND cSOUND_SRC cBGM_MUSIC

GNm:: 包含了视频播放等多媒体相关功能
最常用的 cVIDEO

GNd:: 包含数据加密 转码, 转换为引擎专属格式等相关功能

另外还有比如 GNg_?????? GN3d_?????? GN_?????? 这样命名的函数或类, 这些一般不是应用层常用的 但也可以使用的功能