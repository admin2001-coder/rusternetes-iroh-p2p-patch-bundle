//! Rusternetes node-to-node TCP overlay backed by iroh P2P QUIC.
//!
//! The overlay intentionally keeps Kubernetes/Rusternetes API objects unchanged.
//! kube-proxy still owns service programming, but remote EndpointSlice TCP
//! backends can be rewritten to deterministic 127.84.x.y listeners.  Those
//! local listeners forward traffic over an iroh connection to the advertised
//! owning node, where it is TCP-connected to the real pod endpoint.

use anyhow::{anyhow, Context, Result};
use iroh::{endpoint::presets, Endpoint, EndpointAddr};
use rusternetes_storage::{Storage, StorageBackend};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::env;
use std::hash::{Hash, Hasher};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, RwLock};
use tracing::{debug, error, info, warn};

/// ALPN used by the Rusternetes iroh service-overlay protocol.
pub const RUSTERNETES_IROH_ALPN: &[u8] = b"rusternetes/iroh-overlay/1";

/// Default storage location for iroh node advertisements.
pub const DEFAULT_STORAGE_PREFIX: &str = "/registry/rusternetes.io/iroh-overlay/nodes/";

/// A node's advertised iroh dialing information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IrohNodeAdvertisement {
    pub node_name: String,
    pub endpoint_addr: EndpointAddr,
    pub updated_at_unix: i64,
}

/// Runtime configuration for the iroh overlay.
#[derive(Debug, Clone)]
pub struct IrohOverlayConfig {
    /// Kubernetes/Rusternetes node name for this kube-proxy instance.
    pub node_name: String,
    /// Optional UDP bind address for iroh. Defaults to iroh's preset bind.
    pub bind_addr: Option<SocketAddr>,
    /// Prefix used for deterministic loopback tunnel IPs. The first two octets
    /// are used; default is 127.84.0.0, producing listeners like 127.84.x.y:port.
    pub local_loopback_prefix: Ipv4Addr,
    /// Storage prefix for node advertisements.
    pub storage_prefix: String,
    /// How often to refresh this node's storage advertisement.
    pub publish_interval: Duration,
    /// Optional local file to persist the raw endpoint address as JSON for
    /// operators/debugging. The iroh secret key remains managed by iroh here.
    pub local_addr_file: Option<PathBuf>,
}

impl IrohOverlayConfig {
    pub fn new(node_name: impl Into<String>) -> Self {
        Self {
            node_name: node_name.into(),
            bind_addr: None,
            local_loopback_prefix: Ipv4Addr::new(127, 84, 0, 0),
            storage_prefix: DEFAULT_STORAGE_PREFIX.to_string(),
            publish_interval: Duration::from_secs(20),
            local_addr_file: None,
        }
    }

