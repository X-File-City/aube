use std::path::PathBuf;

/// XDG-compliant cache directory for aube.
/// Uses `$XDG_CACHE_HOME/aube`, `$HOME/.cache/aube`, or `%LOCALAPPDATA%\aube` on Windows.
pub fn cache_dir() -> Option<PathBuf> {
    if let Ok(xdg) = std::env::var("XDG_CACHE_HOME") {
        return Some(PathBuf::from(xdg).join("aube"));
    }
    #[cfg(windows)]
    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        return Some(PathBuf::from(local).join("aube"));
    }
    std::env::var("HOME")
        .ok()
        .map(|h| PathBuf::from(h).join(".cache/aube"))
}

/// Global directory for linked packages.
/// Uses `$XDG_CACHE_HOME/aube/global-links`, `$HOME/.cache/aube/global-links`,
/// or `%LOCALAPPDATA%\aube\global-links` on Windows.
pub fn global_links_dir() -> Option<PathBuf> {
    cache_dir().map(|d| d.join("global-links"))
}

/// Aube-owned global content-addressable store directory.
/// Uses `$HOME/.aube-store/v1/files/` (or `%LOCALAPPDATA%\aube-store\v1\files` on Windows).
pub fn store_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        return Some(PathBuf::from(local).join("aube-store/v1/files"));
    }
    std::env::var("HOME")
        .ok()
        .map(|h| PathBuf::from(h).join(".aube-store/v1/files"))
}
