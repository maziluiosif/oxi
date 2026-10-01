//! Common prefix / suffix of two texts, compared in blocks so it runs at `memcmp` speed.
//!
//! Every keystroke in the editor diffs the new text against the old one (to edit the syntax tree
//! and to reuse laid-out lines). Byte-by-byte iterator comparisons of a multi-megabyte file were
//! a visible part of each frame.

const BLOCK: usize = 64;

/// Length of the longest common prefix of `a` and `b`, in bytes.
pub(crate) fn common_prefix_len(a: &[u8], b: &[u8]) -> usize {
    let max = a.len().min(b.len());
    let mut len = 0;
    while len + BLOCK <= max && a[len..len + BLOCK] == b[len..len + BLOCK] {
        len += BLOCK;
    }
    len + a[len..max]
        .iter()
        .zip(&b[len..max])
        .take_while(|(x, y)| x == y)
        .count()
}

/// Length of the longest common suffix of `a` and `b`, in bytes, at most `max`.
pub(crate) fn common_suffix_len(a: &[u8], b: &[u8], max: usize) -> usize {
    let max = max.min(a.len()).min(b.len());
    let (a_end, b_end) = (a.len(), b.len());
    let mut len = 0;
    while len + BLOCK <= max
        && a[a_end - len - BLOCK..a_end - len] == b[b_end - len - BLOCK..b_end - len]
    {
        len += BLOCK;
    }
    len + a[a_end - max..a_end - len]
        .iter()
        .rev()
        .zip(b[b_end - max..b_end - len].iter().rev())
        .take_while(|(x, y)| x == y)
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn naive_prefix(a: &[u8], b: &[u8]) -> usize {
        a.iter().zip(b).take_while(|(x, y)| x == y).count()
    }

    fn naive_suffix(a: &[u8], b: &[u8], max: usize) -> usize {
        a.iter()
            .rev()
            .zip(b.iter().rev())
            .take(max)
            .take_while(|(x, y)| x == y)
            .count()
    }

    #[test]
    fn matches_a_byte_by_byte_scan() {
        let base: Vec<u8> = (0..1000u32).map(|i| (i % 251) as u8).collect();
        for at in [0, 1, 63, 64, 65, 500, 999] {
            let mut changed = base.clone();
            changed[at] ^= 0xff;
            let mut inserted = base.clone();
            inserted.insert(at, 7);
            for other in [&changed, &inserted, &base[..at].to_vec()] {
                assert_eq!(common_prefix_len(&base, other), naive_prefix(&base, other));
                for max in [0, 10, 64, 200, usize::MAX] {
                    assert_eq!(
                        common_suffix_len(&base, other, max),
                        naive_suffix(&base, other, max),
                        "at {at} max {max}"
                    );
                }
            }
        }
    }
}
