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
    constructor(id, field) {
        Object.defineProperty(this, "_id", { value: id, enumerable: false });
        Object.defineProperty(this, "_field", { value: field, enumerable: false });
    }

    get x() { return __k.getComponent(this._id, this._field, 0); }
    set x(v) { __k.setComponent(this._id, this._field, 0, v); }
    get y() { return __k.getComponent(this._id, this._field, 1); }
    set y(v) { __k.setComponent(this._id, this._field, 1, v); }
    get z() { return __k.getComponent(this._id, this._field, 2); }
    set z(v) { __k.setComponent(this._id, this._field, 2, v); }

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
    constructor(id) {
        Object.defineProperty(this, "_id", { value: id, enumerable: false });
    }

    get name() { return __k.getName(this._id); }

    get valid() { return __k.isValid(this._id); }

    get position() { return new BoundVector3(this._id, FIELD_POSITION); }
    set position(v) { __k.setVec(this._id, FIELD_POSITION, v.x, v.y, v.z); }

    get scale() { return new BoundVector3(this._id, FIELD_SCALE); }
    set scale(v) { __k.setVec(this._id, FIELD_SCALE, v.x, v.y, v.z); }

    // 世界坐标是每帧算出来的派生值，只读。
    get globalPosition() {
        const v = __k.getGlobalPosition(this._id);
        return new Vector3(v[0], v[1], v[2]);
    }

    get visible() { return __k.getVisible(this._id); }
    set visible(v) { __k.setVisible(this._id, !!v); }

    get linearVelocity() {
        const v = __k.getLinvel(this._id);
        return new Vector3(v[0], v[1], v[2]);
    }

    translate(v) { __k.translate(this._id, v.x, v.y, v.z); return this; }
    rotateY(a) { __k.rotateY(this._id, a); return this; }
    lookAt(target) { __k.lookAt(this._id, target.x, target.y, target.z); return this; }

    applyImpulse(v) { __k.applyImpulse(this._id, v.x, v.y, v.z); return this; }
    setLinearVelocity(v) { __k.setLinvel(this._id, v.x, v.y, v.z); return this; }

    playSound() { __k.playSound(this._id); return this; }

    // 名字取自 GDScript 的 `queue_free()`：删除在本次操作里立即生效。
    queueFree() { __k.queueFree(this._id); }

    getNode(name) { return getNode(name); }

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

const engine = {
    get time() { return __k.time(); },
    get deltaTime() { return __k.delta(); },
    getNode,
    raycast,
    print,
    emit,
};
