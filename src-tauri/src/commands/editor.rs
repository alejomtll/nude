//! Abrir proyectos y archivos en el editor instalado (Cursor, VS Code, Zed) o
//! en Finder. Solo lanza procesos locales de macOS; nada de red.
//!
//! Para saltar a una línea se usa la CLI que trae cada app dentro de su
//! bundle (no hace falta que el usuario la haya instalado en el `PATH`). Si no
//! está, se recurre a `open -b <bundle id>` sin línea.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::{Deserialize, Serialize};

use super::blocking;
use crate::error::{NudeError, NudeResult};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Editor {
    Cursor,
    Vscode,
    Zed,
    Finder,
}

struct EditorApp {
    editor: Editor,
    name: &'static str,
    bundle_ids: &'static [&'static str],
    app_names: &'static [&'static str],
    /// CLI dentro del bundle, relativa a `<App>.app/`.
    cli: &'static str,
}

const APPS: &[EditorApp] = &[
    EditorApp {
        editor: Editor::Cursor,
        name: "Cursor",
        bundle_ids: &["com.todesktop.230313mzl4w4u92"],
        app_names: &["Cursor.app"],
        cli: "Contents/Resources/app/bin/cursor",
    },
    EditorApp {
        editor: Editor::Vscode,
        name: "VS Code",
        bundle_ids: &["com.microsoft.VSCode", "com.microsoft.VSCodeInsiders"],
        app_names: &["Visual Studio Code.app", "Visual Studio Code - Insiders.app"],
        cli: "Contents/Resources/app/bin/code",
    },
    EditorApp {
        editor: Editor::Zed,
        name: "Zed",
        bundle_ids: &["dev.zed.Zed", "dev.zed.Zed-Preview"],
        app_names: &["Zed.app", "Zed Preview.app"],
        cli: "Contents/MacOS/cli",
    },
];

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EditorInfo {
    pub id: Editor,
    pub name: &'static str,
    pub installed: bool,
}

fn app_spec(editor: Editor) -> Option<&'static EditorApp> {
    APPS.iter().find(|app| app.editor == editor)
}

/// Ruta del `.app`: `/Applications`, `~/Applications` y, si no, Spotlight.
fn locate_app(app: &EditorApp) -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    for name in app.app_names {
        let mut dirs = vec![PathBuf::from("/Applications")];
        if let Some(home) = &home {
            dirs.push(home.join("Applications"));
        }
        if let Some(found) = dirs.into_iter().map(|d| d.join(name)).find(|p| p.is_dir()) {
            return Some(found);
        }
    }
    for id in app.bundle_ids {
        let output = Command::new("/usr/bin/mdfind")
            .arg(format!("kMDItemCFBundleIdentifier == '{id}'"))
            .stderr(Stdio::null())
            .output()
            .ok()?;
        let text = String::from_utf8_lossy(&output.stdout);
        if let Some(line) = text.lines().map(str::trim).find(|l| l.ends_with(".app")) {
            return Some(PathBuf::from(line));
        }
    }
    None
}

/// Programa y argumentos a ejecutar. Pura, para poder probarla.
fn launch_plan(
    editor: Editor,
    app_path: Option<&Path>,
    target: &Path,
    line: Option<u32>,
    is_dir: bool,
) -> NudeResult<(PathBuf, Vec<String>)> {
    let target_str = target.to_string_lossy().into_owned();
    if editor == Editor::Finder {
        let mut args = Vec::new();
        if !is_dir {
            args.push("-R".to_owned()); // muestra el archivo seleccionado
        }
        args.push(target_str);
        return Ok((PathBuf::from("/usr/bin/open"), args));
    }

    let app = app_spec(editor).ok_or_else(|| NudeError::NotFound("editor".into()))?;
    let app_path = app_path.ok_or_else(|| NudeError::NotFound(format!("{} no está instalado", app.name)))?;
    let cli = app_path.join(app.cli);
    let located = match line.filter(|_| !is_dir) {
        Some(line) => format!("{target_str}:{}", line.max(1)),
        None => target_str.clone(),
    };
    if cli.is_file() {
        let args = match editor {
            Editor::Zed => vec![located],
            // `-g` = ir a la línea; sin línea, abrir la carpeta/archivo tal cual.
            _ if line.is_some() && !is_dir => vec!["-g".to_owned(), located],
            _ => vec![target_str],
        };
        return Ok((cli, args));
    }
    Ok((
        PathBuf::from("/usr/bin/open"),
        vec!["-a".to_owned(), app_path.to_string_lossy().into_owned(), target_str],
    ))
}

