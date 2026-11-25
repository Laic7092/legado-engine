use std::sync::Mutex;

#[derive(Debug, Clone, Default)]
pub struct ShowBrowserRequest {
    pub url: String,
    pub html: Option<String>,
    pub preload_js: Option<String>,
    pub options: Option<serde_json::Value>,
    pub title: Option<String>,
    pub await_result: bool,
    pub panel_id: Option<String>,
    pub external_window: bool,
    pub book_source_url: Option<String>,
    pub await_panel_id: Option<String>,
}

static PENDING: Mutex<Option<ShowBrowserRequest>> = Mutex::new(None);

pub fn set_show_browser_request(request: ShowBrowserRequest) {
    *PENDING.lock().unwrap_or_else(|e| e.into_inner()) = Some(request);
}

pub fn take_show_browser_request() -> Option<ShowBrowserRequest> {
    PENDING.lock().unwrap_or_else(|e| e.into_inner()).take()
}
