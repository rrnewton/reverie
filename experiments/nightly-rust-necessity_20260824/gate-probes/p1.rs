use std::sync::Arc;
fn main() {
    let mut a = Arc::new(5u32);
    unsafe {
        *Arc::get_mut_unchecked(&mut a) = 6;
    }
    println!("{}", a);
}
