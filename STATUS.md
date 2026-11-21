# Estado del proyecto Nude

> Checklist vivo. Cada agente lo actualiza al terminar una tarea (ver AGENTS.md §5).
> Última actualización: 2026-11-21 — Correcciones de Fase 6: ⌘K y diagnóstico del watcher.

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

## Fase 5 — Rediseño visual neutro ✅

Solo `src/`; `src-tauri/` sin cambios (IPC, eventos y comandos intactos).

### Paleta y tipografía
- [x] Carbón neutro sin tintes: base `#121212`, sidebar/inspector `#161616`, superficies `#1C1C1C`, hover `#242424`, activo `#222222`, bordes `1px #262626`, separadores `#222222`
- [x] Texto `#EDEDED` / `#8A8A8A` / `#5C5C5C`, prompts `#A3A3A3`; monoespaciada `JetBrains Mono` → `SF Mono` → `Menlo` (se usa la instalada, sin descargar fuentes) en hashes, rutas, modelos, ramas y diffs
- [x] Sin gradientes, sombras ni acentos morados. Color solo en adiciones/eliminaciones del diff (y las letras `A`/`D`) y en el punto de 6 px de cada agente (Claude Code, Codex, OpenCode; manual en gris)
- [x] Grafo Git en escala de grises; avisos de seguridad neutros (icono + negrita), botón destructivo con texto rojo de "eliminación"

### Wordmark
- [x] `src/Wordmark.tsx`: "Nude" en tipografía modular de bloques (matriz 6×7 por letra, trazos de 2 módulos, contraformas cuadradas, `crispEdges`). Diseño propio en ese estilo, no una copia del logo de OpenCode
- [x] Bienvenida (sin proyectos): wordmark centrado en `#282828` + un único botón "Abrir carpeta" (Finder)
- [x] Marca de agua en el timeline vacío y versión pequeña en la cabecera del sidebar

### Paneles
- [x] Sidebar de 220 px colapsable a un riel de 44 px (botón o `⌘B`, preferencia guardada en `localStorage`); lista plana, activo con fondo `#222222`, rutas abreviadas con `~`
- [x] "Quitar proyecto" pasa a un menú `···` que aparece al pasar el cursor (o con foco) sobre cada proyecto; se eliminan los botones del pie
- [x] "Checkpoint" manual como botón compacto en la cabecera del timeline
- [x] Timeline en filas separadas por líneas de `1px`: punto de agente, agente, modelo y nº de archivos en mono, tiempo relativo; prompt en una línea limpia. "Rollback" aparece al pasar el cursor o en la fila seleccionada
- [x] `cleanPrompt` (`workspace.ts`) quita bloques y marcas de código, encabezados, énfasis, citas, viñetas, enlaces y saltos de línea reales o escapados (`\n`)
- [x] Inspector: cabecera "Cambios", lista con `M`/`A`/`D`, ruta del archivo en mono y visor de diff estilo GitHub con números de línea antiguos/nuevos; se ocultan las cabeceras `diff --git`/`index`/`---`/`+++`

### Verificación
- [x] `npm run build` sin errores
- [x] Revisado en navegador con el IPC simulado: timeline con prompts Markdown, alertas, diff, grafo, menú `···`, sidebar colapsado y pantalla de bienvenida
- [ ] Revisar en la app Tauri real (fuentes del sistema, selector de Finder)

## Pantalla inicial de proyectos ✅

- [x] Nude se abre sin proyecto seleccionado. El inicio muestra el wordmark geométrico de 288 px en `#2A2A2A` sobre `#121212` y la lista de proyectos con rutas truncadas en monoespaciado.
- [x] Desde el inicio se puede abrir un proyecto registrado, elegir una carpeta en Finder o pegar una ruta con el diálogo existente.
- [x] Abrir un proyecto muestra el espacio de trabajo de tres paneles. El wordmark del sidebar vuelve al inicio incluso con la barra colapsada.
- [x] `npm run build` sin errores. Verificados con IPC simulado el inicio, la apertura, el regreso en ambos estados de la barra lateral, el diálogo de ruta y la selección o cancelación en Finder.

