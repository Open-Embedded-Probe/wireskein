use std::io::Read;
use std::time::Instant;
use wsk::*;
fn main() {
    let a: Vec<String> = std::env::args().collect();
    let mut raw = vec![]; std::fs::File::open(&a[1]).unwrap().read_to_end(&mut raw).unwrap();
    let edges: Vec<i64> = raw.chunks_exact(8).map(|c| i64::from_le_bytes(c.try_into().unwrap())).collect();
    let (initial, n) = (0u8, 9967616i64);
    let units = [69.45675425298973, 34.71751091473357, 23.147208035313568, 17.36100633218203, 13.889752328029694, 8.680818570555894];
    let hyps = hypotheses(8e6, &units);
    let mut tt = [0f64; 6];
    let mut s = Scratch::default();
    for p in &hyps {
        let t0 = Instant::now();
        let key = (p.t.to_bits(), p.idle);
        if s.grid_key != Some(key) {
            k_starts(&edges, initial, p.idle, &mut s.starts);
            let st = std::mem::take(&mut s.starts);
            k_sample_grid(&edges, initial, n, &st, p.t, 13, p.idle, &mut s.grid);
            s.starts = st; s.grid_key = Some(key);
        }
        let t1 = Instant::now();
        let (nbits, frame) = frame_len(p);
        s.busy.clear(); s.busy.extend(s.starts.iter().map(|&x| x + (nbits as f64 + 0.5) * p.t));
        k_chain(&s.starts, &s.busy, &mut s.idx);
        let t2 = Instant::now();
        s.fs.clear(); s.fe.clear();
        for &i in &s.idx { let st = s.starts[i as usize]; s.fs.push(st); s.fe.push(st + frame * p.t); }
        k_in_windows(&s.fs, &s.fe, &edges, &mut s.win);
        let t3 = Instant::now();
        let m = uart_glue(p, &edges, &s.starts, &s.grid, &s.idx, &s.win, &s.fs);
        let t4 = Instant::now();
        std::hint::black_box(m);
        tt[0] += (t1 - t0).as_secs_f64(); tt[1] += (t2 - t1).as_secs_f64(); tt[2] += (t3 - t2).as_secs_f64(); tt[3] += (t4 - t3).as_secs_f64();
    }
    let n = hyps.len() as f64;
    println!("per hyp us: grid(amortized) {:.1}  chain {:.1}  windows {:.1}  glue {:.1}", tt[0]/n*1e6, tt[1]/n*1e6, tt[2]/n*1e6, tt[3]/n*1e6);
    println!("starts per key: {}", s.starts.len());
}
