//! 文本编辑模型：光标、选区、增删改。
//!
//! 纯逻辑，不碰渲染也不碰输入设备——于是最容易出错的那部分
//! （多字节字符上的光标移动）能直接测。
//!
//! # 偏移永远落在字符边界上
//!
//! 光标和选区都是**字节偏移**，因为要拿去切字符串。但 Rust 的字符串
//! 切片在非字符边界上会直接 panic——一个中文字三个字节，退格时
//! 减 1 就炸。所有移动都走 [`prev_boundary`] / [`next_boundary`]，
//! 一次跨过整个字符。
//!
//! # 按字符而不是按字素簇
//!
//! 「é」可以是一个码位，也可以是「e + 组合重音」两个码位；后者按
//! 字符退格会先删掉重音、再删掉 e，两次才删干净。正确做法是按
//! **字素簇**（UAX #29），那需要一张 Unicode 表。
//!
//! 这里按字符，并把这个取舍写在这里：对中英文是对的，对带组合符号的
//! 拉丁扩展、绝大多数表情符号是不对的。

use std::ops::Range;

/// 一个文本框的编辑状态。
///
/// 不持有文本本身——文本归调用方。这样同一份文本可以有多个视图，
/// 而且调用方随时能直接读写它。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TextEdit {
    /// 光标位置（字节偏移）。
    cursor: usize,
    /// 选区的另一端。等于 `cursor` 表示没有选区。
    ///
    /// 分成 cursor / anchor 而不是 start / end：Shift+方向键要从
    /// **锚点**往外扩，两端谁大谁小是会来回换的。
    anchor: usize,
}

impl TextEdit {
    /// 光标在开头、没有选区。
    pub fn new() -> Self {
        Self::default()
    }

    /// 光标位置（字节偏移）。
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// 选区。没有选区时是一个空区间。
    pub fn selection(&self) -> Range<usize> {
        let (a, b) = (self.cursor.min(self.anchor), self.cursor.max(self.anchor));
        a..b
    }

    /// 有没有选中内容。
    pub fn has_selection(&self) -> bool {
        self.cursor != self.anchor
    }

    /// 把光标放到某个偏移，并清掉选区。
    ///
    /// 偏移会被夹到最近的字符边界上——鼠标点击算出来的偏移不保证落在
    /// 边界上，直接用会在下一次切片时 panic。
    pub fn set_cursor(&mut self, text: &str, offset: usize) {
        self.cursor = clamp_boundary(text, offset);
        self.anchor = self.cursor;
    }

    /// 把选区的另一端拖到某个偏移（鼠标拖选）。
    pub fn drag_to(&mut self, text: &str, offset: usize) {
        self.cursor = clamp_boundary(text, offset);
    }

    /// 全选。
    pub fn select_all(&mut self, text: &str) {
        self.anchor = 0;
        self.cursor = text.len();
    }

    /// 清掉选区，光标留在原地。
    pub fn clear_selection(&mut self) {
        self.anchor = self.cursor;
    }

    /// 文本被外部改过之后，把光标夹回合法范围。
    ///
    /// 不夹的话，外部把文本清空之后光标还停在旧位置，下一次切片直接 panic。
    pub fn clamp(&mut self, text: &str) {
        self.cursor = clamp_boundary(text, self.cursor);
        self.anchor = clamp_boundary(text, self.anchor);
    }

    // ───────────────────────── 编辑 ─────────────────────────

    /// 插入一段文本。有选区时先替换掉选区。
    pub fn insert(&mut self, text: &mut String, insert: &str) {
        self.delete_selection(text);
        text.insert_str(self.cursor, insert);
        self.cursor += insert.len();
        self.anchor = self.cursor;
    }

    /// 退格：删选区，没有选区就删光标前**一个字符**。
    pub fn backspace(&mut self, text: &mut String) {
        if self.delete_selection(text) {
            return;
        }
        if self.cursor == 0 {
            return;
        }
        // 减 1 会落在多字节字符中间，切片时 panic。
        let start = prev_boundary(text, self.cursor);
        text.replace_range(start..self.cursor, "");
        self.cursor = start;
        self.anchor = start;
    }

    /// 删除：删选区，没有选区就删光标后一个字符。
    pub fn delete(&mut self, text: &mut String) {
        if self.delete_selection(text) {
            return;
        }
        if self.cursor >= text.len() {
            return;
        }
        let end = next_boundary(text, self.cursor);
        text.replace_range(self.cursor..end, "");
    }

    /// 删掉选区，返回是否真的删了。
    fn delete_selection(&mut self, text: &mut String) -> bool {
        let range = self.selection();
        if range.is_empty() {
            return false;
        }
        text.replace_range(range.clone(), "");
        self.cursor = range.start;
        self.anchor = range.start;
        true
    }

