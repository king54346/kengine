// kscript 前奏的第二部分：异步（await / 计时器）、信号、console、四元数、数学工具。
//
// 在 `prelude.js` 之后求值，能用那边定义的 Node / Vector3 / __inspect。

// ── 错误描述 ──

function __describeError(error) {
    if (error instanceof Error) {
        let text = error.name + ": " + error.message;
        if (typeof error.stack === "string" && error.stack.length > 0) text += "\n" + error.stack;
        return text;
    }
    return __inspect(error);
}

// ── 计时器（游戏时间）──
//
// 和浏览器不同，这里的时间是**游戏时间**（`engine.time`）：暂停游戏、放慢
// 时间时计时器跟着停、跟着慢。到点的回调在当帧 `_process` 全部跑完之后执行，
// 回调里的 `self` 是登记计时器的那个脚本的节点。
//
//     await wait(1.5);                  // 等 1.5 秒
//     await nextFrame();                // 等到下一帧
//     const id = setTimeout(() => self.queueFree(), 3000);   // 毫秒，和浏览器一致
//     setInterval(() => print("tick"), 500);
//     clearTimeout(id);
//
// 一个计时器最早在**登记之后的下一帧**触发，哪怕延迟是 0——
// `while (x) await nextFrame();` 这种写法因此每帧只走一圈，而不是当场转死。

globalThis.__timers = [];
let __timerSeq = 1;
let __tickNow = 0;

function __beginTick(tick) { __tickNow = tick; }

function __addTimer(seconds, callback, interval) {
    const delay = Number(seconds);
    const wait = Number.isFinite(delay) && delay > 0 ? delay : 0;
    const id = __timerSeq++;
    globalThis.__timers.push({
        id,
        due: __k.time() + wait,
        interval: interval ? Math.max(wait, 0) : -1,
        owner: __k.selfId(),
        born: __tickNow,
        callback,
    });
    return id;
}

// 跑下一个到点的计时器。没有到点的返回 false。
// 由运行时在 tick 末尾反复调用，每跑一个就清一次 Promise 队列，
// 所以 `await wait()` 之后的代码在 `self` 还指着自己时就跑完了。
function __fireNextTimer(now) {
    const timers = globalThis.__timers;
    let best = -1;
    for (let i = 0; i < timers.length; i++) {
        const t = timers[i];
        if (t.due <= now && t.born < __tickNow && (best < 0 || t.due < timers[best].due)) best = i;
    }
    if (best < 0) return false;
    const timer = timers[best];
    if (timer.interval >= 0) {
        // 按「上一次该到的时刻」往后排，不按现在：长期运行不会越拖越晚。
        timer.due = Math.max(timer.due + Math.max(timer.interval, 1e-3), now - 1);
        timer.born = __tickNow;
    } else {
        timers.splice(best, 1);
    }
    if (timer.owner >= 0 && !__k.isValid(timer.owner)) {
        // 节点删了，它的计时器作废。
        __cancelTimersOf(timer.owner);
        return true;
    }
    const previous = __k.setSelf(timer.owner);
    try {
        timer.callback();
    } catch (error) {
        __k.asyncError(timer.owner, __describeError(error));
    } finally {
        __k.setSelf(previous);
    }
    return true;
}

function __cancelTimersOf(owner) {
    globalThis.__timers = globalThis.__timers.filter(t => t.owner !== owner);
}

// ── 可复现的 Math.random ──
//
// 设了环境变量 `KENGINE_SEED` 时换成一个带种子的生成器（mulberry32），同一个种子
// 每次跑出同一串数——截图回归、复现 bug 时用。没设就是原来那个。
(function () {
    const seed = __k.randomSeed();
    if (seed === null) return;
    let state = (seed >>> 0) || 1;
    Math.random = function () {
        state = (state + 0x6D2B79F5) >>> 0;
        let t = state;
        t = Math.imul(t ^ (t >>> 15), t | 1);
        t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
        return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
    };
})();

function wait(seconds) {
    return new Promise(resolve => { __addTimer(seconds, resolve, false); });
}

function nextFrame() { return wait(0); }

function setTimeout(callback, milliseconds, ...args) {
    if (typeof callback !== "function") throw new TypeError("setTimeout 的第一个参数要是函数");
    return __addTimer((Number(milliseconds) || 0) / 1000, () => callback(...args), false);
}

function setInterval(callback, milliseconds, ...args) {
    if (typeof callback !== "function") throw new TypeError("setInterval 的第一个参数要是函数");
    return __addTimer((Number(milliseconds) || 0) / 1000, () => callback(...args), true);
}

