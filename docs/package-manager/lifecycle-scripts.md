# Lifecycle scripts

Packages can define lifecycle scripts such as `preinstall`, `install`,
`postinstall`, and `prepare`. aube treats root scripts and dependency scripts
differently.

## Root scripts

Root package scripts run during install unless scripts are ignored:

```sh
aube install --ignore-scripts
```

## Dependency scripts

Dependency lifecycle scripts follow the pnpm v11 build approval model. Packages
must be explicitly allowlisted before their install-time scripts run.

```sh
aube ignored-builds
aube approve-builds
aube rebuild
```

Supported policy fields — aube reads all of these at install time:

In `pnpm-workspace.yaml` (pnpm v10+ home for these settings, and what
`aube approve-builds` writes to):

```yaml
onlyBuiltDependencies:
  - sharp
neverBuiltDependencies:
  - untrusted-package
allowBuilds:
  esbuild: true
```

In `package.json` (pnpm v9 / legacy — still honored as a read source):

```json
{
  "pnpm": {
    "allowBuilds": {
      "esbuild": true
    },
    "onlyBuiltDependencies": ["sharp"],
    "neverBuiltDependencies": ["untrusted-package"]
  }
}
```

Deny rules win over allow rules. Workspace-yaml entries and
`package.json` entries merge; you don't have to migrate a legacy
`pnpm.allowBuilds` to start using `aube approve-builds`.

## Git dependencies

Git dependencies with `prepare` scripts get a nested install in the clone
before aube snapshots the package. The final linked package uses the packed
result, not the raw checkout.

## Side effects cache

Allowlisted dependency builds can cache their post-build package tree and reuse
it on future installs with the same input hash.

## Bun comparison

Bun also treats dependency scripts as a security boundary and uses an allowlist
model through `trustedDependencies`. aube uses pnpm-compatible policy fields so
the same manifest can stay close to pnpm's model.
