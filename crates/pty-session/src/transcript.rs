//! A plain-text transcript of a terminal byte stream.
//!
//! Terminal output is full of control sequences (colours, cursor moves, spinners that redraw one
//! line with `\r`). This keeps only what a person would read: the finished lines, plus the
//! unfinished last line (a prompt such as `Paste code > `). It is bounded in lines, bytes per
//! line and total bytes, so a program that prints forever cannot grow it.

use std::collections::VecDeque;

/// Most finished lines kept; older ones are dropped.
pub const MAX_LINES: usize = 4_000;
/// Most characters kept per line; the rest of a longer line is ignored.
pub const MAX_LINE_CHARS: usize = 2_000;
/// Most bytes of text kept in all.
pub const MAX_KEPT_BYTES: usize = 512 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Ground,
    /// After `ESC`.
    Escape,
    /// `ESC [` ... until a final byte; holds the parameter bytes.
    Csi,
    /// `ESC ]`, `ESC P`, `ESC ^`, `ESC _`: a string that ends at BEL or `ESC \`.
    Str,
    /// Inside a string, after an `ESC` (waiting for the `\`).
    StrEsc,
    /// `ESC (` and friends: one more byte follows.
    Charset,
}

/// Finished lines and the unfinished last line of a terminal stream.
#[derive(Debug)]
pub struct Transcript {
    lines: VecDeque<String>,
    /// Absolute index (since the start) of `lines[0]`.
    base: u64,
    kept_bytes: usize,
    cur: Vec<char>,
    col: usize,
    mode: Mode,
    csi: String,
    /// Bytes of a UTF-8 character that was cut by the end of a chunk.
    partial: Vec<u8>,
}

impl Default for Transcript {
    fn default() -> Self {
        Self::new()
    }
}

/// What [`Transcript::take_lines`] hands out.
#[derive(Debug, PartialEq, Eq)]
pub struct Taken {
    pub lines: Vec<String>,
    /// Absolute index of the first line NOT returned (pass it back next time).
    pub next: u64,
    /// Lines between the caller's position and the first line returned were dropped (too old).
    pub dropped: u64,
    /// More finished lines are waiting (the byte limit of this call was reached).
    pub more: bool,
}

impl Transcript {
    #[must_use]
    pub fn new() -> Self {
        Self {
            lines: VecDeque::new(),
            base: 0,
            kept_bytes: 0,
            cur: Vec::new(),
            col: 0,
            mode: Mode::Ground,
            csi: String::new(),
            partial: Vec::new(),
        }
    }

    /// Absolute index one past the last finished line.
    #[must_use]
    pub fn end(&self) -> u64 {
        self.base + self.lines.len() as u64
    }

    /// The unfinished last line (a prompt), without trailing blanks.
    #[must_use]
    pub fn prompt(&self) -> String {
        self.cur.iter().collect::<String>().trim_end().to_owned()
    }

    /// Feed the next chunk of output.
    pub fn feed(&mut self, bytes: &[u8]) {
        let mut data = std::mem::take(&mut self.partial);
        data.extend_from_slice(bytes);
        let mut rest: &[u8] = &data;
        while !rest.is_empty() {
            match std::str::from_utf8(rest) {
                Ok(text) => {
                    self.feed_str(text);
                    break;
                }
                Err(e) => {
                    let (good, bad) = rest.split_at(e.valid_up_to());
                    self.feed_str(&String::from_utf8_lossy(good));
                    match e.error_len() {
                        // A character cut by the end of the chunk: keep it for the next one.
                        None => {
                            self.partial = bad.to_vec();
                            break;
                        }
                        Some(n) => {
                            self.put('\u{fffd}');
                            rest = &bad[n..];
                        }
                    }
                }
            }
        }
    }

    fn feed_str(&mut self, text: &str) {
        for c in text.chars() {
            self.step(c);
        }
    }

