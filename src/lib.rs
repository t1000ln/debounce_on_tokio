//! 基于tokio异步上下文防抖器。
//! 主要用于限流执行任务，特别是这些任务需要运行在tokio的异步上下文环境。
//! 用法示例：
//! ```rust
//! use std::thread;
//! use std::time::Duration;
//! use debounce_fltk::TokioDebounce;
//!
//!
//! let mut debounced_fn = TokioDebounce::new_debounce(Box::new(move |param: String| {
//!     println!("执行任务，入参：{}", param);
//! }), Duration::from_millis(1000), false);
//! for i in 0..50 {
//!     debounced_fn.update_param(i.to_string());
//!     thread::sleep(Duration::from_millis(200));
//! }
//! // 每隔1000毫秒打印一次任务信息。
//! ```
//! 防抖器结构体实例被创建后，将会通过tokio工作线程或协程无限循环检查新任务，在每个`duration`参数指定的周期内检查一次，
//! 其余时间处于休眠状态。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use fltk::app;
use once_cell::sync::Lazy;
use parking_lot::RwLock;

/// 基于tokio异步上下文防抖器。
#[derive(Debug, Clone)]
pub struct TokioDebounce<T> {
    jobs: Arc<AtomicU64>,
    last_param: Arc<Mutex<Option<T>>>,
}

impl<T: Clone + Send + 'static> TokioDebounce<T> {
    /// 创建新的防抖器实例。
    ///
    /// # Arguments
    ///
    /// * `task`: 回调任务。
    /// * `duration`: 延迟执行时限。
    /// * `ui_thread`: 是否在UI线程中执行任务。
    ///
    /// returns: TokioDebounce<T>
    ///
    /// # Examples
    ///
    /// ```
    ///
    /// ```
    pub fn new_debounce(task: Box<dyn FnMut(T) + Send + Sync + 'static>, duration: Duration, ui_thread: bool) -> Self {
        let jobs = Arc::new(AtomicU64::new(0));
        let jobs_clone = jobs.clone();
        let last_param: Arc<Mutex<Option<T>>> = Arc::new(Mutex::new(None));
        let last_param_clone = last_param.clone();
        let task = Arc::new(RwLock::new(task));
        tokio::spawn(async move {
            tokio::time::sleep(duration).await;
            loop {
                if jobs_clone.load(Ordering::Relaxed) > 0 {
                    if let Ok(mut param) = last_param_clone.lock() {
                        if let Some(param) = param.as_mut() {
                            if ui_thread {
                                app::awake_callback({
                                    let param = param.clone();
                                    let task = task.clone();
                                    move || {
                                        task.write()(param.clone());
                                    }
                                });
                            } else {
                                (task.write())(param.clone());
                            }
                        }
                    }
                    jobs_clone.store(0, Ordering::SeqCst);
                }
                tokio::time::sleep(duration).await;
            }

        });

        Self {
            jobs,
            last_param,
        }
    }

    /// 创建新的节流器实例。第一次请求将立即执行，之后在每个限定间隔时间内最多执行一次。
    ///
    /// # Arguments
    ///
    /// * `task`: 回调任务。如果任务返回 `true` 表示循环执行任务直到其内部的数据消耗完，否则表示停止执行任务。请注意自行维护返回值，避免陷入无效循环。
    /// * `duration`: 延迟执行时限。
    /// * `ui_thread`: 是否在UI线程中执行任务。
    ///
    /// returns: TokioDebounce<T>
    ///
    /// # Examples
    ///
    /// ```
    ///
    /// ```
    pub fn new_throttle(task: Box<dyn FnMut(T) -> bool + Send + Sync + 'static>, duration: Duration, ui_thread: bool) -> Self {
        let jobs = Arc::new(AtomicU64::new(0));
        let jobs_clone = jobs.clone();
        let last_param: Arc<Mutex<Option<T>>> = Arc::new(Mutex::new(None));
        let last_param_clone = last_param.clone();
        let task = Arc::new(RwLock::new(task));
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(duration);
            interval.tick().await;
            loop {
                if jobs_clone.load(Ordering::Relaxed) > 0 {
                    if let Ok(mut param) = last_param_clone.lock() {
                        if let Some(param) = param.as_mut() {
                            if ui_thread {
                                app::awake_callback({
                                    let param = param.clone();
                                    let task = task.clone();
                                    let jobs_clone = jobs_clone.clone();
                                    move || {
                                        if task.write()(param.clone()) {
                                            jobs_clone.fetch_add(1, Ordering::SeqCst);
                                        } else {
                                            jobs_clone.store(0, Ordering::SeqCst);
                                        }
                                    }
                                });
                            } else {
                                if (task.write())(param.clone()) {
                                    jobs_clone.fetch_add(1, Ordering::SeqCst);
                                } else {
                                    jobs_clone.store(0, Ordering::SeqCst);
                                }
                            }
                        }
                    }
                }
                interval.tick().await;
            }

        });

        Self {
            jobs,
            last_param,
        }
    }

    /// 加入新的任务参数，旧的任务参数被抛弃。
    /// 等待执行时刻到来，执行器将使用最新的任务参数去执行任务。
    ///
    /// # Arguments
    ///
    /// * `value`: 最新的任务参数。
    ///
    /// returns: ()
    ///
    /// # Examples
    ///
    /// ```
    /// use std::thread;
    /// use std::time::Duration;
    /// use debounce_fltk::TokioDebounce;
    ///
    /// let mut debounced_fn = TokioDebounce::new_debounce(Box::new(move |param: String| {
    ///     println!("Debounced: {:?}", param);
    /// }), Duration::from_millis(1000), false);
    /// for i in 0..10 {
    ///     thread::sleep(Duration::from_millis(200));
    ///     debounced_fn.update_param(i.to_string());
    /// }
    /// ```
    pub fn update_param(&mut self, value: T) {
        if let Ok(mut lp) = self.last_param.lock() {
            lp.replace(value);
            self.jobs.fetch_add(1, Ordering::SeqCst);
        }
    }
}


