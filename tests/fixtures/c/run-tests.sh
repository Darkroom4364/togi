#!/usr/bin/env bash
set -euo pipefail

build_dir="$(mktemp -d)"
trap 'rm -rf "$build_dir"' EXIT

cc -Wall -Wextra -Werror calc.c test_calc.c -o "$build_dir/test_calc"
"$build_dir/test_calc"