    fn step(&mut self, c: char) {
        match self.mode {
            Mode::Ground => match c {
                '\x1b' => self.mode = Mode::Escape,
                '\n' => self.commit(),
                '\r' => self.col = 0,
                '\x08' => self.col = self.col.saturating_sub(1),
                '\t' => {
                    // `put` ignores everything at the line limit, so the loop must stop there:
                    // at column `MAX_LINE_CHARS` the column never moves again.
                    let to = ((self.col / 8 + 1) * 8).min(MAX_LINE_CHARS);
                    while self.col < to {
                        self.put(' ');
                    }
                }
                c if c.is_control() => {}
                c => self.put(c),
            },
            Mode::Escape => {
                self.mode = match c {
                    '[' => {
                        self.csi.clear();
                        Mode::Csi
                    }
                    ']' | 'P' | '^' | '_' | 'X' => Mode::Str,
                    '(' | ')' | '*' | '+' | '#' | '%' => Mode::Charset,
                    _ => Mode::Ground,
                };
            }
            Mode::Csi => {
                if ('\u{40}'..='\u{7e}').contains(&c) {
                    let params = std::mem::take(&mut self.csi);
                    self.csi_final(c, &params);
                    self.mode = Mode::Ground;
                } else if self.csi.len() < 32 {
                    self.csi.push(c);
                }
            }
            Mode::Str => match c {
                '\x07' => self.mode = Mode::Ground,
                '\x1b' => self.mode = Mode::StrEsc,
                _ => {}
            },
            Mode::StrEsc => {
                self.mode = if c == '\\' { Mode::Ground } else { Mode::Str };
            }
            Mode::Charset => self.mode = Mode::Ground,
        }
    }

    fn csi_final(&mut self, last: char, params: &str) {
        let first: usize = params
            .trim_start_matches('?')
            .split(';')
            .next()
            .and_then(|p| p.parse().ok())
            .unwrap_or(0);
        match last {
            // Erase in line: to the end, or the whole line (a spinner is about to redraw it).
            'K' => match first {
                0 => self.cur.truncate(self.col),
                2 => {
                    self.cur.clear();
                    self.col = 0;
                }
                _ => {}
            },
            'C' => self.col = self.col.saturating_add(first.max(1)).min(MAX_LINE_CHARS),
            'D' => self.col = self.col.saturating_sub(first.max(1)),
            'G' => self.col = first.saturating_sub(1).min(MAX_LINE_CHARS),
            _ => {}
        }
    }

    fn put(&mut self, c: char) {
        if self.col >= MAX_LINE_CHARS {
            return;
        }
        while self.cur.len() < self.col {
            self.cur.push(' ');
        }
        if self.col < self.cur.len() {
            self.cur[self.col] = c;
        } else {
            self.cur.push(c);
        }
        self.col += 1;
    }

    fn commit(&mut self) {
        let line: String = self.cur.iter().collect::<String>().trim_end().to_owned();
        self.cur.clear();
        self.col = 0;
        self.kept_bytes += line.len() + 1;
        self.lines.push_back(line);
        while self.lines.len() > MAX_LINES || self.kept_bytes > MAX_KEPT_BYTES {
            if let Some(old) = self.lines.pop_front() {
                self.kept_bytes -= old.len() + 1;
                self.base += 1;
            } else {
                break;
            }
        }
    }

