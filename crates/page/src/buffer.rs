use ropey::Rope;
use std::ops::Range;

#[derive(Clone, Debug)]
pub struct Edit {
    pub start: usize,
    pub removed: Rope,
    pub inserted: Rope,
    pub before: (usize, usize),
    pub after: (usize, usize),
    before_state: u64,
    after_state: u64,
}

pub struct Document {
    pub text: Rope,
    pub cursor: usize,
    pub anchor: usize,
    pub revision: u64,
    pub line_ending: &'static str,
    pub changed: Range<usize>,
    undo: Vec<Edit>,
    redo: Vec<Edit>,
    saved: Rope,
    saved_revision: u64,
    state: u64,
    saved_state: u64,
    states: im::OrdMap<u64, u64>,
}

impl Default for Document {
    fn default() -> Self {
        Self::new(Rope::new())
    }
}

impl Document {
    pub fn new(text: Rope) -> Self {
        let line_ending = if text.chars().take(8192).any(|c| c == '\r') {
            "\r\n"
        } else {
            "\n"
        };
        Self {
            saved: text.clone(),
            text,
            cursor: 0,
            anchor: 0,
            revision: 0,
            line_ending,
            changed: 0..1,
            undo: Vec::new(),
            redo: Vec::new(),
            saved_revision: 0,
            state: 0,
            saved_state: 0,
            states: [(0u64, 0u64)].into_iter().collect(),
        }
    }
    pub fn selection(&self) -> Range<usize> {
        self.cursor.min(self.anchor)..self.cursor.max(self.anchor)
    }
    pub fn dirty(&self) -> bool {
        self.state != self.saved_state
    }
    pub fn mark_saved(&mut self, revision: u64, snapshot: Rope) {
        self.saved_revision = revision;
        self.saved_state = self.states.get(&revision).copied().unwrap_or(revision);
        self.saved = snapshot;
    }
    pub fn saved_snapshot(&self) -> Rope {
        self.saved.clone()
    }
    pub fn append_loaded(&mut self, chunk: Rope) {
        self.saved.append(chunk.clone());
        self.text.append(chunk);
    }
    pub fn select(&mut self, cursor: usize, extend: bool) {
        self.cursor = cursor.min(self.text.len_chars());
        if !extend {
            self.anchor = self.cursor;
        }
    }
    pub fn replace(&mut self, range: Range<usize>, text: &str) {
        self.replace_rope(range, Rope::from_str(text));
    }
    pub fn replace_rope(&mut self, range: Range<usize>, inserted: Rope) {
        let changed_line = self.text.char_to_line(range.start);
        let before = (self.cursor, self.anchor);
        let removed = self.text.slice(range.clone()).into();
        let tail = self.text.split_off(range.end);
        self.text.remove(range.start..);
        self.text.append(inserted.clone());
        self.text.append(tail);
        self.cursor = range.start + inserted.len_chars();
        self.anchor = self.cursor;
        self.changed = changed_line..self.text.char_to_line(self.cursor) + 1;
        self.undo.push(Edit {
            start: range.start,
            removed,
            inserted,
            before,
            after: (self.cursor, self.anchor),
            before_state: self.state,
            after_state: self.revision + 1,
        });
        self.redo.clear();
        self.revision += 1;
        self.state = self.revision;
        self.states.insert(self.revision, self.state);
    }
    pub fn insert(&mut self, text: &str) {
        self.replace(self.selection(), text);
    }
    fn apply(&mut self, start: usize, count: usize, replacement: &Rope, caret: (usize, usize)) {
        let changed_line = self.text.char_to_line(start);
        let tail = self.text.split_off(start + count);
        self.text.remove(start..);
        self.text.append(replacement.clone());
        self.text.append(tail);
        (self.cursor, self.anchor) = caret;
        self.changed = changed_line..self.text.char_to_line(start + replacement.len_chars()) + 1;
        self.revision += 1;
    }
    pub fn undo(&mut self) {
        if let Some(edit) = self.undo.pop() {
            self.apply(
                edit.start,
                edit.inserted.len_chars(),
                &edit.removed,
                edit.before,
            );
            self.state = edit.before_state;
            self.states.insert(self.revision, self.state);
            self.redo.push(edit);
        }
    }
    pub fn redo(&mut self) {
        if let Some(edit) = self.redo.pop() {
            self.apply(
                edit.start,
                edit.removed.len_chars(),
                &edit.inserted,
                edit.after,
            );
            self.state = edit.after_state;
            self.states.insert(self.revision, self.state);
            self.undo.push(edit);
        }
    }
    pub fn delete(&mut self, backwards: bool) {
        let mut range = self.selection();
        if range.is_empty() {
            if backwards {
                range.start = range.start.saturating_sub(1);
                if range.start > 0
                    && self.text.char(range.start) == '\n'
                    && self.text.char(range.start - 1) == '\r'
                {
                    range.start -= 1;
                }
            } else {
                range.end = (range.end + 1).min(self.text.len_chars());
                if range.end < self.text.len_chars()
                    && self.text.char(range.start) == '\r'
                    && self.text.char(range.end) == '\n'
                {
                    range.end += 1;
                }
            }
        }
        if !range.is_empty() {
            self.replace(range, "");
        }
    }
    pub fn horizontal(&mut self, right: bool, extend: bool, word: bool) {
        let range = self.selection();
        let mut pos = if !extend && !range.is_empty() {
            if right { range.end } else { range.start }
        } else {
            self.cursor
        };
        if range.is_empty() || extend {
            if right {
                pos = (pos + 1).min(self.text.len_chars());
                if pos < self.text.len_chars()
                    && pos > 0
                    && self.text.char(pos - 1) == '\r'
                    && self.text.char(pos) == '\n'
                {
                    pos += 1;
                }
                if word {
                    while pos < self.text.len_chars() && !self.text.char(pos).is_whitespace() {
                        pos += 1;
                    }
                }
            } else {
                pos = pos.saturating_sub(1);
                if pos > 0 && self.text.char(pos) == '\n' && self.text.char(pos - 1) == '\r' {
                    pos -= 1;
                }
                if word {
                    while pos > 0 && !self.text.char(pos - 1).is_whitespace() {
                        pos -= 1;
                    }
                }
            }
        }
        self.select(pos, extend);
    }
    pub fn enter(&mut self) {
        let start = self.text.line_to_char(self.text.char_to_line(self.cursor));
        let prefix: String = self
            .text
            .slice(start..self.cursor.min(start + 512))
            .chars()
            .collect();
        let indent: String = prefix
            .chars()
            .take_while(|c| *c == ' ' || *c == '\t')
            .collect();
        let trimmed = prefix.trim_start();
        let mut continuation = String::new();
        for marker in ["- [ ] ", "- [x] ", "* [ ] ", "* [x] ", "- ", "* ", "+ "] {
            if let Some(rest) = trimmed.strip_prefix(marker) {
                if rest.is_empty() {
                    self.replace(start..self.cursor, "");
                    return;
                }
                continuation = marker.replace("[x]", "[ ]");
                break;
            }
        }
        if continuation.is_empty() {
            let digits: String = trimmed.chars().take_while(char::is_ascii_digit).collect();
            if let Some(rest) = trimmed
                .strip_prefix(&digits)
                .and_then(|s| s.strip_prefix(". "))
                && !digits.is_empty()
            {
                if rest.is_empty() {
                    self.replace(start..self.cursor, "");
                    return;
                }
                continuation =
                    format!("{}. ", digits.parse::<u64>().unwrap_or(0).saturating_add(1));
            }
        }
        self.insert(&format!("{}{indent}{continuation}", self.line_ending));
    }
    pub fn type_text(&mut self, text: &str) {
        let close = match text {
            "(" => Some(")"),
            "[" => Some("]"),
            "{" => Some("}"),
            "\"" => Some("\""),
            "'" => Some("'"),
            "`" => Some("`"),
            _ => None,
        };
        if text.chars().count() == 1
            && matches!(text, ")" | "]" | "}" | "\"" | "'" | "`")
            && self.selection().is_empty()
            && self.cursor < self.text.len_chars()
            && self.text.char(self.cursor).to_string() == text
        {
            self.select(self.cursor + 1, false);
            return;
        }
        if let Some(close) = close {
            // Apostrophes within words are literal, not quote pairs.
            if text == "'" && self.cursor > 0 && self.text.char(self.cursor - 1).is_alphanumeric() {
                self.insert(text);
                return;
            }
            let selection = self.selection();
            if !selection.is_empty() {
                self.surround(text, close);
            } else {
                self.insert(&format!("{text}{close}"));
                self.cursor -= 1;
                self.anchor = self.cursor;
            }
        } else {
            self.insert(text);
        }
        if let Some(edit) = self.undo.last_mut() {
            edit.after = (self.cursor, self.anchor);
        }
    }
    pub fn surround(&mut self, opening: &str, closing: &str) {
        let range = self.selection();
        let length = range.len();
        let mut inserted = Rope::from_str(opening);
        inserted.append(self.text.slice(range.clone()).into());
        inserted.append(Rope::from_str(closing));
        self.replace_rope(range.clone(), inserted);
        self.anchor = range.start + opening.chars().count();
        self.cursor = self.anchor + length;
        if let Some(edit) = self.undo.last_mut() {
            edit.after = (self.cursor, self.anchor);
        }
    }
    pub fn prefix(&mut self, marker: &str, heading: bool) {
        if self.selection().is_empty() {
            let start = self.text.line_to_char(self.text.char_to_line(self.cursor));
            let sample = self
                .text
                .slice(start..(start + 8).min(self.text.len_chars()))
                .to_string();
            let remove = if heading {
                sample.chars().take_while(|c| *c == '#').count()
            } else {
                0
            };
            let remove = if heading && sample.as_bytes().get(remove) == Some(&b' ') {
                remove + 1
            } else {
                remove
            };
            if !heading && sample.starts_with(marker) {
                self.replace(start..start + marker.chars().count(), "");
            } else {
                self.replace(start..start + remove, marker);
            }
            return;
        }
        let first = self.text.char_to_line(self.selection().start);
        let last = self.text.char_to_line(self.selection().end);
        let start = self.text.line_to_char(first);
        let end = if last + 1 < self.text.len_lines() {
            self.text.line_to_char(last + 1)
        } else {
            self.text.len_chars()
        };
        let source = self.text.slice(start..end).to_string();
        let mut output = String::new();
        for line in source.split_inclusive('\n') {
            let line = if heading {
                line.trim_start_matches('#').trim_start_matches(' ')
            } else {
                line
            };
            if let Some(rest) = line.strip_prefix(marker).filter(|_| !heading) {
                output.push_str(rest);
            } else {
                output.push_str(marker);
                output.push_str(line);
            }
        }
        self.replace(start..end, &output);
    }
    pub fn indent(&mut self, outdent: bool) {
        let range = self.selection();
        if range.is_empty() {
            let start = self.text.line_to_char(self.text.char_to_line(self.cursor));
            let spaces = self
                .text
                .slice(start..(start + 4).min(self.text.len_chars()))
                .chars()
                .take_while(|c| *c == ' ')
                .count();
            if outdent {
                let remove = if start < self.text.len_chars() && self.text.char(start) == '\t' {
                    1
                } else {
                    spaces
                };
                self.replace(start..start + remove, "");
                self.select(range.start.saturating_sub(remove).max(start), false);
            } else {
                self.replace(start..start, "    ");
                self.select(range.start + 4, false);
            }
            if let Some(edit) = self.undo.last_mut() {
                edit.after = (self.cursor, self.anchor);
            }
            return;
        }
        let first = self.text.char_to_line(range.start);
        let last = self.text.char_to_line(range.end);
        let start = self.text.line_to_char(first);
        let end = if last + 1 < self.text.len_lines() {
            self.text.line_to_char(last + 1)
        } else {
            self.text.len_chars()
        };
        let source = self.text.slice(start..end).to_string();
        let output: String = source
            .split_inclusive('\n')
            .map(|line| {
                if outdent {
                    if let Some(rest) = line.strip_prefix('\t') {
                        rest.to_owned()
                    } else {
                        let n = line.bytes().take(4).take_while(|c| *c == b' ').count();
                        line[n..].to_owned()
                    }
                } else {
                    format!("    {line}")
                }
            })
            .collect();
        self.replace(start..end, &output);
        if range.is_empty() {
            self.select(
                (range.start as isize
                    + if outdent {
                        output.len() as isize - source.len() as isize
                    } else {
                        4
                    })
                .max(start as isize) as usize,
                false,
            );
        } else {
            self.anchor = start;
        }
        if let Some(edit) = self.undo.last_mut() {
            edit.after = (self.cursor, self.anchor);
        }
    }
}

