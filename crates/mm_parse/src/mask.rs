//! Span-consumption bookkeeping (§3.1).
//!
//! Extractors run in priority order over the same working text; each claims a
//! byte range and marks it consumed. Byte ranges only ever come from `regex`
//! or `aho-corasick` matches (both guarantee UTF-8 char-boundary-aligned
//! offsets), so residual ranges computed from the complement are always safe
//! to slice.

use std::ops::Range;

#[derive(Debug, Clone)]
pub struct Consumption {
    len: usize,
    consumed: Vec<bool>,
}

impl Consumption {
    pub fn new(text: &str) -> Self {
        Consumption {
            len: text.len(),
            consumed: vec![false; text.len()],
        }
    }

    /// Mark `range` as consumed. No-op (rather than panicking) if `range` is
    /// out of bounds — defensive, since §3.4 requires the parser to never
    /// panic regardless of extractor bugs on adversarial input.
    pub fn mark(&mut self, range: Range<usize>) {
        let end = range.end.min(self.len);
        let start = range.start.min(end);
        for b in &mut self.consumed[start..end] {
            *b = true;
        }
    }

    /// `true` if every byte in `range` is currently unconsumed.
    pub fn is_free(&self, range: Range<usize>) -> bool {
        if range.start >= range.end {
            return true; // empty range: vacuously free
        }
        let end = range.end.min(self.len);
        if range.start >= end {
            return true;
        }
        self.consumed[range.start..end].iter().all(|b| !*b)
    }

    /// Maximal unconsumed byte ranges, in order. Each range's edges coincide
    /// with either the string boundary or the edge of a consumed range, both
    /// of which are guaranteed char-boundary-safe.
    pub fn residual_ranges(&self) -> Vec<Range<usize>> {
        let mut out = Vec::new();
        let mut i = 0;
        while i < self.len {
            if self.consumed[i] {
                i += 1;
                continue;
            }
            let start = i;
            while i < self.len && !self.consumed[i] {
                i += 1;
            }
            out.push(start..i);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn residual_ranges_around_a_consumed_middle() {
        let text = "Movie Title 1080p Extra";
        let mut c = Consumption::new(text);
        let start = text.find("1080p").unwrap();
        c.mark(start..start + 5);
        let ranges = c.residual_ranges();
        let strs: Vec<&str> = ranges.iter().map(|r| &text[r.clone()]).collect();
        assert_eq!(strs, vec!["Movie Title ", " Extra"]);
    }

    #[test]
    fn mark_out_of_bounds_does_not_panic() {
        let mut c = Consumption::new("abc");
        c.mark(0..1000);
        assert!(c.residual_ranges().is_empty());
    }
}
