#!/bin/sh
# Luvus Qoder CLI integration. Qoder passes SessionStart JSON through stdin;
# the Luvus binary parses only its bounded session_id field.

[ "${1:-}" = "session" ] || exit 0
if [ "${LUVUS_ENV:-}" != "1" ] || [ -z "${LUVUS_SOCKET_PATH:-}" ] || [ -z "${LUVUS_PANE_ID:-}" ]; then
  printf '{}\n'
  exit 0
fi

luvus_bin="${LUVUS_BIN_PATH:-luvus}"
"$luvus_bin" integration hook qodercli 2>/dev/null || printf '{}\n'
