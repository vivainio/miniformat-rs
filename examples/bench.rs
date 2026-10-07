//! cargo run --release --example bench -- FILE
//! Times the event reader (with and without duplicate-key checks) and the tree loaders, in-process.
use std::time::Instant;

fn best(mut f: impl FnMut()) -> f64 {
    (0..5)
        .map(|_| {
            let t = Instant::now();
            f();
            t.elapsed().as_secs_f64()
        })
        .fold(f64::MAX, f64::min)
}

fn main() {
    let path = std::env::args().nth(1).expect("FILE");
    let text = std::fs::read_to_string(&path).unwrap();
    let mb = text.len() as f64 / 1e6;
    let events = |strict: bool| {
        best(|| {
            for ev in miniformat::Reader::new(&text, None).unwrap().strict_keys(strict) {
                std::hint::black_box(ev.unwrap());
            }
        })
    };
    let loads = best(|| drop(std::hint::black_box(miniformat::loads(&text, None).unwrap())));
    let borrowed = best(|| drop(std::hint::black_box(miniformat::loads_borrowed(&text, None).unwrap())));
    let rows = [
        ("events, no key check", events(false)),
        ("events, strict keys", events(true)),
        ("loads_borrowed (tree)", borrowed),
        ("loads (owned tree)", loads),
    ];
    for (name, secs) in rows {
        println!("{name:22} {:7.1} ms  {:6.0} MB/s", secs * 1e3, mb / secs);
    }
}
