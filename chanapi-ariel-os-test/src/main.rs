#![no_main]
#![no_std]
#![allow(async_fn_in_trait)]

use ariel_os::debug::{exit, log::info, ExitCode};
use chanapi::transport_embassy::EmbassyService;
use core::sync::atomic::{AtomicU8, Ordering};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use static_cell::StaticCell;
use testing_service::{
    GreeterClient, GreeterRequest, GreeterResponse, GreeterService, greeter_serve,
};

// -- The service channel --
static SERVICE: EmbassyService<CriticalSectionRawMutex, GreeterRequest, GreeterResponse, 2> =
    EmbassyService::new();

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
    let mut server = SERVICE.server();
    info!("server task started");
    if let Err(_e) = greeter_serve(&svc, &mut server).await {
        info!("server error");
    }
}

// -- Client task 1 --
#[ariel_os::task(autostart)]
async fn client_task_1() {
    static CLIENT_CELL: StaticCell<
        chanapi::transport_embassy::EmbassyClient<
            CriticalSectionRawMutex,
            GreeterRequest,
            GreeterResponse,
            2,
        >,
    > = StaticCell::new();
    let transport: &'static _ = CLIENT_CELL.init(SERVICE.client());
    let client = GreeterClient::new(transport);

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
    static CLIENT_CELL: StaticCell<
        chanapi::transport_embassy::EmbassyClient<
            CriticalSectionRawMutex,
            GreeterRequest,
            GreeterResponse,
            2,
        >,
    > = StaticCell::new();
    let transport: &'static _ = CLIENT_CELL.init(SERVICE.client());
    let client = GreeterClient::new(transport);

    let greeting = client.greet("client 2").await.expect("greet failed");
    info!("[client 2] {}", greeting.as_str());

    let healthy = client.health().await.expect("health failed");
    info!("[client 2] healthy: {}", healthy);

    info!("[client 2] done.");
    if CLIENTS_DONE.fetch_add(1, Ordering::AcqRel) + 1 == NUM_CLIENTS {
        exit(ExitCode::SUCCESS);
    }
}