    /// 选中的文本。
    pub fn selected<'a>(&self, text: &'a str) -> &'a str {
        &text[self.selection()]
    }

    // ───────────────────────── 移动 ─────────────────────────

    /// 左移一个字符。`select` 为真时扩展选区。
    pub fn move_left(&mut self, text: &str, select: bool) {
        // 没按 Shift 且有选区时，左键是「跳到选区左端」而不是「再左移一格」。
        // 这是所有文本框的通行手感。
        if !select && self.has_selection() {
            self.cursor = self.selection().start;
        } else {
            self.cursor = prev_boundary(text, self.cursor);
        }
        self.after_move(select);
    }

    /// 右移一个字符。
    pub fn move_right(&mut self, text: &str, select: bool) {
        if !select && self.has_selection() {
            self.cursor = self.selection().end;
        } else {
            self.cursor = next_boundary(text, self.cursor);
        }
        self.after_move(select);
    }

    /// 跳到行首。
    pub fn move_home(&mut self, select: bool) {
        self.cursor = 0;
        self.after_move(select);
    }

    /// 跳到行尾。
    pub fn move_end(&mut self, text: &str, select: bool) {
        self.cursor = text.len();
        self.after_move(select);
    }

    fn after_move(&mut self, select: bool) {
        if !select {
            self.anchor = self.cursor;
        }
    }
}

/// 把偏移夹到 `[0, len]` 内最近的字符边界（向下取）。
fn clamp_boundary(text: &str, offset: usize) -> usize {
    let mut offset = offset.min(text.len());
    while offset > 0 && !text.is_char_boundary(offset) {
        offset -= 1;
    }
    offset
}

