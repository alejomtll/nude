//! Asistente de seguridad: avisos sobre lo que hizo un checkpoint.
//!
//! - **Archivos sensibles**: credenciales, dependencias/build, migraciones de
//!   base de datos y configuración de despliegue.
//! - **Bucles**: el mismo archivo editado en más de `LOOP_THRESHOLD`
//!   checkpoints seguidos de la misma sesión (síntoma típico de un agente que
//!   se ha atascado reintentando).

use std::collections::{HashMap, HashSet};
use std::path::Path;

use serde::Serialize;

use crate::db::Database;
use crate::error::{NudeError, NudeResult};

/// Se avisa cuando la racha de ediciones consecutivas supera este número.
pub const LOOP_THRESHOLD: usize = 4;

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum AlertKind {
    Sensitive,
    Loop,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum SensitiveCategory {
    Secrets,
    Dependencies,
    Migrations,
    Deployment,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FileAlert {
    pub file_path: String,
    pub kind: AlertKind,
    pub category: Option<SensitiveCategory>,
    pub message: String,
    /// Para `Loop`: nº de checkpoints seguidos que editaron el archivo.
    pub edit_streak: Option<usize>,
}

const DEPENDENCY_FILES: &[&str] = &[
    "Cargo.toml", "Cargo.lock", "package.json", "package-lock.json", "pnpm-lock.yaml",
    "yarn.lock", "bun.lockb", "bun.lock", "requirements.txt", "pyproject.toml", "poetry.lock",
    "Pipfile", "Pipfile.lock", "go.mod", "go.sum", "Gemfile", "Gemfile.lock", "composer.json",
    "composer.lock", "pubspec.yaml", "Podfile", "build.gradle", "build.gradle.kts",
];

const DEPLOYMENT_FILES: &[&str] = &[
    "tauri.conf.json", "Dockerfile", "docker-compose.yml", "docker-compose.yaml",
    "vercel.json", "netlify.toml", "fly.toml", "Procfile",
];

/// Categoría y explicación si `path` es un archivo sensible.
pub fn sensitive_reason(path: &str) -> Option<(SensitiveCategory, &'static str)> {
    let p = Path::new(path);
    let name = p.file_name()?.to_str()?;
    let lower = name.to_ascii_lowercase();
    let ext = p.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase);
    let dirs: Vec<String> = p
        .parent()
        .map(|d| {
            d.components()
                .filter_map(|c| c.as_os_str().to_str())
                .map(str::to_ascii_lowercase)
                .collect()
        })
        .unwrap_or_default();

    let is_env = lower == ".env" || lower.starts_with(".env.") || lower.ends_with(".env");
    let is_key = matches!(ext.as_deref(), Some("pem" | "key" | "p12" | "pfx" | "keystore" | "jks"))
        || lower.starts_with("id_rsa")
        || lower.starts_with("id_ed25519")
        || (lower.starts_with("credentials") && lower.ends_with(".json"))
        || lower.starts_with("secrets.")
        || lower == ".npmrc"
        || lower == ".netrc";
    if is_env || is_key {
        return Some((SensitiveCategory::Secrets, "Puede contener credenciales o claves privadas"));
    }

    if DEPENDENCY_FILES.contains(&name) {
        return Some((
            SensitiveCategory::Dependencies,
            "Cambia dependencias o la configuración de compilación",
        ));
    }

    let in_migrations_dir = dirs
        .iter()
        .any(|d| matches!(d.as_str(), "migrations" | "migration" | "migrate" | "alembic"));
    if in_migrations_dir
        || lower.contains("migration")
        || lower == "schema.prisma"
        || lower == "schema.rb"
        || lower == "structure.sql"
    {
        return Some((SensitiveCategory::Migrations, "Modifica el esquema de la base de datos"));
    }

    let in_ci = dirs.windows(2).any(|w| w[0] == ".github" && w[1] == "workflows")
        || dirs.first().is_some_and(|d| d == ".circleci")
        || lower == ".gitlab-ci.yml";
    if in_ci || DEPLOYMENT_FILES.contains(&name) {
        return Some((SensitiveCategory::Deployment, "Afecta al empaquetado, CI o despliegue"));
    }
    None
}

/// Racha final de checkpoints consecutivos que tocaron cada archivo del
/// último elemento de `history` (cronológico, el último es el checkpoint
/// evaluado).
pub fn edit_streaks(history: &[(String, Vec<String>)]) -> HashMap<String, usize> {
    let mut streaks = HashMap::new();
    let Some((_, last_files)) = history.last() else {
        return streaks;
    };
    for file in last_files {
        let streak = history
            .iter()
            .rev()
            .take_while(|(_, files)| files.iter().any(|f| f == file))
            .count();
        streaks.insert(file.clone(), streak);
    }
    streaks
}

pub fn checkpoint_alerts(db: &Database, checkpoint_id: &str) -> NudeResult<Vec<FileAlert>> {
    if db.get_checkpoint(checkpoint_id)?.is_none() {
        return Err(NudeError::NotFound(format!("checkpoint {checkpoint_id}")));
    }
    let files = db.list_file_diffs(checkpoint_id)?;
    let history = db.session_file_history(checkpoint_id)?;
    let streaks = edit_streaks(&history);

    let mut alerts = Vec::new();
    let mut seen = HashSet::new();
    for file in &files {
        if !seen.insert(file.file_path.as_str()) {
            continue;
        }
        if let Some((category, message)) = sensitive_reason(&file.file_path) {
            alerts.push(FileAlert {
                file_path: file.file_path.clone(),
                kind: AlertKind::Sensitive,
                category: Some(category),
                message: message.to_owned(),
                edit_streak: None,
            });
        }
        if let Some(&streak) = streaks.get(&file.file_path) {
            if streak > LOOP_THRESHOLD {
                alerts.push(FileAlert {
                    file_path: file.file_path.clone(),
                    kind: AlertKind::Loop,
                    category: None,
                    message: format!(
                        "Editado en {streak} checkpoints seguidos de esta sesión: el agente puede estar en bucle"
                    ),
                    edit_streak: Some(streak),
                });
            }
        }
    }
    Ok(alerts)
}


// ------------------------------------------------------------ Guardrails

/// Normaliza el patrón de una regla del firewall: sin `./` ni `/` iniciales y
/// con `dir/` convertido en `dir/**`. Rechaza patrones vacíos, con `..` o que
/// apunten dentro de `.git`.
pub fn normalize_guardrail_pattern(pattern: &str) -> NudeResult<String> {
    let mut p = pattern.trim().replace('\\', "/");
    while let Some(rest) = p.strip_prefix("./") {
        p = rest.to_owned();
    }
    let p = p.trim_start_matches('/');
    let mut p = p.to_owned();
    if p.ends_with('/') {
        p.push_str("**");
    }
    let invalid = p.is_empty()
        || p.len() > 200
        || p.contains('\0')
        || p.split('/').any(|seg| seg.is_empty() || seg == ".." || seg == "." || seg == ".git");
    if invalid {
        return Err(NudeError::GitOperation(format!("patrón no válido: «{}»", pattern.trim())));
    }
    Ok(p)
}

/// `true` si la ruta relativa `rel_path` (separada por `/`) cae bajo `pattern`.
///
/// - `*` y `?` no cruzan `/`; `**` como segmento completo abarca 0 o más carpetas.
/// - Un patrón sin `/` se compara con cada componente de la ruta, como en
///   `.gitignore`: `.env*` protege `config/.env.local` y `migrations` todo lo
///   que haya dentro de cualquier carpeta `migrations`.
/// - Con `/`, el patrón se ancla a la raíz del proyecto (`migrations/**`).
pub fn guardrail_matches(pattern: &str, rel_path: &str) -> bool {
    let path: Vec<&str> = rel_path.split('/').filter(|s| !s.is_empty()).collect();
    if path.is_empty() {
        return false;
    }
    // `dir/` equivale a `dir/**` aunque el patrón no se haya normalizado.
    let owned;
    let pattern = if pattern.ends_with('/') {
        owned = format!("{pattern}**");
        owned.as_str()
    } else {
        pattern
    };
    if !pattern.contains('/') {
        return path.iter().any(|component| segment_matches(pattern.as_bytes(), component.as_bytes()));
    }
    let pat: Vec<&str> = pattern.split('/').filter(|s| !s.is_empty()).collect();
    segments_match(&pat, &path)
}

fn segments_match(pat: &[&str], path: &[&str]) -> bool {
    match pat.split_first() {
        None => path.is_empty(),
        Some((&"**", rest)) => (0..=path.len()).any(|skip| segments_match(rest, &path[skip..])),
        Some((first, rest)) => match path.split_first() {
            Some((component, path_rest)) => {
                segment_matches(first.as_bytes(), component.as_bytes()) && segments_match(rest, path_rest)
            }
            None => false,
        },
    }
}

/// Comodines `*` y `?` dentro de un solo segmento (sin `/`).
fn segment_matches(pat: &[u8], text: &[u8]) -> bool {
    let (mut p, mut t) = (0usize, 0usize);
    let mut star: Option<(usize, usize)> = None;
    while t < text.len() {
        if p < pat.len() && (pat[p] == b'?' || pat[p] == text[t]) {
            p += 1;
            t += 1;
        } else if p < pat.len() && pat[p] == b'*' {
            star = Some((p, t));
            p += 1;
        } else if let Some((sp, st)) = star {
            p = sp + 1;
            t = st + 1;
            star = Some((sp, st + 1));
        } else {
            return false;
        }
    }
    while p < pat.len() && pat[p] == b'*' {
        p += 1;
    }
    p == pat.len()
}

#[cfg(test)]
mod guardrail_tests {
    use super::*;

    #[test]
    fn glob_semantics_follow_gitignore_style_rules() {
        assert!(guardrail_matches(".env*", ".env"));
        assert!(guardrail_matches(".env*", "config/.env.local"));
        assert!(!guardrail_matches(".env*", "src/env.ts"));
        assert!(guardrail_matches("Cargo.lock", "src-tauri/Cargo.lock"));
        assert!(guardrail_matches("*.lock", "yarn.lock"));
        assert!(guardrail_matches("migrations", "db/migrations/001.sql"));

        assert!(guardrail_matches("migrations/**", "migrations/001_init.sql"));
        assert!(guardrail_matches("migrations/**", "migrations/a/b.sql"));
        assert!(guardrail_matches("migrations/", "migrations/a/b.sql"));
        assert!(!guardrail_matches("migrations/**", "db/migrations/001.sql"), "anclado a la raíz");
        assert!(guardrail_matches("**/migrations/**", "db/migrations/001.sql"));
        assert!(guardrail_matches("**/migrations/**", "migrations/001.sql"));
        assert!(guardrail_matches("src/*.rs", "src/main.rs"));
        assert!(!guardrail_matches("src/*.rs", "src/db/mod.rs"), "* no cruza carpetas");
        assert!(guardrail_matches("src/**/*.rs", "src/db/mod.rs"));
        assert!(guardrail_matches("file?.txt", "file1.txt"));
        assert!(!guardrail_matches("file?.txt", "file10.txt"));
    }

    #[test]
    fn patterns_are_normalized_and_validated() {
        assert_eq!(normalize_guardrail_pattern("  ./migrations/ ").expect("ok"), "migrations/**");
        assert_eq!(normalize_guardrail_pattern("/Cargo.lock").expect("ok"), "Cargo.lock");
        for bad in ["", "   ", "../secret", "a//b", ".git/config", "a/./b"] {
            assert!(normalize_guardrail_pattern(bad).is_err(), "{bad:?} debería rechazarse");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::models::{AgentTool, ChangeType, FileDiff};

    #[test]
    fn classifies_sensitive_files() {
        use SensitiveCategory::*;
        let cases = [
            (".env", Some(Secrets)),
            ("app/.env.local", Some(Secrets)),
            ("certs/server.pem", Some(Secrets)),
            ("Cargo.toml", Some(Dependencies)),
            ("src-tauri/Cargo.toml", Some(Dependencies)),
            ("web/package.json", Some(Dependencies)),
            ("package-lock.json", Some(Dependencies)),
            ("db/migrations/001_init.sql", Some(Migrations)),
            ("src-tauri/src/db/migrations.rs", Some(Migrations)),
            ("prisma/schema.prisma", Some(Migrations)),
            (".github/workflows/ci.yml", Some(Deployment)),
            ("src-tauri/tauri.conf.json", Some(Deployment)),
            ("src/App.tsx", None),
            ("README.md", None),
            ("src/environment.ts", None),
        ];
        for (path, expected) in cases {
            assert_eq!(sensitive_reason(path).map(|(c, _)| c), expected, "{path}");
        }
    }

    #[test]
    fn streak_counts_only_consecutive_trailing_edits() {
        let h = |rows: &[&[&str]]| -> Vec<(String, Vec<String>)> {
            rows.iter()
                .enumerate()
                .map(|(i, files)| (i.to_string(), files.iter().map(|f| f.to_string()).collect()))
                .collect()
        };
        let history = h(&[&["a"], &["a", "b"], &[], &["a", "b"], &["a"], &["a", "b"]]);
        let streaks = edit_streaks(&history);
        assert_eq!(streaks["a"], 3);
        assert_eq!(streaks["b"], 1);
        assert!(edit_streaks(&[]).is_empty());
    }

    #[test]
    fn alerts_flag_sensitive_files_and_loops_in_the_same_session() {
        let db = Database::open_in_memory().expect("db");
        let (project, _) = db.upsert_project("demo", "/tmp/demo").expect("p");
        let session = db
            .get_or_create_session(&project.id, AgentTool::ClaudeCode, Some("s"), None)
            .expect("s");
        let diff = |path: &str| FileDiff {
            file_path: path.into(),
            change_type: ChangeType::Modified,
            patch_text: None,
        };

        let mut last = None;
        for i in 0..5 {
            let mut files = vec![diff("src/lib.rs")];
            if i == 4 {
                files.push(diff(".env"));
            }
            last = Some(
                db.insert_checkpoint(&session.id, &project.id, &format!("refs/nude/checkpoints/s/{i}"), None, &files)
                    .expect("cp"),
            );
        }
        let last = last.expect("last");
        let alerts = checkpoint_alerts(&db, &last.id).expect("alerts");
        assert!(alerts.iter().any(|a| a.file_path == ".env" && a.kind == AlertKind::Sensitive));
        let loop_alert = alerts.iter().find(|a| a.kind == AlertKind::Loop).expect("loop");
        assert_eq!(loop_alert.file_path, "src/lib.rs");
        assert_eq!(loop_alert.edit_streak, Some(5));

        // Otra sesión no hereda la racha.
        let other = db
            .get_or_create_session(&project.id, AgentTool::Codex, Some("o"), None)
            .expect("o");
        let cp = db
            .insert_checkpoint(&other.id, &project.id, "refs/nude/checkpoints/o/0", None, &[diff("src/lib.rs")])
            .expect("cp");
        assert!(checkpoint_alerts(&db, &cp.id).expect("a").is_empty());
    }
}
