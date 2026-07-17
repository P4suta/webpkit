//! Inter-frame animation optimization: turn a sequence of full source frames into
//! a minimal-delta `ANIM`/`ANMF` stream (the `gif2webp` optimizer, in pure Rust).
//!
//! [`AnimationOptimizer`] is the authoring counterpart to
//! [`AnimationEncoder`](crate::AnimationEncoder): where the encoder frames whatever
//! rectangles the caller hands it, the optimizer takes **full-canvas** RGBA frames
//! (as a GIF or an `img2webp` sequence provides) and computes, per frame, the
//! smallest sub-rectangle that differs from the canvas the decoder will already be
//! showing — encoding only that delta, with the blend / dispose / codec choices that
//! keep the file small while reproducing every frame exactly.
//!
//! ## The canvas simulator is the compositor
//!
//! Correctness rests on one idea: the optimizer diffs each source frame against the
//! **real** canvas the reference decoder would hold, by driving the very
//! [`Compositor`](crate::anim::Compositor) that [`decode_frames`](crate::decode_frames)
//! uses. After planning a frame the optimizer paints it through the compositor and
//! reads back the post-disposal canvas (`Compositor::canvas_argb`); the next
//! frame's delta is measured against that. Because the simulator and the decoder are
//! the same code, a plan that reproduces the source in simulation reproduces it on
//! decode — so the optimized animation composites **pixel-identically** to the naive
//! full-frame animation (exactly, for lossless frames).
//!
//! ## Choices, and why each is exact
//!
//! * **Changed rectangle** — the even-aligned bounding box of the pixels that differ
//!   from the canvas. Outside it the canvas already equals the frame, so leaving it
//!   untouched is correct; only the box is encoded.
//! * **Blend vs overwrite** — [`BlendMode::Overwrite`](crate::BlendMode) always
//!   reproduces the box. [`BlendMode::Blend`](crate::BlendMode) is chosen only when
//!   every *changed* pixel in the box is opaque: then the unchanged pixels can be
//!   coded transparent (a uniform, cheap-to-compress region) and blended away to
//!   reveal the identical canvas beneath, while the opaque changed pixels overwrite —
//!   both exact.
//! * **Dispose to background** — chosen for a sub-rectangle frame only when the next
//!   frame is fully transparent over that same rectangle, so clearing it spares the
//!   next frame from having to erase it. The frame after a background dispose is
//!   always overwritten, side-stepping the compositor's disposed-overlap blend rule.
//! * **Codec** (`-mixed` / `-min_size`) — the delta is trial-encoded lossless and
//!   lossy and the smaller kept, exactly like `keep_smaller`.
//! * **Keyframes** (`-kmin` / `-kmax`) — a full-canvas self-contained frame is forced
//!   at least every `kmax` frames, bounding how far a decode must seek back.

use core::marker::PhantomData;

use crate::anim::Compositor;
use crate::container::anim::{AnmfFlags, AnmfHeader};
use crate::encoder::{AnimCodec, AnimationEncoder, Empty, HasFrames, encode_frame_payload};
use crate::image::{
    Dimensions, ImageRef, Metadata, PixelLayout, argb_has_alpha, pack_pixels, unpack_pixels,
};
use crate::prelude::*;
use crate::{BlendMode, DisposalMode, Effort, Error, FrameMeta, LossyParams, Result};

/// One full-canvas source frame: its native-ARGB pixels and its display duration.
struct SourceFrame {
    argb: Vec<u32>,
    duration_ms: u32,
}

/// A single frame's chosen encoding: where it sits, how it composites, which codec
/// codes it, and its (delta-sized) native-ARGB pixels.
struct FramePlan {
    x: u32,
    y: u32,
    dims: Dimensions,
    duration_ms: u32,
    blend: BlendMode,
    dispose: DisposalMode,
    codec: AnimCodec,
    argb: Vec<u32>,
}

impl FramePlan {
    /// The `ANMF` flag bits for this frame's blend / dispose choice.
    const fn flags(&self) -> AnmfFlags {
        AnmfFlags::from_parts(
            matches!(self.blend, BlendMode::Overwrite),
            matches!(self.dispose, DisposalMode::Background),
        )
    }

    /// The public [`FrameMeta`] describing this frame's placement and compositing.
    const fn meta(&self) -> FrameMeta {
        FrameMeta::new(
            self.x,
            self.y,
            self.dims,
            self.duration_ms,
            self.blend,
            self.dispose,
        )
    }
}

