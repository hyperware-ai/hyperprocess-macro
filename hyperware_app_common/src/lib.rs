// this is hyperware_app_common
use std::cell::RefCell;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use futures_util::task::noop_waker_ref;
use hyperware_process_lib::{
    http::server::{HttpServer, IncomingHttpRequest},
    logging::info,
    get_state, http, set_state, timer, BuildError, LazyLoadBlob, Message, Request, SendError,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

pub use hyperware_process_lib;

pub mod prelude {
    pub use crate::APP_CONTEXT;
    pub use crate::RESPONSE_REGISTRY;
    // Add other commonly used items here
}

thread_local! {
    pub static APP_CONTEXT: RefCell<AppContext> = RefCell::new(AppContext {
        hidden_state: None,
        executor: Executor::new(),
    });

    pub static RESPONSE_REGISTRY: RefCell<HashMap<String, Vec<u8>>> = RefCell::new(HashMap::new());

    pub static APP_HELPERS: RefCell<AppHelpers> = RefCell::new(AppHelpers {
        current_server: None,
        current_message: None,
        current_http_context: None,
    });
}

#[derive(Clone)]
pub struct HttpRequestContext {
    pub request: IncomingHttpRequest,
    pub response_headers: HashMap<String, String>,
}

pub struct AppContext {
    pub hidden_state: Option<HiddenState>,
    pub executor: Executor,
}

pub struct AppHelpers {
    pub current_server: Option<*mut HttpServer>,
    pub current_message: Option<Message>,
    pub current_http_context: Option<HttpRequestContext>,
}

// Access function for the current path
pub fn get_path() -> Option<String> {
    APP_HELPERS.with(|helpers| {
        helpers.borrow().current_http_context.as_ref()
            .and_then(|ctx| ctx.request.path().ok())
    })
}

// Access function for the current server
pub fn get_server() -> Option<&'static mut HttpServer> {
    APP_HELPERS.with(|ctx| ctx.borrow().current_server.map(|ptr| unsafe { &mut *ptr }))
}

pub fn get_http_method() -> Option<String> {
    APP_HELPERS.with(|helpers| {
        helpers.borrow().current_http_context.as_ref()
            .and_then(|ctx| ctx.request.method().ok())
            .map(|m| m.to_string())
    })
}

// Set response headers that will be included in the HTTP response
pub fn set_response_headers(headers: HashMap<String, String>) {
    APP_HELPERS.with(|helpers| {
        if let Some(ctx) = &mut helpers.borrow_mut().current_http_context {
            ctx.response_headers = headers;
        }
    })
}

// Add a single response header
pub fn add_response_header(key: String, value: String) {
    APP_HELPERS.with(|helpers| {
        if let Some(ctx) = &mut helpers.borrow_mut().current_http_context {
            ctx.response_headers.insert(key, value);
        }
    })
}


pub fn clear_http_request_context() {
    APP_HELPERS.with(|helpers| {
        helpers.borrow_mut().current_http_context = None;
    })
}

// Access function for the source address of the current message
pub fn source() -> hyperware_process_lib::Address {
    APP_HELPERS.with(|ctx| {
        ctx.borrow()
            .current_message
            .as_ref()
            .expect("No message in current context")
            .source()
            .clone()
    })
}

/// Get query parameters from the current HTTP request path
/// Returns None if not in an HTTP context or no query parameters present
pub fn get_query_params() -> Option<HashMap<String, String>> {
    get_path().map(|path| {
        let mut params = HashMap::new();
        if let Some(query_start) = path.find('?') {
            let query = &path[query_start + 1..];
            for pair in query.split('&') {
                if let Some(eq_pos) = pair.find('=') {
                    let key = pair[..eq_pos].to_string();
                    let value = pair[eq_pos + 1..].to_string();
                    params.insert(key, value);
                }
            }
        }
        params
    })
}

pub struct Executor {
    tasks: Vec<Pin<Box<dyn Future<Output = ()>>>>,
}

impl Executor {
    pub fn new() -> Self {
        Self { tasks: Vec::new() }
    }

    pub fn spawn(&mut self, fut: impl Future<Output = ()> + 'static) {
        self.tasks.push(Box::pin(fut));
    }

    pub fn poll_all_tasks(&mut self) {
        let mut ctx = Context::from_waker(noop_waker_ref());
        let mut completed = Vec::new();

        for i in 0..self.tasks.len() {
            if let Poll::Ready(()) = self.tasks[i].as_mut().poll(&mut ctx) {
                completed.push(i);
            }
        }

        for idx in completed.into_iter().rev() {
            let _ = self.tasks.remove(idx);
        }
    }
}
struct ResponseFuture {
    correlation_id: String,
    // Capture HTTP context at creation time
    http_context: Option<HttpRequestContext>,
}

impl ResponseFuture {
    fn new(correlation_id: String) -> Self {
        // Capture current HTTP context when future is created (at .await point)
        let http_context = APP_HELPERS.with(|helpers| {
            helpers.borrow().current_http_context.clone()
        });

        Self {
            correlation_id,
            http_context,
        }
    }
}

