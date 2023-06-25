mod sleep;

pub use crate::time::Duration;
pub use sleep::Timer;

/// Sleeps for the number of specified milliseconds
pub async fn sleep(msec: u64) {
    Timer::new(Duration::millis(msec)).await
}

pub(crate) fn check_slp() {
    sleep::SLEEP_QUEUE.wakeup()
}
