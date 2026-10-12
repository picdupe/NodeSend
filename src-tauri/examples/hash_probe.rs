//! Matches the transfer path: SHA-256 plus lowercase hex on 4 MiB chunks.
use sha2::{Digest, Sha256};
use std::{hint::black_box, time::Instant};

fn main() {
    assert_eq!(
        format!("{:x}", Sha256::digest(b"abc")),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    let chunk = vec![0x5a; 4 * 1024 * 1024];
    let rounds = 64;
    let start = Instant::now();
    for _ in 0..rounds {
        black_box(format!("{:x}", Sha256::digest(black_box(&chunk))));
    }
    let seconds = start.elapsed().as_secs_f64();
    println!(
        "SHA-256: {:.2} MB/s ({seconds:.3}s)",
        chunk.len() as f64 * rounds as f64 / seconds / 1e6
    );
    #[cfg(target_arch = "x86_64")]
    println!("CPU SHA-NI: {}", std::is_x86_feature_detected!("sha"));
}
