#![no_main]
#![no_std]
#![allow(async_fn_in_trait)]

use ariel_os::debug::{exit, log::info, ExitCode};
use core::sync::atomic::{AtomicU8, Ordering};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use testing_service::{GreeterService, greeter_embassy_service, greeter_serve};

// -- Two greeter service instances --
greeter_embassy_service!(casual, CriticalSectionRawMutex, 2);
greeter_embassy_service!(formal, CriticalSectionRawMutex, 2);

/// Track how many clients have finished.
static CLIENTS_DONE: AtomicU8 = AtomicU8::new(0);
const NUM_CLIENTS: u8 = 2;

// -- Casual implementation --
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

// -- Formal implementation --
struct FormalGreeter;

impl GreeterService for FormalGreeter {
    async fn greet(&self, name: &str) -> heapless::String<64> {
        let mut s = heapless::String::new();
        let _ = s.push_str("Good day, ");
        let _ = s.push_str(name);
        let _ = s.push_str(". How do you do?");
        s
    }

    async fn health(&self) -> bool {
        true
    }
}

// -- Server tasks --
#[ariel_os::task(autostart)]
async fn casual_server_task() {
    let mut server = casual_server!();
    info!("casual server started");
    let _ = greeter_serve(&CasualGreeter, &mut server).await;
}

#[ariel_os::task(autostart)]
async fn formal_server_task() {
    let mut server = formal_server!();
    info!("formal server started");
    let _ = greeter_serve(&FormalGreeter, &mut server).await;
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
