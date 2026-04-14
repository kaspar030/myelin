//! Tokio test binary exercising the greeter service over tokio channels.

use chanapi::transport_tokio;
use testing_service::{
    GreeterClient, GreeterRequest, GreeterResponse, GreeterService, greeter_serve,
};

// -- Service implementation --

struct GreeterImpl;

impl GreeterService for GreeterImpl {
    async fn greet(&self, name: &str) -> heapless::String<64> {
        let mut s = heapless::String::new();
        let _ = s.push_str("Hello, ");
        let _ = s.push_str(name);
        let _ = s.push_str("!");
        s
    }

    async fn health(&self) -> bool {
        true
    }
}

#[tokio::main]
async fn main() {
    let (client_transport, mut server_transport) =
        transport_tokio::create::<GreeterRequest, GreeterResponse>(8);

    let client = GreeterClient::new(client_transport);

    // Spawn the service task.
    let svc = GreeterImpl;
    let server_handle = tokio::spawn(async move {
        if let Err(e) = greeter_serve(&svc, &mut server_transport).await {
            eprintln!("server error: {e}");
        }
    });

    // Make some calls.
    let greeting = client.greet("mama").await.expect("greet failed");
    println!("{greeting}");

    let healthy = client.health().await.expect("health failed");
    println!("healthy: {healthy}");

    // Drop client to shut down server.
    drop(client);
    let _ = server_handle.await;

    println!("done.");
}