/// Whitespace-separated words, like `str::split_whitespace().count()` but without copying the rope.
pub fn word_count(text: ropey::RopeSlice) -> usize {
    let mut inside = false;
    text.chars()
        .filter(|c| {
            let word = !c.is_whitespace();
            let start = word && !inside;
            inside = word;
            start
        })
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn edits_keep_unicode_and_original_endings() {
        let mut doc = Document::new(Rope::from_str("- café\r\n日本語"));
        doc.select(6, false);
        doc.enter();
        doc.type_text("한글");
        assert_eq!(doc.text.to_string(), "- café\r\n- 한글\r\n日本語");
        doc.undo();
        doc.undo();
        assert_eq!(doc.text.to_string(), "- café\r\n日本語");
        doc.redo();
        doc.redo();
        assert!(doc.text.to_string().contains("한글"));
        doc.select(doc.text.len_chars(), false);
        doc.type_text("(");
        doc.type_text(")");
        assert!(doc.text.to_string().ends_with("日本語()"));
    }
    #[test]
    fn deleted_selection_is_logged_without_flattening_the_file() {
        let mut doc = Document::new(Rope::from_str("αβγ\r\nδ"));
        doc.anchor = 1;
        doc.cursor = 5;
        doc.insert("é");
        assert_eq!(doc.text.to_string(), "αéδ");
        doc.undo();
        assert_eq!(doc.text.to_string(), "αβγ\r\nδ");
        assert_eq!(doc.selection(), 1..5);
    }
    #[test]
    fn saved_state_survives_undo_redo_branching_and_delayed_saves() {
        let mut doc = Document::new(Rope::from_str("hello"));
        doc.select(5, false);
        doc.insert("!");
        let revision = doc.revision;
        let saved = doc.text.clone();
        doc.insert("?");
        doc.mark_saved(revision, saved);
        assert!(doc.dirty());
        doc.undo();
        assert!(!doc.dirty());
        doc.redo();
        assert!(doc.dirty());
        doc.undo();
        doc.insert(".");
        assert!(doc.dirty());
        doc.anchor = 0;
        doc.cursor = 5;
        doc.surround("**", "**");
        let selected = doc.selection();
        doc.undo();
        doc.redo();
        assert_eq!(doc.selection(), selected);
    }
    #[test]
    fn counts_words_across_whitespace_and_chunks() {
        assert_eq!(word_count(Rope::from_str("").slice(..)), 0);
        assert_eq!(word_count(Rope::from_str(" a  b\r\nc\t").slice(..)), 3);
        let long = "word ".repeat(5000);
        assert_eq!(word_count(Rope::from_str(&long).slice(..)), 5000);
    }
}
