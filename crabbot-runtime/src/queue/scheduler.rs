use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::Arc,
};

use tokio::sync::{Mutex, OwnedSemaphorePermit, Semaphore, oneshot};

use crate::{
    config::QueueConfig,
    error::{Error, Result},
};

type RespId = u64;

#[derive(Debug)]
pub struct WorkItem {
    pub resp_id: RespId,
    pub session_key: String,
    pub body: String,

    // Holds one global capacity permit while this item is in-flight.
    _cap: OwnedSemaphorePermit,
}

#[derive(Debug)]
struct State<T> {
    next_id: RespId,

    queues: HashMap<String, VecDeque<(RespId, String)>>,
    rr: VecDeque<String>,

    in_flight_sessions: HashSet<String>,
    waiters: HashMap<RespId, oneshot::Sender<Result<T>>>,
}

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
                waiters: HashMap::new(),
            }),
            available: Semaphore::new(0),
            capacity: Arc::new(Semaphore::new(max_parallel)),
            kick: Semaphore::new(0),
        }
    }

    pub async fn add(&self, session_key: String, body: String) -> oneshot::Receiver<Result<T>> {
        let (tx, rx) = oneshot::channel();

        let mut st = self.st.lock().await;

        let id = st.next_id;
        st.next_id += 1;

        st.waiters.insert(id, tx);

        let q = st.queues.entry(session_key.clone()).or_default();
        let was_empty = q.is_empty();
        q.push_back((id, body));

        if was_empty {
            st.rr.push_back(session_key);
        }

        drop(st);

        // This must run for next_ready() to ever wake from available.acquire().
        self.available.add_permits(1);
        rx
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

                    let rr_len = st.rr.len();
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

                        match q.pop_front() {
                            Some((resp_id, body)) => {
                                let still_has_items = !q.is_empty();

                                st.in_flight_sessions.insert(sk.clone());

                                if still_has_items {
                                    st.rr.push_back(sk.clone());
                                } else {
                                    st.queues.remove(&sk);
                                }

                                drop(st);
                                drop(avail); // consume the “one queued item” permit

                                return Ok(Some(WorkItem {
                                    resp_id,
                                    session_key: sk,
                                    body,
                                    _cap: cap, // hold capacity until complete()
                                }));
                            }
                            None => {
                                st.queues.remove(&sk);
                            }
                        }
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
                            kick.map_err(|_| Error::other("kick semaphore closed"))?;
                            continue;
                        }
                    }
                }
            }
        }
    }

    pub async fn complete(&self, item: WorkItem, result: Result<T>) {
        let waiter = {
            let mut st = self.st.lock().await;
            st.in_flight_sessions.remove(&item.session_key);
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
}
