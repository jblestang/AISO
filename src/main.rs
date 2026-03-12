use anyhow::{anyhow, Context, Result};
use clap::Parser;
use chrono::{DateTime, Utc};
use jsonschema::JSONSchema;
use serde_json::{Map, Number, Value};
use std::{net::SocketAddr, path::PathBuf, sync::Arc, time::Duration};
use tokio::{net::UdpSocket, time};
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;
 
/// Data diode side proxy:
/// - polls a JSON provider (HTTPS) on a timer
/// - validates generic JSON + JSON Schema
/// - filters to an allowlisted field subset (schema extension)
/// - forwards sanitized JSON via UDP
#[derive(Parser, Debug, Clone)]
#[command(name = "aisd-opensky-proxy")]
struct Args {
    /// Poll interval in seconds
    #[arg(long, default_value_t = 5)]
    poll_secs: u64,
 
    /// UDP destination (high-side listener address)
    #[arg(long, default_value = "127.0.0.1:42000")]
    udp_dest: SocketAddr,
 
    /// Local UDP bind (low-side)
    #[arg(long, default_value = "0.0.0.0:0")]
    udp_bind: SocketAddr,
 
    /// Path to JSON schema
    #[arg(long, default_value = "schema/upstream_message.schema.json")]
    schema_path: PathBuf,
 
    /// Max UDP payload bytes (drop if exceeded)
    #[arg(long, default_value_t = 1400)]
    max_payload_bytes: usize,
}
 
#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive("info".parse()?))
        .init();
 
    let args = Args::parse();

    let schema_json = tokio::fs::read_to_string(&args.schema_path)
        .await
        .with_context(|| format!("read schema {}", args.schema_path.display()))?;
    let schema_value: Value = serde_json::from_str(&schema_json).context("parse schema JSON")?;
    // jsonschema 0.18 stores references into the schema; give it a stable `'static` address.
    let schema_value: &'static Value = Box::leak(Box::new(schema_value));
 
    let allow_fields =
        extract_allow_fields(schema_value).context("extract x-allowUpstream flags from schema")?;
    let provider = extract_provider(schema_value).context("extract x-provider from schema")?;
    let input_schema_ptr = extract_input_schema_pointer(schema_value)
        .context("extract x-inputSchemaPointer from schema")?;
    let input_schema_value = schema_value
        .pointer(&input_schema_ptr)
        .ok_or_else(|| anyhow!("x-inputSchemaPointer not found: {}", input_schema_ptr))?;
    let compiled_input = JSONSchema::options()
        .compile(input_schema_value)
        .context("compile input JSON schema")?;
    // Note: jsonschema crate version used here compiles schemas without an explicit draft enum.
    // The schema file declares draft 2020-12 via `$schema`; compilation will follow supported keywords.
    let compiled = JSONSchema::options()
        .compile(schema_value)
        .context("compile JSON schema")?;
 
    let compiled = Arc::new(compiled);
    let compiled_input = Arc::new(compiled_input);
    let allow_fields = Arc::new(allow_fields);
    let provider = Arc::new(provider);
 
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .user_agent("aisd-opensky-proxy/0.1")
        .build()
        .context("build HTTP client")?;
 
    let udp_socket = UdpSocket::bind(args.udp_bind)
        .await
        .with_context(|| format!("bind UDP {}", args.udp_bind))?;
    info!("UDP bound on {}", udp_socket.local_addr()?);
 
    let poll_task = {
        let args = args.clone();
        let provider = provider.clone();
        let compiled_input = compiled_input.clone();
        let compiled = compiled.clone();
        let allow_fields = allow_fields.clone();
        let client = client.clone();
        let udp_socket = udp_socket;
        tokio::spawn(async move {
            if let Err(e) = poll_loop(
                &client,
                provider,
                compiled_input,
                Duration::from_secs(args.poll_secs),
                args.udp_dest,
                args.max_payload_bytes,
                compiled,
                allow_fields,
                udp_socket,
            )
            .await
            {
                error!(error = %e, "poll loop exited");
            }
        })
    };
 
    let _ = tokio::try_join!(poll_task)?;
    Ok(())
}
 
