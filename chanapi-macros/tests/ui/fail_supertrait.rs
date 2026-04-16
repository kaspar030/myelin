#[chanapi::service]
pub trait FooService: Send {
    fn bar(&self);
}

fn main() {}
