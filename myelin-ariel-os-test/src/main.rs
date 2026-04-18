#![no_main]
#![no_std]
#![allow(async_fn_in_trait)]

//! FIXME: this crate was excluded from the workspace to track a pre-existing
//! `myelin/embassy` E0119 (see task 476). With the `#[myelin::service]` v1
//! adoption (task 482) the trait now uses `String`, which requires `alloc`.
//! Ariel-OS v0.4 does not ship a global allocator out of the box, so wiring
//! one up (e.g. `embedded-alloc`) is deferred. Until that lands, this crate
//! will not compile — same "pre-existing embassy issue" level as before the
//! macro adoption.

extern crate alloc;

use alloc::format;
use alloc::string::{String, ToString};

use ariel_os::debug::{ExitCode, exit, log::info};
use core::sync::atomic::{AtomicU8, Ordering};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use testing_service::{
    GreeterService, GreeterServiceSync, greeter_embassy_service, greeter_serve,
    greeter_serve_sync,
};

// -- Two greeter service instances --
greeter_embassy_service!(casual, CriticalSectionRawMutex, 4);
greeter_embassy_service!(formal, CriticalSectionRawMutex, 3);

/// Track how many clients have finished.
static CLIENTS_DONE: AtomicU8 = AtomicU8::new(0);
const NUM_CLIENTS: u8 = 3;

/// Ariel OS thread `BlockOn` adapter.
struct ThreadBlockOn;
impl myelin::BlockOn for ThreadBlockOn {
    fn block_on<F: core::future::Future>(&self, fut: F) -> F::Output {
        ariel_os::thread::block_on(fut)
    }
}

// -- Casual implementation: parameterized with a server ID --
struct CasualGreeter {
    id: u8,
}

impl GreeterService for CasualGreeter {
    async fn greet(&self, name: String) -> String {
        format!("Yo, {name}! (server {})", self.id)
    }

    fn health(&self) -> bool {
        true
    }
}

// -- Formal implementation (sync, runs in a thread) --
struct FormalGreeter;

impl GreeterServiceSync for FormalGreeter {
    fn greet(&self, name: String) -> String {
        format!("Good day, {name}. How do you do?")
    }

    fn health(&self) -> bool {
        true
    }
}

// -- Casual server 1: async task (MPMC — shares the same channel) --
#[ariel_os::task(autostart)]
async fn casual_server_1() {
    let mut server = casual_server!();
    info!("casual server 1 started");
    let _ = greeter_serve(&CasualGreeter { id: 1 }, &mut server).await;
}

// -- Casual server 2: async task (MPMC — shares the same channel) --
#[ariel_os::task(autostart)]
async fn casual_server_2() {
    let mut server = casual_server!();
    info!("casual server 2 started");
    let _ = greeter_serve(&CasualGreeter { id: 2 }, &mut server).await;
}

// -- Formal server: sync thread --
#[ariel_os::thread(autostart)]
fn formal_server_thread() {
    let mut server = formal_server!();
    info!("formal server started (sync thread)");
    let _ = greeter_serve_sync(&FormalGreeter, &mut server, &ThreadBlockOn);
}

// -- Client task 1: uses both services --
#[ariel_os::task(autostart)]
async fn client_task_1() {
    let casual = casual_client!();
    let formal = formal_client!();

    for i in 0..3u8 {
        let name = format!("c1-{i}");
        let g = casual.greet(name).await;
        info!("[client 1] {}", g.as_str());
    }

    let g = formal.greet("client 1".to_string()).await;
    info!("[client 1] formal: {}", g.as_str());

    info!("[client 1] done.");
    if CLIENTS_DONE.fetch_add(1, Ordering::AcqRel) + 1 == NUM_CLIENTS {
        exit(ExitCode::SUCCESS);
    }
}

// -- Client task 2: uses both services --
#[ariel_os::task(autostart)]
async fn client_task_2() {
    let casual = casual_client!();
    let formal = formal_client!();

    for i in 0..3u8 {
        let name = format!("c2-{i}");
        let g = casual.greet(name).await;
        info!("[client 2] {}", g.as_str());
    }

    let g = formal.greet("client 2".to_string()).await;
    info!("[client 2] formal: {}", g.as_str());

    info!("[client 2] done.");
    if CLIENTS_DONE.fetch_add(1, Ordering::AcqRel) + 1 == NUM_CLIENTS {
        exit(ExitCode::SUCCESS);
    }
}

// -- Client thread: sync, uses casual service --
#[ariel_os::thread(autostart)]
fn client_thread() {
    let client = casual_client_sync!(ThreadBlockOn);

    for i in 0..3u8 {
        let name = format!("thr-{i}");
        let g = client.greet(name);
        info!("[thread] {}", g.as_str());
    }

    info!("[thread] done.");
    if CLIENTS_DONE.fetch_add(1, Ordering::AcqRel) + 1 == NUM_CLIENTS {
        exit(ExitCode::SUCCESS);
    }
}
