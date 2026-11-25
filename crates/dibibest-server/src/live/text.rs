//! Text changes and their transformation (spike, issue #88).
//!
//! The model is the one of `@codemirror/collab`: a change is made on one version of the text, and a change that
//! was made on an older version is *rebased* over the ones accepted since. The wire form is a list of
//! replacements `[from, to, insert]`, all positions in UTF-16 code units of the text the change was made on
//! (what a browser string and CodeMirror use), sorted and not overlapping. Inside, a change is the classic
//! retain / delete / insert sequence, because that is the form that has a well known, small `transform`.
//!
//! Tie rule: when both changes insert at one position, the first argument of [`transform`] goes first. The
//! server passes the already accepted change first, so an accepted change never moves behind a late one.

use serde::{Deserialize, Serialize};

/// One replacement as it goes over the wire: `[from, to, insert]`.
pub type Change = (u32, u32, String);

#[derive(Debug, Clone, PartialEq, Eq)]
enum Comp {
    Retain(usize),
    Delete(usize),
    Insert(Vec<u16>),
}

/// A change to a text of `base_len` UTF-16 units.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Op {
    comps: Vec<Comp>,
    base_len: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[error("bad change: {0}")]
pub struct BadChange(pub String);

impl Op {
    pub fn base_len(&self) -> usize {
        self.base_len
    }

    /// Length of the text after the change.
    pub fn target_len(&self) -> usize {
        self.comps
            .iter()
            .map(|c| match c {
                Comp::Retain(n) => *n,
                Comp::Delete(_) => 0,
                Comp::Insert(s) => s.len(),
            })
            .sum()
    }

    pub fn is_noop(&self) -> bool {
        self.comps.iter().all(|c| matches!(c, Comp::Retain(_)))
    }

    /// Builds a change from wire replacements made on a text of `base_len` units.
    pub fn from_changes(changes: &[Change], base_len: usize) -> Result<Op, BadChange> {
        let mut op = Builder::default();
        let mut pos = 0usize;
        for (i, (from, to, ins)) in changes.iter().enumerate() {
            let (from, to) = (*from as usize, *to as usize);
            if from > to {
                return Err(BadChange(format!(
                    "change {i}: from {from} is after to {to}"
                )));
            }
            if to > base_len {
                return Err(BadChange(format!(
                    "change {i}: to {to} is past the end of the text ({base_len})"
                )));
            }
            if from < pos {
                return Err(BadChange(format!(
                    "change {i}: overlaps or is out of order (starts at {from}, previous ends at {pos})"
                )));
            }
            op.retain(from - pos);
            op.delete(to - from);
            op.insert(ins.encode_utf16().collect());
            pos = to;
        }
        op.retain(base_len - pos);
        Ok(op.finish(base_len))
    }

    /// The replacements of this change, in the wire form.
    pub fn to_changes(&self) -> Vec<Change> {
        let mut out: Vec<Change> = Vec::new();
        let mut pos = 0usize;
        // The replacement being collected: where it starts, how much it deletes, what it inserts.
        let mut open: Option<(usize, usize, Vec<u16>)> = None;
        let close = |open: &mut Option<(usize, usize, Vec<u16>)>, out: &mut Vec<Change>| {
            if let Some((from, del, ins)) = open.take() {
                out.push((
                    from as u32,
                    (from + del) as u32,
                    String::from_utf16_lossy(&ins),
                ));
            }
        };
        for c in &self.comps {
            match c {
                Comp::Retain(n) => {
                    close(&mut open, &mut out);
                    pos += n;
                }
                Comp::Delete(n) => {
                    let o = open.get_or_insert((pos, 0, Vec::new()));
                    o.1 += n;
                    pos += n;
                }
                Comp::Insert(s) => {
                    let o = open.get_or_insert((pos, 0, Vec::new()));
                    o.2.extend_from_slice(s);
                }
            }
        }
        close(&mut open, &mut out);
        out
    }

