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
| CBOR stream | `cbor` | Cross-process, self-describing (nitro bus, nbusctl tail) |

## Unreleased

- `stream::codec`: add `CborCodec` alongside `PostcardCodec`, gated on
  the new `cbor` Cargo feature (off by default, pulls in
  `minicbor-serde`). Produces a self-describing wire format suitable
  for debuggable IPC buses (e.g. nitro's `nbus` / `nbusctl tail`).
  Postcard remains the default; the new codec is additive and slots
  into `StreamTransport` / `DuplexStreamTransport` unchanged through
  `CborCodecError`, which unifies `minicbor-serde`'s distinct
  encode/decode error types (the transport requires
  `Encoder::Error == Decoder::Error`).
- `stream::routing`: add `MuxedSlots::new_boxed() -> Box<Self>`, which
  constructs the `N × BUF` slot array directly in a heap allocation.
  The existing `MuxedSlots::new()` builds the slot array on the stack
  first, so it overflows typical runtime worker stacks (e.g. tokio's
  2 MiB default) for large `N * BUF` configurations. `new_boxed` keeps
  stack use bounded regardless of `BUF`.
- `stream::duplex::DuplexShared::slots` is now `Box<MuxedSlots<N, BUF>>`,
  and `DuplexStreamTransport::with_layers` uses `MuxedSlots::new_boxed()`.
  This lets `DuplexStreamTransport<_, _, _, _, 32, 131_072>` construct
  on a 1 MiB stack — see the new
  `duplex_construction_on_restricted_stack` regression test.
  Hot paths (`acquire`, `deliver`, `recv_reply`, `try_recv_slot`) are
  unchanged — access goes through `Box`'s auto-deref.
- `stream::transport`: add `with_boxed_router` constructor; consumes
  `Box<MuxedSlots<N, BUF>>` from `MuxedSlots::new_boxed()`, avoids
  stack-overflow for `BUF` ≥ 256 KiB.

## Provenance

This library was coded by Anthropic's Claude Opus using the [tau agent](https://github.com/tau-agent/tau).


## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.
