//! Write the protocol's TypeScript types (and optionally its JSON Schema).
//!
//! ```text
//! cargo run -p sk-protocol --bin gen-ts                      # crates/sk-protocol/generated/protocol.ts
//! cargo run -p sk-protocol --bin gen-ts -- <out.ts>          # somewhere else
//! cargo run -p sk-protocol --bin gen-ts -- --check [out.ts]  # exit 1 if the file is stale (CI)
//! cargo run -p sk-protocol --bin gen-ts -- --schema <out.json>
//! ```

use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    let mut check = false;
    let mut schema_out: Option<PathBuf> = None;
    let mut out: Option<PathBuf> = None;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--check" => check = true,
            "--schema" => match args.next() {
                Some(p) => schema_out = Some(p.into()),
                None => {
                    eprintln!("--schema needs a path");
                    return ExitCode::from(2);
                }
            },
            "-h" | "--help" => {
                eprintln!("usage: gen-ts [--check] [OUT.ts] [--schema OUT.json]");
                return ExitCode::SUCCESS;
            }
            _ => out = Some(a.into()),
        }
    }
    let out = out.unwrap_or_else(|| {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("generated")
            .join("protocol.ts")
    });
    let text = sk_protocol::ts::generate();
    if check {
        let current = std::fs::read_to_string(&out).unwrap_or_default();
        if current != text {
            eprintln!(
                "{} is stale: run `cargo run -p sk-protocol --bin gen-ts`",
                out.display()
            );
            return ExitCode::FAILURE;
        }
        return ExitCode::SUCCESS;
    }
    if let Some(parent) = out.parent()
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        eprintln!("cannot create {}: {e}", parent.display());
        return ExitCode::FAILURE;
    }
    if let Err(e) = std::fs::write(&out, &text) {
        eprintln!("cannot write {}: {e}", out.display());
        return ExitCode::FAILURE;
    }
    eprintln!(
        "wrote {} ({} methods)",
        out.display(),
        sk_protocol::methods::METHODS.len()
    );
    if let Some(path) = schema_out {
        let doc = sk_protocol::schema::export_schema();
        let json = serde_json::to_string_pretty(&doc).expect("json") + "\n";
        if let Err(e) = std::fs::write(&path, json) {
            eprintln!("cannot write {}: {e}", path.display());
            return ExitCode::FAILURE;
        }
        eprintln!("wrote {}", path.display());
    }
    ExitCode::SUCCESS
}
