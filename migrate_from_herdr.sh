#!/bin/sh
# Run from a checkout, a standalone download, or a pipe:
# curl -fsSL https://raw.githubusercontent.com/Shaloc/luvus/main/migrate_from_herdr.sh | sh -s -- --install
set -eu

# Define the whole entrypoint before running it when downloaded through a pipe.
main() {
    command -v python3 >/dev/null 2>&1 &&
        python3 -c 'import sys; sys.exit(sys.version_info < (3, 6))' || {
        printf '%s\n' 'error: Python 3.6 or newer is required' >&2
        exit 1
    }

    # A checkout uses its own matching files without downloading anything.
    if [ -f "$0" ]; then
        script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
        if [ -f "$script_dir/scripts/migrate_from_herdr.py" ] && [ -f "$script_dir/install.sh" ]; then
            exec python3 "$script_dir/scripts/migrate_from_herdr.py" "$@"
        fi
    fi

    command -v curl >/dev/null 2>&1 || {
        printf '%s\n' 'error: curl is required to download migration files' >&2
        exit 1
    }
    umask 077
    cache_dir="${XDG_CACHE_HOME:-$HOME/.cache}/luvus"
    mkdir -p "$cache_dir"
    stage=$(mktemp -d "$cache_dir/.herdr-migration.XXXXXX")
    trap 'rm -rf "$stage"' 0
    trap 'exit 130' INT
    trap 'exit 143' TERM
    mkdir "$stage/scripts"
    base=https://raw.githubusercontent.com/Shaloc/luvus/main
    for file in install.sh scripts/migrate_from_herdr.py; do
        curl -fsSL --connect-timeout 10 --max-time 60 -o "$stage/$file" "$base/$file"
    done
    # Keep this shell alive to remove the private downloads on success/failure.
    python3 "$stage/scripts/migrate_from_herdr.py" "$@"
}

main "$@"
