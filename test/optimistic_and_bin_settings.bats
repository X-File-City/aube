#!/usr/bin/env bats

# Coverage for three `.npmrc`-only settings landed together because they
# share a wiring pattern but do not share a code path:
#
#   * optimisticRepeatInstall    — gates the `ensure_installed` fast path
#   * extendNodePath             — injects NODE_PATH into `.bin` shims
#   * preferSymlinkedExecutables — POSIX `.bin` entries: symlink vs shim
#
# All tests run against the offline fixture registry; see
# test/registry/config.yaml.
#
# bats file_tags=serial

# Force within-file tests to run one at a time regardless of --jobs.
# shellcheck disable=SC2034
BATS_NO_PARALLELIZE_WITHIN_FILE=1

setup() {
	load 'test_helper/common_setup'
	_common_setup
}

teardown() {
	_common_teardown
}

# ---- optimisticRepeatInstall ------------------------------------------

@test "optimisticRepeatInstall defaults to on — second run skips auto-install" {
	_setup_basic_fixture
	aube install
	run aube run hello
	assert_success
	refute_output --partial "Auto-installing"
}

@test "optimisticRepeatInstall=false forces auto-install on every run" {
	_setup_basic_fixture
	aube install
	echo "optimisticRepeatInstall=false" >>.npmrc

	run aube run hello
	assert_success
	assert_output --partial "Auto-installing"
	assert_output --partial "optimisticRepeatInstall=false"
	assert_output --partial "hello from aube!"
}

@test "optimisticRepeatInstall=true is equivalent to the default" {
	_setup_basic_fixture
	aube install
	echo "optimisticRepeatInstall=true" >>.npmrc

	run aube run hello
	assert_success
	refute_output --partial "Auto-installing"
}

# ---- extendNodePath + preferSymlinkedExecutables ----------------------

# Fixture: single-bin package with its tarball fully committed to the
# offline registry. `loose-envify@1.4.0` ships `cli.js` as the
# `loose-envify` bin and pulls in `js-tokens`, both of which are
# pre-seeded under test/registry/storage/.
_setup_bin_fixture() {
	cat >package.json <<-EOF
		{
		  "name": "aube-test-bin-shims",
		  "version": "1.0.0",
		  "dependencies": { "loose-envify": "1.4.0" }
		}
	EOF
}

@test "default POSIX .bin/<name> entry is a symlink" {
	_setup_bin_fixture
	aube install
	assert_file_exists node_modules/.bin/loose-envify
	# Symlink, not a regular file — the pre-settings behavior aube has
	# always shipped on POSIX.
	[ -L node_modules/.bin/loose-envify ]
}

@test "preferSymlinkedExecutables=false writes a POSIX shell shim" {
	_setup_bin_fixture
	echo "preferSymlinkedExecutables=false" >>.npmrc
	aube install

	assert_file_exists node_modules/.bin/loose-envify
	# Regular file, not a symlink.
	[ ! -L node_modules/.bin/loose-envify ]
	# Shell script starts with `#!/bin/sh`.
	head -n1 node_modules/.bin/loose-envify | grep -q '^#!/bin/sh'
	# Must be marked executable so the `.bin` entry can be invoked
	# directly without `sh <path>`.
	[ -x node_modules/.bin/loose-envify ]
}

@test "preferSymlinkedExecutables=false + extendNodePath=true exports NODE_PATH" {
	_setup_bin_fixture
	cat >>.npmrc <<-EOF
		preferSymlinkedExecutables=false
		extendNodePath=true
	EOF
	aube install

	# shellcheck disable=SC2016  # literal `$basedir` is the content we grep for
	grep -q 'export NODE_PATH="\$basedir/\.\."' node_modules/.bin/loose-envify
}

@test "extendNodePath=false suppresses NODE_PATH in the shim" {
	_setup_bin_fixture
	cat >>.npmrc <<-EOF
		preferSymlinkedExecutables=false
		extendNodePath=false
	EOF
	aube install

	assert_file_exists node_modules/.bin/loose-envify
	run grep -c 'NODE_PATH' node_modules/.bin/loose-envify
	# `grep -c` exits 1 on zero matches, so a blanket `assert_failure`
	# would pass for the wrong reason — compare the match count directly
	# instead of relying on the exit status.
	[ "$output" = "0" ]
}

@test "preferSymlinkedExecutables=false shim is actually invokable" {
	_setup_bin_fixture
	echo "preferSymlinkedExecutables=false" >>.npmrc
	aube install

	# Exec the shim directly — the wrapper should run the JS entry
	# with node. `loose-envify --help` errors out without an input
	# file, but the relevant signal is that the shim starts node at
	# all: 127 = command not found (bad interpreter path), 126 =
	# permission denied (missing +x). Anything else means the shim
	# handed control off to node as intended.
	run node_modules/.bin/loose-envify --help
	[ "$status" -ne 127 ]
	[ "$status" -ne 126 ]
}