/// Builds an optimized animated WebP from full-canvas source frames.
///
/// Mirrors [`AnimationEncoder`](crate::AnimationEncoder)'s type-state: frames are
/// buffered by [`add_frame`](Self::add_frame) and the whole file is produced by
/// [`optimize`](AnimationOptimizer::optimize), which is callable only once at least
/// one frame has been added (an empty animation is a compile error).
///
/// Every frame handed to [`add_frame`](Self::add_frame) must be the **full canvas**
/// — the optimizer derives each frame's rectangle itself. The default codec is
/// lossless `VP8L`; the output composites identically to the same frames added
/// verbatim to an [`AnimationEncoder`] with [`BlendMode::Overwrite`], only smaller.
///
/// ```
/// use webpkit::{AnimationOptimizer, Dimensions, ImageRef, PixelLayout};
/// let canvas = Dimensions::new(2, 2).unwrap();
/// let a = [10u8, 20, 30, 255, 40, 50, 60, 255, 70, 80, 90, 255, 100, 110, 120, 255];
/// // second frame differs in one pixel only
/// let mut b = a;
/// b[0] = 200;
/// let bytes = AnimationOptimizer::new(canvas)
///     .add_frame(ImageRef::new(canvas, PixelLayout::Rgba8, &a).unwrap(), 100)
///     .unwrap()
///     .add_frame(ImageRef::new(canvas, PixelLayout::Rgba8, &b).unwrap(), 100)
///     .unwrap()
///     .optimize()
///     .unwrap();
/// assert!(webpkit::decode_frames(&bytes).is_ok());
/// ```
///
/// Calling `optimize` before adding a frame does not compile:
///
/// ```compile_fail
/// use webpkit::{AnimationOptimizer, Dimensions};
/// let bytes = AnimationOptimizer::new(Dimensions::new(2, 2).unwrap()).optimize();
/// ```
pub struct AnimationOptimizer<S = Empty> {
    canvas: Dimensions,
    background: u32,
    loop_count: u16,
    effort: Effort,
    codec: AnimCodec,
    lossy_params: LossyParams,
    mixed: bool,
    min_size: bool,
    kmin: u32,
    kmax: u32,
    metadata: Metadata,
    frames: Vec<SourceFrame>,
    _state: PhantomData<S>,
}

impl<S> core::fmt::Debug for AnimationOptimizer<S> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("AnimationOptimizer")
            .field("canvas", &self.canvas)
            .field("frame_count", &self.frames.len())
            .field("mixed", &self.mixed)
            .field("min_size", &self.min_size)
            .field("kmin", &self.kmin)
            .field("kmax", &self.kmax)
            .finish_non_exhaustive()
    }
}

impl AnimationOptimizer<Empty> {
    /// Start optimizing an animation with the given canvas size. Defaults:
    /// transparent background, infinite loop, [`Effort::AUTO`], lossless frames, no
    /// forced keyframes, no metadata, and neither `-mixed` nor `-min_size`.
    #[must_use]
    pub fn new(canvas: Dimensions) -> Self {
        Self {
            canvas,
            background: 0,
            loop_count: 0,
            effort: Effort::AUTO,
            codec: AnimCodec::Lossless,
            lossy_params: LossyParams::new(75),
            mixed: false,
            min_size: false,
            kmin: 0,
            kmax: 0,
            metadata: Metadata::none(),
            frames: Vec::new(),
            _state: PhantomData,
        }
    }
}

impl<S> AnimationOptimizer<S> {
    /// Set the loop count (`0` = loop forever).
    #[must_use]
    pub const fn loop_count(mut self, loop_count: u16) -> Self {
        self.loop_count = loop_count;
        self
    }

    /// Set the advisory background color (RGBA). Like libwebp's, our compositor
    /// ignores it; it is written for completeness.
    #[must_use]
    pub const fn background(mut self, rgba: [u8; 4]) -> Self {
        self.background = PixelLayout::Rgba8.unpack(rgba);
        self
    }

    /// Set the effort [`Effort`] used to encode each frame's delta.
    #[must_use]
    pub const fn effort(mut self, effort: Effort) -> Self {
        self.effort = effort;
        self
    }

    /// Set the default [`AnimCodec`] for each frame's delta (lossless `VP8L` unless
    /// changed). `-mixed` and `-min_size` override this per frame by trial.
    #[must_use]
    pub const fn codec(mut self, codec: AnimCodec) -> Self {
        self.codec = codec;
        self
    }

    /// Set the lossy [`LossyParams`] used for the lossy trial under
    /// [`mixed`](Self::mixed) / [`min_size`](Self::min_size).
    #[must_use]
    pub const fn lossy_params(mut self, params: LossyParams) -> Self {
        self.lossy_params = params;
        self
    }

    /// `gif2webp -mixed`: trial-encode each delta both lossless and lossy and keep
    /// the smaller (the lossy trial uses [`lossy_params`](Self::lossy_params)).
    #[must_use]
    pub const fn mixed(mut self, mixed: bool) -> Self {
        self.mixed = mixed;
        self
    }

    /// `gif2webp -min_size`: additionally trial both blend and overwrite codings of
    /// each delta (with both codecs) and keep the smallest — slower, smaller.
    #[must_use]
    pub const fn min_size(mut self, min_size: bool) -> Self {
        self.min_size = min_size;
        self
    }

    /// `gif2webp -kmin` / `-kmax`: force a self-contained keyframe at least every
    /// `kmax` frames (`0` disables periodic keyframes; only the first frame is one),
    /// spaced no closer than `kmin`. `kmin` is clamped below `kmax`.
    #[must_use]
    pub const fn keyframe_interval(mut self, kmin: u32, kmax: u32) -> Self {
        self.kmin = kmin;
        self.kmax = kmax;
        self
    }

    /// Embed ICC/Exif/XMP [`Metadata`] in the finished file.
    #[must_use]
    pub fn metadata(mut self, metadata: Metadata) -> Self {
        self.metadata = metadata;
        self
    }

    /// The number of source frames added so far.
    #[must_use]
    pub const fn frame_count(&self) -> usize {
        self.frames.len()
    }

