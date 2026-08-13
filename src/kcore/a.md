pool：存储层，负责“把对象放进一个可复用的对象池里，并用安全句柄访问”
reflect：认知层，负责“运行时知道一个对象有什么字段、是什么类型、怎么遍历”
visitor：读写层，负责“把对象树读/写成二进制或文本（ASCII）”

pool（对象池 + 句柄管理）
    关键文件：src/core/src/pool/mod.rs、src/core/src/pool/handle.rs
    核心点是 Handle<T>（通常包含 index + generation）：
    index 指向池里的槽位
    generation 防止“旧句柄误用”（槽位复用后，旧 handle 自动失效）
    适合场景：游戏实体、资源对象等大量创建/删除但希望稳定引用的对象。
    你可以把它当“带防呆机制的 arena”。

reflect（运行时反射）
    关键文件：src/core/src/reflect/mod.rs、src/core/src/reflect/field.rs
    提供 Reflect trait 和一系列 FieldRef / FieldMut / TypeInfo 能力：
    运行时拿到类型名、字段列表、字段元信息
    可以做字段级遍历、编辑器面板、调试器展示、通用工具处理
    价值：避免为每个类型手写“怎么枚举字段”。

visitor（统一序列化/反序列化访问）
    关键文件：src/core/src/visitor/mod.rs、src/core/src/visitor/reader/*、src/core/src/visitor/writer/*
    Visit trait 是核心抽象：对象通过 visit(...) 把自己交给访问器读写。
    内部是树形节点结构（VisitorNode），支持：
    读：Binary/ASCII -> 树 -> 对象
    写：对象 -> 树 -> Binary/ASCII
    还处理 region 作用域、错误类型、版本兼容等通用序列化问题


对象存放在 pool，外部持有 Handle<T> 引用对象。
需要通用处理（编辑器/存档）时，用 reflect 拿到字段与类型信息。
需要持久化或加载时，通过 visitor 的 Visit 过程把数据写出/读回。