//! This library allows you to remotely control many Sennheiser manufactured professional audio devices.
//! This includes some devices of other brands owned by Sennheiser, like for example Neumanns DSP based studio monitors.
//!
//! ```rust,ignore
//! let mut discovery = ssc::discover().await?
//!
//! let device_info = discovery.next().await;
//!
//! let mut client = Client::connect(device_info.socket_addr, device_info.protocol).await?
//!
//! let serial: String = client.get("/device/identity/serial");
//!
//! println!("Serial Number: {serial}");
//! ```

use std::collections::HashMap;
use std::net::Ipv6Addr;
use std::sync::Arc;

use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::{Map, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, ReadHalf, WriteHalf};
use tokio::net::{TcpStream, ToSocketAddrs, UdpSocket};
use tokio::sync::{oneshot, Mutex};
use tokio::{task, time};

mod discovery;
pub mod error;

pub use discovery::{run as discover, DiscoveredDevice, Protocol};

enum WriteSocketKind {
    TCP(WriteHalf<TcpStream>),
    #[allow(dead_code)]
    UDP(UdpSocket),
}

enum ReadSocketKind {
    TCP(ReadHalf<TcpStream>),
    #[allow(dead_code)]
    UDP(UdpSocket),
}

struct State {
    reply_to: Option<oneshot::Sender<String>>,
}

/// An SSC Client connected to a device
/// ```rust,ignore
/// # // We can't run it because obviously no such device exits
/// let mut client = Client::connect(("2001:db8::42", 45), Protocol::TCP).await?
///
/// let serial: String = client.get("/device/identity/serial");
///
/// println!("Serial Number: {serial}");
/// ```
pub struct Client {
    state: Arc<Mutex<State>>,
    socket: WriteSocketKind,
}

#[derive(PartialEq, Eq, Debug)]
pub enum ListNode {
    Branch(Box<HashMap<String, ListNode>>),
    Leaf,
}

impl Client {
    /// Connects to a device using the specified protocol
    pub async fn connect<TSA: ToSocketAddrs>(addr: TSA, mode: Protocol) -> error::Result<Self> {
        let (write_socket, read_socket) = match mode {
            Protocol::UDP => {
                let socket = UdpSocket::bind((Ipv6Addr::UNSPECIFIED, 0)).await?;
                socket.connect(addr).await?;
                WriteSocketKind::UDP(socket);
                todo!()
            }
            Protocol::TCP => {
                let (rx, tx) = tokio::io::split(TcpStream::connect(addr).await?);
                (WriteSocketKind::TCP(tx), ReadSocketKind::TCP(rx))
            }
        };

        let state = Arc::new(Mutex::new(State { reply_to: None }));

        task::spawn(receiver(state.clone(), read_socket));

        Ok(Client {
            state,
            socket: write_socket,
        })
    }

    /// Gets a certain path and automatically deserializes the data at the specified path
    pub async fn get<T: DeserializeOwned>(&mut self, path: &str) -> error::Result<T> {
        let (tx, rx) = oneshot::channel();
        self.register_callback(path.to_owned(), tx).await;
        self.send_message(path, &serde_json::Value::Null).await?;

        let response = wait_response(rx).await?;

        unserialize_json_message(path, response)
    }

    /// Sets a path to the specified value
    pub async fn set<T: Serialize>(&mut self, path: &str, value: &T) -> error::Result<()> {
        let (tx, rx) = oneshot::channel();
        self.register_callback(path.to_owned(), tx).await;
        self.send_message(path, &value).await?;

        let res = wait_response(rx).await?;

        println!("SET Result: {res}");

        Ok(())
    }

    /// Returns the entire SSC tree of the device or of a subpath
    pub async fn list(&mut self, path: &str) -> error::Result<HashMap<String, ListNode>> {
        let message = build_json_message(path, &serde_json::Value::Null)?;

        let message = if path == "/" {
            message
        } else {
            serde_json::Value::Array(vec![message])
        };

        let (tx, rx) = oneshot::channel();
        self.register_callback(path.to_owned(), tx).await;

        self.send_message("/osc/schema", &message).await?;

        let res = wait_response(rx).await?;

        let outer_response: Vec<serde_json::Value> = unserialize_json_message("/osc/schema", res)?;

        let actual_schema: HashMap<String, serde_json::Value> =
            unpack_json_message(path, outer_response.into_iter().next().unwrap())?;

        let mut res = HashMap::new();

        for (k, v) in actual_schema {
            let v = match v {
                Value::Null => ListNode::Leaf,
                Value::Object(_) => {
                    let sub_path = if path == "/" {
                        format!("/{k}")
                    } else {
                        format!("{path}/{k}")
                    };
                    ListNode::Branch(Box::new(Box::pin(self.list(&sub_path)).await?))
                }
                _ => return Err(error::Error::InvalidPath),
            };
            res.insert(k, v);
        }

        Ok(res)
    }

