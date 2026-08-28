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

    constructor(id) {
        this.#id = id;
    }

    get name() { return __k.getName(this.#id); }

    get valid() { return __k.isValid(this.#id); }

    get position() { return new BoundVector3(this.#id, FIELD_POSITION); }
    set position(v) { __k.setVec(this.#id, FIELD_POSITION, v.x, v.y, v.z); }

    get scale() { return new BoundVector3(this.#id, FIELD_SCALE); }
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

    getNode(name) { return getNode(name); }

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

// 按名字找节点，找不到返回 null（GDScript 里是 null，不是异常）。
function getNode(name) {
    const id = __k.find(name);
    return id < 0 ? null : new Node(id);
}

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
};

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
    for (let i = 0; i < arguments.length; i++) parts.push(String(arguments[i]));
    __k.log(parts.join(" "));
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
