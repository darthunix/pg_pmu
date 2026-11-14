#!/bin/sh
# Build PostgreSQL at a revision and install it into a prefix, with
# auto_explain from contrib, for CI:
#   build-postgres.sh <revision> <prefix>
# With assertions and debugging information, as in development; without
# ICU and readline, which the suites do not need.
set -eu
REVISION=$1
PREFIX=$2
SOURCE=$(mktemp -d)
git -C "$SOURCE" init -q
git -C "$SOURCE" fetch -q --depth 1 https://github.com/postgres/postgres.git "$REVISION"
git -C "$SOURCE" checkout -q FETCH_HEAD
cd "$SOURCE"
./configure -q --prefix="$PREFIX" --enable-cassert --enable-debug \
	--without-icu --without-readline
make -s -j"$(getconf _NPROCESSORS_ONLN)"
make -s install
make -s -C contrib/auto_explain install
