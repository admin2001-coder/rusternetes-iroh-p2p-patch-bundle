#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="${1:-$(pwd)}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

cd "$REPO_ROOT"

need_file() {
  if [[ ! -f "$1" ]]; then
    echo "error: expected file not found: $1" >&2
    exit 1
  fi
}

need_file Cargo.toml
need_file crates/kube-proxy/Cargo.toml
need_file crates/kube-proxy/src/lib.rs
need_file crates/kube-proxy/src/proxy.rs
need_file crates/rusternetes/Cargo.toml

rm -rf crates/iroh-overlay
mkdir -p crates
cp -a "$SCRIPT_DIR/crates/iroh-overlay" crates/iroh-overlay
mkdir -p docs
cp "$SCRIPT_DIR/docs/IROH_P2P_NETWORKING.md" docs/IROH_P2P_NETWORKING.md

backup_once() {
  local file="$1"
  if [[ ! -f "$file.before-iroh-overlay" ]]; then
    cp "$file" "$file.before-iroh-overlay"
  fi
}

backup_once Cargo.toml
backup_once crates/kube-proxy/Cargo.toml
backup_once crates/kube-proxy/src/lib.rs
backup_once crates/kube-proxy/src/proxy.rs
backup_once crates/rusternetes/Cargo.toml

python3 - <<'PY'
from pathlib import Path
import re

ROOT = Path('.')


def read(path: str) -> str:
    return (ROOT / path).read_text()


def write(path: str, text: str) -> None:
    (ROOT / path).write_text(text)


def ensure_once(text: str, needle: str, insertion: str, *, before: bool = False) -> str:
    if insertion.strip() in text:
        return text
    if needle not in text:
        raise SystemExit(f"patch failed: marker not found: {needle!r}")
    return text.replace(needle, insertion + needle if before else needle + insertion, 1)

# Workspace member
text = read('Cargo.toml')
if '"crates/iroh-overlay"' not in text:
    text = ensure_once(text, '    "crates/kube-proxy",\n', '    "crates/iroh-overlay",\n')
write('Cargo.toml', text)

# kube-proxy Cargo.toml feature + optional dependency
text = read('crates/kube-proxy/Cargo.toml')
if 'iroh-overlay' not in text.split('[dependencies]')[0]:
    text = ensure_once(text, 'sqlite = ["rusternetes-storage/sqlite"]\n', 'iroh-overlay = ["dep:rusternetes-iroh-overlay"]\n')
if 'rusternetes-iroh-overlay' not in text.split('[dependencies]', 1)[1]:
    text = ensure_once(
        text,
        'rusternetes-storage = { path = "../storage" }\n',
        'rusternetes-iroh-overlay = { path = "../iroh-overlay", optional = true }\n',
    )
write('crates/kube-proxy/Cargo.toml', text)

# all-in-one Cargo feature forwarding to kube-proxy
text = read('crates/rusternetes/Cargo.toml')
if 'iroh-overlay = ["rusternetes-kube-proxy/iroh-overlay"]' not in text:
    text = text.replace(']\n\n[dependencies]\n', ']\niroh-overlay = ["rusternetes-kube-proxy/iroh-overlay"]\n\n[dependencies]\n', 1)
write('crates/rusternetes/Cargo.toml', text)

# kube-proxy lib.rs: start overlay runtime when feature+env are enabled.
text = read('crates/kube-proxy/src/lib.rs')
old = 'let kube_proxy = Arc::new(tokio::sync::Mutex::new(KubeProxy::new(Arc::clone(&storage))?));'
new = '''#[cfg(feature = "iroh-overlay")]
    let overlay_runtime = if let Some(overlay_config) =
        rusternetes_iroh_overlay::IrohOverlayConfig::from_env(config.node_name.clone())?
    {
        Some(
            rusternetes_iroh_overlay::IrohOverlayRuntime::start(
                Arc::clone(&storage),
                overlay_config,
            )
            .await?,
        )
    } else {
        None
    };

    #[cfg(feature = "iroh-overlay")]
    let kube_proxy = Arc::new(tokio::sync::Mutex::new(KubeProxy::new_with_overlay(
        Arc::clone(&storage),
        config.node_name.clone(),
        overlay_runtime,
    )?));

    #[cfg(not(feature = "iroh-overlay"))]
    let kube_proxy = Arc::new(tokio::sync::Mutex::new(KubeProxy::new(Arc::clone(&storage))?));'''
