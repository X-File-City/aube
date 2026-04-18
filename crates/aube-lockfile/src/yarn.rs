//! Parser for yarn.lock v1 (classic yarn format).
//!
//! The format is line-based, similar to YAML but not quite:
//!
//! ```text
//! # comment
//! "@scope/pkg@^1.0.0", "@scope/pkg@^1.1.0":
//!   version "1.2.3"
//!   resolved "https://..."
//!   integrity sha512-...
//!   dependencies:
//!     other-pkg "^2.0.0"
//! ```
//!
//! Top-level blocks are keyed by one or more comma-separated specifiers
//! (`name@range`). The body is indented 2 spaces. Nested sections like
//! `dependencies:` add another 2 spaces of indentation.
//!
//! yarn.lock does not distinguish direct deps from transitive ones, so we
//! cross-reference specifiers against the project's package.json to populate
//! `importers["."]`.
//!
//! yarn berry (v2+) uses a proper YAML format with a `__metadata:` header
//! and is rejected with a clear error.

use crate::{DepType, DirectDep, Error, LockedPackage, LockfileGraph};
use std::collections::BTreeMap;
use std::path::Path;

/// Parse a yarn.lock v1 file into a LockfileGraph.
///
/// The manifest is needed to identify direct dependencies (yarn.lock has
/// no notion of direct vs transitive).
pub fn parse(path: &Path, manifest: &aube_manifest::PackageJson) -> Result<LockfileGraph, Error> {
    let content = std::fs::read_to_string(path).map_err(|e| Error::Io(path.to_path_buf(), e))?;

    // Berry (yarn 2+) lockfiles start with a `__metadata:` key.
    if content
        .lines()
        .any(|l| l.trim_start().starts_with("__metadata:"))
    {
        return Err(Error::Parse(
            path.to_path_buf(),
            "yarn berry (v2+) lockfiles are not supported — run `yarn set version classic` first, or use package-lock.json".to_string(),
        ));
    }

    let blocks = tokenize_blocks(&content).map_err(|e| Error::Parse(path.to_path_buf(), e))?;

    // spec_to_dep_path maps each specifier (e.g. "is-odd@^3.0.0") to its
    // resolved dep_path ("is-odd@3.0.1"). Used for resolving direct deps
    // from package.json ranges and transitive dep references.
    let mut spec_to_dep_path: BTreeMap<String, String> = BTreeMap::new();
    let mut packages: BTreeMap<String, LockedPackage> = BTreeMap::new();

    for block in &blocks {
        let version = block
            .fields
            .get("version")
            .ok_or_else(|| {
                Error::Parse(
                    path.to_path_buf(),
                    format!("yarn.lock block {:?} has no version", block.specs),
                )
            })?
            .clone();

        // All specs in the key map to the same resolved package.
        // Extract the package name from the first spec.
        let name = parse_spec_name(&block.specs[0]).ok_or_else(|| {
            Error::Parse(
                path.to_path_buf(),
                format!(
                    "could not parse package name from yarn.lock spec '{}'",
                    block.specs[0]
                ),
            )
        })?;

        let dep_path = format!("{name}@{version}");

        for spec in &block.specs {
            spec_to_dep_path.insert(spec.clone(), dep_path.clone());
        }

        // Only insert the first occurrence; dedup is fine because yarn.lock
        // already guarantees unique (name, version) entries.
        if !packages.contains_key(&dep_path) {
            packages.insert(
                dep_path.clone(),
                LockedPackage {
                    name: name.clone(),
                    version: version.clone(),
                    integrity: block.fields.get("integrity").cloned(),
                    // Store raw "name@range" pairs for now; resolve below.
                    dependencies: block
                        .dependencies
                        .iter()
                        .map(|(n, r)| (n.clone(), format!("{n}@{r}")))
                        .collect(),
                    dep_path,
                    ..Default::default()
                },
            );
        }
    }

    // Second pass: resolve transitive dep references to dep_paths.
    let mut resolved: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    for (dep_path, pkg) in &packages {
        let mut deps: BTreeMap<String, String> = BTreeMap::new();
        for (name, raw_spec) in &pkg.dependencies {
            if let Some(resolved_path) = spec_to_dep_path.get(raw_spec) {
                deps.insert(name.clone(), resolved_path.clone());
            }
        }
        resolved.insert(dep_path.clone(), deps);
    }
    for (dep_path, deps) in resolved {
        if let Some(pkg) = packages.get_mut(&dep_path) {
            pkg.dependencies = deps;
        }
    }

    // Build direct deps from the manifest, cross-referencing against spec_to_dep_path.
    let mut direct: Vec<DirectDep> = Vec::new();
    let push_direct = |name: &str, range: &str, dep_type: DepType, direct: &mut Vec<DirectDep>| {
        let spec = format!("{name}@{range}");
        if let Some(dep_path) = spec_to_dep_path.get(&spec) {
            direct.push(DirectDep {
                name: name.to_string(),
                dep_path: dep_path.clone(),
                dep_type,
                specifier: None,
            });
        }
    };
    for (name, range) in &manifest.dependencies {
        push_direct(name, range, DepType::Production, &mut direct);
    }
    for (name, range) in &manifest.dev_dependencies {
        push_direct(name, range, DepType::Dev, &mut direct);
    }
    for (name, range) in &manifest.optional_dependencies {
        push_direct(name, range, DepType::Optional, &mut direct);
    }

    let mut importers = BTreeMap::new();
    importers.insert(".".to_string(), direct);

    Ok(LockfileGraph {
        importers,
        packages,
        ..Default::default()
    })
}

