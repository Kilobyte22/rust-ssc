use core::time;
use std::{net::SocketAddr, thread, vec};

use tokio::{
    sync::{broadcast, mpsc},
    task::{self, JoinHandle},
};
use zeroconf::{
    browser::TMdnsBrowser, event_loop::TEventLoop, txt_record::TTxtRecord, MdnsBrowser,
    ServiceDiscovery, ServiceType,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    UDP,
    TCP,
}

impl Protocol {
    fn mdns_name(&self) -> &'static str {
        match self {
            Protocol::UDP => "udp",
            Protocol::TCP => "tcp",
        }
    }
}

#[non_exhaustive]
pub enum SSCVersion {
    Version1,
}

#[non_exhaustive]
pub struct DiscoveredDevice {
    pub name: String,
    pub ssc_version: SSCVersion,
    pub protocol: Protocol,
    pub socket_addr: SocketAddr,
    pub model: Option<String>,
    pub id: Option<String>,
}

#[derive(Debug)]
enum ProcessingError {
    NoTXTRecord,
    InvalidTXTVersion,
    InvalidSSCVersion,
}

pub struct Discovery {
    handle: JoinHandle<()>,
    rx: mpsc::Receiver<DiscoveredDevice>,
}

impl Discovery {
    pub async fn next(&mut self) -> DiscoveredDevice {
        self.rx.recv().await.unwrap()
    }
}

impl Drop for Discovery {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

pub async fn run() -> Discovery {
    let (out_tx, out_rx) = mpsc::channel(16);
    let (tx, mut rx) = mpsc::channel(16);
    let (exit_tx, exit_rx) = broadcast::channel(1);
    let (tx2, exit_rx2) = (tx.clone(), exit_tx.subscribe());

    thread::spawn(move || discovery_thread(Protocol::UDP, tx, exit_rx));
    thread::spawn(move || discovery_thread(Protocol::TCP, tx2, exit_rx2));

    let handle = task::spawn(async move {
        // Will be dropped when the task exits, leading to the background threads to terminate within 0.1s
        let _guard = exit_tx;
        let mut discovered_devices = vec![];

        while let Some((protocol, message)) = rx.recv().await {
            match message {
                Ok(message) => {
                    if discovered_devices.contains(message.name()) {
                        log::debug!("Device {} already seen, ignoring", message.name());
                        continue;
                    }
                    discovered_devices.push(message.name().to_owned());

                    match process_message(&message, protocol) {
                        Ok(device) => out_tx.send(device).await.unwrap(),
                        Err(e) => log::error!("Failed Discovery for {}: {e:?}", message.name()),
                    }
                }
                Err(e) => {
                    log::error!("Error during discovery: {e}");
                    log::debug!("Details: {e:?}");
                }
            }
        }
    });

    Discovery { handle, rx: out_rx }
}

fn process_message(
    message: &ServiceDiscovery,
    protocol: Protocol,
) -> Result<DiscoveredDevice, ProcessingError> {
    if let Some(txt) = message.txt() {
        let txtvers = txt.get("txtvers");
        if txtvers != Some("1".to_owned()) {
            return Err(ProcessingError::InvalidTXTVersion);
        }

        if let Some(sscvers) = txt.get("sscvers") {
            if !sscvers.starts_with("1.") {
                return Err(ProcessingError::InvalidSSCVersion);
            }

            let model = txt.get("model");
            let id = txt.get("id");

            Ok(DiscoveredDevice {
                name: message.name().to_owned(),
                ssc_version: SSCVersion::Version1,
                protocol,
                socket_addr: SocketAddr::new(message.address().parse().unwrap(), *message.port()),
                model,
                id,
            })
        } else {
            return Err(ProcessingError::InvalidSSCVersion);
        }
    } else {
        return Err(ProcessingError::NoTXTRecord);
    }
}

fn discovery_thread(
    protocol: Protocol,
    reporter: mpsc::Sender<(Protocol, zeroconf::Result<ServiceDiscovery>)>,
    mut exit_handle: broadcast::Receiver<()>,
) {
    let service_type = ServiceType::new("ssc", protocol.mdns_name()).unwrap();

    let mut browser = MdnsBrowser::new(service_type);

    browser.set_service_discovered_callback(Box::new(move |message, _| {
        reporter.blocking_send((protocol, message)).unwrap();
    }));

    let event_loop = browser.browse_services().unwrap();

    loop {
        event_loop.poll(time::Duration::from_millis(100)).unwrap();
        match exit_handle.try_recv() {
            Ok(()) => {
                unreachable!("Receiver will never receive any messages")
            }
            Err(broadcast::error::TryRecvError::Empty) => {}
            Err(broadcast::error::TryRecvError::Closed) => return,
            Err(broadcast::error::TryRecvError::Lagged(_)) => {
                unreachable!("Receiver will never receive any messages")
            }
        }
    }
}