if 'IrohOverlayRuntime::start' not in text:
    if old not in text:
        raise SystemExit('patch failed: kube-proxy lib.rs constructor marker not found')
    text = text.replace(old, new, 1)
write('crates/kube-proxy/src/lib.rs', text)

# kube-proxy proxy.rs: add overlay-aware endpoint rewriting.
text = read('crates/kube-proxy/src/proxy.rs')
if 'rusternetes_iroh_overlay::IrohOverlayRuntime' not in text:
    text = text.replace('use tracing::{debug, error, info};', 'use tracing::{debug, error, info, warn};')
    text = ensure_once(
        text,
        'use crate::iptables::IptablesManager;\n',
        '\n#[cfg(feature = "iroh-overlay")]\nuse rusternetes_iroh_overlay::IrohOverlayRuntime;\n',
    )

if '#[cfg(feature = "iroh-overlay")]\n    node_name: String,' not in text:
    text = text.replace(
        '    last_sync_hash: u64,\n}',
        '    last_sync_hash: u64,\n    #[cfg(feature = "iroh-overlay")]\n    node_name: String,\n    #[cfg(feature = "iroh-overlay")]\n    overlay: Option<Arc<IrohOverlayRuntime>>,\n}',
        1,
    )

if 'pub fn new_with_overlay(' not in text:
    old_ctor = '''pub fn new(storage: Arc<StorageBackend>) -> Result<Self> {
        let iptables = IptablesManager::new();
        iptables.initialize()?;

        Ok(Self {
            storage,
            iptables,
            last_sync_hash: 0,
        })
    }'''
    new_ctor = '''pub fn new(storage: Arc<StorageBackend>) -> Result<Self> {
        #[cfg(feature = "iroh-overlay")]
        {
            return Self::new_with_overlay(storage, String::new(), None);
        }

        #[cfg(not(feature = "iroh-overlay"))]
        {
            let iptables = IptablesManager::new();
            iptables.initialize()?;

            Ok(Self {
                storage,
                iptables,
                last_sync_hash: 0,
            })
        }
    }

    #[cfg(feature = "iroh-overlay")]
    pub fn new_with_overlay(
        storage: Arc<StorageBackend>,
        node_name: String,
        overlay: Option<Arc<IrohOverlayRuntime>>,
    ) -> Result<Self> {
        let iptables = IptablesManager::new();
        iptables.initialize()?;

        Ok(Self {
            storage,
            iptables,
            last_sync_hash: 0,
            node_name,
            overlay,
        })
    }'''
    if old_ctor not in text:
        raise SystemExit('patch failed: kube-proxy proxy.rs constructor marker not found')
    text = text.replace(old_ctor, new_ctor, 1)

# Change ready endpoint collection to keep EndpointSlice nodeName.
text = text.replace('let mut ready_addrs: Vec<String> = Vec::new();', 'let mut ready_addrs: Vec<(String, Option<String>)> = Vec::new();', 1)
text = text.replace('ready_addrs.push(addr.clone());', 'ready_addrs.push((addr.clone(), endpoint.node_name.clone()));', 1)

# No-port path: tuple now contains (address, nodeName), but no concrete endpoint port to tunnel.
text = text.replace(
'''for addr in &ready_addrs {
                    endpointslice_map
                        .entry(key.clone())
                        .or_default()
                        .push((addr.clone(), None, 0));
                }''',
'''for (addr, _) in &ready_addrs {
                    endpointslice_map
                        .entry(key.clone())
                        .or_default()
                        .push((addr.clone(), None, 0));
                }''',
1,
)

