#!/usr/bin/env bats

setup() {
	load 'test_helper/common_setup'
	_common_setup
}

teardown() {
	_common_teardown
}

_make_local_pkg() {
	local dir="$1" name="$2" version="$3"
	mkdir -p "$dir"
	cat >"$dir/package.json" <<EOF
{"name":"$name","version":"$version","main":"index.js"}
EOF
	cat >"$dir/index.js" <<EOF
module.exports = "from $name";
EOF
}

@test "aube install handles file: directory dep" {
	_make_local_pkg vendor-dir vendor-dir 1.2.3

	mkdir -p app
	cd app
	cat >package.json <<'EOF'
{"name":"app","version":"0.0.0","dependencies":{"vendor-dir":"file:../vendor-dir"}}
EOF

	run aube install
	assert_success

	assert_file_exists node_modules/vendor-dir/package.json
	assert_file_exists node_modules/vendor-dir/index.js
	run cat node_modules/vendor-dir/package.json
	assert_output --partial '"version":"1.2.3"'

	# Lockfile should record the canonical `file:` specifier
	run cat aube-lock.yaml
	assert_output --partial 'specifier: file:../vendor-dir'
	assert_output --partial 'vendor-dir@file:../vendor-dir'
}

@test "aube install handles link: symlink dep" {
	_make_local_pkg vendor-link vendor-link 2.0.0

	mkdir -p app
	cd app
	cat >package.json <<'EOF'
{"name":"app","version":"0.0.0","dependencies":{"vendor-link":"link:../vendor-link"}}
EOF

	run aube install
	assert_success

	# link: deps are a direct symlink, not a `.aube/` entry.
	[ -L node_modules/vendor-link ]
	run readlink node_modules/vendor-link
	assert_output "../../vendor-link"
	assert_file_exists node_modules/vendor-link/package.json

	# Editing the target should be visible through the symlink.
	echo '{"name":"vendor-link","version":"2.0.1","main":"index.js"}' >../vendor-link/package.json
	run cat node_modules/vendor-link/package.json
	assert_output --partial '"version":"2.0.1"'
}

@test "aube install handles file: tarball dep" {
	# BSD tar (macOS) has no --transform, so stage the files under an
	# actual `package/` directory before archiving.
	mkdir -p staging/package app
	cat >staging/package/package.json <<'EOF'
{"name":"staged-pkg","version":"3.4.5","main":"index.js"}
EOF
	cat >staging/package/index.js <<'EOF'
module.exports = "from staged-pkg";
EOF
	(cd staging && tar -czf ../app/staged-pkg.tgz package)
	cd app

	cat >package.json <<'EOF'
{"name":"app","version":"0.0.0","dependencies":{"staged-pkg":"file:./staged-pkg.tgz"}}
EOF

	run aube install
	assert_success

	assert_file_exists node_modules/staged-pkg/package.json
	run cat node_modules/staged-pkg/package.json
	assert_output --partial '"version":"3.4.5"'
}

@test "excludeLinksFromLockfile omits link: deps from importers on write" {
	# With the flag on, adding a link: dep should leave the lockfile's
	# importers section clean — only the file: entry and any registry
	# deps should appear. The packages/snapshots sections are already
	# link-free unconditionally (pnpm parity), so this exclusively
	# exercises the importer-level filter.
	_make_local_pkg vendor-dir vendor-dir 1.0.0
	_make_local_pkg vendor-link vendor-link 1.0.0

	mkdir -p app
	cd app
	cat >.npmrc <<'RC'
exclude-links-from-lockfile=true
RC
	cat >package.json <<'EOF'
{"name":"app","version":"0.0.0","dependencies":{"vendor-dir":"file:../vendor-dir","vendor-link":"link:../vendor-link"}}
EOF
	run aube install
	assert_success

	# The link target is still symlinked into node_modules — the flag
	# is purely a lockfile-serialization knob, not a linker one.
	[ -L node_modules/vendor-link ]
	assert_file_exists node_modules/vendor-dir/package.json

	# Lockfile settings header reflects the choice.
	run grep "excludeLinksFromLockfile:" aube-lock.yaml
	assert_output --partial "true"

	# vendor-link must NOT appear in importers.
	run awk '/^importers:/,/^packages:/' aube-lock.yaml
	refute_output --partial "vendor-link:"
	# vendor-dir (file:) is unaffected and still listed.
	assert_output --partial "vendor-dir:"
}

@test "aube install round-trips file:/link: through the lockfile" {
	_make_local_pkg vendor-dir vendor-dir 1.0.0
	_make_local_pkg vendor-link vendor-link 1.0.0

	mkdir -p app
	cd app
	cat >package.json <<'EOF'
{"name":"app","version":"0.0.0","dependencies":{"vendor-dir":"file:../vendor-dir","vendor-link":"link:../vendor-link"}}
EOF

	run aube install
	assert_success

	rm -rf node_modules
	run aube install --frozen-lockfile
	assert_success
	assert_file_exists node_modules/vendor-dir/package.json
	[ -L node_modules/vendor-link ]
	run readlink node_modules/vendor-link
	assert_output "../../vendor-link"
}

@test "aube install handles file:/link: in a workspace importer" {
	# Workspace root + two workspace packages + two external local
	# packages the app depends on via file: / link:.
	_make_local_pkg vendor-dir vendor-dir 9.9.9
	_make_local_pkg vendor-link vendor-link 9.9.9

	cat >package.json <<'EOF'
{"name":"ws-root","version":"0.0.0","private":true}
EOF
	cat >pnpm-workspace.yaml <<'EOF'
packages:
  - "packages/*"
EOF
	mkdir -p packages/app
	cat >packages/app/package.json <<'EOF'
{"name":"app","version":"0.0.0","dependencies":{"vendor-dir":"file:../../vendor-dir","vendor-link":"link:../../vendor-link"}}
EOF

	run aube install
	assert_success

	assert_file_exists packages/app/node_modules/vendor-dir/package.json
	run cat packages/app/node_modules/vendor-dir/package.json
	assert_output --partial '"version":"9.9.9"'
	[ -L packages/app/node_modules/vendor-link ]
	# The symlink must actually resolve to the target's package.json
	# — a stale symlink pointing at the wrong base dir would silently
	# pass the `[ -L ]` check above.
	assert_file_exists packages/app/node_modules/vendor-link/package.json
	run cat packages/app/node_modules/vendor-link/package.json
	assert_output --partial '"version":"9.9.9"'
}