## Fase 6 — Paleta de comandos, editores y firewall de archivos ✅

> Pedida como "Fase 5"; se numera 6 porque la Fase 5 ya era el rediseño visual.

### Paleta de comandos (`⌘K`)
- [x] `src/CommandPalette.tsx`: modal estilo Linear/OpenCode sobre `<dialog>`; `⌘K` abre/cierra en cualquier pantalla (salvo con un diálogo abierto), `Esc` o clic fuera cierran, `↑/↓` (y `⌃N/⌃P`) navegan, `↵` ejecuta. Búsqueda difusa (`fuzzyScore` en `workspace.ts`) con agrupación por sección cuando no hay texto
- [x] Comandos: Timeline/Grafo Git, checkpoint manual, deshacer último agente (consulta `preview_undo_last_agent` y abre la confirmación), firewall de archivos, abrir el archivo del inspector (en su primera línea cambiada) o el proyecto en cada editor instalado/Finder, cambiar de proyecto, agregar proyecto, barra lateral y volver al inicio
- [x] Atajo visible `⌘K` en la barra superior

### Editores
- [x] `commands/editor.rs`: `list_editors()` → `{ id, name, installed }[]` y `open_in_editor({ path, line, editor })` con `editor` = `cursor` | `vscode` | `zed` | `finder`
  - Localiza la app en `/Applications`, `~/Applications` o por bundle id con Spotlight (`mdfind`); usa la CLI del propio bundle (`code`/`cursor -g ruta:línea`, `zed ruta:línea`), sin depender del `PATH`. Sin CLI: `open -a <app>` (sin línea). Finder: `open -R` (archivo) u `open` (carpeta)
  - Exige ruta absoluta existente; solo procesos locales, sin red
- [x] Inspector de diffs: botón discreto junto a la ruta para abrir el archivo en la primera línea cambiada; desplegable para elegir editor (la preferencia se guarda en `localStorage`). Desactivado en archivos borrados

### Firewall de archivos (guardrails)
- [x] Migración v3: tabla `project_guardrails` (`project_id`, `pattern`, `action` `warn`|`auto_revert`, `UNIQUE(project_id, pattern)`, `ON DELETE CASCADE`)
- [x] `safety.rs`: `normalize_guardrail_pattern` (quita `./` y `/`, `dir/` → `dir/**`, rechaza `..`, `.git`, vacíos) y `guardrail_matches` (estilo `.gitignore`: `*`/`?` no cruzan carpetas, `**` sí; sin `/` vale en cualquier nivel)
- [x] Comandos `list_guardrails({ projectId })`, `set_guardrail({ projectId, pattern, action })` (crea o cambia la acción) y `remove_guardrail({ guardrailId })`; avisan al watcher para rehacer la copia de los ignorados
- [x] `watcher/guardrails.rs`: el watcher revisa las rutas protegidas con un debounce propio de 250 ms (tope 1 s), siempre **antes** de crear el checkpoint, bajo `CAPTURE_LOCK`
  - Solo actúa si hay una sesión de agente escrita en los últimos 2 min (`detect_session_within`); los cambios del usuario no se bloquean
  - Referencia = árbol del último checkpoint (o `HEAD`). Archivos ignorados por Git (`.env*`): copia **solo en memoria** (≤ 1 MiB, ≤ 256 archivos), tomada al vigilar el proyecto y al cambiar las reglas, y actualizada con los cambios del usuario. Nunca se guarda en disco, SQLite ni `.git`
  - `auto_revert`: reescribe el archivo, lo recrea si se borró o lo elimina si el agente lo creó (`git/guard.rs`: `restore_path_from_baseline`, sin tocar `HEAD`, ramas ni `.git/index`); si se borra una carpeta entera se expanden sus archivos protegidos
  - Evento `guardrail-triggered` (`{ projectId, projectName, agentTool, modelName, detectedAt, violations[] }`) y notificación nativa con `tauri-plugin-notification` (una por ráfaga). No repite el aviso para el mismo contenido