/// Editores conocidos y si están instalados (Finder siempre lo está).
#[tauri::command]
pub async fn list_editors() -> NudeResult<Vec<EditorInfo>> {
    blocking(|| {
        let mut editors: Vec<EditorInfo> = APPS
            .iter()
            .map(|app| EditorInfo { id: app.editor, name: app.name, installed: locate_app(app).is_some() })
            .collect();
        editors.push(EditorInfo { id: Editor::Finder, name: "Finder", installed: true });
        Ok(editors)
    })
    .await
}

/// Abre `path` (archivo o carpeta, absoluto) en `editor`, en `line` si se da.
#[tauri::command]
pub async fn open_in_editor(path: String, line: Option<u32>, editor: Editor) -> NudeResult<()> {
    blocking(move || {
        let target = PathBuf::from(&path);
        if !target.is_absolute() {
            return Err(NudeError::GitOperation(format!("la ruta debe ser absoluta: {path}")));
        }
        let meta = std::fs::metadata(&target)
            .map_err(|_| NudeError::NotFound(format!("{path} ya no existe")))?;
        let app_path = app_spec(editor).and_then(locate_app);
        let (program, args) = launch_plan(editor, app_path.as_deref(), &target, line, meta.is_dir())?;

        let mut child = Command::new(&program)
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        // Las CLI vuelven enseguida; se recoge el proceso sin bloquear la UI.
        std::thread::spawn(move || {
            let _ = child.wait();
        });
        Ok(())
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_app(cli: Option<&str>) -> PathBuf {
        let app = std::env::temp_dir().join(format!("nude-editor-{}/Fake.app", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&app).expect("mkdir");
        if let Some(cli) = cli {
            let path = app.join(cli);
            std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
            std::fs::write(path, "#!/bin/sh\n").expect("cli");
        }
        app
    }

    #[test]
    fn finder_reveals_files_and_opens_folders() {
        let file = Path::new("/repo/src/main.rs");
        let (program, args) = launch_plan(Editor::Finder, None, file, Some(3), false).expect("plan");
        assert_eq!(program, PathBuf::from("/usr/bin/open"));
        assert_eq!(args, ["-R", "/repo/src/main.rs"]);
        let (_, args) = launch_plan(Editor::Finder, None, Path::new("/repo"), None, true).expect("plan");
        assert_eq!(args, ["/repo"]);
    }

    #[test]
    fn editors_use_their_bundled_cli_to_jump_to_a_line() {
        let file = Path::new("/repo/src/main.rs");
        let code = tmp_app(Some("Contents/Resources/app/bin/code"));
        let (program, args) = launch_plan(Editor::Vscode, Some(&code), file, Some(42), false).expect("plan");
        assert!(program.ends_with("Contents/Resources/app/bin/code"));
        assert_eq!(args, ["-g", "/repo/src/main.rs:42"]);

        let zed = tmp_app(Some("Contents/MacOS/cli"));
        let (_, args) = launch_plan(Editor::Zed, Some(&zed), file, Some(7), false).expect("plan");
        assert_eq!(args, ["/repo/src/main.rs:7"]);
        let (_, args) = launch_plan(Editor::Zed, Some(&zed), Path::new("/repo"), Some(7), true).expect("plan");
        assert_eq!(args, ["/repo"], "las carpetas no llevan línea");

        // Sin CLI en el bundle: `open -a` sin línea.
        let cursor = tmp_app(None);
        let (program, args) = launch_plan(Editor::Cursor, Some(&cursor), file, Some(1), false).expect("plan");
        assert_eq!(program, PathBuf::from("/usr/bin/open"));
        assert_eq!(args[0], "-a");
        assert_eq!(args[2], "/repo/src/main.rs");

        assert!(launch_plan(Editor::Cursor, None, file, None, false).is_err(), "no instalado");
        for app in [code, zed, cursor] {
            let _ = std::fs::remove_dir_all(app.parent().expect("parent"));
        }
    }
}
