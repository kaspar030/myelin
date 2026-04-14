#![no_main]
#![no_std]
#![allow(async_fn_in_trait)]

use ariel_os::debug::{exit, log::info, ExitCode};
use chanapi::transport_embassy::EmbassyLocal;
use core::sync::atomic::{AtomicU8, Ordering};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use testing_service::{
    GreeterClient, GreeterRequest, GreeterResponse, GreeterService, greeter_serve,
};

// -- Static transport: 2 callers, channel depth 2 --
static TRANSPORT: EmbassyLocal<
    CriticalSectionRawMutex,
    GreeterRequest,
    GreeterResponse,
    2,
    2,
> = EmbassyLocal::new();

/// Track how many clients have finished.
static CLIENTS_DONE: AtomicU8 = AtomicU8::new(0);
const NUM_CLIENTS: u8 = 2;

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

// -- Server task --
#[ariel_os::task(autostart)]
async fn server_task() {
    let svc = GreeterImpl;
    let mut server = TRANSPORT.server();
    info!("server task started");
    // This runs forever (or until something breaks).
    if let Err(_e) = greeter_serve(&svc, &mut server).await {
        info!("server error");
    }
}

// -- Client task 1 --
#[ariel_os::task(autostart)]
async fn client_task_1() {
    let client = GreeterClient::new(TRANSPORT.client());

    let greeting = client.greet("client 1").await.expect("greet failed");
    info!("[client 1] {}", greeting.as_str());

    let healthy = client.health().await.expect("health failed");
    info!("[client 1] healthy: {}", healthy);

    info!("[client 1] done.");
    if CLIENTS_DONE.fetch_add(1, Ordering::AcqRel) + 1 == NUM_CLIENTS {
        exit(ExitCode::SUCCESS);
    }
}

// -- Client task 2 --
#[ariel_os::task(autostart)]
async fn client_task_2() {
    let client = GreeterClient::new(TRANSPORT.client());

    let greeting = client.greet("client 2").await.expect("greet failed");
    info!("[client 2] {}", greeting.as_str());

    let healthy = client.health().await.expect("health failed");
    info!("[client 2] healthy: {}", healthy);

    info!("[client 2] done.");
    if CLIENTS_DONE.fetch_add(1, Ordering::AcqRel) + 1 == NUM_CLIENTS {
        exit(ExitCode::SUCCESS);
    }
}
