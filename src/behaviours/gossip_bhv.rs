use std::{
  collections::{HashSet, VecDeque},
  task::Poll,
};

use libp2p::{
  gossipsub::{self, IdentTopic, TopicHash},
  swarm::{NetworkBehaviour, ToSwarm},
  PeerId,
};

use crate::utils::ExponentialBackoff;

#[derive(Debug)]
pub enum Event {
  MsgReceived { topic: TopicHash, contents: Vec<u8> },
}

pub struct Behaviour {
  gossip: gossipsub::Behaviour,
  in_progress_messages: VecDeque<(IdentTopic, Vec<u8>)>,
  backoff: ExponentialBackoff,
  event_queue: Vec<Event>,
  subsriptions: HashSet<String>,
  enable_republish: bool,
}

impl Behaviour {
  pub fn new(behaviour: gossipsub::Behaviour, enable_republish: bool) -> Self {
    Self {
      gossip: behaviour,
      in_progress_messages: Default::default(),
      backoff: Default::default(),
      event_queue: Default::default(),
      subsriptions: Default::default(),
      enable_republish,
    }
  }

  pub fn add_explicit_peers(&mut self, peers: Vec<PeerId>) {
    for peer in peers {
      info!("added peer {peer}");
      self.gossip.add_explicit_peer(&peer);
    }
  }

  pub fn publish(&mut self, topic: impl Into<String>, contents: impl Into<Vec<u8>>) {
    let topic = IdentTopic::new(topic.into());
    let contents = contents.into();
    if let Err(e) = self.gossip.publish(topic.clone(), contents.clone()) {
      error!("publish error: {e}");
      if self.enable_republish {
        self.in_progress_messages.push_back((topic, contents));
      }
    }
  }

  pub fn subscribe(&mut self, topic: impl Into<String>) {
    let topic = topic.into();
    if !self.subsriptions.contains(&topic) {
      let itopic = IdentTopic::new(&topic);
      if let Err(e) = self.gossip.subscribe(&itopic) {
        error!("subscribe error: {e}");
      }
      self.subsriptions.insert(topic.to_owned());
    }
  }

  pub fn unsubscribe(&mut self, topic: impl Into<String>) {
    let topic = topic.into();
    if self.subsriptions.contains(&topic) {
      let itopic = IdentTopic::new(&topic);
      if let Err(e) = self.gossip.unsubscribe(&itopic) {
        error!("unsubscribe error: {e}");
      }
      self.subsriptions.remove(&topic);
    }
  }

  pub fn drain_publish(&mut self) -> bool {
    while let Some((topic, contents)) = self.in_progress_messages.pop_front() {
      if self
        .gossip
        .publish(topic.clone(), contents.clone())
        .map(|_| ())
        .map_err(|_| self.in_progress_messages.push_back((topic, contents)))
        .is_err()
      {
        return false;
      }
    }
    true
  }

  fn handle_event(&mut self, event: gossipsub::Event) {
    use gossipsub::Event::*;
    // TODO: Handle other events
    if let Message { message, .. } = event {
      self.event_queue.push(Event::MsgReceived {
        topic: message.topic,
        contents: message.data,
      });
    }
  }
}

impl NetworkBehaviour for Behaviour {
  type ConnectionHandler = <gossipsub::Behaviour as NetworkBehaviour>::ConnectionHandler;

  type ToSwarm = Event;

  fn handle_established_inbound_connection(
    &mut self,
    connection_id: libp2p::swarm::ConnectionId,
    peer: libp2p::PeerId,
    local_addr: &libp2p::Multiaddr,
    remote_addr: &libp2p::Multiaddr,
  ) -> Result<libp2p::swarm::THandler<Self>, libp2p::swarm::ConnectionDenied> {
    self
      .gossip
      .handle_established_inbound_connection(connection_id, peer, local_addr, remote_addr)
  }

