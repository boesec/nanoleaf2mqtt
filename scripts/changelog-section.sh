#!/usr/bin/env bash
# Prints the body of one version's section of CHANGELOG.md (Keep a Changelog format), the release
# notes of that version:
#   changelog-section.sh <version>
# The section starts at the heading "## [<version>]" and ends before the next "## " heading or
# the link references at the end. Fails if the section is missing or empty.
set -euo pipefail

if [ "$#" -ne 1 ]; then
    echo "usage: $0 <version>" >&2
    exit 2
fi
version=$1
changelog="$(cd "$(dirname "$0")/.." && pwd)/CHANGELOG.md"

section=$(awk -v heading="## [$version]" '
    index($0, heading) == 1 { found = 1; next }
    found && (/^## / || /^\[[^]]+\]: /) { exit }
    found { print }
' "$changelog" | sed -e '/./,$!d')
# Leading blank lines are dropped by sed, trailing ones by the command substitution.

if [ -z "${section//[[:space:]]/}" ]; then
    echo "FAILED: no section for version $version in CHANGELOG.md" >&2
    exit 1
fi
printf '%s\n' "$section"
