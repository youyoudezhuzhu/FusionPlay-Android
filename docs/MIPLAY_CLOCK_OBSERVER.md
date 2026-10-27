# MiPlay clock observer and legacy media compatibility

Baseline: `4b7e01c9d39c22a7afaffe0b9151f4519a240708`. This branch adds opt-in diagnostics and an independently selectable NAS media compatibility mode. Normal presentation-clock, queue, and playback-rate algorithms are retained.

## Reproduced compatibility failure

On 2026-10-27 an official Xiaomi tablet connected to the Linux receiver and successfully exchanged timer samples and TimeOffset. The USB audio output opened at 48kHz, but playback stopped immediately with invalid AAC sample-rate errors (8kHz/96kHz), followed by an RTSP parsing error.

Three captured attempts contained legacy fixed-key media without a `0x6c` session-key announcement. `TsDemuxer::finish_pes` returned the encrypted elementary payload unchanged when `self.keys` was absent. An independent decoder, using the previously established library-fixed key and each PES private IV, recovered valid 48kHz ADTS in **49/49, 27/27 and 50/50 frames**. RTSP-advertised key fields did not directly decode those frames.

`--legacy-fixed-media` explicitly enables that known legacy family when there are no session keys and a PES private IV is present. It is disabled by default. Existing session keys take priority; clear ADTS retains its existing path. A known-answer CBC regression checks the explicit opt-in and private-IV gates.

With only this compatibility switch changed, the next official-tablet→NAS experiment played normally through real USB headphones, as confirmed by the operator. Logs showed 48kHz AAC, `sender_clock` startup, and normal channel closure. The later RTSP error did not recur; no separate RTSP parser fix is included and no general claim about all connection failures is made.

## Linux NAS host

```bash
cargo test --locked --target-dir target --manifest-path src/FusionPlay.MiPlaySdk/Cargo.toml
cargo build --locked --example clock_observer \
  --target-dir target --manifest-path src/FusionPlay.MiPlaySdk/Cargo.toml

# Use the actual device id returned here; the host refuses a silent fallback.
./target/debug/examples/clock_observer --list-outputs

./target/debug/examples/clock_observer \
  --legacy-fixed-media \
  --bind <NAS-LAN-IP> --interface <LAN-interface> \
  --output <exact-CPAL-output-id> \
  --jsonl /tmp/miplay-observer/run-01/clock.jsonl \
  --stop-file /tmp/miplay-observer/run-01/stop --seconds 600
```

The commands above explicitly select a shared target directory. The receiver name is **MiPlay Clock Observer NAS**, initial receiver volume is 20%, and it uses the normal discovery/control implementation. Stop the individual host by creating its stop file or let its bounded run expire. No system audio configuration or network routes are changed.

## Diagnostic output

Call `clock_observer::enable(path)` before starting a receiver. By default no writer exists. On Android, the JNI host opts in only if its existing identity directory contains the local `enable-clock-observer` marker; JSONL goes to `miplay-clock.jsonl` in that directory. This Android path has not been built or exercised in the NAS experiment.

The bounded queue accepts fixed-size records; a worker serializes and writes JSONL. Audio callbacks do not serialize JSON or write files. Raw PES samples are decimated (first four, then every32); output snapshots every64 callbacks plus actual hard-drop events. Timer samples, offset updates, and RTSP/control transitions are recorded independently. Queue loss is counted; writing stops at64MiB with a limit event. The clock observer records neither PCM, authentication messages, keys, nor media titles.

Fields include session id, local/server time, raw40B timer samples, timer acceptance and model estimates, raw PES PTS, queue-front media time, TimeOffset, target timer time, output latency, schedule error, queue depth/target, selected playback rate, waits and actual late drops. Missing measurements remain null. PTS jumps are marked for correlation with control logs, not automatically labeled as seek.

Queue records are fresh **post-decision software snapshots**, not exact DAC timestamps or a replay of the earlier decision's atomic reads. Startup mode is also reported by the existing `audio_synchronization_started` event. The Android and desktop playback policies differ in the baseline: Android does not hard-drop running audio or use the desktop rate controller.

The successful NAS run produced239 JSONL records, including60 timer samples and82 complete `M+O-(T+L)` snapshots, with zero queue-lost records. Their arithmetic residual was0; this only validates internal field consistency. No acoustic multi-device synchronization precision, firmware self-recovery, or network impairment tolerance is established.

## Validation and scope

- 93 SDK unit tests passed, including existing loopback protocol tests and the new compatibility gate regression.
- Native Linux example compiled; official-tablet→NAS USB playback was exercised.
- Android APK was not built in this experiment.
- No raw PCAP, account data, session material, device addresses, or captured audio is included in this branch.
- Source and changes retain the upstream repository license; see `LICENSE`.
