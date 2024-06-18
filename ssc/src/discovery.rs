pub enum DiscoveryMode {
    UDP,
    TCP,
}

impl Default for DiscoveryMode {
    fn default() -> Self {
        DiscoveryMode::UDP
    }
}

pub async fn discover_devices(discovery_mode: DiscoveryMode) -> crate::error::Result<()> {
    let svc_name = match discovery_mode {
        DiscoveryMode::UDP => "_ssc._udp.local",
        DiscoveryMode::TCP => "_ssc._tcp.local",
    };

    todo!()
}
