# Rusternetes Iroh P2P Patch Bundle

This bundle adds an experimental P2P networking overlay based on [Iroh](https://www.iroh.computer/) to Rusternetes.

## Contents

- `crates/iroh-overlay/`: Rust crate implementing the overlay.
- `docs/IROH_P2P_NETWORKING.md`: design notes / usage details.
- `rusternetes-iroh-overlay.diff`: patch file.
- `apply_iroh_p2p_patch.sh`: helper script to apply the patch.

## Quick start

```bash
./apply_iroh_p2p_patch.sh /path/to/rusternetes
```
