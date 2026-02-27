# Spec: Remove `v` (version) key from response envelope

## Decision

Remove the `v` key from all JSON response envelopes. The response contract remains `meta` + `result` only.

## Rationale

Versioning the response object when there is no other version at this stage is pointless. The v2 envelope is the only supported format; there is no v1 compatibility mode, no mixed output, and no consumer that needs to branch on `v`. Adding a version field "for the future" adds noise without current benefit. If a breaking change to the envelope shape is introduced later, we can introduce versioning then—either via a new `v` field or through other means (e.g. schema URL, content-type, or structural changes that are self-evident).

## Change

- **Before**: `{ "v": 2, "meta": {...}, "result": {...} }`
- **After**: `{ "meta": {...}, "result": {...} }`

## Affected

- `src/types.rs`: Remove `v` field from `ResultEnvelope`, remove `RESPONSE_ENVELOPE_VERSION`
- `src/errors.rs`: Remove `"v": 2` from fallback JSON in `print_result`, update tests
- `README.md`: Update response examples
- `specs/v2-integration.md`: Update response contract examples

## Verification

- `cargo test`
- `cargo clippy --all-targets --all-features -- -D warnings`
- `cargo build --release`

## Test execution

### Tests modified

- `src/errors.rs`: `success_envelope_uses_v2_shape` and `error_envelope_includes_v2_error_fields` — removed `assert_eq!(value["v"], json!(2))`; both tests now assert only `meta` and `result` shape.

### Commands run

```bash
cargo test
cargo clippy --all-targets --all-features -- -D warnings
cargo build --release
```

### Expected results

- **cargo test**: All tests pass. The envelope tests verify that `meta.requestId`, `result.ok`, `result.command`, and (for success) `result.data` are present; the `v` key is no longer asserted.
- **cargo clippy**: No warnings.
- **cargo build --release**: Clean release build.
