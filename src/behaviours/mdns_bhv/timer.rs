use std::time::{Duration, Instant};

#[derive(Debug)]
pub struct Timer {
  inner: Interval,
}

use ::tokio::time::{self, Instant as TokioInstant, Interval, MissedTickBehavior};
use futures::Stream;
use std::{
  pin::Pin,
  task::{Context, Poll},
};

impl Timer {
  pub fn at(instant: Instant) -> Self {
    // Taken from: https://docs.rs/async-io/1.7.0/src/async_io/lib.rs.html#91
    let mut inner = time::interval_at(
      TokioInstant::from_std(instant),
      Duration::new(std::u64::MAX, 1_000_000_000 - 1),
    );
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

impl Stream for Timer {
  type Item = TokioInstant;

  fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
    self.inner.poll_tick(cx).map(Some)
  }

  fn size_hint(&self) -> (usize, Option<usize>) {
    (std::usize::MAX, None)
  }
}
