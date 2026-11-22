//! Carriles paralelos: un worktree aislado de Git por agente.
//!
//! Cada carril vive en `<base>/<slug>/` (dentro de los datos de Nude, nunca en
//! el proyecto) sobre una rama temporal `nude/lane-<slug>` creada desde el
//! `HEAD` del proyecto. El agente trabaja ahí sin tocar el árbol principal.
//!
//! Igual que `branches`, este módulo sí toca ramas y `HEAD`, pero solo cuando
//! el usuario lo pide desde la UI (AGENTS.md §2):
//! - crear/descartar un carril añade/borra **su** rama `nude/lane-*` y su
//!   entrada en `.git/worktrees/`; nunca el `HEAD`, el índice ni el árbol del
//!   repositorio principal;
//! - aplicar un carril crea un commit de merge (`--no-ff`) en la rama activa
//!   del proyecto tras comprobar en memoria que no hay conflictos y que el
//!   checkout no pisará cambios locales: si algo falla, el repo queda intacto.

use std::fs;
use std::path::{Path, PathBuf};

use git2::{
    BranchType, Commit, ErrorCode, FileFavor, Index, MergeOptions, Repository, Signature,
    WorktreeAddOptions, WorktreePruneOptions, build::CheckoutBuilder,
};
use serde::Serialize;

use super::branches::ensure_clean_state;
use super::diff::collect_file_diffs;
use super::repository::{open_worktree, path_to_string};
use super::snapshot::snapshot_tree;
use crate::db::models::FileDiff;
use crate::error::{NudeError, NudeResult};

/// Prefijo de las ramas temporales de los carriles.
pub const LANE_BRANCH_PREFIX: &str = "nude/lane-";
const MAX_SLUG_LEN: usize = 40;

/// Worktree recién creado para un carril.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreatedLane {
    pub slug: String,
    /// `nude/lane-<slug>`.
    pub branch: String,
    /// Ruta canónica del worktree.
    pub path: String,
}

/// Cambios del carril respecto al punto del que salió.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LaneDiff {
    /// Rama activa del proyecto principal (destino de "Aplicar"); `None` si
    /// `HEAD` está separado.
    pub base_branch: Option<String>,
    /// Ancestro común entre el carril y la rama principal.
    pub base_commit: String,
    /// Cambios del carril (commits + árbol de trabajo sin confirmar, incluidos
    /// archivos nuevos no ignorados) respecto a `base_commit`.
    pub files: Vec<FileDiff>,
    /// Rutas que chocarían al aplicarlo ahora sobre la rama principal.
    pub conflicts: Vec<String>,
}

/// Resultado de aplicar un carril a la rama principal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaneMerge {
    Merged { commit_id: String, branch: String, files: usize },
    /// No se tocó nada: hay conflictos y el usuario no confirmó resolverlos.
    Conflicts(Vec<String>),
}

/// `"Refactor de pagos"` → `"refactor-de-pagos"` (ASCII, `-`, ≤ 40 caracteres).
pub fn lane_slug(name: &str) -> String {
    let mut slug = String::new();
    for c in name.trim().chars().flat_map(char::to_lowercase) {
        let c = match c {
            'á' | 'à' | 'ä' | 'â' | 'ã' => 'a',
            'é' | 'è' | 'ë' | 'ê' => 'e',
            'í' | 'ì' | 'ï' | 'î' => 'i',
            'ó' | 'ò' | 'ö' | 'ô' | 'õ' => 'o',
            'ú' | 'ù' | 'ü' | 'û' => 'u',
            'ñ' => 'n',
            'ç' => 'c',
            c => c,
        };
        if c.is_ascii_alphanumeric() {
            slug.push(c);
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
        if slug.len() >= MAX_SLUG_LEN {
            break;
        }
    }
    let slug = slug.trim_end_matches('-');
    if slug.is_empty() { "carril".to_owned() } else { slug.to_owned() }
}

fn worktree_name(slug: &str) -> String {
    format!("nude-lane-{slug}")
}

fn head_commit(repo: &Repository) -> NudeResult<Commit<'_>> {
    match repo.head() {
        Ok(head) => Ok(head.peel_to_commit()?),
        Err(e) if matches!(e.code(), ErrorCode::UnbornBranch | ErrorCode::NotFound) => Err(
            NudeError::GitOperation("el repositorio no tiene commits todavía; haz el primero antes de abrir carriles".into()),
        ),
        Err(e) => Err(e.into()),
    }
}

