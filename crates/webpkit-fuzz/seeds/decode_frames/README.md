# `decode_frames` corpus seeds

libFuzzer starts from the files in this directory and mutates them, so good
seeds are real **animated** `.webp` files that reach deep into the animation
walk this target exercises: the `ANIM`/`ANMF` parser, the per-frame VP8L /
VP8-lossy decode (with sibling `ALPH` alpha), and the canvas compositor.

Populate this directory with **animated-lossy `.webp` fixtures** (an `ANIM`
container whose `ANMF` frames are VP8 lossy + optional `ALPH`, ideally mixed
with a VP8L frame) copied from the committed animated-lossy conformance
fixtures, so a mutated byte still lands on the container -> ANMF -> webpkit-lossy /
webpkit-lossless -> compositor dispatch.

Binary seeds are intentionally not committed here by the authoring agent; the
parent populates them from the checked-in animated-lossy conformance fixtures.
An animation with several frames and per-frame blend/dispose flags yields the
highest-value seed, but any valid animated `.webp` the umbrella decoder accepts
is a usable starting point.

The corpus is optional: `cargo fuzz run decode_frames` works with an empty
directory, just less efficiently.
