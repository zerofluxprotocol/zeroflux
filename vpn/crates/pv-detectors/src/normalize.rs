//! Text views with byte-offset maps back to the original buffer.
//!
//! Detectors may run on a normalized view (NFKC, confusables folded, zero-width
//! characters removed, case-folded, separators stripped). Every byte of the view
//! records the original byte range it came from, so findings map back exactly.

use unicode_normalization::UnicodeNormalization;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct NormOpts {
    /// NFKC + confusable folding + zero-width removal.
    #[serde(default)]
    pub unicode: bool,
    #[serde(default)]
    pub casefold: bool,
    /// Collapse runs of whitespace to one space.
    #[serde(default)]
    pub whitespace: bool,
    /// Drop separators: space, '-', '_', '.', '/', ':'.
    #[serde(default)]
    pub separators: bool,
}

impl NormOpts {
    pub fn is_identity(&self) -> bool {
        !self.unicode && !self.casefold && !self.whitespace && !self.separators
    }
    pub fn confusables() -> Self {
        NormOpts { unicode: true, ..Default::default() }
    }
}

#[derive(Debug, Clone)]
pub struct View {
    pub text: String,
    /// For each byte of `text`: original (start, end) byte range.
    starts: Vec<usize>,
    ends: Vec<usize>,
}

impl View {
    pub fn identity(s: &str) -> View {
        let mut starts = Vec::with_capacity(s.len());
        let mut ends = Vec::with_capacity(s.len());
        for (i, c) in s.char_indices() {
            for _ in 0..c.len_utf8() {
                starts.push(i);
                ends.push(i + c.len_utf8());
            }
        }
        View { text: s.to_string(), starts, ends }
    }

    pub(crate) fn from_parts(text: String, starts: Vec<usize>, ends: Vec<usize>) -> View {
        View { text, starts, ends }
    }

    /// Map a view span back to an original byte span on char boundaries.
    pub fn map_span(&self, start: usize, end: usize) -> Option<(usize, usize)> {
        if start >= end || end > self.text.len() {
            return None;
        }
        Some((self.starts[start], self.ends[end - 1]))
    }
}

fn is_zero_width(c: char) -> bool {
    matches!(c, '\u{200B}' | '\u{200C}' | '\u{200D}' | '\u{2060}' | '\u{FEFF}' | '\u{00AD}' | '\u{180E}')
}

/// Small table of Latin look-alikes used to evade pattern matching.
pub fn fold_confusable(c: char) -> char {
    match c {
        'а' => 'a', 'е' => 'e', 'о' => 'o', 'р' => 'p', 'с' => 'c', 'у' => 'y', 'х' => 'x', 'і' => 'i', 'ј' => 'j', 'ѕ' => 's', 'ԁ' => 'd', 'ɡ' => 'g', 'һ' => 'h', 'ⅼ' => 'l',
        'А' => 'A', 'В' => 'B', 'Е' => 'E', 'К' => 'K', 'М' => 'M', 'Н' => 'H', 'О' => 'O', 'Р' => 'P', 'С' => 'C', 'Т' => 'T', 'Х' => 'X', 'І' => 'I', 'Ј' => 'J', 'Ѕ' => 'S',
        'Α' => 'A', 'Β' => 'B', 'Ε' => 'E', 'Ζ' => 'Z', 'Η' => 'H', 'Ι' => 'I', 'Κ' => 'K', 'Μ' => 'M', 'Ν' => 'N', 'Ο' => 'O', 'Ρ' => 'P', 'Τ' => 'T', 'Υ' => 'Y', 'Χ' => 'X',
        'ο' => 'o', 'ν' => 'v', 'α' => 'a', 'ι' => 'i', 'κ' => 'k', 'ρ' => 'p', 'τ' => 't', 'υ' => 'u', 'χ' => 'x',
        '‐' | '‑' | '‒' | '–' | '—' | '―' | '−' | '﹣' | '－' => '-',
        '＠' | '﹫' => '@',
        _ => c,
    }
}

fn is_sep(c: char) -> bool {
    matches!(c, ' ' | '-' | '_' | '.' | '/' | ':' | '\t')
}

pub fn view(s: &str, opts: NormOpts) -> View {
    if opts.is_identity() {
        return View::identity(s);
    }
    let mut text = String::with_capacity(s.len());
    let mut starts = Vec::with_capacity(s.len());
    let mut ends = Vec::with_capacity(s.len());
    let mut last_space = false;
    let push = |text: &mut String, starts: &mut Vec<usize>, ends: &mut Vec<usize>, c: char, a: usize, b: usize| {
        text.push(c);
        for _ in 0..c.len_utf8() {
            starts.push(a);
            ends.push(b);
        }
    };
    for (i, c) in s.char_indices() {
        let j = i + c.len_utf8();
        let mut chars: Vec<char> = Vec::with_capacity(2);
        if opts.unicode {
            if is_zero_width(c) {
                continue;
            }
            for n in c.to_string().nfkc() {
                chars.push(fold_confusable(n));
            }
        } else {
            chars.push(c);
        }
        for mut ch in chars {
            if opts.casefold {
                let lower: Vec<char> = ch.to_lowercase().collect();
                if lower.len() == 1 {
                    ch = lower[0];
                } else {
                    for l in lower {
                        push(&mut text, &mut starts, &mut ends, l, i, j);
                    }
                    last_space = false;
                    continue;
                }
            }
            if opts.separators && is_sep(ch) {
                continue;
            }
            if opts.whitespace && ch.is_whitespace() {
                if last_space {
                    continue;
                }
                last_space = true;
                push(&mut text, &mut starts, &mut ends, ' ', i, j);
                continue;
            }
            last_space = false;
            push(&mut text, &mut starts, &mut ends, ch, i, j);
        }
    }
    View { text, starts, ends }
}

/// Normalize a standalone value (for registering exact secrets).
pub fn normalize(s: &str, opts: NormOpts) -> String {
    let v = view(s, opts).text;
    if opts.whitespace {
        v.trim().to_string()
    } else {
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_maps_bytes() {
        let v = View::identity("aé b");
        assert_eq!(v.map_span(1, 3), Some((1, 3)));
    }

    #[test]
    fn confusables_and_zero_width() {
        let s = "ѕk-\u{200B}АBC"; // Cyrillic s, zero-width, Cyrillic A
        let v = view(s, NormOpts::confusables());
        assert_eq!(v.text, "sk-ABC");
        let (a, b) = v.map_span(0, v.text.len()).unwrap();
        assert_eq!(&s[a..b], s);
    }

    #[test]
    fn fullwidth_digits_nfkc() {
        let s = "１２３-４５-６７８９";
        let v = view(s, NormOpts::confusables());
        assert_eq!(v.text, "123-45-6789");
        let (a, b) = v.map_span(0, 3).unwrap();
        assert_eq!(&s[a..b], "１２３");
    }

    #[test]
    fn separators_and_case() {
        let o = NormOpts { casefold: true, separators: true, ..Default::default() };
        assert_eq!(normalize("AB-12_cd.9", o), "ab12cd9");
        let s = "x AB 12-CD9 y";
        let v = view(s, o);
        let pos = v.text.find("ab12cd9").unwrap();
        let (a, b) = v.map_span(pos, pos + 7).unwrap();
        assert_eq!(&s[a..b], "AB 12-CD9");
    }

    #[test]
    fn whitespace_collapse() {
        let o = NormOpts { whitespace: true, ..Default::default() };
        assert_eq!(view("a  \n b", o).text, "a b");
    }
}
