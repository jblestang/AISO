## Cybersecurity Peer Review: DoS & Illicit Information Risks

### 1. Scope

This review assesses how `aisd-opensky-proxy` could be used or abused to:

- cause **denial of service (DoS)** (availability loss), or
- **pass information illicitly** (confidentiality loss) across a one-way boundary.

The software is a low-side (Internet-facing) proxy that polls a JSON provider (OpenSky by default), validates data with JSON Schema, sanitizes/filters it, and transmits sanitized JSON over UDP (plus an optional TCP snapshot service).

### 2. Assumptions and Trust Boundaries

- **Provider data is untrusted**. It may be malformed, unexpectedly large, or adversarial.
- **Schema file is trusted configuration**. If an attacker can modify the schema, they can change allowlists, mappings, endpoints, etc.
- **UDP receiver is assumed to be on the higher-security side** (data diode direction is low → high).
- **TCP snapshot** is an auxiliary local interface and can be exposed unintentionally if bound to non-local addresses.

### 3. DoS (Availability) Risks

#### 3.1 Oversized / high-rate provider responses

- **Attack vector**: Provider returns an extremely large records array (e.g., `states`) or frequent updates.
- **Impact**: High CPU and memory use during parse, validate, transform, and chunk/send. Poll loop can fall behind.
- **Implementation note**: Current chunking logic creates clones during sizing decisions, which can become costly with large arrays.
- **Mitigations**:
  - Add **hard caps**:
    - max JSON bytes read/parsed per poll
    - max records processed per poll
    - max UDP datagrams per poll
  - Replace clone-based chunk sizing with a non-quadratic strategy (incremental accounting or streaming).
  - Implement backoff/jitter on repeated failures to avoid tight loops.

#### 3.2 Schema validation cost

- **Attack vector**: Large JSON + expensive validation features (regex, deep structures).
- **Impact**: CPU DoS.
- **Mitigations**:
  - Keep regex patterns simple (current ICAO pattern is simple).
  - Filter/normalize first (drop non-allowlisted keys and nulls), then validate strict output to reduce validation workload and avoid rejecting safe messages due to irrelevant extra fields.

#### 3.3 UDP flood / network saturation

- **Attack vector**: Many records or chunking into many UDP packets per poll.
- **Impact**: Link saturation; receiver overload.
- **Mitigations**:
  - Cap datagrams per poll and drop remainder with warnings/metrics.
  - Consider batching strategy and explicit MTU-aware sizing.

#### 3.4 TCP snapshot misuse

- **Attack vector**: Repeated connections to snapshot port (especially if exposed on 0.0.0.0).
- **Impact**: File descriptor and CPU consumption; log flooding.
- **Mitigations**:
  - Bind TCP to localhost by default (or firewall restrict).
  - Add connection rate limiting / max concurrent connections.
  - Consider disabling TCP snapshot in production.

#### 3.5 Logging as a DoS amplifier

- **Attack vector**: Crafted input causes repeated warnings (validation errors, oversize drops).
- **Impact**: Disk/CPU pressure from logs.
- **Mitigations**:
  - Rate-limit repetitive log messages; aggregate counts.

### 4. Illicit Information / Covert Channels (Confidentiality) Risks

#### 4.1 Schema/provider directives as a configuration exfil channel

- **Attack vector**: If attacker can alter the schema file, they can:
  - change provider URL
  - modify field mappings
  - modify allowlist
  - encode exfiltration in allowed fields
- **Impact**: Controlled exfiltration across the boundary.
- **Mitigations**:
  - Treat schema as a controlled artifact:
    - strict file permissions
    - integrity checks (hash/signature)
    - change control and monitoring

#### 4.2 Whitespace-modulated covert channel in JSON

- **Attack vector**: Encode bits by adding/removing spaces/newlines between tokens.
- **Current behavior**: Output uses minified serialization (`serde_json::to_string()`), and unit tests assert no inter-token whitespace.
- **Residual risk**: If a transmitted **string value** can contain whitespace, an attacker could encode bits inside the string payload itself.
- **Mitigations**:
  - Reject transmitted string values containing whitespace (fail closed).
  - Constrain transmitted string values with strict patterns/lengths (e.g., RFC3339 for `ts`).
  - Normalize strings (trim/uppercase) only if explicitly required and documented.

#### 4.3 Numeric covert channel (precision modulation)

- **Attack vector**: Encode data by varying floating-point precision or least significant digits.
- **Current behavior**: Numbers are forwarded as provided (after schema constraints).
- **Mitigations**:
  - Quantize/round numeric values to a fixed resolution.
  - Prefer scaled integers (e.g., lat/lon in 1e-5 degrees) to reduce channel capacity.

#### 4.4 Field presence/absence modulation

- **Attack vector**: Encode bits by selectively omitting optional fields.
- **Current behavior**: The sanitizer emits a **fixed set of allowlisted fields**; missing values are represented as explicit `null`.
- **Mitigations**:
  - Keep fixed field presence to reduce the capacity of a presence/absence channel.
  - If further reduction is required, consider quantization (see 4.3) and strict string constraints (see 4.2).

#### 4.5 Key order modulation

- **Attack vector**: Encode bits by permuting key order.
- **Current behavior**: Keys are serialized in **alphabetical order**; unit tests assert canonical ordering.
- **Mitigation**: Keep deterministic key ordering and avoid pretty-printing.

### 5. Validation/Filtering Ordering: Security-Relevant Behavior

- Current pipeline performs **allowlist filtering first**, then applies output canonicalization policies, then validates the sanitized object against the upstream schema.
- With `additionalProperties:false` on the upstream schema, extra fields do not cause validation failure because they are removed before validation.
- **Security impact**:
  - **Positive**: Reduces CPU cost of schema validation and reduces self-DoS risk from irrelevant extra fields while still enforcing strict output structure.
  - **Negative**: If canonicalization/filtering contains a bug, validation will only see the post-filter representation; mitigated by unit tests and strict schema on the sanitized output.
- Recommendation:
  - Keep permissive handling pre-filter, but retain strict validation of the post-filter output schema (current implementation).

### 6. Supply Chain and Dependency Considerations

- The dependency graph (HTTP/TLS, JSON schema validation, async runtime) is non-trivial.
- Recommendations:
  - run vulnerability scanning (e.g., `cargo audit`)
  - keep `Cargo.lock` under review
  - minimal features and strict update process

### 7. High-Impact Recommendations (Priority)

1. **Eliminate clone-based chunking** to reduce CPU/memory DoS risk.
2. Add **caps** (max response size, max records, max datagrams) to bound work per poll.
3. **Quantize numeric fields** and tighten string constraints to reduce covert channels.
4. Lock down schema file integrity (permissions + hashes/signatures).
5. Restrict/disable TCP snapshot in production and rate-limit connections/logging.

