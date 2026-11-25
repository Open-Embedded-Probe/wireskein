//! How long analyze_file takes and how much it holds, alone (wsk-analyze FILE [MODE]): the time to the GUI's list.

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let mode = a.get(1).map(|s| s.as_str()).unwrap_or("all");
    let t = std::time::Instant::now();
    let opts = wireskein_analysis::split::Options::default();
    let split = a.get(2).is_some_and(|s| s == "split");
    let r = wireskein_analysis::analyze_file(
        std::path::Path::new(&a[0]),
        &serde_json::json!({}),
        None,
        mode,
        None,
        if split { Some(&opts) } else { None },
    );
    let rss = std::fs::read_to_string("/proc/self/status")
        .unwrap_or_default()
        .lines()
        .find(|l| l.starts_with("VmHWM"))
        .unwrap_or("")
        .to_string();
    match r {
        Ok(doc) => {
            let n: usize = doc["parts"][0]["claims"].as_array().map(|v| v.len()).unwrap_or(0);
            if split {
                let s = doc["parts"][0]["split"].to_string();
                eprintln!("split: {}", &s[..s.len().min(400)]);
                for c in doc["parts"][0]["claims"].as_array().into_iter().flatten() {
                    let ls: Vec<String> = c["layers"].as_object().map(|o| o.iter().map(|(k, v)| format!("{k}:{}", v.as_array().map(|x| x.len()).unwrap_or(0))).collect()).unwrap_or_default();
                    eprintln!("  {} {} {:?}", c["protocol"], c["verdict"], ls);
                }
            }
            eprintln!("{mode}: {:.2} s, {n} claims, {} bytes of JSON, peak {rss}", t.elapsed().as_secs_f64(), doc.to_string().len());
        }
        Err(e) => eprintln!("{mode}: error {e} after {:.2} s, peak {rss}", t.elapsed().as_secs_f64()),
    }
}
