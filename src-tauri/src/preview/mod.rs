//! Capturas de servidores loopback elegidos expresamente por el usuario.
#[cfg(target_os = "macos")]
mod native;

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tauri::{AppHandle, Emitter, Url};
use tokio::sync::Semaphore;

use crate::db::{Database, PreviewInfo};
use crate::error::{NudeError, NudeResult};
use crate::git;

pub const EVENT_PREVIEW_CAPTURED: &str = "preview-captured";
const PROBE_TIMEOUT: Duration = Duration::from_millis(800);

#[derive(Clone)]
pub struct PreviewService {
    db: Database,
    capture_slot: Arc<Semaphore>,
}

impl PreviewService {
    pub fn new(db: Database) -> Self {
        Self {
            db,
            capture_slot: Arc::new(Semaphore::new(1)),
        }
    }

    pub async fn capture(
        &self,
        app: &AppHandle,
        project: String,
        checkpoint: String,
    ) -> NudeResult<Option<PreviewInfo>> {
        let result = self
            .capture_with(project, checkpoint, async |url| {
                #[cfg(target_os = "macos")]
                return native::capture(app, url.as_str(), &content_rules(&url)).await;
                #[cfg(not(target_os = "macos"))]
                {
                    let _ = (app, url);
                    Ok(None)
                }
            })
            .await?;
        if let Some(info) = &result {
            let _ = app.emit(EVENT_PREVIEW_CAPTURED, info);
        }
        Ok(result)
    }

    async fn capture_with(
        &self,
        project: String,
        checkpoint: String,
        render: impl AsyncFnOnce(Url) -> NudeResult<Option<Vec<u8>>>,
    ) -> NudeResult<Option<PreviewInfo>> {
        let _slot = self
            .capture_slot
            .acquire()
            .await
            .map_err(|e| NudeError::Task(e.to_string()))?;

        let db = self.db.clone();
        let target = tauri::async_runtime::spawn_blocking(move || {
            let project_row = db
                .get_project(&project)?
                .ok_or_else(|| NudeError::NotFound(format!("proyecto {project}")))?;
            let checkpoint_row = db
                .get_checkpoint(&checkpoint)?
                .filter(|cp| cp.project_id == project)
                .ok_or_else(|| NudeError::NotFound(format!("checkpoint {checkpoint}")))?;
            let Some(raw_url) = project_row.preview_url.as_deref() else {
                return Ok(None);
            };
            if db
                .latest_checkpoint(&project)?
                .is_none_or(|latest| latest.id != checkpoint_row.id)
            {
                return Ok(None);
            }
            let url = parse_url(raw_url)?;
            if !is_listening(&url)
                || !matches_checkpoint(&project_row.root_path, &checkpoint_row.git_ref)?
            {
                return Ok(None);
            }
            Ok::<_, NudeError>(Some((project_row, checkpoint_row, url)))
        })
        .await
        .map_err(|e| NudeError::Task(e.to_string()))??;
        let Some((project, checkpoint, url)) = target else {
            return Ok(None);
        };

        let url_text = url.as_str().to_owned();
        let Some(png) = render(url).await? else {
            return Ok(None);
        };
        let captured_at = chrono::Utc::now().timestamp_millis();
        let db = self.db.clone();
        tauri::async_runtime::spawn_blocking(move || {
            if !matches_checkpoint(&project.root_path, &checkpoint.git_ref)? {
                return Ok(None);
            }
            let info = PreviewInfo {
                project_id: project.id,
                checkpoint_id: checkpoint.id,
                captured_at,
                url: url_text,
            };
            Ok(db.save_preview(&info, &png)?.then_some(info))
        })
        .await
        .map_err(|e| NudeError::Task(e.to_string()))?
    }
}

fn matches_checkpoint(root: &str, reference: &str) -> NudeResult<bool> {
    let root = Path::new(root);
    Ok(git::working_tree_id(root)? == git::checkpoint_tree_id(root, reference)?)
}

pub fn normalize_url(raw: Option<&str>) -> NudeResult<Option<String>> {
    raw.map(str::trim)
        .filter(|url| !url.is_empty())
        .map(|url| parse_url(url).map(|url| url.to_string()))
        .transpose()
}

