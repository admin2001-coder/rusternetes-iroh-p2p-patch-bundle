# Rusternetes iroh P2P node networking overlay

This patch adds an optional node-to-node TCP service overlay that uses iroh P2P QUIC while leaving Kubernetes/Rusternetes API objects unchanged.

## What changes

- Adds a new crate: `crates/iroh-overlay`.
- Adds a `kube-proxy` feature: `iroh-overlay`.
- When `RUSTERNETES_IROH_OVERLAY=1` is present and the feature is compiled in, kube-proxy starts one iroh endpoint for the node.
- Each node publishes an `IrohNodeAdvertisement` under `/registry/rusternetes.io/iroh-overlay/nodes/<node-name>` using the existing Rusternetes storage backend.
- kube-proxy keeps its existing iptables service programming. For remote TCP EndpointSlice backends with a concrete `nodeName` and port, it substitutes a deterministic local loopback listener such as `127.84.x.y:<pod-port>`.
- That local listener forwards the TCP stream over iroh to the advertised remote node, where the remote node connects to the real pod endpoint.

## Compatibility model

The patch is intentionally additive:

- Existing behavior is preserved unless the code is built with `--features iroh-overlay` and the environment variable `RUSTERNETES_IROH_OVERLAY=1` is set.
- Service, Endpoints, EndpointSlice, Node, and Pod schemas are not changed.
- EndpointSlice entries without `nodeName`, non-TCP protocols, old-style Endpoints, and endpoints with unknown port `0` fall back to Rusternetes' native kube-proxy behavior.
- UDP is not tunneled by this patch. UDP service traffic remains native.

## Build

```bash
git clone https://github.com/calfonso/rusternetes.git
cd rusternetes
/path/to/apply_iroh_p2p_patch.sh .

cargo check -p rusternetes-kube-proxy --features iroh-overlay
cargo check -p rusternetes --features iroh-overlay
```

## Runtime environment

Set these on every Rusternetes node that should participate in the overlay:

```bash
export RUSTERNETES_IROH_OVERLAY=1
export RUSTERNETES_IROH_BIND_ADDR=0.0.0.0:0
```

Optional variables:

```bash
# Default: 127.84.0.0. Must remain in 127.0.0.0/8.
export RUSTERNETES_IROH_LOCAL_PREFIX=127.84.0.0

# Default: /registry/rusternetes.io/iroh-overlay/nodes/
export RUSTERNETES_IROH_STORAGE_PREFIX=/registry/rusternetes.io/iroh-overlay/nodes/

# Default: 20
export RUSTERNETES_IROH_PUBLISH_INTERVAL_SECS=20

# Optional debug output containing this node's advertisement JSON.
export RUSTERNETES_IROH_ADDR_FILE=/var/lib/rusternetes/iroh-addr.json
```

For ClusterIP/NodePort traffic DNATed from pod namespaces to 127.84.x.y loopback listeners, Linux may require:

```bash
sudo sysctl -w net.ipv4.conf.all.route_localnet=1
```

For production, persist that sysctl through your node bootstrap or system configuration.

## Design notes

The goal is a Taubyte-style resilient P2P fabric inside Rusternetes' current service-routing path, without replacing the Kubernetes-facing API. iroh supplies encrypted QUIC peer connections, direct-path discovery, and relay fallback; Rusternetes storage supplies the node advertisement catalog; kube-proxy keeps service compatibility by continuing to program iptables.

This is a service-layer TCP overlay, not a complete pod-CIDR L3 replacement. A full CNI/L3 replacement would require changes outside kube-proxy and is intentionally not included here because it would not be 1:1 compatible with the current Rusternetes kube-proxy architecture.
