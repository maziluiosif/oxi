//! Unified patch parsing: files, hunks, line numbers and word-level change ranges.

use super::*;

pub(super) fn parse(text: &str) -> DiffModel {
    let mut model = DiffModel::default();
    let mut lines = text.lines().peekable();
    if text.starts_with("commit ") {
        let mut commit = CommitInfo::default();
        let mut message = Vec::new();
        while let Some(line) = lines.next_if(|l| !l.starts_with("diff --git ")) {
            if let Some(hash) = line.strip_prefix("commit ") {
                commit.hash = hash.trim().to_owned();
            } else if let Some(author) = line.strip_prefix("Author:") {
                commit.author = author.trim().to_owned();
            } else if let Some(date) = line.strip_prefix("Date:") {
                commit.date = date.trim().to_owned();
            } else {
                message.push(line.strip_prefix("    ").unwrap_or(line));
            }
        }
        commit.message = message.join("\n").trim().to_owned();
        model.commit = Some(commit);
    }

    let mut file: Option<DiffFile> = None;
    let (mut old_left, mut new_left) = (0usize, 0usize);
    let (mut old_no, mut new_no) = (0usize, 0usize);
    let mut old_end: Option<usize> = None;
    for line in lines {
        if old_left > 0 || new_left > 0 {
            let (kind, content) = match line.as_bytes().first() {
                Some(b'-') => (Kind::Removed, &line[1..]),
                Some(b'+') => (Kind::Added, &line[1..]),
                Some(b' ') => (Kind::Context, &line[1..]),
                None => (Kind::Context, ""),
                Some(b'\\') => continue,
                _ => {
                    // A truncated or malformed hunk: stop consuming lines as its body.
                    old_left = 0;
                    new_left = 0;
                    (Kind::Context, "")
                }
            };
            if old_left > 0 || new_left > 0 {
                let f = file.get_or_insert_with(DiffFile::default);
                let (o, n) = match kind {
                    Kind::Removed => (Some(old_no), None),
                    Kind::Added => (None, Some(new_no)),
                    Kind::Context => (Some(old_no), Some(new_no)),
                };
                if o.is_some() {
                    old_no += 1;
                    old_left = old_left.saturating_sub(1);
                }
                if n.is_some() {
                    new_no += 1;
                    new_left = new_left.saturating_sub(1);
                }
                f.items.push(Item::Line(Line {
                    kind,
                    old_no: o,
                    new_no: n,
                    text: content.trim_end_matches('\r').to_owned(),
                    words: Vec::new(),
                    offset: [0, 0],
                }));
                old_end = Some(old_no);
                continue;
            }
        }
        if let Some(paths) = line.strip_prefix("diff --git ") {
            model.files.extend(file.take());
            let mut f = DiffFile::default();
            if let Some((a, b)) = paths.split_once(" b/") {
                f.old_path = Some(a.strip_prefix("a/").unwrap_or(a).to_owned());
                f.new_path = Some(b.to_owned());
            }
            file = Some(f);
            old_end = None;
        } else if let Some(path) = line.strip_prefix("--- ") {
            if file.as_ref().is_none_or(|f| !f.items.is_empty()) {
                model.files.extend(file.take());
                old_end = None;
            }
            file.get_or_insert_with(DiffFile::default).old_path = patch_path(path, "a/");
        } else if let Some(path) = line.strip_prefix("+++ ") {
            file.get_or_insert_with(DiffFile::default).new_path = patch_path(path, "b/");
        } else if line.starts_with("@@") {
            let Some((old_start, old_len, new_start, new_len, heading)) = hunk_header(line) else {
                continue;
            };
            let f = file.get_or_insert_with(DiffFile::default);
            let hidden = match old_end {
                Some(end) => old_start.saturating_sub(end),
                None => old_start.saturating_sub(1),
            };
            if hidden > 0 {
                f.items.push(Item::Gap {
                    heading: heading.to_owned(),
                    hidden: Some(hidden),
                });
            }
            (old_no, new_no) = (old_start.max(1), new_start.max(1));
            (old_left, new_left) = (old_len, new_len);
        } else if line.starts_with("Binary files") || line.starts_with("GIT binary patch") {
            file.get_or_insert_with(DiffFile::default).binary = true;
        } else if line.starts_with("new file mode") {
            if let Some(f) = file.as_mut() {
                f.old_path = None;
            }
        } else if line.starts_with("deleted file mode") {
            if let Some(f) = file.as_mut() {
                f.new_path = None;
            }
        } else if let Some(path) = line.strip_prefix("rename from ") {
            file.get_or_insert_with(DiffFile::default).old_path = Some(path.to_owned());
        } else if let Some(path) = line.strip_prefix("rename to ") {
            file.get_or_insert_with(DiffFile::default).new_path = Some(path.to_owned());
        } else if line.starts_with("… [truncated]") {
            file.get_or_insert_with(DiffFile::default).truncated = true;
        }
    }
    model.files.extend(file);
    for file in &mut model.files {
        finish_file(file);
    }
    model
}

pub(super) fn patch_path(path: &str, prefix: &str) -> Option<String> {
    let path = path.trim_end();
    (path != "/dev/null").then(|| path.strip_prefix(prefix).unwrap_or(path).to_owned())
}

