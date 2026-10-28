# Legacy fixed-media receiver compatibility

Baseline: `4b7e01c9d39c22a7afaffe0b9151f4519a240708`.

An official Xiaomi sender connected to the Linux receiver, but encrypted AAC was treated as clear data when no `0x6c` session keys were supplied. This produced invalid ADTS sample-rate errors (8kHz/96kHz) and immediate disconnection despite the actual source being 48kHz.

An independent decoder recovered valid 48kHz ADTS from **49/49, 27/27, and 50/50 frames** using the established library-fixed key and each PES private IV. RTSP-advertised key fields did not directly decode those media frames. This establishes a compatibility gap in that legacy media family; it does not explain every connection failure.

## Opt-in fix

Before starting a receiver, a Rust host can call:

```rust
fusionplay_miplay_sdk::media::enable_legacy_fixed_media_compatibility();
```

The process-wide option is disabled by default and is captured when a media demuxer is created. Existing session keys take priority. The fallback requires no session key and a PES private IV; clear ADTS retains its original path. This branch adds no observer, media-stall injector, group-control changes, or custom receiver host.

The compatibility path was exercised using a separate local Linux host, with USB headphone playback confirmed by the operator after enabling this switch. A later RTSP error from the failed attempts did not recur; no separate RTSP parser fix is claimed. Android APK behavior has not been tested.

## Regression coverage

The AES-CBC known-answer regression checks explicit opt-in and private-IV gates using synthetic data. Captured media, device details, authentication material, and session keys are not included. The SDK unit-test suite passes 91 tests for this compatibility-only change.

Source and modifications retain the upstream license; see `LICENSE`.
