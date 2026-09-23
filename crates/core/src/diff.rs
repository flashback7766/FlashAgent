//! Line-based unified diff for the approval preview of write/edit calls.

/// `old = None` means a new file. Very large changed regions degrade to a
/// summary line.
pub fn unified(old: Option<&str>, new: &str, path: &str, context: usize) -> String {
    let old_text = old.unwrap_or("");
    let old_lines: Vec<&str> = old_text.lines().collect();
    let new_lines: Vec<&str> = new.lines().collect();

    // Trim common prefix/suffix so the DP matrix stays small.
    let mut prefix = 0;
    while prefix < old_lines.len() && prefix < new_lines.len() && old_lines[prefix] == new_lines[prefix] {
        prefix += 1;
    }
    let mut suffix = 0;
    while suffix < old_lines.len() - prefix
        && suffix < new_lines.len() - prefix
        && old_lines[old_lines.len() - 1 - suffix] == new_lines[new_lines.len() - 1 - suffix]
    {
        suffix += 1;
    }
    let mid_old = &old_lines[prefix..old_lines.len() - suffix];
    let mid_new = &new_lines[prefix..new_lines.len() - suffix];

    // Cap: beyond this the DP is not worth it.
    const CAP: usize = 1500;
    // git's form for an absolute path is a/etc/hosts, not a//etc/hosts.
    let path = path.trim_start_matches('/');
    let header_old = if old.is_none() { "/dev/null".to_string() } else { format!("a/{path}") };
    let mut out = format!("--- {header_old}\n+++ b/{path}\n");
    if mid_old.len() > CAP || mid_new.len() > CAP {
        out.push_str(&format!(
            "@@ too large for diff: -{} +{} changed lines\n",
            mid_old.len(),
            mid_new.len()
        ));
        return out;
    }

    let mid_ops = diff_ops(mid_old, mid_new);
    if mid_ops.is_empty() {
        return out; // identical
    }
    // The trimmed ends are put back as unchanged lines: they are the context
    // that shows which of several identical lines is being changed.
    let ops: Vec<Op> = old_lines[..prefix]
        .iter()
        .map(|l| Op::Same(l))
        .chain(mid_ops)
        .chain(old_lines[old_lines.len() - suffix..].iter().map(|l| Op::Same(l)))
        .collect();

    // Hunks are runs of changes plus up to `context` unchanged lines around them.
    let mut hunks: Vec<(usize, usize)> = Vec::new(); // [start, end) over ops
    let mut start = 0usize;
    let mut end = 0usize;
    let mut since_change = usize::MAX;
    for (i, op) in ops.iter().enumerate() {
        let is_change = !matches!(op, Op::Same(_));
        if is_change {
            if since_change == usize::MAX || since_change > 2 * context {
                if end > start {
                    hunks.push((start, end));
                }
                start = i.saturating_sub(context);
            }
            since_change = 0;
            end = i + 1;
        } else if since_change != usize::MAX {
            since_change += 1;
            if since_change <= context {
                end = i + 1;
            }
        }
    }
    if end > start {
        hunks.push((start, end));
    }

    for (hstart, hend) in hunks {
        let mut old_pos = 1;
        let mut new_pos = 1;
        for op in &ops[..hstart] {
            match op {
                Op::Same(_) => {
                    old_pos += 1;
                    new_pos += 1;
                }
                Op::Del(_) => old_pos += 1,
                Op::Add(_) => new_pos += 1,
            }
        }
        let (os, ns) = (old_pos, new_pos);
        let mut old_cnt = 0;
        let mut new_cnt = 0;
        let mut body = String::new();
        for op in &ops[hstart..hend] {
            match op {
                Op::Same(l) => {
                    old_cnt += 1;
                    new_cnt += 1;
                    body.push(' ');
                    body.push_str(l);
                }
                Op::Del(l) => {
                    old_cnt += 1;
                    body.push('-');
                    body.push_str(l);
                }
                Op::Add(l) => {
                    new_cnt += 1;
                    body.push('+');
                    body.push_str(l);
                }
            }
            body.push('\n');
        }
        out.push_str(&format!("@@ -{os},{old_cnt} +{ns},{new_cnt} @@\n{body}"));
    }
    out
}

