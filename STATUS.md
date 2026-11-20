# Estado del proyecto Nude

> Checklist vivo. Cada agente lo actualiza al terminar una tarea (ver AGENTS.md §5).
> Última actualización: 2026-11-20 — Persistencia entre actualizaciones e identidad de instalación en macOS.

## Fase 1 — Backend base (Rust) ✅

### `src-tauri/src/db/` — SQLite
- [x] `Database::open_in_app_dir()` crea el directorio de datos de Tauri y abre `nude.db`
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

## Fase 2 — Watcher, sesiones de agentes y rollback ✅

### `src-tauri/src/watcher/`
- [x] `watcher/mod.rs`: hilo propio con `notify` (recursivo) sobre todos los proyectos de SQLite; debounce de 1,5 s por proyecto (con tope de 15 s si no deja de cambiar); evaluación inicial al arrancar por si hubo cambios con la app cerrada
- [x] `watcher/filter.rs`: ignora `.git/`, `node_modules/`, `target/` (relativos a la raíz) y temporales (`*~`, `.swp/.swo`, `.tmp`, `.part`, `.DS_Store`, `.#*`, `4913`…)
- [x] `watcher/sessions.rs`: detector de sesión activa (escrita en los últimos 15 min) → `model_name` + último prompt (≤240 car.); si no hay ninguna → `manual`
  - Claude Code: `~/.claude/projects/<cwd con no-alfanuméricos → "-">/*.jsonl`
  - Codex: `~/.codex/sessions/AAAA/MM/DD/rollout-*.jsonl`, emparejado por `cwd`
  - OpenCode: `~/.local/share/opencode/opencode.db` (SQLite, solo lectura) con fallback al formato antiguo `storage/*.json`
- [x] `watcher/capture.rs`: `capture_if_changed` compara el árbol de trabajo con el del último checkpoint (o `HEAD` si no hay) y solo entonces crea ref + checkpoint + `file_diffs`; los diffs son respecto al checkpoint anterior; `CAPTURE_LOCK` serializa watcher y comandos
- [x] Evento Tauri `checkpoint-created` (payload: `Checkpoint` en camelCase)

### `src-tauri/src/git/`
- [x] `working_tree_id`, `head_tree_id`, `checkpoint_tree_id` (detección de cambios reales sin crear commits)
- [x] `create_snapshot(..., diff_base_ref)` calcula `files` contra un checkpoint previo
- [x] `restore_working_tree(root, git_ref)`: deja el working tree idéntico al de la ref (reescribe, recrea, borra y poda directorios vacíos; respeta ignorados; no toca `HEAD`, ramas ni `.git/index`); solo acepta refs bajo `refs/nude/checkpoints/`

### `src-tauri/src/db/`
- [x] Migración v2: `agent_sessions.external_id` + índice único parcial
- [x] `get_or_create_session` (reutiliza por id externo; al abrir una nueva cierra las anteriores del proyecto), `get_checkpoint`, `latest_checkpoint`, `list_file_diffs`

### `src-tauri/src/commands/`
- [x] `get_checkpoint_diffs({ checkpointId })` → `FileDiff[]` (`filePath`, `changeType`, `patchText`)
- [x] `rollback_checkpoint({ checkpointId })` → `{ safetyCheckpoint, restoredCheckpoint, filesRestored }`: guarda antes el estado actual como checkpoint de seguridad (el rollback es reversible) y registra el estado restaurado; ambos emiten `checkpoint-created`
- [x] `add_project` registra el proyecto en el watcher al momento

### Verificación
- [x] `cargo check` sin errores ni avisos
- [x] `cargo test --lib` — 27 tests (incluye watcher real con ráfaga de escrituras → 1 checkpoint, sesiones de los 3 agentes, restore exacto, rollback + deshacer)

