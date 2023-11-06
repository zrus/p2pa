use std::time::Duration;

use crate::prelude::f;

#[derive(Debug, Clone)]
pub struct Config {
  pub ttl: Duration,
  pub query_interval: Duration,
  pub ipv6: bool,
  pub service_name: Vec<u8>,
  pub service_name_fqdn: String,
  pub meta_query_service: Vec<u8>,
  pub meta_query_service_fqdn: String,
}

impl Default for Config {
  fn default() -> Self {
    Self {
      ttl: Duration::from_secs(300),
      query_interval: Duration::from_secs(2),
      ipv6: Default::default(),
      service_name: Default::default(),
      service_name_fqdn: Default::default(),
      meta_query_service: Default::default(),
      meta_query_service_fqdn: Default::default(),
    }
  }
}

#[allow(dead_code)]
impl Config {
  pub fn ttl(self, ttl: Duration) -> Self {
    Self { ttl, ..self }
  }

  pub fn query_interval(self, query_interval: Duration) -> Self {
    Self {
      query_interval,
      ..self
    }
  }

  pub fn enable_ipv6(self) -> Self {
    Self { ipv6: true, ..self }
  }

  pub fn service_name(self, service_name: impl Into<String>) -> Self {
    let name = service_name.into();
    let name = if !name.is_empty() {
      f!("_{name}.")
    } else {
      String::new()
    };
    let service_name = f!("{name}_p2p._udp.local").into_bytes();
    let service_name_fqdn = f!("{name}_p2p._udp.local.");
    let meta_query_service = f!("{name}_services._dns-sd._udp.local").into_bytes();
    let meta_query_service_fqdn = f!("{name}_services._dns-sd._udp.local.");
    Self {
      service_name,
      service_name_fqdn,
      meta_query_service,
      meta_query_service_fqdn,
      ..self
    }
  }
}