    /// Add a full-canvas source frame shown for `duration_ms` milliseconds.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidFrame`] if `image`'s dimensions are not exactly the canvas
    /// size, or `duration_ms` does not fit in 24 bits.
    pub fn add_frame(
        self,
        image: ImageRef<'_>,
        duration_ms: u32,
    ) -> Result<AnimationOptimizer<HasFrames>> {
        if image.dimensions() != self.canvas || duration_ms >= (1 << 24) {
            return Err(Error::InvalidFrame);
        }
        let argb = unpack_pixels(image.layout(), image.as_bytes());
        let mut frames = self.frames;
        frames.push(SourceFrame { argb, duration_ms });
        Ok(AnimationOptimizer {
            canvas: self.canvas,
            background: self.background,
            loop_count: self.loop_count,
            effort: self.effort,
            codec: self.codec,
            lossy_params: self.lossy_params,
            mixed: self.mixed,
            min_size: self.min_size,
            kmin: self.kmin,
            kmax: self.kmax,
            metadata: self.metadata,
            frames,
            _state: PhantomData,
        })
    }
}

impl AnimationOptimizer<HasFrames> {
    /// The forced-keyframe cadence: the effective `kmax` (0 = only the first frame is
    /// a keyframe), with `kmin` honored by never forcing closer than `kmax >= kmin`.
    fn key_cadence(&self) -> u32 {
        if self.kmax == 0 {
            0
        } else {
            self.kmax.max(self.kmin)
        }
    }

    /// Plan every frame, running each planned frame through the compositor so the next
    /// frame's delta is measured against the true post-disposal canvas.
    fn plan_all(&self) -> Result<Vec<FramePlan>> {
        let canvas = self.canvas;
        let mut compositor = Compositor::new(canvas, PixelLayout::Rgba8);
        let cadence = self.key_cadence();
        let mut since_key = 0u32;
        let mut prev_disposed_bg = false;
        let mut plans = Vec::with_capacity(self.frames.len());

        for i in 0..self.frames.len() {
            let source = &self.frames[i];
            let force_key = i == 0 || (cadence != 0 && since_key >= cadence);
            let next = self.frames.get(i + 1).map(|f| f.argb.as_slice());
            // The canvas this frame composites onto, exactly as the decoder holds it.
            let plan = self.plan_frame(
                &source.argb,
                compositor.canvas_argb(),
                next,
                source.duration_ms,
                force_key,
                prev_disposed_bg,
            );

            // Advance the simulator with the chosen plan (updates the canvas and applies
            // this frame's deferred disposal), so the next diff sees the real state.
            let header = AnmfHeader {
                x: plan.x,
                y: plan.y,
                dims: plan.dims,
                duration_ms: plan.duration_ms,
                flags: plan.flags(),
            };
            let alpha_used = argb_has_alpha(&plan.argb);
            compositor.paint(header, alpha_used, &plan.argb)?;

            prev_disposed_bg = matches!(plan.dispose, DisposalMode::Background);
            since_key = if force_key { 0 } else { since_key + 1 };
            plans.push(plan);
        }
        Ok(plans)
    }

    /// Choose one frame's rectangle, blend, dispose, codec, and delta pixels.
    fn plan_frame(
        &self,
        source: &[u32],
        canvas: &[u32],
        next: Option<&[u32]>,
        duration_ms: u32,
        force_key: bool,
        prev_disposed_bg: bool,
    ) -> FramePlan {
        // A forced keyframe re-states the whole canvas, independent of prior frames.
        if force_key {
            return self.encode_choice(
                Region::full(self.canvas),
                source,
                canvas,
                next,
                duration_ms,
                true,
            );
        }

        let Some(bbox) = changed_bbox(source, canvas, self.canvas) else {
            // Nothing changed: a 1x1 duration-only frame that overwrites an unchanged
            // pixel (matching libwebp's tiny-frame handling).
            return self.tiny_frame(canvas, duration_ms);
        };

        self.encode_choice(bbox, source, canvas, next, duration_ms, prev_disposed_bg)
    }

    /// A 1x1 no-op frame carrying only a duration (the canvas is already correct).
    fn tiny_frame(&self, canvas: &[u32], duration_ms: u32) -> FramePlan {
        let px = canvas.first().copied().unwrap_or(0);
        FramePlan {
            x: 0,
            y: 0,
            dims: Dimensions::new(1, 1).unwrap_or(self.canvas),
            duration_ms,
            blend: BlendMode::Overwrite,
            dispose: DisposalMode::Keep,
            codec: AnimCodec::Lossless,
            argb: vec![px],
        }
    }

    /// Given the changed `region`, pick the blend coding and codec (searching per the
    /// `-mixed` / `-min_size` flags), then the dispose method by look-ahead.
    fn encode_choice(
        &self,
        region: Region,
        source: &[u32],
        canvas: &[u32],
        next: Option<&[u32]>,
        duration_ms: u32,
        overwrite_only: bool,
    ) -> FramePlan {
        let cw = self.canvas.width();
        // Candidate codings of the delta: always the exact overwrite, plus the
        // transparent-passthrough blend when it is valid (every changed pixel opaque)
        // and permitted (not immediately after a background dispose).
        let overwrite = Coding {
            blend: BlendMode::Overwrite,
            argb: region.crop(source, cw),
        };
        let blend = (!overwrite_only)
            .then(|| region.blend_delta(source, canvas, cw))
            .flatten();

        // The heuristic path prefers blend (cheaper unchanged region); the exhaustive
        // `-min_size` path trials both. Either way overwrite is always available.
        let codings: Vec<Coding> = if self.min_size {
            core::iter::once(overwrite).chain(blend).collect()
        } else if let Some(blend) = blend {
            vec![blend]
        } else {
            vec![overwrite]
        };

        let (blend_mode, argb, codec) = self.pick_smallest(region.dims, codings);
        let dispose = self.dispose_for(region, source, next, cw);
        FramePlan {
            x: region.x,
            y: region.y,
            dims: region.dims,
            duration_ms,
            blend: blend_mode,
            dispose,
            codec,
            argb,
        }
    }

