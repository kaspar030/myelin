//! Tokio test binary exercising the greeter service.
//!
//! Demonstrates a realistic pattern: the server is set up in one place,
//! client handles are obtained separately.

use testing_service::{
    GreeterClient, GreeterService, GreeterTokioService, greeter_serve,
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
    // -- Server setup (e.g., in a server module) --
    let mut service = GreeterTokioService::new(8);
    let mut server = service.server();

    let server_handle = tokio::spawn(async move {
        let svc = GreeterImpl;
        if let Err(e) = greeter_serve(&svc, &mut server).await {
            eprintln!("server error: {e}");
        }
    });

    // -- Client usage (e.g., in a different module/task) --
    let client = GreeterClient::new(service.client());

    let greeting = client.greet("mama").await.expect("greet failed");
    println!("{greeting}");

    let healthy = client.health().await.expect("health failed");
    println!("healthy: {healthy}");

    // A second client from the same service.
    let client2 = GreeterClient::new(service.client());
    let greeting2 = client2.greet("papa").await.expect("greet failed");
    println!("{greeting2}");

    // Drop clients to shut down server.
    drop(client);
    drop(client2);
    drop(service);
    let _ = server_handle.await;

    println!("done.");
}
