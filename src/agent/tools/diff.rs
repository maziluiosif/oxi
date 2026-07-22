//! Unified diff generation for file tool results.

/// Produce a minimal-ish unified diff between `before` and `after` text.
/// Context lines: 3 (standard). Output is capped at 8 000 chars so it stays
/// lightweight in the UI. Small files use an LCS diff; large files use a
/// bounded prefix/suffix diff so a big edit cannot allocate an O(m*n) matrix.
pub(crate) fn make_unified_diff(path: &str, before: &str, after: &str) -> String {
    if before == after {
        return String::new();
    }

    let before_lines: Vec<&str> = before.lines().collect();
    let after_lines: Vec<&str> = after.lines().collect();
    let m = before_lines.len();
    let n = after_lines.len();

    const MAX_LCS_CELLS: usize = 1_000_000;
    let ops = if m.saturating_mul(n) <= MAX_LCS_CELLS {
        lcs_diff_ops(&before_lines, &after_lines)
    } else {
        bounded_diff_ops(&before_lines, &after_lines)
    };

    render_unified_diff(path, &ops)
}

fn lcs_diff_ops<'a>(before_lines: &[&'a str], after_lines: &[&'a str]) -> Vec<(char, &'a str)> {
    let m = before_lines.len();
    let n = after_lines.len();

    let mut dp = vec![vec![0u32; n + 1]; m + 1];
    for i in 1..=m {
        for j in 1..=n {
            dp[i][j] = if before_lines[i - 1] == after_lines[j - 1] {
                dp[i - 1][j - 1] + 1
            } else {
                dp[i - 1][j].max(dp[i][j - 1])
            };
        }
    }

    let mut ops: Vec<(char, &str)> = Vec::new();
    let (mut i, mut j) = (m, n);
    while i > 0 || j > 0 {
        if i > 0 && j > 0 && before_lines[i - 1] == after_lines[j - 1] {
            ops.push((' ', before_lines[i - 1]));
            i -= 1;
            j -= 1;
        } else if j > 0 && (i == 0 || dp[i][j - 1] >= dp[i - 1][j]) {
            ops.push(('+', after_lines[j - 1]));
            j -= 1;
        } else {
            ops.push(('-', before_lines[i - 1]));
            i -= 1;
        }
    }
    ops.reverse();
    ops
}

fn bounded_diff_ops<'a>(before_lines: &[&'a str], after_lines: &[&'a str]) -> Vec<(char, &'a str)> {
    let mut prefix = 0usize;
    while prefix < before_lines.len()
        && prefix < after_lines.len()
        && before_lines[prefix] == after_lines[prefix]
    {
        prefix += 1;
    }

    let mut suffix = 0usize;
    while suffix < before_lines.len().saturating_sub(prefix)
        && suffix < after_lines.len().saturating_sub(prefix)
        && before_lines[before_lines.len() - 1 - suffix]
            == after_lines[after_lines.len() - 1 - suffix]
    {
        suffix += 1;
    }

    let mut ops = Vec::with_capacity(before_lines.len() + after_lines.len());
    ops.extend(before_lines[..prefix].iter().map(|line| (' ', *line)));
    ops.extend(
        before_lines[prefix..before_lines.len() - suffix]
            .iter()
            .map(|line| ('-', *line)),
    );
    ops.extend(
        after_lines[prefix..after_lines.len() - suffix]
            .iter()
            .map(|line| ('+', *line)),
    );
    ops.extend(
        before_lines[before_lines.len() - suffix..]
            .iter()
            .map(|line| (' ', *line)),
    );
    ops
}

fn render_unified_diff(path: &str, ops: &[(char, &str)]) -> String {
    const CTX: usize = 3;
    let total = ops.len();
    let changed: Vec<usize> = ops
        .iter()
        .enumerate()
        .filter(|(_, (kind, _))| *kind != ' ')
        .map(|(index, _)| index)
        .collect();

    if changed.is_empty() {
        return String::new();
    }

    let mut hunks: Vec<(usize, usize)> = Vec::new();
    let mut hunk_start = changed[0].saturating_sub(CTX);
    let mut hunk_end = (changed[0] + CTX + 1).min(total);
    for &changed_index in &changed[1..] {
        if changed_index.saturating_sub(CTX) <= hunk_end {
            hunk_end = (changed_index + CTX + 1).min(total);
        } else {
            hunks.push((hunk_start, hunk_end));
            hunk_start = changed_index.saturating_sub(CTX);
            hunk_end = (changed_index + CTX + 1).min(total);
        }
    }
    hunks.push((hunk_start, hunk_end));

    let mut out = format!("--- a/{path}\n+++ b/{path}\n");
    let mut before_nums = Vec::with_capacity(ops.len());
    let mut after_nums = Vec::with_capacity(ops.len());
    let (mut before_line, mut after_line) = (1usize, 1usize);
    for (kind, _) in ops {
        before_nums.push(before_line);
        after_nums.push(after_line);
        match kind {
            ' ' => {
                before_line += 1;
                after_line += 1;
            }
            '-' => before_line += 1,
            '+' => after_line += 1,
            _ => {}
        }
    }

    for (start, end) in hunks {
        let before_start = before_nums[start];
        let after_start = after_nums[start];
        let before_count = ops[start..end]
            .iter()
            .filter(|(kind, _)| *kind != '+')
            .count();
        let after_count = ops[start..end]
            .iter()
            .filter(|(kind, _)| *kind != '-')
            .count();
        out.push_str(&format!(
            "@@ -{before_start},{before_count} +{after_start},{after_count} @@\n"
        ));
        for (kind, line) in &ops[start..end] {
            out.push(*kind);
            out.push_str(line);
            out.push('\n');
        }
        if out.len() > 8_000 {
            out.push_str("\n[diff truncated]\n");
            break;
        }
    }
    out
}
