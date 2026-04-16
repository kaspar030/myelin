#[chanapi::service]
pub trait FooService<'a> {
    fn bar(&self);
}

fn main() {}
