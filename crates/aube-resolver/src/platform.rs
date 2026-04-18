//! Platform filtering for `os` / `cpu` / `libc` package metadata.
//!
//! npm-style packages can declare the platforms they support via the
//! `os`, `cpu`, and `libc` arrays in `package.json`. Each entry is
//! either a positive match (`"linux"`, `"x64"`, `"glibc"`) or a
//! negation prefixed with `!` (`"!win32"`). pnpm's rule:
//!
//!   - empty array        → unconstrained (installable everywhere)
//!   - any negation hit   → reject
//!   - at least one pos   → accept only if one positive matches
//!   - negations only     → accept if no negation matched
//!
//! pnpm lets the user widen the match set beyond the host via
//! `pnpm.supportedArchitectures` — an object with `os`/`cpu`/`libc`
//! arrays, each entry either a concrete value or the literal `"current"`
//! which expands to the host triple. The package passes if ANY of the
//! (os, cpu, libc) combinations in the supported set is installable.
//!
//! This module stays intentionally small: no reading of config, no
//! serde, just the matcher and host detection. Configuration lives on
//! the `Resolver`, which calls [`is_supported`] during filtering.

/// User-declared override for the host triple used when filtering
/// optional dependencies. Missing arrays fall back to the host; the
/// literal `"current"` inside any array expands to the same host value
/// so users can write `["current", "linux"]` to keep their native
/// platform *and* also resolve optionals for Linux.
#[derive(Debug, Clone, Default)]
pub struct SupportedArchitectures {
    pub os: Vec<String>,
    pub cpu: Vec<String>,
    pub libc: Vec<String>,
}

impl SupportedArchitectures {
    /// Expand any `"current"` entries to the host triple and default
    /// empty arrays to `[host]`. The result is a non-empty list of
    /// (os, cpu, libc) combinations the caller can test against.
    fn combinations(&self) -> Vec<(String, String, String)> {
        let host = host_triple();
        let expand = |field: &[String], host_val: &str| -> Vec<String> {
            if field.is_empty() {
                return vec![host_val.to_string()];
            }
            field
                .iter()
                .map(|v| {
                    if v == "current" {
                        host_val.to_string()
                    } else {
                        v.clone()
                    }
                })
                .collect()
        };
        let os = expand(&self.os, host.0);
        let cpu = expand(&self.cpu, host.1);
        let libc = expand(&self.libc, host.2);
        let mut out = Vec::with_capacity(os.len() * cpu.len() * libc.len());
        for o in &os {
            for c in &cpu {
                for l in &libc {
                    out.push((o.clone(), c.clone(), l.clone()));
                }
            }
        }
        out
    }
}

/// Return the host's (os, cpu, libc) triple using npm's vocabulary.
/// `libc` is `"glibc"` / `"musl"` on Linux and `""` elsewhere — npm
/// only sets `libc` on Linux packages, so non-Linux hosts treat libc
/// constraints as a no-op.
pub fn host_triple() -> (&'static str, &'static str, &'static str) {
    let os = match std::env::consts::OS {
        "macos" => "darwin",
        "windows" => "win32",
        other => other,
    };
    let cpu = match std::env::consts::ARCH {
        "x86_64" => "x64",
        "x86" => "ia32",
        "aarch64" => "arm64",
        "powerpc64" => "ppc64",
        other => other,
    };
    let libc = if cfg!(target_os = "linux") {
        if cfg!(target_env = "musl") {
            "musl"
        } else {
            "glibc"
        }
    } else {
        ""
    };
    (os, cpu, libc)
}

/// Apply npm's `os`/`cpu`/`libc` rules to a single (pkg_field, host)
/// pair. An empty pkg array is unconstrained; negations reject; at
/// least one positive entry means one must match.
fn field_matches(pkg_field: &[String], host: &str) -> bool {
    if pkg_field.is_empty() {
        return true;
    }
    let mut has_positive = false;
    let mut positive_matched = false;
    for entry in pkg_field {
        if let Some(neg) = entry.strip_prefix('!') {
            if neg == host {
                return false;
            }
        } else {
            has_positive = true;
            if entry == host {
                positive_matched = true;
            }
        }
    }
    !has_positive || positive_matched
}

/// Decide whether a package is installable on any of the (os, cpu,
/// libc) combinations expanded from `supported`. The `pkg_libc` check
/// is skipped when the host libc is empty (non-Linux) — npm doesn't
/// enforce libc off Linux.
pub fn is_supported(
    pkg_os: &[String],
    pkg_cpu: &[String],
    pkg_libc: &[String],
    supported: &SupportedArchitectures,
) -> bool {
    for (os, cpu, libc) in supported.combinations() {
        if !field_matches(pkg_os, &os) {
            continue;
        }
        if !field_matches(pkg_cpu, &cpu) {
            continue;
        }
        if !libc.is_empty() && !field_matches(pkg_libc, &libc) {
            continue;
        }
        return true;
    }
    false
}

