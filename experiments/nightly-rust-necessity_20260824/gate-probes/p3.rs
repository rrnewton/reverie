#[thread_local]
static X: u32 = 1;
fn main() {
    println!("{}", X);
}
