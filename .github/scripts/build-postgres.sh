#!/bin/sh
# Build PostgreSQL at a revision and install it into a prefix, with
# auto_explain from contrib, for CI:
#   build-postgres.sh <revision> <prefix> [release]
# With assertions and debugging information, as in development, or
# with "release" without them, for observations of the counts; without
# ICU and readline, which neither needs.
set -eu
REVISION=$1
PREFIX=$2
case "${3:-assert}" in
assert) CHECKS="--enable-cassert --enable-debug" ;;
release) CHECKS="" ;;
*) echo "unknown flavour $3" >&2; exit 1 ;;
esac
SOURCE=$(mktemp -d)
git -C "$SOURCE" init -q
git -C "$SOURCE" fetch -q --depth 1 https://github.com/postgres/postgres.git "$REVISION"
git -C "$SOURCE" checkout -q FETCH_HEAD
cd "$SOURCE"
# shellcheck disable=SC2086
./configure -q --prefix="$PREFIX" $CHECKS --without-icu --without-readline
make -s -j"$(getconf _NPROCESSORS_ONLN)"
make -s install
make -s -C contrib/auto_explain install
