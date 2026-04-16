# chanapi — Design Notes

## Transport Layering

Stream transports (postcard over TCP, JSON over websocket, etc.) should decompose into three composable layers:

1. **Framing** — how bytes are chunked: `LengthPrefixed`, `COBS`, `WebSocketFramer`
2. **Encoding** — how types become bytes: `PostcardCodec`, `JsonCodec`, `ZerocopyCodec`
3. **Reply Routing** — how responses get back to callers: `Sequential`, `MuxedSlots<N>`, `MuxedOneshot`

A `StreamTransport<Framer, Codec, Router>` composes them and implements `ClientTransport`/`ServerTransport`.

Local transports (embassy, tokio) stay monolithic — they don't need framing or encoding.

## Service Composition

Multiple API traits on a single server, single channel:

```rust
chanapi::compose_service!(
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

## Cancel Safety

- Cancelling a client call is always safe (no corruption, no leaks)
- Server may do wasted work if client cancelled after send
- Server cancellation: clients get error (tokio/postcard) or hang (embassy)

See crate-level docs in `chanapi/src/lib.rs` for full details.