/// 全局防抖锁集合。
static DEBOUNCE_LOCK: Lazy<RwLock<HashMap<i64, Instant>>> = Lazy::new(|| RwLock::new(HashMap::new()));

/// 在一段程序中的某个位置设置一个限流检查点，凡是需要通过该屏障的执行逻辑都要被检查是否超出节流限制，在一个有限的时段内只允许第一个执行逻辑通过。
/// 若超出节流限制，则应结束该段逻辑。
///
/// # Arguments
///
/// * `task_id`: 自定义的任务编号。每段任务的编号都是唯一的。
/// * `limit`: 防抖有效时限。超过时限后检查点的状态将被重置。
///
/// returns: bool 若返回true则表明未超限，可以继续向下执行。返回false表示当前逻辑不能继续执行，必须立刻返回，因为已经有一个同样的任务在最近执行过。
///
/// # Examples
///
/// ```
/// use std::sync::OnceLock;
/// use std::time::{Duration, Instant};
/// use fltk::enums::Event;
/// use fltk::prelude::WidgetExt;
/// use fltk::tree::Tree;
/// use idgenerator_thin::YitIdHelper;
/// use debounce_fltk::throttle_check;
///
/// static PANEL_TASK_ID: OnceLock<i64> = OnceLock::new();
///
/// fn panel_event_handler(tree: Tree, evt: fltk::enums::Event) -> bool {
///     if tree.has_focus() && evt == Event::KeyUp {
///         println!("接收到的事件：{:?}", evt);
///         let debounce_task_id = PANEL_TASK_ID.get_or_init(|| YitIdHelper::next_id());
///         if !throttle_check(*debounce_task_id, Duration::from_millis(300)) {
///             return false;
///         }
///         println!("通过防抖检查后处理的事件: {:?}", evt);
///     }
///     false
/// }
/// ```
pub fn throttle_check(task_id: i64, limit: Duration) -> bool {
    if let Some(mut map) = DEBOUNCE_LOCK.try_write() {
        if let Some(last_start) = map.get(&task_id) {
            if last_start.elapsed() < limit {
                false
            } else {
                map.insert(task_id, Instant::now());
                true
            }
        } else {
            map.insert(task_id, Instant::now());
            true
        }
    } else {
        false
    }
}

#[cfg(test)]
mod tests {


}
