#!/usr/bin/env bash
# Run Rust quality gates via Make: format, lint, test.
# Usage: check.sh
# Discovers the nearest Makefile by walking up from the current directory.
# Exits 0 on success, non-zero on failure or missing Makefile.

set -euo pipefail

start_dir="$(pwd)"
dir="${start_dir}"

while true; do
	if [[ -f "${dir}/Makefile" ]]; then
		cd "${dir}"
		break
	fi
	parent="$(dirname "${dir}")"
	if [[ "${parent}" == "${dir}" ]]; then
		echo "error: no Makefile found from ${start_dir} upward; bootstrap the project before Rust checks" >&2
		exit 1
	fi
	dir="${parent}"
done

echo "rust-check: makefile root ${dir}"

make format
make lint
make test
