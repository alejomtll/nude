use std::ffi::{CString, c_char, c_void};

use tauri::AppHandle;
use tokio::sync::oneshot;

use crate::db::MAX_PREVIEW_BYTES;
use crate::error::{NudeError, NudeResult};

type Reply = oneshot::Sender<Option<Vec<u8>>>;

unsafe extern "C" {
    fn nude_capture_preview(
        url: *const c_char,
        rules: *const c_char,
        context: *mut c_void,
        callback: extern "C" fn(*mut c_void, *const u8, usize),
    );
}

extern "C" fn receive_png(context: *mut c_void, bytes: *const u8, len: usize) {
    // The bridge calls this exactly once. NSData owns the bytes until we return.
    let reply = unsafe { Box::from_raw(context.cast::<Reply>()) };
    let png = if bytes.is_null() || len == 0 || len > MAX_PREVIEW_BYTES {
        None
    } else {
        Some(unsafe { std::slice::from_raw_parts(bytes, len) }.to_vec())
    };
    let _ = reply.send(png);
}

pub async fn capture(app: &AppHandle, url: &str, rules: &str) -> NudeResult<Option<Vec<u8>>> {
    let url = CString::new(url).map_err(|e| NudeError::Task(e.to_string()))?;
    let rules = CString::new(rules).map_err(|e| NudeError::Task(e.to_string()))?;
    let (send, receive) = oneshot::channel();
    app.run_on_main_thread(move || {
        let context = Box::into_raw(Box::new(send)).cast();
        // The bridge copies both strings synchronously, and owns the response context.
        unsafe { nude_capture_preview(url.as_ptr(), rules.as_ptr(), context, receive_png) };
    })
    .map_err(|e| NudeError::Task(e.to_string()))?;
    receive.await.map_err(|e| NudeError::Task(e.to_string()))
}
