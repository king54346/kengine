// 自转 + 上下浮动的方块。
//
// 脚本文件是一个**函数体**，`return` 一个带生命周期方法的对象。
// 写成函数体而不是对象字面量，是为了让脚本有自己的闭包变量——
// 下面的 elapsed / direction 每个实例各有一份，不是所有实例共享一个全局。

let elapsed = 0;
let direction = 1;
let reports = 0;

return {
    _ready() {
        print("spinner 醒了，挂在", self.name, "上");
    },

    _process(delta) {
        elapsed += delta;

        // GDScript 的手感：读一个分量、加一点、写回去，立刻生效。
        self.rotateY(delta * 1.5);
        self.position.y += direction * delta * 0.8;

        if (self.position.y > 1.6) direction = -1;
        if (self.position.y < 0.4) direction = 1;

        // 每两秒给 Rust 侧发一个信号。
        if (elapsed > 2 * (reports + 1)) {
            reports += 1;
            emit("spinner.tick", reports);
        }
    },
};
