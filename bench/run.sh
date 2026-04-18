#!/usr/bin/env bash
# Benchmark aube against npm, yarn, and pnpm across the same scenarios used
# at https://pnpm.io/benchmarks. Results are written to bench/results.json
# and bench/results.md.
#
# Usage: bench/run.sh [--runs N]
#
# Requires: hyperfine, jq, npm, yarn, pnpm, and a built `aube` binary
# (run `cargo build` first or set AUBE_BIN=/path/to/aube).

set -euo pipefail

# Serialize benchmark runs across this machine so concurrent invocations
# (other agents, worktrees, terminals) don't fight for disk/CPU and skew
# the numbers. Re-exec under flock if we haven't already taken the lock.
BENCH_LOCK=${BENCH_LOCK:-/tmp/aube-bench.lock}
if [[ -z "${AUBE_BENCH_LOCKED:-}" ]]; then
	if ! command -v flock >/dev/null 2>&1; then
		echo "bench/run.sh requires flock (brew install flock)" >&2
		exit 1
	fi
	export AUBE_BENCH_LOCKED=1
	exec flock "$BENCH_LOCK" "$0" "$@"
fi

RUNS=3
while [[ $# -gt 0 ]]; do
	case $1 in
	--runs)
		RUNS=$2
		shift 2
		;;
	*)
		echo "unknown arg: $1" >&2
		exit 1
		;;
	esac
done

REPO_ROOT=$(cd "$(dirname "$0")/.." && pwd)
FIXTURE="$REPO_ROOT/bench/fixture"
WORK_ROOT=$(mktemp -d -t aube-bench-XXXXXX)
RESULTS_DIR="$REPO_ROOT/bench"
RAW_DIR="$WORK_ROOT/raw"
mkdir -p "$RAW_DIR"

AUBE_BIN=${AUBE_BIN:-$REPO_ROOT/target/debug/aube}
if [[ ! -x "$AUBE_BIN" ]]; then
	echo "aube binary not found at $AUBE_BIN — run 'cargo build' first" >&2
	exit 1
fi

for tool in hyperfine jq npm yarn pnpm; do
	if ! command -v "$tool" >/dev/null 2>&1; then
		echo "missing required tool: $tool" >&2
		exit 1
	fi
done

cleanup() { rm -rf "$WORK_ROOT"; }
trap cleanup EXIT

# Scenarios: name, has_cache, has_lockfile, has_node_modules, action
# action is either "install" or "update".
SCENARIOS=(
	"clean              0 0 0 install"
	"cache              1 0 0 install"
	"lockfile           0 1 0 install"
	"node_modules       0 0 1 install"
	"cache_lockfile_nm  1 1 1 install"
	"cache_lockfile     1 1 0 install"
	"cache_nm           1 0 1 install"
	"lockfile_nm        0 1 1 install"
	"update             1 1 1 update"
)

# Per-PM tuple: id, display, lockfile name. Display is unused at the
# moment but kept so the table is easy to scan.
PMS=(
	"npm  npm  package-lock.json"
	"yarn yarn yarn.lock"
	"pnpm pnpm pnpm-lock.yaml"
	"aube aube pnpm-lock.yaml"
)

cmd_for() {
	local pm=$1 action=$2
	case "$pm:$action" in
	npm:install) echo "npm  install --no-audit --no-fund --prefer-offline=false --loglevel=error" ;;
	npm:update) echo "npm  update  --no-audit --no-fund --loglevel=error" ;;
	yarn:install) echo "yarn install --silent --non-interactive" ;;
	yarn:update) echo "yarn upgrade --silent --non-interactive" ;;
	pnpm:install) echo "pnpm install --silent --config.confirmModulesPurge=false" ;;
	pnpm:update) echo "pnpm update  --silent" ;;
	aube:install) echo "$AUBE_BIN install" ;;
	aube:update) echo "$AUBE_BIN update" ;;
	esac
}

# Per-PM cache directory layout under a per-run HOME so each scenario can
# control whether the cache is warm or cold.
warm_cache() {
	local pm=$1 dir=$2
	pushd "$dir" >/dev/null
	HOME="$dir/home" XDG_CACHE_HOME="$dir/home/.cache" \
		bash -c "$(cmd_for "$pm" install)" >/dev/null 2>&1 || true
	popd >/dev/null
}

