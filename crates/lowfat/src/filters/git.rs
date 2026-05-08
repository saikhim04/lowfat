//! Native git filter — compact status, diff, log, show output.

use anyhow::Result;
use lowfat_core::level::Level;
use lowfat_plugin::plugin::{FilterInput, FilterOutput, FilterPlugin, PluginInfo};

pub struct GitFilter;

impl FilterPlugin for GitFilter {
    fn info(&self) -> PluginInfo {
        PluginInfo {
            name: "git-compact".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            commands: vec!["git".into()],
            // The first four have dedicated filter arms below; the rest are
            // listed so `lowfat history` breaks them out by subcommand instead
            // of collapsing them under bare `git`. They fall through to the
            // generic head_nonblank handler, which is fine for typically-short
            // output like `git add` or `git commit`.
            subcommands: vec![
                "status".into(),
                "log".into(),
                "diff".into(),
                "show".into(),
                "add".into(),
                "commit".into(),
                "checkout".into(),
                "switch".into(),
                "restore".into(),
                "branch".into(),
                "merge".into(),
                "rebase".into(),
                "reset".into(),
                "revert".into(),
                "cherry-pick".into(),
                "stash".into(),
                "tag".into(),
                "fetch".into(),
                "pull".into(),
                "push".into(),
                "clone".into(),
                "remote".into(),
                "init".into(),
                "config".into(),
                "blame".into(),
                "reflog".into(),
                "describe".into(),
                "rm".into(),
                "mv".into(),
                "clean".into(),
                "bisect".into(),
                "grep".into(),
            ],
        }
    }

    fn filter(&self, input: &FilterInput) -> Result<FilterOutput> {
        let text = match input.subcommand.as_str() {
            "status" => filter_status(&input.raw, input.level),
            "log" => filter_log(&input.raw, input.level),
            "diff" => filter_diff(&input.raw, input.level),
            "show" => filter_show(&input.raw, input.level),
            _ => head_nonblank(&input.raw, input.level.head_limit(30)),
        };
        Ok(FilterOutput {
            passthrough: text.is_empty(),
            text,
        })
    }
}

fn filter_status(raw: &str, level: Level) -> String {
    let limit = match level {
        Level::Lite => 60,
        Level::Full => 30,
        Level::Ultra => 15,
    };

    let lines: Vec<&str> = raw
        .lines()
        .filter(|line| match level {
            // Ultra: only short-status file lines (e.g. " M src/main.rs")
            Level::Ultra => {
                let trimmed = line.trim_start();
                trimmed.len() >= 2
                    && trimmed.as_bytes().get(1).copied() == Some(b' ')
                    && is_status_char(trimmed.as_bytes()[0])
            }
            // Lite: status lines + context headers
            Level::Lite => {
                let trimmed = line.trim_start();
                is_status_line(trimmed)
                    || trimmed.starts_with("## ")
                    || trimmed.starts_with("On branch")
                    || trimmed.starts_with("Changes")
                    || trimmed.starts_with("Untracked")
            }
            // Full: status lines + branch header
            Level::Full => {
                let trimmed = line.trim_start();
                is_status_line(trimmed) || trimmed.starts_with("## ")
            }
        })
        .take(limit)
        .collect();

    if lines.is_empty() {
        "git status: clean".into()
    } else {
        lines.join("\n")
    }
}

fn filter_log(raw: &str, level: Level) -> String {
    // All levels apply the same two redundancy drops as `git show`: strip
    // trailers from message bodies and abbreviate the long commit hash.
    match level {
        Level::Ultra => raw
            .lines()
            .filter(|l| (l.starts_with("commit ") || l.starts_with("    ")) && !is_trailer(l))
            .take(10)
            .map(|l| abbreviate_commit_line(l).unwrap_or_else(|| l.to_string()))
            .collect::<Vec<_>>()
            .join("\n"),
        // Lite/Full keep the full log shape; only the line cap differs.
        _ => {
            let limit = if level == Level::Lite { 50 } else { 25 };
            raw.lines()
                .filter(|l| !is_trailer(l))
                .take(limit)
                .map(|l| abbreviate_commit_line(l).unwrap_or_else(|| l.to_string()))
                .collect::<Vec<_>>()
                .join("\n")
        }
    }
}

