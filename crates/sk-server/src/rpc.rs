//! Method routing: binds protocol [`Method`]s to async handlers (Python `server/rpc.py`).
//!
//! A handler gets a [`Call`] (the server, the session, the request id) and the method's parsed,
//! validated params, and returns the method's result or an [`RpcError`]. The router erases the
//! types: it parses the raw params, runs the handler and encodes the whole response frame.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use serde_json::Value;
use serde_json::value::RawValue;
use sk_protocol::methods::Method;
use sk_protocol::{ErrorCode, ParamsError, RequestId, encode_response, parse_params};

use crate::app::Server;
use crate::session::Session;

/// Answer a request with a JSON-RPC error.
#[derive(Clone, Debug, PartialEq)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    pub data: Option<Value>,
}

impl RpcError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> RpcError {
        RpcError {
            code: code.code().into(),
            message: message.into(),
            data: None,
        }
    }

    pub fn with_data(mut self, data: Value) -> RpcError {
        self.data = Some(data);
        self
    }

    pub fn invalid_params(message: impl Into<String>) -> RpcError {
        RpcError::new(ErrorCode::InvalidParams, message)
    }

    pub fn invalid_request(message: impl Into<String>) -> RpcError {
        RpcError::new(ErrorCode::InvalidRequest, message)
    }

    pub fn internal(message: impl Into<String>) -> RpcError {
        RpcError::new(ErrorCode::InternalError, message)
    }

    pub fn cancelled() -> RpcError {
        RpcError::new(ErrorCode::RequestCancelled, "request cancelled")
    }

    /// A call a newer request with the same key made pointless ([`crate::compute::Latest`]).
    pub fn superseded() -> RpcError {
        RpcError::new(ErrorCode::RequestCancelled, "superseded by a newer request")
    }

    pub fn from_params(e: &ParamsError, message: &str) -> RpcError {
        RpcError::new(ErrorCode::InvalidParams, message).with_data(e.data())
    }
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({})", self.message, self.code)
    }
}

impl std::error::Error for RpcError {}

pub type RpcResult<T> = Result<T, RpcError>;

/// What a handler is answering.
#[derive(Clone)]
pub struct Call {
    pub server: Arc<Server>,
    pub session: Arc<Session>,
    pub id: RequestId,
}

pub type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;

/// A type-erased handler: raw params in, the encoded response frame out.
pub type Handler =
    Arc<dyn Fn(Call, Option<Box<RawValue>>) -> BoxFuture<RpcResult<String>> + Send + Sync>;

/// Method name → handler.
#[derive(Default, Clone)]
pub struct Router {
    routes: HashMap<String, Handler>,
}

impl Router {
    pub fn new() -> Router {
        Router::default()
    }

    /// Register the handler of method `M`.
    ///
    /// # Panics
    /// When `M` is registered twice.
    pub fn add<M, F, Fut>(&mut self, handler: F)
    where
        M: Method,
        M::Params: Send + 'static,
        M::Result: Send + 'static,
        F: Fn(Call, M::Params) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = RpcResult<M::Result>> + Send + 'static,
    {
        self.add_raw(M::NAME, move |call, raw| {
            let parsed = parse_params::<M::Params>(raw.as_deref());
            let fut = parsed.map(|p| handler(call.clone(), p));
            async move {
                let fut = fut.map_err(|e| RpcError::from_params(&e, "invalid params"))?;
                let result = fut.await?;
                Ok(encode_response(&call.id, &result))
            }
        });
    }

    /// Register a handler that takes the raw params and returns the whole response frame (tests).
    pub fn add_raw<F, Fut>(&mut self, name: &str, handler: F)
    where
        F: Fn(Call, Option<Box<RawValue>>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = RpcResult<String>> + Send + 'static,
    {
        let h: Handler = Arc::new(move |call, raw| Box::pin(handler(call, raw)));
        assert!(
            self.routes.insert(name.to_owned(), h).is_none(),
            "method {name:?} registered twice"
        );
    }

    pub fn get(&self, name: &str) -> Option<&Handler> {
        self.routes.get(name)
    }

    pub fn names(&self) -> Vec<&str> {
        let mut v: Vec<&str> = self.routes.keys().map(String::as_str).collect();
        v.sort_unstable();
        v
    }
}

pub fn not_found(name: &str) -> RpcError {
    RpcError::new(
        ErrorCode::MethodNotFound,
        format!("method not found: {name}"),
    )
}
