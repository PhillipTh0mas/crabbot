use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::Arc,
};

use tokio::sync::{Mutex, OwnedSemaphorePermit, Semaphore, oneshot};
use tokio_util::sync::CancellationToken;

use crate::{
    config::QueueConfig,
    error::{Error, Result},
};

type RespId = u64;

/// Special resp_id value meaning "no waiter / fire-and-forget".
const NO_RESP: RespId = 0;

/// Priority levels for queue items.
/// Lower numeric value = higher priority.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Priority {
    /// User-initiated work and task ticks (highest priority).
    Normal = 0,
    /// Background thinking / housekeeping (lower priority).
    Background = 1,
}

impl Default for Priority {
    fn default() -> Self {
        Priority::Normal
    }
}

#[derive(Debug)]
pub struct WorkItem {
    pub resp_id: RespId,
    pub session_key: String,
    pub body: String,
    pub priority: Priority,
    pub cancel: CancellationToken,

    // Holds one global capacity permit while this item is in-flight.
    _cap: OwnedSemaphorePermit,
}

impl WorkItem {
    /// Returns true if this item has a caller waiting for a response.
    pub fn has_waiter(&self) -> bool {
        self.resp_id != NO_RESP
    }
}

#[derive(Debug)]
struct QueueEntry {
    resp_id: RespId,
    body: String,
    priority: Priority,
}

impl QueueEntry {
    fn fire_and_forget(priority: Priority) -> Self {
        Self {
            resp_id: NO_RESP,
            body: String::new(),
            priority,
        }
    }
}

#[derive(Debug)]
struct State<T> {
    next_id: RespId,

    queues: HashMap<String, VecDeque<QueueEntry>>,
    rr: VecDeque<String>,

    in_flight_sessions: HashSet<String>,
    session_cancel_tokens: HashMap<String, CancellationToken>,
    waiters: HashMap<RespId, oneshot::Sender<Result<T>>>,

    /// Tracks how many background items have been skipped in a row
    /// to prevent starvation.
    background_skip_count: usize,
}

/// Maximum number of scheduling rounds that background items can be
/// skipped before they get promoted. Prevents starvation.
const MAX_BACKGROUND_SKIPS: usize = 5;

#[derive(Debug)]
pub struct QueueScheduler<T> {
    st: Mutex<State<T>>,

    // number of queued items across all sessions
    available: Semaphore,

    // global max-parallel gate (RAII via OwnedSemaphorePermit)
    capacity: Arc<Semaphore>,

    // wake pickers when a session becomes available again
    kick: Semaphore,
}

