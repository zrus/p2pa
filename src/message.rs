#[derive(Debug)]
pub struct Message {
  pub topic: String,
  pub contents: Vec<u8>,
}
