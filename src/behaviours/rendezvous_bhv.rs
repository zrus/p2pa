use std::{collections::HashSet, task::Poll};

use libp2p::{
  core::ConnectedPoint,
  multiaddr::Protocol,
  rendezvous::{self, Cookie, Namespace, Ttl},
  swarm::{FromSwarm, NetworkBehaviour, ToSwarm},
  Multiaddr, PeerId,
};

use crate::{prelude::*, utils::ExponentialBackoff};

#[derive(Debug)]
pub enum Event {
  Discovered(HashSet<(PeerId, Multiaddr)>),
  Registered(Namespace),
}

pub struct Behaviour {
  current_id: PeerId,
  rdvz: rendezvous::client::Behaviour,
  cookie: Option<Cookie>,
  in_progress_register: HashSet<(Multiaddr, Namespace)>,
  in_progress_discovery: HashSet<(Multiaddr, Namespace)>,
  connected_rdvzs: Vec<(PeerId, Multiaddr)>,
  backoff: Option<ExponentialBackoff>,
  event_queue: Vec<Event>,
  ttl: Option<Ttl>,
}

impl Behaviour {
  pub fn new(
    behaviour: rendezvous::client::Behaviour,
    current_id: PeerId,
    ttl: Option<Ttl>,
  ) -> Self {
    Self {
      current_id,
      rdvz: behaviour,
      cookie: Default::default(),
      in_progress_register: Default::default(),
      in_progress_discovery: Default::default(),
      connected_rdvzs: Default::default(),
      backoff: Default::default(),
      event_queue: Default::default(),
      ttl,
    }
  }

  pub fn drain_discover(&mut self) {
    for (peer, addr) in &self.connected_rdvzs {
      for (rdvz_addr, namespace) in &self.in_progress_register {
        if *addr == *rdvz_addr {
          if let Err(e) = self.rdvz.register(namespace.clone(), *peer, self.ttl) {
            error!("register rendezvous failed: {e}");
          }
        }
      }
      for (rdvz_addr, namespace) in &self.in_progress_discovery {
        if *addr == *rdvz_addr {
          self
            .rdvz
            .discover(Some(namespace.clone()), self.cookie.clone(), None, *peer)
        }
      }
    }
  }

  pub fn register(&mut self, rdvz_node: Multiaddr, namespace: impl Into<String>) -> Ret {
    let namespace = namespace.into();
    self
      .in_progress_register
      .insert((rdvz_node, Namespace::new(namespace.into())?));
    Ok(())
  }

  pub fn discover(&mut self, rdvz_node: Multiaddr, namespace: impl Into<String>) -> Ret {
    self
      .in_progress_discovery
      .insert((rdvz_node, Namespace::new(namespace.into())?));
    Ok(())
  }

  fn handle_event(&mut self, event: rendezvous::client::Event) {
    use rendezvous::client::Event::*;
    match event {
      Discovered {
        rendezvous_node,
        registrations,
        cookie,
      } => {
        self.cookie.replace(cookie);

        let mut discovered = HashSet::new();
        for registration in registrations {
          let peer = registration.record.peer_id();
          let namespace = registration.namespace;
          if peer == self.current_id {
            continue;
          }

          self.in_progress_discovery.retain(|info| {
            !(info.0.iter().any(|p| p == Protocol::P2p(rendezvous_node)) && info.1 == namespace)
          });

          for address in registration.record.addresses() {
            discovered.insert((peer, address.clone()));
          }
        }
        self.event_queue.push(Event::Discovered(discovered));
      }
      Registered {
        rendezvous_node,
        namespace,
        ..
      } => {
        self.in_progress_register.retain(|info| {
          !(info.0.iter().any(|p| p == Protocol::P2p(rendezvous_node)) && info.1 == namespace)
        });
        self.event_queue.push(Event::Registered(namespace));
      }
      _ => {}
    }
  }
}

impl NetworkBehaviour for Behaviour {
  type ConnectionHandler = <rendezvous::client::Behaviour as NetworkBehaviour>::ConnectionHandler;

  type ToSwarm = Event;

  fn handle_established_inbound_connection(
    &mut self,
    connection_id: libp2p::swarm::ConnectionId,
    peer: libp2p::PeerId,
    local_addr: &libp2p::Multiaddr,
    remote_addr: &libp2p::Multiaddr,
  ) -> Result<libp2p::swarm::THandler<Self>, libp2p::swarm::ConnectionDenied> {
    self
      .rdvz
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
      .rdvz
      .handle_established_outbound_connection(connection_id, peer, addr, role_override)
  }

  fn handle_pending_inbound_connection(
    &mut self,
    connection_id: libp2p::swarm::ConnectionId,
    local_addr: &Multiaddr,
    remote_addr: &Multiaddr,
  ) -> Result<(), libp2p::swarm::ConnectionDenied> {
    self
      .rdvz
      .handle_pending_inbound_connection(connection_id, local_addr, remote_addr)
  }

  fn handle_pending_outbound_connection(
    &mut self,
    connection_id: libp2p::swarm::ConnectionId,
    maybe_peer: Option<PeerId>,
    addresses: &[Multiaddr],
    effective_role: libp2p::core::Endpoint,
  ) -> Result<Vec<Multiaddr>, libp2p::swarm::ConnectionDenied> {
    self.rdvz.handle_pending_outbound_connection(
      connection_id,
      maybe_peer,
      addresses,
      effective_role,
    )
  }

  fn on_swarm_event(&mut self, event: FromSwarm) {
    if let FromSwarm::ConnectionEstablished(e) = event {
      if let ConnectedPoint::Dialer { address, .. } = &e.endpoint {
        for (rdvz_addr, namespace) in &self.in_progress_register {
          if *address == *rdvz_addr {
            if let Err(e) = self.rdvz.register(namespace.clone(), e.peer_id, self.ttl) {
              error!("register rendezvous failed: {e}");
            }
          }
        }
        for (rdvz_addr, namespace) in &self.in_progress_discovery {
          if *address == *rdvz_addr {
            self.rdvz.discover(
              Some(namespace.clone()),
              self.cookie.clone(),
              None,
              e.peer_id,
            )
          }
        }
        self.connected_rdvzs.push((e.peer_id, address.clone()));
      }
    }
    self.rdvz.on_swarm_event(event)
  }

  fn on_connection_handler_event(
    &mut self,
    peer_id: libp2p::PeerId,
    connection_id: libp2p::swarm::ConnectionId,
    event: libp2p::swarm::THandlerOutEvent<Self>,
  ) {
    self
      .rdvz
      .on_connection_handler_event(peer_id, connection_id, event)
  }

  fn poll(
    &mut self,
    cx: &mut std::task::Context<'_>,
  ) -> std::task::Poll<libp2p::swarm::ToSwarm<Self::ToSwarm, libp2p::swarm::THandlerInEvent<Self>>>
  {
    if let Some(backoff) = self.backoff.as_mut() {
      if backoff.is_expired() {
        backoff.start_next(true);
        self.drain_discover();
      }
    }
    while let Poll::Ready(ready) = NetworkBehaviour::poll(&mut self.rdvz, cx) {
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