fn filter_diff(raw: &str, level: Level) -> String {
    let limit = match level {
        Level::Lite => 400,
        Level::Ultra => 30,
        Level::Full => 200,
    };

    let lines = compact_diff_body(raw, level, limit);
    if lines.is_empty() {
        // Nothing matched — output is likely `git diff --stat` / `--name-only` /
        // `--shortstat`, which has no `diff `/`@@ ` markers. Return a compact
        // pass instead of empty (which would trigger raw passthrough and
        // record zero savings).
        return head_nonblank(raw, level.head_limit(50));
    }
    lines.join("\n")
}

fn filter_show(raw: &str, level: Level) -> String {
    match level {
        Level::Lite => {
            // Permissive: drop only index/mode meta and message trailers,
            // abbreviate the commit hash. Keep everything else, including
            // unchanged context lines, so callers asking for high-fidelity
            // output still get it.
            let cleaned: Vec<String> = raw
                .lines()
                .filter(|l| !is_index_meta(l) && !is_trailer(l))
                .take(200)
                .map(|l| abbreviate_commit_line(l).unwrap_or_else(|| l.to_string()))
                .collect();
            cleaned.join("\n")
        }
        Level::Ultra => {
            let cleaned: Vec<String> = raw
                .lines()
                .filter(|l| {
                    !is_trailer(l)
                        && (l.starts_with("commit ")
                            || l.starts_with("Author:")
                            || l.starts_with("Date:")
                            || l.starts_with("    ")
                            || l.starts_with("diff --git")
                            || (l.contains(" | ") && l.chars().any(|c| c == '+' || c == '-')))
                })
                .take(20)
                .map(|l| abbreviate_commit_line(l).unwrap_or_else(|| l.to_string()))
                .collect();
            cleaned.join("\n")
        }
        // Full: commit header + diff-content lines (same treatment as `git diff`).
        // The state machine drops three categories of redundancy:
        //   - pre-hunk metadata (`--- a/X`, `+++ b/X`, `index …`, mode lines) —
        //     `--- ` / `+++ ` always duplicate the path already on `diff --git`;
        //   - commit-message trailers (`Signed-off-by:`, `Co-authored-by:`, …)
        //     which add nothing for code understanding;
        //   - the long form of the commit hash (`commit <40-hex>` → 12-hex).
        // The 4-space rule for commit-message bodies is positional: once we cross
        // the first `diff ` line, leading "    " is deeply indented diff context
        // (Rust source, JSON…) and must drop, not bleed in as message body.
        Level::Full => {
            let mut in_diff = false;
            let mut in_hunk = false;
            let mut output: Vec<String> = Vec::with_capacity(64);
            for line in raw.lines() {
                if output.len() >= 100 {
                    break;
                }
                if line.starts_with("diff ") {
                    in_diff = true;
                    in_hunk = false;
                    output.push(line.to_string());
                    continue;
                }
                if line.starts_with("@@ ") {
                    in_hunk = true;
                    output.push(line.to_string());
                    continue;
                }
                if in_diff && !in_hunk {
                    // Pre-hunk metadata — `--- `, `+++ `, `index `, mode, …
                    continue;
                }
                if in_hunk {
                    if line.starts_with('+') || line.starts_with('-') {
                        output.push(line.to_string());
                    }
                    continue;
                }
                // Pre-diff: commit header / message body. Trailers and the long
                // hash form are the two redundancy sources.
                if is_trailer(line) || !is_commit_header(line) {
                    continue;
                }
                match abbreviate_commit_line(line) {
                    Some(abbrev) => output.push(abbrev),
                    None => output.push(line.to_string()),
                }
            }
            if output.is_empty() {
                // No commit/diff structure recognized — possibly `git show <tag>`
                // or unusual format. Fall back to compact pass.
                return head_nonblank(raw, level.head_limit(60));
            }
            output.join("\n")
        }
    }
}

