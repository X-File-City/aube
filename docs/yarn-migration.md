# Yarn migration

aube can install directly from Yarn classic lockfiles. You do not need to
delete `yarn.lock`, remove `node_modules`, or translate dependency ranges before
trying aube.

## Yarn classic

```sh
aube install
```

aube reads and updates Yarn v1 `yarn.lock` in place (no surprise
`aube-lock.yaml` appears alongside it) and installs packages into
`node_modules/.aube/`.

Commit the updated `yarn.lock` so Yarn classic users and aube users see the
same resolved versions. You do not need `aube import` for a normal rollout;
`aube install` keeps `yarn.lock` as the shared source of truth.

Use `aube import` only if the team intentionally wants to convert the project
to `aube-lock.yaml`. After import succeeds, remove `yarn.lock` so future
installs keep writing `aube-lock.yaml`.

## Yarn Berry and PnP

Modern Yarn Berry PnP projects need a layout migration because aube writes
`node_modules`, not `.pnp.cjs`. Move those projects to a `node_modules` linker
before using aube as the install command.

## Differences from Yarn classic

- aube keeps package files in a global content-addressable store.
- aube uses isolated symlinks instead of a hoisted flat tree by default.
- Workspace package discovery follows `aube-workspace.yaml` (or
  `pnpm-workspace.yaml` when the project already has one).
- Dependency lifecycle script approval follows the pnpm v11 allowlist model.

## Rollout checklist

- Run `aube install`.
- Commit the updated `yarn.lock` so Yarn and aube users both see the same
  resolved versions.
- Update one CI job from `yarn install --frozen-lockfile` to `aube ci` or
  `aube install --frozen-lockfile`.
- Run the same test scripts you run after Yarn installs.
- Convert to `aube-lock.yaml` later only if the team chooses to standardize on
  aube's lockfile.

Reference: [Yarn classic install](https://classic.yarnpkg.com/lang/en/docs/cli/install/)
