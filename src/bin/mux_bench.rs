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

    let mut storage = encoded;
    let start = Instant::now();
    let mut bytes = 0usize;
    for _ in 0..ITERS {
        let frame = MuxFrame::decode_owned(storage).unwrap();
        bytes = bytes.wrapping_add(frame.payload().len());
        storage = frame.into_storage();
        black_box(&storage);
    }
    let elapsed = start.elapsed();
    let ns_per = elapsed.as_secs_f64() * 1e9 / ITERS as f64;
    let mbps = bytes as f64 / elapsed.as_secs_f64() / 1024.0 / 1024.0;

    println!("rushway_mux_decode_owned_reused iters={ITERS} payload={PAYLOAD} ns/op={ns_per:.2} MB/s={mbps:.2}");
}
