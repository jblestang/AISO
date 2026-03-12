## Cybersecurity Peer Review: DoS & Illicit Information Risks

### 1. Scope

This review assesses how `aisd-opensky-proxy` could be used or abused to:

- cause **denial of service (DoS)** (availability loss), or
- **pass information illicitly** (confidentiality loss) across a one-way boundary.

The software is a low-side (Internet-facing) proxy that polls a JSON provider (OpenSky by default), validates data with JSON Schema, sanitizes/filters it, and transmits sanitized JSON over UDP.

### 2. Assumptions and Trust Boundaries

- **Provider data is untrusted**. It may be malformed, unexpectedly large, or adversarial.
- **Schema file is trusted configuration and not attacker-reachable at runtime**. It is delivered as a signed, integrity‑protected artifact and only changed through controlled deployment or administrative processes.
- **UDP receiver is assumed to be on the higher-security side** (data diode direction is low → high).
- **Network DoS (UDP flooding) is primarily handled by external controls** (firewall/diode enforcing rate limits and filtering).

### 3. DoS (Availability) Risks

#### 3.1 Oversized / high-rate provider responses

- **Attack vector**: Provider returns an extremely large records array (e.g., `states`) or very frequent updates.
- **Impact**: High CPU and memory use during parse, validate, transform, and chunk/send. Poll loop can fall behind.
- **Implementation notes**:
  - A hard cap of **10 MiB** is enforced on the provider JSON body (`MAX_JSON_BYTES`). Responses larger than this are rejected before parsing.
  - Chunking logic still performs work proportional to the number of records and can be costly for large-but-allowed responses.
- **Mitigations**:
  - Enforce a hard cap on JSON body size (implemented).
  - Consider additional caps:
    - max records processed per poll
    - max UDP datagrams per poll
  - Replace clone-based chunk sizing with a non-quadratic strategy (incremental accounting or streaming).
  - Implement backoff/jitter on repeated failures to avoid tight loops.
- **Residual risk status**: **OPEN (bounded)** – JSON size is capped at 10 MiB, but work per poll can still be high within that envelope.

#### 3.2 Schema validation cost

- **Attack vector**: Large JSON + expensive validation features (regex, deep structures).
- **Impact**: CPU DoS.
- **Mitigations**:
  - Keep regex patterns simple (current ICAO pattern is simple).
  - Filter/normalize first (drop non-allowlisted keys and apply canonicalization), then validate strict output to reduce validation workload and avoid rejecting safe messages due to irrelevant extra fields.
- **Residual risk status**: **PARTIALLY MITIGATED** – ordering reduces cost, but large (yet allowed) inputs can still be expensive to validate.

#### 3.3 UDP flood / network saturation

- **Attack vector**: Many records or chunking into many UDP packets per poll.
- **Impact**: Link saturation; receiver overload.
- **Assumed mitigation outside this component**:
  - A dedicated firewall / diode enforcement device in front of the equipment performs rate limiting and flood protection for UDP.
- **Residual considerations in this component (defence-in-depth)**:
  - Cap datagrams per poll and drop remainder with warnings/metrics.
  - Consider batching strategy and explicit MTU-aware sizing.
- **Residual risk status**: **ASSUMED HANDLED EXTERNALLY** – treat as closed for this component; re-open only if network protections are not present.

#### 3.4 Logging as a DoS amplifier

- **Attack vector**: Crafted input causes repeated warnings (validation errors, oversize drops).
- **Impact**: Disk/CPU pressure from logs.
- **Mitigations**:
  - Rate-limit repetitive log messages; aggregate counts. (Not yet implemented in code.)
- **Residual risk status**: **OPEN (minor)** – log volume can still be driven by hostile provider behaviour.

### 4. Illicit Information / Covert Channels (Confidentiality) Risks

#### 4.1 Schema/provider directives as a configuration exfil channel

- **Assumptions**: The schema file is part of a signed, integrity‑protected configuration set (e.g. verified signature or hash at deployment/startup).
- **Attack vector** (residual): If an insider or misconfigured deployment **still** manages to alter the schema and its signature/hash, it can:
  - change provider URL
  - modify field mappings
  - modify allowlist (per-property `x-allowUpstream`)
  - encode exfiltration in allowed fields
