//! GPU 分段计时：一帧里每个 pass 在显卡上花了多久。
//!
//! 在 pass 之间往命令编码器里写时间戳（`write_timestamp`），帧末把查询结果解析到
//! 一块缓冲、拷进可映射的回读缓冲，**异步**映射——结果晚两三帧才拿到，但一帧都不等。
//! 三块回读缓冲轮流用，映射还没好的那块这一帧跳过（只是少一个样本）。
//!
//! 要适配器支持 `TIMESTAMP_QUERY` 和 `TIMESTAMP_QUERY_INSIDE_ENCODERS`；不支持时
//! [`GpuTimer::new`] 返回 `None`，剖析面板只显示 CPU 那一栏。

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

/// 一帧最多多少个时间戳。
const MAX_MARKS: u32 = 64;
/// 回读缓冲轮转几块。
const RING: usize = 3;

/// 一块回读缓冲和它对应那一帧的标签。
struct Slot {
    buffer: wgpu::Buffer,
    labels: Vec<&'static str>,
    /// 映射完成了（回调里置位）。
    ready: Arc<AtomicBool>,
    /// 已经发起了映射、还没读。
    in_flight: bool,
}

/// GPU 分段计时器。
pub(crate) struct GpuTimer {
    queries: wgpu::QuerySet,
    resolve: wgpu::Buffer,
    slots: Vec<Slot>,
    /// 这一帧写到第几个查询、各自的标签。
    count: u32,
    labels: Vec<&'static str>,
    /// 这一帧用哪块回读缓冲。`None` 表示它还被上一轮占着，这一帧不记。
    current: Option<usize>,
    next_slot: usize,
    /// 一个时间戳刻度是多少纳秒。
    period_ns: f32,
    /// 最近一次读回的结果：`(段名, 毫秒)`。
    latest: Vec<(&'static str, f32)>,
}

impl GpuTimer {
    /// 设备支持时建一个。
    pub(crate) fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Option<Self> {
        let needed =
            wgpu::Features::TIMESTAMP_QUERY | wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS;
        if !device.features().contains(needed) {
            return None;
        }
        let size = MAX_MARKS as u64 * 8;
        let queries = device.create_query_set(&wgpu::QuerySetDescriptor {
            label: Some("kengine gpu timer"),
            ty: wgpu::QueryType::Timestamp,
            count: MAX_MARKS,
        });
        let resolve = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("kengine gpu timer resolve"),
            size,
            usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let slots = (0..RING)
            .map(|_| Slot {
                buffer: device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("kengine gpu timer readback"),
                    size,
                    usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                }),
                labels: Vec::new(),
                ready: Arc::new(AtomicBool::new(false)),
                in_flight: false,
            })
            .collect();
        Some(Self {
            queries,
            resolve,
            slots,
            count: 0,
            labels: Vec::new(),
            current: None,
            next_slot: 0,
            period_ns: queue.get_timestamp_period(),
            latest: Vec::new(),
        })
    }

    /// 开一帧：先把已经映射好的旧结果读出来，再挑一块空闲的回读缓冲。
    pub(crate) fn begin_frame(&mut self) {
        self.collect();
        self.count = 0;
        self.labels.clear();
        let slot = self.next_slot;
        self.current = (!self.slots[slot].in_flight).then_some(slot);
        if self.current.is_some() {
            self.next_slot = (slot + 1) % RING;
        }
    }

    /// 在编码器里记一个时间戳：从这里到下一个标记之间的工作算进 `label`。
    pub(crate) fn mark(&mut self, encoder: &mut wgpu::CommandEncoder, label: &'static str) {
        if self.current.is_none() || self.count >= MAX_MARKS {
            return;
        }
        encoder.write_timestamp(&self.queries, self.count);
        self.labels.push(label);
        self.count += 1;
    }

    /// 帧末：解析查询、拷进回读缓冲。要在 `submit` 之前调。
    pub(crate) fn resolve(&mut self, encoder: &mut wgpu::CommandEncoder) {
        let Some(slot) = self.current else { return };
        if self.count < 2 {
            return;
        }
        encoder.resolve_query_set(&self.queries, 0..self.count, &self.resolve, 0);
        encoder.copy_buffer_to_buffer(
            &self.resolve,
            0,
            &self.slots[slot].buffer,
            0,
            self.count as u64 * 8,
        );
        self.slots[slot].labels = std::mem::take(&mut self.labels);
    }

    /// `submit` 之后：发起异步映射。
    pub(crate) fn after_submit(&mut self) {
        let Some(slot) = self.current.take() else {
            return;
        };
        let entry = &mut self.slots[slot];
        if entry.labels.len() < 2 {
            return;
        }
        let ready = entry.ready.clone();
        ready.store(false, Ordering::Release);
        entry.in_flight = true;
        let bytes = entry.labels.len() as u64 * 8;
        entry
            .buffer
            .map_async(wgpu::MapMode::Read, 0..bytes, move |result| {
                if result.is_ok() {
                    ready.store(true, Ordering::Release);
                }
            });
    }

    /// 读出已经映射好的那几块，更新 [`latest`](Self::latest)。
    fn collect(&mut self) {
        for slot in &mut self.slots {
            if !slot.in_flight || !slot.ready.load(Ordering::Acquire) {
                continue;
            }
            let bytes = slot.labels.len() as u64 * 8;
            let stamps: Vec<u64> = match slot.buffer.slice(0..bytes).get_mapped_range() {
                Ok(view) => view
                    .chunks_exact(8)
                    .map(|c| u64::from_le_bytes(c.try_into().unwrap()))
                    .collect(),
                Err(_) => Vec::new(),
            };
            slot.buffer.unmap();
            slot.in_flight = false;
            // 第 i 段 = 第 i 个标记到第 i+1 个标记；最后一个标记是帧尾，不单独成段。
            self.latest = slot
                .labels
                .iter()
                .zip(stamps.windows(2))
                .map(|(label, pair)| {
                    let ticks = pair[1].saturating_sub(pair[0]);
                    (*label, ticks as f32 * self.period_ns / 1.0e6)
                })
                .collect();
        }
    }

    /// 最近一次读回的各段耗时（毫秒）。
    pub(crate) fn latest(&self) -> &[(&'static str, f32)] {
        &self.latest
    }
}
