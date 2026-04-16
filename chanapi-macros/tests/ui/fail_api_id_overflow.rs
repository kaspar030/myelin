#[chanapi::service(api_id = 70000)]
pub trait FooService {
    fn bar(&self);
}

fn main() {}
