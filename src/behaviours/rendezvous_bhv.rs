use std::{
  collections::HashSet,
  task::Poll,
  time::{Duration, Instant},
};

use futures::StreamExt;
use libp2p::{
  core::ConnectedPoint,
  multiaddr::Protocol,
  rendezvous::{self, Cookie, Namespace, Ttl},
  swarm::{FromSwarm, NetworkBehaviour, ToSwarm},
  Multiaddr, PeerId,
};

use crate::prelude::*;

use super::mdns_bhv::{ProbeState, Timer};

#[derive(Debug)]
pub enum Event {
  Discovered(HashSet<(PeerId, Multiaddr)>),
  Registered(Namespace),
}

pub struct Behaviour {
  current_id: PeerId,
  rdvz: rendezvous::client::Behaviour,
  cookie: Option<Cookie>,
  registrations: HashSet<(Multiaddr, Namespace)>,
  discovery: HashSet<(Multiaddr, Namespace)>,
  connected_rdvzs: Vec<(PeerId, Multiaddr)>,
  event_queue: Vec<Event>,
  ttl: Option<Ttl>,
  probe_state: ProbeState,
  timeout: Timer,
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
      registrations: Default::default(),
      discovery: Default::default(),
      connected_rdvzs: Default::default(),
      event_queue: Default::default(),
      ttl,
      probe_state: ProbeState::Probing(Duration::from_secs(ttl.unwrap_or(30))),
      timeout: Timer::interval_at(Instant::now(), Duration::from_secs(ttl.unwrap_or(30))),
    }
  }

  pub fn register(&mut self, rdvz_node: Multiaddr, namespace: impl Into<String>) -> Ret {
    let namespace = namespace.into();
    self
      .registrations
      .insert((rdvz_node, Namespace::new(namespace)?));
    Ok(())
  }

  pub fn unregister(&mut self, namespace: impl Into<String>) -> Ret {
    let unregister_namespace = Namespace::new(namespace.into())?;
    for (point, namespace) in &self.registrations {
      match point.clone().pop() {
        Some(Protocol::P2p(node)) if unregister_namespace == *namespace => {
          self.rdvz.unregister(namespace.clone(), node);
        }
        _ => {}
      }
    }
    Ok(())
  }

  pub fn discover(&mut self, rdvz_node: Multiaddr, namespace: impl Into<String>) -> Ret {
    self
      .discovery
      .insert((rdvz_node, Namespace::new(namespace.into())?));
    Ok(())
  }

  fn handle_event(&mut self, event: rendezvous::client::Event) {
    use rendezvous::client::Event::*;
    match event {
      Discovered {
        registrations,
        cookie,
        ..
      } => {
        self.cookie.replace(cookie);

        let mut discovered = HashSet::new();
        for registration in registrations {
          let peer = registration.record.peer_id();
          if peer == self.current_id {
            continue;
          }

          for address in registration.record.addresses() {
            discovered.insert((peer, address.clone()));
          }
        }
        self.event_queue.push(Event::Discovered(discovered));
      }
      Registered { namespace, .. } => {
        self.event_queue.push(Event::Registered(namespace));
      }
      _ => {}
    }
  }

  fn reset_timer(&mut self) {
    let interval = *self.probe_state.interval();
    self.timeout = Timer::interval(interval);
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
    match event {
      FromSwarm::ConnectionEstablished(e) => {
        if let ConnectedPoint::Dialer { address, .. } = &e.endpoint {
          for (rdvz_addr, namespace) in &self.registrations {
            if *address == *rdvz_addr {
              if let Err(e) = self.rdvz.register(namespace.clone(), e.peer_id, self.ttl) {
                error!("register rendezvous failed: {e}");
              }
            }
          }
          for (rdvz_addr, namespace) in &self.discovery {
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
      FromSwarm::NewListener(_) => {
        for (rdvz_addr, namespace) in &self.registrations {
          if let Some(Protocol::P2p(peer)) = rdvz_addr.clone().pop() {
            if let Err(e) = self.rdvz.register(namespace.clone(), peer, self.ttl) {
              error!("register rendezvous failed: {e}");
            }
          }
        }
      }
      _ => {}
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
    let ttl = Duration::from_secs(self.ttl.unwrap_or(30));
    while self.timeout.poll_next_unpin(cx).is_ready() {
      for (rdvz_addr, namespace) in &self.registrations {
        if let Some(Protocol::P2p(peer)) = rdvz_addr.clone().pop() {
          if let Err(e) = self.rdvz.register(namespace.clone(), peer, self.ttl) {
            error!("register rendezvous failed: {e}");
          }
        }
      }

      if let ProbeState::Probing(interval) = self.probe_state {
        self.probe_state = if interval >= ttl {
          ProbeState::Finished(ttl)
        } else {
          ProbeState::Probing(interval)
        };
      }

      self.reset_timer();
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
        _ => {}
      }
    }
    if !self.event_queue.is_empty() {
      return Poll::Ready(ToSwarm::GenerateEvent(self.event_queue.remove(0)));
    }
    Poll::Pending
  }
}
