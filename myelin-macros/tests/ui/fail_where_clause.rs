#[myelin::service]
pub trait FooService
where
    Self: Sized,
{
    fn bar(&self);
}

fn main() {}
