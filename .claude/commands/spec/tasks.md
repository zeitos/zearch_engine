---
allowed-tools: Bash(cat:*), Bash(test:*), Bash(bash:*), Write
description: Create implementation task list
---

## Context

Current spec: !`bash .claude/commands/spec/_spec-helper.sh current`
Design approved: !`bash .claude/commands/spec/_spec-helper.sh check-approved design`

## Your Task

1. Verify design is approved
2. Create tasks.md with:
   - Overview with time estimates
   - Phase breakdown (Foundation, Core, Testing, Deployment)
   - Detailed task list with checkboxes
   - Task dependencies
   - Risk mitigation tasks
3. Each task should be specific and actionable
4. Use markdown checkboxes: `- [ ] Task description`

Organize tasks to enable incremental development and testing.
