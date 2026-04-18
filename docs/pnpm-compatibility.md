# pnpm Compatibility

aube targets pnpm v11 compatibility for package-manager workflows. The CLI
surface, the `node_modules` layout, and today's YAML file shapes are familiar
to pnpm users, but aube owns its own on-disk files and directories so the two
tools can coexist in the same machine and project without stepping on each
other.

## Parity status

aube targets feature parity with pnpm for package-manager workflows, but not
every pnpm command is part of that promise. Runtime management and a few
registry/account-management commands are intentionally left out or kept as
compatibility stubs.

For a compact gap list, see the
[README compatibility notes](https://github.com/endevco/aube#commands-you-may-recognize).
For command and flag details, see the [CLI reference](/cli/).

## What's the same

- **`node_modules` layout.** aube produces the same isolated symlink layout
  pnpm uses with `node-linker=isolated` (the default): top-level entries are
  symlinks into `node_modules/.aube/<dep_path>/node_modules/<name>`. The only
  difference from pnpm is the directory name — `.aube/` instead of `.pnpm/`.
- **YAML compatibility.** `aube-lock.yaml` and `aube-workspace.yaml` use
  pnpm-compatible YAML shapes today. aube reads and writes both the
  `aube-*` and `pnpm-*` filenames, and preserves whichever file the
  project already has on disk — new projects get the `aube-*` names as
  the default. The on-disk shapes may diverge after the parity phase.
- **Build approvals.** Dependency lifecycle script approval follows pnpm v11's
  allowlist model. Use explicit policy fields in `package.json` or
  `aube-workspace.yaml` to opt in.
- **CLI surface.** Commands, flags, and exit codes mirror pnpm's. See the
  [CLI reference](/cli/).

## What's different

- **Aube-owned global store.** aube's content-addressable store lives at
  `~/.aube-store/v1/files/`, not `~/.pnpm-store/`. Tarballs are re-downloaded
  on first use; subsequent installs hit the aube store. This is intentional:
  sharing the pnpm store means sharing its layout assumptions too, and we'd
  rather own our state cleanly.
- **Default YAML filenames for new projects.** When a project has no
  lockfile yet, aube creates `aube-lock.yaml`. If the project already has
  `pnpm-lock.yaml` or any other supported lockfile (`package-lock.json`,
  `npm-shrinkwrap.json`, `yarn.lock`, `bun.lock`), aube reads and writes
  that file in place — it never migrates them to the aube-named variants.
  Workspace YAML is almost always read-only for aube: install / add /
  remove / update never touch it. The one exception is `aube approve-builds`,
  which writes approvals into `pnpm-workspace.yaml`'s
  `onlyBuiltDependencies` (matching pnpm v10+), creating the file if
  missing. aube does not generate an `aube-workspace.yaml` for you —
  create it yourself if you want the aube-named variant.
- **Virtual-store directory.** The per-project virtual store is
  `node_modules/.aube/`, not `node_modules/.pnpm/`. If a project already has
  a pnpm-built `node_modules`, aube leaves it alone and installs alongside
  — the two virtual stores live side by side.
- **Speed.** See the [benchmarks](/benchmarks).
- **`aube test`.** Equivalent to `pnpm install-test`: aube auto-installs
  before running `test`, so the two-step pnpm workflow becomes one command.

## Migrating

Run `aube install` in any pnpm project. aube reads the existing
`pnpm-lock.yaml` and `pnpm-workspace.yaml`, writes any lockfile updates
back to the same `pnpm-lock.yaml`, and installs into `node_modules/.aube/`.
No new files are created alongside the ones you already have — aube
treats the existing YAML as canonical. Projects that start without a
lockfile or workspace file get the `aube-*` variants by default; if you
prefer those names for an existing pnpm project, rename the files
yourself and aube will preserve them going forward.

For the practical command-by-command migration path, see
[For pnpm users](/pnpm-users) and [Migrating projects](/migration).
