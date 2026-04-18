#!/usr/bin/env bats

# Peer dependency handling with pnpm-style peer contexts. pnpm defaults
# to `auto-install-peers=true`, so a package that declares a required peer
# gets that peer installed as a sibling symlink inside its own
# `node_modules/` — which is how Node's module resolver finds it when the
# package does `require('react')`. Our resolver's peer-context post-pass
# additionally suffixes each consumer's dep_path with `(peer@ver)` so the
# same package with different peer resolutions lands in separate
# `.aube/` directories, matching pnpm's v9 lockfile shape.

setup() {
	load 'test_helper/common_setup'
	_common_setup
}

teardown() {
	_common_teardown
}

@test "required peer is auto-installed and sibling-linked with peer-suffix dep_path" {
	# use-sync-external-store@1.2.0 declares peerDep react ^16.8 || ^17 || ^18.
	# Intentionally do NOT list react in package.json — auto-install-peers
	# should still drag it in and the post-pass should still sibling-link
	# it inside use-sync-external-store's .aube directory.
	cat >package.json <<'JSON'
{
  "name": "peer-test",
  "version": "1.0.0",
  "dependencies": {
    "use-sync-external-store": "1.2.0"
  }
}
JSON
	run aube install
	assert_success

	# Top-level symlink follows.
	assert_link_exists node_modules/use-sync-external-store

	# Auto-installed peer is hoisted to the root importer — pnpm
	# parity. react should be a top-level symlink even though the
	# user didn't list it.
	assert_link_exists node_modules/react

	# Some react version must exist under .aube (which version doesn't
	# matter — depends on whatever latest satisfies ^16.8 || ^17 || ^18).
	run bash -c 'ls node_modules/.aube | grep ^react@ | head -1'
	assert_success
	assert_output --partial "react@"

	# Lockfile's importers section lists react with the declared peer
	# range as the specifier — that's what pnpm writes for hoisted
	# auto-installed peers.
	run bash -c 'grep -A2 "^      react:" aube-lock.yaml | head -3'
	assert_success
	assert_output --partial "specifier:"
	assert_output --partial "^16.8.0"

	# The use-sync-external-store directory name should include a
	# `_react@...` peer suffix — that's the core parity change. The
	# lockfile still writes the raw pnpm-style `(react@...)`; it's
	# only the on-disk filename that flattens the parens into `_`
	# via `dep_path_to_filename` so peer-heavy graphs don't overflow
	# NAME_MAX.
	run bash -c 'ls node_modules/.aube | grep ^use-sync-external-store@'
	assert_success
	assert_output --partial "_react@"

	# Sibling react symlink exists inside use-sync-external-store's
	# virtual-store node_modules — what Node's resolver actually walks.
	local uses_dir
	uses_dir=$(find node_modules/.aube -maxdepth 1 -name 'use-sync-external-store@*' -print -quit)
	[ -n "$uses_dir" ]
	assert_link_exists "$uses_dir/node_modules/react"

	# Sanity: resolve react from inside the package.
	run node -e 'console.log(require.resolve("react", { paths: [require.resolve("use-sync-external-store")] }))'
	assert_success
	assert_output --partial "react"
}

@test "required peer dedupes to a root-installed version and still sibling-links" {
	# react is pinned at the root — the peer walker should reuse that
	# version and the post-pass should still create the sibling link,
	# and the contextualized dep_path should embed react@17.0.2.
	cat >package.json <<'JSON'
{
  "name": "peer-dedupe",
  "version": "1.0.0",
  "dependencies": {
    "react": "17.0.2",
    "use-sync-external-store": "1.2.0"
  }
}
JSON
	run aube install
	assert_success

	# Exactly one react canonical version in the virtual store.
	# Peer-contextualized react dirs (hypothetical, since react itself
	# has no peers) would show up as `react@VER_something@...`; the
	# `_` filter strips them, leaving only bare `react@VER` entries.
	run bash -c 'ls node_modules/.aube | grep ^react@ | grep -v "_" | wc -l | tr -d " "'
	assert_success
	assert_output "1"

	run bash -c 'ls node_modules/.aube | grep "^react@17.0.2$"'
	assert_success

	# use-sync-external-store's peer-contextualized directory references
	# react@17.0.2 — that's the dedupe-to-root happening via the post-pass.
	# On disk the parens flatten to `_` (see `dep_path_to_filename`).
	run bash -c 'ls node_modules/.aube | grep "^use-sync-external-store@1.2.0_react@17.0.2$"'
	assert_success

	# Sibling symlink exists and points at the same react@17.0.2 we resolved.
	local uses_dir
	uses_dir=$(find node_modules/.aube -maxdepth 1 -name 'use-sync-external-store@1.2.0_*' -print -quit)
	[ -n "$uses_dir" ]
	assert_link_exists "$uses_dir/node_modules/react"

	# And `require('react')` from within the package resolves to 17.0.2.
	run node -e 'console.log(require.resolve("react", { paths: [require.resolve("use-sync-external-store")] }))'
	assert_success
	assert_output --partial "react@17.0.2"
}

