use super::slot::GLOBAL_GENERATION_ACTIVE;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::sync::Notify;

/// 任务租约 RAII 守卫：在准入时构造，只有在闭包退出、资源释放时在 Drop 中原子递减
#[derive(Debug)]
pub struct ActiveTaskGuard {
    active_tasks: Arc<AtomicUsize>,
    notify_drain: Arc<Notify>,
    epoch: u64,
}

impl ActiveTaskGuard {
    pub fn new(
        active_tasks: Arc<AtomicUsize>,
        notify_drain: Arc<Notify>,
        epoch: u64,
    ) -> Self {
        active_tasks.fetch_add(1, Ordering::SeqCst);
        GLOBAL_GENERATION_ACTIVE.fetch_add(1, Ordering::SeqCst);
        Self {
            active_tasks,
            notify_drain,
            epoch,
        }
    }

    pub fn epoch(&self) -> u64 {
        self.epoch
    }
}

impl Drop for ActiveTaskGuard {
    fn drop(&mut self) {
        let prev_active = self.active_tasks.fetch_sub(1, Ordering::SeqCst);
        GLOBAL_GENERATION_ACTIVE.fetch_sub(1, Ordering::SeqCst);
        
        // 当活跃任务归零时，唤醒可能正在等待 Drain 排空的协程
        if prev_active == 1 {
            self.notify_drain.notify_waiters();
        }
    }
}