function clearTimeout(id) {
    globalThis.__timers = globalThis.__timers.filter(t => t.id !== id);
}
const clearInterval = clearTimeout;

// 生命周期方法返回了 Promise（写成了 `async _ready()`）：盯着它，被拒绝时
// 把错误算到这个脚本头上。不盯的话 async 函数里的异常会无声消失。
function __watchPromise(promise, owner) {
    promise.then(undefined, error => __k.asyncError(owner, __describeError(error)));
}

// ── 信号 ──

globalThis.__connections = new Map();   // "节点:信号" → [{ owner, callback, once }]

function __connect(node, signal, callback, once) {
    if (typeof callback !== "function") throw new TypeError("connect 的回调要是函数");
    const key = node + ":" + signal;
    let list = globalThis.__connections.get(key);
    if (list === undefined) {
        list = [];
        globalThis.__connections.set(key, list);
    }
    // 同一个回调重复订阅只算一次（Godot 会报错，这里安静地忽略）。
    if (!list.some(c => c.callback === callback)) list.push({ owner: __k.selfId(), callback, once });
    return true;
}

function __disconnect(node, signal, callback) {
    const key = node + ":" + signal;
    const list = globalThis.__connections.get(key);
    if (list === undefined) return;
    const kept = callback === undefined ? [] : list.filter(c => c.callback !== callback);
    if (kept.length === 0) globalThis.__connections.delete(key);
    else globalThis.__connections.set(key, kept);
}

// 发出信号，返回实际通知到了几个订阅者。
function __emitSignal(node, signal, args) {
    const key = node + ":" + signal;
    const list = globalThis.__connections.get(key);
    if (list === undefined) return 0;
    // 拷一份再遍历：回调里取消订阅、再订阅都不会打乱这一轮。
    const snapshot = list.slice();
    const remaining = list.filter(c => !c.once);
    if (remaining.length === 0) globalThis.__connections.delete(key);
    else globalThis.__connections.set(key, remaining);
    let delivered = 0;
    for (const connection of snapshot) {
        if (connection.owner >= 0 && !__k.isValid(connection.owner)) continue;
        const previous = __k.setSelf(connection.owner);
        try {
            connection.callback(...args);
            delivered++;
        } catch (error) {
            __k.asyncError(connection.owner, __describeError(error));
        } finally {
            __k.setSelf(previous);
        }
    }
    return delivered;
}

// 一个脚本停掉或节点删掉时：它登记的计时器、它的订阅、别人对它的订阅一并作废。
function __forgetOwner(owner) {
    __cancelTimersOf(owner);
    for (const [key, list] of Array.from(globalThis.__connections.entries())) {
        const kept = list.filter(c => c.owner !== owner);
        if (key.startsWith(owner + ":") || kept.length === 0) globalThis.__connections.delete(key);
        else if (kept.length !== list.length) globalThis.__connections.set(key, kept);
    }
}

function __resetAsync() {
    globalThis.__timers = [];
    globalThis.__connections = new Map();
}

// ── console ──
//
// 和浏览器一样的四个级别，参数用空格连起来；对象会被展开成文本，
// 不会只打出一个 `[object Object]`。
const console = {
    log(...args) { __k.logLevel(1, args.map(a => __inspect(a)).join(" ")); },
    info(...args) { __k.logLevel(1, args.map(a => __inspect(a)).join(" ")); },
    debug(...args) { __k.logLevel(0, args.map(a => __inspect(a)).join(" ")); },
    warn(...args) { __k.logLevel(2, args.map(a => __inspect(a)).join(" ")); },
    error(...args) { __k.logLevel(3, args.map(a => a instanceof Error ? __describeError(a) : __inspect(a)).join(" ")); },
    assert(condition, ...args) {
        if (!condition) __k.logLevel(3, "断言失败" + (args.length ? "：" + args.map(a => __inspect(a)).join(" ") : ""));
    },
    // 计时：用的是真实时间（毫秒），量脚本自己的耗时用。
    time(label) { __consoleTimers.set(String(label === undefined ? "default" : label), Date.now()); },
    timeEnd(label) {
        const name = String(label === undefined ? "default" : label);
        const start = __consoleTimers.get(name);
        if (start === undefined) return;
        __consoleTimers.delete(name);
        __k.logLevel(1, name + "：" + (Date.now() - start) + " ms");
    },
};
const __consoleTimers = new Map();

// ── 四元数 ──
//
// 旋转的另一种写法：没有万向节锁，插值（slerp）走最短弧。
// `node.quaternion` 读写它；`q.rotate(v)` 转一个向量。