  fn handle_established_outbound_connection(
    &mut self,
    connection_id: libp2p::swarm::ConnectionId,
    peer: libp2p::PeerId,
    addr: &libp2p::Multiaddr,
    role_override: libp2p::core::Endpoint,
  ) -> Result<libp2p::swarm::THandler<Self>, libp2p::swarm::ConnectionDenied> {
    self
      .gossip
      .handle_established_outbound_connection(connection_id, peer, addr, role_override)
  }

  fn handle_pending_inbound_connection(
    &mut self,
    connection_id: libp2p::swarm::ConnectionId,
    local_addr: &libp2p::Multiaddr,
    remote_addr: &libp2p::Multiaddr,
  ) -> Result<(), libp2p::swarm::ConnectionDenied> {
    self
      .gossip
      .handle_pending_inbound_connection(connection_id, local_addr, remote_addr)
  }

  fn handle_pending_outbound_connection(
    &mut self,
    connection_id: libp2p::swarm::ConnectionId,
    maybe_peer: Option<libp2p::PeerId>,
    addresses: &[libp2p::Multiaddr],
    effective_role: libp2p::core::Endpoint,
  ) -> Result<Vec<libp2p::Multiaddr>, libp2p::swarm::ConnectionDenied> {
    self.gossip.handle_pending_outbound_connection(
      connection_id,
      maybe_peer,
      addresses,
      effective_role,
    )
  }

  fn on_swarm_event(&mut self, event: libp2p::swarm::FromSwarm) {
    self.gossip.on_swarm_event(event)
  }

  fn on_connection_handler_event(
    &mut self,
    peer_id: libp2p::PeerId,
    connection_id: libp2p::swarm::ConnectionId,
    event: libp2p::swarm::THandlerOutEvent<Self>,
  ) {
    self
      .gossip
      .on_connection_handler_event(peer_id, connection_id, event)
  }

  fn poll(
    &mut self,
    cx: &mut std::task::Context<'_>,
  ) -> std::task::Poll<libp2p::swarm::ToSwarm<Self::ToSwarm, libp2p::swarm::THandlerInEvent<Self>>>
  {
    if self.enable_republish && self.backoff.is_expired() {
      let all_republished = self.drain_publish();
      self.backoff.start_next(all_republished);
    }
    while let Poll::Ready(ready) = NetworkBehaviour::poll(&mut self.gossip, cx) {
      match ready {
        ToSwarm::GenerateEvent(event) => self.handle_event(event),
        ToSwarm::Dial { opts } => {
          return Poll::Ready(ToSwarm::Dial { opts });
        }
        ToSwarm::NotifyHandler {
          peer_id,
          handler,
          event,
        } => {
          return Poll::Ready(ToSwarm::NotifyHandler {
            peer_id,
            handler,
            event,
          });
        }
        ToSwarm::CloseConnection {
          peer_id,
          connection,
        } => {
          return Poll::Ready(ToSwarm::CloseConnection {
            peer_id,
            connection,
          });
        }
        ToSwarm::ListenOn { opts } => {
          return Poll::Ready(ToSwarm::ListenOn { opts });
        }
        ToSwarm::RemoveListener { id } => {
          return Poll::Ready(ToSwarm::RemoveListener { id });
        }
        ToSwarm::NewExternalAddrCandidate(c) => {
          return Poll::Ready(ToSwarm::NewExternalAddrCandidate(c));
        }
        ToSwarm::ExternalAddrConfirmed(c) => {
          return Poll::Ready(ToSwarm::ExternalAddrConfirmed(c));
        }
        ToSwarm::ExternalAddrExpired(c) => {
          return Poll::Ready(ToSwarm::ExternalAddrExpired(c));
        }
        _ => {}
      }
    }
    if !self.event_queue.is_empty() {
      return Poll::Ready(ToSwarm::GenerateEvent(self.event_queue.remove(0)));
    }
    Poll::Pending
  }
}