    /// The finished lines from absolute index `from` on, at most `max_bytes` of text.
    #[must_use]
    pub fn take_lines(&self, from: u64, max_bytes: usize) -> Taken {
        let dropped = self.base.saturating_sub(from);
        let start = from.max(self.base);
        let skip = usize::try_from(start - self.base).unwrap_or(usize::MAX);
        let mut lines = Vec::new();
        let mut used = 0usize;
        let mut next = start;
        let mut more = false;
        for line in self.lines.iter().skip(skip) {
            // Always return at least one line, so a long line cannot stall the reader.
            if !lines.is_empty() && used + line.len() + 1 > max_bytes {
                more = true;
                break;
            }
            used += line.len() + 1;
            lines.push(line.clone());
            next += 1;
        }
        Taken {
            lines,
            next,
            dropped,
            more,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all(t: &Transcript) -> Vec<String> {
        t.take_lines(0, usize::MAX).lines
    }

    #[test]
    fn colours_and_cursor_moves_are_dropped() {
        let mut t = Transcript::new();
        t.feed(b"\x1b[1;32mhello\x1b[0m world\r\n\x1b[?25l\x1b]0;title\x07next\x1b(B line\n");
        assert_eq!(all(&t), ["hello world", "next line"]);
        assert_eq!(t.prompt(), "");
    }

    #[test]
    fn a_carriage_return_redraws_the_line_like_a_spinner() {
        let mut t = Transcript::new();
        t.feed(b"| working\r/ working\r\x1b[2K- done\n");
        assert_eq!(all(&t), ["- done"]);
        let mut t = Transcript::new();
        t.feed(b"progress 10%\rprogress 100%\n");
        assert_eq!(all(&t), ["progress 100%"]);
    }

    #[test]
    fn the_unfinished_line_is_the_prompt() {
        let mut t = Transcript::new();
        t.feed(b"Open the link\r\nPaste code here > ");
        assert_eq!(all(&t), ["Open the link"]);
        assert_eq!(t.prompt(), "Paste code here >");
        t.feed(b"abc\r\n");
        assert_eq!(all(&t), ["Open the link", "Paste code here > abc"]);
        assert_eq!(t.prompt(), "");
    }

    #[test]
    fn an_escape_sequence_or_character_split_across_chunks_is_still_understood() {
        let mut t = Transcript::new();
        t.feed(b"a\x1b[3");
        t.feed(b"1mb\x1b");
        t.feed(b"]0;ti");
        t.feed(b"tle\x1b\\c\n");
        assert_eq!(all(&t), ["abc"]);
        let bytes = "çay\n".as_bytes();
        let mut t = Transcript::new();
        t.feed(&bytes[..1]);
        t.feed(&bytes[1..]);
        assert_eq!(all(&t), ["çay"]);
    }

    #[test]
    fn backspace_and_cursor_columns_overwrite() {
        let mut t = Transcript::new();
        t.feed(b"abc\x08\x08X\n");
        t.feed(b"ab\x1b[3Dxy\n");
        assert_eq!(all(&t), ["aXc", "xy"]);
    }

    #[test]
    fn it_is_bounded_in_lines_and_line_length() {
        let mut t = Transcript::new();
        for i in 0..(MAX_LINES + 50) {
            t.feed(format!("line {i}\n").as_bytes());
        }
        let first = t.take_lines(0, usize::MAX);
        assert_eq!(first.lines.len(), MAX_LINES);
        assert_eq!(first.dropped, 50);
        assert_eq!(first.lines[0], "line 50");
        let mut t = Transcript::new();
        t.feed("x".repeat(MAX_LINE_CHARS * 3).as_bytes());
        assert_eq!(t.prompt().len(), MAX_LINE_CHARS);
        let mut big = Transcript::new();
        for _ in 0..2000 {
            big.feed(format!("{}\n", "y".repeat(1000)).as_bytes());
        }
        assert!(big.kept_bytes <= MAX_KEPT_BYTES);
    }

    /// Run `feed` on its own thread: a regression must fail the test, not hang the run.
    fn feeds_within(bytes: Vec<u8>) -> Transcript {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut t = Transcript::new();
            t.feed(&bytes);
            let _ = tx.send(t);
        });
        rx.recv_timeout(std::time::Duration::from_secs(5))
            .expect("the transcript hung")
    }

    #[test]
    fn a_tab_at_the_line_limit_does_not_hang() {
        // Exactly at the limit, just before it, and a run of tabs past it.
        for pad in [MAX_LINE_CHARS, MAX_LINE_CHARS - 3, MAX_LINE_CHARS + 50] {
            let mut bytes = "x".repeat(pad).into_bytes();
            bytes.extend_from_slice(b"\t\t\tend\n");
            let t = feeds_within(bytes);
            let lines = all(&t);
            assert_eq!(lines.len(), 1, "{pad}");
            assert!(lines[0].chars().count() <= MAX_LINE_CHARS, "{pad}");
        }
        // A cursor move to the limit followed by a tab.
        let t = feeds_within(b"a\x1b[5000C\tb\n".to_vec());
        assert_eq!(all(&t).len(), 1);
    }

    #[test]
    fn a_huge_cursor_move_does_not_overflow() {
        let t = feeds_within(format!("a\x1b[{}Cb\x1b[{}Gc\n", usize::MAX, usize::MAX).into_bytes());
        assert_eq!(all(&t).len(), 1);
    }

    #[test]
    fn take_lines_resumes_and_honours_a_byte_limit() {
        let mut t = Transcript::new();
        t.feed(b"one\ntwo\nthree\n");
        let a = t.take_lines(0, 8);
        assert_eq!(a.lines, ["one", "two"]);
        assert!(a.more);
        let b = t.take_lines(a.next, 8);
        assert_eq!(b.lines, ["three"]);
        assert!(!b.more);
        assert_eq!(b.next, t.end());
        // One line always comes out, however small the limit.
        assert_eq!(t.take_lines(0, 0).lines, ["one"]);
    }
}
