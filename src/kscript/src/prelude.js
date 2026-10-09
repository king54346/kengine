// kscript 的 JS 前奏：把扁平的原生函数包成 GDScript 那样的对象接口。
//
// 为什么放在 JS 里而不是 Rust 里：getter/setter、类、链式方法在 JS 是母语，
// 用 boa 的对象 API 去拼同样的东西要多写十倍代码，还更难读。
// Rust 那边只留最小的桥（`__k.*`），所有手感都在这一层。
//
// 与 GDScript 的**唯一无法弥合的差别**：JavaScript 没有运算符重载，
// 所以向量只能写 `a.add(b)`，写不出 `a + b`。

class Vector3 {
    constructor(x, y, z) {
        this.x = x || 0;
        this.y = y || 0;
        this.z = z || 0;
    }

    add(o) { return new Vector3(this.x + o.x, this.y + o.y, this.z + o.z); }
    sub(o) { return new Vector3(this.x - o.x, this.y - o.y, this.z - o.z); }
    mul(s) { return new Vector3(this.x * s, this.y * s, this.z * s); }
    neg() { return new Vector3(-this.x, -this.y, -this.z); }
    dot(o) { return this.x * o.x + this.y * o.y + this.z * o.z; }

    cross(o) {
        return new Vector3(
            this.y * o.z - this.z * o.y,
            this.z * o.x - this.x * o.z,
            this.x * o.y - this.y * o.x,
        );
    }

    length() { return Math.sqrt(this.dot(this)); }
    lengthSquared() { return this.dot(this); }
    distanceTo(o) { return this.sub(o).length(); }

    normalized() {
        const n = this.length();
        // 零向量归一化会得到 NaN，一路传进场景就是物体无声消失。
        return n > 1e-9 ? this.mul(1 / n) : new Vector3(0, 0, 0);
    }

    lerp(o, t) { return this.add(o.sub(this).mul(t)); }
    clone() { return new Vector3(this.x, this.y, this.z); }
    toString() { return "(" + this.x + ", " + this.y + ", " + this.z + ")"; }
}

Vector3.ZERO = () => new Vector3(0, 0, 0);
Vector3.ONE = () => new Vector3(1, 1, 1);
Vector3.UP = () => new Vector3(0, 1, 0);
Vector3.DOWN = () => new Vector3(0, -1, 0);
Vector3.RIGHT = () => new Vector3(1, 0, 0);
Vector3.LEFT = () => new Vector3(-1, 0, 0);
// 本引擎（和 glTF）的约定：前方是 -Z。
Vector3.FORWARD = () => new Vector3(0, 0, -1);
Vector3.BACK = () => new Vector3(0, 0, 1);

// 绑定到节点某个向量字段的代理。
//
// 存在的理由只有一句：让 `self.position.y += delta` 能写进场景。
// 直接返回一个普通 Vector3 的话，`.y += 1` 改的是那个临时副本，
// 写完就被丢掉——脚本看起来在动，物体纹丝不动，而且不报错。
class BoundVector3 {
    // 用**私有字段**（`#`）存内部账本，而不是普通属性。
    //
    // 目的和原来那句 `Object.defineProperty(this, "_id", { enumerable: false })`
    // 一样——脚本作者不该看见它，`JSON.stringify` 与 `for...in` 也不该带上它
    // （`_save()` 里顺手存了个节点的话，存档里会多出一串没有意义的下标）。
    // 区别在于代价：`defineProperty` 每次都要走一遍属性描述符的完整流程，
    // 而 `self.position.y += dt` **每写一次就新建一个 BoundVector3**，
    // 这条是脚本里最常走的路。私有字段是类的内建槽位，没有那套开销。
    //
    // 这条路径到底多贵，`benches/script.rs` 里有一档 `raw_bridge` 做对照：
    // 它绕开整个包装层直接捅桥，两者的差值就是包装的价钱。
    #id;
    #field;

    constructor(id, field) {
        this.#id = id;
        this.#field = field;
    }