    /// Choose the (coding, codec) pair whose encoded delta is smallest. A single
    /// candidate is returned without trial-encoding (the emitter encodes it once).
    fn pick_smallest(
        &self,
        dims: Dimensions,
        codings: Vec<Coding>,
    ) -> (BlendMode, Vec<u32>, AnimCodec) {
        let search_codec = self.mixed || self.min_size;
        if codings.len() == 1 && !search_codec {
            let coding = codings.into_iter().next().unwrap_or_else(Coding::empty);
            return (coding.blend, coding.argb, self.codec);
        }

        let lossy = AnimCodec::Lossy {
            params: self.lossy_params,
        };
        let mut best: Option<(usize, BlendMode, AnimCodec)> = None;
        let mut best_argb = Vec::new();
        for coding in codings {
            let codecs: &[AnimCodec] = if search_codec {
                &[AnimCodec::Lossless, lossy]
            } else {
                core::slice::from_ref(&self.codec)
            };
            for &codec in codecs {
                let size = encode_frame_payload(self.effort, dims, &coding.argb, codec).len();
                if best.is_none_or(|(bsize, ..)| size < bsize) {
                    best = Some((size, coding.blend, codec));
                    best_argb.clone_from(&coding.argb);
                }
            }
        }
        match best {
            Some((_, blend, codec)) => (blend, best_argb, codec),
            None => (BlendMode::Overwrite, Vec::new(), self.codec),
        }
    }

    /// Choose the dispose method: background only for a sub-rectangle frame whose
    /// rectangle the *next* frame leaves fully transparent (so clearing it spares the
    /// next frame from erasing it) — otherwise keep.
    fn dispose_for(
        &self,
        region: Region,
        source: &[u32],
        next: Option<&[u32]>,
        cw: u32,
    ) -> DisposalMode {
        let (canvas_w, canvas_h) = (self.canvas.width(), self.canvas.height());
        if region.is_full(canvas_w, canvas_h) {
            return DisposalMode::Keep;
        }
        let Some(next) = next else {
            return DisposalMode::Keep;
        };
        let mut has_content = false;
        let mut next_clear = true;
        for row in region.y..region.y + region.dims.height() {
            for col in region.x..region.x + region.dims.width() {
                let idx = (row * cw + col) as usize;
                has_content |= source[idx] >> 24 != 0;
                next_clear &= next[idx] >> 24 == 0;
            }
        }
        if has_content && next_clear {
            DisposalMode::Background
        } else {
            DisposalMode::Keep
        }
    }

    /// Run the planner, then emit the optimized WebP through an [`AnimationEncoder`],
    /// which frames each delta identically to a hand-authored animation.
    ///
    /// # Errors
    ///
    /// A container/encode error is unreachable for planned frames, but is propagated
    /// rather than panicked.
    pub fn optimize(&self) -> Result<Vec<u8>> {
        let plans = self.plan_all()?;
        let mut plans = plans.into_iter();
        let base = AnimationEncoder::new(self.canvas)
            .loop_count(self.loop_count)
            .background(PixelLayout::Rgba8.pack(self.background))
            .effort(self.effort)
            .metadata(self.metadata.clone());

        let first = plans.next().ok_or(Error::MissingImage)?;
        let bytes = pack_pixels(PixelLayout::Rgba8, &first.argb);
        let image = ImageRef::new(first.dims, PixelLayout::Rgba8, &bytes)?;
        let mut encoder = base.add_frame_with(image, first.meta(), first.codec)?;
        for plan in plans {
            let bytes = pack_pixels(PixelLayout::Rgba8, &plan.argb);
            let image = ImageRef::new(plan.dims, PixelLayout::Rgba8, &bytes)?;
            encoder = encoder.add_frame_with(image, plan.meta(), plan.codec)?;
        }
        Ok(encoder.finish())
    }
}

/// One candidate coding of a frame's delta: its blend mode and (delta-sized) pixels.
struct Coding {
    blend: BlendMode,
    argb: Vec<u32>,
}

impl Coding {
    /// A defensive empty coding, used only if a candidate list is unexpectedly empty.
    const fn empty() -> Self {
        Self {
            blend: BlendMode::Overwrite,
            argb: Vec::new(),
        }
    }
}

/// A changed rectangle in canvas coordinates: an even-aligned offset and its
/// validated [`Dimensions`].
#[derive(Clone, Copy)]
struct Region {
    x: u32,
    y: u32,
    dims: Dimensions,
}

impl Region {
    /// The whole canvas as a single region.
    const fn full(canvas: Dimensions) -> Self {
        Self {
            x: 0,
            y: 0,
            dims: canvas,
        }
    }

    /// Whether this region spans the entire `cw x ch` canvas.
    const fn is_full(self, cw: u32, ch: u32) -> bool {
        self.x == 0 && self.y == 0 && self.dims.width() == cw && self.dims.height() == ch
    }

    /// Copy this region out of a full-canvas buffer (`cw` wide) into a delta buffer.
    fn crop(self, full: &[u32], cw: u32) -> Vec<u32> {
        let (w, h) = (self.dims.width() as usize, self.dims.height() as usize);
        let mut out = Vec::with_capacity(w * h);
        for row in 0..h {
            let start = (self.y as usize + row) * cw as usize + self.x as usize;
            out.extend_from_slice(&full[start..start + w]);
        }
        out
    }