@test "user pin outside declared peer range stays pinned and warns" {
	# react@19.0.0 does NOT satisfy use-sync-external-store's declared
	# peer range (^16.8.0 || ^17.0.0 || ^18.0.0). pnpm keeps the user's
	# direct pin authoritative, wires that version into the peer context,
	# and reports the mismatch instead of installing a second satisfying
	# react@18 tree.
	cat >package.json <<'JSON'
{
  "name": "per-range-auto-resolve",
  "version": "1.0.0",
  "dependencies": {
    "react": "19.0.0",
    "use-sync-external-store": "1.2.0"
  }
}
JSON
	run aube install
	assert_success
	assert_output --partial "Issues with peer dependencies found"
	assert_output --partial "expected peer react@^16.8.0 || ^17.0.0 || ^18.0.0, found 19.0.0"

	# Only the user's pinned react version lives in the lockfile.
	run bash -c 'grep -q "^  react@19.0.0:" aube-lock.yaml'
	assert_success
	run bash -c 'grep -qE "^  react@18[^(]*:" aube-lock.yaml'
	assert_failure

	# use-sync-external-store's snapshot key references the root pin,
	# matching pnpm's lockfile shape for this mismatch.
	run bash -c 'grep -F "use-sync-external-store@1.2.0(react@19" aube-lock.yaml'
	assert_success

	# Top-level node_modules/react points at the user's pin (react@19).
	run readlink node_modules/react
	assert_output --partial "react@19.0.0"
}

@test "auto-install-peers=false leaves peers alone and warns they're missing" {
	# With auto-install disabled, the resolver must NOT drag in any peer
	# version on its own: no top-level node_modules/react, no hoisted
	# react entry in the lockfile importers section, no peer-context
	# suffix on use-sync-external-store. The unmet warning still fires.
	cat >.npmrc <<'RC'
auto-install-peers=false
RC
	cat >package.json <<'JSON'
{
  "name": "peers-off",
  "version": "1.0.0",
  "dependencies": {
    "use-sync-external-store": "1.2.0"
  }
}
JSON
	run aube install
	assert_success
	assert_output --partial "Issues with peer dependencies found"
	assert_output --partial "missing required peer react@"

	# No top-level react symlink.
	run test -e node_modules/react
	assert_failure

	# No .aube/react@* entry at all.
	run bash -c 'ls node_modules/.aube 2>/dev/null | grep ^react@ || true'
	refute_output --partial "react@"

	# use-sync-external-store's virtual dir has no peer-context suffix
	# (neither the raw `(react@...)` pnpm form nor the on-disk
	# `_react@...` flattened form).
	run bash -c 'ls node_modules/.aube | grep ^use-sync-external-store@'
	assert_success
	refute_output --partial "_react@"

	# The lockfile's importers section lists only use-sync-external-store.
	# (react appears once lower down under the package's own
	# `peerDependencies:` declaration, which is metadata — not a hoist.)
	run awk '/^importers:/,/^packages:/' aube-lock.yaml
	assert_success
	refute_output --partial "react:"

	# The lockfile's settings header records the off state so
	# subsequent installs stay consistent.
	run grep "autoInstallPeers:" aube-lock.yaml
	assert_output --partial "false"
}

@test "no unmet peer warning when resolved version satisfies the declared range" {
	# react@17.0.2 satisfies ^16.8.0 || ^17.0.0 || ^18.0.0 — silent install.
	cat >package.json <<'JSON'
{
  "name": "met-peer",
  "version": "1.0.0",
  "dependencies": {
    "react": "17.0.2",
    "use-sync-external-store": "1.2.0"
  }
}
JSON
	run aube install
	assert_success
	refute_output --partial "Issues with peer dependencies"
	refute_output --partial "expected peer"
}

