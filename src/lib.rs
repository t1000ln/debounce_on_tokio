//! 基于tokio异步上下文防抖器和超时计时器。
//! 主要用于限流执行任务，特别是这些任务需要运行在tokio的异步上下文环境。
//! 在每个限流周期内，最多执行一次预定任务。
//! 必须通过`update_param`方法更新任务参数，才能激活任务。
//! 任务参数在执行后就被消耗
//! 用法示例：
//! ```rust
//! use std::thread;
//! use std::time::{Duration, Instant};
//! use debounce_fltk::TokioDebounce;
//!
//! let start = Instant::now();
//! let mut debounced_fn = TokioDebounce::new_debounce(move |param: String| {
//!     println!("经过 {:?}, 执行任务，入参：{}", start.elapsed(), param);
//! }, Duration::from_millis(1000), false);
//! for i in 0..50 {
//!     debounced_fn.update_param(i.to_string());
//!     tokio::time::sleep(Duration::from_millis(200)).await;
//! }
//! // 每隔1000毫秒打印一次任务信息。
//! ```
//! 防抖器结构体实例被创建后，将会通过tokio工作线程或协程无限循环检查新任务，在每个`duration`参数指定的周期内检查一次，
//! 其余时间处于休眠状态。
//! <br>
//! 超时计时器用于在超过设定时限后执行任务。

use std::collections::HashMap;
use std::fmt::Debug;
use std::sync::{Arc};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use fltk::app;
use once_cell::sync::Lazy;
use parking_lot::{Mutex, RwLock};
use tokio::runtime::{Runtime};

