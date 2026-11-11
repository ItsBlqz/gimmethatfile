#!/usr/bin/env bash
# Apply (or update) the branch protection ruleset in .github/rulesets/main.json.
#
# Run it again after changing the ruleset file:
#
#     scripts/protect-main.sh
#
# Requires the GitHub CLI (`gh`), logged in as a repo admin. On Windows, run
# it from Git Bash, or from PowerShell with:
#
#     & "$env:ProgramFiles\Git\bin\bash.exe" scripts/protect-main.sh
set -euo pipefail

cd "$(dirname "$0")/.."
repo=$(gh repo view --json nameWithOwner -q .nameWithOwner)
file=.github/rulesets/main.json
# The ruleset's own name (the first "name" key, indented two spaces).
name=$(sed -n 's/^  "name": *"\([^"]*\)".*/\1/p' "$file")

id=$(gh api "repos/$repo/rulesets" -q ".[] | select(.name == \"$name\") | .id" 2>/dev/null || true)
if [[ -n "$id" ]]; then
  gh api --method PUT "repos/$repo/rulesets/$id" --input "$file" >/dev/null
  echo "Updated ruleset '$name' (#$id) on $repo"
else
  id=$(gh api --method POST "repos/$repo/rulesets" --input "$file" -q .id)
  echo "Created ruleset '$name' (#$id) on $repo"
fi
echo "Review it at: https://github.com/$repo/settings/rules/$id"