/// `@@ -a,b +c,d @@ heading` → (a, b, c, d, heading); omitted lengths default to 1.
pub(super) fn hunk_header(line: &str) -> Option<(usize, usize, usize, usize, &str)> {
    let rest = line.strip_prefix("@@ ")?;
    let (ranges, heading) = rest.split_once(" @@").unwrap_or((rest, ""));
    let mut parts = ranges.split_whitespace();
    let range = |part: &str| -> Option<(usize, usize)> {
        let (start, len) = part.split_once(',').unwrap_or((part, "1"));
        Some((start.parse().ok()?, len.parse().ok()?))
    };
    let (old_start, old_len) = range(parts.next()?.strip_prefix('-')?)?;
    let (new_start, new_len) = range(parts.next()?.strip_prefix('+')?)?;
    Some((old_start, old_len, new_start, new_len, heading.trim()))
}

/// Pair changed lines for word highlights, then build each side's highlight text.
pub(super) fn finish_file(file: &mut DiffFile) {
    let n = file.items.len();
    file.block_of = vec![None; n];
    let mut i = 0;
    while i < n {
        if file.kind(i).is_none_or(|kind| kind == Kind::Context) {
            i += 1;
            continue;
        }
        let start = i;
        while file.kind(i).is_some_and(|kind| kind != Kind::Context) {
            file.block_of[i] = Some(file.blocks.len());
            i += 1;
        }
        file.blocks.push(start..i);
    }
    let mut i = 0;
    while i < n {
        if file.kind(i) != Some(Kind::Removed) {
            i += 1;
            continue;
        }
        let start = i;
        while i < n && file.kind(i) == Some(Kind::Removed) {
            i += 1;
        }
        let mid = i;
        while i < n && file.kind(i) == Some(Kind::Added) {
            i += 1;
        }
        for k in 0..(mid - start).min(i - mid) {
            let (old, new) = (start + k, mid + k);
            let (Some(a), Some(b)) = (file.line(old), file.line(new)) else {
                continue;
            };
            let (old_words, new_words) = word_diff(&a.text, &b.text);
            if let Item::Line(line) = &mut file.items[old] {
                line.words = old_words;
            }
            if let Item::Line(line) = &mut file.items[new] {
                line.words = new_words;
            }
        }
    }

    let mut texts = [String::new(), String::new()];
    for item in &mut file.items {
        let Item::Line(line) = item else { continue };
        file.max_chars = file.max_chars.max(line.text.chars().count());
        match line.kind {
            Kind::Removed => file.removed += 1,
            Kind::Added => file.added += 1,
            Kind::Context => {}
        }
        for (side, text) in texts.iter_mut().enumerate() {
            let present = match line.kind {
                Kind::Context => true,
                Kind::Removed => side == 0,
                Kind::Added => side == 1,
            };
            if present {
                line.offset[side] = text.len();
                text.push_str(&line.text);
                text.push('\n');
            }
        }
    }
    file.side_text = texts;
}

pub(super) fn tokens(text: &str) -> Vec<Range<usize>> {
    fn class(c: char) -> u8 {
        if c.is_alphanumeric() || c == '_' {
            0
        } else if c.is_whitespace() {
            1
        } else {
            2
        }
    }
    let mut out: Vec<Range<usize>> = Vec::new();
    let mut previous = None;
    for (index, c) in text.char_indices() {
        let class = class(c);
        let end = index + c.len_utf8();
        match out.last_mut() {
            Some(last) if previous == Some(class) && class != 2 => last.end = end,
            _ => out.push(index..end),
        }
        previous = Some(class);
    }
    out
}

/// Changed byte ranges of two similar lines (token LCS). Empty when the lines are too different
/// for word highlights to help.
pub(super) fn word_diff(old: &str, new: &str) -> (Vec<Range<usize>>, Vec<Range<usize>>) {
    let (a, b) = (tokens(old), tokens(new));
    let (n, m) = (a.len(), b.len());
    if n == 0 || m == 0 || n * m > 40_000 {
        return (Vec::new(), Vec::new());
    }
    let width = m + 1;
    let mut lcs = vec![0u16; (n + 1) * width];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[i * width + j] = if old[a[i].clone()] == new[b[j].clone()] {
                lcs[(i + 1) * width + j + 1] + 1
            } else {
                lcs[(i + 1) * width + j].max(lcs[i * width + j + 1])
            };
        }
    }
    let (mut keep_a, mut keep_b) = (vec![false; n], vec![false; m]);
    let (mut i, mut j) = (0, 0);
    while i < n && j < m {
        if old[a[i].clone()] == new[b[j].clone()] {
            keep_a[i] = true;
            keep_b[j] = true;
            i += 1;
            j += 1;
        } else if lcs[(i + 1) * width + j] >= lcs[i * width + j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    let old_ranges = changed_ranges(old, &a, &keep_a);
    let new_ranges = changed_ranges(new, &b, &keep_b);
    let changed: usize = old_ranges.iter().chain(&new_ranges).map(|r| r.len()).sum();
    let total = old.trim().len() + new.trim().len();
    if changed as f32 > total as f32 * MAX_WORD_CHANGE {
        return (Vec::new(), Vec::new());
    }
    (old_ranges, new_ranges)
}

/// Merge unkept tokens into ranges, bridging single whitespace gaps between changed words.
pub(super) fn changed_ranges(
    text: &str,
    tokens: &[Range<usize>],
    keep: &[bool],
) -> Vec<Range<usize>> {
    let mut out: Vec<Range<usize>> = Vec::new();
    for (token, kept) in tokens.iter().zip(keep) {
        if *kept {
            continue;
        }
        match out.last_mut() {
            Some(last) if text[last.end..token.start].trim().is_empty() => last.end = token.end,
            _ => out.push(token.clone()),
        }
    }
    out
}
