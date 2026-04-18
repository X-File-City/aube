//! Platform-specific directory-link and bin-shim creation.
//!
//! ## Directory links ([`create_dir_link`])
//!
//! On Unix, [`create_dir_link`] is a thin wrapper around
//! `std::os::unix::fs::symlink` — same semantics as any other
//! symlink-based linker.
//!
//! On Windows, [`create_dir_link`] creates an **NTFS junction**
//! rather than a real symlink. Junctions don't require Developer
//! Mode or admin rights, which is the whole reason pnpm and npm use
//! them for `node_modules` tree layout on Windows (they go through
//! Node's `fs.symlink(target, path, 'junction')`, which translates
//! to the same `FSCTL_SET_REPARSE_POINT` dance the `junction` crate
//! wraps). Real Windows symlinks via `std::os::windows::fs::
//! symlink_dir` would require either elevated privileges or
//! Developer Mode — neither of which is available on GitHub-hosted
//! `windows-latest` runners or on vanilla Windows developer
//! machines, so using real symlinks would break installs in both
//! places.
//!
//! There is one wrinkle vs. Unix symlinks that callers must honor:
//! **Junctions only accept absolute targets.** If the caller passes
//! a relative target, this helper resolves it against the link's
//! parent directory before handing it to `junction::create`.
//!
//! ## Bin shims ([`create_bin_shim`])
//!
//! Two dials control the shape of each entry:
//!
//! - `prefer_symlinked_executables` (POSIX only). Default `None` is
//!   "platform default", which on POSIX is a plain symlink — same as
//!   pnpm's `preferSymlinkedExecutables=true`. `Some(false)` falls
//!   back to a shell-script shim matching the Windows shell wrapper;
//!   callers opt into this when they need `extendNodePath` to
//!   actually set `NODE_PATH` (a bare symlink can't export env vars).
//!   Windows never creates real symlinks here — Developer Mode /
//!   admin rights would be required, and both are commonly absent on
//!   CI and developer machines.
//!
//! - `extend_node_path`. When `true`, shell/cmd/powershell shims set
//!   `NODE_PATH` to `$basedir/..` (the top-level `node_modules`) so
//!   the shimmed binary can resolve modules regardless of where it's
//!   invoked from. Matches pnpm's `extendNodePath=true`. No-op when
//!   the final output is a symlink (POSIX default) — symlinks can't
//!   export env vars, which is why callers who care pair it with
//!   `prefer_symlinked_executables=false`.
//!
//! On Windows, `create_bin_shim` writes three plain-text wrapper
//! scripts into the bin directory — `.cmd` (for cmd.exe), `.ps1`
//! (PowerShell), and an extensionless shell script (Git Bash /
//! MSYS2). This is the same approach pnpm and npm use via
//! `cmd-shim`, and it avoids the need for Developer Mode or admin
//! rights entirely.

use std::io;
use std::path::{Component, Path, PathBuf};

/// Create a directory link from `link` to `target`.
///
/// - Unix: a plain symlink (relative or absolute target OK).
/// - Windows: an NTFS junction (relative targets are resolved to
///   absolute against `link`'s parent first).
pub fn create_dir_link(target: &Path, link: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link)
    }
    #[cfg(windows)]
    {
        let abs_target = if target.is_absolute() {
            target.to_path_buf()
        } else {
            let parent = link.parent().ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "junction link has no parent directory",
                )
            })?;
            normalize_path(&parent.join(target))
        };
        junction::create(abs_target, link)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (target, link);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "directory links are not supported on this platform",
        ))
    }
}

/// Options controlling the shape of a generated bin entry.
///
/// `Default` preserves the pre-settings behavior: POSIX symlink,
/// Windows shim without `NODE_PATH`.
#[derive(Debug, Clone, Copy, Default)]
pub struct BinShimOptions {
    /// Export `NODE_PATH` (pointing at the top-level `node_modules`)
    /// in shell / cmd / PowerShell shims. Has no effect when the
    /// final entry is a POSIX symlink.
    pub extend_node_path: bool,
    /// POSIX-only. `None` → platform default (symlink). `Some(true)` is
    /// equivalent. `Some(false)` writes a shell-script shim instead, so
    /// `extend_node_path` can actually inject `NODE_PATH`. Ignored on
    /// Windows — shims are always used there.
    pub prefer_symlinked_executables: Option<bool>,
}

