use crate::config::CaptureTarget;
use std::collections::HashSet;
use std::net::{IpAddr, SocketAddr};

#[derive(Debug, Clone)]
pub struct CaptureFilter {
    targets: Vec<CompiledTarget>,
}

#[derive(Debug, Clone)]
struct CompiledTarget {
    ip: IpAddr,
    ports: Option<HashSet<u16>>,
}

impl CaptureFilter {
    pub fn new(targets: Vec<CaptureTarget>) -> anyhow::Result<Self> {
        let mut compiled_targets = Vec::with_capacity(targets.len());

        for target in targets {
            let ip = target.ip.parse::<IpAddr>()?;
            let ports = target.ports.and_then(|ports| {
                if ports.is_empty() {
                    None
                } else {
                    Some(ports.into_iter().collect())
                }
            });

            compiled_targets.push(CompiledTarget { ip, ports });
        }

        Ok(Self {
            targets: compiled_targets,
        })
    }

    pub fn should_capture(&self, target: &SocketAddr) -> bool {
        self.targets.iter().any(|entry| {
            entry.ip == target.ip()
                && entry
                    .ports
                    .as_ref()
                    .map(|ports| ports.contains(&target.port()))
                    .unwrap_or(true)
        })
    }
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;

    use super::*;
    use crate::config::CaptureTarget;

    #[test]
    fn matches_exact_ip_and_port() {
        let filter = CaptureFilter::new(vec![CaptureTarget {
            ip: "1.2.3.4".to_string(),
            ports: Some(vec![9000, 9001]),
        }])
        .expect("filter should build");

        assert!(filter.should_capture(&"1.2.3.4:9000".parse::<SocketAddr>().unwrap()));
    }

    #[test]
    fn rejects_non_matching_ip() {
        let filter = CaptureFilter::new(vec![CaptureTarget {
            ip: "1.2.3.4".to_string(),
            ports: Some(vec![9000]),
        }])
        .expect("filter should build");

        assert!(!filter.should_capture(&"1.2.3.5:9000".parse::<SocketAddr>().unwrap()));
    }

    #[test]
    fn rejects_non_matching_port() {
        let filter = CaptureFilter::new(vec![CaptureTarget {
            ip: "1.2.3.4".to_string(),
            ports: Some(vec![9000]),
        }])
        .expect("filter should build");

        assert!(!filter.should_capture(&"1.2.3.4:9001".parse::<SocketAddr>().unwrap()));
    }

    #[test]
    fn missing_ports_match_all_ports() {
        let filter = CaptureFilter::new(vec![CaptureTarget {
            ip: "1.2.3.4".to_string(),
            ports: None,
        }])
        .expect("filter should build");

        assert!(filter.should_capture(&"1.2.3.4:1".parse::<SocketAddr>().unwrap()));
        assert!(filter.should_capture(&"1.2.3.4:65535".parse::<SocketAddr>().unwrap()));
    }

    #[test]
    fn empty_ports_match_all_ports() {
        let filter = CaptureFilter::new(vec![CaptureTarget {
            ip: "1.2.3.4".to_string(),
            ports: Some(Vec::new()),
        }])
        .expect("filter should build");

        assert!(filter.should_capture(&"1.2.3.4:9000".parse::<SocketAddr>().unwrap()));
    }
}