bench_one() {
	local pm=$1 lockname=$2 sname=$3 has_cache=$4 has_lock=$5 has_nm=$6 action=$7
	local dir="$WORK_ROOT/$pm/$sname"
	mkdir -p "$dir/home/.cache"
	cp -R "$FIXTURE"/. "$dir/"

	# Always start from a known state for the cache.
	if [[ $has_cache == 1 ]]; then
		warm_cache "$pm" "$dir"
	fi

	# Pre-generate a lockfile if the scenario needs one (and the cache hasn't
	# already produced one).
	if [[ $has_lock == 1 && ! -f "$dir/$lockname" ]]; then
		pushd "$dir" >/dev/null
		HOME="$dir/home" XDG_CACHE_HOME="$dir/home/.cache" \
			bash -c "$(cmd_for "$pm" install)" >/dev/null 2>&1 || true
		popd >/dev/null
	fi

	# Snapshot the per-PM cache so we can restore it between hyperfine runs
	# when the scenario calls for a warm cache.
	local cache_snapshot="$dir/cache.tar"
	if [[ $has_cache == 1 ]]; then
		tar -C "$dir/home" -cf "$cache_snapshot" . 2>/dev/null || true
	fi

	# Snapshot the lockfile / node_modules so they can be restored / cleared
	# for each timing run.
	local lock_snapshot="$dir/lock.snapshot"
	if [[ -f "$dir/$lockname" ]]; then cp "$dir/$lockname" "$lock_snapshot"; fi
	local nm_snapshot="$dir/nm.tar"
	if [[ $has_nm == 1 ]]; then
		pushd "$dir" >/dev/null
		HOME="$dir/home" XDG_CACHE_HOME="$dir/home/.cache" \
			bash -c "$(cmd_for "$pm" install)" >/dev/null 2>&1 || true
		if [[ -d node_modules ]]; then tar -cf "$nm_snapshot" node_modules; fi
		popd >/dev/null
	fi

	local prepare="rm -rf '$dir/node_modules' '$dir/$lockname' '$dir/home'; mkdir -p '$dir/home/.cache'"
	if [[ $has_cache == 1 ]]; then
		prepare="$prepare; tar -C '$dir/home' -xf '$cache_snapshot'"
	fi
	if [[ $has_lock == 1 && -f "$lock_snapshot" ]]; then
		prepare="$prepare; cp '$lock_snapshot' '$dir/$lockname'"
	fi
	if [[ $has_nm == 1 && -f "$nm_snapshot" ]]; then
		prepare="$prepare; tar -C '$dir' -xf '$nm_snapshot'"
	fi

	local install_cmd
	install_cmd="cd '$dir' && HOME='$dir/home' XDG_CACHE_HOME='$dir/home/.cache' $(cmd_for "$pm" "$action")"

	local out="$RAW_DIR/${pm}_${sname}.json"
	if hyperfine --warmup 0 --runs "$RUNS" --prepare "$prepare" \
		--export-json "$out" "$install_cmd" \
		>"$dir/hyperfine.log" 2>&1; then
		jq -r '.results[0].mean' "$out"
	else
		echo "FAIL: $pm/$sname — see $dir/hyperfine.log" >&2
		echo "null"
	fi
}

echo "Benchmarking with $RUNS runs per scenario..." >&2

declare -A RESULTS
for scenario in "${SCENARIOS[@]}"; do
	read -r sname has_cache has_lock has_nm action <<<"$scenario"
	for pm_entry in "${PMS[@]}"; do
		read -r pm _ lockname <<<"$pm_entry"
		echo "  $pm / $sname..." >&2
		mean=$(bench_one "$pm" "$lockname" "$sname" "$has_cache" "$has_lock" "$has_nm" "$action")
		RESULTS["${pm}_${sname}"]=$mean
	done
done

# Build JSON output
{
	echo "{"
	echo "  \"runs\": $RUNS,"
	echo "  \"results\": {"
	first=1
	for scenario in "${SCENARIOS[@]}"; do
		read -r sname _ _ _ _ <<<"$scenario"
		[[ $first == 1 ]] || echo ","
		first=0
		printf '    "%s": {' "$sname"
		pm_first=1
		for pm_entry in "${PMS[@]}"; do
			read -r pm _ _ <<<"$pm_entry"
			val="${RESULTS[${pm}_${sname}]:-null}"
			[[ $pm_first == 1 ]] || printf ", "
			pm_first=0
			printf '"%s": %s' "$pm" "$val"
		done
		printf "}"
	done
	echo
	echo "  }"
	echo "}"
} >"$RESULTS_DIR/results.json"

# Build markdown table
fmt() {
	local v=$1
	if [[ "$v" == "null" || -z "$v" ]]; then
		echo "—"
		return
	fi
	awk -v v="$v" 'BEGIN { if (v < 1) printf "%dms", v*1000; else printf "%.2fs", v }'
}

{
	echo "| scenario | npm | yarn | pnpm | aube |"
	echo "| --- | --- | --- | --- | --- |"
	for scenario in "${SCENARIOS[@]}"; do
		read -r sname _ _ _ _ <<<"$scenario"
		label=$(echo "$sname" | tr '_' ' ')
		printf "| %s | %s | %s | %s | %s |\n" \
			"$label" \
			"$(fmt "${RESULTS[npm_$sname]:-null}")" \
			"$(fmt "${RESULTS[yarn_$sname]:-null}")" \
			"$(fmt "${RESULTS[pnpm_$sname]:-null}")" \
			"$(fmt "${RESULTS[aube_$sname]:-null}")"
	done
} >"$RESULTS_DIR/results.md"

echo "wrote $RESULTS_DIR/results.json and $RESULTS_DIR/results.md" >&2
cat "$RESULTS_DIR/results.md" >&2
