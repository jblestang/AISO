## Software Requirements Specification (SRS)

### 1. Purpose

This document specifies requirements for `aisd-opensky-proxy`, a low-side (Internet-facing) data collection and sanitization proxy intended to feed a higher-security network through a **one-way data diode**.

### 2. Scope

The software:

- Polls a provider JSON API (OpenSky by default, configured in the JSON Schema).
- Validates the **full provider response** against an embedded input JSON Schema.
- Transforms provider records into a strict upstream message format.
- Validates and sanitizes upstream messages using JSON Schema plus schema extensions.
- Emits sanitized messages **one-way** via UDP and optionally serves a TCP snapshot for pull-based ingestion.

### 3. Definitions

- **Upstream message**: the per-aircraft JSON object emitted by this proxy (or arrays of such objects).
- **Schema extensions**: non-standard JSON Schema keys used by this program:
  - `x-provider`
  - `x-inputSchemaPointer`
  - per-property `x-allowUpstream`

### 3.1 Custom JSON Schema Extensions (Normative)

This software defines the following **custom JSON Schema extensions**. They are **required** for correct operation unless stated otherwise.

#### EXT-001 Per-property `x-allowUpstream`

- **Location**: under each JSON Schema property (`properties.<field>.x-allowUpstream`)
- **Type**: boolean
- **Meaning**: marks a property as permitted to be transmitted upstream.
- **Rules**:
  - Only properties with `x-allowUpstream: true` shall appear in sanitized upstream objects (RQ-008, RQ-009).

#### EXT-002 `x-inputSchemaPointer`

- **Type**: string (JSON Pointer)
- **Meaning**: points to a schema within the same JSON document that validates the **raw provider response**.
- **Rules**:
  - The program shall compile and validate raw provider data against the schema referenced by this pointer (RQ-003).

#### EXT-003 `x-provider`

- **Type**: object
- **Meaning**: provider configuration embedded inside the schema.
- **Required properties**:
  - `url` (string): provider endpoint URL (RQ-002)
  - `records_path` (string, JSON Pointer): where the records array lives in the provider response (RQ-004)
  - `root_unix_ts_path` (string, JSON Pointer): provider unix timestamp (RQ-005)
  - `output_ts_field` (string): upstream field name for timestamp (RQ-005)
  - `fields` (array): field mappings (RQ-006)
    - each element is an object with:
      - `name` (string): upstream field name
      - `record_pointer` (string, JSON Pointer): pointer applied to each record element

### 4. External Interfaces

#### 4.1 CLI

The program shall accept CLI arguments:

- `--schema-path <path>`: JSON Schema file containing upstream schema, provider directives, and input schema.
- `--poll-secs <n>`: polling period (seconds).
- `--udp-bind <addr:port>`: local UDP bind address.
- `--udp-dest <addr:port>`: destination UDP address.
- `--tcp-bind <addr:port>`: TCP bind address for snapshot service.
- `--max-payload-bytes <n>`: max UDP payload size; messages larger than this are dropped or chunked.

#### 4.2 Network Interfaces

- **Provider**: HTTPS GET to the configured provider URL.
- **UDP output**: sends sanitized JSON payloads to `--udp-dest`.
- **TCP snapshot**: listens on `--tcp-bind`, writes one JSON line to each client, then closes.

### 5. Functional Requirements

#### RQ-001 Polling

The software shall poll the provider endpoint at a fixed interval (`--poll-secs`).

#### RQ-002 Provider URL from Schema

The software shall obtain the provider URL from the JSON Schema extension `x-provider.url`.

#### RQ-003 Input Schema Validation

The software shall validate the **full provider JSON response** against a JSON Schema located at the JSON Pointer given by `x-inputSchemaPointer` in the same schema document.

#### RQ-004 Record Extraction

The software shall locate the records array in the provider response using `x-provider.records_path` (JSON Pointer).

#### RQ-005 Timestamp Derivation

The software shall read a unix timestamp from the provider response at `x-provider.root_unix_ts_path` (JSON Pointer), convert it to RFC3339 UTC, and place it into each upstream object under the field name `x-provider.output_ts_field`.

#### RQ-006 Field Mapping

For each record element, the software shall map fields described in `x-provider.fields[]`:

- `name`: upstream field name
- `record_pointer`: JSON Pointer applied to the record element to obtain the value (or `null` if missing)

#### RQ-007 Upstream Schema Validation

The software shall validate each upstream object against the root JSON Schema.

#### RQ-008 Allowlist Filtering

The software shall enforce a schema extension allowlist by including only properties whose schemas are marked with `x-allowUpstream: true` and dropping any other properties.

#### RQ-008a Filtering/Validation Ordering

The software shall apply allowlist filtering and output canonicalization policies **before** validating the upstream message against the upstream schema, so that strict schemas (e.g. `additionalProperties: false`) do not cause rejection due to irrelevant extra fields.

#### RQ-009 Fixed Field Presence (Nulls Allowed)

The software shall emit all allowlisted fields in each sanitized upstream object. If an allowlisted field is missing from the transformed object, it shall be emitted with the value `null`.

#### RQ-010 Deterministic Output: Alphabetical Key Order

The software shall serialize upstream objects with keys ordered **alphabetically**.

#### RQ-011 Deterministic Output: No Inter-Token Whitespace

The software shall serialize JSON outputs in a minified form with no inter-token whitespace (spaces/tabs/newlines outside string values), to reduce covert-channel capacity via whitespace modulation.

#### RQ-017 String Whitespace Restriction (Covert-Channel Reduction)

The software shall reject (fail closed) any sanitized upstream message that contains transmitted string values with whitespace characters (space, tab, newline, carriage return).

#### RQ-018 Numeric Quantization (Covert-Channel Reduction)

The software shall quantize all transmitted floating-point numeric values to a fixed precision of **five** decimal digits after the decimal separator before serialization, and the upstream schema for latitude/longitude shall constrain those fields to numeric steps of `0.00001` (JSON Schema `multipleOf`).

#### RQ-012 UDP Transmission

The software shall send sanitized payloads via UDP to `--udp-dest`.

#### RQ-013 Chunking / Payload Limit

The software shall enforce `--max-payload-bytes` by:

- chunking arrays into multiple UDP datagrams when possible, and/or
- dropping oversize payloads with a warning.

#### RQ-019 Provider JSON Size Cap

The software shall reject any provider HTTP response whose JSON body exceeds 10 mebibytes (10 * 1024 * 1024 bytes) in size.

#### RQ-015 Logging

The software shall log operational events at info/warn level, including UDP bind, TCP bind, validation drops, oversize drops, and send errors.

#### RQ-016 Extension Validation (Fail Closed)

The software shall fail closed (no transmission) if required schema extensions are missing or malformed, including:

- no properties marked with valid `x-allowUpstream: true`
- missing/invalid `x-inputSchemaPointer`
- missing/invalid `x-provider` (including missing required properties)

### 6. Non-Functional Requirements

#### NFR-001 Safety and Robustness

The software shall fail closed for invalid data: if validation fails, data shall not be transmitted.

#### NFR-002 Repeatability

Given the same sanitized data structure, serialization shall be deterministic (key order and whitespace).

### 7. Requirement-to-Implementation Notes

- The OpenSky-specific input validation schema and extraction directives are carried inside `schema/upstream_message.schema.json` under `$defs` and `x-provider`.
