//! Timing probe: capture a named output N times through wlr-screencopy, print per-frame latency.
//!
//!     screencopy OUTPUT WIDTH HEIGHT out.ppm [N] [embed_cursor(0|1)]
#[cfg(all(target_os = "linux", feature = "wayland"))]
fn main() {
    use scrap::wayland::screencopy::ScreencopyCapturer;
    use scrap::{Frame, Pixfmt, TraitCapturer, TraitPixelBuffer};
    use std::io::Write;
    use std::time::{Duration, Instant};

    let args: Vec<String> = std::env::args().collect();
    if args.len() < 5 {
        eprintln!("usage: screencopy OUTPUT WIDTH HEIGHT out.ppm [N] [embed(0|1)]");
        std::process::exit(2);
    }
    let (name, w, h) = (&args[1], args[2].parse().unwrap(), args[3].parse().unwrap());
    let n: usize = args.get(5).map(|s| s.parse().unwrap()).unwrap_or(30);
    let embed = args.get(6).map(|s| s == "1").unwrap_or(true);
    let work_ms: u64 = args.get(7).map(|s| s.parse().unwrap()).unwrap_or(0);
    let mut cap = ScreencopyCapturer::new(name, w, h, embed).expect("create capturer");
    let mut last = None;
    let mut times = Vec::with_capacity(n);
    let t0 = Instant::now();
    for i in 0..n {
        let f0 = Instant::now();
        match cap.frame(Duration::from_millis(1000)) {
            Ok(Frame::PixelBuffer(pb)) => {
                let dt = f0.elapsed();
                times.push(dt.as_secs_f64() * 1000.0);
                if i == n - 1 {
                    let (fw, fh, stride) = (pb.width(), pb.height(), pb.stride()[0]);
                    let mut rgb = Vec::with_capacity(fw * fh * 3);
                    for row in pb.data().chunks_exact(stride).take(fh) {
                        for px in row[..fw * 4].chunks_exact(4) {
                            match pb.pixfmt() {
                                Pixfmt::BGRA => rgb.extend_from_slice(&[px[2], px[1], px[0]]),
                                _ => rgb.extend_from_slice(&[px[0], px[1], px[2]]),
                            }
                        }
                    }
                    last = Some((fw, fh, rgb));
                }
            }
            Ok(Frame::Texture(_)) => println!("frame {i}: texture (unexpected)"),
            Err(e) => println!("frame {i}: error {e}"),
        }
        if work_ms > 0 {
            std::thread::sleep(Duration::from_millis(work_ms));
        }
    }
    let total = t0.elapsed();
    let ok = times.len();
    times.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let sum: f64 = times.iter().sum();
    println!(
        "{ok}/{n} frames in {:.1} ms -> {:.1} fps sustained; per-frame ms min {:.1} p50 {:.1} p90 {:.1} max {:.1} avg {:.1}",
        total.as_secs_f64() * 1000.0,
        ok as f64 / total.as_secs_f64(),
        times.first().unwrap_or(&0.0),
        times.get(ok / 2).unwrap_or(&0.0),
        times.get(ok * 9 / 10).unwrap_or(&0.0),
        times.last().unwrap_or(&0.0),
        sum / ok.max(1) as f64
    );
    if let Some((fw, fh, rgb)) = last {
        let mut f = std::fs::File::create(&args[4]).unwrap();
        write!(f, "P6\n{fw} {fh}\n255\n").unwrap();
        f.write_all(&rgb).unwrap();
    }
}

#[cfg(not(all(target_os = "linux", feature = "wayland")))]
fn main() {
    eprintln!("linux + wayland feature only");
}
