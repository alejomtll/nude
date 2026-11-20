//! Qué eventos del sistema de ficheros merecen despertar al watcher.

use std::path::Path;

/// Directorios que nunca generan checkpoints (relativos a la raíz del proyecto).
const IGNORED_DIRS: &[&str] = &[".git", "node_modules", "target"];

const TEMP_EXTENSIONS: &[&str] = &[
    "swp", "swo", "swx", "tmp", "temp", "part", "crdownload", "bak", "orig",
];

const TEMP_NAMES: &[&str] = &[".DS_Store", "4913", ".localized"];

/// `true` si el cambio en `path` debe ignorarse.
pub fn is_ignored(root: &Path, path: &Path) -> bool {
    let Ok(rel) = path.strip_prefix(root) else {
        return true; // fuera del proyecto
    };
    if rel.as_os_str().is_empty() {
        return true;
    }
    if rel
        .components()
        .any(|c| IGNORED_DIRS.iter().any(|d| c.as_os_str() == *d))
    {
        return true;
    }
    let Some(name) = rel.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    name.ends_with('~')
        || name.starts_with(".#")
        || name.starts_with(".nfs")
        || TEMP_NAMES.contains(&name)
        || Path::new(name)
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| TEMP_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_noise_but_keeps_source_files() {
        let root = Path::new("/work/app");
        for ignored in [
            "/work/app/.git/index",
            "/work/app/node_modules/x/index.js",
            "/work/app/src-tauri/target/debug/nude",
            "/work/app/src/main.rs~",
            "/work/app/src/.main.rs.swp",
            "/work/app/.DS_Store",
            "/work/app/src/.#lock",
            "/work/app/out.tmp",
            "/work/app/4913",
            "/elsewhere/file.rs",
        ] {
            assert!(is_ignored(root, Path::new(ignored)), "{ignored}");
        }
        for kept in ["/work/app/src/main.rs", "/work/app/README.md", "/work/app/targets/a.rs"] {
            assert!(!is_ignored(root, Path::new(kept)), "{kept}");
        }
    }

    #[test]
    fn a_root_inside_a_target_dir_is_not_ignored() {
        let root = Path::new("/work/target/app");
        assert!(!is_ignored(root, Path::new("/work/target/app/src/lib.rs")));
    }
}
