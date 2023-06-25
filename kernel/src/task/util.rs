mod sleep;

pub use sleep::Timer;

pub async fn sleep(nsecs: u64) {
    Timer::new(nsecs).await
}

pub(crate) fn check_slp() {
    sleep::SLEEP_QUEUE.wakeup()
}
