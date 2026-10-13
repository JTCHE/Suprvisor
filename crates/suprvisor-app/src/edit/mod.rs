//! A single-line text box model, with the keys of a Windows edit control: caret and selection,
//! word moves (Ctrl+Left/Right), word delete (Ctrl+Backspace/Delete), select all, undo.
//! Positions are byte offsets on char boundaries. No Win32 here, so the logic is testable.

#[derive(Clone, Default)]
pub struct Edit {
    pub text: String,
    pub caret: usize,
    /// The other end of the selection; equal to `caret` when nothing is selected.
    pub anchor: usize,
    undo: Vec<(String, usize, usize)>,
    typing: bool, // the last change was a typed char: the next one joins its undo step
}

#[derive(PartialEq, Clone, Copy)]
enum Class {
    Space,
    Word,
    Punct,
}

fn class(c: char) -> Class {
    if c.is_whitespace() {
        Class::Space
    } else if c.is_alphanumeric() || c == '_' {
        Class::Word
    } else {
        Class::Punct
    }
}

impl Edit {
    pub fn selection(&self) -> (usize, usize) {
        (self.caret.min(self.anchor), self.caret.max(self.anchor))
    }

    pub fn selected(&self) -> &str {
        let (a, b) = self.selection();
        &self.text[a..b]
    }

    fn prev(&self, i: usize) -> usize {
        self.text[..i].char_indices().next_back().map_or(0, |(j, _)| j)
    }

    fn next(&self, i: usize) -> usize {
        self.text[i..].chars().next().map_or(i, |c| i + c.len_utf8())
    }

    /// Start of the word before `i`: skips spaces, then one run of the same class.
    pub fn word_left(&self, mut i: usize) -> usize {
        let cls = |i: usize| self.text[..i].chars().next_back().map(class);
        while cls(i) == Some(Class::Space) {
            i = self.prev(i);
        }
        if let Some(k) = cls(i) {
            while cls(i) == Some(k) {
                i = self.prev(i);
            }
        }
        i
    }

    /// Start of the next word after `i`: skips one run of the same class, then spaces.
    pub fn word_right(&self, mut i: usize) -> usize {
        let cls = |i: usize| self.text[i..].chars().next().map(class);
        if let Some(k) = cls(i).filter(|&k| k != Class::Space) {
            while cls(i) == Some(k) {
                i = self.next(i);
            }
        }
        while cls(i) == Some(Class::Space) {
            i = self.next(i);
        }
        i
    }

    /// The word around `i` (double-click).
    pub fn word_at(&mut self, i: usize) {
        let k = self.text[i..].chars().next().or_else(|| self.text[..i].chars().next_back()).map(class);
        let (mut a, mut b) = (i, i);
        while self.text[..a].chars().next_back().map(class) == k && k.is_some() {
            a = self.prev(a);
        }
        while self.text[b..].chars().next().map(class) == k && k.is_some() {
            b = self.next(b);
        }
        (self.anchor, self.caret) = (a, b);
    }

    /// Moves the caret to `i`; with `extend`, the selection grows from the anchor.
    pub fn move_to(&mut self, i: usize, extend: bool) {
        self.caret = i.min(self.text.len());
        if !extend {
            self.anchor = self.caret;
        }
        self.typing = false;
    }

    /// Left or Right, with Ctrl (`word`) and Shift (`extend`). Without Shift, a selection collapses
    /// to its side first.
    pub fn arrow(&mut self, right: bool, word: bool, extend: bool) {
        let (a, b) = self.selection();
        let i = match (right, word) {
            _ if !extend && !word && a != b => {
                if right { b } else { a }
            }
            (false, false) => self.prev(self.caret),
            (true, false) => self.next(self.caret),
            (false, true) => self.word_left(self.caret),
            (true, true) => self.word_right(self.caret),
        };
        self.move_to(i, extend);
    }

    pub fn select_all(&mut self) {
        (self.anchor, self.caret) = (0, self.text.len());
    }

