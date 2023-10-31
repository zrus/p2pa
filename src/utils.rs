use std::time::{Duration, Instant};

use tokio::sync::{mpsc::UnboundedSender, Mutex};

pub fn is_loopback(addr: &libp2p::Multiaddr) -> bool {
  addr
    .iter()
    .any(|p| p == libp2p::multiaddr::Protocol::Ip4([127, 0, 0, 1].into()))
}

pub mod unbounded_channel {
  use crate::prelude::*;
  pub use tokio::sync::mpsc::error::{
    SendError as UnboundedSendError, TryRecvError as UnboundedTryRecvError,
  };
  use tokio::sync::mpsc::{UnboundedReceiver as InnerReceiver, UnboundedSender as InnerSender};

  /// A receiver error returned from [`UnboundedReceiver`]'s `recv`
  #[derive(Debug, PartialEq, Eq, Clone)]
  pub struct UnboundedRecvError;

  impl std::fmt::Display for UnboundedRecvError {
    fn fmt(&self, fmt: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
      write!(fmt, stringify!(UnboundedRecvError))
    }
  }

  impl std::error::Error for UnboundedRecvError {}

  use tokio::sync::Mutex;

  pub struct UnboundedSender<T>(pub(super) InnerSender<T>);
  pub struct UnboundedReceiver<T>(pub(super) Mutex<InnerReceiver<T>>);

  #[must_use]
  pub fn unbounded<T>() -> (UnboundedSender<T>, UnboundedReceiver<T>) {
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    let receiver = Mutex::new(receiver);
    (UnboundedSender(sender), UnboundedReceiver(receiver))
  }

  impl<T> UnboundedSender<T> {
    pub fn send(&self, msg: T) -> Ret<(), UnboundedSendError<T>> {
      self.0.send(msg)
    }
  }

  impl<T> UnboundedReceiver<T> {
    pub async fn recv(&self) -> Ret<T, UnboundedRecvError> {
      self.0.lock().await.recv().await.ok_or(UnboundedRecvError)
    }
  }

  impl<T> Clone for UnboundedSender<T> {
    fn clone(&self) -> Self {
      Self(self.0.clone())
    }
  }

  impl<T> std::fmt::Debug for UnboundedSender<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
      f.debug_struct("UnboundedSender").finish()
    }
  }

  impl<T> std::fmt::Debug for UnboundedReceiver<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
      f.debug_struct("UnboundedReceiver").finish()
    }
  }
}

#[derive(Debug)]
pub struct ExponentialBackoff {
  reset_val: Duration,
  backoff_factor: u32,
  timeout: Duration,
  started: Option<Instant>,
}

impl ExponentialBackoff {
  pub fn new(backoff_factor: u32, next_timeout: Duration) -> Self {
    Self {
      reset_val: next_timeout,
      backoff_factor,
      timeout: next_timeout * backoff_factor,
      started: None,
    }
  }

  #[allow(unused)]
  pub fn reset(&mut self) {
    self.timeout = self.reset_val;
  }

  pub fn start_next(&mut self, result: bool) {
    if result {
      self.timeout = self.reset_val;
      self.started = Some(Instant::now());
    } else {
      if let Some(r) = self.timeout.checked_mul(self.backoff_factor) {
        self.timeout = r;
      }
      self.started = Some(Instant::now())
    }
  }

  pub fn is_expired(&self) -> bool {
    if let Some(then) = self.started {
      then.elapsed() > self.timeout
    } else {
      true
    }
  }
}

impl Default for ExponentialBackoff {
  fn default() -> Self {
    Self {
      reset_val: Duration::from_millis(500),
      backoff_factor: 2,
      timeout: Duration::from_millis(500),
      started: None,
    }
  }
}

#[derive(Default)]
pub struct SubscribableMutex<T: ?Sized> {
  subscribers: Mutex<Vec<UnboundedSender<()>>>,
  mutex: Mutex<T>,
}

impl<T> SubscribableMutex<T> {
  pub fn new(t: T) -> Self {
    Self {
      subscribers: Mutex::default(),
      mutex: Mutex::new(t),
    }
  }

  pub async fn notify_change_subscribers(&self) {
    let mut lock = self.subscribers.lock().await;
    let mut idx_to_remove = Vec::new();
    for (idx, sender) in lock.iter().enumerate() {
      if sender.send(()).is_err() {
        idx_to_remove.push(idx);
      }
    }

    for idx in idx_to_remove.into_iter().rev() {
      lock.remove(idx);
    }
  }

  pub async fn modify<F>(&self, cb: F)
  where
    F: FnOnce(&mut T),
  {
    let mut lock = self.mutex.lock().await;
    cb(&mut *lock);
    drop(lock);
    self.notify_change_subscribers().await;
  }
}

impl<T: Clone> SubscribableMutex<T> {
  pub async fn cloned(&self) -> T {
    self.mutex.lock().await.clone()
  }
}

impl<T: std::fmt::Debug> std::fmt::Debug for SubscribableMutex<T> {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    /// Helper struct to be shown when the inner mutex is locked.
    struct Locked;
    impl std::fmt::Debug for Locked {
      fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("<locked>")
      }
    }

    match self.mutex.try_lock() {
      Err(_) => f
        .debug_struct("SubscribableMutex")
        .field("data", &Locked)
        .finish(),
      Ok(guard) => f
        .debug_struct("SubscribableMutex")
        .field("data", &&*guard)
        .finish(),
    }
  }
}