class Quaternion {
    constructor(x, y, z, w) {
        this.x = x === undefined ? 0 : x;
        this.y = y === undefined ? 0 : y;
        this.z = z === undefined ? 0 : z;
        this.w = w === undefined ? 1 : w;
    }

    static identity() { return new Quaternion(0, 0, 0, 1); }

    static fromAxisAngle(axis, angle) {
        const n = axis.normalized();
        const s = Math.sin(angle / 2);
        return new Quaternion(n.x * s, n.y * s, n.z * s, Math.cos(angle / 2));
    }

    // 欧拉角（弧度，YXZ 顺序，同 `node.rotation`）。
    static fromEuler(v) {
        const qy = Quaternion.fromAxisAngle(Vector3.UP(), v.y);
        const qx = Quaternion.fromAxisAngle(Vector3.RIGHT(), v.x);
        const qz = Quaternion.fromAxisAngle(new Vector3(0, 0, 1), v.z);
        return qy.mul(qx).mul(qz);
    }

    // 从 `from` 方向转到 `to` 方向的最短旋转。
    static fromTo(from, to) {
        const a = from.normalized();
        const b = to.normalized();
        const d = a.dot(b);
        if (d < -0.999999) {
            // 正好反向：随便挑一根和 a 垂直的轴转半圈。
            let axis = Vector3.RIGHT().cross(a);
            if (axis.lengthSquared() < 1e-6) axis = Vector3.UP().cross(a);
            return Quaternion.fromAxisAngle(axis, Math.PI);
        }
        const c = a.cross(b);
        return new Quaternion(c.x, c.y, c.z, 1 + d).normalized();
    }

    mul(o) {
        return new Quaternion(
            this.w * o.x + this.x * o.w + this.y * o.z - this.z * o.y,
            this.w * o.y - this.x * o.z + this.y * o.w + this.z * o.x,
            this.w * o.z + this.x * o.y - this.y * o.x + this.z * o.w,
            this.w * o.w - this.x * o.x - this.y * o.y - this.z * o.z,
        );
    }

    // 转一个向量。
    rotate(v) {
        const u = new Vector3(this.x, this.y, this.z);
        const t = u.cross(v).mul(2);
        return v.add(t.mul(this.w)).add(u.cross(t));
    }

    length() { return Math.sqrt(this.dot(this)); }
    dot(o) { return this.x * o.x + this.y * o.y + this.z * o.z + this.w * o.w; }
    normalized() {
        const n = this.length();
        return n > 1e-9 ? new Quaternion(this.x / n, this.y / n, this.z / n, this.w / n) : Quaternion.identity();
    }
    inverse() {
        const n = this.dot(this);
        return n > 1e-12 ? new Quaternion(-this.x / n, -this.y / n, -this.z / n, this.w / n) : Quaternion.identity();
    }

    slerp(o, t) {
        let d = this.dot(o);
        // 走短的那条弧。
        const target = d < 0 ? new Quaternion(-o.x, -o.y, -o.z, -o.w) : o;
        d = Math.abs(d);
        if (d > 0.9995) {
            return new Quaternion(
                this.x + (target.x - this.x) * t,
                this.y + (target.y - this.y) * t,
                this.z + (target.z - this.z) * t,
                this.w + (target.w - this.w) * t,
            ).normalized();
        }
        const theta = Math.acos(d);
        const s = Math.sin(theta);
        const a = Math.sin((1 - t) * theta) / s;
        const b = Math.sin(t * theta) / s;
        return new Quaternion(
            this.x * a + target.x * b,
            this.y * a + target.y * b,
            this.z * a + target.z * b,
            this.w * a + target.w * b,
        );
    }

    // 两个旋转之间的夹角（弧度）。
    angleTo(o) { return 2 * Math.acos(Math.min(1, Math.abs(this.normalized().dot(o.normalized())))); }

    // 转回欧拉角（弧度，YXZ）。
    toEuler() {
        const { x, y, z, w } = this;
        // 旋转矩阵的第 3 列（前向）与第 2 行……按 YXZ 分解。
        const m12 = 2 * (y * z - w * x);
        const sx = Math.max(-1, Math.min(1, -m12));
        const ex = Math.asin(sx);
        let ey, ez;
        if (Math.abs(sx) < 0.9999999) {
            ey = Math.atan2(2 * (x * z + w * y), 1 - 2 * (x * x + y * y));
            ez = Math.atan2(2 * (x * y + w * z), 1 - 2 * (x * x + z * z));
        } else {
            ey = Math.atan2(-2 * (x * z - w * y), 1 - 2 * (y * y + z * z));
            ez = 0;
        }
        return new Vector3(ex, ey, ez);
    }

