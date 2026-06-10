#!/usr/bin/env bash
set -euo pipefail

build_dir="$(mktemp -d)"
trap 'rm -rf "$build_dir"' EXIT

c++ -Wall -Wextra -Werror calc.cpp test_calc.cpp -o "$build_dir/test_calc"
"$build_dir/test_calc"
