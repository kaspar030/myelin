/// Sketch of the desired end-user experience.
///
/// Eventually the trait + macro will generate all the plumbing.
/// For now we hand-write what the macro *would* produce, so we can
/// iterate on the API shape.

// ----- What the user would write: -----

// #[chanapi::service]
// pub trait Greeter {
//     async fn greet(&self, name: String) -> String;
//     async fn health(&self) -> bool;
// }

// ----- What the user implements: -----

struct GreeterImpl;

impl GreeterImpl {
    async fn greet(&self, name: String) -> String {
        format!("Hello, {name}!")
    }

    async fn health(&self) -> bool {
        true
    }
}

// ----- What the macro would generate (hand-written for now): -----

enum GreeterRequest {
    Greet { name: String },
    Health,
}

enum GreeterResponse {
    Greet(String),
    Health(bool),
}

// ----- Putting it together: -----

#[tokio::main]
async fn main() {
    // For now, just prove the workspace compiles and the crate links.
    let svc = GreeterImpl;
    let reply = svc.greet("world".into()).await;
    println!("{reply}");
    println!("health: {}", svc.health().await);
    println!("chanapi testing binary runs.");
}