# Port path: for TCP EndpointSlice backends on a different node, replace the backend IP with a local iroh tunnel IP.
old_loop = '''for es_port in &es.ports {
                    let port_num = es_port.port.unwrap_or(0) as u16;
                    let port_name = es_port.name.clone();
                    for addr in &ready_addrs {
                        endpointslice_map.entry(key.clone()).or_default().push((
                            addr.clone(),
                            port_name.clone(),
                            port_num,
                        ));
                    }
                }'''
new_loop = '''for es_port in &es.ports {
                    let port_num = es_port.port.unwrap_or(0) as u16;
                    let port_name = es_port.name.clone();
                    let protocol = es_port.protocol.as_deref().unwrap_or("TCP");
                    for (addr, endpoint_node_name) in &ready_addrs {
                        let (backend_ip, backend_port) = self
                            .resolve_overlay_backend(
                                addr,
                                port_num,
                                endpoint_node_name.as_deref(),
                                protocol,
                            )
                            .await;
                        endpointslice_map.entry(key.clone()).or_default().push((
                            backend_ip,
                            port_name.clone(),
                            backend_port,
                        ));
                    }
                }'''
if old_loop in text:
    text = text.replace(old_loop, new_loop, 1)
elif 'resolve_overlay_backend(' not in text:
    raise SystemExit('patch failed: EndpointSlice port loop marker not found')

# Add helper methods.
if 'async fn resolve_overlay_backend(' not in text:
    helper = '''
    #[cfg(feature = "iroh-overlay")]
    async fn resolve_overlay_backend(
        &self,
        addr: &str,
        port: u16,
        endpoint_node_name: Option<&str>,
        protocol: &str,
    ) -> (String, u16) {
        if port == 0 || !protocol.eq_ignore_ascii_case("TCP") {
            return (addr.to_string(), port);
        }

        let Some(remote_node) = endpoint_node_name else {
            return (addr.to_string(), port);
        };
        if remote_node == self.node_name {
            return (addr.to_string(), port);
        }
        let Some(overlay) = &self.overlay else {
            return (addr.to_string(), port);
        };

        match overlay.ensure_tcp_tunnel(remote_node, addr, port).await {
            Ok(local) => (local.ip().to_string(), local.port()),
            Err(err) => {
                warn!(
                    remote_node = %remote_node,
                    backend = %format!("{addr}:{port}"),
                    error = %err,
                    "falling back to native endpoint because iroh tunnel creation failed"
                );
                (addr.to_string(), port)
            }
        }
    }

    #[cfg(not(feature = "iroh-overlay"))]
    async fn resolve_overlay_backend(
        &self,
        addr: &str,
        port: u16,
        _endpoint_node_name: Option<&str>,
        _protocol: &str,
    ) -> (String, u16) {
        (addr.to_string(), port)
    }
'''
    marker = '    /// Sync a single service\n'
    if marker not in text:
        raise SystemExit('patch failed: sync_service marker not found')
    text = text.replace(marker, helper + '\n' + marker, 1)

write('crates/kube-proxy/src/proxy.rs', text)
PY

cat <<'DONE'
Applied the Rusternetes iroh P2P overlay patch.

Build examples:
  cargo check -p rusternetes-kube-proxy --features iroh-overlay
  cargo check -p rusternetes --features iroh-overlay

Enable on each node with:
  export RUSTERNETES_IROH_OVERLAY=1
  export RUSTERNETES_IROH_BIND_ADDR=0.0.0.0:0
  # Optional but often needed for ClusterIP/NodePort DNAT to 127.84.x.y from pod namespaces:
  sudo sysctl -w net.ipv4.conf.all.route_localnet=1
DONE
