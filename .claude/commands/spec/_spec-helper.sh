#!/usr/bin/env bash
# Helper script for spec commands to avoid command_substitution nesting
# Usage: .claude/commands/spec/_spec-helper.sh <command> [args...]

set -e

SPEC_DIR="spec"
CURRENT_SPEC_FILE="$SPEC_DIR/.current-spec"

get_current() {
    cat "$CURRENT_SPEC_FILE" 2>/dev/null || echo ""
}

CURRENT=$(get_current)

case "${1:-}" in
    current)
        if [ -n "$CURRENT" ]; then echo "$CURRENT"; else echo "No active spec"; fi
        ;;
    dir)
        if [ -n "$CURRENT" ]; then
            ls -la "$SPEC_DIR/$CURRENT/" 2>/dev/null || echo "Spec directory not found"
        else
            echo "No active spec"
        fi
        ;;
    check-approved)
        PHASE="${2:-}"
        if [ -n "$CURRENT" ] && [ -f "$SPEC_DIR/$CURRENT/.${PHASE}-approved" ]; then
            echo "Yes"
        else
            echo "No"
        fi
        ;;
    tasks)
        if [ -n "$CURRENT" ] && [ -f "$SPEC_DIR/$CURRENT/tasks.md" ]; then
            grep -n "^- \[" "$SPEC_DIR/$CURRENT/tasks.md" | head -20
        else
            echo "No tasks found"
        fi
        ;;
    incomplete-tasks)
        if [ -n "$CURRENT" ] && [ -f "$SPEC_DIR/$CURRENT/tasks.md" ]; then
            echo "=== Phase Overview ==="
            grep "^## Phase" "$SPEC_DIR/$CURRENT/tasks.md" 2>/dev/null || true
            echo ""
            echo "=== Incomplete Tasks ==="
            grep "^- \[ \]" "$SPEC_DIR/$CURRENT/tasks.md" 2>/dev/null | head -20
        else
            echo "No tasks found"
        fi
        ;;
    *)
        echo "Unknown command: ${1:-}"
        exit 1
        ;;
esac