- [x] Cabecera del proyecto: botón "Firewall" con contador y popover (lista de reglas con conmutador Avisar/Revertir, quitar, añadir patrón, sugerencias `.env*`, `migrations/**`, `Cargo.lock`, `package-lock.json`, `*.pem`)
- [x] Avisos en vivo arriba a la derecha con agente, archivos y si se restauraron; clic = abrir el proyecto

### Verificación
- [x] `cargo check --manifest-path src-tauri/Cargo.toml` sin errores ni avisos
- [x] `cargo test --lib` — 57 pruebas (glob y normalización, CRUD + cascada de guardrails, restauración de un archivo contra `HEAD`/checkpoint, ignorados, auto-revert de tracked/nuevos/carpetas borradas, `.env` desde la copia en memoria, modo aviso sin repetir, cambios del usuario no bloqueados, planes de lanzamiento de editores y **watcher real** con sesión de Claude simulada: el aviso llega antes del debounce, el archivo se restaura y el checkpoint posterior solo contiene lo permitido)
- [x] `npm run build` sin errores
- [x] Revisado en navegador con IPC simulado: `⌘K`, filtro, ejecución (`open_in_editor` con ruta y línea), popover del firewall y aviso en vivo
- [ ] Probar en la app Tauri real: notificación de macOS (permiso del sistema), apertura en Cursor/VS Code/Zed instalados y auto-revert con un agente real

### Notas
- La atribución "es del agente" se basa en la actividad reciente de su historial (2 min). Si editas a mano un archivo protegido mientras un agente está trabajando, con `auto_revert` también se revierte.
- Un `git checkout`/`pull` desde la terminal con un agente activo puede revertir archivos protegidos que cambie (Nude no ve las operaciones Git, el watcher ignora `.git/`). Los cambios de rama hechos desde Nude no se ven afectados.
- Los archivos ignorados protegidos que superan 1 MiB, o los que viven dentro de carpetas ignoradas enteras (`node_modules/`), solo generan aviso.
- La copia en memoria de los ignorados se pierde al cerrar Nude y se rehace al abrir.

## Fase 6.1 — Correcciones tras la prueba en `Nude.app` ✅

### Diagnóstico (con los datos reales, 2026-11-21)
- **⌘K no abría nada porque la app instalada no tenía la Fase 6.** El commit `c73bd` de `main` ("fase 6 …") solo añade `logo-macos.png` y un archivo vacío llamado `=`; el código de la fase vive en la rama `claude/nude-phase-5-commands-guardrails-07f074` sin commitear. El binario de `/Applications/Nude.app` no contiene `open_in_editor` ni `project_guardrails`, y `nude.db` sigue en `user_version = 2` (sin la migración v3).
- **El watcher sí funcionaba**: registró el checkpoint de `logo-macos.png` y `=` (14:54) en `nude`. Los agentes trabajaron hoy en el worktree `.claude/worktrees/nude-phase-5-…`, que está en `.git/info/exclude`: sus cambios no tocan el árbol de `nude`, así que no había nada que capturar ahí. Tampoco hubo sesiones de Codex hoy (`~/.codex/sessions/2026/11/21` no existe).
- La ventana de atribución **nunca bajó de 15 min**: los 2 min solo se aplicaban al firewall. Ahora es explícito en el código, en los tests y en la UI.

