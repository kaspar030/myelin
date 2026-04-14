#![no_main]
#![no_std]
#![allow(async_fn_in_trait)]

use ariel_os::debug::{exit, log::info, ExitCode};
use chanapi::transport_embassy::EmbassyLocal;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use testing_service::{
    GreeterClient, GreeterRequest, GreeterResponse, GreeterService, greeter_serve,
};

// -- Static transport: 1 caller, channel depth 4 --
static TRANSPORT: EmbassyLocal<
    CriticalSectionRawMutex,
    GreeterRequest,
    GreeterResponse,
    1,
    1,
> = EmbassyLocal::new();

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

// -- Client task --
#[ariel_os::task(autostart)]
async fn client_task() {
    let client = GreeterClient::new(TRANSPORT.client());

    let greeting = client.greet("Ariel OS").await.expect("greet failed");
    info!("{}", greeting.as_str());

    let healthy = client.health().await.expect("health failed");
    info!("healthy: {}", healthy);

    info!("done.");
    exit(ExitCode::SUCCESS);
}
