use crate::service::book_service::ShowBrowserRequest;

#[derive(Debug, Clone, serde::Serialize)]
pub struct BrowserPage {
    pub url: String,
    pub body: String,
}

pub fn await_browser(_request: ShowBrowserRequest) -> Result<serde_json::Value, String> {
    Err("browser login is not implemented".to_string())
}

pub fn await_external_window(_request: ShowBrowserRequest) -> Result<BrowserPage, String> {
    Err("browser login is not implemented".to_string())
}

pub fn request_external_window(_request: ShowBrowserRequest) -> Result<(), String> {
    Err("browser login is not implemented".to_string())
}
