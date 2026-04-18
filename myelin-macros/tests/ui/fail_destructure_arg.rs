#[myelin::service]
pub trait FooService {
    fn add(&self, (a, b): (i32, i32)) -> i64;
}

fn main() {}
