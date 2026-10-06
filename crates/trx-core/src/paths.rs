//! Zero-dependency XDG base-directory resolution for the global trx config
//! and the central store.
//!
//! Resolution rules per kind:
//! 1. An explicit, *absolute* `XDG_*` env var wins (relative values ignored).
//! 2. Otherwise on unix (incl. macOS) use the XDG-style layout (`~/.config`,
//!    `~/.local/share`).
//! 3. On Windows use `%APPDATA%` / `%LOCALAPPDATA%`.

use crate::Error;
use std::path::PathBuf;

fn resolve_base(
    xdg: Option<PathBuf>,
    home: Option<PathBuf>,
    is_windows: bool,
    unix_rel: &str,
) -> Option<PathBuf> {
    if let Some(p) = xdg.filter(|p| p.is_absolute()) {
        return Some(p);
    }
    if is_windows {
        std::env::var_os("APPDATA").map(PathBuf::from)
    } else {
        home.map(|h| h.join(unix_rel))
    }
}

fn base_dir(xdg_var: &str, unix_rel: &str) -> crate::Result<PathBuf> {
    resolve_base(
        std::env::var_os(xdg_var).map(PathBuf::from),
        std::env::var_os("HOME").map(PathBuf::from),
        cfg!(windows),
        unix_rel,
    )
    .ok_or_else(|| Error::Other(format!("unable to determine base directory ({xdg_var})")))
}

/// Base config directory (`~/.config` on unix).
pub fn config_base() -> crate::Result<PathBuf> {
    base_dir("XDG_CONFIG_HOME", ".config")
}

/// Base data directory (`~/.local/share` on unix).
pub fn data_base() -> crate::Result<PathBuf> {
    base_dir("XDG_DATA_HOME", ".local/share")
}

/// Expand a leading `~/` to the home directory.
pub fn expand_tilde(path: &str) -> crate::Result<PathBuf> {
    if let Some(rest) = path.strip_prefix("~/") {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| crate::Error::Other("cannot expand '~': HOME not set".to_string()))?;
        return Ok(home.join(rest));
    }
    Ok(PathBuf::from(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_absolute_xdg_wins() {
        // resolve_base is exercised through base_dir only indirectly (env is
        // global), so test the pure function via the same logic.
        let got = resolve_base(
            Some(PathBuf::from("/custom/xdg")),
            Some(PathBuf::from("/home/u")),
            false,
            ".local/share",
        );
        assert_eq!(got, Some(PathBuf::from("/custom/xdg")));
    }

    #[test]
    fn test_relative_xdg_ignored() {
        let got = resolve_base(
            Some(PathBuf::from("relative/xdg")),
            Some(PathBuf::from("/home/u")),
            false,
            ".local/share",
        );
        assert_eq!(got, Some(PathBuf::from("/home/u/.local/share")));
    }

    #[test]
    fn test_unix_layout_without_xdg() {
        let got = resolve_base(None, Some(PathBuf::from("/home/u")), false, ".config");
        assert_eq!(got, Some(PathBuf::from("/home/u/.config")));
    }

    #[test]
    fn test_no_home_no_base_on_unix() {
        let got = resolve_base(None, None, false, ".local/share");
        assert_eq!(got, None);
    }

    #[test]
    fn test_expand_tilde() {
        // Non-tilde paths pass through untouched.
        assert_eq!(
            expand_tilde("/abs/path").unwrap(),
            PathBuf::from("/abs/path")
        );
        assert_eq!(
            expand_tilde("~not-home").unwrap(),
            PathBuf::from("~not-home")
        );
    }
}
