use libp2p::gossipsub::TopicHash;

pub enum Event {
  MsgReceived { topic: TopicHash, contents: Vec<u8> },
}
