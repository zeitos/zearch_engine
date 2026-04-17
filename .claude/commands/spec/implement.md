---
allowed-tools: Bash(cat:*), Bash(test:*), Bash(grep:*), Bash(bash:*), Write
description: Start implementation from approved tasks
argument-hint: [phase-number]
---

## Context

Current spec: !`bash .claude/commands/spec/_spec-helper.sh current`
Tasks approved: !`bash .claude/commands/spec/_spec-helper.sh check-approved tasks`

## Current Tasks

!`bash .claude/commands/spec/_spec-helper.sh incomplete-tasks`

## Your Task

1. Verify all phases are approved
2. If phase number provided ($ARGUMENTS), focus on that phase
3. Display current incomplete tasks
4. Create an implementation session log
5. Guide user to:
   - Work on tasks sequentially
   - Update task checkboxes as completed
   - Commit changes regularly
6. Remind about using Write tool to update tasks.md

Start implementing based on the task list!