/// Remove root-level optional dependencies that fail the platform
/// check or appear in the ignore list from a parsed `LockfileGraph`,
/// then garbage-collect any packages that become unreachable from the
/// surviving importers.
///
/// Used by the install-from-lockfile path, where the resolver's inline
/// filter never runs: the lockfile carries os/cpu/libc per package so
/// aube can re-check on every platform without reparsing packuments.
///
/// Only *root* optional edges are inspected directly. Transitive
/// optional edges are not stripped, because the lockfile does not
/// record per-edge optionality in a form aube currently reads. Any
/// transitive package that becomes unreachable after root-edge pruning
/// is removed by the GC pass.
pub fn filter_graph(
    graph: &mut aube_lockfile::LockfileGraph,
    supported: &SupportedArchitectures,
    ignored: &std::collections::BTreeSet<String>,
) {
    use aube_lockfile::DepType;

    let is_mismatched =
        |pkg: &aube_lockfile::LockedPackage| !is_supported(&pkg.os, &pkg.cpu, &pkg.libc, supported);

    // 1. Drop root optional deps by name or by platform.
    for deps in graph.importers.values_mut() {
        deps.retain(|dep| {
            if dep.dep_type != DepType::Optional {
                return true;
            }
            if ignored.contains(&dep.name) {
                return false;
            }
            !matches!(graph.packages.get(&dep.dep_path), Some(pkg) if is_mismatched(pkg))
        });
    }

    // 2. Garbage-collect unreachable packages by walking from the
    //    surviving roots.
    let mut reachable: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut stack: Vec<String> = Vec::new();
    for deps in graph.importers.values() {
        for dep in deps {
            stack.push(dep.dep_path.clone());
        }
    }
    while let Some(dep_path) = stack.pop() {
        if !reachable.insert(dep_path.clone()) {
            continue;
        }
        if let Some(pkg) = graph.packages.get(&dep_path) {
            for (name, tail) in &pkg.dependencies {
                // Different lockfile readers use different conventions
                // for dependency values: the pnpm reader stores the
                // dep_path *tail* (`"1.2.3"`), while the npm/yarn/bun
                // readers store the full dep_path (`"foo@1.2.3"`).
                // Try the raw value first, then the pnpm-style
                // reconstruction.
                if graph.packages.contains_key(tail) {
                    stack.push(tail.clone());
                } else {
                    let child_key = format!("{name}@{tail}");
                    if graph.packages.contains_key(&child_key) {
                        stack.push(child_key);
                    }
                }
            }
        }
    }
    graph.packages.retain(|k, _| reachable.contains(k));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(xs: &[&str]) -> Vec<String> {
        xs.iter().map(|x| (*x).to_string()).collect()
    }

    #[test]
    fn empty_fields_accept_any_host() {
        let sup = SupportedArchitectures::default();
        assert!(is_supported(&[], &[], &[], &sup));
    }

    #[test]
    fn positive_match_rules() {
        assert!(field_matches(&s(&["linux", "darwin"]), "linux"));
        assert!(!field_matches(&s(&["linux", "darwin"]), "win32"));
    }

    #[test]
    fn negation_rejects_match() {
        assert!(!field_matches(&s(&["!win32"]), "win32"));
        assert!(field_matches(&s(&["!win32"]), "linux"));
    }

    #[test]
    fn mixed_negation_and_positive() {
        // Negation takes precedence: even if a positive also matches,
        // hitting a negation rejects.
        assert!(!field_matches(&s(&["linux", "!linux"]), "linux"));
    }

    #[test]
    fn supported_architectures_widens_with_current() {
        // `["current", "linux"]` should accept the host *or* linux.
        let sup = SupportedArchitectures {
            os: s(&["current", "linux"]),
            ..Default::default()
        };
        // A linux-only package passes regardless of host.
        assert!(is_supported(&s(&["linux"]), &[], &[], &sup));
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn libc_ignored_off_linux() {
        // On a non-Linux host, a package that declares libc=musl
        // should still pass — npm only enforces libc on Linux.
        let sup = SupportedArchitectures::default();
        assert!(is_supported(&[], &[], &s(&["musl"]), &sup));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_glibc_host_rejects_musl_only_package() {
        // The mirror of `libc_ignored_off_linux`: on a glibc Linux
        // host, a package that declares libc=musl must not pass.
        // Skipped on musl Linux builds, since "current" expands to
        // musl there and the package would (correctly) match.
        if cfg!(target_env = "musl") {
            return;
        }
        let sup = SupportedArchitectures::default();
        assert!(!is_supported(&[], &[], &s(&["musl"]), &sup));
    }
}
