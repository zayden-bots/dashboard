use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use deadlock_protocol::ws::{CLOSE_POLICY, Down, PING_INTERVAL_SECS};
use tokio::sync::{mpsc, watch};

pub const MAX_SOCKETS_PER_TOKEN: usize = 3;
const OUTBOUND_CAPACITY: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WsTiming {
    pub hello: Duration,
    pub ping: Duration,
    pub idle: Duration,
}

impl Default for WsTiming {
    fn default() -> Self {
        Self {
            hello: Duration::from_secs(10),
            ping: Duration::from_secs(PING_INTERVAL_SECS.into()),
            idle: Duration::from_secs(75),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SocketInfo {
    pub id: u64,
    pub token_id: i64,
    pub discord_id: i64,
    pub account_id: u32,
}

pub struct Mailbox {
    pub outbound: mpsc::Receiver<Down>,
    pub close: watch::Receiver<Option<u16>>,
}

pub struct Registration {
    hub: Arc<WsHub>,
    id: u64,
}

impl Registration {
    #[must_use]
    pub const fn id(&self) -> u64 {
        self.id
    }
}

impl Drop for Registration {
    fn drop(&mut self) {
        self.hub.lock().sockets.remove(&self.id);
    }
}

pub struct WsHub {
    timing: WsTiming,
    inner: Mutex<Inner>,
}

struct Inner {
    next_id: u64,
    sockets: BTreeMap<u64, Entry>,
}

struct Entry {
    info: SocketInfo,
    outbound: mpsc::Sender<Down>,
    close: watch::Sender<Option<u16>>,
}

impl Entry {
    fn close(&self, code: u16) {
        self.close.send_if_modified(|current| {
            if current.is_none() {
                *current = Some(code);
                true
            } else {
                false
            }
        });
    }
}

impl WsHub {
    #[must_use]
    pub const fn new(timing: WsTiming) -> Self {
        Self {
            timing,
            inner: Mutex::new(Inner { next_id: 0, sockets: BTreeMap::new() }),
        }
    }

    #[must_use]
    pub const fn timing(&self) -> WsTiming {
        self.timing
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn register(
        self: &Arc<Self>,
        token_id: i64,
        discord_id: i64,
        account_id: u32,
    ) -> (Registration, Mailbox) {
        let (outbound_tx, outbound) = mpsc::channel(OUTBOUND_CAPACITY);
        let (close_tx, close) = watch::channel(None);
        let mut inner = self.lock();

        let live = |entry: &&Entry| {
            entry.info.token_id == token_id && entry.close.borrow().is_none()
        };
        while inner.sockets.values().filter(live).count() >= MAX_SOCKETS_PER_TOKEN {
            let Some(oldest) = inner.sockets.values().find(live) else {
                break;
            };
            oldest.close(CLOSE_POLICY);
        }

        let id = inner.next_id;
        inner.next_id += 1;
        inner.sockets.insert(id, Entry {
            info: SocketInfo { id, token_id, discord_id, account_id },
            outbound: outbound_tx,
            close: close_tx,
        });
        drop(inner);

        (Registration { hub: Arc::clone(self), id }, Mailbox { outbound, close })
    }

    pub fn push_all(&self, down: &Down) {
        for entry in self.lock().sockets.values() {
            push(entry, down);
        }
    }

    pub fn push_user(&self, discord_id: i64, down: &Down) {
        for entry in self.lock().sockets.values() {
            if entry.info.discord_id == discord_id {
                push(entry, down);
            }
        }
    }

    pub fn close_token(&self, token_id: i64, code: u16) {
        for entry in self.lock().sockets.values() {
            if entry.info.token_id == token_id {
                entry.close(code);
            }
        }
    }

    pub fn close_socket(&self, id: u64, code: u16) {
        if let Some(entry) = self.lock().sockets.get(&id) {
            entry.close(code);
        }
    }

    #[must_use]
    pub fn sockets(&self) -> Vec<SocketInfo> {
        self.lock().sockets.values().map(|entry| entry.info).collect()
    }

    #[must_use]
    pub fn sockets_of(&self, discord_id: i64) -> Vec<SocketInfo> {
        self.lock()
            .sockets
            .values()
            .filter(|entry| entry.info.discord_id == discord_id)
            .map(|entry| entry.info)
            .collect()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.lock().sockets.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.lock().sockets.is_empty()
    }
}

fn push(entry: &Entry, down: &Down) {
    match entry.outbound.try_send(down.clone()) {
        Err(mpsc::error::TrySendError::Full(_)) => entry.close(CLOSE_POLICY),
        Ok(()) | Err(mpsc::error::TrySendError::Closed(_)) => {},
    }
}