#[derive(Debug)]
struct Block {
    /// Specifier keys: each is a "name@range" string.
    specs: Vec<String>,
    /// Flat scalar fields (version, resolved, integrity, etc.)
    fields: BTreeMap<String, String>,
    /// Nested dependencies section: name -> range
    dependencies: BTreeMap<String, String>,
}

/// Tokenize the yarn.lock body into blocks. This is a line-based parser that
/// recognizes:
/// - Comments (`# …`) and blank lines
/// - Header lines ending in `:` (block keys)
/// - Fields indented with 2 spaces
/// - A special nested `dependencies:` section indented with 4 spaces
fn tokenize_blocks(content: &str) -> Result<Vec<Block>, String> {
    let mut blocks: Vec<Block> = Vec::new();
    let mut current: Option<Block> = None;
    let mut in_deps = false;

    for (lineno, raw_line) in content.lines().enumerate() {
        let line_num = lineno + 1;

        // Strip trailing whitespace but preserve leading indentation
        let line = raw_line.trim_end();
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }

        let indent = line.len() - line.trim_start().len();

        // Top-level: block header (one or more comma-separated specs ending in `:`)
        if indent == 0 {
            if let Some(b) = current.take() {
                blocks.push(b);
            }
            in_deps = false;

            let header = line.trim_end_matches(':').trim();
            if !line.ends_with(':') {
                return Err(format!(
                    "line {line_num}: expected block header ending in ':', got '{line}'"
                ));
            }

            let specs = parse_header_specs(header).map_err(|e| format!("line {line_num}: {e}"))?;
            current = Some(Block {
                specs,
                fields: BTreeMap::new(),
                dependencies: BTreeMap::new(),
            });
            continue;
        }

        let block = current.as_mut().ok_or_else(|| {
            format!("line {line_num}: unexpected indented content before any block header")
        })?;

        if indent == 2 {
            in_deps = false;
            let body = line.trim_start();

            // Check for nested section markers (e.g. `dependencies:`)
            if body.ends_with(':') {
                let section = body.trim_end_matches(':').trim();
                if section == "dependencies"
                    || section == "optionalDependencies"
                    || section == "peerDependencies"
                {
                    // Only track `dependencies:` for our resolution graph; ignore others.
                    in_deps = section == "dependencies";
                    continue;
                }
                // Unknown 2-space section header — ignore.
                continue;
            }

            let (key, value) = split_key_value(body)
                .ok_or_else(|| format!("line {line_num}: could not parse '{body}'"))?;
            block.fields.insert(key, value);
        } else if indent >= 4 && in_deps {
            let body = line.trim_start();
            let (name, range) = split_key_value(body)
                .ok_or_else(|| format!("line {line_num}: could not parse dep '{body}'"))?;
            block.dependencies.insert(name, range);
        }
        // Deeper indents outside `dependencies:` are ignored.
    }

    if let Some(b) = current.take() {
        blocks.push(b);
    }

    Ok(blocks)
}

