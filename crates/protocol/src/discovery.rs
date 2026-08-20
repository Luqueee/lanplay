use std::collections::HashSet;
use std::net::Ipv4Addr;
use std::net::{SocketAddr, ToSocketAddrs};
use std::time::Duration;

use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};

pub const SERVICE_TYPE: &str = "_lanplay._tcp.local.";

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct HostAdvertisement {
    pub identity: String,
    pub name: String,
    pub address: Ipv4Addr,
    pub control_port: u16,
    pub protocol_version: u16,
}

pub struct Discovery {
    daemon: ServiceDaemon,
}

impl Discovery {
    pub fn start() -> Result<Self, mdns_sd::Error> {
        Ok(Self {
            daemon: ServiceDaemon::new()?,
        })
    }

    pub fn advertise(
        &self,
        identity: &str,
        name: &str,
        address: Ipv4Addr,
        control_port: u16,
        protocol_version: u16,
    ) -> Result<(), mdns_sd::Error> {
        let properties = [
            ("identity", identity),
            ("protocol", &protocol_version.to_string()),
        ];
        let service = ServiceInfo::new(
            SERVICE_TYPE,
            name,
            &format!("{name}.local."),
            address.to_string(),
            control_port,
            &properties[..],
        )?;
        self.daemon.register(service)
    }

    pub fn browse(&self, timeout: Duration) -> Result<Vec<HostAdvertisement>, mdns_sd::Error> {
        let receiver = self.daemon.browse(SERVICE_TYPE)?;
        let deadline = std::time::Instant::now() + timeout;
        let mut found = Vec::new();
        let mut names = HashSet::new();
        while let Some(remaining) = deadline.checked_duration_since(std::time::Instant::now()) {
            let Ok(event) = receiver.recv_timeout(remaining) else {
                break;
            };
            let ServiceEvent::ServiceResolved(service) = event else {
                continue;
            };
            let Some(address) = service.get_addresses_v4().into_iter().next() else {
                continue;
            };
            if !names.insert(service.get_fullname().to_owned()) {
                continue;
            }
            let identity = service
                .get_property_val_str("identity")
                .unwrap_or(service.get_fullname())
                .to_owned();
            let protocol_version = service
                .get_property_val_str("protocol")
                .and_then(|value| value.parse().ok())
                .unwrap_or(0);
            found.push(HostAdvertisement {
                identity,
                name: service.get_fullname().to_owned(),
                address,
                control_port: service.get_port(),
                protocol_version,
            });
        }
        self.daemon.stop_browse(SERVICE_TYPE)?;
        Ok(found)
    }

    pub fn shutdown(self) -> Result<(), mdns_sd::Error> {
        let receiver = self.daemon.shutdown()?;
        receiver
            .recv()
            .map(|_| ())
            .map_err(|_| mdns_sd::Error::Msg("discovery daemon stopped".to_owned()))
    }
}
pub fn manual_endpoint(spec: &str, default_port: u16) -> Result<SocketAddr, String> {
    let target = if spec.parse::<SocketAddr>().is_ok() || spec.rfind(':').is_some() {
        spec.to_owned()
    } else {
        format!("{spec}:{default_port}")
    };
    target
        .to_socket_addrs()
        .map_err(|error| format!("{spec} is not an address: {error}"))?
        .next()
        .ok_or_else(|| format!("{spec} resolved to no addresses"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_type_is_lan_scoped_and_identity_is_explicit() {
        assert_eq!(SERVICE_TYPE, "_lanplay._tcp.local.");
        let advertisement = HostAdvertisement {
            identity: "host-01".to_owned(),
            name: "LanPlay Host".to_owned(),
            address: Ipv4Addr::LOCALHOST,
            control_port: 5005,
            protocol_version: 1,
        };
        assert_ne!(advertisement.identity, advertisement.name);
    }
    #[test]
    fn manual_host_without_port_uses_the_control_port() {
        assert_eq!(
            manual_endpoint("127.0.0.1", 5005).expect("loopback resolves"),
            "127.0.0.1:5005".parse().unwrap()
        );
    }
}
