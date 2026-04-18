# Run scripts and binaries

aube follows npm and pnpm script conventions while adding an install-state
check before script execution.

## Scripts

```sh
aube run build
aube test
aube start
aube stop
aube restart
```

Before running a script, aube checks `.aube/.state/install-state.json`. If the
manifest or lockfile changed, aube installs first. Use `--no-install` when you
want to skip that check.

```sh
aube run --no-install build
aube test --no-install
```

Use `--if-present` for optional scripts:

```sh
aube run --if-present lint
```

## Local binaries

```sh
aube exec vitest
aube exec tsc -- --noEmit
```

`exec` runs a binary from the project context with `node_modules/.bin` on
`PATH`.

## One-off binaries

```sh
aube dlx cowsay hi
aube dlx -p create-vite create-vite my-app
```

`dlx` installs into a throwaway project and runs the requested binary.

## Workspace runs

```sh
aube -r run build
aube -F '@scope/*' run test
aube -F './packages/api' exec tsc -- --noEmit
aube -F 'api...' run build
```

`-r` is sugar for `--filter=*`. Filters support exact names, globs, paths,
dependency/dependent graph selectors, git-ref selectors, and exclusions.
