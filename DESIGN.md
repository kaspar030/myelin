# myelin — Design Notes

## Transport Layering

Stream transports (postcard over TCP, JSON over websocket, etc.) should decompose into three composable layers:

1. **Framing** — how bytes are chunked: `LengthPrefixed`, `COBS`, `WebSocketFramer`
2. **Encoding** — how types become bytes: `PostcardCodec`, `CborCodec`, `JsonCodec`, `ZerocopyCodec`
3. **Reply Routing** — how responses get back to callers: `Sequential`, `MuxedSlots<N>`, `MuxedOneshot`

A `StreamTransport<Framer, Codec, Router>` composes them and implements `ClientTransport`/`ServerTransport`.

Local transports (embassy, tokio) stay monolithic — they don't need framing or encoding.

## Service Composition

Multiple API traits on a single server, single channel:

```rust
myelin::compose_service!(
    MyService,
    GreeterService,
    HealthService,
    SomeOtherApi,
);
```

Generates:
- Combined `MyServiceRequest` / `MyServiceResponse` enums wrapping all APIs
- Combined dispatch function delegating to the right trait impl
- Combined client exposing all methods

Transport layer unchanged — just sees `(ComposedReq, ComposedResp)`.

## Wire-Level API Multiplexing (Stream Transports)

For multiple APIs over one byte stream:

```
[u16 api_id][u8 request_slot][u32 payload_len][payload]
```

Server has a registry of `api_id → handler`. Each API's proc macro generates its `api_id`.

Separate concern from local transport composition above.

## Concurrent Stream Requests

Slot index as request ID + fixed-size array of reply slots (no slotmap crate needed):

```
[u8 slot_id][u32 payload_len][payload]
```

Atomic bitmap for slot allocation, same pattern as original embassy transport. Max concurrent requests is a const generic.

## Async I/O

The stream stack is genuinely async. myelin owns two minimal traits,
`io::AsyncBytesRead` and `io::AsyncBytesWrite`, with just `read_exact`,
`write_all`, and `flush`. Core myelin depends on neither `tokio` nor
`futures` — runtime adapters live behind feature flags:

- `BlockingIo` wraps `std::io::Read`/`Write` (no-`await` inline ops).
- `io::futures_io` (feature `futures-io`) adapts `futures_io::AsyncRead`/
  `AsyncWrite` — covers smol, async-std, etc.
- `io::tokio_io` (feature `tokio-io`) adapts tokio's AsyncRead/Write.

Shared access to a reader/writer between concurrent async tasks inside
a single transport goes through `io::LocalLock` — a zero-dep
single-waiter async mutex (AtomicWaker + AtomicBool). Replaces the old
`RefCell`-based scheme which was unsound across `.await`.

## Duplex Stream Transport

`DuplexStreamTransport` lets one peer both call *and* serve over the
same byte stream. A pump future (spawned by the user's runtime) owns
the reader and demultiplexes incoming frames. Registered server halves
receive their requests via per-`api_id` inboxes; outgoing calls are
matched to responses via a shared `MuxedSlots` router.

Wire format (inside each length-prefixed frame):

```
[u8 kind][u16 api_id LE][u8 slot_id][codec bytes]
```

- `kind = 0` → request (from sender to peer's registered server).
- `kind = 1` → response (echoes back the caller's `slot_id`).

This is a **new wire format**, distinct from and incompatible with the
single-direction `StreamTransport + MuxedSlots` format (which uses
just `[u8 slot_id][payload]`). The one-way stream transport stays as
is; duplex is chosen explicitly at the call site.

## Cancel Safety

- Cancelling a client call is always safe (no corruption, no leaks)
- Server may do wasted work if client cancelled after send
- Server cancellation: clients get error (tokio/postcard) or hang (embassy)

See crate-level docs in `myelin/src/lib.rs` for full details.
