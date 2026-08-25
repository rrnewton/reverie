fn main() {
    let id = std::thread::current().id();
    println!("{}", id.as_u64());
}
