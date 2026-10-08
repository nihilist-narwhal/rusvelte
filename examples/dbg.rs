fn main() {
    let src = std::env::args().nth(1).unwrap();
    let key = std::env::args().nth(2).unwrap_or("fragment".into());
    let src = if std::path::Path::new(&src).exists() { std::fs::read_to_string(&src).unwrap() } else { src };
    match rusvelte::parse_modern(&src, false) {
        Ok(v) => println!("{}", serde_json::to_string(&v[&key]).unwrap()),
        Err(e) => println!("ERR {:?}", e),
    }
}