    /// Build config from environment.  Returns `Ok(None)` unless
    /// `RUSTERNETES_IROH_OVERLAY` is one of: 1, true, yes, on.
    pub fn from_env(node_name: impl Into<String>) -> Result<Option<Self>> {
        let enabled = env::var("RUSTERNETES_IROH_OVERLAY")
            .ok()
            .map(|value| matches!(value.to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"))
            .unwrap_or(false);

        if !enabled {
            return Ok(None);
        }

        let mut config = Self::new(node_name);

        if let Ok(value) = env::var("RUSTERNETES_IROH_BIND_ADDR") {
            config.bind_addr = Some(
                value
                    .parse::<SocketAddr>()
                    .with_context(|| format!("invalid RUSTERNETES_IROH_BIND_ADDR={value}"))?,
            );
        }

        if let Ok(value) = env::var("RUSTERNETES_IROH_LOCAL_PREFIX") {
            let ip = value
                .parse::<Ipv4Addr>()
                .with_context(|| format!("invalid RUSTERNETES_IROH_LOCAL_PREFIX={value}"))?;
            if ip.octets()[0] != 127 {
                return Err(anyhow!(
                    "RUSTERNETES_IROH_LOCAL_PREFIX must be in 127.0.0.0/8, got {ip}"
                ));
            }
            config.local_loopback_prefix = ip;
        }

        if let Ok(value) = env::var("RUSTERNETES_IROH_STORAGE_PREFIX") {
            config.storage_prefix = ensure_trailing_slash(value);
        }

        if let Ok(value) = env::var("RUSTERNETES_IROH_PUBLISH_INTERVAL_SECS") {
            let secs = value
                .parse::<u64>()
                .with_context(|| format!("invalid RUSTERNETES_IROH_PUBLISH_INTERVAL_SECS={value}"))?;
            config.publish_interval = Duration::from_secs(secs.max(1));
        }

        if let Ok(value) = env::var("RUSTERNETES_IROH_ADDR_FILE") {
            config.local_addr_file = Some(PathBuf::from(value));
        }

        Ok(Some(config))
    }
}

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
struct TunnelKey {
    remote_node: String,
    dst_ip: String,
    dst_port: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TcpConnectRequest {
    dst_ip: String,
    dst_port: u16,
}

/// Shared overlay runtime.
pub struct IrohOverlayRuntime {
    endpoint: Endpoint,
    storage: Arc<StorageBackend>,
    config: IrohOverlayConfig,
    peers: Arc<RwLock<HashMap<String, EndpointAddr>>>,
    listeners: Arc<Mutex<HashMap<TunnelKey, SocketAddr>>>,
}

impl IrohOverlayRuntime {
    /// Start the local iroh endpoint, storage advertisement loop, peer refresh
    /// loop, and inbound iroh protocol accept loop.
    pub async fn start(storage: Arc<StorageBackend>, config: IrohOverlayConfig) -> Result<Arc<Self>> {
        let mut builder = Endpoint::builder(presets::N0).alpns(vec![RUSTERNETES_IROH_ALPN.to_vec()]);
        if let Some(bind_addr) = config.bind_addr {
            builder = builder
                .bind_addr(bind_addr)
                .map_err(|err| anyhow!("invalid iroh bind address {bind_addr}: {err:?}"))?;
        }

        let endpoint = builder.bind().await.context("failed to bind iroh endpoint")?;
        let runtime = Arc::new(Self {
            endpoint,
            storage,
            config,
            peers: Arc::new(RwLock::new(HashMap::new())),
            listeners: Arc::new(Mutex::new(HashMap::new())),
        });

        runtime.spawn_accept_loop();
        runtime.spawn_publish_loop();
        runtime.spawn_peer_refresh_loop();

        info!(
            node = %runtime.config.node_name,
            addr = ?runtime.endpoint.addr(),
            "Rusternetes iroh overlay started"
        );

        Ok(runtime)
    }

    /// Return a local loopback listener for a remote TCP backend.  The returned
    /// port is intentionally the real backend port so EndpointSlice port matching
    /// in kube-proxy remains unchanged.
    pub async fn ensure_tcp_tunnel(
        self: &Arc<Self>,
        remote_node: &str,
        dst_ip: &str,
        dst_port: u16,
    ) -> Result<SocketAddr> {
        if dst_port == 0 {
            return Err(anyhow!("cannot tunnel endpoint with unknown port 0"));
        }

        let key = TunnelKey {
            remote_node: remote_node.to_string(),
            dst_ip: dst_ip.to_string(),
            dst_port,
        };

        if let Some(existing) = self.listeners.lock().await.get(&key).copied() {
            return Ok(existing);
        }

        // Bind outside the map first.  Try several deterministic loopback IPs in
        // case a previous listener or process owns the computed address.
        for attempt in 0u8..64 {
            let local_ip = self.deterministic_loopback_ip(&key, attempt);
            let local_addr = SocketAddr::new(IpAddr::V4(local_ip), dst_port);
            match TcpListener::bind(local_addr).await {
                Ok(listener) => {
                    let mut listeners = self.listeners.lock().await;
                    if let Some(existing) = listeners.get(&key).copied() {
                        drop(listener);
                        return Ok(existing);
                    }
                    listeners.insert(key.clone(), local_addr);
                    drop(listeners);

                    self.spawn_listener(key.clone(), local_addr, listener);
                    info!(
                        remote_node = %remote_node,
                        dst = %format!("{dst_ip}:{dst_port}"),
                        local = %local_addr,
                        "created iroh TCP tunnel listener"
                    );
                    return Ok(local_addr);
                }
                Err(err) => {
                    debug!(local = %local_addr, error = %err, "local tunnel bind failed; trying another loopback IP");
                }
            }
        }

        Err(anyhow!(
            "unable to bind deterministic loopback listener for {remote_node} {dst_ip}:{dst_port}"
        ))
    }

    pub fn local_endpoint_addr(&self) -> EndpointAddr {
        self.endpoint.addr()
    }

    fn spawn_accept_loop(self: &Arc<Self>) {
        let runtime = Arc::clone(self);
        tokio::spawn(async move {
            loop {
                let Some(connecting) = runtime.endpoint.accept().await else {
                    warn!("iroh endpoint accept loop ended");
                    break;
                };
                let runtime = Arc::clone(&runtime);
                tokio::spawn(async move {
                    match connecting.await {
                        Ok(connection) => {
                            if let Err(err) = runtime.handle_connection(connection).await {
                                error!(error = %err, "iroh inbound connection handling failed");
                            }
                        }
                        Err(err) => error!(error = %err, "iroh inbound connection failed"),
                    }
                });
            }
        });
    }

    fn spawn_publish_loop(self: &Arc<Self>) {
        let runtime = Arc::clone(self);
        tokio::spawn(async move {
            loop {
                if let Err(err) = runtime.publish_advertisement().await {
                    warn!(error = %err, "failed to publish iroh node advertisement");
                }
                tokio::time::sleep(runtime.config.publish_interval).await;
            }
        });
    }

    fn spawn_peer_refresh_loop(self: &Arc<Self>) {
        let runtime = Arc::clone(self);
        tokio::spawn(async move {
            loop {
                if let Err(err) = runtime.refresh_peers().await {
                    warn!(error = %err, "failed to refresh iroh peer advertisements");
                }
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        });
    }

    fn spawn_listener(self: &Arc<Self>, key: TunnelKey, local_addr: SocketAddr, listener: TcpListener) {
        let runtime = Arc::clone(self);
        tokio::spawn(async move {
            loop {
                match listener.accept().await {
                    Ok((tcp, client)) => {
                        let runtime = Arc::clone(&runtime);
                        let key = key.clone();
                        tokio::spawn(async move {
                            if let Err(err) = runtime.forward_tcp_over_iroh(tcp, key).await {
                                warn!(client = %client, error = %err, "iroh overlay forwarding failed");
                            }
                        });
                    }
                    Err(err) => {
                        warn!(local = %local_addr, error = %err, "iroh overlay listener accept failed");
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                }
            }
        });
    }

    async fn forward_tcp_over_iroh(&self, mut tcp: TcpStream, key: TunnelKey) -> Result<()> {
        let remote = self
            .get_peer_addr(&key.remote_node)
            .await
            .with_context(|| format!("no iroh peer advertisement for {}", key.remote_node))?;

        let connection = self
            .endpoint
            .connect(remote, RUSTERNETES_IROH_ALPN)
            .await
            .with_context(|| format!("failed to connect to remote node {}", key.remote_node))?;

        let (mut send, mut recv) = connection.open_bi().await.context("failed to open iroh stream")?;

        let request = TcpConnectRequest {
            dst_ip: key.dst_ip.clone(),
            dst_port: key.dst_port,
        };
        let request_bytes = serde_json::to_vec(&request).context("failed to serialize connect request")?;
        send.write_all(&(request_bytes.len() as u32).to_be_bytes())
            .await
            .context("failed to send request length")?;
        send.write_all(&request_bytes)
            .await
            .context("failed to send request bytes")?;
        send.flush().await.ok();

        let (mut tcp_r, mut tcp_w) = tcp.split();

        let client_to_remote = async {
            let mut buf = [0u8; 16 * 1024];
            loop {
                let n = tcp_r.read(&mut buf).await.context("tcp read failed")?;
                if n == 0 {
                    send.finish().await.ok();
                    break;
                }
                send.write_all(&buf[..n]).await.context("iroh send failed")?;
            }
            Result::<()>::Ok(())
        };

        let remote_to_client = async {
            let mut buf = [0u8; 16 * 1024];
            loop {
                let n = recv.read(&mut buf).await.context("iroh recv failed")?;
                if n == 0 {
                    tcp_w.shutdown().await.ok();
                    break;
                }
                tcp_w.write_all(&buf[..n]).await.context("tcp write failed")?;
            }
            Result::<()>::Ok(())
        };

        tokio::try_join!(client_to_remote, remote_to_client)?;
        Ok(())
    }

    async fn handle_connection(&self, connection: iroh::Connection) -> Result<()> {
        loop {
            let Ok((mut send, mut recv)) = connection.accept_bi().await else {
                break;
            };

            let mut len_buf = [0u8; 4];
            recv.read_exact(&mut len_buf)
                .await
                .context("failed to read request length")?;
            let len = u32::from_be_bytes(len_buf) as usize;
            if len == 0 || len > 8 * 1024 {
                return Err(anyhow!("invalid request length {len}"));
            }

            let mut req_buf = vec![0u8; len];
            recv.read_exact(&mut req_buf)
                .await
                .context("failed to read request")?;
            let request: TcpConnectRequest = serde_json::from_slice(&req_buf).context("invalid request JSON")?;

            let dst = format!("{}:{}", request.dst_ip, request.dst_port);
            let mut tcp = TcpStream::connect(&dst)
                .await
                .with_context(|| format!("failed to connect to backend {dst}"))?;

            let (mut tcp_r, mut tcp_w) = tcp.split();

            let remote_to_backend = async {
                let mut buf = [0u8; 16 * 1024];
                loop {
                    let n = recv.read(&mut buf).await.context("iroh recv failed")?;
                    if n == 0 {
                        tcp_w.shutdown().await.ok();
                        break;
                    }
                    tcp_w.write_all(&buf[..n]).await.context("tcp write failed")?;
                }
                Result::<()>::Ok(())
            };

            let backend_to_remote = async {
                let mut buf = [0u8; 16 * 1024];
                loop {
                    let n = tcp_r.read(&mut buf).await.context("tcp read failed")?;
                    if n == 0 {
                        send.finish().await.ok();
                        break;
                    }
                    send.write_all(&buf[..n]).await.context("iroh send failed")?;
                }
                Result::<()>::Ok(())
            };

            tokio::try_join!(remote_to_backend, backend_to_remote)?;
        }

        Ok(())
    }

    async fn get_peer_addr(&self, node: &str) -> Option<EndpointAddr> {
        self.peers.read().await.get(node).cloned()
    }

    fn advertisement_key(&self, node: &str) -> String {
        format!("{}{}", self.config.storage_prefix, node)
    }

    async fn publish_advertisement(&self) -> Result<()> {
        let ad = IrohNodeAdvertisement {
            node_name: self.config.node_name.clone(),
            endpoint_addr: self.endpoint.addr(),
            updated_at_unix: unix_now(),
        };

        let key = self.advertisement_key(&self.config.node_name);
        self.put_json(&key, &ad).await.context("storage put failed")?;

        if let Some(path) = &self.config.local_addr_file {
            if let Some(parent) = path.parent() {
                tokio::fs::create_dir_all(parent).await.ok();
            }
            let text = serde_json::to_string_pretty(&ad).context("serialize local addr")?;
            tokio::fs::write(path, text).await.ok();
        }

        Ok(())
    }

    async fn refresh_peers(&self) -> Result<()> {
        let prefix = self.config.storage_prefix.clone();
        let keys = self.storage.list_prefix(&prefix).await?;

        let mut peers = HashMap::new();
        for key in keys {
            if let Some(node) = key.strip_prefix(&prefix) {
                if node == self.config.node_name {
                    continue;
                }
                match self.get_json::<IrohNodeAdvertisement>(&key).await {
                    Ok(Some(ad)) => {
                        peers.insert(ad.node_name, ad.endpoint_addr);
                    }
                    Ok(None) => {}
                    Err(err) => warn!(key = %key, error = %err, "failed to decode peer advertisement"),
                }
            }
        }

        *self.peers.write().await = peers;
        Ok(())
    }

    async fn put_json<T: Serialize>(&self, key: &str, value: &T) -> Result<()> {
        let bytes = serde_json::to_vec(value)?;
        self.storage.put(key.as_bytes(), &bytes).await?;
        Ok(())
    }

    async fn get_json<T: DeserializeOwned>(&self, key: &str) -> Result<Option<T>> {
        let Some(bytes) = self.storage.get(key.as_bytes()).await? else {
            return Ok(None);
        };
        Ok(Some(serde_json::from_slice(&bytes)?))
    }

    fn deterministic_loopback_ip(&self, key: &TunnelKey, attempt: u8) -> Ipv4Addr {
        let mut hasher = DefaultHasher::new();
        key.hash(&mut hasher);
        attempt.hash(&mut hasher);
        let hash = hasher.finish();

        let prefix = self.config.local_loopback_prefix.octets();
        // Use bytes 0..2 of the hash for the last two octets; avoid 0 and 255.
        let o3 = ((hash & 0xff) as u8).clamp(1, 254);
        let o4 = (((hash >> 8) & 0xff) as u8).clamp(1, 254);
        Ipv4Addr::new(prefix[0], prefix[1], o3, o4)
    }
}

fn ensure_trailing_slash(mut value: String) -> String {
    if !value.ends_with('/') {
        value.push('/');
    }
    value
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::from_secs(0))
        .as_secs() as i64
}
