#[myelin::service]
pub trait FooService {
    fn bar(&self, name: &str);
}

fn main() {}