/// Rama activa del repositorio principal (`None` si `HEAD` está separado).
fn head_branch(repo: &Repository) -> NudeResult<Option<String>> {
    let head = match repo.head() {
        Ok(head) => head,
        Err(e) if matches!(e.code(), ErrorCode::UnbornBranch | ErrorCode::NotFound) => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    if !head.is_branch() {
        return Ok(None);
    }
    Ok(head.shorthand().ok().map(str::to_owned))
}

/// Crea un worktree aislado en `base/<slug>` sobre una rama nueva
/// `nude/lane-<slug>` que parte del `HEAD` actual de `root`. Si el slug ya está
/// en uso (rama, carpeta o worktree) se añade `-2`, `-3`…
pub fn create_lane_worktree(root: &Path, base: &Path, name: &str) -> NudeResult<CreatedLane> {
    let repo = open_worktree(root)?;
    let head = head_commit(&repo)?;
    fs::create_dir_all(base)?;
    let base = fs::canonicalize(base)?;

    let stem = lane_slug(name);
    let slug = (1..1000)
        .map(|n| if n == 1 { stem.clone() } else { format!("{stem}-{n}") })
        .find(|slug| {
            repo.find_branch(&format!("{LANE_BRANCH_PREFIX}{slug}"), BranchType::Local).is_err()
                && repo.find_worktree(&worktree_name(slug)).is_err()
                && !base.join(slug).exists()
        })
        .ok_or_else(|| NudeError::GitOperation(format!("no queda ningún nombre libre para «{name}»")))?;
    let branch_name = format!("{LANE_BRANCH_PREFIX}{slug}");
    let path = base.join(&slug);

    let branch = repo.branch(&branch_name, &head, false)?;
    let added = {
        let reference = branch.get();
        let mut opts = WorktreeAddOptions::new();
        opts.reference(Some(reference));
        repo.worktree(&worktree_name(&slug), &path, Some(&opts))
    };
    if let Err(e) = added {
        // Deshace la rama para no dejar restos.
        drop(branch);
        if let Ok(mut branch) = repo.find_branch(&branch_name, BranchType::Local) {
            let _ = branch.delete();
        }
        let _ = fs::remove_dir_all(&path);
        return Err(e.into());
    }
    let path = fs::canonicalize(&path)?;
    Ok(CreatedLane { slug, branch: branch_name, path: path_to_string(&path)? })
}

/// Commit con el estado actual del carril (commits del agente + cambios sin
/// confirmar). Si no hay nada sin confirmar, el propio `HEAD` del carril. El
/// commit nuevo no mueve ninguna rama: solo quedará enlazado desde el merge.
fn lane_state_commit<'r>(lane: &'r Repository, lane_name: &str) -> NudeResult<Commit<'r>> {
    let head = head_commit(lane)?;
    let tree_id = snapshot_tree(lane)?;
    if tree_id == head.tree_id() {
        return Ok(head);
    }
    let tree = lane.find_tree(tree_id)?;
    let signature = lane.signature().or_else(|_| Signature::now("Nude", "nude@localhost"))?;
    let id = lane.commit(
        None,
        &signature,
        &signature,
        &format!("Cambios del carril «{lane_name}»"),
        &tree,
        &[&head],
    )?;
    Ok(lane.find_commit(id)?)
}

fn conflict_paths(index: &Index) -> NudeResult<Vec<String>> {
    let mut paths = Vec::new();
    for conflict in index.conflicts()? {
        let conflict = conflict?;
        let entry = conflict.our.or(conflict.their).or(conflict.ancestor);
        if let Some(entry) = entry {
            paths.push(String::from_utf8_lossy(&entry.path).into_owned());
        }
    }
    paths.sort();
    paths.dedup();
    Ok(paths)
}

