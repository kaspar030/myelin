//! Tokio test binary exercising the greeter service and composed service.
//!
//! Demonstrates a realistic pattern: the server is set up in one place,
//! client handles are obtained separately.

use testing_service::{
    CombinedClient, CombinedTokioService, GreeterClient, GreeterService, GreeterTokioService,
    MathService, combined_serve, greeter_serve,
};

// -- Service implementation --

struct GreeterImpl;

impl GreeterService for GreeterImpl {
    async fn greet(&self, name: String) -> String {
        format!("Hello, {name}!")
    }

    fn health(&self) -> bool {
        true
    }
}

// -- Combined impl: implements both traits --
struct CombinedImpl;

impl GreeterService for CombinedImpl {
    async fn greet(&self, name: String) -> String {
        format!("Hi from combined, {name}!")
    }

    fn health(&self) -> bool {
        true
    }
}

impl MathService for CombinedImpl {
    async fn add(&self, a: i32, b: i32) -> i64 {
        (a as i64) + (b as i64)
    }

    async fn multiply(&self, a: i32, b: i32) -> i64 {
        (a as i64) * (b as i64)
    }
}

#[tokio::main]
async fn main() {
    // =========================================================================
    // Test 1: standalone greeter service
    // =========================================================================
    println!("=== Standalone Greeter ===");

    let mut service = GreeterTokioService::new(8);
    let mut server = service.server();

    let server_handle = tokio::spawn(async move {
        let svc = GreeterImpl;
        if let Err(e) = greeter_serve(&svc, &mut server).await {
            eprintln!("server error: {e}");
        }
    });

    let client = GreeterClient::new(service.client());

    let greeting = client
        .greet("mama".to_string())
        .await
        .expect("greet failed");
    println!("{greeting}");

    let healthy = client.health().await.expect("health failed");
    println!("healthy: {healthy}");

    let client2 = GreeterClient::new(service.client());
    let greeting2 = client2
        .greet("papa".to_string())
        .await
        .expect("greet failed");
    println!("{greeting2}");

    drop(client);
    drop(client2);
    drop(service);
    let _ = server_handle.await;

    // =========================================================================
    // Test 2: composed service (Greeter + Math on one channel)
    // =========================================================================
    println!("\n=== Composed Service (Greeter + Math) ===");

    let mut combined_service = CombinedTokioService::new(8);
    let mut combined_server = combined_service.server();

    let combined_handle = tokio::spawn(async move {
        let svc = CombinedImpl;
        if let Err(e) = combined_serve(&svc, &mut combined_server).await {
            eprintln!("combined server error: {e}");
        }
    });

    let combined_client = CombinedClient::new(combined_service.client());

    // Use the greeter sub-service
    let greeting = combined_client
        .greeter()
        .greet("world".to_string())
        .await
        .expect("greet failed");
    println!("{greeting}");

    let healthy = combined_client
        .greeter()
        .health()
        .await
        .expect("health failed");
    println!("healthy: {healthy}");

    // Use the math sub-service
    let sum = combined_client.math().add(2, 3).await.expect("add failed");
    println!("2 + 3 = {sum}");

    let product = combined_client
        .math()
        .multiply(4, 5)
        .await
        .expect("multiply failed");
    println!("4 * 5 = {product}");

    // Verify edge cases
    let big_sum = combined_client
        .math()
        .add(i32::MAX, 1)
        .await
        .expect("add failed");
    println!("MAX + 1 = {big_sum}");

    drop(combined_client);
    drop(combined_service);
    let _ = combined_handle.await;

    println!("\ndone.");
}
