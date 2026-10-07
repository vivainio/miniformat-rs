//! cargo run --release --example mem -- FILE MODE   (MODE: events | events-lax | borrowed | owned); prints peak RSS
fn main() {
    let mut a = std::env::args().skip(1);
    let text = std::fs::read_to_string(a.next().unwrap()).unwrap();
    let mode = a.next().unwrap();
    let mut n = 0usize;
    match mode.as_str() {
        "events" | "events-lax" => {
            for ev in miniformat::Reader::new(&text, None)
                .unwrap()
                .strict_keys(mode == "events")
            {
                std::hint::black_box(ev.unwrap());
                n += 1;
            }
        }
        "borrowed" => {
            n = std::hint::black_box(miniformat::loads_borrowed(&text, None).unwrap())
                .get("x")
                .is_some() as usize
        }
        _ => {
            n = std::hint::black_box(miniformat::loads(&text, None).unwrap())
                .get("x")
                .is_some() as usize
        }
    }
    let rss = std::fs::read_to_string("/proc/self/status").unwrap();
    let hwm = rss.lines().find(|l| l.starts_with("VmHWM")).unwrap();
    println!("{mode:11} {hwm}  ({n})");
}