/// Parse a header like `"foo@^1.0.0", "foo@^1.1.0"` or `foo@^1.0.0` into specs.
fn parse_header_specs(header: &str) -> Result<Vec<String>, String> {
    let mut specs = Vec::new();
    for raw in header.split(',') {
        let s = raw.trim();
        let unquoted = if (s.starts_with('"') && s.ends_with('"') && s.len() >= 2)
            || (s.starts_with('\'') && s.ends_with('\'') && s.len() >= 2)
        {
            &s[1..s.len() - 1]
        } else {
            s
        };
        if unquoted.is_empty() {
            return Err(format!("empty spec in header '{header}'"));
        }
        specs.push(unquoted.to_string());
    }
    if specs.is_empty() {
        return Err(format!("no specs parsed from header '{header}'"));
    }
    Ok(specs)
}

/// Split a body line like `version "1.2.3"` or `foo "^1.0.0"` into (key, value).
/// Values may be quoted or unquoted.
fn split_key_value(line: &str) -> Option<(String, String)> {
    let (key, rest) = line.split_once(char::is_whitespace)?;
    let value = rest.trim();
    let unquoted = if (value.starts_with('"') && value.ends_with('"') && value.len() >= 2)
        || (value.starts_with('\'') && value.ends_with('\'') && value.len() >= 2)
    {
        &value[1..value.len() - 1]
    } else {
        value
    };
    Some((key.to_string(), unquoted.to_string()))
}

/// Extract the package name from a spec like `foo@^1.0.0` or `@scope/pkg@^1.0.0`.
fn parse_spec_name(spec: &str) -> Option<String> {
    if let Some(rest) = spec.strip_prefix('@') {
        // Scoped package: find the '@' that comes after the '/'
        let slash = rest.find('/')?;
        let after_slash = &rest[slash + 1..];
        let at = after_slash.find('@')?;
        Some(format!("@{}", &rest[..slash + 1 + at]))
    } else {
        let at = spec.find('@')?;
        Some(spec[..at].to_string())
    }
}

// ---------------------------------------------------------------------------
// Writer: flat LockfileGraph → yarn.lock v1
// ---------------------------------------------------------------------------