/// Create bin shims for a package binary.
///
/// - Unix (default / `prefer_symlinked_executables != Some(false)`):
///   a symlink from `bin_dir/<name>` to `target`, with the target
///   chmod'd to 755.
/// - Unix (`prefer_symlinked_executables = Some(false)`): a shell
///   wrapper that `exec`s `target` via its detected interpreter. If
///   `extend_node_path` is set, the wrapper exports `NODE_PATH` first.
/// - Windows: three wrapper scripts in `bin_dir`:
///   - `<name>.cmd` — batch wrapper for cmd.exe
///   - `<name>.ps1` — PowerShell wrapper
///   - `<name>` (no extension) — shell wrapper for Git Bash / MSYS2
///
///   `extend_node_path` sets `NODE_PATH` near the top of each wrapper.
///
/// The `target` path should be absolute; the generated wrappers
/// embed a path relative to `bin_dir` so the tree stays relocatable.
pub fn create_bin_shim(
    bin_dir: &Path,
    name: &str,
    target: &Path,
    opts: BinShimOptions,
) -> io::Result<()> {
    #[cfg(unix)]
    {
        let write_shim = matches!(opts.prefer_symlinked_executables, Some(false));
        let link_path = bin_dir.join(name);
        let _ = std::fs::remove_file(&link_path);
        if write_shim {
            let rel = relative_bin_target(bin_dir, target);
            let prog = detect_interpreter(target);
            std::fs::write(
                &link_path,
                generate_posix_shim(&prog, &rel, opts.extend_node_path),
            )?;
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&link_path, std::fs::Permissions::from_mode(0o755))?;
        } else {
            std::os::unix::fs::symlink(target, &link_path)?;
            use std::os::unix::fs::PermissionsExt;
            if target.exists() {
                let _ = std::fs::set_permissions(target, std::fs::Permissions::from_mode(0o755));
            }
        }
    }
    #[cfg(windows)]
    {
        // Remove any stale files (previous shims or legacy symlinks).
        for ext in ["", ".cmd", ".ps1"] {
            let p = if ext.is_empty() {
                bin_dir.join(name)
            } else {
                bin_dir.join(format!("{name}{ext}"))
            };
            let _ = std::fs::remove_file(&p);
        }

        let rel = relative_bin_target(bin_dir, target);
        let prog = detect_interpreter(target);

        let rel_backslash = rel.replace('/', "\\");
        let rel_fwdslash = rel.replace('\\', "/");

        // .cmd (cmd.exe)
        std::fs::write(
            bin_dir.join(format!("{name}.cmd")),
            generate_cmd_shim(&prog, &rel_backslash, opts.extend_node_path),
        )?;

        // .ps1 (PowerShell)
        std::fs::write(
            bin_dir.join(format!("{name}.ps1")),
            generate_ps1_shim(&prog, &rel_fwdslash, opts.extend_node_path),
        )?;

        // extensionless (Git Bash / MSYS2)
        std::fs::write(
            bin_dir.join(name),
            generate_sh_shim(&prog, &rel_fwdslash, opts.extend_node_path),
        )?;
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (bin_dir, name, target, opts);
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "bin shims are not supported on this platform",
        ));
    }
    Ok(())
}

/// Remove bin shims previously created by [`create_bin_shim`].
///
/// On Unix, removes the symlink. On Windows, removes the `.cmd`,
/// `.ps1`, and extensionless wrapper scripts.
pub fn remove_bin_shim(bin_dir: &Path, name: &str) {
    let _ = std::fs::remove_file(bin_dir.join(name));
    #[cfg(windows)]
    {
        let _ = std::fs::remove_file(bin_dir.join(format!("{name}.cmd")));
        let _ = std::fs::remove_file(bin_dir.join(format!("{name}.ps1")));
    }
}