## Notas y limitaciones conocidas
- El rollback no reescribe `.git/index`: tras restaurar, `git status` muestra la diferencia contra el índice como cambios normales. Los ficheros ignorados por `.gitignore` no se capturan ni se restauran.
- Los submódulos se ignoran en el restore. Parches > 512 KiB y binarios se guardan con `patch_text = NULL`.
- Las sesiones de los agentes se leen solo por mtime/cwd; si dos agentes escriben a la vez en el mismo proyecto gana el más reciente.
- Los cambios de un proyecto cuya carpeta desaparece se omiten. El registro se puede quitar desde la barra lateral.
- El inspector muestra diffs de checkpoints guardados. El comando para `working_tree_diff` en vivo sigue pendiente.

## Fase 3 — Interfaz visual nativa ✅

- [x] Barra lateral de proyectos con alta por ruta absoluta, baja con confirmación y checkpoint manual con resumen.
- [x] Timeline del más reciente al más antiguo, con actualización mediante `checkpoint-created`, insignia de agente, modelo, resumen, cantidad de archivos, fecha relativa y confirmación de rollback.
- [x] Inspector con lista de altas, modificaciones y bajas, y parche unificado con adiciones en verde y eliminaciones en rojo.
- [x] `create_manual_checkpoint({ projectId, summary })` crea una ref interna, registra un checkpoint de tipo `manual` y emite el evento, incluso cuando el árbol coincide con el último checkpoint.
- [x] `remove_project({ projectId })` cancela el debounce y la vigilancia, y borra el historial asociado de SQLite. Conserva los archivos del repositorio y las refs internas existentes.
- [x] Los checkpoints consultan el agente y el último modelo registrado en su sesión, incluidos los checkpoints de bases de datos existentes.
- [x] Tailwind y recursos locales empaquetados. `npm run dev` usa Tauri con `--no-dev-server`.
- [x] `cargo check --manifest-path src-tauri/Cargo.toml` y `npm run build` sin errores.
- [x] 29 pruebas Rust aprobadas, incluidas la baja del watcher, la captura manual y la preservación de archivos ignorados durante un rollback.
- [x] Prueba en la app Tauri empaquetada con SQLite temporal. Se verificó el alta por ruta y la suscripción del timeline.
- [ ] Completar la prueba visual de captura manual, inspector, rollback y baja en la app abierta. El backend de esos flujos pasó las pruebas Rust.

## Fase 4 — Herramienta visual para devs nuevos y vibe coders ✅

### Selector nativo y empaquetado macOS
- [x] `tauri-plugin-dialog` (Rust) + `@tauri-apps/plugin-dialog` (npm); permiso `dialog:allow-open` en `capabilities/default.json`
- [x] "Agregar proyecto" (`+` y estado vacío) abre el selector de carpetas de Finder; se mantiene "Pegar ruta" (icono junto al `+` y dentro del diálogo, que también tiene "Elegir carpeta en Finder…"). Si el selector falla, cae al diálogo de ruta
- [x] `tauri.conf.json`: iconos verificados en `src-tauri/icons/` (32, 128, 256 px, `icon.icns`, `icon.ico`); `category: DeveloperTool`, `macOS.minimumSystemVersion: 11.0`

### Grafo Git — backend (`src-tauri/src/git/`)
- [x] `history.rs` (solo lectura): `read_history` → `HEAD` (rama/separado/sin commits), ramas locales con upstream, últimos 50 commits de todas las ramas en orden topológico con `parentIds`, autor, fecha y mensaje, y estado `staged`/`unstaged`/`conflicted`; detecta merge/rebase en curso
- [x] `branches.rs` (acciones explícitas del usuario, nunca automáticas):
  - `create_branch` valida el nombre y que no exista; desde `HEAD` o desde un commit
  - `checkout_branch` es seguro (como `git switch`): conserva cambios locales que no chocan y, si alguno chocaría, falla sin tocar nada
  - `quick_commit` hace commit con **exactamente** los archivos elegidos (árbol = `HEAD` + selección); lo ya preparado y no elegido sigue preparado. Pide `user.name`/`user.email` si faltan
  - Todo se niega durante un merge/rebase en curso