/// Cambios del carril respecto al ancestro común con la rama principal y
/// conflictos que habría al aplicarlo ahora. Solo lectura (escribe objetos en
/// el ODB, no refs, índices ni archivos).
pub fn lane_diff(root: &Path, lane_path: &Path) -> NudeResult<LaneDiff> {
    let repo = open_worktree(root)?;
    let lane = open_worktree(lane_path)?;
    let main_head = head_commit(&repo)?;
    let lane_head = head_commit(&lane)?;
    let base_id = repo.merge_base(main_head.id(), lane_head.id())?;
    let base_tree = repo.find_commit(base_id)?.tree()?;

    let lane_tree = lane.find_tree(snapshot_tree(&lane)?)?;
    let diff = lane.diff_tree_to_tree(Some(&base_tree), Some(&lane_tree), None)?;
    let files = collect_file_diffs(&diff)?;

    let merged = repo.merge_trees(&base_tree, &main_head.tree()?, &lane_tree, None)?;
    Ok(LaneDiff {
        base_branch: head_branch(&repo)?,
        base_commit: base_id.to_string(),
        files,
        conflicts: conflict_paths(&merged)?,
    })
}

/// Resuelve los conflictos que queden quedándose con la versión del carril.
fn take_lane_side(index: &mut Index) -> NudeResult<()> {
    let conflicts: Vec<_> = index.conflicts()?.collect::<Result<_, _>>()?;
    for conflict in conflicts {
        let Some(path) = conflict
            .their
            .as_ref()
            .or(conflict.our.as_ref())
            .or(conflict.ancestor.as_ref())
            .map(|e| PathBuf::from(String::from_utf8_lossy(&e.path).into_owned()))
        else {
            continue;
        };
        index.conflict_remove(&path)?;
        if let Some(mut entry) = conflict.their {
            entry.flags &= !0x3000; // stage 0 (resuelto)
            index.add(&entry)?;
        }
    }
    Ok(())
}

/// Aplica el carril sobre la rama activa del proyecto con un commit de merge
/// (`--no-ff`): padres = `HEAD` del proyecto y el estado del carril.
///
/// Antes de escribir nada comprueba que el repo no esté a mitad de otra
/// operación, que `HEAD` sea una rama y que el merge en memoria no tenga
/// conflictos (salvo `resolve_conflicts`, que se queda con la versión del
/// carril). El checkout es "seguro": si pisaría cambios locales sin guardar,
/// falla sin tocar nada y `HEAD` no se mueve.
pub fn merge_lane(
    root: &Path,
    lane_path: &Path,
    lane_name: &str,
    lane_branch: &str,
    resolve_conflicts: bool,
) -> NudeResult<LaneMerge> {
    let repo = open_worktree(root)?;
    ensure_clean_state(&repo)?;
    let head = repo.head()?;
    if !head.is_branch() {
        return Err(NudeError::GitOperation(
            "el proyecto tiene HEAD separado; cambia a una rama antes de aplicar el carril".into(),
        ));
    }
    let branch_ref = head
        .name()
        .map_err(|_| NudeError::GitOperation("nombre de rama no UTF-8".into()))?
        .to_owned();
    let branch = head.shorthand().map(str::to_owned).unwrap_or_else(|_| branch_ref.clone());
    let ours = head.peel_to_commit()?;

    let lane = open_worktree(lane_path)?;
    let theirs_id = lane_state_commit(&lane, lane_name)?.id();
    let theirs = repo.find_commit(theirs_id)?;
    if repo.graph_descendant_of(ours.id(), theirs.id())? || ours.id() == theirs.id() {
        return Err(NudeError::GitOperation(format!("«{branch}» ya contiene todos los cambios del carril")));
    }

    let mut index = repo.merge_commits(&ours, &theirs, None)?;
    if index.has_conflicts() {
        if !resolve_conflicts {
            return Ok(LaneMerge::Conflicts(conflict_paths(&index)?));
        }
        let mut opts = MergeOptions::new();
        opts.file_favor(FileFavor::Theirs);
        index = repo.merge_commits(&ours, &theirs, Some(&opts))?;
        take_lane_side(&mut index)?;
        if index.has_conflicts() {
            return Ok(LaneMerge::Conflicts(conflict_paths(&index)?));
        }
    }
    let tree = repo.find_tree(index.write_tree_to(&repo)?)?;
    let files = repo.diff_tree_to_tree(Some(&ours.tree()?), Some(&tree), None)?.deltas().len();

    let signature = repo.signature().map_err(|_| {
        NudeError::GitOperation(
            "Git no tiene autor configurado. Ejecuta `git config --global user.name \"Tu Nombre\"` \
             y `git config --global user.email tu@correo` y vuelve a intentarlo"
                .into(),
        )
    })?;
    // El commit se crea sin mover la rama: solo se publica si el checkout va bien.
    let merge_id = repo.commit(
        None,
        &signature,
        &signature,
        &format!("Aplicar carril «{lane_name}» ({lane_branch})"),
        &tree,
        &[&ours, &theirs],
    )?;

    let mut checkout = CheckoutBuilder::new();
    checkout.safe();
    match repo.checkout_tree(tree.as_object(), Some(&mut checkout)) {
        Ok(()) => {}
        Err(e) if e.code() == ErrorCode::Conflict => {
            return Err(NudeError::GitOperation(format!(
                "tienes cambios sin guardar en «{branch}» que chocan con el carril. Haz commit o un \
                 checkpoint y descártalos antes de aplicarlo ({})",
                e.message()
            )));
        }
        Err(e) => return Err(e.into()),
    }
    // Solo avanza si la rama sigue donde estaba al empezar.
    repo.reference_matching(&branch_ref, merge_id, true, ours.id(), &format!("nude: aplicar carril {lane_branch}"))?;
    Ok(LaneMerge::Merged { commit_id: merge_id.to_string(), branch, files })
}