    /// Applies the change. The text must have `base_len` units.
    pub fn apply(&self, text: &[u16]) -> Result<Vec<u16>, BadChange> {
        if text.len() != self.base_len {
            return Err(BadChange(format!(
                "the change is for a text of {} units, this one has {}",
                self.base_len,
                text.len()
            )));
        }
        let mut out = Vec::with_capacity(self.target_len());
        let mut pos = 0usize;
        for c in &self.comps {
            match c {
                Comp::Retain(n) => {
                    out.extend_from_slice(&text[pos..pos + n]);
                    pos += n;
                }
                Comp::Delete(n) => pos += n,
                Comp::Insert(s) => out.extend_from_slice(s),
            }
        }
        Ok(out)
    }
}

#[derive(Default)]
struct Builder {
    comps: Vec<Comp>,
}

impl Builder {
    fn retain(&mut self, n: usize) {
        if n == 0 {
            return;
        }
        if let Some(Comp::Retain(m)) = self.comps.last_mut() {
            *m += n;
        } else {
            self.comps.push(Comp::Retain(n));
        }
    }
    fn delete(&mut self, n: usize) {
        if n == 0 {
            return;
        }
        if let Some(Comp::Delete(m)) = self.comps.last_mut() {
            *m += n;
        } else {
            self.comps.push(Comp::Delete(n));
        }
    }
    fn insert(&mut self, s: Vec<u16>) {
        if s.is_empty() {
            return;
        }
        if let Some(Comp::Insert(m)) = self.comps.last_mut() {
            m.extend_from_slice(&s);
        } else {
            self.comps.push(Comp::Insert(s));
        }
    }
    fn finish(self, base_len: usize) -> Op {
        Op {
            comps: self.comps,
            base_len,
        }
    }
}

/// A cursor over the components of a change that can take part of a retain or delete.
struct Cursor<'a> {
    comps: &'a [Comp],
    i: usize,
    used: usize,
}

impl<'a> Cursor<'a> {
    fn new(op: &'a Op) -> Self {
        Self {
            comps: &op.comps,
            i: 0,
            used: 0,
        }
    }
    fn peek(&self) -> Option<&'a Comp> {
        self.comps.get(self.i)
    }
    /// Units left in the current retain or delete.
    fn rest(&self) -> usize {
        match self.comps.get(self.i) {
            Some(Comp::Retain(n) | Comp::Delete(n)) => n - self.used,
            _ => 0,
        }
    }
    fn skip_whole(&mut self) {
        self.i += 1;
        self.used = 0;
    }
    fn consume(&mut self, n: usize) {
        self.used += n;
        if let Some(Comp::Retain(m) | Comp::Delete(m)) = self.comps.get(self.i) {
            if self.used == *m {
                self.skip_whole();
            }
        }
    }
}

