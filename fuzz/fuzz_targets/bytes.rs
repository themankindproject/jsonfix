//! `repair_bytes` must never panic, agree with `repair` byte for byte on
//! valid UTF-8, report the exact invalid-UTF-8 offset, and restore the
//! caller's buffer when it fails.
#![no_main]

use jsonfix::{ErrorKind, Options, repair, repair_bytes, repair_bytes_into, validate};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // repair_bytes_into appends on success and trims back to the previous
    // length on failure — the sentinel must survive either way.
    const SENTINEL: &[u8] = b"\xe2\x9f\xa6sentinel\xe2\x9f\xa7";
    let mut buf = SENTINEL.to_vec();
    match repair_bytes_into(data, &mut buf, Options::all()) {
        Ok(()) => {
            assert!(buf.starts_with(SENTINEL), "sentinel consumed");
            let repaired = &buf[SENTINEL.len()..];
            let standalone = repair_bytes(data).expect("repair_bytes_into and repair_bytes agree");
            assert_eq!(
                repaired,
                &standalone[..],
                "repair_bytes_into appended {repaired:?} but repair_bytes returned {standalone:?}"
            );
            // Output is JSON text: valid UTF-8, strict-valid, and a fixpoint.
            let text = core::str::from_utf8(repaired).expect("repaired output must be UTF-8");
            validate(text).unwrap_or_else(|error| {
                panic!("repair_bytes({data:?}) produced {text:?} which fails validate: {error}")
            });
            let twice = repair(text).expect("repair of the repaired output");
            assert_eq!(text, twice, "byte path not a fixpoint: {data:?}");

            // On valid input the byte path and the text path cannot drift.
            if let Ok(input) = core::str::from_utf8(data) {
                let via_text = repair(input).expect("text path must agree with the byte path");
                assert_eq!(
                    repaired,
                    via_text.as_bytes(),
                    "repair_bytes and repair disagree on {input:?}"
                );
            }
        }
        Err(error) => {
            assert_eq!(
                buf, SENTINEL,
                "repair_bytes_into must restore the buffer on error ({error})"
            );
            let standalone =
                repair_bytes(data).expect_err("repair_bytes_into failed, so repair_bytes must too");
            assert_eq!(
                error.kind(),
                standalone.kind(),
                "repair_bytes_into and repair_bytes disagree on {data:?}"
            );
            assert!(error.position() <= data.len(), "position past input end");
            if error.kind() == &ErrorKind::InvalidUtf8 {
                // The reported offset is the first byte that does not decode.
                assert!(
                    core::str::from_utf8(&data[..error.position()]).is_ok(),
                    "InvalidUtf8 position {} splits a code unit in {data:?}",
                    error.position()
                );
                assert!(
                    core::str::from_utf8(&data[..error.position() + 1]).is_err(),
                    "InvalidUtf8 position {} is not the first bad byte in {data:?}",
                    error.position()
                );
            }
        }
    }
});