/// Compute the relative path from `bin_dir` to `target`, using
/// forward slashes.
fn relative_bin_target(bin_dir: &Path, target: &Path) -> String {
    pathdiff::diff_paths(target, bin_dir)
        .unwrap_or_else(|| PathBuf::from(target))
        .to_string_lossy()
        .replace('\\', "/")
}

/// Read the shebang line of `target` to determine the interpreter.
/// Falls back to `"node"` for `.js` / `.cjs` / `.mjs` files, or if
/// the target doesn't exist or has no shebang.
///
/// Only reads the first 256 bytes — enough for any realistic shebang
/// line without pulling large bundled scripts into memory.
fn detect_interpreter(target: &Path) -> String {
    use std::io::Read;
    let mut buf = [0u8; 256];
    let n = std::fs::File::open(target)
        .and_then(|mut f| f.read(&mut buf))
        .unwrap_or(0);
    let content = &buf[..n];
    if n > 2
        && content.starts_with(b"#!")
        && let Some(line_end) = content.iter().position(|&b| b == b'\n')
    {
        let line = String::from_utf8_lossy(&content[2..line_end]);
        let line = line.trim();
        // Strip `/usr/bin/env ` prefix (with optional -S flag)
        let prog = if let Some(rest) = line.strip_prefix("/usr/bin/env") {
            let rest = rest.trim_start();
            let rest = rest.strip_prefix("-S").map_or(rest, |r| r.trim_start());
            // Strip leading env var assignments (KEY=val)
            rest.split_whitespace()
                .find(|s| !s.contains('='))
                .unwrap_or("node")
        } else {
            // Absolute path like /usr/bin/node → take basename
            line.split_whitespace()
                .next()
                .and_then(|p| p.rsplit('/').next())
                .unwrap_or("node")
        };
        if !prog.is_empty() {
            return prog.to_string();
        }
    }
    // Default based on extension
    match target.extension().and_then(|e| e.to_str()) {
        Some("js" | "cjs" | "mjs") | None => "node".to_string(),
        Some("cmd" | "bat") => "cmd".to_string(),
        Some("ps1") => "pwsh".to_string(),
        Some("sh") => "sh".to_string(),
        Some(_) => "node".to_string(),
    }
}

#[cfg(windows)]
fn generate_cmd_shim(prog: &str, rel_target_backslash: &str, extend_node_path: bool) -> String {
    // NODE_PATH points at the top-level `node_modules` — one level
    // up from `.bin`. `%~dp0` already ends with a backslash.
    let node_path = if extend_node_path {
        "@SET NODE_PATH=%~dp0..\r\n"
    } else {
        ""
    };
    format!(
        "@SETLOCAL\r\n\
         {node_path}\
         @IF EXIST \"%~dp0\\{prog}.exe\" (\r\n\
         \x20 \"%~dp0\\{prog}.exe\" \"%~dp0\\{rel_target_backslash}\" %*\r\n\
         ) ELSE (\r\n\
         \x20 @SET PATHEXT=%PATHEXT:;.JS;=;%\r\n\
         \x20 {prog} \"%~dp0\\{rel_target_backslash}\" %*\r\n\
         )\r\n"
    )
}

#[cfg(windows)]
fn generate_ps1_shim(prog: &str, rel_target_fwdslash: &str, extend_node_path: bool) -> String {
    let node_path = if extend_node_path {
        "$env:NODE_PATH=\"$basedir/..\"\n"
    } else {
        ""
    };
    format!(
        "#!/usr/bin/env pwsh\n\
         $basedir=Split-Path $MyInvocation.MyCommand.Definition -Parent\n\
         \n\
         {node_path}\
         $exe=\"\"\n\
         if ($PSVersionTable.PSVersion -lt \"6.0\" -or $IsWindows) {{\n\
         \x20 $exe=\".exe\"\n\
         }}\n\
         $ret=0\n\
         if (Test-Path \"$basedir/{prog}$exe\") {{\n\
         \x20 if ($MyInvocation.ExpectingInput) {{\n\
         \x20\x20\x20 $input | & \"$basedir/{prog}$exe\" \"$basedir/{rel_target_fwdslash}\" $args\n\
         \x20 }} else {{\n\
         \x20\x20\x20 & \"$basedir/{prog}$exe\" \"$basedir/{rel_target_fwdslash}\" $args\n\
         \x20 }}\n\
         \x20 $ret=$LASTEXITCODE\n\
         }} else {{\n\
         \x20 if ($MyInvocation.ExpectingInput) {{\n\
         \x20\x20\x20 $input | & \"{prog}$exe\" \"$basedir/{rel_target_fwdslash}\" $args\n\
         \x20 }} else {{\n\
         \x20\x20\x20 & \"{prog}$exe\" \"$basedir/{rel_target_fwdslash}\" $args\n\
         \x20 }}\n\
         \x20 $ret=$LASTEXITCODE\n\
         }}\n\
         exit $ret\n"
    )
}

