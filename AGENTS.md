# AGENTS.md — Proyecto Nude

## 1. Visión del Producto
**Nude** es una aplicación de escritorio 100% local (sin servidores web ni puertos expuestos) desarrollada con **Tauri v2 (Rust + React/TypeScript)** para macOS. Su propósito es ofrecer observabilidad, trazabilidad y control de versiones semántico sobre el trabajo que realizan múltiples agentes de IA (Claude Code, OpenAI Codex CLI, OpenCode) dentro de repositorios Git locales.

## 2. Arquitectura y Límites Estrictos (Invariantes)
- **Local-First & Zero-Network:** Prohibido levantar servidores HTTP locales (`localhost:xxxx`) o hacer peticiones externas. Toda comunicación entre UI y Core ocurre exclusivamente vía **Tauri IPC (`#[tauri::command]` y eventos `app.emit`)**.
- **Separación de Responsabilidades:**
  - `src-tauri/src/` (Rust): Único responsable de operaciones de sistema de archivos (`notify`), lectura/escritura de repositorios Git (`git2`), persistencia en SQLite (`rusqlite`) y parsing de logs locales de agentes (`~/.claude`, `~/.codex`, `~/.opencode`).
  - `src/` (React + TypeScript): Capa puramente visual y de estado de UI. Prohibido ejecutar lógica de negocio o parsing pesado en el frontend.
- **No Ensuciar el Historial Git del Usuario:** Los puntos de restauración automáticos creados por Nude deben almacenarse en referencias internas (`refs/nude/checkpoints/<session_id>/<timestamp>`) o base de datos local, nunca creando commits no solicitados en la rama activa (`main`/`dev`) del usuario. Las únicas operaciones que tocan `HEAD`, ramas o `.git/index` están en `src-tauri/src/git/branches.rs` y `src-tauri/src/git/worktrees.rs` (carriles paralelos: su rama temporal `nude/lane-*` y el merge `--no-ff` que pide el usuario) y solo se ejecutan cuando el usuario las pide desde la UI.

## 3. Esquema Base de Datos (SQLite - `nude.db`)
Ubicación: `~/Library/Application Support/com.nude.app/nude.db`
- `projects`: `id`, `name`, `root_path` (UNIQUE), `created_at`
- `agent_sessions`: `id`, `project_id`, `agent_tool` (`claude-code` | `codex` | `opencode` | `manual`), `model_name`, `external_id` (id de la sesión en el historial del agente; migración v2), `started_at`, `ended_at`
- `checkpoints`: `id`, `session_id`, `project_id`, `git_ref`, `prompt_summary`, `files_changed_count`, `created_at`
- `file_diffs`: `id`, `checkpoint_id`, `file_path`, `change_type` (`added` | `modified` | `deleted`), `patch_text`
- `project_guardrails`: `id`, `project_id`, `pattern` (glob relativo a la raíz, único por proyecto), `action` (`warn` | `auto_revert`), `created_at` (migración v3)
- `agent_lanes`: `id`, `project_id`, `name`, `branch_name` (`nude/lane-<slug>`), `worktree_path` (`~/Library/Application Support/com.nude.app/worktrees/<project_id>/<slug>`), `agent_hint`, `status` (`active` | `merged` | `discarded`), `created_at` (ms Unix) (migración v5). Cada carril activo tiene además una fila interna en `projects` con `root_path = worktree_path` (oculta en `list_projects`).

## 4. Convenciones de Código
- **Rust:** Usar `thiserror` o `Result<T, String>` limpio en comandos Tauri; cero `.unwrap()` en rutas de producción que puedan cerrar la app. Modularizar en:
  - `src-tauri/src/db/` (migraciones y queries SQLite)
  - `src-tauri/src/git/` (diffs, refs internas y rollback quirúrgico)
  - `src-tauri/src/watcher/` (observador de archivos + detector de sesiones de agentes)
  - `src-tauri/src/commands/` (comandos IPC expuestos al frontend)
  - `src-tauri/src/safety.rs` (alertas de archivos sensibles y bucles de agentes)
  - `src-tauri/src/pty/` (terminales PTY de los carriles paralelos: `portable-pty`, shell de login del usuario)
- **Frontend:** Componentes funcionales en TypeScript estricto (`strict: true`), Tailwind CSS, interfaz oscura minimalista estilo macOS nativo.

## 5. Flujo de Trabajo entre Agentes
1. Antes de modificar cualquier archivo, lee este `AGENTS.md` y `STATUS.md` (checklist de estado) para ver qué módulos ya existen. Actualiza `STATUS.md` al terminar.
2. No modifiques el esquema SQLite sin actualizar las migraciones en `src-tauri/src/db/mod.rs`.
3. Tras cada cambio en Rust, valida compilación con `cargo check --manifest-path src-tauri/Cargo.toml`.