/// Borra el worktree del carril y su rama `nude/lane-*`. Solo actúa sobre
/// rutas dentro de `base` y ramas con el prefijo de carriles; el `HEAD`, el
/// índice y el árbol del repositorio principal no se tocan.
pub fn remove_lane_worktree(root: &Path, base: &Path, lane_path: &Path, lane_branch: &str) -> NudeResult<()> {
    if !lane_branch.starts_with(LANE_BRANCH_PREFIX) {
        return Err(NudeError::GitOperation(format!("«{lane_branch}» no es una rama de carril")));
    }
    let base = fs::canonicalize(base).unwrap_or_else(|_| base.to_path_buf());
    let target = fs::canonicalize(lane_path).unwrap_or_else(|_| lane_path.to_path_buf());
    if !target.starts_with(&base) || target == base {
        return Err(NudeError::GitOperation(format!(
            "{} no es un carril de Nude; no se borra",
            lane_path.display()
        )));
    }

    let repo = open_worktree(root)?;
    for name in repo.worktrees()?.iter().flatten().flatten() {
        let Ok(worktree) = repo.find_worktree(name) else { continue };
        let path = fs::canonicalize(worktree.path()).unwrap_or_else(|_| worktree.path().to_path_buf());
        if path != target {
            continue;
        }
        let mut opts = WorktreePruneOptions::new();
        opts.valid(true).locked(true).working_tree(true);
        worktree.prune(Some(&mut opts))?;
    }
    if target.exists() {
        fs::remove_dir_all(&target)?;
    }
    // Carpeta del proyecto en `base`, si quedó vacía.
    if let Some(parent) = target.parent().filter(|p| *p != base) {
        let _ = fs::remove_dir(parent);
    }

    match repo.find_branch(lane_branch, BranchType::Local) {
        Ok(mut branch) => branch.delete()?,
        Err(e) if e.code() == ErrorCode::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lane_slugs_are_safe_branch_and_folder_names() {
        assert_eq!(lane_slug("Refactor de pagos"), "refactor-de-pagos");
        assert_eq!(lane_slug("  Añadir diseño ✨ v2!! "), "anadir-diseno-v2");
        assert_eq!(lane_slug("../../etc"), "etc");
        assert_eq!(lane_slug("💥"), "carril");
        assert!(lane_slug(&"x".repeat(200)).len() <= MAX_SLUG_LEN);
        assert!(git2::Branch::name_is_valid(&format!("{LANE_BRANCH_PREFIX}{}", lane_slug("a.lock b"))).unwrap_or(false));
    }

    #[test]
    fn removing_refuses_paths_outside_the_lane_folder() {
        let dir = std::env::temp_dir().join(format!("nude-wt-guard-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(dir.join("base")).expect("mkdir");
        fs::create_dir_all(dir.join("outside")).expect("mkdir");
        let result = remove_lane_worktree(&dir, &dir.join("base"), &dir.join("outside"), "nude/lane-x");
        assert!(result.is_err());
        assert!(dir.join("outside").exists());
        assert!(remove_lane_worktree(&dir, &dir.join("base"), &dir.join("base/x"), "main").is_err());
        let _ = fs::remove_dir_all(&dir);
    }
}
