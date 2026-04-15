//! Test binary: greeter service over stdio with postcard serialization.
//!
//! - `testing-stdio server` — runs the greeter server, reads requests from stdin,
//!   writes responses to stdout (postcard + length-prefix framing).
//! - `testing-stdio` — spawns itself as a server subprocess and acts as a client.

use chanapi::transport_postcard::PostcardStream;
use testing_service::{
    GreeterRequest, GreeterResponse, GreeterServiceSync, greeter_serve_sync,
};

// -- Service implementation --
struct GreeterImpl;

impl GreeterServiceSync for GreeterImpl {
    fn greet(&self, name: &str) -> heapless::String<64> {
        let mut s = heapless::String::new();
        let _ = s.push_str("Hello over stdio, ");
        let _ = s.push_str(name);
        let _ = s.push_str("!");
        s
    }

    fn health(&self) -> bool {
        true
    }
}

/// Trivial BlockOn for a synchronous transport — the futures are always ready.
struct SyncBlockOn;

impl chanapi::BlockOn for SyncBlockOn {
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

fn run_server() {
    eprintln!("[server] starting on stdio");
    let stdin = std::io::stdin().lock();
    let stdout = std::io::stdout().lock();
    let mut transport = PostcardStream::<_, _, GreeterRequest, GreeterResponse>::new(stdin, stdout);
    let svc = GreeterImpl;
    let _ = greeter_serve_sync(&svc, &mut transport, &SyncBlockOn);
    eprintln!("[server] done");
}

fn run_client() {
    use std::process::{Command, Stdio};

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

    // Client: sends GreeterRequest (outgoing), receives GreeterResponse (incoming)
    let transport =
        PostcardStream::<_, _, GreeterResponse, GreeterRequest>::new(child_stdout, child_stdin);
    let client = testing_service::GreeterClientSync::new(
        testing_service::GreeterClient::new(transport),
        SyncBlockOn,
    );

    let greeting = client.greet("world").expect("greet failed");
    println!("[client] {greeting}");

    let healthy = client.health().expect("health failed");
    println!("[client] healthy: {healthy}");

    drop(client);
    let status = child.wait().expect("failed to wait for server");
    println!("[client] server exited: {status}");
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() > 1 && args[1] == "server" {
        run_server();
    } else {
        run_client();
    }
}
