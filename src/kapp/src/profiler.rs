//! 内置的帧剖析面板：按 F3 打开。
//!
//! 一帧里 CPU 时间花在哪（脚本、逻辑、物理、变换、渲染器各段，按调用层级）、
//! GPU 上每个 pass 多久（适配器支持时间戳查询时），以及最近两百帧的帧时间折线。
//!
//! 剖析关着时 `klog::profile!` 不计时，几乎没有代价；打开面板才开始计。
//! 量性能时记得关垂直同步（`KENGINE_PRESENT=immediate`），不然帧时间被锁在刷新率上。

use klog::profile::FrameProfile;
use kmath::{Vec2, Vec4};
use kui::{Rect, Ui};

/// 帧时间折线保留多少帧。
const HISTORY: usize = 200;

/// 剖析器状态：开没开、最近一帧的结果、帧时间历史。
#[derive(Debug, Default)]
pub struct Profiler {
    visible: bool,
    font_tried: bool,
    cpu: FrameProfile,
    gpu: Vec<(&'static str, f32)>,
    /// 最近若干帧的帧间隔（毫秒），新的在后。
    history: std::collections::VecDeque<f32>,
}

impl Profiler {
    /// 面板开着没有。
    pub fn is_visible(&self) -> bool {
        self.visible
    }

    /// 开关面板（同时开关计时）。
    pub fn set_visible(&mut self, visible: bool) {
        self.visible = visible;
        klog::profile::set_enabled(visible);
        if !visible {
            self.cpu = FrameProfile::default();
            self.gpu.clear();
            self.history.clear();
        }
    }

    /// 最近一帧的 CPU 分段。面板关着时是空的。
    pub fn cpu(&self) -> &FrameProfile {
        &self.cpu
    }

    /// 最近读回的 GPU 分段 `(段名, 毫秒)`。适配器不支持或面板关着时是空的。
    pub fn gpu(&self) -> &[(&'static str, f32)] {
        &self.gpu
    }

    /// 最近若干帧的帧间隔（毫秒），新的在后。
    pub fn history(&self) -> impl Iterator<Item = f32> + '_ {
        self.history.iter().copied()
    }