async fn poll_loop(
    client: &reqwest::Client,
    provider: Arc<ProviderSpec>,
    input_schema: Arc<JSONSchema>,
    interval: Duration,
    udp_dest: SocketAddr,
    max_payload_bytes: usize,
    schema: Arc<JSONSchema>,
    allow_fields: Arc<Vec<String>>,
    udp: UdpSocket,
) -> Result<()> {
    let mut tick = time::interval(interval);
    loop {
        tick.tick().await;
        match fetch_and_transform(client, &provider, &input_schema).await {
            Ok(messages) => {
                // Build one "list of aircraft" per poll. If too large for UDP, chunk into multiple arrays.
                let mut current_chunk: Vec<Value> = Vec::new();
                let mut current_size_est: usize = 2; // [] minimal

                for msg in messages {
                    let filtered = match validate_and_filter(&msg, &schema, &allow_fields) {
                        Ok(v) => v,
                        Err(e) => {
                            warn!(error = %e, "message dropped (validation/filter)");
                            continue;
                        }
                    };

                    // Size-aware chunking by actual encoding (safe but slightly more CPU).
                    // First, try to add to current chunk; if it would exceed, flush current chunk.
                    let mut candidate = current_chunk.clone();
                    candidate.push(filtered.clone());
                    let candidate_bytes =
                        serde_json::to_vec(&Value::Array(candidate)).context("encode JSON")?;

                    if candidate_bytes.len() <= max_payload_bytes {
                        current_chunk.push(filtered);
                        current_size_est = candidate_bytes.len();
                        continue;
                    }

                    if !current_chunk.is_empty() {
                        let bytes = serde_json::to_vec(&Value::Array(std::mem::take(&mut current_chunk)))
                            .context("encode JSON")?;
                        if bytes.len() <= max_payload_bytes {
                            if let Err(e) = udp.send_to(&bytes, udp_dest).await {
                                warn!(error = %e, "udp send failed");
                            }
                        } else {
                            warn!(
                                size = bytes.len(),
                                max = max_payload_bytes,
                                "drop oversized UDP payload (chunk)"
                            );
                        }
                    }

                    // Now try to send the single element as its own chunk.
                    let single_bytes =
                        serde_json::to_vec(&Value::Array(vec![filtered])).context("encode JSON")?;
                    if single_bytes.len() > max_payload_bytes {
                        warn!(
                            size = single_bytes.len(),
                            max = max_payload_bytes,
                            "drop oversized UDP payload (single)"
                        );
                        continue;
                    }
                    if let Err(e) = udp.send_to(&single_bytes, udp_dest).await {
                        warn!(error = %e, "udp send failed");
                        continue;
                    }
                    current_size_est = 2;
                }

                if !current_chunk.is_empty() {
                    let bytes = serde_json::to_vec(&Value::Array(current_chunk)).context("encode JSON")?;
                    if bytes.len() > max_payload_bytes {
                        warn!(
                            size = bytes.len(),
                            max = max_payload_bytes,
                            last_est = current_size_est,
                            "drop oversized UDP payload (final chunk)"
                        );
                    } else if let Err(e) = udp.send_to(&bytes, udp_dest).await {
                        warn!(error = %e, "udp send failed");
                    }
                }
            }
            Err(e) => warn!(error = %e, "fetch/transform failed"),
        }
    }
}
 
#[derive(Debug, Clone)]
struct ProviderField {
    name: String,
    record_pointer: String,
}

#[derive(Debug, Clone)]
struct ProviderSpec {
    url: String,
    records_path: String,
    root_unix_ts_path: String,
    output_ts_field: String,
    fields: Vec<ProviderField>,
}