/// Serialize a [`LockfileGraph`] as a yarn v1 lockfile.
///
/// yarn v1 is flat — unlike npm or bun, there's no nested install
/// path. Every `(name, version)` pair gets exactly one block whose
/// header is a comma-separated list of every spec that resolves to
/// it. We always emit the exact `"name@version"` spec (so transitive
/// deps emitted as `bar "2.5.0"` round-trip), and for direct root
/// deps we *also* emit the manifest range spec (e.g. `"bar@^2.0.0"`)
/// so `yarn install` and `aube install` — both of which look up
/// manifest ranges against the block headers — find the entry.
///
/// Transitive deps that arrive through a semver *range* (e.g. `foo`
/// depends on `bar "^2.0.0"`) are still technically lossy: the
/// original range isn't preserved, so if the parent's resolved
/// `bar` version differs from what the lockfile records, reparse
/// will miss. In practice the writer only runs on a graph the
/// resolver just produced, so the resolved versions match the
/// transitive dep keys exactly and reparse finds them.
///
/// Peer-contextualized variants collapse to a single `name@version`
/// entry (yarn v1's data model has no peer context). `resolved` URLs
/// are omitted for the same reason as the npm writer: we don't
/// persist the origin URL. yarn tolerates missing `resolved`.
pub fn write(
    path: &Path,
    graph: &LockfileGraph,
    manifest: &aube_manifest::PackageJson,
) -> Result<(), Error> {
    // Collapse peer-context variants: one entry per canonical (name, version).
    let mut canonical: BTreeMap<String, &LockedPackage> = BTreeMap::new();
    for pkg in graph.packages.values() {
        canonical
            .entry(format!("{}@{}", pkg.name, pkg.version))
            .or_insert(pkg);
    }

    // Collect every manifest spec that points at a canonical
    // `(name, version)`. Keyed by canonical key; values are the
    // extra range-form spec strings to emit alongside the exact
    // `"name@version"` one in the block header.
    let mut extra_specs: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let root_importer_specs = manifest
        .dependencies
        .iter()
        .chain(manifest.dev_dependencies.iter())
        .chain(manifest.optional_dependencies.iter())
        .chain(manifest.peer_dependencies.iter());
    for dep in graph.importers.get(".").into_iter().flatten() {
        let canonical_key = crate::npm::canonical_key_from_dep_path(&dep.dep_path);
        if !canonical.contains_key(&canonical_key) {
            continue;
        }
        // Look up the range the manifest currently uses for this dep.
        let range = root_importer_specs
            .clone()
            .find(|(n, _)| n.as_str() == dep.name.as_str())
            .map(|(_, r)| r.clone());
        if let Some(range) = range {
            let spec = format!("{}@{range}", dep.name);
            if spec != canonical_key {
                extra_specs.entry(canonical_key).or_default().push(spec);
            }
        }
    }

    let mut out = String::new();
    out.push_str("# THIS IS AN AUTOGENERATED FILE. DO NOT EDIT THIS FILE DIRECTLY.\n");
    out.push_str("# yarn lockfile v1\n\n\n");

    for (canonical_key, pkg) in &canonical {
        // Header: `"name@version"[, "name@range"]*:` — always start
        // with the exact spec so transitive reparse works, then
        // append any manifest range specs pointing at this entry.
        out.push('"');
        out.push_str(canonical_key);
        out.push('"');
        if let Some(extras) = extra_specs.get(canonical_key) {
            for spec in extras {
                out.push_str(", \"");
                out.push_str(spec);
                out.push('"');
            }
        }
        out.push_str(":\n");

        // `  version "..."`
        out.push_str("  version \"");
        out.push_str(&pkg.version);
        out.push_str("\"\n");

        if let Some(integ) = &pkg.integrity {
            out.push_str("  integrity ");
            out.push_str(integ);
            out.push('\n');
        }

        // `  dependencies:` block — resolved to canonical versions so
        // yarn's transitive lookup (`<name>@<version>`) finds the
        // right block key above. Ranges aren't preserved because we
        // never recorded them; writing the exact version is a
        // harmless overconstraint.
        let nonempty_deps: BTreeMap<&str, &str> = pkg
            .dependencies
            .iter()
            .filter_map(|(n, v)| {
                let key = crate::npm::child_canonical_key(n, v);
                if !canonical.contains_key(&key) {
                    return None;
                }
                Some((n.as_str(), crate::npm::dep_value_as_version(n, v)))
            })
            .collect();
        if !nonempty_deps.is_empty() {
            out.push_str("  dependencies:\n");
            for (dep_name, dep_version) in nonempty_deps {
                out.push_str("    ");
                out.push_str(dep_name);
                out.push_str(" \"");
                out.push_str(dep_version);
                out.push_str("\"\n");
            }
        }

        out.push('\n');
    }

    std::fs::write(path, out).map_err(|e| Error::Io(path.to_path_buf(), e))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_manifest(deps: &[(&str, &str)], dev: &[(&str, &str)]) -> aube_manifest::PackageJson {
        aube_manifest::PackageJson {
            name: Some("test".to_string()),
            version: Some("1.0.0".to_string()),
            dependencies: deps
                .iter()
                .map(|(n, r)| (n.to_string(), r.to_string()))
                .collect(),
            dev_dependencies: dev
                .iter()
                .map(|(n, r)| (n.to_string(), r.to_string()))
                .collect(),
            peer_dependencies: Default::default(),
            optional_dependencies: Default::default(),
            update_config: None,
            scripts: Default::default(),
            engines: Default::default(),
            workspaces: None,
            bundled_dependencies: None,
            extra: Default::default(),
        }
    }

    #[test]
    fn test_parse_spec_name() {
        assert_eq!(parse_spec_name("foo@^1.0.0"), Some("foo".to_string()));
        assert_eq!(parse_spec_name("foo@1.2.3"), Some("foo".to_string()));
        assert_eq!(
            parse_spec_name("@scope/pkg@^1.0.0"),
            Some("@scope/pkg".to_string())
        );
        assert_eq!(parse_spec_name("foo"), None);
    }

    #[test]
    fn test_parse_simple() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let content = r#"# yarn lockfile v1

foo@^1.0.0:
  version "1.2.3"
  resolved "https://example.com/foo-1.2.3.tgz"
  integrity sha512-aaa
  dependencies:
    bar "^2.0.0"

bar@^2.0.0:
  version "2.5.0"
  resolved "https://example.com/bar-2.5.0.tgz"
  integrity sha512-bbb
"#;
        std::fs::write(tmp.path(), content).unwrap();
        let manifest = make_manifest(&[("foo", "^1.0.0")], &[]);
        let graph = parse(tmp.path(), &manifest).unwrap();

        assert_eq!(graph.packages.len(), 2);
        assert!(graph.packages.contains_key("foo@1.2.3"));
        assert!(graph.packages.contains_key("bar@2.5.0"));

        let foo = &graph.packages["foo@1.2.3"];
        assert_eq!(foo.integrity.as_deref(), Some("sha512-aaa"));
        assert_eq!(
            foo.dependencies.get("bar").map(String::as_str),
            Some("bar@2.5.0")
        );

        let root = graph.importers.get(".").unwrap();
        assert_eq!(root.len(), 1);
        assert_eq!(root[0].name, "foo");
        assert_eq!(root[0].dep_path, "foo@1.2.3");
    }

    #[test]
    fn test_parse_scoped_and_multi_spec() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let content = r#"# yarn lockfile v1

