//! Character cursor helpers for the lexer.

use crate::span::Span;

pub(crate) struct Cursor<'a> {
    src: &'a str,
    pos: usize,
}

impl<'a> Cursor<'a> {
    pub(crate) fn new(src: &'a str) -> Self {
        Cursor { src, pos: 0 }
    }

    pub(crate) fn pos(&self) -> usize {
        self.pos
    }

    pub(crate) fn set_pos(&mut self, pos: usize) {
        self.pos = pos;
    }

    pub(crate) fn peek_char(&self) -> Option<char> {
        self.src[self.pos..].chars().next()
    }

    pub(crate) fn peek_char_at(&self, offset: usize) -> Option<char> {
        let idx = self.pos + offset;
        if idx >= self.src.len() || !self.src.is_char_boundary(idx) {
            return None;
        }
        self.src[idx..].chars().next()
    }

    pub(crate) fn bump_char(&mut self) -> char {
        let c = self.peek_char().expect("bump past end of input");
        self.pos += c.len_utf8();
        c
    }

    pub(crate) fn remaining(&self) -> &'a str {
        &self.src[self.pos..]
    }

    pub(crate) fn slice(&self, start: usize, end: usize) -> &'a str {
        &self.src[start..end]
    }

    pub(crate) fn span(&self, start: usize, end: usize) -> Span {
        Span::new(start as u32, end as u32)
    }

    pub(crate) fn is_at_end(&self) -> bool {
        self.pos >= self.src.len()
    }
}
