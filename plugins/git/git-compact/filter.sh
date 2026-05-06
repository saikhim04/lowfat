#!/bin/sh
# git-compact — compact git output for LLM contexts
# env: $LOWFAT_LEVEL (lite|full|ultra), $LOWFAT_SUBCOMMAND

RAW=$(cat)
LEVEL="${LOWFAT_LEVEL:-full}"
SUB="$LOWFAT_SUBCOMMAND"

case "$SUB" in
  status)
    result=$(echo "$RAW" | grep -E '^\s*[MADRCU?!] ' | head -n 30)
    if [ -z "$result" ]; then
      echo "git status: clean"
    else
      echo "$result"
    fi
    ;;

  diff)
    case "$LEVEL" in
      ultra)
        # Headers + hunk markers only
        echo "$RAW" | grep -E '^(diff --git|@@ )' | head -n 30
        ;;
      *)
        # Diff lines: headers, hunks, additions, deletions
        echo "$RAW" | grep -E '^(diff |--- |\+\+\+ |@@ |[+-])' | head -n 200
        ;;
    esac
    ;;

  log)
    case "$LEVEL" in
      ultra)
        # Commit hash + message only
        echo "$RAW" | grep -E '^(commit |    )' | head -n 10
        ;;
      *)
        echo "$RAW" | head -n 25
        ;;
    esac
    ;;

  show)
    case "$LEVEL" in
      ultra)
        # Commit metadata + diffstat only
        echo "$RAW" | grep -E '^(commit |Author:|Date:|    |diff --git)' | head -n 20
        ;;
      *)
        # Commit header lines, then diff-content lines once we've crossed the
        # first `diff` marker. The 4-space rule for commit messages is gated by
        # `in_diff`, otherwise it bleeds deeply-indented context (Rust, JSON…)
        # back into the output and tanks savings on small commits.
        echo "$RAW" | awk '
          /^diff / { in_diff=1; print; next }
          !in_diff && /^(commit |Merge:|Author:|Date:|    )/ { print; next }
          in_diff && /^(--- |\+\+\+ |@@ |[+-])/ { print }
        ' | head -n 100
        ;;
    esac
    ;;

  *)
    echo "$RAW" | head -n 30
    ;;
esac
