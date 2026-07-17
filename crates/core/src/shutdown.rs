use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::Notify;

#[derive(Clone, Debug)]
pub struct ShutdownSignal {
    inner: Arc<ShutdownInner>,
}

#[derive(Debug)]
struct ShutdownInner {
    cancelled: AtomicBool,
    notify: Notify,
}

#[derive(Clone, Debug)]
pub struct ShutdownSender {
    inner: Arc<ShutdownInner>,
}

impl ShutdownSignal {
    pub fn new() -> (ShutdownSender, ShutdownSignal) {
        let inner = Arc::new(ShutdownInner {
            cancelled: AtomicBool::new(false),
            notify: Notify::new(),
        });
        (
            ShutdownSender {
                inner: Arc::clone(&inner),
            },
            ShutdownSignal { inner },
        )
    }

    pub async fn cancelled(&self) {
        loop {
            let notified = self.inner.notify.notified();
            if self.inner.cancelled.load(Ordering::SeqCst) {
                return;
            }
            notified.await;
        }
    }
}

impl ShutdownSender {
    pub fn cancel(&self) {
        self.inner.cancelled.store(true, Ordering::SeqCst);
        self.inner.notify.notify_waiters();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn cancelled_returns_promptly_when_already_cancelled() {
        let (tx, signal) = ShutdownSignal::new();
        tx.cancel();

        tokio::time::timeout(Duration::from_millis(100), signal.cancelled())
            .await
            .expect("pre-cancelled signal should resolve promptly");
    }

    #[tokio::test]
    async fn cancelled_returns_when_waiter_starts_before_cancel() {
        let (tx, signal) = ShutdownSignal::new();
        let waiter = tokio::spawn(async move {
            tokio::time::timeout(Duration::from_millis(100), signal.cancelled())
                .await
                .expect("waiter should observe cancellation");
        });

        tokio::task::yield_now().await;
        tx.cancel();

        waiter.await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancelled_does_not_miss_concurrent_cancel() {
        for _ in 0..10_000 {
            let (tx, signal) = ShutdownSignal::new();
            let barrier = Arc::new(tokio::sync::Barrier::new(2));

            let wait_barrier = Arc::clone(&barrier);
            let waiter = tokio::spawn(async move {
                wait_barrier.wait().await;
                tokio::time::timeout(Duration::from_millis(100), signal.cancelled()).await
            });

            let cancel_barrier = Arc::clone(&barrier);
            let canceller = tokio::spawn(async move {
                cancel_barrier.wait().await;
                tx.cancel();
            });

            canceller.await.unwrap();
            waiter
                .await
                .unwrap()
                .expect("concurrent cancellation should resolve waiter");
        }
    }
}
