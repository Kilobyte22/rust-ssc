use std::collections::HashMap;
use std::net::Ipv6Addr;
use std::sync::Arc;

use futures_util::StreamExt;
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::{Map, Value};
use tokio::{task, time};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, ReadHalf, WriteHalf};
use tokio::net::{TcpStream, ToSocketAddrs, UdpSocket};
use tokio::sync::{Mutex, oneshot};

pub use discovery::{discover_devices, DiscoveryMode};

mod discovery;
pub mod error;

enum StateEntry {
    WaitForReply(oneshot::Sender<String>),
    Subscription,
}

enum WriteSocketKind {
    TCP(WriteHalf<TcpStream>),
    UDP(UdpSocket),
}

enum ReadSocketKind {
    TCP(ReadHalf<TcpStream>),
    UDP(UdpSocket),
}

struct State {
    reply_to: Option<oneshot::Sender<String>>,
}

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
    pub async fn connect<TSA: ToSocketAddrs>(
        addr: TSA,
        mode: DiscoveryMode,
    ) -> error::Result<Self> {
        let (write_socket, read_socket) = match mode {
            DiscoveryMode::UDP => {
                let socket = UdpSocket::bind((Ipv6Addr::UNSPECIFIED, 0)).await?;
                socket.connect(addr).await?;
                WriteSocketKind::UDP(socket);
                todo!()
            }
            DiscoveryMode::TCP => {
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

    pub async fn get<T: DeserializeOwned>(&mut self, path: &str) -> error::Result<T> {
        let (tx, rx) = oneshot::channel();
        self.register_callback(path.to_owned(), tx).await;
        self.send_message(path, &serde_json::Value::Null).await?;

        let response = wait_response(rx).await?;

        unserialize_json_message(path, response)
    }

    pub async fn set<T: Serialize>(&mut self, path: &str, value: &T) -> error::Result<()> {
        let (tx, rx) = oneshot::channel();
        self.register_callback(path.to_owned(), tx).await;
        self.send_message(path, &value).await?;

        let res = wait_response(rx).await?;

        println!("SET Result: {res}");

        Ok(())
    }

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

        let mut outer_response: Vec<serde_json::Value> =
            unserialize_json_message("/osc/schema", res)?;

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
                v => return Err(error::Error::InvalidPath),
            };
            res.insert(k, v);
        }

        Ok(res)
    }

    async fn register_callback(&self, path: String, callback: oneshot::Sender<String>) {
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

        let got = String::from_utf8(build_json_message("/test/42", &Test { value: 42 }).unwrap())
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

        let got: Test = unpack_json_message(
            "/test/42",
            r#"{"test": {"42": {"value": 42 } } }"#.to_owned(),
        )
        .unwrap();
        let want = Test { value: 42 };

        assert_eq!(got, want);
    }
}
