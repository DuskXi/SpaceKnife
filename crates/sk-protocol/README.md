# sk-protocol

SpaceKnife WebSocket protocol v1 in Rust: every message type, the JSON-RPC envelope, the binary
frame codec, the filter language, and the TypeScript types for the frontend. Wire format:
[docs/protocol.md](../../docs/protocol.md) (unchanged from the Python backend).

## TypeScript types

`generated/protocol.ts` is generated from the Rust types (same exported names and shapes as the old
`apps/electron/src/renderer/src/protocol/generated.ts`):

```bash
cargo run -p sk-protocol --bin gen-ts                       # writes generated/protocol.ts
cargo run -p sk-protocol --bin gen-ts -- path/to/out.ts     # somewhere else
cargo run -p sk-protocol --bin gen-ts -- --check            # exit 1 if stale
cargo run -p sk-protocol --bin gen-ts -- --schema generated/protocol.schema.json
```

`cargo test -p sk-protocol` fails while `generated/protocol.ts` is stale, so regenerate after
changing a type.

## Conventions

- Params (client → server) types: `#[serde(deny_unknown_fields)]`, constraints in `Validate`.
  Parse with `sk_protocol::parse_params::<P>(request.params.as_deref())`; a `ParamsError` maps to
  `ErrorCode::InvalidParams` with `error.data = err.data()`.
- Payload (server → client) types ignore unknown fields and serialise every field (`None` → `null`).
- Literal unions are `str_enum!` enums (`as_str()`, `ALL`); tagged unions (`FilterCond`,
  `FrameGeometry`) are untagged enums of structs carrying a constant `kind`.
- Timestamps are `chrono::DateTime<Utc>` (RFC 3339, `Z`).

## Case folding

`filter_syntax::casefold` (class names, aliases and spellings, export names) is Python 3.12's
`str.casefold`, the one 0.3.x used: `src/casefold_table.rs` and the test data
`tests/data/casefold_python312.txt` are generated from it, and `tests/casefold.rs` checks every code
point against that data.

```bash
python3.12 -I tools/gen_casefold.py            # rewrite both
python3.12 -I tools/gen_casefold.py --check    # exit 1 if stale
```