- [x] `restore_working_tree_to_base`, `checkpoint_base_commit(s)`, `checkpoint_base_tree_id`
- [x] Comandos (`commands/git.rs`): `get_git_graph({ projectId })` (historial + checkpoints anclados a su commit base), `git_create_branch({ projectId, name, fromCommit?, checkout })`, `git_checkout_branch({ projectId, name })`, `git_quick_commit({ projectId, message, paths })`
- [x] Crear+cambiar y cambiar de rama van envueltos en `watcher::guarded_change`: checkpoint `manual` antes y después, bajo `CAPTURE_LOCK`, para que nada se pierda ni se atribuya al agente activo

### Grafo Git — frontend
- [x] Barra superior con conmutador **Timeline de Agentes / Grafo Git**
- [x] `src/GitGraphView.tsx`: grafo SVG estilo metro (carriles por rama, diagonales en bifurcaciones y fusiones, anillo en `HEAD`, nodo discontinuo "Cambios sin commit"), etiquetas de rama (clic = cambiar), chips de checkpoints de Nude por commit (clic = abre ese checkpoint en el timeline) y botón "Crear rama aquí" por commit
- [x] Panel lateral: ramas locales con "Cambiar", lista de cambios con casillas y commit visual; botón "Rama de prueba" (nombre sugerido `prueba/agente-MMDD-HHmm`)
- [x] Se refresca al crearse checkpoints y al recuperar el foco de la ventana (por si se usó la terminal)

### Asistente de seguridad
- [x] **Deshacer último agente** (cabecera): `preview_undo_last_agent` / `undo_last_agent({ projectId })` vuelve al checkpoint inmediatamente anterior a la primera captura de la última sesión de IA; si no existe, al commit base sobre el que empezó. Guarda antes un checkpoint de seguridad (reversible), los checkpoints que genera son `manual` y el botón se desactiva si ya está deshecho
- [x] **Alertas** (`src-tauri/src/safety.rs`, comando `get_checkpoint_alerts({ checkpointId })`): archivos sensibles (secretos `.env*`/claves; dependencias `Cargo.toml`, `package.json`, lockfiles…; migraciones; despliegue/CI) y bucles (mismo archivo en más de 4 checkpoints seguidos de la misma sesión). El inspector muestra un resumen arriba, un icono en cada archivo afectado y una insignia junto al parche

### Verificación
- [x] `cargo check --manifest-path src-tauri/Cargo.toml` sin errores ni avisos
- [x] `cargo test --lib` — 43 tests (historial, ramas, checkout seguro con y sin conflicto, commit selectivo, restore al commit base, deshacer agente con/sin checkpoint previo e idempotente, alertas sensibles y de bucle)
- [x] `npm run build` sin errores
- [x] UI revisada en navegador con el IPC simulado (grafo con fusión y rama sin fusionar, diálogos de rama y de deshacer, alertas del inspector, chip → timeline)
- [ ] Probar en la app Tauri real: selector de Finder, checkout/commit sobre un repo de verdad y "Deshacer último agente" con una sesión real

### Notas
- El grafo muestra los últimos 50 commits; las líneas que siguen más allá llegan al borde inferior.
- Los cambios hechos con Git desde la terminal no generan eventos (el watcher ignora `.git/`): el grafo se actualiza al volver el foco a la ventana o con el botón de refrescar.

## Persistencia entre actualizaciones en macOS ✅

- [x] **Nude** es el nombre del paquete y de la ventana principal. `com.nude.app` es el identificador estable de las instalaciones existentes.
- [x] La base se guarda en `~/Library/Application Support/com.nude.app/nude.db` vía `app_data_dir`, independientemente de dónde se instale el binario o qué versión se ejecute.
- [x] Las tablas e índices se crean con `IF NOT EXISTS`. La lectura de `user_version` y las migraciones pendientes se ejecutan dentro de una transacción SQLite.
- [x] La revisión del árbol de trabajo y del historial de Git solo encontró la ruta persistente como ubicación de `nude.db`.
- [x] La prueba de actualización y reapertura conservó proyectos, sesiones, checkpoints y diffs de bases con el esquema inicial, con y sin versión registrada.
- [x] `cargo check --manifest-path src-tauri/Cargo.toml`, `cargo test --lib` (44 pruebas) y `npm run build` pasaron.