/// State-aware diff compactor used by `filter_diff` for Lite/Full/Ultra.
///
/// Tracks `in_hunk` to drop pre-hunk metadata (`--- `, `+++ `, `index `, mode)
/// without false-positives against removed source lines that happen to start
/// with `--- ` (e.g. removed comment delimiters). Returns an empty Vec for
/// inputs with no `diff `/`@@ ` markers — caller decides the fallback.
fn compact_diff_body(raw: &str, level: Level, limit: usize) -> Vec<&str> {
    let mut output: Vec<&str> = Vec::with_capacity(64);
    let mut in_hunk = false;

    for line in raw.lines() {
        if output.len() >= limit {
            break;
        }
        if line.starts_with("diff ") {
            in_hunk = false;
            output.push(line);
            continue;
        }
        if line.starts_with("@@ ") {
            in_hunk = true;
            // Ultra: strip the trailing function-context tail to save a few
            // tokens per hunk. Full/Lite keep it — useful for the LLM to know
            // which function the change is in.
            output.push(if level == Level::Ultra {
                trim_hunk_header(line)
            } else {
                line
            });
            continue;
        }
        if level == Level::Ultra {
            // Ultra keeps only `diff `/`@@ ` markers; drop everything else.
            continue;
        }
        if !in_hunk {
            // Pre-hunk: drop `--- `, `+++ `, `index `, mode lines.
            continue;
        }
        if line.starts_with('+') || line.starts_with('-') {
            output.push(line);
        }
    }
    output
}

// --- helpers ---

fn is_status_char(b: u8) -> bool {
    matches!(b, b'M' | b'A' | b'D' | b'R' | b'C' | b'U' | b'?' | b'!')
}

fn is_status_line(s: &str) -> bool {
    s.len() >= 3 && is_status_char(s.as_bytes()[0]) && s.as_bytes()[1] == b' '
        || s.len() >= 4
            && s.as_bytes()[0] == b' '
            && is_status_char(s.as_bytes()[1])
            && s.as_bytes()[2] == b' '
}

fn is_commit_header(l: &str) -> bool {
    l.starts_with("commit ")
        || l.starts_with("Merge:")
        || l.starts_with("Author:")
        || l.starts_with("Date:")
        || l.starts_with("    ")
}

fn is_index_meta(l: &str) -> bool {
    l.starts_with("index ") || l.starts_with("mode ") || l.starts_with("similarity ")
}

/// Commit-message trailers. They sit in the message body (4-space indent) but
/// add no signal for code understanding — bot accounts (`Co-authored-by` from
/// pair programming, `Signed-off-by` from DCO repos) can pile up noticeably.
fn is_trailer(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with("Signed-off-by:")
        || trimmed.starts_with("Co-authored-by:")
        || trimmed.starts_with("Change-Id:")
        || trimmed.starts_with("Reviewed-by:")
        || trimmed.starts_with("Acked-by:")
        || trimmed.starts_with("Tested-by:")
        || trimmed.starts_with("Reported-by:")
        || trimmed.starts_with("Cc:")
}

/// `commit <40-hex>[ decoration]` → `commit <12-hex>[ decoration]`.
/// Returns None if the line isn't a hash header (caller falls back to original).
/// Decoration like `(HEAD -> main)` from `--decorate` is preserved.
fn abbreviate_commit_line(line: &str) -> Option<String> {
    let rest = line.strip_prefix("commit ")?;
    let hash_end = rest
        .find(|c: char| !c.is_ascii_hexdigit())
        .unwrap_or(rest.len());
    if hash_end < 40 {
        return None;
    }
    Some(format!("commit {}{}", &rest[..12], &rest[hash_end..]))
}

/// `@@ -A,B +C,D @@ context` → `@@ -A,B +C,D @@`. Used by Ultra to drop the
/// trailing function-context tail. Returns the input unchanged if the second
/// `@@` marker isn't found.
fn trim_hunk_header(line: &str) -> &str {
    if !line.starts_with("@@ ") {
        return line;
    }
    // Search past the first `@@ ` for the closing ` @@`.
    if let Some(idx) = line[3..].find(" @@") {
        return &line[..3 + idx + 3];
    }
    line
}

