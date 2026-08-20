// 绕着 spinner 转圈，并当场朝脚下打一条射线。
//
// 射线是实时访问的关键证据：脚本在回调中途查询、拿到结果、再据此行动。
// 「快照进、命令出」那套架构做不到这件事——查询只能排到下一帧。

let angle = 0;
let reported = false;

return {
    _process(delta) {
        angle += delta * 2.0;

        const target = getNode("ScriptSpinner");
        if (target === null) return;   // 找不到给 null，和 GDScript 一样

        // 跨节点读世界坐标，算出一个绕圈的偏移。
        const center = target.globalPosition;
        self.position = new Vector3(
            center.x + Math.cos(angle) * 1.2,
            center.y,
            center.z + Math.sin(angle) * 1.2,
        );

        // 即时射线：当场拿到结果，当场据此行动。
        const hit = raycast(self.globalPosition, Vector3.DOWN(), 10.0);
        if (hit !== null && !reported) {
            reported = true;
            print("脚下", hit.distance.toFixed(2), "米是", hit.node ? hit.node.name : "?");
            emit("follower.ground", hit.distance);
        }
    },
};