    clone() { return new Quaternion(this.x, this.y, this.z, this.w); }
    toString() { return "Quaternion(" + this.x + ", " + this.y + ", " + this.z + ", " + this.w + ")"; }
}

// 前奏第一部分（prelude.js）里的函数要用到它。boa 在编译时就把自由变量
// 定好了去向：前一个脚本里的函数看不见后一个脚本的顶层 `class` / `const`，
// 只看得见 globalThis 上的属性。
globalThis.Quaternion = Quaternion;

// ── Vector3 补充 ──

Vector3.fromArray = a => new Vector3(a[0], a[1], a[2]);
Vector3.prototype.toArray = function () { return [this.x, this.y, this.z]; };
Vector3.prototype.div = function (s) { return new Vector3(this.x / s, this.y / s, this.z / s); };
Vector3.prototype.mulVec = function (o) { return new Vector3(this.x * o.x, this.y * o.y, this.z * o.z); };
Vector3.prototype.abs = function () { return new Vector3(Math.abs(this.x), Math.abs(this.y), Math.abs(this.z)); };
Vector3.prototype.min = function (o) { return new Vector3(Math.min(this.x, o.x), Math.min(this.y, o.y), Math.min(this.z, o.z)); };
Vector3.prototype.max = function (o) { return new Vector3(Math.max(this.x, o.x), Math.max(this.y, o.y), Math.max(this.z, o.z)); };
Vector3.prototype.equals = function (o) { return this.x === o.x && this.y === o.y && this.z === o.z; };
Vector3.prototype.isEqualApprox = function (o, epsilon) {
    const e = epsilon === undefined ? 1e-5 : epsilon;
    return Math.abs(this.x - o.x) <= e && Math.abs(this.y - o.y) <= e && Math.abs(this.z - o.z) <= e;
};
// 两个向量的夹角（弧度，0..π）。
Vector3.prototype.angleTo = function (o) {
    const d = this.length() * o.length();
    return d > 1e-12 ? Math.acos(Math.max(-1, Math.min(1, this.dot(o) / d))) : 0;
};
// 绕轴转一个角度。
Vector3.prototype.rotated = function (axis, angle) { return Quaternion.fromAxisAngle(axis, angle).rotate(this); };
Vector3.prototype.projectOnto = function (o) {
    const d = o.lengthSquared();
    return d > 1e-12 ? o.mul(this.dot(o) / d) : Vector3.ZERO();
};
// 投到以 `normal` 为法线的平面上（沿墙滑动就是它）。
Vector3.prototype.slide = function (normal) { return this.sub(normal.mul(this.dot(normal))); };
Vector3.prototype.reflect = function (normal) { return this.sub(normal.mul(2 * this.dot(normal))); };
// 朝 `to` 走最多 `delta` 的距离，不会走过头。
Vector3.prototype.moveToward = function (to, delta) {
    const d = to.sub(this);
    const len = d.length();
    return len <= delta || len < 1e-9 ? to.clone() : this.add(d.mul(delta / len));
};
Vector3.prototype.limitLength = function (max) {
    const len = this.length();
    return len > max && len > 1e-9 ? this.mul(max / len) : this.clone();
};
for (const name of ["toArray", "div", "mulVec", "abs", "min", "max", "equals", "isEqualApprox", "angleTo",
                    "rotated", "projectOnto", "slide", "reflect", "moveToward", "limitLength"]) {
    BoundVector3.prototype[name] = function (...args) { return this.clone()[name](...args); };
}

// ── 数学工具（Godot 的全局函数）──

