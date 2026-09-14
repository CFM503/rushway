#[path = "../protocol.rs"]
mod protocol;

use protocol::{write_frame_parts, MuxCommand, MuxFrame, MUX_HEADER_LEN};
use std::hint::black_box;
use std::time::Instant;

fn main() {
    const ITERS: usize = 200_000;
    const PAYLOAD: usize = 32 * 1024;

    let mut encoded = Vec::with_capacity(MUX_HEADER_LEN + PAYLOAD);
    write_frame_parts(&mut encoded, 7, MuxCommand::Data, &vec![7u8; PAYLOAD]).unwrap();

    let start = Instant::now();
    let mut copied = 0usize;
    for _ in 0..ITERS {
        let frame = MuxFrame::decode(black_box(encoded.as_slice())).unwrap();
        copied = copied.wrapping_add(frame.payload.len());
        black_box(frame.payload.len());
    }
    let elapsed = start.elapsed();
    let copy_ns = elapsed.as_secs_f64() * 1e9 / ITERS as f64;
    let copy_mbps = copied as f64 / elapsed.as_secs_f64() / 1024.0 / 1024.0;

    let mut storage = encoded;
    let start = Instant::now();
    let mut owned = 0usize;
    for _ in 0..ITERS {
        let frame = MuxFrame::decode_owned(storage).unwrap();
        owned = owned.wrapping_add(frame.payload().len());
        storage = frame.into_storage();
        black_box(&storage);
    }
    let elapsed_owned = start.elapsed();
    let owned_ns = elapsed_owned.as_secs_f64() * 1e9 / ITERS as f64;
    let owned_mbps = owned as f64 / elapsed_owned.as_secs_f64() / 1024.0 / 1024.0;
    let speedup = if owned_ns > 0.0 {
        copy_ns / owned_ns
    } else {
        0.0
    };

    println!("rushway_mux_decode_copy iters={ITERS} payload={PAYLOAD} ns/op={copy_ns:.2} MB/s={copy_mbps:.2}");
    println!("rushway_mux_decode_owned_reused iters={ITERS} payload={PAYLOAD} ns/op={owned_ns:.2} MB/s={owned_mbps:.2}");
    println!("owned_vs_copy_speedup={speedup:.2}x");
}
