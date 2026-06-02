# llm-proxy

One OpenAI-compatible API in front of multiple LLM providers. Pool
keys, log usage, fail over.

## Status

Scaffolding. The workspace builds. The server does not serve anything
yet.

## Layout

~~~
apps/llm-proxy                 main binary
crates/llm-proxy-core          config, types, traits
crates/llm-proxy-protocol      OpenAI / Claude / Gemini wire types
crates/llm-proxy-provider      upstream channel implementations
crates/llm-proxy-storage       persistence
crates/llm-proxy-api           admin and user HTTP API
crates/llm-proxy-server        axum wiring
~~~

## Build

~~~
cargo build --release
~~~

## Run

~~~
./target/release/llm-proxy --config ./config.toml
~~~

Config schema is not final. It will change.

## References

These shaped the layout and scope:

- LeenHawk/gproxy
- modpotatodotdev/LLMG
- x5iu/openproxy
- habibi-dev/rust-llm-proxy