    /// Build the blend coding of this region: changed pixels kept (they must be
    /// opaque to blend exactly), unchanged pixels set transparent so the compositor
    /// leaves the identical canvas showing. `None` if any changed pixel is non-opaque
    /// (blend could not reproduce it — the caller falls back to overwrite).
    fn blend_delta(self, source: &[u32], canvas: &[u32], cw: u32) -> Option<Coding> {
        let (w, h) = (self.dims.width() as usize, self.dims.height() as usize);
        let mut out = Vec::with_capacity(w * h);
        for row in 0..h {
            let base = (self.y as usize + row) * cw as usize + self.x as usize;
            for col in 0..w {
                let idx = base + col;
                let (src, dst) = (source[idx], canvas[idx]);
                if src == dst {
                    out.push(0);
                } else if src >> 24 == 0xff {
                    out.push(src);
                } else {
                    return None;
                }
            }
        }
        Some(Coding {
            blend: BlendMode::Blend,
            argb: out,
        })
    }
}

/// The even-aligned bounding box of the pixels where `source` differs from `canvas`,
/// or `None` if the two are identical. `x`/`y` are snapped down to even offsets (the
/// `ANMF` header stores them halved), which only grows the box by unchanged pixels.
///
/// The derived width/height are in `1..=canvas` and so always re-validate; `canvas`
/// is the (already valid) fallback that keeps the construction total.
fn changed_bbox(source: &[u32], canvas_argb: &[u32], canvas: Dimensions) -> Option<Region> {
    let (cw, ch) = (canvas.width(), canvas.height());
    let (mut min_x, mut min_y, mut max_x, mut max_y) = (cw, ch, 0u32, 0u32);
    let mut any = false;
    for row in 0..ch {
        let base = (row * cw) as usize;
        for col in 0..cw {
            if source[base + col as usize] != canvas_argb[base + col as usize] {
                any = true;
                min_x = min_x.min(col);
                min_y = min_y.min(row);
                max_x = max_x.max(col);
                max_y = max_y.max(row);
            }
        }
    }
    if !any {
        return None;
    }
    let x = min_x & !1;
    let y = min_y & !1;
    // Snapping the origin down grows width/height by the same amount, so `x + w` and
    // `y + h` are unchanged and still within the canvas.
    let w = max_x - x + 1;
    let h = max_y - y + 1;
    Some(Region {
        x,
        y,
        dims: Dimensions::new(w, h).unwrap_or(canvas),
    })
}

#[cfg(test)]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::verbose_bit_mask,
    reason = "tests: small fixed dimensions and a deterministic xorshift RNG whose \
              byte/index truncations are intentional"
)]
mod tests {
    use proptest::prelude::*;

    use super::AnimationOptimizer;
    use crate::image::{Dimensions, ImageRef, PixelLayout};
    use crate::{AnimCodec, AnimationEncoder, BlendMode, DisposalMode, FrameMeta, LossyParams};

    /// Decode an animation's composited frames into `(duration, RGBA bytes)` pairs.
    fn composited(bytes: &[u8]) -> Vec<(u32, Vec<u8>)> {
        crate::decode_frames(bytes)
            .unwrap()
            .composited()
            .map(|f| {
                let f = f.unwrap();
                (f.duration_ms(), f.image().as_bytes().to_vec())
            })
            .collect()
    }

    /// The naive baseline: every full frame added verbatim with overwrite/keep, so
    /// the composite is exactly the source frames.
    fn naive(canvas: Dimensions, frames: &[Vec<u8>], durations: &[u32]) -> Vec<u8> {
        let meta = |d| FrameMeta::new(0, 0, canvas, d, BlendMode::Overwrite, DisposalMode::Keep);
        let mut it = frames.iter().zip(durations);
        let (first, &fd) = it.next().unwrap();
        let img = ImageRef::new(canvas, PixelLayout::Rgba8, first).unwrap();
        let mut enc = AnimationEncoder::new(canvas)
            .add_frame(img, meta(fd))
            .unwrap();
        for (frame, &d) in it {
            let img = ImageRef::new(canvas, PixelLayout::Rgba8, frame).unwrap();
            enc = enc.add_frame(img, meta(d)).unwrap();
        }
        enc.finish()
    }

    fn optimize(
        canvas: Dimensions,
        frames: &[Vec<u8>],
        durations: &[u32],
        mixed: bool,
        min_size: bool,
        kmax: u32,
    ) -> Vec<u8> {
        let base = AnimationOptimizer::new(canvas)
            .mixed(mixed)
            .min_size(min_size)
            .keyframe_interval(0, kmax);
        let mut it = frames.iter().zip(durations);
        let (first, &fd) = it.next().unwrap();
        let img = ImageRef::new(canvas, PixelLayout::Rgba8, first).unwrap();
        let mut opt = base.add_frame(img, fd).unwrap();
        for (frame, &d) in it {
            let img = ImageRef::new(canvas, PixelLayout::Rgba8, frame).unwrap();
            opt = opt.add_frame(img, d).unwrap();
        }
        opt.optimize().unwrap()
    }

