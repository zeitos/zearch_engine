---
allowed-tools: Bash(cat:*), Bash(test:*), Bash(bash:*), Bash(ls:*), Bash(grep:*)
description: Review current specification phase or implementation
---

## Review

!`current=$(cat spec/.current-spec 2>/dev/null)
if [ -z "$current" ]; then
    echo "No active spec. Use 'spec switch <spec-name>' to select one."
    exit 0
fi

echo "Active spec: $current"
echo "---"

req_approved=false
design_approved=false
tasks_approved=false

if [ -f "spec/$current/.requirements-approved" ]; then req_approved=true; fi
if [ -f "spec/$current/.design-approved" ]; then design_approved=true; fi
if [ -f "spec/$current/.tasks-approved" ]; then tasks_approved=true; fi

if $req_approved && $design_approved && $tasks_approved; then
    echo "## Implementation Review"
    echo ""
    echo "All specification documents are approved. It's time to review the implementation against the spec."
    echo ""
    echo "**Your Task:**"
    echo ""
    echo "1.  **Analyze the Specification:**"
    echo "    - Read and understand the full specification:"
    echo "      - spec/$current/requirements.md"
    echo "      - spec/$current/design.md"
    echo "      - spec/$current/tasks.md"
    echo ""
    echo "2.  **Inspect the Code:**"
    echo "    - Identify and review the code changes that implement this specification."
    echo "    - Use tools to find the relevant commits or diffs if necessary."
    echo ""
    echo "3.  **Verify Compliance:**"
    echo "    - **Requirements:** Create a checklist from requirements.md and verify that each requirement is met by the code."
    echo "    - **Design:** Confirm the implementation adheres to the architecture, patterns, and component choices outlined in design.md."
    echo "    - **Tasks:** Ensure every item in tasks.md is completed and correctly implemented."
    echo ""
    echo "4.  **Deliver Your Review:**"
    echo "    - Summarize your findings."
    echo "    - Highlight any deviations, bugs, or areas for improvement."
    echo "    - If the implementation is solid, provide a confirmation."

else
    echo "## Specification Review"
    echo ""
    echo "The following specification documents are present:"
    ls -1 "spec/$current/" | grep -E "(requirements|design|tasks)\.md"
    echo ""
    echo "Approval status:"
    if $req_approved; then echo "- Yes requirements.md"; else echo "- No requirements.md"; fi
    if $design_approved; then echo "- Yes design.md"; else echo "- No design.md"; fi
    if $tasks_approved; then echo "- Yes tasks.md"; else echo "- No tasks.md"; fi
    echo ""
    echo "**Your Task:**"
    echo ""
    echo "1. Identify the next document to be reviewed (the first one without approval)."
    echo "2. Display the content of that document."
    echo "3. Provide a review checklist for the document:"
    echo "   - Does it meet all criteria for its type?"
    echo "   - Is it complete, clear, and unambiguous?"
    echo "   - Are there any missing elements or potential issues?"
    echo "4. After your review, remind the user how to approve the document if they are satisfied (e.g., spec approve requirements)."
fi`