    get x() { return __k.getComponent(this.#id, this.#field, 0); }
    set x(v) { __k.setComponent(this.#id, this.#field, 0, v); }
    get y() { return __k.getComponent(this.#id, this.#field, 1); }
    set y(v) { __k.setComponent(this.#id, this.#field, 1, v); }
    get z() { return __k.getComponent(this.#id, this.#field, 2); }
    set z(v) { __k.setComponent(this.#id, this.#field, 2, v); }

    // 下面这些和 Vector3 同名同义，直接借它的实现，省得两处维护。
    add(o) { return this.clone().add(o); }
    sub(o) { return this.clone().sub(o); }
    mul(s) { return this.clone().mul(s); }
    dot(o) { return this.clone().dot(o); }
    cross(o) { return this.clone().cross(o); }
    length() { return this.clone().length(); }
    lengthSquared() { return this.clone().lengthSquared(); }
    distanceTo(o) { return this.clone().distanceTo(o); }
    normalized() { return this.clone().normalized(); }
    lerp(o, t) { return this.clone().lerp(o, t); }
    clone() { return new Vector3(this.x, this.y, this.z); }
    toString() { return this.clone().toString(); }
}

const FIELD_POSITION = 0;
const FIELD_SCALE = 1;

// 一个场景节点。属性读写**立刻**作用在场景上。
class Node {
    // 私有字段，理由同 `BoundVector3`：`self` 每取一次就新建一个 Node。
    #id;
    #position;
    #scale;

    constructor(id) {
        this.#id = id;
        __nodeIds.set(this, id);
    }

    get name() { return __k.getName(this.#id); }

    get valid() { return __k.isValid(this.#id); }

    // 代理对象只记「哪个节点、哪个字段」，没有自己的状态，所以每个 Node 缓存一份。
    // `self` 现在是每个实例常驻的同一个对象，`self.position.y += dt` 这条
    // 最常走的路于是不再每次都新建一个代理。
    get position() { return this.#position ??= new BoundVector3(this.#id, FIELD_POSITION); }
    set position(v) { __k.setVec(this.#id, FIELD_POSITION, v.x, v.y, v.z); }

    get scale() { return this.#scale ??= new BoundVector3(this.#id, FIELD_SCALE); }
    set scale(v) { __k.setVec(this.#id, FIELD_SCALE, v.x, v.y, v.z); }

    // 世界坐标是每帧算出来的派生值，只读。
    get globalPosition() {
        const v = __k.getGlobalPosition(this.#id);
        return new Vector3(v[0], v[1], v[2]);
    }

    // 节点朝向的方向（世界空间，已归一化）。约定同 glTF：前方是 -Z。
    //
    // `lookAt` 的读侧：转过去之后要「朝着那边走」的话得能问出方向来。
    get forward() {
        const v = __k.getForward(this.#id);
        return new Vector3(v[0], v[1], v[2]);
    }

    get visible() { return __k.getVisible(this.#id); }
    set visible(v) { __k.setVisible(this.#id, !!v); }

    get linearVelocity() {
        const v = __k.getLinvel(this.#id);
        return new Vector3(v[0], v[1], v[2]);
    }

    translate(v) { __k.translate(this.#id, v.x, v.y, v.z); return this; }
    rotateY(a) { __k.rotateY(this.#id, a); return this; }
    lookAt(target) { __k.lookAt(this.#id, target.x, target.y, target.z); return this; }

    applyImpulse(v) { __k.applyImpulse(this.#id, v.x, v.y, v.z); return this; }
    setLinearVelocity(v) { __k.setLinvel(this.#id, v.x, v.y, v.z); return this; }

    // ── 动画 ──
    //
    // 名字取剪辑名，和 glTF 里导出的一致。找不到时返回 false 而不是抛异常，
    // 美术改个剪辑名不该让整个脚本停掉。
    playAnimation(name) { return __k.playAnimation(this.#id, String(name)); }

    // 补间：`property` 是 "position" / "rotation"（欧拉角，弧度）/ "scale"，
    // 在 `duration` 秒里按 `ease`（"linear"、"easeOutCubic"、"easeOutBack"…）过渡到 `target`。
    // 返回一个走完时兑现成 true 的 Promise；被顶替、被取消或节点被删时兑现成 false。
    //
    //     await self.tween("position", new Vector3(0, 3, 0), 0.5, "easeOutCubic");
    //     self.tween("scale", 1.2, 0.2, "easeOutBack");   // 缩放可以给一个数
    //
    // 同一节点同一属性只留最新的一段：连点两次，第二段从当前位置接着走。
    tween(property, target, duration, ease) {
        const v = typeof target === "number" ? { x: target, y: target, z: target } : target;
        const id = __k.tween(this.#id, String(property), v.x, v.y, v.z, Number(duration), ease === undefined ? "linear" : String(ease));
        if (id === 0) return Promise.resolve(false);
        return new Promise(resolve => {
            // 每帧看一眼还在不在跑。走完的那一帧最后一次写入已经落地了。
            const poll = () => {
                if (__k.tweenActive(id)) __addTimer(0, poll, false);
                else resolve(__k.tweenFinished(id));
            };
            __addTimer(0, poll, false);
        });
    }
    stopAnimation() { __k.setAnimationPlaying(this.#id, false); return this; }
    resumeAnimation() { __k.setAnimationPlaying(this.#id, true); return this; }
    get animationPlaying() { return __k.isAnimationPlaying(this.#id); }
    set animationSpeed(v) { __k.setAnimationSpeed(this.#id, v); }

    // ── 粒子 ──
    startParticles() { __k.setParticlesPlaying(this.#id, true); return this; }
    stopParticles() { __k.setParticlesPlaying(this.#id, false); return this; }
    set emissionRate(v) { __k.setEmissionRate(this.#id, v); }
    burst(count) { __k.burstParticles(this.#id, count === undefined ? 1 : count); return this; }
    get particleCount() { return __k.particleCount(this.#id); }

    // ── 音频 ──
    playSound() { __k.playSound(this.#id); return this; }
    stopSound() { __k.stopSound(this.#id); return this; }
    set volume(v) { __k.setSoundGain(this.#id, v); }
    set pitch(v) { __k.setSoundPitch(this.#id, v); }
    set soundLooping(v) { __k.setSoundLooping(this.#id, !!v); }

    // 名字取自 GDScript 的 `queue_free()`：删除在本次操作里立即生效。
    queueFree() { __k.queueFree(this.#id); }

    // ── 旋转 ──
    //
    // 欧拉角和 Godot 一样：弧度，YXZ 顺序。偏航在最外层，改俯仰不会把偏航带歪。
    get rotation() { const r = __k.getRotation(this.#id); return new Vector3(r[0], r[1], r[2]); }
    set rotation(v) { __k.setRotation(this.#id, v.x, v.y, v.z); }
    get rotationDegrees() { return this.rotation.mul(180 / Math.PI); }
    set rotationDegrees(v) { this.rotation = new Vector3(v.x, v.y, v.z).mul(Math.PI / 180); }
    get quaternion() { const q = __k.getQuat(this.#id); return new Quaternion(q[0], q[1], q[2], q[3]); }
    set quaternion(q) { __k.setQuat(this.#id, q.x, q.y, q.z, q.w); }

    rotateX(a) { __k.rotateAxis(this.#id, 1, 0, 0, a, true); return this; }
    rotateZ(a) { __k.rotateAxis(this.#id, 0, 0, 1, a, true); return this; }
    // 绕自身的轴转。
    rotate(axis, angle) { __k.rotateAxis(this.#id, axis.x, axis.y, axis.z, angle, true); return this; }
    // 绕父空间（没有父节点时就是世界）的轴转。
    rotateGlobal(axis, angle) { __k.rotateAxis(this.#id, axis.x, axis.y, axis.z, angle, false); return this; }

    // 世界空间的三根轴（已归一化）。`forward` 在上面，是 -Z。
    get right() { const a = __k.getAxes(this.#id); return new Vector3(a[0], a[1], a[2]); }
    get up() { const a = __k.getAxes(this.#id); return new Vector3(a[3], a[4], a[5]); }

    // 写世界坐标：换算成父空间里的局部坐标再写。
    set globalPosition(v) { __k.setGlobalPosition(this.#id, v.x, v.y, v.z); }

    // 局部 ↔ 世界。点会平移，方向（`Global/LocalDirection`）只转不移。
    toGlobal(p) { const v = __k.toGlobal(this.#id, p.x, p.y, p.z, true); return new Vector3(v[0], v[1], v[2]); }
    toLocal(p) { const v = __k.toLocal(this.#id, p.x, p.y, p.z, true); return new Vector3(v[0], v[1], v[2]); }
    toGlobalDirection(d) { const v = __k.toGlobal(this.#id, d.x, d.y, d.z, false); return new Vector3(v[0], v[1], v[2]); }
    toLocalDirection(d) { const v = __k.toLocal(this.#id, d.x, d.y, d.z, false); return new Vector3(v[0], v[1], v[2]); }

    // ── 节点树 ──
    //
    // 挂在场景根下的节点 `parent` 是 null：根不是一个能动的节点。
    get parent() { const id = __k.getParent(this.#id); return id < 0 ? null : new Node(id); }
    get children() { return __k.getChildren(this.#id).map(id => new Node(id)); }

    // 按名字找子节点；`recursive` 为真时找整棵子树（深度优先，先到先得）。
    findChild(name, recursive) {
        const id = __k.findChild(this.#id, String(name), recursive === undefined ? true : !!recursive);
        return id < 0 ? null : new Node(id);
    }

    // 相对路径，和 Godot 一样：`"Arm/Hand"`、`"../Sibling"`、`"/Level/Door"`（从场景根）。
    // 只写一个名字、在子节点里又找不到时，退回按名字全局查找——
    // 这是以前 `getNode` 的行为，老脚本不受影响。
    getNode(path) { return __resolvePath(this, String(path)); }

    // 改挂到另一个节点下（null = 场景根）。`keepGlobal` 默认为真：看起来原地不动。
    reparent(parent, keepGlobal) {
        const target = parent === null || parent === undefined ? -1 : parent.#id;
        return __k.reparent(this.#id, target, keepGlobal === undefined ? true : !!keepGlobal);
    }

    // 两个 Node 对象是不是同一个节点（每次取 `self` 都是新对象，`===` 不管用）。
    equals(other) { return other instanceof Node && other.#id === this.#id; }

    // ── 信号（Godot 的 connect / emit_signal）──
    //
    //     door.connect("opened", () => print("门开了"));     // 订阅
    //     self.emitSignal("opened", 3);                       // 发出，参数原样传给订阅者
    //     const [who] = await enemy.toSignal("died");         // 等一次
    //
    // 订阅者的回调跑的时候，`self` 和 `engine.*` 看到的是**订阅者**自己；
    // 回调抛异常只停掉订阅者的脚本，不连累发出信号的那个。
    // 订阅者的节点删掉之后，它的订阅自动作废。
    connect(signal, callback, once) {
        return __connect(this.#id, String(signal), callback, !!once);
    }
    disconnect(signal, callback) { __disconnect(this.#id, String(signal), callback); return this; }
    emitSignal(signal, ...args) { return __emitSignal(this.#id, String(signal), args); }
    // 下一次发出 `signal` 时兑现的 Promise。只有一个参数时兑现成那个参数，
    // 多个时兑现成数组，没有参数时是 undefined。
    toSignal(signal) {
        return new Promise(resolve => {
            __connect(this.#id, String(signal), (...args) => resolve(args.length <= 1 ? args[0] : args), true);
        });
    }

    // 这个节点上跑着的脚本对象，没挂脚本时是 null。
    //
    // 脚本之间就是这么说话的：
    //
    //     getNode("Inventory").script.add("coin", 1);
    //     enemy.script.hit(25);
    //
    // 拿到的是**对方那个实例本身**，所以能调它的方法、读它挂在 this 上的
    // 字段（闭包里的变量仍然够不着，那是 JS 的规矩）。
    get script() {
        const found = globalThis.__instances[this.#id];
        return found === undefined ? null : found;
    }

    toString() { return "Node(" + this.name + ")"; }
}

// `self` —— 当前脚本挂在的那个节点。
//
// 定义成 **getter** 而不是普通变量：谁在跑是每次回调前由引擎设定的，
// 取一次存起来的话，所有实例都会共用第一个跑起来的那个节点。
Object.defineProperty(globalThis, "self", {
    get() { return new Node(__k.selfId()); },
    configurable: true,
});

// 按名字（或路径）找节点，找不到返回 null（GDScript 里是 null，不是异常）。
//
// 单个名字在整个场景里找；带 `/` 的按路径从场景根往下走：`"Level/Door"`。
function getNode(name) {
    const text = String(name);
    if (text.indexOf("/") >= 0) return __resolvePath(null, text);
    const id = __k.find(text);
    return id < 0 ? null : new Node(id);
}

// 解析节点路径。`from` 为 null 时从场景根开始。
function __resolvePath(from, path) {
    let current = from;
    let parts = path.split("/");
    if (path.startsWith("/")) {
        current = null;
        parts = parts.slice(1);
    }
    parts = parts.filter(p => p.length > 0 && p !== ".");
    if (parts.length === 0) return current;
    // 单个名字：先当子节点找，找不到退回全局（兼容旧的 getNode）。
    if (from !== null && parts.length === 1 && parts[0] !== "..") {
        const child = from.findChild(parts[0], false);
        if (child !== null) return child;
        const id = __k.find(parts[0]);
        return id < 0 ? null : new Node(id);
    }
    for (const part of parts) {
        if (part === "..") {
            if (current === null) return null;
            current = current.parent;
            continue;
        }
        const id = __k.findChild(current === null ? -1 : __nodeId(current), part, false);
        if (id < 0) return null;
        current = new Node(id);
    }
    return current;
}

// 给原生侧和信号表用：从 Node 对象拿回下标。
// 通过 `equals` 那条路访问不了私有字段，这里用一个只在前奏里出现的小技巧：
// 把下标存在一张 WeakMap 里，Node 构造时登记。
const __nodeIds = new WeakMap();
function __nodeId(node) { const id = __nodeIds.get(node); return id === undefined ? -1 : id; }

// 每个脚本实例化时拿到的 `self`：绑定到它自己的节点，`await` 之后、
// 信号回调里、计时器里都指着同一个节点。
function __makeNode(id) { return new Node(id); }

// 脚本实例登记表：节点下标 → 脚本返回的那个对象。
// 由 Rust 侧的运行时在实例化与回收时维护，`Node.script` 查它。
globalThis.__instances = {};

// 按名字生成一个节点，原型由游戏侧用 `register_prototype` 登记。
//
//     const enemy = spawn("Enemy", new Vector3(3, 0.5, 0));
//     enemy.script;   // ← 还是 null：新节点的脚本下一帧才实例化
//
// 名字没登记过时返回 null（引擎会记一条日志），一帧内生成太多同样返回 null。
function spawn(name, position) {
    const p = position || Vector3.ZERO();
    const id = __k.spawn(String(name), p.x, p.y, p.z);
    return id < 0 ? null : new Node(id);
}

// 输入。**只有动作与轴**，没有具体键位——键位绑定在 Rust 侧的 Bindings 里，
// 脚本里写死按键的话，改键功能就永远做不了了。
//
//     if (Input.justPressed("attack")) { ... }
//     const move = Input.axisVector("move_x", "move_z");
const Input = {
    pressed(action) { return __k.actionPressed(String(action)); },
    justPressed(action) { return __k.actionJustPressed(String(action)); },
    justReleased(action) { return __k.actionJustReleased(String(action)); },

    // 一个轴的读数：-1、0 或 1。
    axis(name) { return __k.axis(String(name)); },

    // 两个轴合成的方向，长度不超过 1（斜着走不该比直着快）。
    // 约定：x 轴向右为正，y 轴向前为正，返回值放在 XZ 平面上。
    axisVector(xAxis, yAxis) {
        const v = new Vector3(__k.axis(String(xAxis)), 0, -__k.axis(String(yAxis)));
        return v.lengthSquared() > 1 ? v.normalized() : v;
    },

    // 鼠标键名："left" / "right" / "middle"。
    mousePressed(button) { return __k.mousePressed(String(button)); },
    mouseJustPressed(button) { return __k.mouseJustPressed(String(button)); },

    // 光标还没进过窗口时是 null——原点是个合法坐标，混在一起没法区分。
    get mousePosition() {
        const p = __k.mousePosition();
        return p === null ? null : { x: p[0], y: p[1] };
    },

    get mouseDelta() {
        const d = __k.mouseDelta();
        return { x: d[0], y: d[1] };
    },

    get scrollDelta() {
        const d = __k.scrollDelta();
        return { x: d[0], y: d[1] };
    },

    // 第一人称锁定光标：隐藏、关在窗口里，视角靠 mouseDelta 转。
    // 窗口失焦自动放开、切回来自动恢复。不传参数等于 true。
    lockCursor(locked) { __k.lockCursor(locked === undefined ? true : Boolean(locked)); },
    cursorLocked() { return __k.cursorLocked(); },
};

// 本地化：`tr("menu-start")`、`tr("greeting", { name: "小明" })`。
// 找不到的键原样返回键名——界面上露出 `menu-start` 比空白好找。
// 当前语言由游戏侧设（Rust 的 `klocale::set_language`），`language()` 读它。
function tr(key, args) { return __k.tr(String(key), args === undefined ? null : args); }
function language() { return __k.language(); }

// 一次射线检测的结果。
class RayHit {
    constructor(raw) {
        this.node = raw.node < 0 ? null : new Node(raw.node);
        this.position = new Vector3(raw.px, raw.py, raw.pz);
        this.normal = new Vector3(raw.nx, raw.ny, raw.nz);
        this.distance = raw.distance;
    }
}

// **即时**射线检测：当场拿到结果，可以据此决定下一步做什么。
// 这正是旧的「快照进、命令出」架构做不到的事。
function raycast(from, direction, maxDistance) {
    const raw = __k.raycast(
        from.x, from.y, from.z,
        direction.x, direction.y, direction.z,
        maxDistance === undefined ? 1000.0 : maxDistance,
    );
    return raw === null ? null : new RayHit(raw);
}

function print() {
    let parts = [];
    for (let i = 0; i < arguments.length; i++) parts.push(__inspect(arguments[i]));
    __k.log(parts.join(" "));
}

// 把任意值变成人看得懂的文本。字符串原样，类实例走自己的 toString，
// 普通对象和数组展开（限深度，防循环引用），函数只显示名字。
function __inspect(value, depth) {
    const level = depth === undefined ? 0 : depth;
    if (typeof value === "string") return level === 0 ? value : JSON.stringify(value);
    if (value === null || value === undefined) return String(value);
    if (typeof value === "function") return "[函数 " + (value.name || "匿名") + "]";
    if (typeof value !== "object") return String(value);
    if (value instanceof Error) return value.name + ": " + value.message;
    if (level > 3) return Array.isArray(value) ? "[…]" : "{…}";
    if (Array.isArray(value)) {
        const items = value.slice(0, 50).map(v => __inspect(v, level + 1));
        if (value.length > 50) items.push("…还有 " + (value.length - 50) + " 项");
        return "[" + items.join(", ") + "]";
    }
    // 自己定义了 toString 的类（Vector3、Node……）用它。
    if (typeof value.toString === "function" && value.toString !== Object.prototype.toString) {
        return value.toString();
    }
    const keys = Object.keys(value);
    const shown = keys.slice(0, 50).map(k => k + ": " + __inspect(value[k], level + 1));
    if (keys.length > 50) shown.push("…");
    return "{ " + shown.join(", ") + " }";
}

// 给 Rust 侧发一个信号。
function emit(name, value) { __k.emit(name, value === undefined ? 0 : value); }

// ── 模块系统 ──
//
// CommonJS 风格的 `require`，不是 ES 的 `import`。理由：
//
// - 脚本本身就是**函数体**（见 `script.rs` 的约定），ES 模块要求
//   顶层是模块作用域，两者对不上。
// - ES 模块的解析是**异步**的（`import` 可以在求值中途暂停去加载依赖），
//   而脚本回调必须同步跑完——一帧里不能等 I/O。
// - `require` 的语义只有几行就能写清楚，而且和 Node 一致，不用另学。
//
// 模块源码由 Rust 侧的 `ScriptRuntime::add_module` 事先塞进
// `__moduleSources`，所以 `require` 是纯同步的查表。
// 显式挂到 globalThis 上，不能写成顶层 `const`——顶层的 `const` 进的是
// **全局词法作用域**，不会变成 globalThis 的属性，Rust 侧
// `global_object().get()` 取不到它。
globalThis.__moduleSources = {};
globalThis.__moduleCache = {};

function require(name) {
    if (Object.prototype.hasOwnProperty.call(globalThis.__moduleCache, name)) {
        return globalThis.__moduleCache[name].exports;
    }

    const source = globalThis.__moduleSources[name];
    if (typeof source !== "string") {
        throw new Error(
            "找不到模块「" + name + "」。模块要先用 ScriptRuntime::add_module 注册。"
        );
    }

    const module = { exports: {} };
    // **先放进缓存再执行**：循环依赖时后来者拿到的是一份还没填完的
    // exports，而不是无限递归到栈溢出。这是 CommonJS 的标准行为，
    // 代价是循环依赖里拿到的可能是半成品——所以循环依赖仍然该避免。
    globalThis.__moduleCache[name] = module;

    try {
        // 用 `new Function` 而不是 eval：模块拿到的是自己的作用域，
        // 顶层的 `let` 不会漏进全局去污染别的脚本。
        const factory = new Function("module", "exports", "require", source);
        factory(module, module.exports, require);
    } catch (error) {
        // 执行失败的模块要从缓存里拿掉，否则下次 require 会拿到一个
        // 空壳，报出来的错离真正的原因十万八千里。
        delete globalThis.__moduleCache[name];
        throw error;
    }

    return module.exports;
}

const engine = {
    get time() { return __k.time(); },
    get deltaTime() { return __k.delta(); },
    getNode,
    raycast,
    spawn,
    print,
    emit,
    require,
    Input,
};