@test "conflicting peer ranges keep user pins and warn" {
	# Pin react@17 at the root while pulling in @testing-library/react@14,
	# which declares peers react: ^18 and react-dom: ^18. pnpm keeps the
	# user's direct pins authoritative and reports unmet peer ranges
	# instead of installing parallel react@18/react-dom@18 trees.
	cat >package.json <<'JSON'
{
  "name": "per-range-peer",
  "version": "1.0.0",
  "dependencies": {
    "react": "17.0.2",
    "react-dom": "17.0.2",
    "@testing-library/react": "14.0.0"
  }
}
JSON
	run aube install
	assert_success
	assert_output --partial "Issues with peer dependencies found"
	assert_output --partial "expected peer react@^18.0.0, found 17.0.2"
	assert_output --partial "expected peer react-dom@^18.0.0, found 17.0.2"

	# Only the user's pinned react version should exist in the lockfile.
	run bash -c 'grep -q "^  react@17.0.2:" aube-lock.yaml'
	assert_success
	run bash -c 'grep -qE "^  react@18[^(]*:" aube-lock.yaml'
	assert_failure

	# `@testing-library/react`'s snapshot key references the root pins,
	# matching pnpm's lockfile shape for this mismatch.
	run bash -c 'grep -F "@testing-library/react@14.0.0" aube-lock.yaml | grep -F "react@17.0.2" | grep -F "react-dom@17.0.2"'
	assert_success

	# Top-level node_modules/react points at 17 (the user's pin).
	run readlink node_modules/react
	assert_output --partial "react@17.0.2"
}

@test "nested peer suffixes in lockfile match pnpm v9" {
	# `@testing-library/react@14` peers on both `react` and `react-dom`.
	# `react-dom` itself peers on `react`. pnpm writes
	# `@testing-library/react@14.0.0(react@18.2.0)(react-dom@18.2.0(react@18.2.0))`
	# — the inner `react-dom@18.2.0(react@18.2.0)` is nested. aube's
	# peer-context fixed-point loop should produce the same shape.
	cat >package.json <<'JSON'
{
  "name": "nested-peer",
  "version": "1.0.0",
  "dependencies": {
    "react": "18.2.0",
    "react-dom": "18.2.0",
    "@testing-library/react": "14.0.0"
  }
}
JSON
	run aube install
	assert_success

	# Lockfile must contain the nested snapshot key.
	run grep -F "@testing-library/react@14.0.0(react@18.2.0)(react-dom@18.2.0(react@18.2.0))" aube-lock.yaml
	assert_success
}

@test "strict-peer-dependencies fails install on unmet required peer" {
	# With auto-install-peers off, use-sync-external-store's required
	# react peer is unresolvable. Plain install warns but succeeds;
	# strict-peer-dependencies should flip the same condition into a
	# hard failure with the error-level diagnostic lines.
	cat >.npmrc <<'RC'
auto-install-peers=false
strict-peer-dependencies=true
RC
	cat >package.json <<'JSON'
{
  "name": "strict-peer",
  "version": "1.0.0",
  "dependencies": {
    "use-sync-external-store": "1.2.0"
  }
}
JSON
	run aube install
	assert_failure
	assert_output --partial "Issues with peer dependencies found"
	assert_output --partial "missing required peer react@"
	assert_output --partial "strict-peer-dependencies is enabled"
}

@test "strict-peer-dependencies is off by default — unmet peers warn only" {
	# Same setup as the strict test but without the strict flag. Must
	# succeed and emit the plain warn: prefix, not error:.
	cat >.npmrc <<'RC'
auto-install-peers=false
RC
	cat >package.json <<'JSON'
{
  "name": "nonstrict-peer",
  "version": "1.0.0",
  "dependencies": {
    "use-sync-external-store": "1.2.0"
  }
}
JSON
	run aube install
	assert_success
	assert_output --partial "warn: Issues with peer dependencies found"
	refute_output --partial "strict-peer-dependencies is enabled"
}

