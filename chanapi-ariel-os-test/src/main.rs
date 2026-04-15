#![no_main]
#![no_std]
#![allow(async_fn_in_trait)]

use ariel_os::debug::{ExitCode, exit, log::info};
use core::sync::atomic::{AtomicU8, Ordering};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use testing_service::{
    GreeterService, GreeterServiceSync, greeter_embassy_service, greeter_serve,
    greeter_serve_sync,
};

// -- Two greeter service instances --
greeter_embassy_service!(casual, CriticalSectionRawMutex, 3);
greeter_embassy_service!(formal, CriticalSectionRawMutex, 3);

/// Track how many clients have finished.
static CLIENTS_DONE: AtomicU8 = AtomicU8::new(0);
const NUM_CLIENTS: u8 = 3;

/// Ariel OS thread `BlockOn` adapter.
struct ThreadBlockOn;
impl chanapi::BlockOn for ThreadBlockOn {
    fn block_on<F: core::future::Future>(&self, fut: F) -> F::Output {
        ariel_os::thread::block_on(fut)
    }
}

// -- Casual implementation (async, runs in a task) --
struct CasualGreeter;

impl GreeterService for CasualGreeter {
    async fn greet(&self, name: &str) -> heapless::String<64> {
        let mut s = heapless::String::new();
        let _ = s.push_str("Yo, ");
        let _ = s.push_str(name);
        let _ = s.push_str("!");
        s
    }

    async fn health(&self) -> bool {
        true
    }
}

// -- Formal implementation (sync, runs in a thread) --
struct FormalGreeter;

impl GreeterServiceSync for FormalGreeter {
    fn greet(&self, name: &str) -> heapless::String<64> {
        let mut s = heapless::String::new();
        let _ = s.push_str("Good day, ");
        let _ = s.push_str(name);
        let _ = s.push_str(". How do you do?");
        s
    }

    fn health(&self) -> bool {
        true
    }
}

// -- Casual server: async task --
#[ariel_os::task(autostart)]
async fn casual_server_task() {
    let mut server = casual_server!();
    info!("casual server started (async task)");
    let _ = greeter_serve(&CasualGreeter, &mut server).await;
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

    let g1 = casual.greet("client 1").await;
    info!("[client 1] casual: {}", g1.as_str());

    let g2 = formal.greet("client 1").await;
    info!("[client 1] formal: {}", g2.as_str());

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

    let g1 = casual.greet("client 2").await;
    info!("[client 2] casual: {}", g1.as_str());

    let g2 = formal.greet("client 2").await;
    info!("[client 2] formal: {}", g2.as_str());

    info!("[client 2] done.");
    if CLIENTS_DONE.fetch_add(1, Ordering::AcqRel) + 1 == NUM_CLIENTS {
        exit(ExitCode::SUCCESS);
    }
}

// -- Client thread: sync, uses casual service --
#[ariel_os::thread(autostart)]
fn client_thread() {
    let client = casual_client_sync!(ThreadBlockOn);

    let g = client.greet("thread");
    info!("[thread] casual: {}", g.as_str());

    let h = client.health();
    info!("[thread] healthy: {}", h);

    info!("[thread] done.");
    if CLIENTS_DONE.fetch_add(1, Ordering::AcqRel) + 1 == NUM_CLIENTS {
        exit(ExitCode::SUCCESS);
    }
}
