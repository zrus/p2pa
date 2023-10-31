use std::collections::HashSet;

use libp2p::PeerId;
use tokio::sync::oneshot::Sender;

pub enum Command {
  // Swarm stuff
  Dial(String),
  // DHT Kad stuff
  BeginBootstrap,
  LookupPeer(String),
  AddKnownPeer { peer: String, address: String },
  AddBootNode { peer: String, address: String },
  PutRecord { key: Vec<u8>, value: Vec<u8> },
  GetRecord { key: Vec<u8>, retry: u8 },
  // Shutdown the node
  Shutdown,
  // Relay connect
  ListenViaRelay(String),
  // Rendezvous stuff
  Register { point: String, namespace: String },
  Discover { point: String, namespace: String },
  // Gossipsub stuff
  Publish { topic: String, contents: Vec<u8> },
  Subscribe(String),
  Unsubscribe(String),
  // Node utilities
  GetConnectedPeers(Sender<HashSet<PeerId>>),
  GetConnectedPeersLen(Sender<usize>),
}