/// Transforms two changes made on the same text. Returns `(a', b')` where `a'` applies after `b` and `b'` applies
/// after `a`, and `apply(apply(t, a), b') == apply(apply(t, b), a')`. Where both insert at one position, `a`'s
/// text goes first.
pub fn transform(a: &Op, b: &Op) -> Result<(Op, Op), BadChange> {
    if a.base_len != b.base_len {
        return Err(BadChange(format!(
            "cannot transform changes of different texts ({} and {} units)",
            a.base_len, b.base_len
        )));
    }
    let (mut a2, mut b2) = (Builder::default(), Builder::default());
    let (mut ca, mut cb) = (Cursor::new(a), Cursor::new(b));
    loop {
        match (ca.peek(), cb.peek()) {
            (None, None) => break,
            (Some(Comp::Insert(s)), _) => {
                a2.insert(s.clone());
                b2.retain(s.len());
                ca.skip_whole();
            }
            (_, Some(Comp::Insert(s))) => {
                b2.insert(s.clone());
                a2.retain(s.len());
                cb.skip_whole();
            }
            (Some(x), Some(y)) => {
                let n = ca.rest().min(cb.rest());
                match (x, y) {
                    (Comp::Retain(_), Comp::Retain(_)) => {
                        a2.retain(n);
                        b2.retain(n);
                    }
                    (Comp::Delete(_), Comp::Delete(_)) => {}
                    (Comp::Delete(_), Comp::Retain(_)) => a2.delete(n),
                    (Comp::Retain(_), Comp::Delete(_)) => b2.delete(n),
                    _ => unreachable!("inserts are handled above"),
                }
                ca.consume(n);
                cb.consume(n);
            }
            _ => {
                return Err(BadChange("changes of different lengths".into()));
            }
        }
    }
    let after_b = a2.finish(b.target_len());
    let after_a = b2.finish(a.target_len());
    // `finish` takes the base length: the text each result applies to.
    Ok((
        Op {
            base_len: b.target_len(),
            ..after_b
        },
        Op {
            base_len: a.target_len(),
            ..after_a
        },
    ))
}

pub fn to_utf16(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

pub fn from_utf16(t: &[u16]) -> String {
    String::from_utf16_lossy(t)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn op(changes: &[(u32, u32, &str)], len: usize) -> Op {
        let c: Vec<Change> = changes
            .iter()
            .map(|(f, t, s)| (*f, *t, s.to_string()))
            .collect();
        Op::from_changes(&c, len).unwrap()
    }

    /// Both orders of applying a pair of concurrent changes end in the same text.
    fn converges(base: &str, a: &Op, b: &Op) -> String {
        let t = to_utf16(base);
        let (a2, b2) = transform(a, b).unwrap();
        let via_a = b2.apply(&a.apply(&t).unwrap()).unwrap();
        let via_b = a2.apply(&b.apply(&t).unwrap()).unwrap();
        assert_eq!(via_a, via_b);
        from_utf16(&via_a)
    }

    #[test]
    fn inserts_at_one_position_keep_the_first_argument_first() {
        let a = op(&[(2, 2, "A")], 4);
        let b = op(&[(2, 2, "B")], 4);
        assert_eq!(converges("abcd", &a, &b), "abABcd");
    }

    #[test]
    fn an_insert_inside_a_concurrent_delete_survives() {
        let del = op(&[(1, 5, "")], 6);
        let ins = op(&[(3, 3, "X")], 6);
        assert_eq!(converges("abcdef", &del, &ins), "aXf");
        assert_eq!(converges("abcdef", &ins, &del), "aXf");
    }

    #[test]
    fn overlapping_deletes_delete_once() {
        let a = op(&[(1, 4, "")], 6);
        let b = op(&[(3, 6, "")], 6);
        assert_eq!(converges("abcdef", &a, &b), "a");
    }

    #[test]
    fn wire_form_round_trips() {
        let c: Vec<Change> = vec![(0, 1, "xy".into()), (3, 3, "!".into()), (5, 6, "".into())];
        let o = Op::from_changes(&c, 6).unwrap();
        assert_eq!(o.to_changes(), c);
        assert_eq!(o.target_len(), 6 - 1 + 2 + 1 - 1);
    }

    #[test]
    fn bad_changes_are_refused() {
        assert!(Op::from_changes(&[(3, 2, "".into())], 5).is_err());
        assert!(Op::from_changes(&[(0, 9, "".into())], 5).is_err());
        assert!(Op::from_changes(&[(2, 3, "".into()), (1, 2, "".into())], 5).is_err());
    }

    #[test]
    fn utf16_positions_count_code_units() {
        // "😀" is two units in UTF-16.
        let t = to_utf16("😀b");
        let o = Op::from_changes(&[(2, 2, "X".into())], t.len()).unwrap();
        assert_eq!(from_utf16(&o.apply(&t).unwrap()), "😀Xb");
    }
}
