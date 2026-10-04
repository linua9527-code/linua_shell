# russh 0.44.1 local patch

Source: `russh` 0.44.1 from crates.io (Apache-2.0).

`src/compression.rs` continues zlib compression when the output buffer fills,
including when `flate2` returns `Status::Ok`. The original loop could emit an
incomplete compressed SSH packet for incompressible input, causing the server
to reset the connection during file uploads. The focused roundtrip test covers
successive high-entropy packets on one compression stream.

Remove this override when upgrading to a release with the equivalent fix.