- **Impact**: Controlled exfiltration across the boundary within the space defined by the (now‑trusted) configuration.
- **Mitigations**:
  - Treat schema as a controlled artifact under configuration/change-management:
    - strict file permissions
    - mandatory integrity verification (signature/hash check) before use
    - change control and monitoring
- **Residual risk status**: **CLOSED (for this component)** – remaining exposure is handled by organizational and deployment controls, not by changes to this code.

#### 4.2 Whitespace-modulated covert channel in JSON

- **Attack vector**: Encode bits by adding/removing spaces/newlines between tokens.
- **Current behavior**:
  - Output uses minified serialization (`serde_json::to_string()`); unit tests assert no inter-token whitespace.
  - Transmitted string values are rejected if they contain whitespace characters (space, tab, newline, carriage return).
- **Residual risk**: None via whitespace modulation in this component; no attacker-controlled whitespace remains.
- **Mitigations**:
  - Reject transmitted string values containing whitespace (fail closed).
  - Constrain transmitted string values with strict patterns/lengths (e.g., RFC3339 for `ts`).
  - Normalize strings (trim/uppercase) only if explicitly required and documented.
- **Residual risk status**: **CLOSED** – whitespace-based covert channel is eliminated.

#### 4.3 Numeric covert channel (precision modulation)

- **Attack vector**: Encode data by varying floating-point precision or least significant digits.
- **Current behavior**:
  - Floating-point values are quantized to a fixed precision of **five decimal digits** before serialization.
  - Latitude/longitude are additionally constrained in the schema via custom extension `x-multipleOf: 0.00001`.
- **Mitigations**:
  - Maintain fixed-precision quantization and schema `x-multipleOf` constraints to cap channel capacity in low-order bits.
  - If needed, switch to scaled integers for even tighter control.
- **Residual risk status**: **CLOSED (for this component)** – numeric precision and allowed ranges are fully defined and enforced by quantization plus schema constraints.

#### 4.4 Field presence/absence modulation

- **Attack vector**: Encode bits by selectively omitting optional fields.
- **Current behavior**:
  - The sanitizer emits a **fixed set of allowlisted fields** (those marked `x-allowUpstream: true`).
  - Missing values are represented as explicit `null` (fixed field presence, variable content only).
- **Mitigations**:
  - Keep fixed field presence to reduce the capacity of a presence/absence channel.
  - Combine with numeric quantization and string constraints to limit other modulation avenues.
- **Residual risk status**: **CLOSED** – modulation by presence/absence of fields is not available.

#### 4.5 Key order modulation

- **Attack vector**: Encode bits by permuting key order.
- **Current behavior**:
  - Keys are inserted in a deterministic order (sorted allowlist) and serialized with `serde_json::to_string()`.
  - Unit tests assert canonical ordering and exact serialized strings.
- **Mitigation**:
  - Keep deterministic alphabetical key ordering and avoid pretty-printing or alternative serializers.
- **Residual risk status**: **CLOSED** – canonical alphabetical ordering removes this modulation channel.

### 5. Validation/Filtering Ordering: Security-Relevant Behavior

- Current pipeline performs **allowlist filtering first**, then applies output canonicalization policies (string and numeric constraints), then validates the sanitized object against the upstream schema.
- With `additionalProperties:false` on the upstream schema, extra fields do not cause validation failure because they are removed before validation.
- **Security impact**:
  - **Positive**: Reduces CPU cost of schema validation and reduces self-DoS risk from irrelevant extra fields while still enforcing strict output structure.
  - **Risk consideration**: If canonicalization/filtering contained a bug, validation would only see the post-filter representation; this is addressed by targeted unit tests that exercise filtering, canonicalization, and validation together, plus a strict schema on the sanitized output.
- Recommendation:
  - Keep permissive handling pre-filter, but retain strict validation of the post-filter output schema (current implementation).
  - Maintain and extend tests whenever filtering or canonicalization logic changes, to preserve the intended ordering guarantees.
 - **Residual risk status**: **CLOSED (for this component)** – ordering is intentional, well-defined, and covered by tests and schema constraints.

### 6. Supply Chain and Dependency Considerations

- The dependency graph (HTTP/TLS, JSON schema validation, async runtime) is non-trivial.
- Recommendations:
  - run vulnerability scanning (e.g., `cargo audit`)
  - keep `Cargo.lock` under review
  - minimal features and strict update process
- **Residual risk status**: **CLOSED (for this component)** – remaining exposure is handled by development-process and supply-chain audits, not by changes to this code.

