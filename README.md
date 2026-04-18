# myelin

Define async service APIs as traits, communicate over channels.

Myelin lets you define service interfaces as Rust traits and generates all the
channel plumbing — request/response enums, typed clients, server dispatch loops,
and transport adapters — via a proc macro.

## Features

- **Trait-driven** — define your API once, get client and server code generated
- **Transport-agnostic** — local channels (tokio, embassy, smol) or serialized
  streams (postcard over stdio/TCP/UART)
- **`no_std` compatible** — core library works without allocator; embassy
  transport uses static allocation for embedded targets
- **Cancel-safe** — dropping a client future mid-await never corrupts state
- **Sync + async** — generated clients and servers come in both async and
  blocking (`BlockOn`-based) variants for thread interop
- **Infallible transports** — local transports return values directly (no
  `Result` wrapping) via the `TransportResult` trait

## Quick Example

```rust
#[myelin::service]
pub trait GreeterService {
    async fn greet(&self, name: String) -> String;
    fn health(&self) -> bool;
}
```

This generates `GreeterClient`, `GreeterRequest`/`GreeterResponse` enums,
`greeter_serve()`, `GreeterServiceSync`, and transport-specific helpers.

## Transports

| Transport | Feature | Use case |
|-----------|---------|----------|
| Tokio (mpsc + oneshot) | `tokio` | In-process, tokio async |
| Embassy (static Channel + Signal) | `embassy` | Embedded, `no_std` |
| Smol (async-channel) | `smol` | In-process, smol async |
| Postcard stream | `postcard` | Cross-process, serialized (stdio/TCP/UART) |

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.
