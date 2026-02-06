# CrabBot 🦀

**CrabBot** is a **Rust-native, local-first agent runtime** inspired by OpenClaw’s architecture. It reimplements similar ideas with a focus on **efficiency, small footprint, and explicit control**, avoiding heavy JavaScript stacks and hidden agent state.

> ⚠️ **Status: Work in Progress (WIP)**
> CrabBot is under active development. APIs, on-disk formats, and behavior are expected to change.

## What CrabBot Is

CrabBot runs a local **runtime** that:

* Manages **sessions**, **runs**, and **queues**
* Persists all state locally as transparent files (JSON / JSONL)
* Builds prompts deterministically from history + system rules
* Executes **explicit, auditable tools** (no raw shell access)
* Works with **self-hosted LLMs** and OpenAI-compatible APIs

There is **no hidden memory** and no background agent magic. If the model “remembers” something, you can find it on disk.

## Inspiration

CrabBot is **inspired by OpenClaw’s local agent model**, particularly its gateway-centric design and file-backed session memory.

CrabBot is **not a fork**, **not compatible**, and **not affiliated** with OpenClaw.
The goal is a clean reimplementation in Rust with different trade-offs:

* smaller runtime
* simpler install
* simpler self hosted setups


## Planned Components

* Rust runtime (async, single-binary)
* File-backed session + transcript store
* Tool registry and execution engine compatible ith clawhub
* Local CLI (TUI-first)
* web / native UI via WASM
* Optional webhook ingress via rm87
* m87 based remote device access

## License

Apache License 2.0.