fn head_nonblank(raw: &str, limit: usize) -> String {
    raw.lines()
        .filter(|l| !l.is_empty())
        .take(limit)
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_clean() {
        let out = filter_status("", Level::Full);
        assert_eq!(out, "git status: clean");
    }

    #[test]
    fn status_modified() {
        let raw = " M src/main.rs\n M Cargo.toml\n";
        let out = filter_status(raw, Level::Full);
        assert!(out.contains("src/main.rs"));
        assert!(out.contains("Cargo.toml"));
    }

    #[test]
    fn diff_ultra_headers_only() {
        let raw = "diff --git a/f b/f\nindex abc..def\n--- a/f\n+++ b/f\n@@ -1 +1 @@\n-old\n+new\n";
        let out = filter_diff(raw, Level::Ultra);
        assert!(out.contains("diff --git"));
        assert!(out.contains("@@ "));
        assert!(!out.contains("-old"));
    }

    #[test]
    fn show_full_drops_context_and_meta() {
        let raw = "\
commit abc123
Author: zdk
Date:   Mon

    fix bug

diff --git a/f b/f
index abc..def 100644
--- a/f
+++ b/f
@@ -1,3 +1,3 @@
 unchanged context
-old line
+new line
 more context
";
        let out = filter_show(raw, Level::Full);
        assert!(out.contains("commit abc123"));
        assert!(out.contains("    fix bug"));
        assert!(out.contains("diff --git"));
        assert!(out.contains("-old line"));
        assert!(out.contains("+new line"));
        assert!(!out.contains("unchanged context"), "should drop context: {out}");
        assert!(!out.contains("index abc"), "should drop index meta: {out}");
    }

    #[test]
    fn show_full_drops_indented_context_after_diff() {
        // Regression: `    ` rule for commit-message body must not match
        // deeply indented diff context (Rust source is 4-space indented).
        let raw = "\
commit abc123
Author: zdk
Date:   Mon

    refactor

diff --git a/f b/f
@@ -1,3 +1,3 @@
     let x = 1;
-    let y = 2;
+    let y = 3;
     println!(\"{x} {y}\");
";
        let out = filter_show(raw, Level::Full);
        assert!(out.contains("    refactor"), "keep message body: {out}");
        assert!(out.contains("-    let y = 2;"));
        assert!(out.contains("+    let y = 3;"));
        assert!(
            !out.contains("    let x = 1;"),
            "must drop indented context: {out}"
        );
        assert!(
            !out.contains("println!"),
            "must drop indented context: {out}"
        );
    }

    #[test]
    fn diff_full_drops_redundant_minus_plus_and_index() {
        // `--- a/X` and `+++ b/X` always duplicate the path on `diff --git`
        // and must be dropped to save tokens.
        let raw = "\
diff --git a/f b/f
index abc..def 100644
--- a/f
+++ b/f
@@ -1 +1 @@
-old
+new
";
        let out = filter_diff(raw, Level::Full);
        assert!(out.contains("diff --git"));
        assert!(out.contains("@@ "));
        assert!(out.contains("-old"));
        assert!(out.contains("+new"));
        assert!(!out.contains("--- a/f"), "drop redundant ---: {out}");
        assert!(!out.contains("+++ b/f"), "drop redundant +++: {out}");
        assert!(!out.contains("index abc"), "drop index meta: {out}");
    }

    #[test]
    fn diff_full_keeps_removed_dashes_in_hunk_body() {
        // Removed source lines starting with `--- ` (e.g. comment delimiters)
        // must NOT be confused with the pre-hunk metadata header.
        let raw = "\
diff --git a/f b/f
@@ -1,2 +1,2 @@
---- old comment ----
+++ new comment +++
";
        let out = filter_diff(raw, Level::Full);
        assert!(out.contains("---- old comment ----"), "keep removed content: {out}");
        assert!(out.contains("+++ new comment +++"), "keep added content: {out}");
    }

    #[test]
    fn diff_stat_falls_back_instead_of_passthrough() {
        // `git diff --stat` has no `diff `/`@@ ` markers — the old filter
        // returned empty and triggered raw passthrough (zero savings). Now we
        // run a compact pass so blank lines are at least dropped.
        let raw = "\
 crates/foo.rs | 5 +-
 bar/baz.rs    | 12 +++++++++++

 2 files changed, 15 insertions(+), 2 deletions(-)
";
        let out = filter_diff(raw, Level::Full);
        assert!(!out.is_empty(), "fall back, don't return empty: {out}");
        assert!(out.contains("crates/foo.rs"));
        assert!(out.contains("2 files changed"));
        // Blank line between stat and summary should be collapsed.
        assert!(!out.contains("\n\n"), "collapse blank lines: {out:?}");
    }

    #[test]
    fn diff_ultra_strips_hunk_function_context() {
        let raw = "\
diff --git a/f b/f
@@ -23,13 +23,18 @@ pub struct Foo {
-old
+new
";
        let out = filter_diff(raw, Level::Ultra);
        assert!(out.contains("@@ -23,13 +23,18 @@"));
        assert!(!out.contains("pub struct Foo"), "drop tail context: {out}");
    }

    #[test]
    fn show_full_drops_message_trailers() {
        let raw = "\
commit abc123
Author: zdk <z@d.k>
Date:   Fri May 8 13:01:39 2026 +0700

    fix bug

    Signed-off-by: zdk <z@d.k>
    Co-authored-by: someone <s@o.m>
    Change-Id: I0123456789abcdef

diff --git a/f b/f
@@ -1 +1 @@
-old
+new
";
        let out = filter_show(raw, Level::Full);
        assert!(out.contains("    fix bug"));
        assert!(!out.contains("Signed-off-by"), "drop trailer: {out}");
        assert!(!out.contains("Co-authored-by"), "drop trailer: {out}");
        assert!(!out.contains("Change-Id"), "drop trailer: {out}");
    }

    #[test]
    fn show_full_abbreviates_full_commit_hash() {
        let raw = "\
commit fd9858806e241a70eec9d23017ccf00d90b64c4c
Author: zdk
Date:   Mon

    fix
";
        let out = filter_show(raw, Level::Full);
        assert!(out.contains("commit fd9858806e24"), "abbreviate: {out}");
        assert!(!out.contains("fd9858806e241a70eec9d23017ccf00d90b64c4c"), "drop full hash: {out}");
    }

    #[test]
    fn show_full_preserves_decoration_after_abbreviated_hash() {
        let raw = "\
commit fd9858806e241a70eec9d23017ccf00d90b64c4c (HEAD -> main, origin/main)
Author: zdk
Date:   Mon

    fix
";
        let out = filter_show(raw, Level::Full);
        assert!(
            out.contains("commit fd9858806e24 (HEAD -> main, origin/main)"),
            "preserve --decorate suffix: {out}"
        );
    }

    #[test]
    fn show_full_drops_redundant_minus_plus_in_diff() {
        // Same redundancy fix as `git diff` — but verified through `git show`.
        let raw = "\
commit abc123
Author: zdk
Date:   Mon

    fix

diff --git a/f b/f
index 1..2 100644
--- a/f
+++ b/f
@@ -1 +1 @@
-old
+new
";
        let out = filter_show(raw, Level::Full);
        assert!(!out.contains("--- a/f"), "drop redundant ---: {out}");
        assert!(!out.contains("+++ b/f"), "drop redundant +++: {out}");
        assert!(!out.contains("index 1..2"), "drop index meta: {out}");
    }

    #[test]
    fn log_full_drops_trailers_and_abbreviates_hash() {
        let raw = "\
commit fd9858806e241a70eec9d23017ccf00d90b64c4c
Author: zdk
Date:   Mon

    fix bug

    Signed-off-by: zdk <z@d.k>
    Co-authored-by: someone <s@o.m>

commit abc123
Author: zdk
Date:   Sun

    other
";
        let out = filter_log(raw, Level::Full);
        assert!(out.contains("commit fd9858806e24"), "abbreviate hash: {out}");
        assert!(
            !out.contains("fd9858806e241a70eec9d23017ccf00d90b64c4c"),
            "drop full hash: {out}"
        );
        assert!(!out.contains("Signed-off-by"), "drop trailer: {out}");
        assert!(!out.contains("Co-authored-by"), "drop trailer: {out}");
        assert!(out.contains("    fix bug"), "keep message body: {out}");
        // Short hash stays as-is — the abbreviator only kicks in at ≥40 hex.
        assert!(out.contains("commit abc123"));
    }

    #[test]
    fn log_ultra_compact() {
        let raw = "commit abc123\nAuthor: zdk\nDate: Mon\n\n    fix bug\n\ncommit def456\n";
        let out = filter_log(raw, Level::Ultra);
        assert!(out.contains("commit abc123"));
        assert!(out.contains("    fix bug"));
        // Author/Date stripped in ultra
        assert!(!out.contains("Author:"));
    }
}
