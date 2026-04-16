#[chanapi::service]
pub trait FooService {
    async fn bar(&mut self);
}

fn main() {}
