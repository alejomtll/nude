//! Lectura del historial real del repositorio del usuario (solo lectura).

use std::path::Path;

use git2::{BranchType, ErrorCode, Repository, Sort, Status, StatusOptions};
use serde::Serialize;

use super::repository::open_worktree;
use crate::error::NudeResult;

/// Commits que se devuelven para dibujar el grafo.
pub const GRAPH_COMMIT_LIMIT: usize = 50;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct HeadInfo {
    /// Rama actual (`None` si `HEAD` está separado).
    pub branch: Option<String>,
    /// Commit de `HEAD` (`None` en una rama sin commits).
    pub commit_id: Option<String>,
    pub detached: bool,
    pub unborn: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BranchInfo {
    pub name: String,
    pub commit_id: Option<String>,
    pub is_head: bool,
    pub upstream: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CommitInfo {
    pub id: String,
    pub short_id: String,
    pub parent_ids: Vec<String>,
    pub author_name: String,
    pub author_email: String,
    /// RFC 3339 (UTC).
    pub date: String,
    pub summary: String,
    pub message: String,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum StatusKind {
    Added,
    Modified,
    Deleted,
    Renamed,
    Typechange,
    Untracked,
    Conflicted,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct StatusEntry {
    pub path: String,
    pub kind: StatusKind,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WorkingTreeStatus {
    /// Cambios en el índice (`git add` hecho).
    pub staged: Vec<StatusEntry>,
    /// Cambios en el árbol de trabajo sin añadir, incluidos los no seguidos.
    pub unstaged: Vec<StatusEntry>,
    pub conflicted: Vec<StatusEntry>,
}


#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RepoHistory {
    pub head: HeadInfo,
    pub branches: Vec<BranchInfo>,
    /// Orden topológico, del más reciente al más antiguo.
    pub commits: Vec<CommitInfo>,
    pub status: WorkingTreeStatus,
    /// Estado especial del repo (`merge`, `rebase`…) o `None` si está limpio.
    pub operation: Option<String>,
}

pub fn read_history(root: &Path, limit: usize) -> NudeResult<RepoHistory> {
    let repo = open_worktree(root)?;
    let head = head_info(&repo)?;
    let branches = local_branches(&repo)?;
    let commits = recent_commits(&repo, &branches, &head, limit)?;
    let status = working_tree_status(&repo)?;
    let operation = match repo.state() {
        git2::RepositoryState::Clean => None,
        other => Some(format!("{other:?}").to_lowercase()),
    };
    Ok(RepoHistory {
        head,
        branches,
        commits,
        status,
        operation,
    })
}

pub(super) fn head_info(repo: &Repository) -> NudeResult<HeadInfo> {
    let detached = repo.head_detached().unwrap_or(false);
    match repo.head() {
        Ok(head) => Ok(HeadInfo {
            branch: if detached { None } else { head.shorthand().ok().map(str::to_owned) },
            commit_id: head.target().map(|o| o.to_string()),
            detached,
            unborn: false,
        }),
        Err(e) if matches!(e.code(), ErrorCode::UnbornBranch | ErrorCode::NotFound) => {
            // `HEAD` apunta a `refs/heads/<rama>` que aún no existe.
            let branch = repo
                .find_reference("HEAD")
                .ok()
                .and_then(|r| r.symbolic_target().ok().flatten().map(str::to_owned))
                .and_then(|t| t.strip_prefix("refs/heads/").map(str::to_owned));
            Ok(HeadInfo {
                branch,
                commit_id: None,
                detached: false,
                unborn: true,
            })
        }
        Err(e) => Err(e.into()),
    }
}

fn local_branches(repo: &Repository) -> NudeResult<Vec<BranchInfo>> {
    let mut out = Vec::new();
    for item in repo.branches(Some(BranchType::Local))? {
        let (branch, _) = item?;
        let Some(name) = branch.name()?.map(str::to_owned) else { continue };
        let upstream = branch
            .upstream()
            .ok()
            .and_then(|u| u.name().ok().flatten().map(str::to_owned));
        out.push(BranchInfo {
            commit_id: branch.get().target().map(|o| o.to_string()),
            is_head: branch.is_head(),
            name,
            upstream,
        });
    }
    out.sort_by(|a, b| b.is_head.cmp(&a.is_head).then_with(|| a.name.cmp(&b.name)));
    Ok(out)
}

fn recent_commits(
    repo: &Repository,
    branches: &[BranchInfo],
    head: &HeadInfo,
    limit: usize,
) -> NudeResult<Vec<CommitInfo>> {
    if head.unborn && branches.is_empty() {
        return Ok(Vec::new());
    }
    let mut walk = repo.revwalk()?;
    walk.set_sorting(Sort::TOPOLOGICAL | Sort::TIME)?;
    if head.commit_id.is_some() {
        walk.push_head()?;
    }
    for branch in branches {
        if let Some(id) = &branch.commit_id {
            walk.push(git2::Oid::from_str(id)?)?;
        }
    }

    let mut commits = Vec::with_capacity(limit);
    for oid in walk.take(limit) {
        let commit = repo.find_commit(oid?)?;
        let author = commit.author();
        let date = chrono::DateTime::from_timestamp(commit.time().seconds(), 0)
            .map(|d| d.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
            .unwrap_or_default();
        let id = commit.id().to_string();
        commits.push(CommitInfo {
            short_id: id.chars().take(7).collect(),
            parent_ids: commit.parent_ids().map(|p| p.to_string()).collect(),
            author_name: author.name().unwrap_or_default().to_owned(),
            author_email: author.email().unwrap_or_default().to_owned(),
            date,
            summary: commit.summary().ok().flatten().unwrap_or_default().to_owned(),
            message: commit.message().unwrap_or_default().trim_end().to_owned(),
            id,
        });
    }
    Ok(commits)
}

pub(super) fn working_tree_status(repo: &Repository) -> NudeResult<WorkingTreeStatus> {
    let mut opts = StatusOptions::new();
    opts.include_untracked(true)
        .recurse_untracked_dirs(true)
        .renames_head_to_index(true)
        .include_ignored(false);
    let statuses = repo.statuses(Some(&mut opts))?;

    let mut out = WorkingTreeStatus::default();
    for entry in statuses.iter() {
        let Ok(path) = entry.path().map(str::to_owned) else { continue };
        let s = entry.status();
        if s.contains(Status::CONFLICTED) {
            out.conflicted.push(StatusEntry { path, kind: StatusKind::Conflicted });
            continue;
        }
        let staged = if s.contains(Status::INDEX_NEW) {
            Some(StatusKind::Added)
        } else if s.contains(Status::INDEX_DELETED) {
            Some(StatusKind::Deleted)
        } else if s.contains(Status::INDEX_RENAMED) {
            Some(StatusKind::Renamed)
        } else if s.contains(Status::INDEX_TYPECHANGE) {
            Some(StatusKind::Typechange)
        } else if s.contains(Status::INDEX_MODIFIED) {
            Some(StatusKind::Modified)
        } else {
            None
        };
        let unstaged = if s.contains(Status::WT_NEW) {
            Some(StatusKind::Untracked)
        } else if s.contains(Status::WT_DELETED) {
            Some(StatusKind::Deleted)
        } else if s.contains(Status::WT_RENAMED) {
            Some(StatusKind::Renamed)
        } else if s.contains(Status::WT_TYPECHANGE) {
            Some(StatusKind::Typechange)
        } else if s.contains(Status::WT_MODIFIED) {
            Some(StatusKind::Modified)
        } else {
            None
        };
        if let Some(kind) = staged {
            out.staged.push(StatusEntry { path: path.clone(), kind });
        }
        if let Some(kind) = unstaged {
            out.unstaged.push(StatusEntry { path, kind });
        }
    }
    Ok(out)
}

/// Commit base de varias refs de checkpoint de una vez (mismo orden; `None`
/// si la ref ya no existe o el snapshot se tomó sin commits).
pub fn checkpoint_base_commits(root: &Path, git_refs: &[&str]) -> NudeResult<Vec<Option<String>>> {
    let repo = open_worktree(root)?;
    Ok(git_refs
        .iter()
        .map(|r| {
            repo.find_reference(r)
                .and_then(|r| r.peel_to_commit())
                .ok()
                .and_then(|c| c.parent_ids().next().map(|p| p.to_string()))
        })
        .collect())
}

/// Commit base (padre) de una ref de checkpoint: el `HEAD` de cuando se tomó.
pub fn checkpoint_base_commit(root: &Path, git_ref: &str) -> NudeResult<Option<String>> {
    let repo = open_worktree(root)?;
    let commit = repo.find_reference(git_ref)?.peel_to_commit()?;
    Ok(commit.parent_ids().next().map(|p| p.to_string()))
}

/// Árbol del commit base de una ref de checkpoint (`None` si no tiene padre).
pub fn checkpoint_base_tree_id(root: &Path, git_ref: &str) -> NudeResult<Option<String>> {
    let repo = open_worktree(root)?;
    let commit = repo.find_reference(git_ref)?.peel_to_commit()?;
    Ok(match commit.parent(0) {
        Ok(parent) => Some(parent.tree_id().to_string()),
        Err(_) => None,
    })
}