    async fn register_callback(&self, _path: String, callback: oneshot::Sender<String>) {
        let mut guard = self.state.lock().await;
        guard.reply_to = Some(callback)
    }

    async fn send_message<T: serde::Serialize>(
        &mut self,
        path: &str,
        message: &T,
    ) -> error::Result<()> {
        let mut data = serialize_json_message(path, message)?;

        match &mut self.socket {
            WriteSocketKind::TCP(socket) => {
                data.extend_from_slice(b"\r\n");
                socket.write_all(&data).await?;
            }
            WriteSocketKind::UDP(_) => todo!(),
        }
        Ok(())
    }
}

fn serialize_json_message<T: Serialize>(path: &str, content: &T) -> error::Result<Vec<u8>> {
    let data = build_json_message(path, content)?;
    Ok(serde_json::to_vec(&data)?)
}

fn build_json_message<T: Serialize>(path: &str, content: &T) -> error::Result<serde_json::Value> {
    let components = normalize_path(path)?
        .split("/")
        .collect::<Vec<_>>()
        .into_iter()
        .rev();

    let mut data = serde_json::to_value(content)?;
    for component in components {
        if component == "" {
            data = serde_json::Value::Null;
        } else {
            let mut hm = Map::new();
            hm.insert(component.to_owned(), data);
            data = serde_json::Value::Object(hm);
        }
    }

    Ok(data)
}

fn unserialize_json_message<T: DeserializeOwned>(path: &str, data: String) -> error::Result<T> {
    let value: serde_json::Value = serde_json::from_str(&data)?;

    unpack_json_message(path, value)
}

fn unpack_json_message<T: DeserializeOwned>(
    path: &str,
    mut value: serde_json::Value,
) -> error::Result<T> {
    // TODO: Handle errors
    if path != "/" {
        for component in normalize_path(path)?.split("/") {
            if let serde_json::Value::Object(mut map) = value {
                if let Some((key, new_value)) = map.remove_entry(component) {
                    if key != component {
                        return Err(error::Error::UnexpectedPath);
                    }
                    value = new_value;
                } else {
                    return Err(error::Error::UnexpectedPath);
                }
            } else {
                return Err(error::Error::UnexpectedPath);
            }
        }
    }

    Ok(serde_json::from_value(value)?)
}

async fn wait_response(rx: oneshot::Receiver<String>) -> error::Result<String> {
    Ok(time::timeout(time::Duration::from_secs(5), rx)
        .await
        .map_err(|_| error::Error::RequestTimeout)?
        .map_err(|_| error::Error::ProcessingResponseError)?)
}

async fn receiver(state: Arc<Mutex<State>>, read: ReadSocketKind) {
    match read {
        ReadSocketKind::TCP(read) => {
            let mut lines = BufReader::new(read).lines();

            loop {
                while let Some(line) = lines.next_line().await.unwrap() {
                    let mut guard = state.lock().await;

                    if let Some(reply_to) = guard.reply_to.take() {
                        reply_to.send(line).unwrap();
                    }
                }
            }
        }
        ReadSocketKind::UDP(_) => todo!(),
    }
}

fn normalize_path(path: &str) -> error::Result<&str> {
    if path.starts_with("/") {
        Ok(&path[1..])
    } else {
        Err(error::Error::InvalidPath)
    }
}

#[cfg(test)]
mod test {
    use serde::{Deserialize, Serialize};

    use super::*;

    #[test]
    fn test_build_json_message() {
        #[derive(Serialize)]
        struct Test {
            value: u8,
        }

        let got =
            String::from_utf8(serialize_json_message("/test/42", &Test { value: 42 }).unwrap())
                .unwrap();
        let want = r#"{"test":{"42":{"value":42}}}"#;

        assert_eq!(&got, want);
    }

    #[test]
    fn test_unpack_json_message() {
        #[derive(Deserialize, PartialEq, Eq, Debug)]
        struct Test {
            value: u8,
        }

        let got: Test = unserialize_json_message(
            "/test/42",
            r#"{"test": {"42": {"value": 42 } } }"#.to_owned(),
        )
        .unwrap();
        let want = Test { value: 42 };

        assert_eq!(got, want);
    }
}
