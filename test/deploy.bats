#!/usr/bin/env bats

setup() {
	load 'test_helper/common_setup'
	_common_setup
}

teardown() {
	_common_teardown
}

_setup_workspace_fixture() {
	cp -r "$PROJECT_ROOT/fixtures/workspace/"* .
}

@test "aube deploy: copies selected workspace package into target and installs deps" {
	_setup_workspace_fixture

	run aube deploy --filter @test/lib ./out
	assert_success
	assert_output --partial "deployed @test/lib@1.0.0"

	# Package files were copied verbatim.
	assert_file_exists out/package.json
	assert_file_exists out/index.js

	# The install ran rooted at `out/`, so its deps are present.
	assert_dir_exists out/node_modules/is-odd

	# `out/` is standalone: no workspace/monorepo artifacts leaked in.
	[ ! -f out/pnpm-workspace.yaml ]
}

@test "aube deploy: subsets the source lockfile into the target" {
	_setup_workspace_fixture

	# Prime the source workspace so there's a real aube-lock.yaml to
	# subset. Without this, deploy falls back to a fresh install and
	# the new subset path isn't exercised.
	run aube install
	assert_success
	assert_file_exists aube-lock.yaml

	run aube deploy --filter @test/lib ./out
	assert_success

	# Subset lockfile written alongside the staged package files. Keep
	# the format the source workspace uses (aube-lock.yaml here —
	# pnpm-lock.yaml, yarn.lock, etc. would carry through via
	# detect_existing_lockfile_kind).
	assert_file_exists out/aube-lock.yaml

	# Target is the sole importer, rekeyed to `.`. Source workspace
	# importers (`packages/lib`, `packages/app`) must not leak in —
	# otherwise a frozen install would see ghost entries.
	run grep -c "^  \.:" out/aube-lock.yaml
	assert_output "1"
	run grep -E "^  packages/" out/aube-lock.yaml
	assert_failure

	# Transitive closure pruned correctly: is-odd is in, anything only
	# @test/app needs is not. `@test/app`'s importer is pruned, so its
	# direct deps (e.g. the workspace link on @test/lib) don't appear.
	run grep -E "is-odd|is-number" out/aube-lock.yaml
	assert_success
	run grep -E "^  '?@test/app" out/aube-lock.yaml
	assert_failure
}

@test "aube deploy: rewrites workspace: deps to concrete sibling versions" {
	_setup_workspace_fixture

	# `@test/app` originally has `"@test/lib": "workspace:*"`. Deploy will
	# rewrite the manifest, then run install — which fails here because
	# @test/lib isn't in the fixture registry. That's fine: we just want
	# to verify the manifest was rewritten *before* install ran.
	run aube deploy --filter @test/app ./out
	assert_failure

	assert_file_exists out/package.json
	run node -e "console.log(require('./out/package.json').dependencies['@test/lib'])"
	assert_success
	assert_output "1.0.0"
}

@test "aube deploy: errors when --filter does not match a workspace package" {
	_setup_workspace_fixture

	run aube deploy --filter @test/does-not-exist ./out
	assert_failure
	# Error wraps across lines, so match a short substring that survives.
	assert_output --partial "did not match"
}

@test "aube deploy: refuses to deploy into a non-empty target" {
	_setup_workspace_fixture
	mkdir -p out
	echo hi >out/sentinel

	run aube deploy --filter @test/lib ./out
	assert_failure
	assert_output --partial "not empty"
}

@test "aube deploy: glob filter fans out across every match" {
	_setup_workspace_fixture

	# `@test/*` matches both @test/lib and @test/app. Packages are
	# sorted by name before staging, so the plan is
	# [@test/app → out/app, @test/lib → out/lib]. Staging for both
	# packages runs up front, then the @test/app install runs first
	# and fails because @test/lib isn't in the fixture registry (same
	# reason the existing rewrite test expects failure). The multi-
	# package deploy bails after that failure, so @test/lib never
	# gets an install run — but both target directories must exist
	# with their rewritten manifests because staging precedes install.
	run aube deploy --filter "@test/*" ./out
	assert_file_exists out/lib/package.json
	assert_file_exists out/app/package.json
	# workspace: ref in out/app was rewritten to the concrete version
	# during staging, even though install later failed.
	run node -e "console.log(require('./out/app/package.json').dependencies['@test/lib'])"
	assert_success
	assert_output "1.0.0"
}

@test "aube deploy: multi-match refuses a non-empty target" {
	_setup_workspace_fixture
	mkdir -p out
	echo hi >out/sentinel

	run aube deploy --filter "@test/*" ./out
	assert_failure
	assert_output --partial "not empty"
}

