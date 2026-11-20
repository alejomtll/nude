# Estado del proyecto Nude

> Checklist vivo. Cada agente lo actualiza al terminar una tarea (ver AGENTS.md §5).
> Última actualización: 2026-11-20 — Fase 1 backend.

## Fase 1 — Backend base (Rust) ✅

### `src-tauri/src/db/` — SQLite
- [x] `Database::open()` crea `~/Library/Application Support/com.nude.app/nude.db` (directorio incluido)
- [x] PRAGMAs: `foreign_keys = ON`, `journal_mode = WAL`, `busy_timeout = 5s`
- [x] Migraciones versionadas con `PRAGMA user_version` (`db/migrations.rs`, v1)
- [x] Tablas `projects`, `agent_sessions`, `checkpoints`, `file_diffs` + índices y `CHECK` de enums
- [x] Queries: `upsert_project`, `list_projects`, `get_project`, `list_checkpoints`
- [x] Queries listas para Fase 2 (sin uso aún): `create_session`, `insert_checkpoint` (transaccional, con `file_diffs`)

### `src-tauri/src/git/` — git2
- [x] `register_repository(path)` — valida repo no-bare, acepta subcarpetas, devuelve raíz canónica
- [x] `working_tree_diff(root)` — diff working tree + untracked (sin ignorados) vs `HEAD`; soporta rama sin commits
- [x] `create_snapshot(root, session_id, message)` — commit colgante en `refs/nude/checkpoints/<session_id>/<timestamp>`
  - no mueve `HEAD` ni la rama, no escribe `.git/index` (índice en memoria), no toca el working tree
  - valida `session_id` como componente de ref seguro

### `src-tauri/src/commands/` — IPC
- [x] `add_project({ path })` → `{ project, created }` (idempotente por `root_path`)
- [x] `list_projects()` → `Project[]`
- [x] `get_project_checkpoints({ projectId })` → `Checkpoint[]` (más reciente primero)
- [x] Comandos `async` + `spawn_blocking`; errores serializados como `string`

### Verificación
- [x] `cargo check --manifest-path src-tauri/Cargo.toml` sin errores
- [x] `cargo test --lib` — 10 tests (esquema, upsert, checkpoints, diff, snapshot sin efectos sobre HEAD/index/status, rama sin nacer, ids inseguros)

### Cambios de configuración
- `tauri.conf.json`: `identifier` → `com.nude.app` (para que la ruta de datos coincida con AGENTS.md §3)
- `Cargo.toml`: `uuid` con feature `v4`; nueva dependencia `thiserror = "2"`

## Fase 2 — Watcher y sesiones de agentes ⏳
- [ ] `src-tauri/src/watcher/` con `notify` (la dependencia ya está; `cargo` avisa de que aún no se usa)
- [ ] Detector de sesiones: `~/.claude`, `~/.codex`, `~/.opencode`
- [ ] Conectar `create_snapshot` + `insert_checkpoint` y quitar los `allow(dead_code)` de `git/mod.rs`
- [ ] Comandos: crear checkpoint manual, ver `file_diffs` de un checkpoint, diff actual del proyecto
- [ ] Eventos `app.emit` hacia la UI

## Pendiente / Fuera de alcance por ahora
- [ ] Rollback quirúrgico desde `refs/nude/checkpoints/...`
- [ ] Frontend (`src/`) — sin tocar; el comando `greet` de la plantilla sigue registrado
- [ ] Parches > 512 KiB y binarios se guardan con `patch_text = NULL` (`git/diff.rs::MAX_PATCH_BYTES`)
