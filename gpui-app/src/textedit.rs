//! A caret and a selection over one `String`, plus the edits that move them.
//!
//! Deliberately GPUI-free: the rules are the fiddly part (byte offsets that
//! must land on char boundaries, a selection that edits replace), and keeping
//! them here makes them testable without a window.
//!
//! `editor::TextArea` still carries its own copy of this logic for the
//! multi-line overlay editor; it should move onto this type too.

#[derive(Default)]
pub struct TextEdit {
    pub text: String,
    /// Caret byte offset. Always on a char boundary.
    pub cursor: usize,
    /// Selection anchor byte offset — `None` when nothing is selected. The
    /// selected range is anchor..cursor in either direction.
    pub anchor: Option<usize>,
}

impl TextEdit {
    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
        self.anchor = None;
    }

    /// Replace the whole buffer, parking the caret at the end.
    pub fn set_text(&mut self, text: String) {
        self.text = text;
        self.cursor = self.text.len();
        self.anchor = None;
    }

    /// The selected byte range, low..high. `None` when the anchor is absent
    /// or collapsed onto the caret.
    pub fn selection(&self) -> Option<(usize, usize)> {
        let a = self.anchor?;
        if a == self.cursor {
            return None;
        }
        Some((a.min(self.cursor), a.max(self.cursor)))
    }

    pub fn selected_text(&self) -> Option<&str> {
        self.selection().map(|(s, e)| &self.text[s..e])
    }

    pub fn select_all(&mut self) {
        if self.text.is_empty() {
            return;
        }
        self.anchor = Some(0);
        self.cursor = self.text.len();
    }

    /// Returns whether anything was deleted, so callers can fall back to a
    /// single-character delete.
    pub fn delete_selection(&mut self) -> bool {
        if let Some((s, e)) = self.selection() {
            self.text.replace_range(s..e, "");
            self.cursor = s;
            self.anchor = None;
            true
        } else {
            false
        }
    }

    /// Insert at the caret, replacing the selection if there is one.
    pub fn insert(&mut self, s: &str) {
        self.delete_selection();
        self.text.insert_str(self.cursor, s);
        self.cursor += s.len();
    }

    /// Delete the selection, else the character before the caret.
    pub fn backspace(&mut self) {
        if self.delete_selection() || self.cursor == 0 {
            return;
        }
        let p = self.prev_boundary(self.cursor);
        self.text.replace_range(p..self.cursor, "");
        self.cursor = p;
    }

    /// Delete the selection, else the character after the caret.
    pub fn delete_forward(&mut self) {
        if self.delete_selection() || self.cursor >= self.text.len() {
            return;
        }
        let n = self.next_boundary(self.cursor);
        self.text.replace_range(self.cursor..n, "");
    }

    pub fn prev_boundary(&self, from: usize) -> usize {
        let mut i = from.min(self.text.len());
        while i > 0 {
            i -= 1;
            if self.text.is_char_boundary(i) {
                return i;
            }
        }
        0
    }

    pub fn next_boundary(&self, from: usize) -> usize {
        let mut i = from.min(self.text.len());
        while i < self.text.len() {
            i += 1;
            if self.text.is_char_boundary(i) {
                return i;
            }
        }
        self.text.len()
    }

    /// Move the caret. `select` extends the selection, dropping the anchor
    /// when it is not set; without it the selection collapses.
    pub fn move_cursor(&mut self, to: usize, select: bool) {
        if select {
            if self.anchor.is_none() {
                self.anchor = Some(self.cursor);
            }
        } else {
            self.anchor = None;
        }
        self.cursor = to.min(self.text.len());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(text: &str, cursor: usize) -> TextEdit {
        TextEdit { text: text.into(), cursor, anchor: None }
    }

    #[test]
    fn insert_lands_at_the_caret_not_the_end() {
        let mut t = at("user", 2);
        t.insert("XY");
        assert_eq!(t.text, "usXYer");
        assert_eq!(t.cursor, 4, "caret follows the inserted text");
    }

    #[test]
    fn insert_replaces_a_selection() {
        let mut t = at("user", 0);
        t.select_all();
        t.insert("post");
        assert_eq!(t.text, "post");
        assert_eq!(t.cursor, 4);
        assert!(t.anchor.is_none(), "the selection is consumed");
    }

    #[test]
    fn backspace_takes_the_selection_whole_else_one_char() {
        let mut t = at("user", 4);
        t.backspace();
        assert_eq!(t.text, "use");

        let mut t = at("user", 0);
        t.select_all();
        t.backspace();
        assert_eq!(t.text, "");

        // A no-op at the start rather than an underflow.
        let mut t = at("user", 0);
        t.backspace();
        assert_eq!(t.text, "user");
    }

    #[test]
    fn delete_forward_takes_the_char_after_the_caret() {
        let mut t = at("user", 0);
        t.delete_forward();
        assert_eq!(t.text, "ser");

        // A no-op at the end.
        let mut t = at("user", 4);
        t.delete_forward();
        assert_eq!(t.text, "user");
    }

    /// Byte offsets, so a multi-byte glyph must move as one unit — stepping
    /// by bytes would slice a char in half and panic.
    #[test]
    fn boundaries_step_over_whole_characters() {
        let t = at("한글", 0);
        assert_eq!(t.next_boundary(0), 3);
        assert_eq!(t.next_boundary(3), 6);
        assert_eq!(t.prev_boundary(6), 3);
        assert_eq!(t.prev_boundary(3), 0);

        let mut t = at("한글", 6);
        t.backspace();
        assert_eq!(t.text, "한");
    }

    #[test]
    fn move_cursor_extends_or_collapses_the_selection() {
        let mut t = at("user", 0);
        t.move_cursor(2, true);
        assert_eq!(t.selection(), Some((0, 2)));
        assert_eq!(t.selected_text(), Some("us"));

        // Extending again keeps the original anchor.
        t.move_cursor(4, true);
        assert_eq!(t.selection(), Some((0, 4)));

        // Moving without `select` drops it.
        t.move_cursor(1, false);
        assert_eq!(t.selection(), None);
    }

    #[test]
    fn selection_is_ordered_regardless_of_drag_direction() {
        let mut t = at("user", 4);
        t.move_cursor(1, true);
        assert_eq!(t.selection(), Some((1, 4)), "anchor after cursor still reads low..high");
    }

    #[test]
    fn move_cursor_is_clamped_to_the_text() {
        let mut t = at("user", 0);
        t.move_cursor(99, false);
        assert_eq!(t.cursor, 4);
    }

    #[test]
    fn select_all_on_an_empty_buffer_selects_nothing() {
        let mut t = at("", 0);
        t.select_all();
        assert_eq!(t.selection(), None);
    }
}
