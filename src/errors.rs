#[derive(Debug, thiserror::Error)]
pub enum Error {
  #[error("generic error: {0}")]
  Generic(String),
  #[error(transparent)]
  IoError(#[from] std::io::Error),
}
