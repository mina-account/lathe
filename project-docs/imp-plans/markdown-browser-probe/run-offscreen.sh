#!/usr/bin/env bash
set -euo pipefail
runtime_directory=$(realpath "${1:?Pass the built runtime directory}")
report_path=${2:?Pass the JSONL report path}
for component in markdown_offscreen_probe libcef.so icudtl.dat resources.pak locales; do
    if [[ ! -e "$runtime_directory/$component" ]]; then
        printf 'Missing CEF probe runtime component: %s. Build the locked offscreen feature and retain copied runtime files.\n' "$component" >&2
        exit 1
    fi
done
mkdir -p "$(dirname "$report_path")"
export LD_LIBRARY_PATH="$runtime_directory${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
export LATHE_PROBE_OUTPUT="$(dirname "$report_path")/scene-images"
unset DISPLAY
timeout 35 "$runtime_directory/markdown_offscreen_probe" --smoke --require-wayland > "$report_path" 2> "$report_path.stderr"
printf 'Wayland offscreen browser smoke passed: %s\n' "$report_path"
