//! Test binary: services over stdio with postcard serialization.
//!
//! - `testing-stdio server` — runs the greeter server
//! - `testing-stdio combined-server` — runs the combined (Greeter + Math) server
//! - `testing-stdio` — spawns both server types and acts as a client for each

use testing_service::{
    GreeterRequest, GreeterResponse, GreeterServiceSync, greeter_serve_sync,
    MathServiceSync,
    CombinedRequest, CombinedResponse, combined_serve_sync,
};

// -- Service implementations --
struct GreeterImpl;

impl GreeterServiceSync for GreeterImpl {
    fn greet(&self, name: String) -> String {
        format!("Hello over stdio, {name}!")
    }

    fn health(&self) -> bool {
        true
    }
}

struct CombinedImpl;

impl GreeterServiceSync for CombinedImpl {
    fn greet(&self, name: String) -> String {
        format!("Combined stdio, {name}!")
    }

    fn health(&self) -> bool {
        true
    }
}

impl MathServiceSync for CombinedImpl {
    fn add(&self, a: i32, b: i32) -> i64 {
        (a as i64) + (b as i64)
    }

    fn multiply(&self, a: i32, b: i32) -> i64 {
        (a as i64) * (b as i64)
    }
}

/// Trivial BlockOn for a synchronous transport — the futures are always ready.
struct SyncBlockOn;

impl myelin::BlockOn for SyncBlockOn {
    fn block_on<F: core::future::Future>(&self, fut: F) -> F::Output {
        let mut fut = core::pin::pin!(fut);
        let waker = noop_waker();
        let mut cx = core::task::Context::from_waker(&waker);
        match fut.as_mut().poll(&mut cx) {
            core::task::Poll::Ready(val) => val,
            core::task::Poll::Pending => panic!("sync transport future was not ready"),
        }
    }
}

fn noop_waker() -> core::task::Waker {
    use core::task::{RawWaker, RawWakerVTable, Waker};
    const VTABLE: RawWakerVTable = RawWakerVTable::new(|_| RAW, |_| {}, |_| {}, |_| {});
    const RAW: RawWaker = RawWaker::new(core::ptr::null(), &VTABLE);
    unsafe { Waker::from_raw(RAW) }
}

fn run_greeter_server() {
    eprintln!("[server] starting greeter on stdio");
    let stdin = std::io::stdin().lock();
    let stdout = std::io::stdout().lock();
    let mut transport = myelin::transport_postcard::new_postcard_stream::<_, _, GreeterRequest, GreeterResponse>(stdin, stdout);
    let svc = GreeterImpl;
    let _ = greeter_serve_sync(&svc, &mut transport, &SyncBlockOn);
    eprintln!("[server] greeter done");
}

fn run_combined_server() {
    eprintln!("[server] starting combined on stdio");
    let stdin = std::io::stdin().lock();
    let stdout = std::io::stdout().lock();
    let mut transport = myelin::transport_postcard::new_postcard_stream::<_, _, CombinedRequest, CombinedResponse>(stdin, stdout);
    let svc = CombinedImpl;
    let _ = combined_serve_sync(&svc, &mut transport, &SyncBlockOn);
    eprintln!("[server] combined done");
}

fn run_greeter_client() {
    use std::process::{Command, Stdio};

    println!("=== Greeter over stdio ===");

    let exe = std::env::current_exe().expect("failed to get current exe");
    let mut child = Command::new(&exe)
        .arg("server")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("failed to spawn server");

    let child_stdin = child.stdin.take().unwrap();
    let child_stdout = child.stdout.take().unwrap();

    let transport =
        myelin::transport_postcard::new_postcard_stream::<_, _, GreeterResponse, GreeterRequest>(child_stdout, child_stdin);
    let client = testing_service::GreeterClientSync::new(
        testing_service::GreeterClient::new(transport),
        SyncBlockOn,
    );

    let greeting = client.greet("world".to_string()).expect("greet failed");
    println!("[client] {greeting}");

    let healthy = client.health().expect("health failed");
    println!("[client] healthy: {healthy}");

    drop(client);
    let status = child.wait().expect("failed to wait for server");
    println!("[client] server exited: {status}");
}

fn run_combined_client() {
    use std::process::{Command, Stdio};

    println!("\n=== Combined (Greeter + Math) over stdio ===");

    let exe = std::env::current_exe().expect("failed to get current exe");
    let mut child = Command::new(&exe)
        .arg("combined-server")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("failed to spawn combined server");

    let child_stdin = child.stdin.take().unwrap();
    let child_stdout = child.stdout.take().unwrap();

    let transport =
        myelin::transport_postcard::new_postcard_stream::<_, _, CombinedResponse, CombinedRequest>(child_stdout, child_stdin);
    let client = testing_service::CombinedClientSync::new(
        testing_service::CombinedClient::new(transport),
        SyncBlockOn,
    );

    // Test greeter sub-service
    let greeting = client.greeter().greet("stdio".to_string()).expect("greet failed");
    println!("[client] {greeting}");

    let healthy = client.greeter().health().expect("health failed");
    println!("[client] healthy: {healthy}");

    // Test math sub-service
    let sum = client.math().add(10, 20).expect("add failed");
    println!("[client] 10 + 20 = {sum}");

    let product = client.math().multiply(6, 7).expect("multiply failed");
    println!("[client] 6 * 7 = {product}");

    drop(client);
    let status = child.wait().expect("failed to wait for server");
    println!("[client] combined server exited: {status}");
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() > 1 {
        match args[1].as_str() {
            "server" => run_greeter_server(),
            "combined-server" => run_combined_server(),
            _ => {
                eprintln!("unknown command: {}", args[1]);
                std::process::exit(1);
            }
        }
    } else {
        run_greeter_client();
        run_combined_client();
    }
}
