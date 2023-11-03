use std::{collections::HashSet, task::Poll};

use libp2p::{
  mdns,
  swarm::{NetworkBehaviour, PollParameters, ToSwarm},
  Multiaddr, PeerId,
};

#[derive(Debug)]
pub enum Event {
  Discovered { peers: HashSet<(PeerId, Multiaddr)> },
}

pub struct Behaviour {
  mdns: mdns::tokio::Behaviour,
  event_queue: Vec<Event>,
}

impl Behaviour {
  pub fn new(behaviour: mdns::tokio::Behaviour) -> Self {
    Self {
      mdns: behaviour,
      event_queue: Default::default(),
    }
  }

  fn handle_event(&mut self, event: mdns::Event) {
    use mdns::Event::*;
    match event {
      Discovered(list) => self.event_queue.push(Event::Discovered {
        peers: HashSet::from_iter(list.into_iter()),
      }),
      _ => {}
    }
  }
}

impl NetworkBehaviour for Behaviour {
  type ConnectionHandler = <mdns::tokio::Behaviour as NetworkBehaviour>::ConnectionHandler;

  type ToSwarm = Event;

  fn handle_established_inbound_connection(
    &mut self,
    connection_id: libp2p::swarm::ConnectionId,
    peer: PeerId,
    local_addr: &Multiaddr,
    remote_addr: &Multiaddr,
  ) -> Result<libp2p::swarm::THandler<Self>, libp2p::swarm::ConnectionDenied> {
    self
      .mdns
      .handle_established_inbound_connection(connection_id, peer, local_addr, remote_addr)
  }

  fn handle_established_outbound_connection(
    &mut self,
    connection_id: libp2p::swarm::ConnectionId,
    peer: PeerId,
    addr: &Multiaddr,
    role_override: libp2p::core::Endpoint,
  ) -> Result<libp2p::swarm::THandler<Self>, libp2p::swarm::ConnectionDenied> {
    self
      .mdns
      .handle_established_outbound_connection(connection_id, peer, addr, role_override)
  }

  fn handle_pending_inbound_connection(
    &mut self,
    connection_id: libp2p::swarm::ConnectionId,
    local_addr: &Multiaddr,
    remote_addr: &Multiaddr,
  ) -> Result<(), libp2p::swarm::ConnectionDenied> {
    self
      .mdns
      .handle_pending_inbound_connection(connection_id, local_addr, remote_addr)
  }

  fn handle_pending_outbound_connection(
    &mut self,
    connection_id: libp2p::swarm::ConnectionId,
    maybe_peer: Option<PeerId>,
    addresses: &[Multiaddr],
    effective_role: libp2p::core::Endpoint,
  ) -> Result<Vec<Multiaddr>, libp2p::swarm::ConnectionDenied> {
    self.mdns.handle_pending_outbound_connection(
      connection_id,
      maybe_peer,
      addresses,
      effective_role,
    )
  }

  fn on_swarm_event(&mut self, event: libp2p::swarm::FromSwarm<Self::ConnectionHandler>) {
    self.mdns.on_swarm_event(event)
  }

  fn on_connection_handler_event(
    &mut self,
    peer_id: PeerId,
    connection_id: libp2p::swarm::ConnectionId,
    event: libp2p::swarm::THandlerOutEvent<Self>,
  ) {
    self
      .mdns
      .on_connection_handler_event(peer_id, connection_id, event)
  }

  fn poll(
    &mut self,
    cx: &mut std::task::Context<'_>,
    params: &mut impl PollParameters,
  ) -> std::task::Poll<ToSwarm<Self::ToSwarm, libp2p::swarm::THandlerInEvent<Self>>> {
    while let Poll::Ready(ready) = NetworkBehaviour::poll(&mut self.mdns, cx, params) {
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
      }
    }
    if !self.event_queue.is_empty() {
      return Poll::Ready(ToSwarm::GenerateEvent(self.event_queue.remove(0)));
    }
    Poll::Pending
  }
}
