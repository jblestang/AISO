## Software Test Definition (STD)

### 1. Purpose

Define tests and procedures to verify requirements in `docs/SRS.md`.

### 2. Test Environment

- OS: macOS/Linux
- Tooling: Rust toolchain (`cargo`), network access for live polling (optional)
- Command: `cargo test`

### 3. Unit Tests

Unit tests are located in `src/main.rs` under `#[cfg(test)]`.

#### UT-001 Pass: fixed field presence (nulls allowed) + alphabetical key order + deterministic serialization

- **Requirement coverage**: RQ-007, RQ-008, RQ-009, RQ-010, RQ-011
- **Test**: `sample_expected_to_pass_filter_keeps_nulls_and_orders_keys`
- **Method**:
  - Compile a minimal upstream schema.
  - Validate and filter a message containing an allowlisted `null`.
  - Assert filtered output preserves the null field (fixed field presence).
  - Assert JSON string equals expected canonical minified form.

#### UT-002 Fail: invalid ICAO24 pattern rejected

- **Requirement coverage**: RQ-007, NFR-001
- **Test**: `sample_expected_to_not_pass_filter_invalid_icao24`
- **Method**:
  - Compile schema requiring ICAO24 regex.
  - Validate and filter message with invalid ICAO24.
  - Assert validation fails.

#### UT-003 Demonstration: logic issue when extra fields exist with additionalProperties=false

**(superseded)**: The implementation now filters before strict validation. The current behavior is tested by UT-003a.

#### UT-003a Pass: extra fields dropped before strict validation

- **Requirement coverage**: RQ-008, RQ-008a, RQ-007
- **Test**: `extra_fields_are_dropped_before_strict_validation`
- **Method**:
  - Use a strict schema with `additionalProperties:false`.
  - Validate/filter a message containing an extra field not on the allowlist.
  - Assert the extra field is removed and the result passes strict validation.

#### UT-004 Schema file extraction: provider and input schema pointer

- **Requirement coverage**: RQ-002, RQ-003, RQ-004, RQ-005, RQ-006, RQ-008
- **Test**: `schema_file_extracts_provider_and_input_schema_pointer`
- **Method**:
  - Load `schema/upstream_message.schema.json`.
  - Extract `x-provider` and `x-inputSchemaPointer`.
  - Assert expected values.

#### UT-005 Covert channel mitigation: serialization is minified and alphabetical

- **Requirement coverage**: RQ-010, RQ-011, NFR-002
- **Test**: `serialization_is_alphabetical_and_whitespace_free_to_reduce_covert_channel_risk`
- **Method**:
  - Validate/filter a message and serialize it.
  - Assert output string has no whitespace characters and matches canonical key ordering.

#### UT-006 Field-by-field constraints for upstream schema

- **Requirement coverage**: RQ-007, RQ-008, RQ-009, NFR-001
- **Test**: `upstream_schema_constraints_per_field`
- **Method**:
  - Load and compile the real upstream schema file.
  - Start from a known-good message.
  - Mutate each field to violate constraints (type/pattern/range/required).
  - Assert failures; assert `null` is preserved when allowed.

#### UT-007 Schema extension validation (missing/malformed)

- **Requirement coverage**: RQ-016, NFR-001
- **Test**: `schema_extensions_missing_or_malformed_are_rejected`
- **Method**:
  - Attempt to extract `x-allowUpstream` flags, `x-inputSchemaPointer`, and `x-provider` from malformed schemas.
  - Assert extraction fails (fail closed).

#### UT-008 Reject transmitted strings containing whitespace

- **Requirement coverage**: RQ-017, NFR-001
- **Test**: `transmitted_string_values_must_not_contain_whitespace`
- **Method**:
  - Validate/filter a message where an allowlisted string contains whitespace.
  - Assert the message is rejected (fail closed).

#### UT-009 Numeric quantization to fixed precision

- **Requirement coverage**: RQ-018, NFR-001
- **Test**: `numeric_values_are_quantized_to_fixed_precision`
- **Method**:
  - Validate/filter a message with floating-point numeric fields.
  - Assert the serialized output uses a fixed fractional precision (five decimal digits for latitude/longitude) compatible with the schema `multipleOf` constraint.

### 4. Integration / Manual Tests

#### IT-001 UDP output observable via netcat

- **Requirement coverage**: RQ-012, RQ-013
- **Procedure**:

```bash
nc -u -l 42000
```

In another terminal:

```bash
cargo run --release -- --udp-dest 127.0.0.1:42000 --poll-secs 5
```

Observe JSON arrays being received (one per poll, possibly chunked).

#### IT-002 Provider JSON size cap

- **Requirement coverage**: RQ-019
- **Procedure** (conceptual, requires controllable provider):
  - Configure a test provider endpoint that returns JSON bodies just below and just above 10 MiB.
  - Verify that responses below the limit are accepted and processed.
  - Verify that responses above the limit cause the proxy to log an error and drop the poll without emitting UDP data.

### 5. Traceability Matrix (Requirements → Tests)

- **RQ-001**: (manual) IT-001 via running proxy
- **RQ-002**: UT-004
- **RQ-003**: UT-004 (extraction); (functional) exercised by runtime polling
- **RQ-004**: UT-004
- **RQ-005**: UT-004 (config extraction); (functional) exercised by runtime polling
- **RQ-006**: UT-004 (config extraction); (functional) exercised by runtime polling
- **RQ-007**: UT-001, UT-002, UT-006
- **RQ-008**: UT-001, UT-003a, UT-005
- **RQ-008a**: UT-003a
- **RQ-009**: UT-001, UT-006
- **RQ-010**: UT-001, UT-005
- **RQ-011**: UT-001, UT-005
- **RQ-012**: IT-001
- **RQ-013**: IT-001 (observational); covered by design, can be expanded with mock tests
- **RQ-015**: (manual) observe logs while running
- **RQ-016**: UT-007
- **RQ-017**: UT-008
- **RQ-018**: UT-009
 - **RQ-019**: IT-002

Note: some requirements are best verified as integration/manual (network I/O). If you want full automation, we can add mocked HTTP + UDP receiver tests.
