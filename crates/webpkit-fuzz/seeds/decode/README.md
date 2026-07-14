# `decode` corpus seeds

libFuzzer starts from the files in this directory and mutates them, so good
seeds are real inputs that reach deep into the decode path. Populate this
directory with **transparent-lossy `.webp` files** (a VP8 lossy image plus a
sibling `ALPH` chunk) copied from the `webp` / `webpkit-lossy` conformance fixtures,
so a mutated byte still lands on the container -> webpkit-lossy -> ALPH -> webpkit-lossless
dispatch this target exercises.

Binary seeds are intentionally not committed here by the authoring agent; the
parent populates them from the checked-in conformance fixtures. A pure
transparent-lossy fixture yields the highest-value seed, but any valid `.webp`
the umbrella decoder accepts is a usable starting point.

The corpus is optional: `cargo fuzz run decode` works with an empty directory,
just less efficiently.
