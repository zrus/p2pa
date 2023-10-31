use std::str::FromStr;

macro_rules! build_concrete_known_peers {
  ($name:ident, $peers:expr) => {
    pub fn $name() -> Vec<(libp2p::PeerId, libp2p::Multiaddr)> {
      build_concretes($peers)
    }
  };
}

fn build_concretes(peers: &[(&str, &str)]) -> Vec<(libp2p::PeerId, libp2p::Multiaddr)> {
  let mut concretes = vec![];
  for (peer_id, addr) in peers {
    concretes.push((
      libp2p::PeerId::from_str(peer_id).unwrap(),
      libp2p::Multiaddr::from_str(addr).unwrap(),
    ));
  }
  concretes
}

/// PUBLIC IPFS BOOTSTRAP NODES
const BOOTNODES: [(&str, &str); 4] = [
  (
    "QmNnooDu7bfjPFoTZYxMNLWUQJyrVwtbZg5gBMjTezGAJN",
    "/dnsaddr/bootstrap.libp2p.io",
  ),
  (
    "QmQCU2EcMqAqQPR2i9bChDtGNJchTbq5TbXJJ16u19uLTa",
    "/dnsaddr/bootstrap.libp2p.io",
  ),
  (
    "QmbLHAnMoJPWSCR5Zhtx6BHJX9KiKNN6tpvbUcqanj75Nb",
    "/dnsaddr/bootstrap.libp2p.io",
  ),
  (
    "QmcZf59bWwK5XFi76CZX8cbJ4BhTzzA3gU1ZjYZcYW3dwt",
    "/dnsaddr/bootstrap.libp2p.io",
  ),
];
build_concrete_known_peers!(bootnodes, &BOOTNODES);

/// PRIVATE RELAY SERVERS
#[doc(hidden)]
const RELAY_SERVERS: [(&str, &str); 2] = [
  (
    "12D3KooWQANRJdBvMCE2HRQAPTpunWZ3Vg3k2X2DjJsuCtg4K4SZ",
    "/ip4/162.205.50.166/tcp/4111",
  ),
  (
    "12D3KooWRW5KgEc71mvCcd2hFtuH3HhthJRSpeM7h4LURDif9cMF",
    "/ip4/113.161.95.53/tcp/4111",
  ),
];
build_concrete_known_peers!(relay_servers, &RELAY_SERVERS);

/// PRIVATE RENDEZVOUS POINTS
#[doc(hidden)]
const RDVZ_POINTS: [(&str, &str); 2] = [
  (
    "12D3KooWGjSE7eCu5f1Pa8pGxscmoEcdQ5BRMGqbyMvL2dhrPCS5",
    "/ip4/162.205.50.166/tcp/62649",
  ),
  (
    "12D3KooWENTLiCwwoVwYNbKFEfmNdyHG1jxNn9oPB29JeeZXWjAb",
    "/ip4/113.161.95.53/tcp/62649",
  ),
];
build_concrete_known_peers!(rendezvous_points, &RDVZ_POINTS);