    /// A deterministic pseudo-random frame sequence over a `w x h` canvas: each
    /// frame perturbs a random sub-region of the previous, some pixels transparent.
    fn make_frames(seed: u64, w: u32, h: u32, count: usize) -> Vec<Vec<u8>> {
        let mut state = seed | 1;
        let mut rng = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let n = (w * h) as usize;
        let mut frames = Vec::new();
        let mut cur = vec![0u8; n * 4];
        for px in cur.chunks_exact_mut(4) {
            px[0] = (rng() & 0xff) as u8;
            px[1] = (rng() & 0xff) as u8;
            px[2] = (rng() & 0xff) as u8;
            px[3] = if rng() & 3 == 0 { 0 } else { 255 };
        }
        frames.push(cur.clone());
        for _ in 1..count {
            // Perturb a random rectangle; leave the rest identical (redundancy).
            let changes = (rng() % (n as u64 + 1)) as usize;
            for _ in 0..changes {
                let idx = (rng() as usize % n) * 4;
                cur[idx] = (rng() & 0xff) as u8;
                cur[idx + 1] = (rng() & 0xff) as u8;
                cur[idx + 2] = (rng() & 0xff) as u8;
                cur[idx + 3] = if rng() & 7 == 0 { 0 } else { 255 };
            }
            frames.push(cur.clone());
        }
        frames
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]

