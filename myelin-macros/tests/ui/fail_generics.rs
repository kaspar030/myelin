#[myelin::service]
pub trait FooService<T> {
    fn bar(&self, x: T);
}

fn main() {}
