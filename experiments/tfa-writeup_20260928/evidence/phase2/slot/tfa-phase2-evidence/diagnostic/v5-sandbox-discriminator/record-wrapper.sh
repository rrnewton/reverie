#!/bin/bash
# Diagnostic: record exactly what the Meta launcher execs, then exec it unchanged.
{ printf 'ARGV'; printf ' %q' "$@"; printf '\n'; env -0 | sort -z | tr '\0' '\n' | sed 's/^/ENV /'; } > "$RECORD_OUT"
exec "$@"
