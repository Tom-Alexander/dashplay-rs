//! Browser `fetch` backend for [`dashplayrs::HttpClient`].

use bytes::Bytes;
use dashplayrs::{HttpClient, HttpError, HttpFuture, HttpMethod, HttpRequest, HttpResponse};
use js_sys::Uint8Array;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;
use web_sys::{Headers, Request, RequestInit, RequestMode, Response};

#[derive(Debug, Clone, Default)]
pub struct FetchClient;

impl FetchClient {
    fn window() -> Result<web_sys::Window, HttpError> {
        web_sys::window().ok_or_else(|| HttpError::Transport("no browser window".into()))
    }
}

impl HttpClient for FetchClient {
    fn send<'a>(&'a self, request: HttpRequest) -> HttpFuture<'a, Result<HttpResponse, HttpError>> {
        Box::pin(async move { self.send_request(request).await })
    }
}

impl FetchClient {
    async fn send_request(&self, request: HttpRequest) -> Result<HttpResponse, HttpError> {
        let method = match request.method() {
            HttpMethod::Get => "GET",
            HttpMethod::Head => "HEAD",
            HttpMethod::Post => "POST",
        };

        let mut init = RequestInit::new();
        init.method(method);
        init.mode(RequestMode::Cors);

        let headers = Headers::new()
            .map_err(|_| HttpError::Transport("failed to create request headers".into()))?;
        for (name, value) in request.headers() {
            headers
                .append(&name, &value)
                .map_err(|_| HttpError::Transport(format!("invalid header: {name}")))?;
        }
        init.headers(&headers);

        let fetch_request = Request::new_with_str_and_init(&request.url().to_string(), &init)
            .map_err(|_| HttpError::Transport("failed to create fetch request".into()))?;

        let window = Self::window()?;
        let response_value = JsFuture::from(window.fetch_with_request(&fetch_request))
            .await
            .map_err(|err| HttpError::Transport(format_js_error("fetch failed", err)))?;
        let response: Response = response_value
            .dyn_into()
            .map_err(|_| HttpError::Transport("fetch response was not a Response".into()))?;

        let status = response.status() as u16;
        if request.method() == HttpMethod::Head {
            return Ok(HttpResponse::new(status, Vec::new(), Bytes::new()));
        }

        let buffer_value = JsFuture::from(
            response
                .array_buffer()
                .map_err(|_| HttpError::Transport("response body read failed".into()))?,
        )
        .await
        .map_err(|err| HttpError::Transport(format_js_error("body read failed", err)))?;

        let bytes = js_value_to_bytes(&buffer_value)?;
        Ok(HttpResponse::new(status, Vec::new(), bytes))
    }
}

fn js_value_to_bytes(value: &wasm_bindgen::JsValue) -> Result<Bytes, HttpError> {
    let array = Uint8Array::new(value);
    let mut bytes = vec![0u8; array.length() as usize];
    array.copy_to(&mut bytes);
    Ok(Bytes::from(bytes))
}

fn format_js_error(context: &str, err: wasm_bindgen::JsValue) -> String {
    if let Some(message) = err.as_string() {
        format!("{context}: {message}")
    } else {
        format!("{context}: {err:?}")
    }
}
