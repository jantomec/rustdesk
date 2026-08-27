//! Capture a named Wayland output through wlr-screencopy and write it as a PPM.
//!
//!     cargo run --release -p scrap --example screencopy --features wayland -- OUTPUT WIDTH HEIGHT out.ppm
#[cfg(all(target_os = "linux", feature = "wayland"))]
fn main() {
    use scrap::wayland::screencopy::ScreencopyCapturer;
    use scrap::{Frame, Pixfmt, TraitCapturer, TraitPixelBuffer};
    use std::io::Write;
    use std::time::{Duration, Instant};

    let args: Vec<String> = std::env::args().collect();
    if args.len() != 5 {
        eprintln!("usage: screencopy OUTPUT WIDTH HEIGHT out.ppm");
        std::process::exit(2);
    }
    let (name, w, h) = (&args[1], args[2].parse().unwrap(), args[3].parse().unwrap());
    let mut cap = ScreencopyCapturer::new(name, w, h, true).expect("create capturer");
    let mut last = None;
    let t0 = Instant::now();
    for i in 0..10 {
        match cap.frame(Duration::from_millis(500)) {
            Ok(Frame::PixelBuffer(pb)) => {
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
                println!("frame {i}: {fw}x{fh} stride {stride} fmt {:?}", pb.pixfmt());
            }
            Ok(Frame::Texture(_)) => println!("frame {i}: texture (unexpected)"),
            Err(e) => println!("frame {i}: error {e}"),
        }
    }
    println!("10 frames in {:?}", t0.elapsed());
    let (fw, fh, rgb) = last.expect("at least one frame");
    let mut f = std::fs::File::create(&args[4]).unwrap();
    write!(f, "P6\n{fw} {fh}\n255\n").unwrap();
    f.write_all(&rgb).unwrap();
    println!("wrote {}", args[4]);
}

#[cfg(not(all(target_os = "linux", feature = "wayland")))]
fn main() {
    eprintln!("linux + wayland feature only");
}