### ⌘K
- [x] Listener global en **fase de captura** (`{ capture: true }`) con `preventDefault()` + `stopImmediatePropagation()`; ignora ⌘K con un diálogo de acción abierto
- [x] Menú nativo = menú por defecto de macOS + **Ir › Paleta de comandos… `⌘K`** (`CmdOrCtrl+K`), que emite `open-command-palette`. Si llegan la tecla y el acelerador, el segundo se descarta (400 ms)
- [x] `lib.rs` no tenía menú propio; el menú por defecto de Tauri no usa ⌘K

### Watcher
- [x] `watcher/status.rs`: estado por proyecto (`starting` | `active` | `error` | `stopped`, error exacto, vigilando desde, último evento, última revisión, último checkpoint). Evento `watcher-status` cuando cambia el estado o el error
- [x] Al arrancar se vigilan **todos** los proyectos de SQLite en el hilo del watcher. Antes de vigilar se comprueba que macOS deja leer la carpeta; un `PermissionDenied` da un mensaje que explica cómo conceder el permiso. Si falla `notify`, la carpeta no existe o una revisión falla, el error se guarda y se ve en la UI
- [x] Los proyectos que fallan se reintentan cada 30 s. Si `notify` no arranca, el hilo sigue vivo para el escaneo forzado
- [x] `force_scan({ projectId })` → `{ checkpoint, status }`: reintenta la vigilancia, pasa el firewall y revisa el árbol al momento, sin esperar a `notify` ni al debounce
- [x] `get_watcher_status({ projectId })` y `get_watcher_diagnostics({ projectId })` → estado, última sesión de agente de los últimos 7 días (`cwd`, hora, si aún es atribuible) y worktrees enlazados del repo con su rama, su última sesión y si ya son proyecto

### Sesiones de agentes (`watcher/sessions.rs`)
- [x] Ventana parametrizada: `detect_active_session` = 15 min (checkpoints) y `detect_session_within(…, AGENT_WINDOW = 2 min)` solo para el firewall
- [x] `cwd` normalizado (`canonicalize` + sin `/` final) al comparar Codex, OpenCode y Claude; se prueba la raíz tal cual y canónica
- [x] Claude: si la carpeta codificada de `~/.claude/projects` no coincide (symlink, subcarpeta, `/` final), busca entre las sesiones activas la que declare un `cwd` dentro del proyecto
- [x] Un `cwd` dentro de un repo o worktree anidado (con su propio `.git`, como `.claude/worktrees/<tarea>`) ya no se atribuye al proyecto padre
- [x] `DetectedSession.cwd` con la carpeta de trabajo real del agente

### UI
- [x] El indicador junto al nombre del proyecto es un botón: `en vivo`, `conectando`, `sin conexión` o **`sin vigilancia`** en rojo si hay error
- [x] Su popover muestra el estado (`Activo` o el error exacto), las horas, la última sesión de agente (`cwd` y hora), las ventanas 15/2 min, los **worktrees sin vigilar** con botón "Vigilar" (los da de alta como proyecto) y **"Forzar escaneo ahora"**

### Verificación
- [x] `cargo check --manifest-path src-tauri/Cargo.toml` sin errores ni avisos
- [x] `cargo test --lib` — 61 pruebas (nuevas: ventana 15 min frente a 2 min, `cwd` con symlink, `/` final y subcarpeta, plan B de Claude por `cwd`, worktree anidado, estado de error exacto con carpeta inexistente, escaneo forzado sin esperar al debounce y reintento al volver la carpeta)
- [x] `npm run build` sin errores
- [x] Revisado con IPC simulado: popover de diagnóstico, escaneo forzado y ⌘K (tecla, evento nativo, duplicado descartado)
- [ ] Reconstruir `Nude.app` **desde esta rama** y probar ⌘K, el menú Ir y el diagnóstico en la app real

### Notas
- Para registrar el trabajo de un agente en un worktree, hay que vigilar ese worktree como proyecto (botón "Vigilar" del diagnóstico). Sus checkpoints usan las mismas refs internas `refs/nude/checkpoints/…`, compartidas por todos los worktrees del repo.
