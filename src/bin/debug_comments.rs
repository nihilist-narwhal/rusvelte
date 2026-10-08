//! Print the comments the parser records for a component (debugging aid)
fn main() {
    let path = std::env::args().nth(1).unwrap();
    let source = std::fs::read_to_string(path).unwrap();
    let alloc = oxc_allocator::Allocator::default();
    let c = rusvelte::parse(&alloc, &source, false).unwrap();
    for x in &c.root.comments {
        println!("{} {} {:?}", x.start, x.end, x.value.chars().take(40).collect::<String>());
    }
}