const Mathf = {
    PI: Math.PI,
    TAU: Math.PI * 2,
    lerp(a, b, t) { return a + (b - a) * t; },
    inverseLerp(a, b, v) { return a === b ? 0 : (v - a) / (b - a); },
    remap(v, fromA, fromB, toA, toB) { return Mathf.lerp(toA, toB, Mathf.inverseLerp(fromA, fromB, v)); },
    clamp(v, lo, hi) { return Math.min(hi, Math.max(lo, v)); },
    clamp01(v) { return Math.min(1, Math.max(0, v)); },
    smoothstep(a, b, v) {
        const t = Mathf.clamp01(Mathf.inverseLerp(a, b, v));
        return t * t * (3 - 2 * t);
    },
    moveToward(from, to, delta) {
        return Math.abs(to - from) <= delta ? to : from + Math.sign(to - from) * delta;
    },
    // 取模，结果总在 [lo, hi) 里（负数也一样，和 `%` 不同）。
    wrap(v, lo, hi) { const r = hi - lo; return r === 0 ? lo : lo + ((((v - lo) % r) + r) % r); },
    pingPong(v, length) { const t = Mathf.wrap(v, 0, length * 2); return length - Math.abs(t - length); },
    degToRad(d) { return d * Math.PI / 180; },
    radToDeg(r) { return r * 180 / Math.PI; },
    // 角度的插值，走短的那边（350° 到 10° 是往前 20°，不是倒回 340°）。
    lerpAngle(a, b, t) {
        const d = Mathf.wrap(b - a, -Math.PI, Math.PI);
        return a + d * t;
    },
    // 与帧率无关的平滑跟随：`x = Mathf.damp(x, target, 10, delta)`。
    damp(current, target, lambda, delta) { return Mathf.lerp(current, target, 1 - Math.exp(-lambda * delta)); },
    isEqualApprox(a, b, epsilon) { return Math.abs(a - b) <= (epsilon === undefined ? 1e-5 : epsilon); },
};

// ── 随机数 ──
//
// 可设种子的伪随机数（xorshift128+ 的 32 位变体）。同一个种子每次跑出
// 同一串数——回放、联机同步、复现 bug 都靠它。`Math.random()` 做不到。

class RandomNumberGenerator {
    constructor(seed) { this.seed = seed === undefined ? Date.now() : seed; }

    set seed(value) {
        let s = (Number(value) >>> 0) || 0x9e3779b9;
        // 种子打散成四个状态字，别让相近的种子出相近的序列。
        // 写成一步一行而不是一个大表达式：boa 的解析器每多嵌一层括号要多吃
        // 一百多 KB 的栈，Windows 主线程只有 1 MB。
        const next = () => {
            s = (s + 0x6d2b79f5) >>> 0;
            let t = s;
            let u = t >>> 15;
            t = Math.imul(t ^ u, t | 1);
            u = t >>> 7;
            u = Math.imul(t ^ u, t | 61);
            t ^= t + u;
            u = t >>> 14;
            return (t ^ u) >>> 0;
        };
        this.state = [next(), next(), next(), next()];
        this._seed = Number(value);
    }
    get seed() { return this._seed; }

    // [0, 2³²) 的整数。
    randi() {
        const st = this.state;
        let t = st[3];
        let s = st[0];
        st[3] = st[2]; st[2] = st[1]; st[1] = s;
        t ^= t << 11; t ^= t >>> 8;
        st[0] = (t ^ s ^ (s >>> 19)) >>> 0;
        return st[0];
    }
    // [0, 1)。
    randf() { return this.randi() / 4294967296; }
    randfRange(a, b) { return a + (b - a) * this.randf(); }
    // [a, b] 里的整数（两端都含）。
    randiRange(a, b) { return a + Math.floor(this.randf() * (b - a + 1)); }
    // 正态分布（Box–Muller）。
    randfn(mean, deviation) {
        const u = 1 - this.randf();
        const v = this.randf();
        return (mean || 0) + (deviation === undefined ? 1 : deviation) * Math.sqrt(-2 * Math.log(u)) * Math.cos(2 * Math.PI * v);
    }
    pick(array) { return array.length === 0 ? undefined : array[Math.floor(this.randf() * array.length)]; }
    shuffle(array) {
        for (let i = array.length - 1; i > 0; i--) {
            const j = Math.floor(this.randf() * (i + 1));
            [array[i], array[j]] = [array[j], array[i]];
        }
        return array;
    }
    // 单位球面上的随机方向。
    direction() {
        const z = this.randfRange(-1, 1);
        const a = this.randf() * Math.PI * 2;
        const r = Math.sqrt(1 - z * z);
        return new Vector3(r * Math.cos(a), r * Math.sin(a), z);
    }
}

// 全局的那一个，和 Godot 的 `randf()` / `randi()` / `randomize()` 一样用。
const __globalRng = new RandomNumberGenerator();
function randf() { return __globalRng.randf(); }
function randi() { return __globalRng.randi(); }
function randfRange(a, b) { return __globalRng.randfRange(a, b); }
function randiRange(a, b) { return __globalRng.randiRange(a, b); }
function seed(value) { __globalRng.seed = value; }

// ── engine 命名空间补充 ──

Object.assign(engine, {
    wait,
    nextFrame,
    setTimeout,
    setInterval,
    clearTimeout,
    clearInterval,
    console,
    Quaternion,
    Mathf,
    RandomNumberGenerator,
});