@test "peerDependencyRules.ignoreMissing silences missing-peer warning" {
	# pnpm.peerDependencyRules.ignoreMissing in package.json should
	# suppress the unmet-peer warning for matching names. The underlying
	# condition (auto-install-peers=false + declared required peer) is
	# identical to the plain-warn test above; the escape hatch is what
	# differs.
	cat >.npmrc <<'RC'
auto-install-peers=false
RC
	cat >package.json <<'JSON'
{
  "name": "ignore-missing-peer",
  "version": "1.0.0",
  "dependencies": {
    "use-sync-external-store": "1.2.0"
  },
  "pnpm": {
    "peerDependencyRules": {
      "ignoreMissing": ["react"]
    }
  }
}
JSON
	run aube install
	assert_success
	refute_output --partial "Issues with peer dependencies found"
}

@test "peerDependencyRules.ignoreMissing also silences strict-peer-dependencies error" {
	# Same rule should suppress the hard failure under strict mode —
	# otherwise the escape hatch would be useless in CI setups that
	# enable strict-peer-dependencies across the board.
	cat >.npmrc <<'RC'
auto-install-peers=false
strict-peer-dependencies=true
RC
	cat >package.json <<'JSON'
{
  "name": "strict-ignored",
  "version": "1.0.0",
  "dependencies": {
    "use-sync-external-store": "1.2.0"
  },
  "pnpm": {
    "peerDependencyRules": {
      "ignoreMissing": ["react"]
    }
  }
}
JSON
	run aube install
	assert_success
	refute_output --partial "strict-peer-dependencies is enabled"
}

@test "peerDependencyRules.ignoreMissing respects glob patterns" {
	# `react*` should catch `react` but NOT an unrelated missing peer.
	# Verified here by pointing ignoreMissing at a non-matching pattern
	# and confirming the warning still fires.
	cat >.npmrc <<'RC'
auto-install-peers=false
RC
	cat >package.json <<'JSON'
{
  "name": "ignore-mismatch",
  "version": "1.0.0",
  "dependencies": {
    "use-sync-external-store": "1.2.0"
  },
  "pnpm": {
    "peerDependencyRules": {
      "ignoreMissing": ["vue*"]
    }
  }
}
JSON
	run aube install
	assert_success
	assert_output --partial "warn: Issues with peer dependencies found"
	assert_output --partial "missing required peer react@"
}

@test "peerDependencyRules.ignoreMissing is read from pnpm-workspace.yaml" {
	# Same behavior as the package.json case above, but sourced from
	# pnpm-workspace.yaml. Covers the settings-generator path.
	cat >.npmrc <<'RC'
auto-install-peers=false
RC
	cat >pnpm-workspace.yaml <<'YAML'
peerDependencyRules:
  ignoreMissing:
    - react
YAML
	cat >package.json <<'JSON'
{
  "name": "ignore-missing-ws",
  "version": "1.0.0",
  "dependencies": {
    "use-sync-external-store": "1.2.0"
  }
}
JSON
	run aube install
	assert_success
	refute_output --partial "Issues with peer dependencies found"
}

@test "peerDependencyRules.allowAny bypasses semver mismatch on resolved peer" {
	# auto-install-peers=false + react@19 pinned at root means
	# use-sync-external-store resolves its peer to react@19, which is
	# outside its declared range (^16.8 || ^17 || ^18). That fires the
	# "expected peer react@..., found 19" warning normally; allowAny
	# silences it.
	cat >.npmrc <<'RC'
auto-install-peers=false
RC
	cat >package.json <<'JSON'
{
  "name": "allow-any-peer",
  "version": "1.0.0",
  "dependencies": {
    "use-sync-external-store": "1.2.0",
    "react": "19.0.0"
  },
  "pnpm": {
    "peerDependencyRules": {
      "allowAny": ["react"]
    }
  }
}
JSON
	run aube install
	assert_success
	refute_output --partial "Issues with peer dependencies found"
}

@test "peerDependencyRules.allowAny also silences missing-peer warnings" {
	# allowAny is a strict superset of ignoreMissing: it bypasses the
	# semver check and also silences missing peers for matching names.
	# Verified here without pinning react — the peer is literally
	# missing and allowAny: react silences it.
	cat >.npmrc <<'RC'
auto-install-peers=false
RC
	cat >package.json <<'JSON'
{
  "name": "allow-any-missing",
  "version": "1.0.0",
  "dependencies": {
    "use-sync-external-store": "1.2.0"
  },
  "pnpm": {
    "peerDependencyRules": {
      "allowAny": ["react"]
    }
  }
}
JSON
	run aube install
	assert_success
	refute_output --partial "Issues with peer dependencies found"
}
