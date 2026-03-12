# AISD OpenSky Data Diode Proxy

Rust proxy that **polls OpenSky**, validates and sanitizes aircraft state data, and forwards it **one-way** over **UDP** (plus an optional **TCP snapshot** interface).

## 4-stage pipeline

1. **Acquire (Internet side)**: periodic HTTPS GET to OpenSky `/api/states/all`.
2. **Normalize (generic JSON)**: transform OpenSky state vectors into a strict JSON object per aircraft (using `serde_json::Value`).
3. **Validate + Constrain (JSON Schema)**:
   - validate message against `schema/upstream_message.schema.json`
   - enforce per-property extension: `x-allowUpstream` (field allowlist)
4. **Transmit (diode boundary)**:
   - **UDP-out**: send sanitized JSON payloads (arrays of upstream objects)

## Documentation

See `docs/`:

- `docs/SRS.md`: software requirements specification
- `docs/STD.md`: software test definition (incl. traceability)
- `docs/STR.md`: software test results

## Schema extension: per-property `x-allowUpstream`

`schema/upstream_message.schema.json` contains:

- normal JSON Schema constraints (`type`, ranges, `additionalProperties: false`, etc.)
- per-property extension flags `x-allowUpstream: true` on the **only** fields permitted to cross the boundary.

The proxy drops everything else even if present in input.

## Run

```bash
cargo run --release -- \
  --udp-dest 192.0.2.10:42000 \
  --poll-secs 5
```

Environment logging:

```bash
RUST_LOG=info cargo run --release
```