fn extract_provider(schema: &Value) -> Result<ProviderSpec> {
    let p = schema
        .get("x-provider")
        .and_then(|v| v.as_object())
        .ok_or_else(|| anyhow!("schema missing object 'x-provider'"))?;

    let url = p
        .get("url")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("x-provider.url must be string"))?
        .to_string();
    let records_path = p
        .get("records_path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("x-provider.records_path must be string"))?
        .to_string();
    let root_unix_ts_path = p
        .get("root_unix_ts_path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("x-provider.root_unix_ts_path must be string"))?
        .to_string();
    let output_ts_field = p
        .get("output_ts_field")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("x-provider.output_ts_field must be string"))?
        .to_string();

    let fields_arr = p
        .get("fields")
        .and_then(|v| v.as_array())
        .ok_or_else(|| anyhow!("x-provider.fields must be array"))?;
    let mut fields = Vec::with_capacity(fields_arr.len());
    for f in fields_arr {
        let fo = f
            .as_object()
            .ok_or_else(|| anyhow!("x-provider.fields elements must be objects"))?;
        let name = fo
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("x-provider.fields[].name must be string"))?
            .to_string();
        let record_pointer = fo
            .get("record_pointer")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("x-provider.fields[].record_pointer must be string"))?
            .to_string();
        fields.push(ProviderField { name, record_pointer });
    }
    Ok(ProviderSpec {
        url,
        records_path,
        root_unix_ts_path,
        output_ts_field,
        fields,
    })
}

fn extract_input_schema_pointer(schema: &Value) -> Result<String> {
    schema
        .get("x-inputSchemaPointer")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("schema missing string 'x-inputSchemaPointer'"))
        .map(|s| s.to_string())
}

async fn fetch_and_transform(
    client: &reqwest::Client,
    provider: &ProviderSpec,
    input_schema: &JSONSchema,
) -> Result<Vec<Value>> {
    // Hard cap on provider JSON size to reduce DoS surface.
    const MAX_JSON_BYTES: usize = 10 * 1024 * 1024; // 10 MiB
    let resp = client.get(&provider.url).send().await?.error_for_status()?;
    let body = resp.bytes().await?;
    if body.len() > MAX_JSON_BYTES {
        return Err(anyhow!(
            "provider JSON too large: {} bytes (max {})",
            body.len(),
            MAX_JSON_BYTES
        ));
    }
    let root: Value = serde_json::from_slice(&body)?;

    // Validate the full provider JSON against the input schema (OpenSky complete schema lives in $defs).
    if let Err(errors) = input_schema.validate(&root) {
        let mut first = None;
        for e in errors {
            first = Some(e.to_string());
            break;
        }
        return Err(anyhow!(
            "input schema validation failed{}",
            first.map(|s| format!(": {s}")).unwrap_or_default()
        ));
    }

    let unix = root
        .pointer(&provider.root_unix_ts_path)
        .and_then(|v| v.as_i64())
        .ok_or_else(|| {
            anyhow!(
                "provider JSON missing integer at {}",
                provider.root_unix_ts_path
            )
        })?;
    let ts: DateTime<Utc> = DateTime::<Utc>::from_timestamp(unix, 0)
        .ok_or_else(|| anyhow!("invalid unix time: {}", unix))?;
    let ts_value = Value::String(ts.to_rfc3339());

    let records = root
        .pointer(&provider.records_path)
        .and_then(|v| v.as_array())
        .ok_or_else(|| anyhow!("provider JSON missing array at {}", provider.records_path))?;

    let mut out = Vec::with_capacity(records.len());
    for rec in records {
        let mut obj = Map::new();
        obj.insert(provider.output_ts_field.clone(), ts_value.clone());

        for f in &provider.fields {
            let v = rec.pointer(&f.record_pointer).cloned().unwrap_or(Value::Null);
            obj.insert(f.name.clone(), v);
        }
        out.push(Value::Object(obj));
    }
    Ok(out)
}
 