    /// 一帧结束：收下这一帧的数据。
    pub(crate) fn record(&mut self, frame_ms: f32, cpu: FrameProfile, gpu: &[(&'static str, f32)]) {
        if !self.visible {
            return;
        }
        self.cpu = cpu;
        self.gpu = gpu.to_vec();
        self.history.push_back(frame_ms);
        while self.history.len() > HISTORY {
            self.history.pop_front();
        }
    }

    /// 画面板。要在 UI 这一帧收尾之前调。
    pub(crate) fn draw(&mut self, ui: &mut Ui) {
        if !self.visible {
            return;
        }
        if !ui.has_font() && !self.font_tried {
            self.font_tried = true;
            if let Some(path) = kfont::system_font()
                && let Ok(font) = kfont::Font::from_file(path)
            {
                ui.add_font(font);
            }
        }

        let style = kfont::TextStyle {
            size: 13.0,
            ..Default::default()
        };
        let line = 18.0;
        let width = 340.0;
        let screen = ui.screen();
        let x0 = (screen.x - width - 12.0).max(0.0);
        let mut y = 12.0;

        // 先量一下要多高，再画底板。
        let rows = 3
            + self.cpu.entries.len()
            + if self.gpu.is_empty() {
                1
            } else {
                self.gpu.len() + 1
            };
        let chart = 60.0;
        let height = 16.0 + chart + rows as f32 * line + 12.0;
        let panel = Rect {
            min: Vec2::new(x0, y),
            max: Vec2::new(x0 + width, y + height),
        };
        ui.rounded_rect(panel, 8.0, srgb(0x15171c, 0.92));
        ui.border(panel, 8.0, 1.0, Vec4::new(1.0, 1.0, 1.0, 0.10));
        let x = x0 + 12.0;
        y += 8.0;

        let text = srgb(0xeceef3, 1.0);
        let dim = srgb(0xa3aabb, 1.0);
        let accent = srgb(0x3d8bff, 1.0);
        let warn = srgb(0xffb347, 1.0);

        // ── 标题：帧时间 ──
        let recent: Vec<f32> = self.history.iter().rev().take(30).copied().collect();
        let average = if recent.is_empty() {
            0.0
        } else {
            recent.iter().sum::<f32>() / recent.len() as f32
        };
        let worst = recent.iter().copied().fold(0.0f32, f32::max);
        let fps = if average > 0.0 { 1000.0 / average } else { 0.0 };
        ui.text(
            Vec2::new(x, y),
            &format!("帧 {average:.2} ms（{fps:.0} fps）  最慢 {worst:.2} ms   F3 关闭"),
            &style,
            text,
            None,
        );
        y += line + 4.0;

        // ── 折线：最近两百帧 ──
        let chart_rect = Rect {
            min: Vec2::new(x, y),
            max: Vec2::new(x0 + width - 12.0, y + chart),
        };
        ui.rect(chart_rect, Vec4::new(1.0, 1.0, 1.0, 0.04));
        let scale = self.history.iter().copied().fold(16.7f32, f32::max) * 1.1;
        // 16.7 ms（60 fps）那条参考线。
        let guide = chart_rect.max.y - chart * (16.7 / scale);
        ui.segment(
            Vec2::new(chart_rect.min.x, guide),
            Vec2::new(chart_rect.max.x, guide),
            1.0,
            Vec4::new(1.0, 1.0, 1.0, 0.15),
        );
        let step = (chart_rect.max.x - chart_rect.min.x) / (HISTORY - 1) as f32;
        let offset = HISTORY - self.history.len();
        let points: Vec<Vec2> = self
            .history
            .iter()
            .enumerate()
            .map(|(i, ms)| {
                Vec2::new(
                    chart_rect.min.x + (i + offset) as f32 * step,
                    chart_rect.max.y - chart * (ms / scale).min(1.0),
                )
            })
            .collect();
        if points.len() >= 2 {
            ui.polyline(&points, 1.5, accent);
        }
        y += chart + 8.0;

        // ── CPU ──
        ui.text(Vec2::new(x, y), "CPU", &style, dim, None);
        y += line;
        let frame_ms = self
            .cpu
            .entries
            .first()
            .map_or(0.0, |e| e.ms() as f32)
            .max(1e-3);
        let bar_x = x0 + width - 110.0;
        for entry in &self.cpu.entries {
            let ms = entry.ms() as f32;
            let indent = entry.depth() as f32 * 12.0;
            let label = if entry.calls > 1 {
                format!("{} ×{}", entry.name(), entry.calls)
            } else {
                entry.name().to_string()
            };
            ui.text(
                Vec2::new(x + indent, y),
                &label,
                &style,
                if entry.depth() == 0 { text } else { dim },
                Some(bar_x - x - indent - 50.0),
            );
            ui.text(
                Vec2::new(bar_x - 48.0, y),
                &format!("{ms:6.2}"),
                &style,
                text,
                None,
            );
            let fraction = (ms / frame_ms).clamp(0.0, 1.0);
            let bar = Rect {
                min: Vec2::new(bar_x, y + 4.0),
                max: Vec2::new(bar_x + 98.0 * fraction, y + line - 4.0),
            };
            ui.rect(
                bar,
                if fraction > 0.5 && entry.depth() > 0 {
                    warn
                } else {
                    accent
                },
            );
            y += line;
        }

        // ── GPU ──
        if self.gpu.is_empty() {
            ui.text(
                Vec2::new(x, y),
                "GPU：适配器不支持时间戳查询，或结果还没读回",
                &style,
                dim,
                None,
            );
        } else {
            let total: f32 = self.gpu.iter().map(|(_, ms)| ms).sum();
            ui.text(
                Vec2::new(x, y),
                &format!("GPU  {total:.2} ms"),
                &style,
                dim,
                None,
            );
            y += line;
            for (label, ms) in &self.gpu {
                ui.text(Vec2::new(x + 12.0, y), label, &style, dim, None);
                ui.text(
                    Vec2::new(bar_x - 48.0, y),
                    &format!("{ms:6.2}"),
                    &style,
                    text,
                    None,
                );
                let fraction = (ms / total.max(1e-3)).clamp(0.0, 1.0);
                let bar = Rect {
                    min: Vec2::new(bar_x, y + 4.0),
                    max: Vec2::new(bar_x + 98.0 * fraction, y + line - 4.0),
                };
                ui.rect(bar, accent);
                y += line;
            }
        }
    }
}

/// sRGB 十六进制 → UI 用的线性色。
fn srgb(hex: u32, alpha: f32) -> Vec4 {
    let channel = |shift: u32| {
        let c = ((hex >> shift) & 0xff) as f32 / 255.0;
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    Vec4::new(channel(16), channel(8), channel(0), alpha)
}
