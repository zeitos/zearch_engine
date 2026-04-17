---
allowed-tools: Bash(cat:*), Bash(grep:*), Bash(bash:*), Write
description: Mark a task as complete
argument-hint: <task-description-or-number>
---

## Current Tasks

!`bash .claude/commands/spec/_spec-helper.sh tasks`

## Your Task

Update the task status for: "$ARGUMENTS"

1. Find the matching task in tasks.md
2. Change `- [ ]` to `- [x]` for that task
3. Show updated progress statistics
4. Suggest next task to work on

Use the Write tool to update the file.
