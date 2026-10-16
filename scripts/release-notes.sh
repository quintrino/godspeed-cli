#!/usr/bin/env bash
# Prints the release notes for a version from the changelog: the lines under the heading
# "## VERSION" (which may go on with ": date" or " (date)") up to the next "## " heading, without
# the blank lines at either end. "###" sub-headings are part of the notes.
#
#   bash scripts/release-notes.sh 0.11.0 [CHANGELOG.md]
#
# Exits with status 1, printing nothing, if the changelog has no heading for the version or the
# section under it is empty. Exits with status 2 if it is run without a version.
set -euo pipefail

if [ $# -lt 1 ] || [ -z "$1" ]; then
    echo "usage: $0 VERSION [CHANGELOG]" >&2
    exit 2
fi
version=$1
changelog=${2:-CHANGELOG.md}

awk -v v="$version" '
    index($0, "## " v) == 1 {
        rest = substr($0, length("## " v) + 1)
        if (rest == "" || rest ~ /^[ :]/) { inside = 1; next }
    }
    inside && /^## / { exit }
    inside { lines[n++] = $0 }
    END {
        first = 0
        while (first < n && lines[first] ~ /^[ \t]*$/) first++
        last = n - 1
        while (last >= first && lines[last] ~ /^[ \t]*$/) last--
        if (last < first) exit 1
        for (i = first; i <= last; i++) print lines[i]
    }
' "$changelog"
