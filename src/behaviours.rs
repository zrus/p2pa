pub mod dht_bhv;
pub mod gossip_bhv;
pub mod mdns_bhv;
pub mod rendezvous_bhv;

use libp2p::{
  dcutr, identify, relay,
  swarm::{behaviour::toggle::Toggle, NetworkBehaviour},
};

#[derive(NetworkBehaviour)]
pub struct NodeBehaviour {
  pub identify: identify::Behaviour,
  pub gossip: gossip_bhv::Behaviour,
  pub mdns: Toggle<mdns_bhv::Behaviour>,
  pub rdvz: Toggle<rendezvous_bhv::Behaviour>,
  pub dht: Toggle<dht_bhv::Behaviour>,
  pub dcutr: Toggle<dcutr::Behaviour>,
  pub relay: Toggle<relay::client::Behaviour>,
}