    fn save(&mut self) {
        self.undo.push((self.text.clone(), self.caret, self.anchor));
        if self.undo.len() > 100 {
            self.undo.remove(0);
        }
    }

    /// Replaces the selection with `s` (typing, paste).
    pub fn insert(&mut self, s: &str, typed: bool) {
        if !(typed && self.typing) {
            self.save();
        }
        let (a, b) = self.selection();
        self.text.replace_range(a..b, s);
        self.caret = a + s.len();
        self.anchor = self.caret;
        self.typing = typed && !s.contains(' ');
    }

    /// Backspace (`forward` false) or Delete, a char or (`word`) to the word edge. Returns whether
    /// the text changed.
    pub fn delete(&mut self, forward: bool, word: bool) -> bool {
        let (mut a, mut b) = self.selection();
        if a == b {
            match (forward, word) {
                (false, false) => a = self.prev(a),
                (false, true) => a = self.word_left(a),
                (true, false) => b = self.next(b),
                (true, true) => b = self.word_right(b),
            }
        }
        if a == b {
            return false;
        }
        self.save();
        self.text.replace_range(a..b, "");
        (self.caret, self.anchor, self.typing) = (a, a, false);
        true
    }

    pub fn undo(&mut self) -> bool {
        let Some((t, c, a)) = self.undo.pop() else { return false };
        (self.text, self.caret, self.anchor, self.typing) = (t, c, a, false);
        true
    }

    /// Empties the box (Esc, the clear button). Undo brings the text back.
    pub fn clear(&mut self) {
        if !self.text.is_empty() {
            self.save();
        }
        self.text.clear();
        (self.caret, self.anchor, self.typing) = (0, 0, false);
    }

    /// Byte offset of UTF-16 index `u` (DirectWrite positions).
    pub fn from_utf16(&self, u: u32) -> usize {
        let mut n = 0;
        for (i, c) in self.text.char_indices() {
            if n >= u as usize {
                return i;
            }
            n += c.len_utf16();
        }
        self.text.len()
    }

    /// UTF-16 index of byte offset `i`.
    pub fn utf16(&self, i: usize) -> u32 {
        self.text[..i].encode_utf16().count() as u32
    }
}

#[cfg(test)]
mod tests {
    use super::Edit;

    fn edit(s: &str) -> Edit {
        let mut e = Edit::default();
        e.insert(s, false);
        e
    }

    #[test]
    fn keys_of_an_edit_control() {
        let mut e = edit("visual studio.exe");
        e.delete(false, true); // Ctrl+Backspace: "exe"
        assert_eq!(e.text, "visual studio.");
        e.delete(false, true); // the dot
        e.delete(false, true); // "studio"
        assert_eq!(e.text, "visual ");
        e.delete(false, true);
        assert_eq!(e.text, "");
        assert!(!e.delete(false, true));

        let mut e = edit("foo bar baz");
        e.arrow(false, true, false);
        assert_eq!(e.caret, 8, "Ctrl+Left to the start of baz");
        e.arrow(false, true, true);
        assert_eq!(e.selected(), "bar ");
        e.arrow(true, false, false);
        assert_eq!((e.caret, e.anchor), (8, 8), "Right collapses the selection to its end");
        e.move_to(0, false);
        e.arrow(true, true, false);
        assert_eq!(e.caret, 4, "Ctrl+Right to the start of the next word");
        e.select_all();
        e.insert("q", true);
        e.insert("w", true);
        assert_eq!(e.text, "qw", "typing replaces the selection");
        e.undo();
        assert_eq!(e.text, "foo bar baz", "one undo step for the typed run");
        e.word_at(5);
        assert_eq!(e.selected(), "bar");

        let mut e = edit("é€x");
        e.arrow(false, false, false);
        e.arrow(false, false, false);
        assert_eq!(e.utf16(e.caret), 1);
        assert_eq!(e.from_utf16(2), "é€".len());
        e.delete(true, false);
        assert_eq!(e.text, "éx");
    }
}