"@scope/pkg@^1.0.0", "@scope/pkg@^1.1.0":
  version "1.1.0"
  integrity sha512-zzz
"#;
        std::fs::write(tmp.path(), content).unwrap();
        let manifest = make_manifest(&[("@scope/pkg", "^1.0.0")], &[]);
        let graph = parse(tmp.path(), &manifest).unwrap();

        assert!(graph.packages.contains_key("@scope/pkg@1.1.0"));
        let root = graph.importers.get(".").unwrap();
        assert_eq!(root[0].name, "@scope/pkg");
        assert_eq!(root[0].dep_path, "@scope/pkg@1.1.0");
    }

    #[test]
    fn test_reject_berry() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let content = "__metadata:\n  version: 6\n";
        std::fs::write(tmp.path(), content).unwrap();
        let manifest = make_manifest(&[], &[]);
        let err = parse(tmp.path(), &manifest).unwrap_err();
        assert!(matches!(err, Error::Parse(_, msg) if msg.contains("berry")));
    }

    /// Parse → write → parse should preserve package set,
    /// versions, integrity, and the resolved transitive graph. If
    /// the writer emits malformed block headers or forgets to
    /// requote, round-trip breaks here.
    #[test]
    fn test_write_roundtrip() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let content = r#"# yarn lockfile v1

foo@^1.0.0:
  version "1.2.3"
  integrity sha512-foo
  dependencies:
    bar "^2.0.0"

bar@^2.0.0:
  version "2.5.0"
  integrity sha512-bar
"#;
        std::fs::write(tmp.path(), content).unwrap();
        let manifest = make_manifest(&[("foo", "^1.0.0")], &[]);
        let graph = parse(tmp.path(), &manifest).unwrap();

        let out = tempfile::NamedTempFile::new().unwrap();
        write(out.path(), &graph, &manifest).unwrap();

        // Re-parse the output. The manifest is the same — direct-dep
        // resolution requires a spec key of `foo@^1.0.0`, but the
        // writer emits `"foo@1.2.3"`. So direct-dep lookup will
        // miss; we only assert the packages/transitives round-trip.
        let reparsed_manifest = make_manifest(&[], &[]);
        let reparsed = parse(out.path(), &reparsed_manifest).unwrap();

        assert!(reparsed.packages.contains_key("foo@1.2.3"));
        assert!(reparsed.packages.contains_key("bar@2.5.0"));
        assert_eq!(
            reparsed.packages["foo@1.2.3"].integrity.as_deref(),
            Some("sha512-foo")
        );
        // foo's transitive dep on bar must still resolve: the writer
        // emits `bar "2.5.0"` under foo's dependencies, and reparse
        // finds the block keyed `"bar@2.5.0"` via spec_to_dep_path.
        assert_eq!(
            reparsed.packages["foo@1.2.3"]
                .dependencies
                .get("bar")
                .map(String::as_str),
            Some("bar@2.5.0")
        );
    }

    #[test]
    fn test_dev_dep_classification() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let content = r#"foo@^1.0.0:
  version "1.0.0"
"#;
        std::fs::write(tmp.path(), content).unwrap();
        let manifest = make_manifest(&[], &[("foo", "^1.0.0")]);
        let graph = parse(tmp.path(), &manifest).unwrap();
        let root = graph.importers.get(".").unwrap();
        assert_eq!(root[0].dep_type, DepType::Dev);
    }
}
