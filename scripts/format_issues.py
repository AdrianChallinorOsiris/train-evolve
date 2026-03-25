#!/usr/bin/env python3
"""Format GitHub issues JSON into readable markdown for the agent."""

import json
import sys


def count_reactions(reaction_groups):
    """Count total positive reactions (thumbsup, heart, hooray, rocket)."""
    positive = {"THUMBS_UP", "HEART", "HOORAY", "ROCKET"}
    total = 0
    for group in (reaction_groups or []):
        if group.get("content") in positive:
            total += group.get("totalCount", 0)
    return total


def format_issues(issues):
    if not issues:
        return "No community issues today."

    # Sort by reaction count descending
    issues.sort(key=lambda i: count_reactions(i.get("reactionGroups")), reverse=True)

    lines = ["# Community Issues\n"]
    lines.append(f"{len(issues)} open issues with `agent-input` label.\n")

    for issue in issues:
        num = issue.get("number", "?")
        title = issue.get("title", "Untitled")
        body = issue.get("body", "").strip()
        reactions = count_reactions(issue.get("reactionGroups"))
        labels = [l.get("name", "") for l in issue.get("labels", []) if l.get("name") != "agent-input"]

        lines.append(f"### Issue #{num}: {title}")
        if reactions > 0:
            lines.append(f"👍 {reactions} reactions")
        if labels:
            lines.append(f"Labels: {', '.join(labels)}")
        lines.append("")
        # Truncate long issue bodies
        if len(body) > 500:
            body = body[:500] + "\n[... truncated]"
        if body:
            lines.append(body)
        lines.append("")
        lines.append("---")
        lines.append("")

    return "\n".join(lines)


if __name__ == "__main__":
    if len(sys.argv) < 2:
        print("No community issues today.")
        sys.exit(0)

    try:
        with open(sys.argv[1]) as f:
            issues = json.load(f)
        print(format_issues(issues))
    except (json.JSONDecodeError, FileNotFoundError):
        print("No community issues today.")
