#!/usr/bin/env bash
set -euo pipefail

file="$1"
first_line="$(grep -v '^#' "$file" | head -n 1)"
pattern='^(feat|fix|refactor|perf|test|docs|build|chore)(\([a-z0-9-]+\))?!?: .+'

if ! [[ "$first_line" =~ $pattern ]]; then
  echo "commit message must start with type(scope): subject" >&2
  echo "types: feat fix refactor perf test docs build chore" >&2
  exit 1
fi

if grep -qi '^co-authored-by:' "$file"; then
  echo "commit message must not contain Co-Authored-By" >&2
  exit 1
fi
