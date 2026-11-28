//! Microphone check for native programs (28.11): `SYS_SOUND` `SOUND_MIC_*`,
//! the HDA input with a cursor of its own (the same input as `hw:0,1` of the
//! Linux layer, a cursor per open). `miccheck [seconds]` prints the input, reads for 1..30 s
//! (default 2) and prints frames, peak and rms. Two copies at once must both
//! get every frame - no consumer takes frames from another.

#![no_std]
#![no_main]

use user_progs::{Args, Line, exit, mic_close, mic_info, mic_open, mic_read, uptime_ms};

#[unsafe(no_mangle)]
pub extern "C" fn _start(argc: usize, argv: *const *const u8) -> ! {
    // SAFETY: the pair (argc, argv) comes from the kernel and lives as long as the program.
    let args = unsafe { Args::new(argc, argv) };
    let seconds = args.get(1).and_then(|word| word.parse::<u64>().ok()).unwrap_or(2).clamp(1, 30);
    let mut name = [0u8; 128];
    let len = mic_info(&mut name);
    if len < 0 {
        Line::new().str("miccheck: no microphone input (").signed(len).str(")").end();
        exit(1)
    }
    let shown = core::str::from_utf8(&name[..(len as usize).min(name.len())]).unwrap_or("?");
    Line::new().str("miccheck: input ").str(shown).end();
    let handle = mic_open();
    if handle < 0 {
        Line::new().str("miccheck: FAILED, open error ").signed(handle).end();
        exit(1)
    }
    // 25 ms a read.
    let mut buf = [0u8; 4800];
    let want = seconds * 48_000;
    let start = uptime_ms();
    let deadline = start + seconds * 1000 + 2000;
    let (mut frames, mut peak, mut squares, mut empty) = (0u64, 0u16, 0u64, 0u64);
    while frames < want && uptime_ms() < deadline {
        let got = mic_read(handle, &mut buf);
        if got < 0 {
            Line::new().str("miccheck: FAILED, read error ").signed(got).end();
            exit(1)
        }
        if got == 0 {
            empty += 1;
            continue;
        }
        for pair in buf[..got as usize].chunks_exact(2) {
            let sample = i16::from_le_bytes([pair[0], pair[1]]);
            peak = peak.max(sample.unsigned_abs());
            squares += (i64::from(sample) * i64::from(sample)) as u64;
        }
        frames += got as u64 / 4;
    }
    let ms = uptime_ms() - start;
    mic_close(handle);
    let rms = (squares / (frames * 2).max(1)).isqrt();
    Line::new()
        .str("miccheck: read ")
        .num(frames)
        .str(" frames in ")
        .num(ms)
        .str(" ms, peak ")
        .num(u64::from(peak))
        .str(", rms ")
        .num(rms)
        .str(", empty reads ")
        .num(empty)
        .end();
    if frames >= want * 9 / 10 {
        Line::new().str("miccheck: OK").end();
        exit(0)
    }
    Line::new().str("miccheck: FAILED, too few frames").end();
    exit(1)
}