fn validate_and_filter(msg: &Value, schema: &JSONSchema, allow_fields: &[String]) -> Result<Value> {
    // Stage 1: generic JSON check (Value already guarantees syntactic JSON)
    if !msg.is_object() {
        return Err(anyhow!("message must be JSON object"));
    }

    // Stage 2: allowlist filter with fixed field presence.
    // Policy: always emit all allowlisted fields; use null when missing.
    let obj = msg.as_object().unwrap();
    let mut keys: Vec<&str> = allow_fields.iter().map(|s| s.as_str()).collect();
    keys.sort_unstable();

    let mut filtered = Map::with_capacity(keys.len());
    for k in keys {
        filtered.insert(
            k.to_string(),
            obj.get(k).cloned().unwrap_or(Value::Null),
        );
    }

    // If, for any reason, no fields survive allowlist filtering, do not emit an empty object.
    if filtered.is_empty() {
        return Err(anyhow!(
            "sanitized object has no allowlisted fields; dropping message"
        ));
    }

    // Stage 3: canonicalize numeric and string values to reduce covert channels.
    // - Numeric policy: quantize floating-point values to a fixed number of decimal digits (five for latitude/longitude).
    // - String policy: transmitted string values must not contain whitespace characters.
    const SCALE: f64 = 100000.0; // five decimal digits
    for (k, v) in filtered.iter_mut() {
        match v {
            Value::String(s) => {
                if s.chars().any(|c| c == ' ' || c == '\n' || c == '\t' || c == '\r') {
                    return Err(anyhow!("string field '{}' contains whitespace", k));
                }
            }
            Value::Number(n) => {
                if let Some(f) = n.as_f64() {
                    let quantized = (f * SCALE).round() / SCALE;
                    *v = Value::Number(
                        Number::from_f64(quantized)
                            .ok_or_else(|| anyhow!("failed to encode quantized number for '{}'", k))?,
                    );
                }
            }
            _ => {}
        }
    }

    // Stage 3: schema validation on sanitized output
    let filtered_value = Value::Object(filtered);
    if let Err(errors) = schema.validate(&filtered_value) {
        let mut first = None;
        for e in errors {
            first = Some(e.to_string());
            break;
        }
        return Err(anyhow!(
            "schema validation failed{}",
            first.map(|s| format!(": {s}")).unwrap_or_default()
        ));
    }

    Ok(filtered_value)
}
 
