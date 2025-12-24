use crate::backend::{Backend, BackendView};
use crate::balancer::LoadBalancingMode;
use std::sync::Arc;
use tokio::sync::{Mutex, Notify};
use tokio::time::Instant;
use tracing::{debug, debug_span, Instrument};

pub struct BackendPool {
    inner: Mutex<PoolState>,
    notify: Notify,
}

struct PoolState {
    backends: Vec<BackendState>,
    rr_cursor: usize,
}

struct BackendState {
    backend: Backend,
    active: usize,
}

pub struct BackendLease {
    pool: Arc<BackendPool>,
    backend: Backend,
    backend_id: String,
}

impl BackendPool {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(PoolState {
                backends: Vec::new(),
                rr_cursor: 0,
            }),
            notify: Notify::new(),
        }
    }

    pub async fn add_backend(&self, backend: Backend) -> BackendView {
        let view = {
            let mut state = self.inner.lock().await;
            state.backends.push(BackendState { backend, active: 0 });
            state
                .backends
                .last()
                .expect("backend just inserted")
                .view()
        };
        debug!(
            backend_id = %view.id,
            backend_url = %view.url,
            "backend added"
        );
        self.notify.notify_waiters();
        view
    }

    pub async fn remove_backend(&self, id: &str) -> Option<BackendView> {
        let removed = {
            let mut state = self.inner.lock().await;
            let pos = state.backends.iter().position(|b| b.backend.id == id)?;
            let removed = state.backends.remove(pos);
            if state.rr_cursor >= state.backends.len() {
                state.rr_cursor = 0;
            }
            removed
        };
        let view = removed.view();
        debug!(
            backend_id = %view.id,
            backend_url = %view.url,
            "backend removed"
        );
        self.notify.notify_waiters();
        Some(view)
    }

    pub async fn list_backends(&self) -> Vec<BackendView> {
        let state = self.inner.lock().await;
        state.backends.iter().map(BackendState::view).collect()
    }

    pub async fn acquire(self: &Arc<Self>, mode: LoadBalancingMode) -> Option<BackendLease> {
        match mode {
            LoadBalancingMode::LeastConn => self.try_acquire(mode).await,
            LoadBalancingMode::Queue => self.acquire_queue().await,
        }
    }

    async fn release(&self, id: &str) {
        let mut state = self.inner.lock().await;
        if let Some(backend) = state.backends.iter_mut().find(|b| b.backend.id == id) {
            backend.active = backend.active.saturating_sub(1);
            debug!(
                backend_id = %backend.backend.id,
                active_connections = backend.active,
                "backend released"
            );
        }
        drop(state);
        self.notify.notify_waiters();
    }

    async fn try_acquire(self: &Arc<Self>, mode: LoadBalancingMode) -> Option<BackendLease> {
        let mut state = self.inner.lock().await;
        let idx = match mode {
            LoadBalancingMode::LeastConn => select_least_conn(&state.backends),
            LoadBalancingMode::Queue => select_queue(&mut state),
        }?;
        Some(allocate_backend(self, &mut state, idx))
    }

    async fn acquire_queue(self: &Arc<Self>) -> Option<BackendLease> {
        let mut wait_start: Option<Instant> = None;
        loop {
            let notified = {
                let mut state = self.inner.lock().await;
                if state.backends.is_empty() {
                    debug!("queue acquire: no backends configured");
                    return None;
                }
                if let Some(idx) = select_queue(&mut state) {
                    if let Some(start) = wait_start.take() {
                        debug!(
                            queue_wait_ms = start.elapsed().as_millis(),
                            "queue wait complete"
                        );
                    }
                    return Some(allocate_backend(self, &mut state, idx));
                }
                if wait_start.is_none() {
                    wait_start = Some(Instant::now());
                }
                debug!("queue acquire: waiting for available backend");
                self.notify.notified()
            };
            notified
                .instrument(debug_span!("queue_wait"))
                .await;
        }
    }
}

impl BackendLease {
    pub fn backend(&self) -> &Backend {
        &self.backend
    }
}

impl Drop for BackendLease {
    fn drop(&mut self) {
        let pool = Arc::clone(&self.pool);
        let backend_id = self.backend_id.clone();
        tokio::spawn(async move {
            pool.release(&backend_id).await;
        });
    }
}

impl BackendState {
    fn view(&self) -> BackendView {
        BackendView {
            id: self.backend.id.clone(),
            url: self.backend.url.clone(),
            scheme: self.backend.scheme,
            max_connections: self.backend.max_connections,
            active_connections: self.active,
        }
    }

    fn is_available(&self) -> bool {
        match self.backend.max_connections {
            Some(max) => self.active < max,
            None => true,
        }
    }
}

fn select_least_conn(backends: &[BackendState]) -> Option<usize> {
    let mut selected: Option<usize> = None;
    for (idx, backend) in backends.iter().enumerate() {
        if !backend.is_available() {
            continue;
        }
        match selected {
            None => selected = Some(idx),
            Some(best) => {
                if backend.active < backends[best].active {
                    selected = Some(idx);
                }
            }
        }
    }
    selected
}

fn select_queue(state: &mut PoolState) -> Option<usize> {
    let total = state.backends.len();
    if total == 0 {
        return None;
    }
    for offset in 0..total {
        let idx = (state.rr_cursor + offset) % total;
        if state.backends[idx].is_available() {
            state.rr_cursor = (idx + 1) % total;
            return Some(idx);
        }
    }
    None
}

fn allocate_backend(
    pool: &Arc<BackendPool>,
    state: &mut PoolState,
    idx: usize,
) -> BackendLease {
    let backend_state = state
        .backends
        .get_mut(idx)
        .expect("backend index exists");
    backend_state.active = backend_state.active.saturating_add(1);
    let backend = backend_state.backend.clone();
    let backend_id = backend.id.clone();
    debug!(
        backend_id = %backend.id,
        backend_url = %backend.url,
        active_connections = backend_state.active,
        "backend leased"
    );

    BackendLease {
        pool: Arc::clone(pool),
        backend,
        backend_id,
    }
}
