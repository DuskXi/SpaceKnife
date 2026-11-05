//! Subscription topics (Python `server/topics.py`). A topic factory turns `$/subscribe` params
//! into a live [`Subscription`]; its pushers check [`SubFlag::closed`] and emit events with the
//! subscription's id.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use sk_core::log::LogBus;
use sk_protocol::ErrorCode;
use sk_protocol::methods::LogRecord;
use sk_protocol::streams::{LogTopicParams, SubscribeParams, TOPIC_LOG};

use crate::app::Server;
use crate::rpc::{RpcError, RpcResult};
use crate::session::Session;
use crate::util::random_hex;

/// Shared by a subscription and its pushers: closed once unsubscribed or the session ended; an
/// optional rate limit (`rate_hz`).
pub struct SubFlag {
    pub id: String,
    closed: AtomicBool,
    min_gap: Option<Duration>,
    last: Mutex<Option<Instant>>,
}

impl SubFlag {
    pub fn closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    /// Whether an event may go now under the rate limit (`force`: always, e.g. a final status);
    /// records the time when it may.
    pub fn admit(&self, force: bool) -> bool {
        if self.closed() {
            return false;
        }
        let Some(gap) = self.min_gap else { return true };
        let mut last = self.last.lock();
        let now = Instant::now();
        if force || last.is_none_or(|t| now.duration_since(t) >= gap) {
            *last = Some(now);
            true
        } else {
            false
        }
    }
}

/// A live subscription.
pub struct Subscription {
    pub topic: String,
    pub id: String,
    pub flag: Arc<SubFlag>,
    on_close: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

impl Subscription {
    pub fn new(topic: &str, rate_hz: Option<f64>) -> Subscription {
        let id = format!("s{}", random_hex(6));
        let flag = Arc::new(SubFlag {
            id: id.clone(),
            closed: AtomicBool::new(false),
            min_gap: rate_hz.filter(|r| *r > 0.0).map(|r| Duration::from_secs_f64(1.0 / r)),
            last: Mutex::new(None),
        });
        Subscription { topic: topic.to_owned(), id, flag, on_close: Mutex::new(None) }
    }

    pub fn on_close(&self, f: impl FnOnce() + Send + 'static) {
        *self.on_close.lock() = Some(Box::new(f));
    }

    pub fn close(&self) {
        if !self.flag.closed.swap(true, Ordering::SeqCst)
            && let Some(f) = self.on_close.lock().take()
        {
            f();
        }
    }
}

pub type TopicFactory =
    Arc<dyn Fn(&Arc<Server>, &Arc<Session>, &SubscribeParams) -> RpcResult<Subscription> + Send + Sync>;

#[derive(Default, Clone)]
pub struct TopicRegistry {
    factories: HashMap<String, TopicFactory>,
}

impl TopicRegistry {
    pub fn register(
        &mut self,
        name: &str,
        f: impl Fn(&Arc<Server>, &Arc<Session>, &SubscribeParams) -> RpcResult<Subscription> + Send + Sync + 'static,
    ) {
        self.factories.insert(name.to_owned(), Arc::new(f));
    }

    pub fn open(&self, server: &Arc<Server>, session: &Arc<Session>, params: &SubscribeParams) -> RpcResult<Subscription> {
        match self.factories.get(&params.topic) {
            Some(f) => f(server, session, params),
            None => Err(RpcError::new(ErrorCode::UnknownTopic, format!("unknown topic: {}", params.topic))
                .with_data(serde_json::json!({ "topics": self.names() }))),
        }
    }

    pub fn names(&self) -> Vec<String> {
        let mut v: Vec<String> = self.factories.keys().cloned().collect();
        v.sort();
        v
    }
}

/// `log` topic: replays a backlog, then streams new records at or above `min_level`. Records
/// arrive on the logging thread and only go into the session's queue there.
pub fn open_log(bus: Arc<LogBus>, session: &Arc<Session>, params: &SubscribeParams) -> RpcResult<Subscription> {
    let opts: LogTopicParams = params.topic_params().map_err(|e| RpcError::from_params(&e, "invalid log topic params"))?;
    let sub = Subscription::new(TOPIC_LOG, None);
    let flag = sub.flag.clone();
    let weak = Arc::downgrade(session);
    let (backlog, token) = bus.subscribe_with_backlog(opts.backlog as usize, opts.min_level, move |entry| {
        if flag.closed() {
            return;
        }
        if let Some(s) = weak.upgrade() {
            s.emit::<LogRecord>(&entry.to_event(&flag.id));
        }
    });
    for e in backlog {
        session.emit::<LogRecord>(&e.to_event(&sub.id));
    }
    sub.on_close(move || bus.unsubscribe(token));
    Ok(sub)
}