fn extract_allow_fields(schema: &Value) -> Result<Vec<String>> {
    let props = schema
        .get("properties")
        .and_then(|v| v.as_object())
        .ok_or_else(|| anyhow!("schema missing object 'properties'"))?;
    let mut out = Vec::new();
    for (name, prop) in props {
        let allow = prop
            .get("x-allowUpstream")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if allow {
            out.push(name.clone());
        }
    }
    if out.is_empty() {
        return Err(anyhow!(
            "no properties marked with boolean 'x-allowUpstream: true'"
        ));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn compile_schema(schema: Value) -> JSONSchema {
        let schema: &'static Value = Box::leak(Box::new(schema));
        JSONSchema::options().compile(schema).expect("compile schema")
    }

    #[test]
    fn sample_expected_to_pass_filter_keeps_nulls_and_orders_keys() {
        let schema = json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "ts": { "type": "string", "x-allowUpstream": true },
                "icao24": { "type": "string", "pattern": "^[0-9a-fA-F]{6}$", "x-allowUpstream": true },
                "lat": { "type": ["number", "null"], "x-allowUpstream": true },
                "lon": { "type": ["number", "null"], "x-allowUpstream": true },
                "velocity": { "type": ["number", "null"], "x-allowUpstream": true }
            },
            "required": ["ts", "icao24"],
        });
        let compiled = compile_schema(schema.clone());
        let allow = extract_allow_fields(&schema).unwrap();

        let msg = json!({
            "icao24": "71c737",
            "ts": "2026-03-12T06:44:43+00:00",
            "lat": 35.2928123,
            "lon": 126.7196123,
            "velocity": null
        });

        let filtered = validate_and_filter(&msg, &compiled, &allow).unwrap();

        // Deterministic serialization: alphabetical key order and quantized numbers
        let s = serde_json::to_string(&filtered).unwrap();
        assert_eq!(
            s,
            r#"{"icao24":"71c737","lat":35.29281,"lon":126.71961,"ts":"2026-03-12T06:44:43+00:00","velocity":null}"#
        );
    }

    #[test]
    fn sample_expected_to_not_pass_filter_invalid_icao24() {
        let schema = json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "ts": { "type": "string", "x-allowUpstream": true },
                "icao24": { "type": "string", "pattern": "^[0-9a-fA-F]{6}$", "x-allowUpstream": true }
            },
            "required": ["ts", "icao24"],
        });
        let compiled = compile_schema(schema.clone());
        let allow = extract_allow_fields(&schema).unwrap();

        let msg = json!({
            "ts": "2026-03-12T06:44:43+00:00",
            "icao24": "BAD"
        });

        let err = validate_and_filter(&msg, &compiled, &allow).unwrap_err();
        assert!(err.to_string().contains("schema validation failed"));
    }

    #[test]
    fn extra_fields_are_dropped_before_strict_validation() {
        // With "filter first, then validate", extra fields are removed and the strict schema still passes.
        let schema = json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "ts": { "type": "string", "x-allowUpstream": true },
                "icao24": { "type": "string", "pattern": "^[0-9a-fA-F]{6}$", "x-allowUpstream": true }
            },
            "required": ["ts", "icao24"]
        });
        let compiled = compile_schema(schema.clone());
        let allow = extract_allow_fields(&schema).unwrap();

        let msg = json!({
            "ts": "2026-03-12T06:44:43+00:00",
            "icao24": "71c737",
            "callsign": "SHOULD_HAVE_BEEN_FILTERED"
        });

        let filtered = validate_and_filter(&msg, &compiled, &allow).unwrap();
        assert_eq!(filtered, json!({"icao24":"71c737","ts":"2026-03-12T06:44:43+00:00"}));
    }

    #[test]
    fn schema_file_extracts_provider_and_input_schema_pointer() {
        let schema_str = include_str!("../schema/upstream_message.schema.json");
        let schema: Value = serde_json::from_str(schema_str).unwrap();
        let provider = extract_provider(&schema).unwrap();
        assert!(provider.url.contains("opensky-network.org"));
        assert_eq!(provider.records_path, "/states");

        let ptr = extract_input_schema_pointer(&schema).unwrap();
        assert_eq!(ptr, "/$defs/opensky_response");
    }

    #[test]
    fn serialization_is_alphabetical_and_whitespace_free_to_reduce_covert_channel_risk() {
        // "Hidden channel modulated by spaces" is not possible if we never pretty-print and the
        // serializer produces a canonical minified representation. This test asserts that for a
        // representative message (with values that contain no whitespace themselves).
        let schema = json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "type": "object",
            "additionalProperties": true,
            "properties": {
                "ts": { "type": "string", "x-allowUpstream": true },
                "icao24": { "type": "string", "x-allowUpstream": true },
                "lat": { "type": ["number", "null"], "multipleOf": 0.00001, "x-allowUpstream": true },
                "lon": { "type": ["number", "null"], "multipleOf": 0.00001, "x-allowUpstream": true }
            },
            "required": ["ts", "icao24"],
        });
        let compiled = compile_schema(schema.clone());
        let allow = extract_allow_fields(&schema).unwrap();

        // Add an un-allowlisted field containing "space modulation" that should not survive filtering.
        let msg = json!({
            "ts": "2026-03-12T06:44:43+00:00",
            "icao24": "71c737",
            "lat": 35.2928123,
            "lon": 126.7196123,
            "covert": "     \t   \n   "
        });

        let filtered = validate_and_filter(&msg, &compiled, &allow).unwrap();
        let s = serde_json::to_string(&filtered).unwrap();

        // Alphabetical keys and quantized numbers
        assert_eq!(
            s,
            r#"{"icao24":"71c737","lat":35.29281,"lon":126.71961,"ts":"2026-03-12T06:44:43+00:00"}"#
        );

        // No whitespace outside of string values (here: none inside values either), so overall string has no whitespace.
        assert!(!s.chars().any(|c| c == ' ' || c == '\n' || c == '\t' || c == '\r'));
    }

    #[test]
    fn upstream_schema_constraints_per_field() {
        let schema_str = include_str!("../schema/upstream_message.schema.json");
        let schema: Value = serde_json::from_str(schema_str).unwrap();
        let allow = extract_allow_fields(&schema).unwrap();
        let compiled = compile_schema(schema);

        let base = json!({
            "ts": "2026-03-12T06:44:43+00:00",
            "icao24": "71c737",
            "lat": 35.29281,
            "lon": 126.71961,
            "velocity": 241.13,
            "true_track": 184.9,
            "geo_altitude": 6637.02,
            "baro_altitude": 6705.6,
            "vertical_rate": 0.33
        });

        // Sanity: base passes.
        validate_and_filter(&base, &compiled, &allow).expect("base message should pass");

        // Helper: build a modified object for each case.
        let mut cases: Vec<(&str, Box<dyn Fn(Value) -> Value>, bool)> = Vec::new();

        cases.push((
            "missing_ts",
            Box::new(|mut v| {
                v.as_object_mut().unwrap().remove("ts");
                v
            }),
            false,
        ));
        cases.push((
            "missing_icao24",
            Box::new(|mut v| {
                v.as_object_mut().unwrap().remove("icao24");
                v
            }),
            false,
        ));

        cases.push((
            "ts_wrong_type",
            Box::new(|mut v| {
                v.as_object_mut().unwrap().insert("ts".into(), json!(123));
                v
            }),
            false,
        ));

        cases.push((
            "icao24_bad_pattern_short",
            Box::new(|mut v| {
                v.as_object_mut().unwrap().insert("icao24".into(), json!("abc"));
                v
            }),
            false,
        ));
        cases.push((
            "icao24_bad_pattern_non_hex",
            Box::new(|mut v| {
                v.as_object_mut().unwrap().insert("icao24".into(), json!("zzzzzz"));
                v
            }),
            false,
        ));

        cases.push((
            "lat_too_low",
            Box::new(|mut v| {
                v.as_object_mut().unwrap().insert("lat".into(), json!(-90.1));
                v
            }),
            false,
        ));
        cases.push((
            "lat_too_high",
            Box::new(|mut v| {
                v.as_object_mut().unwrap().insert("lat".into(), json!(90.1));
                v
            }),
            false,
        ));
        cases.push((
            "lon_too_low",
            Box::new(|mut v| {
                v.as_object_mut().unwrap().insert("lon".into(), json!(-180.1));
                v
            }),
            false,
        ));
        cases.push((
            "lon_too_high",
            Box::new(|mut v| {
                v.as_object_mut().unwrap().insert("lon".into(), json!(180.1));
                v
            }),
            false,
        ));

        cases.push((
            "velocity_negative",
            Box::new(|mut v| {
                v.as_object_mut().unwrap().insert("velocity".into(), json!(-0.1));
                v
            }),
            false,
        ));

        cases.push((
            "true_track_too_low",
            Box::new(|mut v| {
                v.as_object_mut().unwrap().insert("true_track".into(), json!(-0.1));
                v
            }),
            false,
        ));
        cases.push((
            "true_track_too_high",
            Box::new(|mut v| {
                v.as_object_mut().unwrap().insert("true_track".into(), json!(360.1));
                v
            }),
            false,
        ));

        // Nulls should validate and be transmitted as explicit nulls (fixed field presence).
        cases.push((
            "lat_null_is_allowed_and_preserved",
            Box::new(|mut v| {
                v.as_object_mut().unwrap().insert("lat".into(), Value::Null);
                v
            }),
            true,
        ));

        // Extra fields are dropped by allowlist filtering before strict validation.
        cases.push((
            "extra_field_dropped",
            Box::new(|mut v| {
                v.as_object_mut().unwrap().insert("extra".into(), json!(true));
                v
            }),
            true,
        ));

        for (name, make, should_pass) in cases {
            let msg = make(base.clone());
            let res = validate_and_filter(&msg, &compiled, &allow);
            match (should_pass, res) {
                (true, Ok(filtered)) => {
                    if name == "lat_null_is_allowed_and_preserved" {
                        assert!(filtered.get("lat").is_some(), "lat key should be present");
                        assert!(filtered.get("lat").unwrap().is_null(), "lat should be null");
                    }
                }
                (true, Err(e)) => panic!("{name} expected pass, got error: {e}"),
                (false, Ok(v)) => panic!("{name} expected fail, got Ok: {v}"),
                (false, Err(_)) => {}
            }
        }
    }

    #[test]
    fn schema_extensions_missing_or_malformed_are_rejected() {
        // No properties with x-allowUpstream:true
        let s = json!({"type":"object","properties":{"ts":{"type":"string"}}});
        assert!(extract_allow_fields(&s).is_err());

        // Missing x-inputSchemaPointer
        let s = json!({"x-provider": {"url":"x","records_path":"/a","root_unix_ts_path":"/t","output_ts_field":"ts","fields":[] }});
        assert!(extract_input_schema_pointer(&s).is_err());

        // Malformed x-provider
        let s = json!({"x-inputSchemaPointer": "/$defs/x", "x-provider": {"url": 123}});
        assert!(extract_provider(&s).is_err());

        // Malformed x-provider.fields element
        let s = json!({
            "x-inputSchemaPointer": "/$defs/x",
            "x-provider": {
                "url": "https://example.test",
                "records_path": "/states",
                "root_unix_ts_path": "/time",
                "output_ts_field": "ts",
                "fields": [ {"name": "icao24"} ]
            }
        });
        assert!(extract_provider(&s).is_err());
    }

    #[test]
    fn transmitted_string_values_must_not_contain_whitespace() {
        let schema = json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "type": "object",
            "additionalProperties": true,
            "properties": {
                "icao24": { "type": "string", "x-allowUpstream": true },
                "ts": { "type": "string", "x-allowUpstream": true }
            },
            "required": ["icao24","ts"],
        });
        let compiled = compile_schema(schema.clone());
        let allow = extract_allow_fields(&schema).unwrap();

        let msg = json!({
            "icao24": "71c737 ",
            "ts": "2026-03-12T06:44:43+00:00"
        });

        let err = validate_and_filter(&msg, &compiled, &allow).unwrap_err();
        assert!(err.to_string().contains("contains whitespace"));
    }

    #[test]
    fn numeric_values_are_quantized_to_fixed_precision() {
        let schema = json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "lat": { "type": "number", "x-allowUpstream": true },
                "lon": { "type": "number", "x-allowUpstream": true }
            },
            "required": ["lat","lon"]
        });
        let compiled = compile_schema(schema.clone());
        let allow = extract_allow_fields(&schema).unwrap();

        let msg = json!({
            "lat": 35.2928123,
            "lon": 126.7196123
        });

        let filtered = validate_and_filter(&msg, &compiled, &allow).unwrap();
        let s = serde_json::to_string(&filtered).unwrap();

        // Expect quantized values at five-decimal precision grid (numbers may omit trailing zeros).
        assert_eq!(s, r#"{"lat":35.29281,"lon":126.71961}"#);
    }
}