/// 基于tokio异步上下文防抖器。
#[derive(Debug, Clone)]
pub struct TokioDebounce<T> {
    jobs: Arc<AtomicU64>,
    last_param: Arc<Mutex<Option<T>>>,
    skip_next: Arc<AtomicBool>,
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
    /// use std::sync::Arc;
    /// use std::sync::atomic::{AtomicUsize, Ordering};
    /// use std::time::{Duration, Instant};
    /// use parking_lot::RwLock;
    /// use debounce_fltk::TokioDebounce;
    ///
    /// let total_exec_count = Arc::new(AtomicUsize::new(0));
    /// let start = Arc::new(RwLock::new(Instant::now()));
    /// let mut debounce_fn = TokioDebounce::new_debounce({
    ///     let start_clone = start.clone();
    ///     let total_exec_count_clone = total_exec_count.clone();
    ///     move |param: String| {
    ///         total_exec_count_clone.fetch_add(1, Ordering::SeqCst);
    ///         println!("Debounced: {:?}，执行时刻：{:?}", param, start_clone.read().elapsed());
    ///     }
    /// }, Duration::from_millis(300), false);
    /// debounce_fn.update_param(0.to_string());
    ///
    /// for i in 1..=10 {
    ///     tokio::time::sleep(Duration::from_millis(100)).await;
    ///     debounce_fn.update_param(i.to_string());
    /// }
    /// tokio::time::sleep(Duration::from_millis(500)).await;
    /// println!("结束时刻：{:?}", start.read().elapsed());
    /// assert_eq!(total_exec_count.load(Ordering::SeqCst), 4);
    ///
    /// *start.write() = Instant::now();
    /// total_exec_count.store(0, Ordering::SeqCst);
    /// for i in 1..=10 {
    ///     tokio::time::sleep(Duration::from_millis(100)).await;
    ///     debounce_fn.update_param(i.to_string());
    ///     debounce_fn.delay_once();
    /// }
    /// tokio::time::sleep(Duration::from_millis(500)).await;
    /// println!("结束时刻：{:?}", start.read().elapsed());
    /// assert_eq!(total_exec_count.load(Ordering::SeqCst), 1);
    /// ```
    pub fn new_debounce<F>(task: F, duration: Duration, ui_thread: bool) -> Self
    where F: FnMut(T) + Send + Sync + 'static {
        let jobs = Arc::new(AtomicU64::new(0));
        let jobs_clone = jobs.clone();
        let last_param: Arc<Mutex<Option<T>>> = Arc::new(Mutex::new(None));
        let last_param_clone = last_param.clone();
        let task = Arc::new(RwLock::new(task));
        let skip_next = Arc::new(AtomicBool::new(false));
        let skip_next_once = skip_next.clone();
        tokio::spawn(async move {
            tokio::time::sleep(duration).await;
            loop {
                if !skip_next_once.load(Ordering::Relaxed) {
                    if jobs_clone.load(Ordering::Relaxed) > 0 {
                        if let Some(param) = last_param_clone.lock().take() {
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
                        jobs_clone.store(0, Ordering::SeqCst);
                    }
                } else {
                    skip_next_once.store(false, Ordering::Relaxed);
                }

                tokio::time::sleep(duration).await;
            }

        });

        Self {
            jobs,
            last_param,
            skip_next,
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
    /// use std::sync::Arc;
    /// use std::sync::atomic::{AtomicUsize, Ordering};
    /// use std::time::{Duration, Instant};
    /// use parking_lot::RwLock;
    /// use debounce_fltk::TokioDebounce;
    ///
    /// let total_exec_count = Arc::new(AtomicUsize::new(0));
    /// let start = Arc::new(RwLock::new(Instant::now()));
    /// let mut throttle_fn = TokioDebounce::new_throttle({
    ///     let start_clone = start.clone();
    ///     let total_exec_count_clone = total_exec_count.clone();
    ///     move |param: String| {
    ///         total_exec_count_clone.fetch_add(1, Ordering::SeqCst);
    ///         println!("Throttle: {:?}，执行时刻：{:?}", param, start_clone.read().elapsed());
    ///         false
    ///     }
    /// }, Duration::from_millis(300), false);
    /// throttle_fn.update_param(0.to_string());
    ///
    /// for i in 1..=10 {
    ///     tokio::time::sleep(Duration::from_millis(100)).await;
    ///     throttle_fn.update_param(i.to_string());
    /// }
    /// tokio::time::sleep(Duration::from_millis(500)).await;
    /// println!("结束时刻：{:?}", start.read().elapsed());
    /// assert_eq!(total_exec_count.load(Ordering::SeqCst), 5);
    ///
    /// *start.write() = Instant::now();
    /// total_exec_count.store(0, Ordering::SeqCst);
    /// for i in 1..=10 {
    ///     tokio::time::sleep(Duration::from_millis(100)).await;
    ///     throttle_fn.delay_once();
    ///     throttle_fn.update_param(i.to_string());
    /// }
    /// tokio::time::sleep(Duration::from_millis(500)).await;
    /// println!("结束时刻：{:?}", start.read().elapsed());
    /// assert_eq!(total_exec_count.load(Ordering::SeqCst), 1);
    /// ```
    pub fn new_throttle<F>(task: F, duration: Duration, ui_thread: bool) -> Self
    where F: FnMut(T) -> bool + Send + Sync + 'static {
        let jobs = Arc::new(AtomicU64::new(0));
        let jobs_clone = jobs.clone();
        let last_param: Arc<Mutex<Option<T>>> = Arc::new(Mutex::new(None));
        let last_param_clone = last_param.clone();
        let task = Arc::new(RwLock::new(task));
        let skip_next = Arc::new(AtomicBool::new(false));
        let skip_next_once = skip_next.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(duration);
            interval.tick().await;
            loop {
                if !skip_next_once.load(Ordering::Relaxed) {
                    if jobs_clone.load(Ordering::Relaxed) > 0 {
                        if let Some(param) = last_param_clone.lock().as_mut() {
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
                                if task.write()(param.clone()) {
                                    jobs_clone.fetch_add(1, Ordering::SeqCst);
                                } else {
                                    jobs_clone.store(0, Ordering::SeqCst);
                                }
                            }
                        }
                    }
                } else {
                    skip_next_once.store(false, Ordering::Relaxed);
                }

                interval.tick().await;
            }

        });

        Self {
            jobs,
            last_param,
            skip_next,
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
        self.last_param.lock().replace(value);
        self.jobs.fetch_add(1, Ordering::SeqCst);
    }

    /// 设置延后一个周期执行任务，如果有任务的话。
    /// 如果在每个周期内都调用该方法，可以实现无限延后执行任务，直到最后一次未调用该方法的周期结束后才最终执行一次任务。
    /// <br>
    /// 该方法与`TokioDebounce::update_param`方法没有直接联系，调用二者时不分先后顺序。
    pub fn delay_once(&mut self) {
        self.skip_next.store(true, Ordering::SeqCst);
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

/// 超时任务类型。
#[derive(Debug, Clone, Copy)]
pub enum TimeoutType {
    /// 一次性任务，执行完就退出。
    Once,
    /// 重复性超时任务，以接近固定的周期自动重复执行。
    Repeat,
    /// 单次超时任务，但执行完并不退出，而是进入休眠等待唤醒后可再执行单次任务。
    /// 使用这种类型可以不同的超时间隔，多次执行超时任务，而不必多次创建任务实例。
    OnceMore
}

/// 超时后执行的任务。<br>
/// 在超时前调用`update_param`方法更新参数。<br>
/// 在超时前调用`touch`方法重新计时。<br>
/// 这个任务计时器不能精准计时，可能会有微秒到毫秒级的误差。
#[derive(Debug, Clone)]
pub struct TimeoutTask<T> {
    last_param: Arc<Mutex<Option<T>>>,
    last_tick: Arc<Mutex<Instant>>,
    timeout_type: Arc<RwLock<TimeoutType>>,
    thread_handler: Arc<Mutex<JoinHandle<()>>>
}

impl<T: Debug + Clone + Send + 'static> TimeoutTask<T> {
    /// 创建新的超时任务计时器。
    ///
    /// # Arguments
    ///
    /// * `task`: 回调任务。
    /// * `timeout`: 超时设定。
    /// * `ui_thread`: 是否在`fltk`的`UI主线程`中执行任务。
    /// * `repeat`: 是否重复执行任务。若为`true`则会按照超时间隔重复执行任务。
    ///
    /// returns: TimeoutTask<T>
    ///
    /// # Examples
    ///
    /// ```
    /// use std::sync::Arc;
    /// use std::sync::atomic::{AtomicUsize, Ordering};
    /// use std::time::{Duration, Instant};
    /// use parking_lot::RwLock;
    /// use debounce_fltk::{TimeoutTask, TimeoutType};
    ///
    /// let total_exec_count = Arc::new(AtomicUsize::new(0));
    /// let start = Arc::new(RwLock::new(Instant::now()));
    /// let mut timeout_fn = TimeoutTask::new({
    ///     let start_clone = start.clone();
    ///     let total_exec_count_clone = total_exec_count.clone();
    ///     move |p| {
    ///         total_exec_count_clone.fetch_add(1, Ordering::SeqCst);
    ///         println!("Timeout: {:?}，执行时刻：{:?}", p, start_clone.read().elapsed());
    ///     }
    /// }, Duration::from_millis(300), false, TimeoutType::Repeat);
    /// timeout_fn.feed_param(());
    ///
    /// for _ in 1..=10 {
    ///     tokio::time::sleep(Duration::from_millis(100)).await;
    ///     timeout_fn.touch();
    /// }
    /// tokio::time::sleep(Duration::from_millis(500)).await;
    /// assert_eq!(total_exec_count.load(Ordering::SeqCst), 1);
    /// total_exec_count.store(0, Ordering::SeqCst);
    /// timeout_fn.touch();
    /// println!("重新计数时刻：{:?}", start.read().elapsed());
    ///
    /// // timeout_fn.stop_repeat();
    /// tokio::time::sleep(Duration::from_millis(2000)).await;
    /// assert_eq!(total_exec_count.load(Ordering::SeqCst), 6);
    /// println!("结束时刻：{:?}", start.read().elapsed());
    /// ```
    pub fn new<F>(task: F, timeout: Duration, ui_thread: bool, timeout_type: TimeoutType) -> Self
    where F: FnMut(T) + Clone + Send + Sync +'static{
        let last_param = Arc::new(Mutex::new(None));
        let last_tick = Arc::new(Mutex::new(Instant::now()));
        let timeout_type = Arc::new(RwLock::new(timeout_type));
        let jh = thread::spawn({
            let ui_thread = ui_thread.clone();
            let last_param = last_param.clone();
            let last_tick = last_tick.clone();
            let timeout_type = timeout_type.clone();
            let task = task.clone();
            move || {
                let tt = {timeout_type.read().clone()};
                match tt {
                    TimeoutType::Once | TimeoutType::OnceMore => {
                        let lack_of_param = { last_param.lock().is_none() };
                        if lack_of_param {
                            thread::park();
                        }
                    }
                    _ => {}
                }

                loop {
                    Runtime::new().unwrap().block_on({
                        let ui_thread = ui_thread.clone();
                        let last_param = last_param.clone();
                        let last_tick = last_tick.clone();
                        let timeout_type = timeout_type.clone();
                        let mut task = task.clone();
                        async move {
                            loop {
                                loop {
                                    // 检查最近一次的更新时间到当前时刻是否超过预定时限
                                    let tmp_dur = last_tick.lock().elapsed();
                                    // println!("距离上次更新过去时间：{:?}", tmp_dur);
                                    if let Some(left) = timeout.clone().checked_sub(tmp_dur) {
                                        // 最近一次更新时间与当前时间的时差小于预定时限，则休眠这个时差后再次检查
                                        tokio::time::sleep(left).await;
                                    } else {
                                        // 如果最近一次更新时间与当前时刻之间的时差已经大于等于预定时限，则开始执行预定任务。
                                        break;
                                    }
                                }
                                *last_tick.lock() = Instant::now();

                                if ui_thread {
                                    app::awake_callback({
                                        let lp = last_param.clone();
                                        let mut task = task.clone();
                                        let timeout_type = timeout_type.clone();
                                        move || {
                                            let tt = { timeout_type.read().clone() };
                                            match tt {
                                                TimeoutType::Once | TimeoutType::OnceMore => {
                                                    if let Some(p) = lp.lock().take() {
                                                        task(p);
                                                    }
                                                }
                                                TimeoutType::Repeat => {
                                                    if let Some(p) = lp.lock().clone() {
                                                        task(p);
                                                    }
                                                }
                                            }
                                        }
                                    });
                                    let tt = { timeout_type.read().clone() };
                                    match tt {
                                        TimeoutType::Once | TimeoutType::OnceMore => {
                                            break;
                                        }
                                        TimeoutType::Repeat => {
                                            continue;
                                        }
                                    }
                                } else {
                                    let tt = { timeout_type.read().clone() };
                                    match tt {
                                        TimeoutType::Once | TimeoutType::OnceMore => {
                                            if let Some(p) = last_param.lock().take() {
                                                task(p);
                                            }
                                            break;
                                        },
                                        TimeoutType::Repeat => {
                                            if let Some(p) = last_param.lock().clone() {
                                                task(p);
                                            }
                                            continue;
                                        },
                                    }
                                }
                            }
                        }
                    });

                    let tt = { timeout_type.read().clone() };
                    match tt {
                        TimeoutType::Once | TimeoutType::Repeat => {
                            break;
                        }
                        TimeoutType::OnceMore => {
                            thread::park();
                            continue;
                        }
                    }
                }
            }
        });

        Self {
            last_param,
            last_tick,
            timeout_type,
            thread_handler: Arc::new(Mutex::new(jh))
        }
    }

    /// 更新待执行任务的入参。
    ///
    /// # Arguments
    ///
    /// * `new_param`: 新的任务参数。
    ///
    /// returns: ()
    ///
    /// # Examples
    ///
    /// ```
    ///
    /// ```
    pub fn feed_param(&mut self, new_param: T) {
        self.last_param.lock().replace(new_param);
        self.thread_handler.lock().thread().unpark();
    }

    /// 重置计时器，将计时器起始点设置为当前时刻。
    pub fn touch(&mut self) {
        *self.last_tick.lock() = Instant::now();
    }

    /// 如果当前任务初始化为重复执行，则改变重复执行的行为成单次执行。即执行完本次超时任务后退出计时器。
    pub fn stop_repeat(&mut self) {
        *self.timeout_type.write() = TimeoutType::Once;
    }

    /// 当超时任务类型为`TimeoutType::OnceMore`时，填充新的任务参数，再执行一次超时任务。
    /// 调用该方法后，且在未超时前仍可使用`feed_param()`方法覆盖执行参数。
    ///
    /// # Arguments
    ///
    /// * `new_param`:
    ///
    /// returns: ()
    ///
    /// # Examples
    ///
    /// ```
    /// use std::sync::Arc;
    /// use std::time::{Duration, Instant};
    /// use parking_lot::RwLock;
    /// use debounce_fltk::{TimeoutTask, TimeoutType};
    ///
    /// let init_time = Arc::new(RwLock::new(Instant::now()));
    /// let mut timeout_fn = TimeoutTask::new({
    ///     let init_time_clone = init_time.clone();
    ///     move |()| {
    ///         println!("Timeout 执行时刻：{:?}", init_time_clone.read().elapsed());
    ///     }
    /// }, Duration::from_millis(300), false, TimeoutType::OnceMore);
    /// timeout_fn.feed_param(());
    ///
    /// tokio::time::sleep(Duration::from_millis(1000)).await;
    ///
    /// timeout_fn.once_more_with_param(());
    /// tokio::time::sleep(Duration::from_millis(3000)).await;
    ///
    /// timeout_fn.once_more_with_param(());
    /// tokio::time::sleep(Duration::from_millis(500)).await;
    /// ```
    pub fn once_more_with_param(&mut self, new_param: T) {
        let tt = { self.timeout_type.read().clone() };
        if let TimeoutType::OnceMore = tt {
            self.feed_param(new_param);
            self.touch();
            self.thread_handler.lock().thread().unpark();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};
    use parking_lot::RwLock;
    use crate::{TimeoutTask, TimeoutType, TokioDebounce};

    #[tokio::test]
    pub async fn debounce_test() {
        let total_exec_count = Arc::new(AtomicUsize::new(0));
        let start = Arc::new(RwLock::new(Instant::now()));
        let mut debounce_fn = TokioDebounce::new_debounce({
            let start_clone = start.clone();
            let total_exec_count_clone = total_exec_count.clone();
            move |param: String| {
              total_exec_count_clone.fetch_add(1, Ordering::SeqCst);
              println!("Debounced: {:?}，执行时刻：{:?}", param, start_clone.read().elapsed());
            }
        }, Duration::from_millis(300), false);
        debounce_fn.update_param(0.to_string());

        for i in 1..=10 {
            tokio::time::sleep(Duration::from_millis(100)).await;
            debounce_fn.update_param(i.to_string());
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
        println!("结束时刻：{:?}", start.read().elapsed());
        assert_eq!(total_exec_count.load(Ordering::SeqCst), 4);

        *start.write() = Instant::now();
        total_exec_count.store(0, Ordering::SeqCst);
        for i in 1..=10 {
            tokio::time::sleep(Duration::from_millis(100)).await;
            debounce_fn.update_param(i.to_string());
            debounce_fn.delay_once();
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
        println!("结束时刻：{:?}", start.read().elapsed());
        assert_eq!(total_exec_count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    pub async fn throttle_test() {
        let total_exec_count = Arc::new(AtomicUsize::new(0));
        let start = Arc::new(RwLock::new(Instant::now()));
        let mut throttle_fn = TokioDebounce::new_throttle({
            let start_clone = start.clone();
            let total_exec_count_clone = total_exec_count.clone();
            move |param: String| {
                total_exec_count_clone.fetch_add(1, Ordering::SeqCst);
                println!("Throttle: {:?}，执行时刻：{:?}", param, start_clone.read().elapsed());
                false
            }
        }, Duration::from_millis(300), false);
        throttle_fn.update_param(0.to_string());

        for i in 1..=10 {
            tokio::time::sleep(Duration::from_millis(100)).await;
            throttle_fn.update_param(i.to_string());
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
        println!("结束时刻：{:?}", start.read().elapsed());
        assert_eq!(total_exec_count.load(Ordering::SeqCst), 5);

        *start.write() = Instant::now();
        total_exec_count.store(0, Ordering::SeqCst);
        for i in 1..=10 {
            tokio::time::sleep(Duration::from_millis(100)).await;
            throttle_fn.delay_once();
            throttle_fn.update_param(i.to_string());
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
        println!("结束时刻：{:?}", start.read().elapsed());
        assert_eq!(total_exec_count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    pub async fn timeout_test() {
        let total_exec_count = Arc::new(AtomicUsize::new(0));
        let start = Arc::new(RwLock::new(Instant::now()));
        let mut timeout_fn = TimeoutTask::new({
            let start_clone = start.clone();
            let total_exec_count_clone = total_exec_count.clone();
            move |p| {
                total_exec_count_clone.fetch_add(1, Ordering::SeqCst);
                println!("Timeout: {:?}，执行时刻：{:?}", p, start_clone.read().elapsed());
            }
        }, Duration::from_millis(300), false, TimeoutType::Repeat);
        tokio::time::sleep(Duration::from_millis(500)).await;
        timeout_fn.feed_param(());

        for _ in 1..=10 {
            tokio::time::sleep(Duration::from_millis(100)).await;
            timeout_fn.touch();
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert_eq!(total_exec_count.load(Ordering::SeqCst), 1);
        total_exec_count.store(0, Ordering::SeqCst);
        timeout_fn.touch();
        println!("重新计数时刻：{:?}", start.read().elapsed());

        // timeout_fn.stop_repeat();
        tokio::time::sleep(Duration::from_millis(2000)).await;
        assert_eq!(total_exec_count.load(Ordering::SeqCst), 6);
        println!("结束时刻：{:?}", start.read().elapsed());
    }

    #[tokio::test]
    pub async fn timeout_once_more_test() {
        let init_time = Arc::new(RwLock::new(Instant::now()));
        let mut timeout_fn = TimeoutTask::new({
            let init_time_clone = init_time.clone();
            move |()| {
                println!("Timeout 执行时刻：{:?}", init_time_clone.read().elapsed());
            }
        }, Duration::from_millis(300), false, TimeoutType::OnceMore);
        timeout_fn.feed_param(());

        tokio::time::sleep(Duration::from_millis(1000)).await;

        timeout_fn.once_more_with_param(());
        tokio::time::sleep(Duration::from_millis(3000)).await;

        timeout_fn.once_more_with_param(());
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    #[tokio::test]
    pub async fn debounce_test2() {
        let start = Instant::now();
        let mut debounced_fn = TokioDebounce::new_debounce(move |param: String| {
            println!("经过 {:?}, 执行任务，入参：{}", start.elapsed(), param);
        }, Duration::from_millis(1000), false);
        for i in 0..50 {
            debounced_fn.update_param(i.to_string());
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }
}