impl<T: Send + 'static> QueueScheduler<T> {
    pub fn new(config: &QueueConfig) -> Self {
        let max_parallel = config.max_parallel_runs.max(1);

        Self {
            st: Mutex::new(State {
                next_id: 1,
                queues: HashMap::new(),
                rr: VecDeque::new(),
                in_flight_sessions: HashSet::new(),
                session_cancel_tokens: HashMap::new(),
                waiters: HashMap::new(),
                background_skip_count: 0,
            }),
            available: Semaphore::new(0),
            capacity: Arc::new(Semaphore::new(max_parallel)),
            kick: Semaphore::new(0),
        }
    }

    /// Add a work item with default (Normal) priority.
    pub async fn add(&self, session_key: String, body: String) -> oneshot::Receiver<Result<T>> {
        self.add_with_priority(session_key, body, Priority::Normal)
            .await
    }

    /// Add a work item with explicit priority.
    pub async fn add_with_priority(
        &self,
        session_key: String,
        body: String,
        priority: Priority,
    ) -> oneshot::Receiver<Result<T>> {
        let (tx, rx) = oneshot::channel();

        let mut st = self.st.lock().await;

        let id = st.next_id;
        st.next_id += 1;

        st.waiters.insert(id, tx);

        let q = st.queues.entry(session_key.clone()).or_default();
        let was_empty = q.is_empty();
        q.push_back(QueueEntry {
            resp_id: id,
            body,
            priority,
        });

        if was_empty {
            st.rr.push_back(session_key);
        }

        drop(st);

        // This must run for next_ready() to ever wake from available.acquire().
        self.available.add_permits(1);
        rx
    }

    /// Fire-and-forget: schedule a session for processing without a body or response channel.
    /// Used to re-schedule a session after yielding mid-tool-loop.
    /// If the session already has items queued or is in-flight, this is a no-op
    /// (the session will be picked up again anyway).
    pub async fn schedule(&self, session_key: String, priority: Priority) {
        let mut st = self.st.lock().await;

        // Don't double-queue: if already queued or in-flight, skip.
        let already_queued = st
            .queues
            .get(&session_key)
            .map(|q| !q.is_empty())
            .unwrap_or(false);
        if already_queued {
            return;
        }

        let q = st.queues.entry(session_key.clone()).or_default();
        q.push_back(QueueEntry::fire_and_forget(priority));
        // Only add to round-robin if the queue was empty (i.e. we just inserted the first item).
        // Since we checked `already_queued` above, q was definitely empty before push_back.
        st.rr.push_back(session_key);

        drop(st);
        self.available.add_permits(1);
    }

    /// Check if a session already has items queued (not in-flight, but waiting).
    pub async fn has_queued(&self, session_key: &str) -> bool {
        let st = self.st.lock().await;
        st.queues
            .get(session_key)
            .map(|q| !q.is_empty())
            .unwrap_or(false)
    }

    /// Check if a session has items either queued or currently in-flight.
    pub async fn has_queued_or_inflight(&self, session_key: &str) -> bool {
        let st = self.st.lock().await;
        let queued = st
            .queues
            .get(session_key)
            .map(|q| !q.is_empty())
            .unwrap_or(false);
        let inflight = st.in_flight_sessions.contains(session_key);
        queued || inflight
    }

    /// Returns:
    /// - Ok(Some(item)) when an item is ready to process
    /// - Ok(None) when cancelled
    pub async fn next_ready(
        &self,
        cancel: &tokio_util::sync::CancellationToken,
    ) -> Result<Option<WorkItem>> {
        loop {
            // Wait until at least one queued item exists OR cancelled.
            tokio::select! {
                _ = cancel.cancelled() => return Ok(None),
                permit = self.available.acquire() => {
                    let avail = permit.map_err(|_| Error::other("available semaphore closed"))?;

                    // Also wait for global capacity OR cancelled.
                    let cap = tokio::select! {
                        _ = cancel.cancelled() => { drop(avail); return Ok(None); }
                        p = self.capacity.clone().acquire_owned() => {
                            p.map_err(|_| Error::other("capacity semaphore closed"))?
                        }
                    };

                    // Try to pick a runnable item under the lock.
                    let mut st = self.st.lock().await;

                    // Determine if we should allow background items this round.
                    // If background items have been skipped too many times, promote them.
                    let allow_background = st.background_skip_count >= MAX_BACKGROUND_SKIPS;

                    let rr_len = st.rr.len();
                    let mut picked = None;
                    let mut skipped_background = false;

                    for _ in 0..rr_len {
                        let sk = match st.rr.pop_front() {
                            Some(sk) => sk,
                            None => break,
                        };

                        if st.in_flight_sessions.contains(&sk) {
                            st.rr.push_back(sk);
                            continue;
                        }

                        let q = match st.queues.get_mut(&sk) {
                            Some(q) => q,
                            None => continue,
                        };

                        // Peek at the front item's priority
                        let front_priority = q.front().map(|e| e.priority);

                        match front_priority {
                            Some(Priority::Background) if !allow_background => {
                                // Skip background items if normal items might be available
                                // and we haven't starved background too long
                                skipped_background = true;
                                st.rr.push_back(sk);
                                continue;
                            }
                            Some(_) => {
                                // Normal priority or background that's been promoted
                                match q.pop_front() {
                                    Some(entry) => {
                                        let still_has_items = !q.is_empty();

                                        st.in_flight_sessions.insert(sk.clone());
                                        let cancel_token = CancellationToken::new();
                                        st.session_cancel_tokens.insert(sk.clone(), cancel_token.clone());

                                        if still_has_items {
                                            st.rr.push_back(sk.clone());
                                        } else {
                                            st.queues.remove(&sk);
                                        }

                                        // Reset background skip count when we pick something
                                        if entry.priority == Priority::Background {
                                            st.background_skip_count = 0;
                                        }

                                        picked = Some((sk, entry, allow_background));
                                        break;
                                    }
                                    None => {
                                        st.queues.remove(&sk);
                                    }
                                }
                            }
                            None => {
                                st.queues.remove(&sk);
                            }
                        }
                    }

                    // If we skipped background items but found nothing normal,
                    // do a second pass allowing background
                    if picked.is_none() && skipped_background {
                        let rr_len2 = st.rr.len();
                        for _ in 0..rr_len2 {
                            let sk = match st.rr.pop_front() {
                                Some(sk) => sk,
                                None => break,
                            };

                            if st.in_flight_sessions.contains(&sk) {
                                st.rr.push_back(sk);
                                continue;
                            }

                            let q = match st.queues.get_mut(&sk) {
                                Some(q) => q,
                                None => continue,
                            };

                            match q.pop_front() {
                                Some(entry) => {
                                    let still_has_items = !q.is_empty();
                                    st.in_flight_sessions.insert(sk.clone());
                                    let cancel_token = CancellationToken::new();
                                    st.session_cancel_tokens.insert(sk.clone(), cancel_token.clone());

                                    if still_has_items {
                                        st.rr.push_back(sk.clone());
                                    } else {
                                        st.queues.remove(&sk);
                                    }

                                    st.background_skip_count = 0;
                                    picked = Some((sk, entry, true));
                                    break;
                                }
                                None => {
                                    st.queues.remove(&sk);
                                }
                            }
                        }
                    }

                    if let Some((sk, entry, _)) = picked {
                        // Increment skip count if we skipped background items
                        if skipped_background && entry.priority != Priority::Background {
                            st.background_skip_count = st.background_skip_count.saturating_add(1);
                        }

                        let cancel_token = st.session_cancel_tokens.get(&sk).cloned().unwrap();
                        drop(st);
                        drop(avail); // consume the "one queued item" permit

                        return Ok(Some(WorkItem {
                            resp_id: entry.resp_id,
                            session_key: sk,
                            body: entry.body,
                            priority: entry.priority,
                            cancel: cancel_token,
                            _cap: cap, // hold capacity until complete()
                        }));
                    }

                    // No runnable item (likely all sessions are in-flight).
                    // Give permits back and wait for a kick (session freed) OR cancelled.
                    drop(st);
                    drop(cap);
                    drop(avail);

                    // restore the "available item" permit we reserved but couldn't use
                    self.available.add_permits(1);

                    tokio::select! {
                        _ = cancel.cancelled() => return Ok(None),
                        kick = self.kick.acquire() => {
                            let _permit = kick.map_err(|_| Error::other("kick semaphore closed"))?;
                            drop(_permit);
                            continue;
                        }
                    }
                }
            }
        }
    }

    /// Returns the set of session keys that are currently being processed (in-flight).
    pub async fn in_flight_sessions(&self) -> HashSet<String> {
        let st = self.st.lock().await;
        st.in_flight_sessions.clone()
    }

    pub async fn complete(&self, item: WorkItem, result: Result<T>) {
        let waiter = {
            let mut st = self.st.lock().await;
            st.in_flight_sessions.remove(&item.session_key);
            st.session_cancel_tokens.remove(&item.session_key);
            st.waiters.remove(&item.resp_id)
        };

        if let Some(tx) = waiter {
            let _ = tx.send(result);
        }

        // dropping `item` releases capacity via `_cap`
        drop(item);

        // wake a picker that was blocked because all sessions were in-flight
        self.kick.add_permits(1);
    }

    /// Complete a work item without sending a response.
    /// Used when `process` yields mid-loop (e.g. after executing a tool call)
    /// and re-schedules itself. The slot is freed but no waiter is notified yet.
    /// The `resp_id` is returned so the caller can keep it and resolve the waiter later.
    pub async fn complete_step(&self, item: WorkItem) -> RespId {
        let resp_id = item.resp_id;
        {
            let mut st = self.st.lock().await;
            st.in_flight_sessions.remove(&item.session_key);
            st.session_cancel_tokens.remove(&item.session_key);
            // Do NOT remove the waiter — we'll resolve it on a future step.
        }

        // dropping `item` releases capacity via `_cap`
        drop(item);

        // wake a picker that was blocked because all sessions were in-flight
        self.kick.add_permits(1);

        resp_id
    }

    /// Re-schedule a session and carry forward a pending waiter's resp_id.
    /// The new WorkItem will inherit the resp_id so that when processing
    /// finally completes, the original caller gets their response.
    pub async fn reschedule_with_resp(
        &self,
        session_key: String,
        resp_id: RespId,
        priority: Priority,
    ) {
        let mut st = self.st.lock().await;

        let q = st.queues.entry(session_key.clone()).or_default();
        let was_empty = q.is_empty();
        q.push_back(QueueEntry {
            resp_id,
            body: String::new(),
            priority,
        });

        if was_empty {
            st.rr.push_back(session_key);
        }

        drop(st);
        self.available.add_permits(1);
    }

    /// Resolve a pending waiter directly by resp_id, without going through a WorkItem.
    /// Used when a later step produces the final response for an earlier caller.
    pub async fn resolve_waiter(&self, resp_id: RespId, result: Result<T>) {
        if resp_id == NO_RESP {
            return;
        }
        let waiter = {
            let mut st = self.st.lock().await;
            st.waiters.remove(&resp_id)
        };
        if let Some(tx) = waiter {
            let _ = tx.send(result);
        }
    }

    /// Interrupt an in-flight session by cancelling its token.
    /// Returns `true` if the session was found and cancelled, `false` otherwise.
    pub async fn interrupt_session(&self, session_key: &str) -> bool {
        let st = self.st.lock().await;
        if let Some(token) = st.session_cancel_tokens.get(session_key) {
            token.cancel();
            true
        } else {
            false
        }
    }
}