#[cfg(windows)]
fn generate_sh_shim(prog: &str, rel_target_fwdslash: &str, extend_node_path: bool) -> String {
    let node_path = if extend_node_path {
        "export NODE_PATH=\"$basedir/..\"\n"
    } else {
        ""
    };
    format!(
        "#!/bin/sh\n\
         basedir=$(dirname \"$(echo \"$0\" | sed -e 's,\\\\,/,g')\")\n\
         \n\
         case `uname` in\n\
         \x20\x20\x20 *CYGWIN*|*MINGW*|*MSYS*)\n\
         \x20\x20\x20\x20\x20\x20\x20 if command -v cygpath > /dev/null 2>&1; then\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20 basedir=`cygpath -w \"$basedir\"`\n\
         \x20\x20\x20\x20\x20\x20\x20 fi\n\
         \x20\x20\x20 ;;\n\
         esac\n\
         \n\
         {node_path}\
         if [ -x \"$basedir/{prog}\" ]; then\n\
         \x20 exec \"$basedir/{prog}\" \"$basedir/{rel_target_fwdslash}\" \"$@\"\n\
         else\n\
         \x20 exec {prog} \"$basedir/{rel_target_fwdslash}\" \"$@\"\n\
         fi\n"
    )
}

/// Marker the POSIX shim writer stamps into every generated file so
/// [`parse_posix_shim_target`] can unambiguously identify our shims and
/// recover the `$basedir`-relative target path on uninstall. Any format
/// change here must bump the `v1` suffix so older shims stop being
/// recognized (forcing a reinstall) rather than being silently
/// misparsed.
pub const POSIX_SHIM_MARKER_PREFIX: &str = "# aube-bin-shim v1 target=";

/// POSIX shell-script shim used when `prefer_symlinked_executables=false`
/// (so `extend_node_path` can actually inject `NODE_PATH`). Mirrors the
/// Windows `generate_sh_shim` output without the cygpath dance, with a
/// stamped [`POSIX_SHIM_MARKER_PREFIX`] comment at the top so
/// `unlink_bins` can locate the embedded target without having to parse
/// the shell body.
#[cfg(unix)]
fn generate_posix_shim(prog: &str, rel_target_fwdslash: &str, extend_node_path: bool) -> String {
    let node_path = if extend_node_path {
        "export NODE_PATH=\"$basedir/..\"\n"
    } else {
        ""
    };
    format!(
        "#!/bin/sh\n\
         {POSIX_SHIM_MARKER_PREFIX}{rel_target_fwdslash}\n\
         basedir=$(dirname \"$0\")\n\
         {node_path}\
         if [ -x \"$basedir/{prog}\" ]; then\n\
         \x20 exec \"$basedir/{prog}\" \"$basedir/{rel_target_fwdslash}\" \"$@\"\n\
         else\n\
         \x20 exec {prog} \"$basedir/{rel_target_fwdslash}\" \"$@\"\n\
         fi\n"
    )
}

/// Recover the `$basedir`-relative target embedded by
/// [`generate_posix_shim`]. Returns `None` for any content that lacks
/// the [`POSIX_SHIM_MARKER_PREFIX`] marker — including shims written by
/// other tools and older aube versions if the marker is ever bumped.
/// Lives in this module so the format contract stays in one file with
/// its writer.
pub fn parse_posix_shim_target(content: &str) -> Option<&str> {
    for line in content.lines() {
        if let Some(rest) = line.strip_prefix(POSIX_SHIM_MARKER_PREFIX) {
            return Some(rest);
        }
    }
    None
}

