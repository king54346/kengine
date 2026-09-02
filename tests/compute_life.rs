//! 盯着 `compute_game_of_life` 例子里那个计算着色器。
//!
//! 例子本身要开窗口才看得见，而「图案演化得对不对」用眼睛核对也不牢靠。
//! 这里用无头设备把同一个着色器跑起来，拿周期已知的经典图案对答案。
//!
//! 没有可用显卡时（CI 上常见）整组测试跳过——不装作通过，也不误报失败。

use kengine::krender::ComputeContext;
use kengine::kshader::Shader;

/// 必须和着色器里的 `GRID` 一致。
const GRID: usize = 64;
const CELLS: usize = GRID * GRID;

/// 例子和测试共用同一份着色器源码——两边各存一份的话，
/// 改了例子而测试还在验旧代码，这测试就白写了。
const SHADER: &str = include_str!("../examples/kengine/shader/compute_game_of_life.wgsl");

/// 一台跑着生命游戏的无头 GPU。
struct Life {
    /// 借的是整个进程共用的那一台，不是自己开的（见 `shared_headless`）。
    gpu: &'static ComputeContext,
    pipeline: kengine::krender::ComputePipeline,
    buffers: [kengine::krender::StorageBuffer; 2],
    front: usize,
}

impl Life {
    /// 没有可用适配器就返回 `None`，调用方据此跳过。
    fn new(seed: &[u32]) -> Option<Self> {
        // 共用整个测试进程的那一台设备，见 `shared_headless` 的文档。
        let gpu = ComputeContext::shared_headless()?;
        let shader = Shader::from_wgsl(SHADER).expect("着色器编译不过");
        let pipeline = gpu.create_pipeline(&shader).expect("建不了计算管线");

        let bytes: Vec<u8> = seed.iter().flat_map(|c| c.to_le_bytes()).collect();
        let buffers = [
            gpu.create_buffer("gen a", &bytes),
            gpu.create_buffer_zeroed("gen b", bytes.len() as u64),
        ];

        Some(Life {
            gpu,
            pipeline,
            buffers,
            front: 0,
        })
    }

    fn step(&mut self) {
        let back = 1 - self.front;
        self.gpu.dispatch(
            &self.pipeline,
            &[&self.buffers[self.front], &self.buffers[back]],
            [8, 8, 1],
        );
        self.front = back;
    }

    fn read(&self) -> Vec<u32> {
        let bytes = self.gpu.read(&self.buffers[self.front]).expect("读不回来");
        bytes
            .chunks_exact(4)
            .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect()
    }
}

/// 把一串坐标铺成一张网格。
fn grid_of(alive: &[(usize, usize)]) -> Vec<u32> {
    let mut cells = vec![0u32; CELLS];
    for &(x, y) in alive {
        cells[y * GRID + x] = 1;
    }
    cells
}

/// 网格里活着的格子，排好序，好直接比较。
fn alive_cells(cells: &[u32]) -> Vec<(usize, usize)> {
    let mut out: Vec<_> = cells
        .iter()
        .enumerate()
        .filter(|&(_, &c)| c == 1)
        .map(|(i, _)| (i % GRID, i / GRID))
        .collect();
    out.sort_unstable();
    out
}

#[test]
fn a_blinker_blinks() {
    // 最小的振荡器：三个一排，下一代转 90°，再下一代转回来。
    // 周期是 2，所以既能验规则算对了，也能验双缓冲没把两代搅在一起。
    let horizontal = [(10, 10), (11, 10), (12, 10)];
    let vertical = [(11, 9), (11, 10), (11, 11)];

    let Some(mut life) = Life::new(&grid_of(&horizontal)) else {
        eprintln!("没有可用的计算设备，跳过");
        return;
    };

    life.step();
    assert_eq!(alive_cells(&life.read()), vertical, "第一代没转成竖的");

    life.step();
    let mut expected = horizontal.to_vec();
    expected.sort_unstable();
    assert_eq!(alive_cells(&life.read()), expected, "第二代没转回横的");
}

#[test]
fn a_block_sits_still() {
    // 2×2 的方块是静物：每个格子都正好 3 个邻居，谁也不死，
    // 周围也没有格子能凑够 3 个邻居复生。
    let block = [(20, 20), (21, 20), (20, 21), (21, 21)];

    let Some(mut life) = Life::new(&grid_of(&block)) else {
        eprintln!("没有可用的计算设备，跳过");
        return;
    };

    for generation in 1..=5 {
        life.step();
        let mut expected = block.to_vec();
        expected.sort_unstable();
        assert_eq!(
            alive_cells(&life.read()),
            expected,
            "第 {generation} 代动了"
        );
    }
}

#[test]
fn a_glider_walks_diagonally() {
    // 滑翔机每 4 代把自己整体挪动 (+1, +1)。这条最能说明问题：
    // 邻居索引算错一位的话，图案要么散架要么走偏。
    let glider = [(5, 5), (6, 6), (4, 7), (5, 7), (6, 7)];

    let Some(mut life) = Life::new(&grid_of(&glider)) else {
        eprintln!("没有可用的计算设备，跳过");
        return;
    };

    for _ in 0..4 {
        life.step();
    }

    let mut expected: Vec<_> = glider.iter().map(|&(x, y)| (x + 1, y + 1)).collect();
    expected.sort_unstable();
    assert_eq!(alive_cells(&life.read()), expected, "滑翔机没走对地方");
}

#[test]
fn the_grid_wraps_around_the_edges() {
    // 贴着边放一个 blinker：中间那个在 x=0，两头分别在 x=1 和 x=63。
    // 不做环绕的话它会缺一个邻居，直接死掉。
    let horizontal = [(63, 30), (0, 30), (1, 30)];
    let vertical = [(0, 29), (0, 30), (0, 31)];

    let Some(mut life) = Life::new(&grid_of(&horizontal)) else {
        eprintln!("没有可用的计算设备，跳过");
        return;
    };

    life.step();
    assert_eq!(
        alive_cells(&life.read()),
        vertical,
        "跨过左右边界的 blinker 没能正常翻转"
    );
}

#[test]
fn an_empty_grid_stays_empty() {
    // 空网格什么也长不出来。这条看着像废话，但它专门盯着一类真实的错误：
    // 缓冲没清零、或者读写绑反了，都会凭空冒出格子来。
    let Some(mut life) = Life::new(&vec![0u32; CELLS]) else {
        eprintln!("没有可用的计算设备，跳过");
        return;
    };

    life.step();
    life.step();

    assert_eq!(life.read().iter().sum::<u32>(), 0, "空网格里长出了东西");
}