/// 从 `offset` 往前跨一个字符。已经在开头时返回 0。
pub fn prev_boundary(text: &str, offset: usize) -> usize {
    let offset = clamp_boundary(text, offset);
    if offset == 0 {
        return 0;
    }
    let mut index = offset - 1;
    while index > 0 && !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

/// 从 `offset` 往后跨一个字符。已经在末尾时返回 `text.len()`。
pub fn next_boundary(text: &str, offset: usize) -> usize {
    let offset = clamp_boundary(text, offset);
    if offset >= text.len() {
        return text.len();
    }
    let mut index = offset + 1;
    while index < text.len() && !text.is_char_boundary(index) {
        index += 1;
    }
    index
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 「中」「文」各三字节，`a` 一字节。
    const MIXED: &str = "中a文";

    fn at_end(text: &str) -> TextEdit {
        let mut edit = TextEdit::new();
        edit.move_end(text, false);
        edit
    }

    #[test]
    fn a_new_edit_starts_at_the_beginning() {
        let edit = TextEdit::new();
        assert_eq!(edit.cursor(), 0);
        assert!(!edit.has_selection());
    }

    #[test]
    fn typing_inserts_at_the_cursor() {
        let mut text = String::from("ac");
        let mut edit = TextEdit::new();
        edit.move_right("ac", false);
        edit.insert(&mut text, "b");
        assert_eq!(text, "abc");
        assert_eq!(edit.cursor(), 2);
    }

    #[test]
    fn backspace_removes_a_whole_multibyte_character() {
        // 光标减 1 会落在多字节字符中间，下一次切片直接 panic。
        let mut text = String::from(MIXED);
        let mut edit = at_end(&text);
        edit.backspace(&mut text);
        assert_eq!(text, "中a");
        edit.backspace(&mut text);
        assert_eq!(text, "中");
        edit.backspace(&mut text);
        assert_eq!(text, "");
    }

    #[test]
    fn delete_removes_a_whole_multibyte_character() {
        let mut text = String::from(MIXED);
        let mut edit = TextEdit::new();
        edit.delete(&mut text);
        assert_eq!(text, "a文");
    }

    #[test]
    fn backspace_at_the_start_does_nothing() {
        let mut text = String::from("abc");
        let mut edit = TextEdit::new();
        edit.backspace(&mut text);
        assert_eq!(text, "abc");
        assert_eq!(edit.cursor(), 0);
    }

    #[test]
    fn delete_at_the_end_does_nothing() {
        let mut text = String::from("abc");
        let mut edit = at_end(&text);
        edit.delete(&mut text);
        assert_eq!(text, "abc");
    }

    #[test]
    fn moving_steps_over_whole_characters() {
        let mut edit = TextEdit::new();
        edit.move_right(MIXED, false);
        assert_eq!(edit.cursor(), 3, "「中」是三个字节");
        edit.move_right(MIXED, false);
        assert_eq!(edit.cursor(), 4, "「a」是一个字节");
        edit.move_right(MIXED, false);
        assert_eq!(edit.cursor(), 7);
        edit.move_right(MIXED, false);
        assert_eq!(edit.cursor(), 7, "到头了就不动");
    }

    #[test]
    fn moving_left_steps_over_whole_characters() {
        let mut edit = at_end(MIXED);
        edit.move_left(MIXED, false);
        assert_eq!(edit.cursor(), 4);
        edit.move_left(MIXED, false);
        assert_eq!(edit.cursor(), 3);
        edit.move_left(MIXED, false);
        assert_eq!(edit.cursor(), 0);
        edit.move_left(MIXED, false);
        assert_eq!(edit.cursor(), 0);
    }

    #[test]
    fn every_cursor_position_is_a_char_boundary() {
        // 不是边界的话，下一次切片就是 panic。
        let mut edit = TextEdit::new();
        for _ in 0..10 {
            edit.move_right(MIXED, false);
            assert!(MIXED.is_char_boundary(edit.cursor()));
        }
    }

    #[test]
    fn shift_arrow_extends_the_selection() {
        let mut edit = TextEdit::new();
        edit.move_right(MIXED, true);
        edit.move_right(MIXED, true);
        assert!(edit.has_selection());
        assert_eq!(edit.selected(MIXED), "中a");
    }

    #[test]
    fn a_plain_arrow_collapses_the_selection_to_its_edge() {
        // 所有文本框的通行手感：有选区时按左键跳到选区左端，
        // 而不是在选区左端的基础上再左移一格。
        let mut edit = TextEdit::new();
        edit.move_end(MIXED, false);
        edit.move_left(MIXED, true);
        edit.move_left(MIXED, true);
        assert!(edit.has_selection());

        edit.move_left(MIXED, false);
        assert!(!edit.has_selection());
        assert_eq!(edit.cursor(), 3, "该跳到选区左端，不是再往左一格");
    }

    #[test]
    fn typing_replaces_the_selection() {
        let mut text = String::from(MIXED);
        let mut edit = TextEdit::new();
        edit.select_all(&text);
        edit.insert(&mut text, "新");
        assert_eq!(text, "新");
        assert!(!edit.has_selection());
    }

    #[test]
    fn backspace_removes_the_selection_not_one_character() {
        let mut text = String::from("abcdef");
        let mut edit = TextEdit::new();
        edit.set_cursor(&text, 1);
        edit.drag_to(&text, 4);
        edit.backspace(&mut text);
        assert_eq!(text, "aef");
        assert_eq!(edit.cursor(), 1);
    }

    #[test]
    fn selection_works_in_both_directions() {
        // 从右往左拖选和从左往右拖选，选中的内容该一样。
        let mut forward = TextEdit::new();
        forward.set_cursor(MIXED, 0);
        forward.drag_to(MIXED, 4);

        let mut backward = TextEdit::new();
        backward.set_cursor(MIXED, 4);
        backward.drag_to(MIXED, 0);

        assert_eq!(forward.selected(MIXED), backward.selected(MIXED));
        assert_eq!(forward.selection(), backward.selection());
    }

    #[test]
    fn clicking_inside_a_character_snaps_to_a_boundary() {
        // 鼠标算出来的偏移不保证落在字符边界上。
        let mut edit = TextEdit::new();
        edit.set_cursor(MIXED, 1); // 「中」的第二个字节
        assert_eq!(edit.cursor(), 0);
        assert!(MIXED.is_char_boundary(edit.cursor()));

        edit.set_cursor(MIXED, 2);
        assert_eq!(edit.cursor(), 0);
    }

    #[test]
    fn clicking_past_the_end_clamps() {
        let mut edit = TextEdit::new();
        edit.set_cursor(MIXED, 9999);
        assert_eq!(edit.cursor(), MIXED.len());
    }

    #[test]
    fn clamp_recovers_after_the_text_is_replaced_externally() {
        // 外部把文本清空之后光标还停在旧位置，下一次切片直接 panic。
        let mut edit = at_end("很长的一段文本");
        edit.clamp("短");
        assert!(edit.cursor() <= "短".len());
        assert!("短".is_char_boundary(edit.cursor()));
    }

    #[test]
    fn home_and_end_jump_to_the_edges() {
        let mut edit = TextEdit::new();
        edit.move_end(MIXED, false);
        assert_eq!(edit.cursor(), MIXED.len());
        edit.move_home(false);
        assert_eq!(edit.cursor(), 0);
    }

    #[test]
    fn shift_home_selects_to_the_start() {
        let mut edit = at_end(MIXED);
        edit.move_home(true);
        assert_eq!(edit.selected(MIXED), MIXED);
    }

    #[test]
    fn inserting_an_ime_string_works_like_typing() {
        // 输入法合成完之后交上来的是一整串，不是一个字符。
        let mut text = String::from("前后");
        let mut edit = TextEdit::new();
        edit.set_cursor(&text, 3);
        edit.insert(&mut text, "中间插入");
        assert_eq!(text, "前中间插入后");
    }

    #[test]
    fn boundaries_are_idempotent_at_the_edges() {
        assert_eq!(prev_boundary("", 0), 0);
        assert_eq!(next_boundary("", 0), 0);
        assert_eq!(next_boundary(MIXED, MIXED.len()), MIXED.len());
        assert_eq!(prev_boundary(MIXED, 0), 0);
    }

    #[test]
    fn an_empty_selection_selects_nothing() {
        let edit = TextEdit::new();
        assert_eq!(edit.selected(MIXED), "");
        assert!(edit.selection().is_empty());
    }
}