# Narrow @test/lib's publish surface to just package.json + index.js so
# `scripts/run.sh` and `tests/fixture.txt` are off the pack path. The
# `deployAllFiles` tests below rely on that exclusion.
_setup_lib_with_unpublished_files() {
	_setup_workspace_fixture
	# Rewrite package.json with a `files` field that excludes our extras.
	cat >packages/lib/package.json <<'EOF'
{
  "name": "@test/lib",
  "version": "1.0.0",
  "main": "index.js",
  "files": ["index.js"],
  "dependencies": {
    "is-odd": "^3.0.1"
  }
}
EOF
	mkdir -p packages/lib/scripts packages/lib/tests
	echo "#!/bin/sh" >packages/lib/scripts/run.sh
	echo "fixture data" >packages/lib/tests/fixture.txt
}

@test "aube deploy: default honors pack's selection (files field excludes extras)" {
	_setup_lib_with_unpublished_files

	run aube deploy --filter @test/lib ./out
	assert_success

	assert_file_exists out/package.json
	assert_file_exists out/index.js
	# The `files` field restricted publish to index.js + package.json
	# (+ always-on files, but scripts/tests aren't in that set), so
	# deploy's default path must not copy them either.
	[ ! -f out/scripts/run.sh ]
	[ ! -f out/tests/fixture.txt ]
}

@test "aube deploy: deploy-all-files=true copies files pack's selection skips" {
	_setup_lib_with_unpublished_files
	# Project-level .npmrc is the source of truth for the deploy
	# command (read before any chdir into the target).
	echo "deploy-all-files=true" >.npmrc

	run aube deploy --filter @test/lib ./out
	assert_success

	# Publish surface is still there...
	assert_file_exists out/package.json
	assert_file_exists out/index.js
	# ...but so are the files that `files` / `.npmignore` would have
	# filtered.
	assert_file_exists out/scripts/run.sh
	assert_file_exists out/tests/fixture.txt
}

@test "aube deploy: deployAllFiles in pnpm-workspace.yaml honored" {
	_setup_lib_with_unpublished_files
	# Append the setting to the existing workspace yaml so we exercise
	# the workspaceYaml source path (camelCase alias).
	printf "\ndeployAllFiles: true\n" >>pnpm-workspace.yaml

	run aube deploy --filter @test/lib ./out
	assert_success

	assert_file_exists out/scripts/run.sh
	assert_file_exists out/tests/fixture.txt
}

@test "aube deploy: deploy-all-files=true still skips node_modules and .git" {
	_setup_lib_with_unpublished_files
	echo "deploy-all-files=true" >.npmrc

	# Pre-populate node_modules/ and .git/ inside the source package.
	# Both are filesystem cruft that must never end up in the deploy
	# target, even when "all files" is on.
	mkdir -p packages/lib/node_modules/ghost packages/lib/.git
	echo '{"name":"ghost"}' >packages/lib/node_modules/ghost/package.json
	echo "ref: refs/heads/main" >packages/lib/.git/HEAD

	run aube deploy --filter @test/lib ./out
	assert_success

	assert_file_exists out/scripts/run.sh
	[ ! -e out/node_modules/ghost ]
	[ ! -e out/.git ]
}

@test "aube deploy: deploy-all-files=true copies symlinked files via their target" {
	# Regression: `DirEntry::file_type()` uses lstat, so without
	# following file symlinks the walk would silently drop them —
	# contradicting the "copy every file" promise. Directory
	# symlinks are intentionally skipped (cycle risk) and covered
	# below.
	_setup_lib_with_unpublished_files
	echo "deploy-all-files=true" >.npmrc

	# Symlinked file: deploy target should receive the real content.
	echo "linked payload" >packages/lib/real.txt
	ln -s real.txt packages/lib/linked.txt

	# Symlinked directory pointing at a sibling inside the package.
	# Must NOT recurse into it (cycle / out-of-tree risk) — the
	# link itself is silently dropped. Contents still reach the
	# target through the direct `scripts/` walk.
	ln -s scripts packages/lib/scripts-alias

	run aube deploy --filter @test/lib ./out
	assert_success

	# Symlinked file: content copied verbatim (fs::copy follows).
	assert_file_exists out/linked.txt
	run cat out/linked.txt
	assert_output "linked payload"

	# Symlinked directory: neither the link nor a copy of the dir
	# under the alias name ended up in the target.
	[ ! -e out/scripts-alias ]
	# The real directory was walked directly, so its content is there.
	assert_file_exists out/scripts/run.sh
}
