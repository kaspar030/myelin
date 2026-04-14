//! Hand-written example of what the proc macro would generate.
//!
//! This exercises the transport traits with real channels to iterate
//! on the API before writing the macro.

use chanapi::transport::{ClientTransport, ServerTransport};
use chanapi::transport_tokio;
use chanapi::CallError;

// ===== What the user writes =====

// #[chanapi::service]
// pub trait GreeterService {
//     async fn greet(&self, name: String) -> String;
//     async fn health(&self) -> bool;
// }

struct GreeterServiceImpl;

impl GreeterServiceImpl {
    async fn greet(&self, name: String) -> String {
        format!("Hello, {name}!")
    }

    async fn health(&self) -> bool {
        true
    }
}

// ===== What the macro generates =====

// -- Request / Response enums --

enum GreeterRequest {
    Greet { name: String },
    Health,
}

enum GreeterResponse {
    Greet(String),
    Health(bool),
}

// -- Client struct --

struct GreeterClient<T> {
    transport: T,
}

impl<T> GreeterClient<T>
where
    T: ClientTransport<GreeterRequest, GreeterResponse>,
{
    fn new(transport: T) -> Self {
        Self { transport }
    }

    async fn greet(&self, name: String) -> Result<String, CallError<T::Error>> {
        let resp = self
            .transport
            .call(GreeterRequest::Greet { name })
            .await
            .map_err(CallError::Transport)?;

        match resp {
            GreeterResponse::Greet(s) => Ok(s),
            _ => unreachable!("protocol violation: unexpected response variant"),
        }
    }

    async fn health(&self) -> Result<bool, CallError<T::Error>> {
        let resp = self
            .transport
            .call(GreeterRequest::Health)
            .await
            .map_err(CallError::Transport)?;

        match resp {
            GreeterResponse::Health(b) => Ok(b),
            _ => unreachable!("protocol violation: unexpected response variant"),
        }
    }
}

// -- Server dispatch --

async fn greeter_serve<T>(
    svc: &GreeterServiceImpl,
    transport: &mut T,
) -> Result<(), T::Error>
where
    T: ServerTransport<GreeterRequest, GreeterResponse>,
{
    loop {
        let (req, token) = transport.recv().await?;
        let resp = match req {
            GreeterRequest::Greet { name } => GreeterResponse::Greet(svc.greet(name).await),
            GreeterRequest::Health => GreeterResponse::Health(svc.health().await),
        };
        // If the client dropped, we just ignore the error.
        let _ = transport.reply(token, resp).await;
    }
}

// ===== Main =====

#[tokio::main]
async fn main() {
    let (client_transport, mut server_transport) =
        transport_tokio::create::<GreeterRequest, GreeterResponse>(8);

    let client = GreeterClient::new(client_transport);

    // Spawn the service task.
    let svc = GreeterServiceImpl;
    let server_handle = tokio::spawn(async move {
        if let Err(e) = greeter_serve(&svc, &mut server_transport).await {
            eprintln!("server error: {e}");
        }
    });

    // Make some calls.
    let greeting = client.greet("mama".into()).await.expect("greet failed");
    println!("{greeting}");

    let healthy = client.health().await.expect("health failed");
    println!("healthy: {healthy}");

    // Drop the client to shut down the server.
    drop(client);
    let _ = server_handle.await;

    println!("done.");
}
