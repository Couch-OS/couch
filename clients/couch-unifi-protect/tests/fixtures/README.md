`test-pattern.h264` contains eight synthetic `testsrc2` frames, no camera data.
Generated with FFmpeg `6.1.1-3ubuntu5+esm13` on 2026-09-10:

```sh
ffmpeg -hide_banner -f lavfi -i testsrc2=size=480x270:rate=8 -frames:v 8 \
  -c:v libx264 -preset ultrafast -tune zerolatency -f h264 test-pattern.h264 -y
```

The integration test serves this through local certificate-pinned RTSPS and
SDES-SRTP, including fragmented NAL packets, then checks decoded moving RGB frames
and view cancellation. It never contacts a console. The test is explicitly ignored in ordinary host runs because desktop FFmpeg
closures can exceed the production address-space bound. CI runs it with
`--ignored` inside Alpine with FFmpeg installed. The synthetic generator recipe
is reproducible input provenance, not a cross-version byte identity claim.
