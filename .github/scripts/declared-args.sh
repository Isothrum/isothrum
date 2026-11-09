#!/usr/bin/env bash
# Prints, one per line, the `isothrum` arguments for the candidates a run manifest declares, so a
# workflow can offer every value it has and pass only those the generated tree takes:
#
#   param:NAME=VALUE    --param NAME=VALUE           when NAME is one of the manifest's params
#   secret:NAME=VAR     --secret-from-env NAME=VAR   when NAME is one of its secret slots and VAR
#                                                    is set (the value never reaches the line)
#
# A candidate with an empty value is skipped. Read the output with `mapfile -t args < file`.
#
# Usage: declared-args.sh <run.json> <candidate>...
set -euo pipefail

manifest=$1
shift

declares() {
  jq -e --arg list "$1" --arg name "$2" 'any(.[$list][]?; .name == $name)' "$manifest" >/dev/null
}

for candidate in "$@"; do
  kind=${candidate%%:*}
  rest=${candidate#*:}
  name=${rest%%=*}
  value=${rest#*=}
  if [ -z "$value" ]; then
    continue
  fi
  case $kind in
    param)
      if declares params "$name"; then
        printf '%s\n' --param "$name=$value"
      fi
      ;;
    secret)
      if declares secrets "$name" && [ -n "${!value:-}" ]; then
        printf '%s\n' --secret-from-env "$name=$value"
      fi
      ;;
    *)
      echo "declared-args.sh: unknown candidate kind in '$candidate' (param: or secret:)" >&2
      exit 2
      ;;
  esac
done
