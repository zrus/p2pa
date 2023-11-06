use crate::constants::*;
use libp2p::{Multiaddr, PeerId};

#[derive(Debug, Builder, Default)]
pub struct Config {
  #[builder(setter(into, strip_option), default)]
  seed: Option<Vec<u8>>,
  #[builder(setter(into), default = "bootnodes()")]
  bootstrap_nodes: Vec<(PeerId, Multiaddr)>,
  #[builder(setter(into), default = "relay_servers()")]
  relay_nodes: Vec<(PeerId, Multiaddr)>,
  #[builder(setter(into), default = "rendezvous_points()")]
  rendezvous_nodes: Vec<(PeerId, Multiaddr)>,
  #[builder(setter(into), default)]
  rendezvous_namespaces: Vec<String>,
  #[builder(setter(strip_option), default)]
  rendezvous_ttl: Option<u64>,
  #[builder(setter(into), default = "String::new()")]
  mdns_service_name: String,
  #[builder(default)]
  enable_mdns: bool,
  #[builder(default)]
  enable_rendezvous: bool,
  #[builder(default)]
  enable_dht: bool,
  #[builder(default)]
  enable_relay: bool,
  #[builder(default)]
  enable_republish: bool,
}

impl Config {
  pub fn seed(&self) -> Option<&[u8]> {
    self.seed.as_deref()
  }

  pub fn bootstrap_nodes(&self) -> &[(PeerId, Multiaddr)] {
    &self.bootstrap_nodes
  }

  pub fn relay_nodes(&self) -> &[(PeerId, Multiaddr)] {
    &self.relay_nodes
  }

  pub fn rendezvous_points(&self) -> &[(PeerId, Multiaddr)] {
    &self.rendezvous_nodes
  }

  pub fn rendezvous_namespaces(&self) -> &[String] {
    &self.rendezvous_namespaces
  }

  pub fn rendezvous_ttl(&self) -> Option<u64> {
    self.rendezvous_ttl
  }

  pub fn mdns_service_name(&self) -> &str {
    &self.mdns_service_name
  }

  pub fn enable_mdns(&self) -> bool {
    self.enable_mdns
  }

  pub fn enable_rendezvous(&self) -> bool {
    self.enable_rendezvous
  }

  pub fn enable_dht(&self) -> bool {
    self.enable_dht
  }

  pub fn enable_relay(&self) -> bool {
    self.enable_relay
  }

  pub fn enable_republish(&self) -> bool {
    self.enable_republish
  }
}