impl Future for ResponseFuture {
    type Output = Vec<u8>;

    fn poll(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Self::Output> {
        let correlation_id = &self.correlation_id;

        let maybe_bytes = RESPONSE_REGISTRY.with(|registry| {
            let mut registry_mut = registry.borrow_mut();
            registry_mut.remove(correlation_id)
        });

        if let Some(bytes) = maybe_bytes {
            // Restore this future's captured context
            if let Some(ref context) = self.http_context {
                APP_HELPERS.with(|helpers| {
                    helpers.borrow_mut().current_http_context = Some(context.clone());
                });
            }

            Poll::Ready(bytes)
        } else {
            Poll::Pending
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Error)]
pub enum AppSendError {
    #[error("SendError: {0}")]
    SendError(SendError),
    #[error("BuildError: {0}")]
    BuildError(BuildError),
}

pub async fn sleep(sleep_ms: u64) -> Result<(), AppSendError> {
    let request = Request::to(("our", "timer", "distro", "sys"))
        .body(timer::TimerAction::SetTimer(sleep_ms))
        .expects_response((sleep_ms / 1_000) + 1);

    let correlation_id = Uuid::new_v4().to_string();
    if let Err(e) = request.context(correlation_id.as_bytes().to_vec()).send() {
        return Err(AppSendError::BuildError(e));
    }

    let _ = ResponseFuture::new(correlation_id).await;

    return Ok(());
}

pub async fn send<R>(request: Request) -> Result<R, AppSendError>
where
    R: serde::de::DeserializeOwned,
{
    let request = if request.timeout.is_some() {
        request
    } else {
        request.expects_response(30)
    };

    let correlation_id = Uuid::new_v4().to_string();
    if let Err(e) = request.context(correlation_id.as_bytes().to_vec()).send() {
        return Err(AppSendError::BuildError(e));
    }

    let response_bytes = ResponseFuture::new(correlation_id).await;
    if let Ok(r) = serde_json::from_slice::<R>(&response_bytes) {
        return Ok(r);
    }

    let e = serde_json::from_slice::<SendError>(&response_bytes)
        .expect("Failed to deserialize response to send()");
    return Err(AppSendError::SendError(e));
}

pub async fn send_rmp<R>(request: Request) -> Result<R, AppSendError>
where
    R: serde::de::DeserializeOwned,
{
    let request = if request.timeout.is_some() {
        request
    } else {
        request.expects_response(30)
    };

    let correlation_id = Uuid::new_v4().to_string();
    if let Err(e) = request.context(correlation_id.as_bytes().to_vec()).send() {
        return Err(AppSendError::BuildError(e));
    }

    let response_bytes = ResponseFuture::new(correlation_id).await;
    if let Ok(r) = rmp_serde::from_slice::<R>(&response_bytes) {
        return Ok(r);
    }

    let e = rmp_serde::from_slice::<SendError>(&response_bytes)
        .expect("Failed to deserialize response to send()");
    return Err(AppSendError::SendError(e));
}

#[macro_export]
macro_rules! hyper {
    ($($code:tt)*) => {
        $crate::APP_CONTEXT.with(|ctx| {
            ctx.borrow_mut().executor.spawn(async move {
                $($code)*
            })
        })
    };
}


// Enum defining the state persistance behaviour
#[derive(Clone)]
pub enum SaveOptions {
    // Never Persist State
    Never,
    // Persist State Every Message
    EveryMessage,
    // Persist State Every N Messages
    EveryNMessage(u64),
    // Persist State Every N Seconds
    EveryNSeconds(u64),
    // Persist State Only If Changed
    OnDiff,
}
pub struct HiddenState {
    save_config: SaveOptions,
    message_count: u64,
    old_state: Option<Vec<u8>>, // Stores the serialized state from before message processing
}

impl HiddenState {
    pub fn new(save_config: SaveOptions) -> Self {
        Self {
            save_config,
            message_count: 0,
            old_state: None,
        }
    }

    fn should_save_state(&mut self) -> bool {
        match self.save_config {
            SaveOptions::Never => false,
            SaveOptions::EveryMessage => true,
            SaveOptions::EveryNMessage(n) => {
                self.message_count += 1;
                if self.message_count >= n {
                    self.message_count = 0;
                    true
                } else {
                    false
                }
            }
            SaveOptions::EveryNSeconds(_) => false, // Handled by timer instead
            SaveOptions::OnDiff => false, // Will be handled separately with state comparison
        }
    }
}

// TODO: We need a timer macro again.

/// Store a snapshot of the current state before processing a message
/// This is used for OnDiff save option to compare state before and after
/// Only stores if old_state is None (i.e., first time or after a save)
pub fn store_old_state<S>(state: &S)
where
    S: serde::Serialize,
{
    APP_CONTEXT.with(|ctx| {
        let mut ctx_mut = ctx.borrow_mut();
        if let Some(ref mut hidden_state) = ctx_mut.hidden_state {
            if matches!(hidden_state.save_config, SaveOptions::OnDiff) && hidden_state.old_state.is_none() {
                if let Ok(s_bytes) = rmp_serde::to_vec(state) {
                    hidden_state.old_state = Some(s_bytes);
                }
            }
        }
    });
}

/// Trait that must be implemented by application state types
pub trait State {
    /// Creates a new instance of the state.
    fn new() -> Self;
}

/// Initialize state from persisted storage or create new if none exists
/// TODO: Delete?
pub fn initialize_state<S>() -> S
where
    S: serde::de::DeserializeOwned + Default,
{
    match get_state() {
        Some(bytes) => match rmp_serde::from_slice::<S>(&bytes) {
            Ok(state) => state,
            Err(e) => {
                panic!("error deserializing existing state: {e}. We're panicking because we don't want to nuke state by setting it to a new instance.");
            }
        },
        None => {
            info!("no existing state, creating new one");
            S::default()
        }
    }
}

pub fn setup_server(
    ui_config: Option<&hyperware_process_lib::http::server::HttpBindingConfig>,
    endpoints: &[Binding],
) -> http::server::HttpServer {
    let mut server = http::server::HttpServer::new(5);

    if let Some(ui) = ui_config {
        if let Err(e) = server.serve_ui("ui", vec!["/"], ui.clone()) {
            panic!("failed to serve UI: {e}. Make sure that a ui folder is in /pkg");
        }
    }

    // Verify no duplicate paths
    let mut seen_paths = std::collections::HashSet::new();
    for endpoint in endpoints.iter() {
        let path = match endpoint {
            Binding::Http { path, .. } => path,
            Binding::Ws { path, .. } => path,
        };
        if !seen_paths.insert(path) {
            panic!("duplicate path found: {}", path);
        }
    }

    for endpoint in endpoints {
        match endpoint {
            Binding::Http { path, config } => {
                server
                    .bind_http_path(path.to_string(), config.clone())
                    .expect("failed to serve API path");
            }
            Binding::Ws { path, config } => {
                server
                    .bind_ws_path(path.to_string(), config.clone())
                    .expect("failed to bind WS path");
            }
        }
    }

    server
}

/// Pretty prints a SendError in a more readable format
pub fn pretty_print_send_error(error: &SendError) {
    let kind = &error.kind;
    let target = &error.target;

    // Try to decode body as UTF-8 string, fall back to showing as bytes
    let body = String::from_utf8(error.message.body().to_vec())
        .map(|s| format!("\"{}\"", s))
        .unwrap_or_else(|_| format!("{:?}", error.message.body()));

    // Try to decode context as UTF-8 string
    let context = error
        .context
        .as_ref()
        .map(|bytes| String::from_utf8_lossy(bytes).into_owned());

    hyperware_process_lib::logging::error!(
        "SendError {{
    kind: {:?},
    target: {},
    body: {},
    context: {}
}}",
        kind,
        target,
        body,
        context
            .map(|s| format!("\"{}\"", s))
            .unwrap_or("None".to_string())
    );
}