fn parse_url(raw: &str) -> NudeResult<Url> {
    let invalid = || {
        NudeError::Task(
            "Usa una URL HTTP o HTTPS local (localhost, 127.0.0.1 o [::1]), sin credenciales."
                .into(),
        )
    };
    let url = Url::parse(raw).map_err(|_| invalid())?;
    if matches!(url.scheme(), "http" | "https")
        && matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"))
        && url.username().is_empty()
        && url.password().is_none()
        && url.port() != Some(0)
    {
        return Ok(url);
    }
    Err(invalid())
}

fn is_listening(url: &Url) -> bool {
    let Some(port) = url.port_or_known_default() else {
        return false;
    };
    let v4 = IpAddr::V4(Ipv4Addr::LOCALHOST);
    let v6 = IpAddr::V6(Ipv6Addr::LOCALHOST);
    let hosts: &[IpAddr] = match url.host_str() {
        Some("localhost") => &[v4, v6],
        Some("127.0.0.1") => &[v4],
        Some("[::1]") => &[v6],
        _ => return false,
    };
    let start = Instant::now();
    hosts.iter().any(|host| {
        let remaining = PROBE_TIMEOUT.saturating_sub(start.elapsed());
        !remaining.is_zero()
            && TcpStream::connect_timeout(&SocketAddr::new(*host, port), remaining).is_ok()
    })
}

#[cfg(any(target_os = "macos", test))]
fn content_rules(url: &Url) -> String {
    let host = url
        .host_str()
        .unwrap_or_default()
        .replace('.', "\\.")
        .replace('[', "\\[")
        .replace(']', "\\]");
    let port = match url.port() {
        Some(port) => format!(":{port}"),
        None => format!("(:{})?", url.port_or_known_default().unwrap_or_default()),
    };
    let scheme = url.scheme();
    let websocket = if scheme == "https" { "wss" } else { "ws" };
    let allow = |pattern: String| {
        serde_json::json!({
            "trigger": {"url-filter": pattern},
            "action": {"type": "ignore-previous-rules"}
        })
    };
    serde_json::json!([
        {"trigger": {"url-filter": ".*"}, "action": {"type": "block"}},
        allow(format!("^{scheme}://{host}{port}/")),
        allow(format!("^{websocket}://{host}{port}/")),
        allow("^data:".into()),
        allow("^blob:".into()),
        allow("^about:".into())
    ])
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::models::AgentTool;

    #[test]
    fn accepts_only_loopback_http_urls_without_credentials() {
        for valid in [
            "http://localhost:5173",
            "https://127.0.0.1:3000/route",
            "http://[::1]:8080/",
        ] {
            assert!(normalize_url(Some(valid)).expect("valid").is_some());
        }
        for invalid in [
            "file:///etc/passwd",
            "http://localhost.example.com",
            "http://10.0.0.1",
            "http://localhost@10.0.0.1",
            "http://user:password@localhost",
            "http://localhost:0",
        ] {
            assert!(normalize_url(Some(invalid)).is_err(), "{invalid}");
        }
        assert_eq!(normalize_url(Some("  ")).expect("disable"), None);
        let rules = content_rules(&parse_url("http://[::1]:3000/").expect("url"));
        let rules: serde_json::Value = serde_json::from_str(&rules).expect("rule list");
        assert_eq!(rules[0]["action"]["type"], "block");
        assert_eq!(rules[1]["trigger"]["url-filter"], r"^http://\[::1\]:3000/");
    }

    #[tokio::test]
    async fn offline_server_does_not_render_or_write_a_preview() {
        let socket = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("reserve a port");
        let port = socket.local_addr().expect("address").port();
        drop(socket); // Al probar la conexión no hay un servidor escuchando.
        let db = Database::open_in_memory().expect("db");
        let (project, _) = db.upsert_project("demo", "/tmp/demo").expect("project");
        let session = db
            .get_or_create_session(&project.id, AgentTool::Manual, None, None)
            .expect("session");
        let checkpoint = db
            .insert_checkpoint(
                &session.id,
                &project.id,
                "refs/nude/checkpoints/s/1",
                None,
                &[],
            )
            .expect("checkpoint");
        db.set_preview_url(&project.id, Some(&format!("http://127.0.0.1:{port}/")))
            .expect("URL");
        let service = PreviewService::new(db.clone());
        let result = service
            .capture_with(project.id.clone(), checkpoint.id.clone(), async |_| {
                panic!("WebKit must not start when the port is closed");
            })
            .await
            .expect("offline is not an error");
        assert!(result.is_none());
        assert!(
            db.get_checkpoint(&checkpoint.id)
                .expect("checkpoint")
                .is_some()
        );
    }
}