/// Collapse `.` / `..` components without touching the filesystem.
/// Used on Windows to give `junction::create` an absolute target when
/// the caller computed a relative `../../foo` — `canonicalize` isn't
/// an option because it requires the target to already exist and
/// strips the UNC prefix the junction API is happy to accept.
/// Also exposed cross-platform so callers can resolve relative paths
/// stored in POSIX shims without tripping over macOS's `/var` →
/// `/private/var` symlink (canonicalize eagerly follows that symlink,
/// which throws off the `..` count in shim-embedded relative targets).
pub fn normalize_path(path: &Path) -> PathBuf {
    let mut out: Vec<Component> = Vec::new();
    for comp in path.components() {
        match comp {
            Component::ParentDir => {
                if !matches!(
                    out.last(),
                    None | Some(Component::RootDir) | Some(Component::Prefix(_))
                ) {
                    out.pop();
                } else {
                    out.push(comp);
                }
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    out.iter().map(|c| c.as_os_str()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_interpreter_shebang_env_node() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("cli.js");
        std::fs::write(&script, "#!/usr/bin/env node\nconsole.log('hi');\n").unwrap();
        assert_eq!(detect_interpreter(&script), "node");
    }

    #[test]
    fn detect_interpreter_shebang_env_with_s_flag() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("cli.js");
        std::fs::write(
            &script,
            "#!/usr/bin/env -S node --harmony\nconsole.log('hi');\n",
        )
        .unwrap();
        assert_eq!(detect_interpreter(&script), "node");
    }

    #[test]
    fn detect_interpreter_shebang_absolute_path() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("cli.js");
        std::fs::write(&script, "#!/usr/bin/node\nconsole.log('hi');\n").unwrap();
        assert_eq!(detect_interpreter(&script), "node");
    }

    #[test]
    fn detect_interpreter_shebang_env_python() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("cli.py");
        std::fs::write(&script, "#!/usr/bin/env python3\nprint('hi')\n").unwrap();
        assert_eq!(detect_interpreter(&script), "python3");
    }

    #[test]
    fn detect_interpreter_shebang_with_env_vars() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("cli.js");
        std::fs::write(
            &script,
            "#!/usr/bin/env NODE_OPTIONS=--max-old-space-size=4096 node\nconsole.log('hi');\n",
        )
        .unwrap();
        assert_eq!(detect_interpreter(&script), "node");
    }

    #[test]
    fn detect_interpreter_no_shebang_js() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("cli.js");
        std::fs::write(&script, "console.log('hi');\n").unwrap();
        assert_eq!(detect_interpreter(&script), "node");
    }

    #[test]
    fn detect_interpreter_nonexistent_file_defaults_to_node() {
        assert_eq!(
            detect_interpreter(Path::new("/nonexistent/file.js")),
            "node"
        );
    }

    #[test]
    fn relative_bin_target_computes_path() {
        let bin_dir = Path::new("/project/node_modules/.bin");
        let target =
            Path::new("/project/node_modules/.aube/is-odd@3.0.1/node_modules/is-odd/cli.js");
        let rel = relative_bin_target(bin_dir, target);
        assert_eq!(rel, "../.aube/is-odd@3.0.1/node_modules/is-odd/cli.js");
    }

    #[cfg(windows)]
    #[test]
    fn normalize_collapses_parent_and_cur_dir() {
        let p = Path::new(r"C:\a\b\.\..\c\d\..\e");
        assert_eq!(normalize_path(p), PathBuf::from(r"C:\a\c\e"));
    }

    #[cfg(windows)]
    #[test]
    fn creates_junction_without_developer_mode() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target");
        std::fs::create_dir(&target).unwrap();
        std::fs::write(target.join("marker.txt"), b"hi").unwrap();

        let link = dir.path().join("parent").join("link");
        std::fs::create_dir_all(link.parent().unwrap()).unwrap();
        // Relative target, mimicking how the linker builds them.
        let rel = Path::new("..").join("target");
        create_dir_link(&rel, &link).unwrap();

        assert_eq!(std::fs::read(link.join("marker.txt")).unwrap(), b"hi");
    }

    #[cfg(windows)]
    #[test]
    fn create_bin_shim_writes_three_files() {
        let dir = tempfile::tempdir().unwrap();
        let bin_dir = dir.path().join("node_modules/.bin");
        std::fs::create_dir_all(&bin_dir).unwrap();

        let pkg_dir = dir
            .path()
            .join("node_modules/.aube/is-odd@3.0.1/node_modules/is-odd");
        std::fs::create_dir_all(&pkg_dir).unwrap();
        let script = pkg_dir.join("cli.js");
        std::fs::write(&script, "#!/usr/bin/env node\nconsole.log('hi');\n").unwrap();

        create_bin_shim(&bin_dir, "is-odd", &script, BinShimOptions::default()).unwrap();

        // All three files must exist
        assert!(bin_dir.join("is-odd.cmd").exists());
        assert!(bin_dir.join("is-odd.ps1").exists());
        assert!(bin_dir.join("is-odd").exists());

        // .cmd should reference node and the relative target
        let cmd = std::fs::read_to_string(bin_dir.join("is-odd.cmd")).unwrap();
        assert!(cmd.contains("node.exe"));
        assert!(cmd.contains(".aube"));

        // .ps1 should reference node
        let ps1 = std::fs::read_to_string(bin_dir.join("is-odd.ps1")).unwrap();
        assert!(ps1.contains("node$exe"));

        // extensionless should be a shell script
        let sh = std::fs::read_to_string(bin_dir.join("is-odd")).unwrap();
        assert!(sh.starts_with("#!/bin/sh"));
    }

    #[cfg(windows)]
    #[test]
    fn create_bin_shim_cleans_old_files() {
        let dir = tempfile::tempdir().unwrap();
        let bin_dir = dir.path().join("node_modules/.bin");
        std::fs::create_dir_all(&bin_dir).unwrap();

        let pkg_dir = dir.path().join("pkg");
        std::fs::create_dir_all(&pkg_dir).unwrap();
        let script = pkg_dir.join("cli.js");
        std::fs::write(&script, "#!/usr/bin/env node\nconsole.log('v1');\n").unwrap();

        // First shim
        create_bin_shim(&bin_dir, "mycli", &script, BinShimOptions::default()).unwrap();
        let cmd1 = std::fs::read_to_string(bin_dir.join("mycli.cmd")).unwrap();

        // Update script and re-shim
        std::fs::write(&script, "#!/usr/bin/env node\nconsole.log('v2');\n").unwrap();
        create_bin_shim(&bin_dir, "mycli", &script, BinShimOptions::default()).unwrap();
        let cmd2 = std::fs::read_to_string(bin_dir.join("mycli.cmd")).unwrap();

        // Content should be the same (same target path), but no error from overwrite
        assert_eq!(cmd1, cmd2);
    }

    #[cfg(windows)]
    #[test]
    fn remove_bin_shim_removes_all_files() {
        let dir = tempfile::tempdir().unwrap();
        let bin_dir = dir.path().join("node_modules/.bin");
        std::fs::create_dir_all(&bin_dir).unwrap();

        let pkg_dir = dir.path().join("pkg");
        std::fs::create_dir_all(&pkg_dir).unwrap();
        let script = pkg_dir.join("cli.js");
        std::fs::write(&script, "console.log('hi');\n").unwrap();

        create_bin_shim(&bin_dir, "mycli", &script, BinShimOptions::default()).unwrap();
        assert!(bin_dir.join("mycli.cmd").exists());
        assert!(bin_dir.join("mycli.ps1").exists());
        assert!(bin_dir.join("mycli").exists());

        remove_bin_shim(&bin_dir, "mycli");
        assert!(!bin_dir.join("mycli.cmd").exists());
        assert!(!bin_dir.join("mycli.ps1").exists());
        assert!(!bin_dir.join("mycli").exists());
    }

    #[cfg(unix)]
    #[test]
    fn create_bin_shim_creates_symlink_on_unix() {
        let dir = tempfile::tempdir().unwrap();
        let bin_dir = dir.path().join("node_modules/.bin");
        std::fs::create_dir_all(&bin_dir).unwrap();

        let pkg_dir = dir.path().join("pkg");
        std::fs::create_dir_all(&pkg_dir).unwrap();
        let script = pkg_dir.join("cli.js");
        std::fs::write(&script, "#!/usr/bin/env node\nconsole.log('hi');\n").unwrap();

        create_bin_shim(&bin_dir, "mycli", &script, BinShimOptions::default()).unwrap();

        let link = bin_dir.join("mycli");
        assert!(link.symlink_metadata().unwrap().file_type().is_symlink());

        // Target should be executable
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&script).unwrap().permissions().mode();
        assert_eq!(mode & 0o755, 0o755);
    }

    #[cfg(unix)]
    #[test]
    fn create_bin_shim_writes_posix_shim_when_symlink_opt_out() {
        let dir = tempfile::tempdir().unwrap();
        let bin_dir = dir.path().join("node_modules/.bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        let pkg_dir = dir.path().join("pkg");
        std::fs::create_dir_all(&pkg_dir).unwrap();
        let script = pkg_dir.join("cli.js");
        std::fs::write(&script, "#!/usr/bin/env node\nconsole.log('hi');\n").unwrap();

        create_bin_shim(
            &bin_dir,
            "mycli",
            &script,
            BinShimOptions {
                extend_node_path: false,
                prefer_symlinked_executables: Some(false),
            },
        )
        .unwrap();

        let path = bin_dir.join("mycli");
        // Must be a regular file, not a symlink.
        let meta = path.symlink_metadata().unwrap();
        assert!(!meta.file_type().is_symlink());
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.starts_with("#!/bin/sh"));
        assert!(content.contains("exec \"$basedir/node\""));
        // Marker comment has to land in the shim so `parse_posix_shim_target`
        // can round-trip the target on uninstall.
        assert!(content.contains(POSIX_SHIM_MARKER_PREFIX));
        // NODE_PATH should NOT be exported when extend_node_path=false.
        assert!(!content.contains("NODE_PATH"));
        // Must be marked executable.
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o111, 0o111);
    }

    #[cfg(unix)]
    #[test]
    fn parse_posix_shim_target_round_trips_generator_output() {
        // The parser and generator live together so this loop-back
        // guards the format contract end-to-end: anything that
        // changes the marker on one side breaks this test.
        let dir = tempfile::tempdir().unwrap();
        let bin_dir = dir.path().join("node_modules/.bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        let pkg_dir = dir
            .path()
            .join("node_modules/.aube/semver@1.0.0/node_modules/semver");
        std::fs::create_dir_all(&pkg_dir).unwrap();
        let script = pkg_dir.join("bin/semver.js");
        std::fs::create_dir_all(script.parent().unwrap()).unwrap();
        std::fs::write(&script, "#!/usr/bin/env node\n").unwrap();

        create_bin_shim(
            &bin_dir,
            "semver",
            &script,
            BinShimOptions {
                extend_node_path: true,
                prefer_symlinked_executables: Some(false),
            },
        )
        .unwrap();

        let content = std::fs::read_to_string(bin_dir.join("semver")).unwrap();
        let rel = parse_posix_shim_target(&content).expect("shim should carry its marker");
        assert_eq!(
            rel,
            "../.aube/semver@1.0.0/node_modules/semver/bin/semver.js",
        );
    }

    #[test]
    fn parse_posix_shim_target_rejects_foreign_scripts() {
        // Arbitrary shell content without our marker must not match —
        // otherwise `unlink_bins` would start removing bins owned by
        // other tooling.
        assert!(parse_posix_shim_target("#!/bin/sh\necho hi\n").is_none());
        // A stray `exec` line with `$basedir/...` isn't enough: the
        // dedicated marker is the only anchor.
        assert!(
            parse_posix_shim_target("#!/bin/sh\nexec node \"$basedir/../pkg/cli.js\" \"$@\"\n",)
                .is_none()
        );
    }

    #[cfg(unix)]
    #[test]
    fn create_bin_shim_injects_node_path_in_posix_shim() {
        let dir = tempfile::tempdir().unwrap();
        let bin_dir = dir.path().join("node_modules/.bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        let pkg_dir = dir.path().join("pkg");
        std::fs::create_dir_all(&pkg_dir).unwrap();
        let script = pkg_dir.join("cli.js");
        std::fs::write(&script, "#!/usr/bin/env node\nconsole.log('hi');\n").unwrap();

        create_bin_shim(
            &bin_dir,
            "mycli",
            &script,
            BinShimOptions {
                extend_node_path: true,
                prefer_symlinked_executables: Some(false),
            },
        )
        .unwrap();

        let content = std::fs::read_to_string(bin_dir.join("mycli")).unwrap();
        assert!(content.contains("export NODE_PATH=\"$basedir/..\""));
    }

    #[cfg(unix)]
    #[test]
    fn create_bin_shim_ignores_node_path_for_symlink() {
        // extend_node_path is meaningless when the output is a bare
        // symlink — no file to inject an env export into. The symlink
        // still gets created, and the test only confirms that the
        // Some(true) / None paths behave identically.
        let dir = tempfile::tempdir().unwrap();
        let bin_dir = dir.path().join("node_modules/.bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        let pkg_dir = dir.path().join("pkg");
        std::fs::create_dir_all(&pkg_dir).unwrap();
        let script = pkg_dir.join("cli.js");
        std::fs::write(&script, "#!/usr/bin/env node\nconsole.log('hi');\n").unwrap();

        create_bin_shim(
            &bin_dir,
            "mycli",
            &script,
            BinShimOptions {
                extend_node_path: true,
                prefer_symlinked_executables: None,
            },
        )
        .unwrap();

        let link = bin_dir.join("mycli");
        assert!(link.symlink_metadata().unwrap().file_type().is_symlink());
    }

    #[cfg(windows)]
    #[test]
    fn create_bin_shim_injects_node_path_on_windows() {
        let dir = tempfile::tempdir().unwrap();
        let bin_dir = dir.path().join("node_modules/.bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        let pkg_dir = dir.path().join("pkg");
        std::fs::create_dir_all(&pkg_dir).unwrap();
        let script = pkg_dir.join("cli.js");
        std::fs::write(&script, "#!/usr/bin/env node\nconsole.log('hi');\n").unwrap();

        create_bin_shim(
            &bin_dir,
            "mycli",
            &script,
            BinShimOptions {
                extend_node_path: true,
                prefer_symlinked_executables: None,
            },
        )
        .unwrap();

        let cmd = std::fs::read_to_string(bin_dir.join("mycli.cmd")).unwrap();
        assert!(cmd.contains("@SET NODE_PATH=%~dp0.."));
        let ps1 = std::fs::read_to_string(bin_dir.join("mycli.ps1")).unwrap();
        assert!(ps1.contains("$env:NODE_PATH=\"$basedir/..\""));
        let sh = std::fs::read_to_string(bin_dir.join("mycli")).unwrap();
        assert!(sh.contains("export NODE_PATH=\"$basedir/..\""));
    }

    #[cfg(windows)]
    #[test]
    fn create_bin_shim_omits_node_path_when_false() {
        let dir = tempfile::tempdir().unwrap();
        let bin_dir = dir.path().join("node_modules/.bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        let pkg_dir = dir.path().join("pkg");
        std::fs::create_dir_all(&pkg_dir).unwrap();
        let script = pkg_dir.join("cli.js");
        std::fs::write(&script, "console.log('hi');\n").unwrap();

        create_bin_shim(
            &bin_dir,
            "mycli",
            &script,
            BinShimOptions {
                extend_node_path: false,
                prefer_symlinked_executables: None,
            },
        )
        .unwrap();

        let cmd = std::fs::read_to_string(bin_dir.join("mycli.cmd")).unwrap();
        assert!(!cmd.contains("NODE_PATH"));
    }
}
