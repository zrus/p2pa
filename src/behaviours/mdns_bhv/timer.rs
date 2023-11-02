use std::{
  pin::Pin,
  task::{Context, Poll},
  time::{Duration, Instant},
};

/// Simple wrapper for the different type of timers
#[derive(Debug)]
pub struct Timer {
  inner: Interval,
}

use ::tokio::time::{self, Instant as TokioInstant, Interval, MissedTickBehavior};
use futures::Stream;

pub type TokioTimer = Timer;
impl TokioTimer {
  pub fn at(instant: Instant) -> Self {
    let mut inner = time::interval_at(TokioInstant::from_std(instant), Duration::MAX);
    inner.set_missed_tick_behavior(MissedTickBehavior::Skip);
    Self { inner }
  }

  pub fn interval(duration: Duration) -> Self {
    let mut inner = time::interval_at(TokioInstant::now() + duration, duration);
    inner.set_missed_tick_behavior(MissedTickBehavior::Skip);
    Self { inner }
  }

  pub fn interval_at(start: Instant, duration: Duration) -> Self {
    let mut inner = time::interval_at(TokioInstant::from_std(start), duration);
    inner.set_missed_tick_behavior(MissedTickBehavior::Skip);
    Self { inner }
  }
}

impl Stream for TokioTimer {
  type Item = TokioInstant;

  fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
    self.inner.poll_tick(cx).map(Some)
  }

  fn size_hint(&self) -> (usize, Option<usize>) {
    (std::usize::MAX, None)
  }
}
