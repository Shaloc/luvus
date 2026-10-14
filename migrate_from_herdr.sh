#!/bin/sh
# Run from a checkout of this fork. Python handles JSON and portable file I/O.
set -eu
command -v python3 >/dev/null 2>&1 || {
    printf '%s\n' 'error: Python 3.6 or newer is required' >&2
    exit 1
}
script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
exec python3 "$script_dir/scripts/migrate_from_herdr.py" "$@"
