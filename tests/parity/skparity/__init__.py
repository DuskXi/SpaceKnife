"""SpaceKnife protocol parity harness (tests/parity/README.md).

Drives two backends over the WebSocket protocol with the same language-neutral scenarios,
normalizes what they answer and compares the transcripts, or records a reference transcript as a
golden file for the Rust replayer (crates/sk-server/tests/parity_replay.rs).
"""