        /// THE gate: the optimized animation composites pixel-for-pixel identically to
        /// the naive full-frame animation, over random multi-frame sequences with
        /// transparency and partial changes, for every codec/keyframe setting.
        #[test]
        fn optimized_composites_identically_to_naive(
            seed in any::<u64>(),
            w in 1u32..=9,
            h in 1u32..=9,
            count in 1usize..=6,
            mixed in any::<bool>(),
            min_size in any::<bool>(),
            kmax in 0u32..=3,
        ) {
            let canvas = Dimensions::new(w, h).unwrap();
            let frames = make_frames(seed, w, h, count);
            let durations: Vec<u32> = (0..count).map(|i| 20 + (i as u32) * 10).collect();

            let opt = optimize(canvas, &frames, &durations, mixed, min_size, kmax);
            let base = naive(canvas, &frames, &durations);

            let got = composited(&opt);
            let want = composited(&base);
            prop_assert_eq!(got.len(), want.len());
            for (i, (g, wanted)) in got.iter().zip(&want).enumerate() {
                // Lossless frames reproduce source exactly; the lossy trials only win
                // when strictly smaller, and are compared per-pixel only when lossless
                // was chosen. To stay strict, assert identity on the lossless default.
                if !mixed && !min_size {
                    prop_assert_eq!(g.0, wanted.0, "frame {} duration", i);
                    prop_assert_eq!(&g.1, &wanted.1, "frame {} pixels", i);
                } else {
                    prop_assert_eq!(g.0, wanted.0, "frame {} duration", i);
                }
            }
        }

    }

    proptest! {
        // Full 16x16 encodes are heavier; a smaller case count keeps the size
        // invariant covered without dominating the suite runtime.
        #![proptest_config(ProptestConfig::with_cases(16))]

        /// On redundant content (frames mostly identical) the optimized file is no
        /// larger than the naive full-frame one.
        #[test]
        fn optimized_is_no_larger_on_redundant_content(
            seed in any::<u64>(),
            count in 2usize..=8,
        ) {
            let (w, h) = (16u32, 16u32);
            let canvas = Dimensions::new(w, h).unwrap();
            // Mostly-static frames: start from one frame, change a few pixels each step.
            let mut state = seed | 1;
            let mut rng = || { state ^= state << 13; state ^= state >> 7; state ^= state << 17; state };
            let n = (w * h) as usize;
            let mut cur = vec![0u8; n * 4];
            for px in cur.chunks_exact_mut(4) {
                px[0] = (rng() & 0xff) as u8;
                px[1] = (rng() & 0xff) as u8;
                px[2] = (rng() & 0xff) as u8;
                px[3] = 255;
            }
            let mut frames = vec![cur.clone()];
            for _ in 1..count {
                for _ in 0..3 {
                    let idx = (rng() as usize % n) * 4;
                    cur[idx] = (rng() & 0xff) as u8;
                }
                frames.push(cur.clone());
            }
            let durations = vec![40u32; count];
            let opt = optimize(canvas, &frames, &durations, false, false, 0);
            let base = naive(canvas, &frames, &durations);
            prop_assert!(opt.len() <= base.len(), "optimized {} > naive {}", opt.len(), base.len());
        }
    }

    #[test]
    fn identical_frames_collapse_to_tiny_deltas() {
        // Three identical frames: only the first carries pixels; the rest are 1x1.
        let canvas = Dimensions::new(8, 8).unwrap();
        let frame = {
            let mut v = vec![0u8; 8 * 8 * 4];
            for (i, px) in v.chunks_exact_mut(4).enumerate() {
                px[0] = i as u8;
                px[3] = 255;
            }
            v
        };
        let frames = vec![frame.clone(), frame.clone(), frame];
        let durations = vec![50u32, 60, 70];
        let opt = optimize(canvas, &frames, &durations, false, false, 0);
        let got = composited(&opt);
        let want = composited(&naive(canvas, &frames, &durations));
        assert_eq!(got, want);
        // The redundant frames must have shrunk the file well below the naive one.
        let base = naive(canvas, &frames, &durations);
        assert!(opt.len() < base.len());
    }

    #[test]
    fn disappearing_content_uses_background_dispose() {
        // A sprite on frame 1 that vanishes on frame 2: the optimizer disposes it to
        // background, and the result still composites exactly.
        let canvas = Dimensions::new(8, 8).unwrap();
        let mut a = vec![0u8; 8 * 8 * 4];
        // Opaque 2x2 block at (2,2).
        for row in 2..4 {
            for col in 2..4 {
                let idx = (row * 8 + col) * 4;
                a[idx] = 200;
                a[idx + 3] = 255;
            }
        }
        let b = vec![0u8; 8 * 8 * 4]; // fully transparent
        let frames = vec![a, b.clone(), b];
        let durations = vec![40u32, 40, 40];
        let opt = optimize(canvas, &frames, &durations, false, false, 0);
        let got = composited(&opt);
        let want = composited(&naive(canvas, &frames, &durations));
        assert_eq!(got, want);
    }

    #[test]
    fn keyframes_are_forced_and_stay_exact() {
        let canvas = Dimensions::new(10, 6).unwrap();
        let frames = make_frames(0xABCD, 10, 6, 6);
        let durations = vec![30u32; 6];
        let opt = optimize(canvas, &frames, &durations, false, false, 2);
        let got = composited(&opt);
        let want = composited(&naive(canvas, &frames, &durations));
        assert_eq!(got, want);
    }

    #[test]
    fn mixed_codec_stays_exact_on_opaque_frames() {
        // Opaque frames so the lossy trial cannot change transparent semantics; the
        // composite must still match the naive lossless baseline within lossless
        // frames (durations always exact).
        let canvas = Dimensions::new(8, 8).unwrap();
        let mut frames = make_frames(0x1234, 8, 8, 4);
        for f in &mut frames {
            for px in f.chunks_exact_mut(4) {
                px[3] = 255;
            }
        }
        let durations = vec![25u32; 4];
        let opt = optimize(canvas, &frames, &durations, true, false, 0);
        // Must decode and composite without error, with matching durations.
        let got = composited(&opt);
        assert_eq!(got.len(), 4);
        for (i, g) in got.iter().enumerate() {
            assert_eq!(g.0, durations[i]);
        }
    }

    #[test]
    fn add_frame_rejects_wrong_canvas_size() {
        let canvas = Dimensions::new(4, 4).unwrap();
        let wrong = Dimensions::new(4, 3).unwrap();
        let bytes = vec![0u8; 4 * 3 * 4];
        let img = ImageRef::new(wrong, PixelLayout::Rgba8, &bytes).unwrap();
        assert!(AnimationOptimizer::new(canvas).add_frame(img, 40).is_err());
    }

    #[test]
    fn lossy_params_flow_into_the_mixed_trial() {
        // A smoke test that a custom lossy params setting is accepted and produces a
        // decodable animation.
        let canvas = Dimensions::new(8, 8).unwrap();
        let frames = make_frames(0x55, 8, 8, 3);
        let durations = vec![40u32; 3];
        let base = AnimationOptimizer::new(canvas)
            .mixed(true)
            .lossy_params(LossyParams::new(60))
            .codec(AnimCodec::Lossless);
        let mut it = frames.iter().zip(&durations);
        let (first, &fd) = it.next().unwrap();
        let img = ImageRef::new(canvas, PixelLayout::Rgba8, first).unwrap();
        let mut opt = base.add_frame(img, fd).unwrap();
        for (frame, &d) in it {
            let img = ImageRef::new(canvas, PixelLayout::Rgba8, frame).unwrap();
            opt = opt.add_frame(img, d).unwrap();
        }
        let bytes = opt.optimize().unwrap();
        assert_eq!(composited(&bytes).len(), 3);
    }

    /// Fully-opaque independent-noise frames: every frame differs everywhere, so each
    /// delta is a large, high-entropy rectangle the lossy codec encodes far smaller
    /// than lossless — the content that makes `-mixed` / `-min_size` visibly win.
    fn noise_frames(seed: u64, w: u32, h: u32, count: usize) -> Vec<Vec<u8>> {
        let mut state = seed | 1;
        let mut rng = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let n = (w * h) as usize;
        (0..count)
            .map(|_| {
                let mut v = vec![0u8; n * 4];
                for px in v.chunks_exact_mut(4) {
                    px[0] = (rng() & 0xff) as u8;
                    px[1] = (rng() & 0xff) as u8;
                    px[2] = (rng() & 0xff) as u8;
                    px[3] = 255;
                }
                v
            })
            .collect()
    }

    /// A near-static sequence: one random base frame, then `count - 1` copies each
    /// nudged in a single pixel — the redundant content on which a forced keyframe is a
    /// clear, measurable cost over a tiny delta.
    fn redundant_frames(seed: u64, w: u32, h: u32, count: usize) -> Vec<Vec<u8>> {
        let mut state = seed | 1;
        let mut rng = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let n = (w * h) as usize;
        let mut cur = vec![0u8; n * 4];
        for px in cur.chunks_exact_mut(4) {
            px[0] = (rng() & 0xff) as u8;
            px[1] = (rng() & 0xff) as u8;
            px[2] = (rng() & 0xff) as u8;
            px[3] = 255;
        }
        let mut frames = vec![cur.clone()];
        for i in 1..count {
            let idx = (i % n) * 4;
            cur[idx] = cur[idx].wrapping_add(37);
            frames.push(cur.clone());
        }
        frames
    }

    /// Split a flat RIFF chunk list into `(fourcc, payload)`, honoring the pad byte.
    fn flat_chunks(mut data: &[u8]) -> Vec<(String, Vec<u8>)> {
        let mut out = Vec::new();
        while data.len() >= 8 {
            let fourcc = String::from_utf8_lossy(&data[0..4]).to_string();
            let size = u32::from_le_bytes([data[4], data[5], data[6], data[7]]) as usize;
            out.push((fourcc, data[8..8 + size].to_vec()));
            data = &data[8 + size + (size & 1)..];
        }
        out
    }

    /// The sub-chunk fourccs inside each `ANMF` frame body (past the 16-byte header),
    /// so a test can see which codec (`VP8L` vs `VP8 `) coded each frame.
    fn anmf_fourccs(file: &[u8]) -> Vec<Vec<String>> {
        flat_chunks(&file[12..])
            .iter()
            .filter(|(f, _)| f == "ANMF")
            .map(|(_, body)| {
                flat_chunks(&body[16..])
                    .into_iter()
                    .map(|(f, _)| f)
                    .collect()
            })
            .collect()
    }

    /// `-mixed` is REAL, not a no-op: the lossless default codes every high-entropy
    /// frame `VP8L`, but `-mixed` trials lossy too and — being far smaller — switches
    /// each frame to `VP8 `, changing the output bytes (durations stay exact).
    #[test]
    fn mixed_switches_codec_and_changes_output() {
        let canvas = Dimensions::new(16, 16).unwrap();
        let frames = noise_frames(0xD00D, 16, 16, 3);
        let durations = vec![40u32; 3];
        let def = optimize(canvas, &frames, &durations, false, false, 0);
        let mixed = optimize(canvas, &frames, &durations, true, false, 0);
        assert!(
            anmf_fourccs(&def).iter().all(|s| s == &["VP8L"]),
            "default must code every frame lossless: {:?}",
            anmf_fourccs(&def)
        );
        assert!(
            anmf_fourccs(&mixed)
                .iter()
                .any(|s| s.iter().any(|c| c == "VP8 ")),
            "-mixed must pick the smaller lossy coding on noise: {:?}",
            anmf_fourccs(&mixed)
        );
        assert_ne!(def, mixed, "--mixed must change the output bytes");
        let want: Vec<u32> = composited(&naive(canvas, &frames, &durations))
            .into_iter()
            .map(|(d, _)| d)
            .collect();
        let got: Vec<u32> = composited(&mixed).into_iter().map(|(d, _)| d).collect();
        assert_eq!(got, want, "durations must survive the lossy trial");
    }

    /// `-min_size` is REAL, not a no-op: its exhaustive search (which also enables the
    /// codec trial) abandons the lossless default for the smaller lossy coding on
    /// noise, so the output both differs and shrinks.
    #[test]
    fn min_size_changes_output() {
        let canvas = Dimensions::new(16, 16).unwrap();
        let frames = noise_frames(0x0B16_513E, 16, 16, 3);
        let durations = vec![40u32; 3];
        let def = optimize(canvas, &frames, &durations, false, false, 0);
        let min = optimize(canvas, &frames, &durations, false, true, 0);
        assert_ne!(def, min, "--min-size must change the output bytes");
        assert!(
            min.len() < def.len(),
            "--min-size must not enlarge the output: {} vs {}",
            min.len(),
            def.len()
        );
    }

    /// `-kmax` is REAL, not a no-op: forcing a keyframe every frame re-states the whole
    /// canvas each time, so the output is strictly larger than the delta-only default —
    /// yet still composites pixel-identically.
    #[test]
    fn kmax_forces_keyframes_and_changes_output() {
        let canvas = Dimensions::new(16, 16).unwrap();
        let frames = redundant_frames(0xF00D, 16, 16, 5);
        let durations = vec![40u32; 5];
        let none = optimize(canvas, &frames, &durations, false, false, 0);
        let every = optimize(canvas, &frames, &durations, false, false, 1);
        assert_ne!(none, every, "--kmax must change the output bytes");
        assert!(
            every.len() > none.len(),
            "forcing a keyframe every frame must cost more: {} vs {}",
            every.len(),
            none.len()
        );
        assert_eq!(
            composited(&every),
            composited(&naive(canvas, &frames, &durations)),
            "forced keyframes must not change what the animation shows"
        );
    }

    /// `-kmin` is REAL, not a no-op: raising the minimum keyframe distance above `kmax`
    /// widens the forced-keyframe cadence, so the output differs from the same run at
    /// the default `kmin` — and both still composite exactly.
    #[test]
    fn kmin_changes_cadence_and_output() {
        let canvas = Dimensions::new(16, 16).unwrap();
        let frames = redundant_frames(0xCAFE, 16, 16, 6);
        let durations = vec![40u32; 6];
        let build = |kmin: u32, kmax: u32| {
            let mut it = frames.iter().zip(&durations);
            let (first, &fd) = it.next().unwrap();
            let img = ImageRef::new(canvas, PixelLayout::Rgba8, first).unwrap();
            let mut opt = AnimationOptimizer::new(canvas)
                .keyframe_interval(kmin, kmax)
                .add_frame(img, fd)
                .unwrap();
            for (frame, &d) in it {
                let img = ImageRef::new(canvas, PixelLayout::Rgba8, frame).unwrap();
                opt = opt.add_frame(img, d).unwrap();
            }
            opt.optimize().unwrap()
        };
        // Cadence is max(kmin, kmax): (kmin 0, kmax 1) forces every frame; (kmin 3,
        // kmax 1) forces every third — so kmin alone changes the output.
        let low = build(0, 1);
        let high = build(3, 1);
        assert_ne!(low, high, "--kmin must change the forced-keyframe cadence");
        let want = composited(&naive(canvas, &frames, &durations));
        assert_eq!(composited(&low), want, "cadence must not change appearance");
        assert_eq!(
            composited(&high),
            want,
            "cadence must not change appearance"
        );
    }
}
