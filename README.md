# Rusternetes iroh P2P overlay patch bundle

This bundle contains an in-place patch for `github.com/calfonso/rusternetes` that adds an optional iroh P2P node-to-node TCP service overlay while keeping Rusternetes/Kubernetes API compatibility.

## Apply

```bash
git clone https://github.com/calfonso/rusternetes.git
cd rusternetes
/path/to/this/bundle/apply_iroh_p2p_patch.sh .
```

## Build

```bash
cargo check -p rusternetes-kube-proxy --features iroh-overlay
cargo check -p rusternetes --features iroh-overlay
```

## Enable at runtime

```bash
export RUSTERNETES_IROH_OVERLAY=1
export RUSTERNETES_IROH_BIND_ADDR=0.0.0.0:0
```

Optional, and commonly needed for ClusterIP/NodePort traffic DNATed to loopback tunnel listeners:

```bash
sudo sysctl -w net.ipv4.conf.all.route_localnet=1
```

See `docs/IROH_P2P_NETWORKING.md` for the design and full environment variable list.

## Important compatibility note

The patch is additive. Without `--features iroh-overlay` and `RUSTERNETES_IROH_OVERLAY=1`, Rusternetes keeps its existing kube-proxy behavior. With the overlay enabled, TCP EndpointSlice backends with remote `nodeName` values are routed through iroh; UDP and ambiguous endpoints fall back to native routing.
