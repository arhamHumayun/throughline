#![allow(clippy::print_stdout, clippy::unwrap_used)]
fn main() {
    for p in std::env::args().skip(1) {
        let src = std::fs::read_to_string(&p).unwrap();
        let f = diffview_core::ast::parse_file(&p, &src).unwrap();
        println!("== {p}");
        for d in &f.definitions { println!("  def {} {:?}", d.name, d.kind); }
        for e in &f.exports { println!("  export {:?}", e); }
    }
}