// For demonstration, we'll define them all in one place.
// Make sure the signatures match the real function signatures you require!
pub fn no_init_fn<S>(_state: &mut S) {
    // does nothing
}

pub fn no_ws_handler<S>(
    _state: &mut S,
    _server: &mut http::server::HttpServer,
    _channel_id: u32,
    _msg_type: http::server::WsMessageType,
    _blob: LazyLoadBlob,
) {
    // does nothing
}

pub fn no_http_api_call<S>(_state: &mut S, _req: ()) {
    // does nothing
}

pub fn no_local_request<S>(_msg: &Message, _state: &mut S, _req: ()) {
    // does nothing
}

pub fn no_remote_request<S>(_msg: &Message, _state: &mut S, _req: ()) {
    // does nothing
}

#[derive(Clone, Debug)]
pub enum Binding {
    Http {
        path: &'static str,
        config: hyperware_process_lib::http::server::HttpBindingConfig,
    },
    Ws {
        path: &'static str,
        config: hyperware_process_lib::http::server::WsBindingConfig,
    },
}

pub fn maybe_save_state<S>(state: &S)
where
    S: serde::Serialize,
{
    APP_CONTEXT.with(|ctx| {
        let mut ctx_mut = ctx.borrow_mut();
        if let Some(ref mut hidden_state) = ctx_mut.hidden_state {
            let should_save = if matches!(hidden_state.save_config, SaveOptions::OnDiff) {
                // For OnDiff, compare current state with old state
                if let Ok(current_bytes) = rmp_serde::to_vec(state) {
                    let state_changed = match &hidden_state.old_state {
                        Some(old_bytes) => old_bytes != &current_bytes,
                        None => true, // If no old state, consider it changed
                    };

                    if state_changed {
                        true
                    } else {
                        false
                    }
                } else {
                    false
                }
            } else {
                hidden_state.should_save_state()
            };

            if should_save {
                if let Ok(s_bytes) = rmp_serde::to_vec(state) {
                    let _ = set_state(&s_bytes);

                    // Clear old_state after saving so it can be set again on next message
                    if matches!(hidden_state.save_config, SaveOptions::OnDiff) {
                        hidden_state.old_state = None;
                    }
                }
            }
        }
    });
}