#[derive(Clone, Copy)]
enum Op<'a> {
    Same(&'a str),
    Del(&'a str),
    Add(&'a str),
}

fn diff_ops<'a>(old: &'a [&'a str], new: &'a [&'a str]) -> Vec<Op<'a>> {
    let n = old.len();
    let m = new.len();
    let mut dp = vec![0usize; (n + 1) * (m + 1)];
    let width = m + 1;
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            dp[i * width + j] = if old[i] == new[j] {
                dp[(i + 1) * width + j + 1] + 1
            } else {
                dp[(i + 1) * width + j].max(dp[i * width + j + 1])
            };
        }
    }
    let mut ops = Vec::with_capacity(n + m);
    let (mut i, mut j) = (0, 0);
    while i < n && j < m {
        if old[i] == new[j] {
            ops.push(Op::Same(old[i]));
            i += 1;
            j += 1;
        } else if dp[(i + 1) * width + j] >= dp[i * width + j + 1] {
            ops.push(Op::Del(old[i]));
            i += 1;
        } else {
            ops.push(Op::Add(new[j]));
            j += 1;
        }
    }
    ops.extend(old[i..].iter().map(|l| Op::Del(l)));
    ops.extend(new[j..].iter().map(|l| Op::Add(l)));
    ops
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modification_produces_minus_and_plus() {
        let d = unified(Some("a\nb\nc\n"), "a\nB\nc\n", "f.txt", 2);
        assert!(d.contains("--- a/f.txt"));
        let abs = unified(Some("x\n"), "y\n", "/etc/hosts", 3);
        assert!(abs.contains("--- a/etc/hosts") && abs.contains("+++ b/etc/hosts"), "{abs}");
        assert!(!abs.contains("a//"), "{abs}");
        assert!(d.contains("+++ b/f.txt"));
        assert!(d.contains("-b"));
        assert!(d.contains("+B"));
        assert!(d.contains("@@"));
    }

    #[test]
    fn a_change_shows_the_lines_around_it() {
        let old = "fn a() {\n    x();\n    return None;\n}\nfn b() {\n    y();\n    return None;\n}\n";
        let new = "fn a() {\n    x();\n    return None;\n}\nfn b() {\n    y();\n    return Some(1);\n}\n";
        let d = unified(Some(old), new, "f.rs", 2);
        assert!(d.contains("@@ -5,4 +5,4 @@\n fn b() {\n     y();\n-    return None;\n+    return Some(1);\n }\n"), "{d}");
        // Two changes far apart are two hunks, each with its own context.
        let old: String = (1..=20).map(|i| format!("{i}\n")).collect();
        let new = old.replace("3\n", "three\n").replace("17\n", "seventeen\n");
        let d = unified(Some(&old), &new, "n.txt", 1);
        assert!(d.contains("@@ -2,3 +2,3 @@\n 2\n-3\n+three\n 4\n"), "{d}");
        assert!(d.contains("@@ -16,3 +16,3 @@\n 16\n-17\n+seventeen\n 18\n"), "{d}");
    }

    #[test]
    fn new_file_uses_dev_null() {
        let d = unified(None, "one\ntwo\n", "new.txt", 3);
        assert!(d.contains("--- /dev/null"));
        assert!(d.contains("+one"));
        assert!(d.contains("+two"));
    }

    #[test]
    fn identical_content_has_no_hunks() {
        let d = unified(Some("same\n"), "same\n", "f", 3);
        assert!(!d.contains("@@"));
    }

    #[test]
    fn large_change_degrades_to_summary() {
        let old = "x\n".repeat(2000);
        let new = "y\n".repeat(2000);
        let d = unified(Some(&old), &new, "f", 3);
        assert!(d.contains("too large for diff"));
    }
}
