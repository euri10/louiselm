# Android capture logo proposals

These five one-shot concepts are exploratory raster assets. **Scarlet Signal**
(option 2) is the selected Android launcher icon and is packaged as
density-specific raster resources under
`android/app/src/main/res/mipmap-*/ic_launcher.png`. It remains raster artwork;
no vector or adaptive-icon variant is provided.

## Concepts

1. **Rebel Echo** — Louise Michel's profile speaks into a microphone-shaped
   negative space, making voice capture the most immediate reading.
2. **Scarlet Signal** — A frontal revolutionary portrait turns a flowing red
   scarf into a waveform, joining identity, motion, and speech.
3. **Quiet Vault** — Fingerprint-like paths reveal a profile inside a shield,
   with the protected recording dot emphasizing private local-first capture.
4. **Living Archive** — A face hidden in an abstract flame/quill preserves a
   spoken spark, connecting radical thought with durable capture.
5. **Unblinking Witness** — A resolute eye inside capture brackets treats the
   app as a pocket witness, with one red dot supplying the recording cue.

## Generation prompts

All five images were generated independently with the built-in image-generation
workflow. They intentionally received distinct subjects, compositions, and
palettes rather than being derived as variations of one mark.

## Selected refinements

The follow-up pass targets only the two directions selected for continuation:

- **[refined-02-scarlet-signal.png](refined-02-scarlet-signal.png)** removes the fuzzy halo and excess poster texture while keeping Louise Michel, the red scarf, and the three-bar scarf waveform. The mouth is unobstructed.
- **[refined-05-unblinking-witness.png](refined-05-unblinking-witness.png)** replaces the transparent-looking field with solid cobalt and simplifies the eye, hair, and capture brackets for launcher-size clarity.

See **[refined-comparison-sheet.png](refined-comparison-sheet.png)** for the
side-by-side comparison. **Scarlet Signal** is the selected final direction and
is packaged as the Android launcher icon; **Unblinking Witness** remains a
concept.

### Refinement prompt for 02

```text
Refine only the existing Scarlet Signal logo. Preserve the frontal Louise Michel portrait, red scarf, three-bar scarf waveform, circular composition, and black/cream/scarlet identity. Remove the fuzzy glowing red halo and poster-like texture; use one clean flat scarlet background. Reduce facial and hair detail to crisp restrained screen-print shapes for 48px readability. The scarf waveform is the only capture cue; do not add a microphone or place anything over the mouth. No text, letters, gradients, shadows, mockups, 3D, watermark, or extra objects.
```

### Refinement prompt for 05

```text
Refine only the existing Unblinking Witness logo. Preserve the asymmetrical resolute eye, swept dark hair, four open capture brackets, and one red recording dot. Replace the transparent/black-looking field with a solid flat cobalt-blue square background. Make the brackets crisp and evenly weighted; simplify hair and facial line detail into bold editorial screen-print shapes for 48px readability. No microphone, waveform, text, letters, gradients, shadows, mockups, 3D, watermark, or extra objects.
```

### 01 — Rebel Echo

```text
Use case: logo-brand
Asset type: Android launcher icon proposal, 1024x1024 square raster
Primary request: Create an original LouiseLM capture-app mark called "Rebel Echo": a bold side-profile portrait/head silhouette of 19th-century French revolutionary Louise Michel, with the negative space at her mouth becoming one clean microphone capsule and a single sound-wave arc.
Style/medium: minimal vector-like logo mark, historically suggestive woodcut geometry simplified to very few shapes, flat colors only
Composition/framing: one centered compact emblem, large strong silhouette, generous Android safe-zone margin, instantly readable at 48px
Color palette: charcoal black, warm ivory, one restrained deep revolutionary red accent
Constraints: no text, no letters, no gradient, no shadow, no mockup, no 3D, no border, no watermark; original design; do not imitate any existing app logo or contemporary artwork; restrained facial detail; square image with a clean solid ivory background
```

### 02 — Scarlet Signal

```text
Use case: logo-brand
Asset type: Android launcher icon proposal, 1024x1024 square raster
Primary request: Create an original LouiseLM capture-app mark called "Scarlet Signal": a near-frontal simplified head of 19th-century French revolutionary Louise Michel, recognizable through her swept-back hair and resolute expression; one flowing red scarf wraps beneath the portrait and transforms into exactly three bold audio waveform bars.
Style/medium: crisp modern linocut logo, flat vector-like shapes, radically simplified
Composition/framing: centered circular composition with the portrait as a compact bust and the scarf-waveform forming the lower third; strong silhouette and generous Android safe-zone margin; readable at 48px
Color palette: black, warm cream, saturated scarlet red
Constraints: materially different from a profile-with-microphone concept; no microphone object; no text, no letters, no gradient, no shadow, no mockup, no 3D, no watermark; original design; do not imitate any existing logo or contemporary artwork; restrained facial detail; square image with clean solid scarlet background and cream/black mark
```

### 03 — Quiet Vault

```text
Use case: logo-brand
Asset type: Android launcher icon proposal, 1024x1024 square raster
Primary request: Create an original LouiseLM capture-app mark called "Quiet Vault": an abstract local-first recording symbol where a minimal Louise Michel head cameo is carved from fingerprint-like concentric grooves inside a stout rounded shield; one small solid recording dot is protected at the center.
Style/medium: geometric Swiss-modern vector-like logo, flat shapes, sparse negative space, not a portrait illustration
Composition/framing: centered shield emblem filling about two thirds of the square, compact symmetrical silhouette, generous Android safe-zone margin, readable at 48px
Color palette: deep midnight blue, pale mint, one small coral recording-dot accent
Constraints: materially different from portrait-plus-microphone and scarf-waveform concepts; communicate privacy and local capture; no microphone, no sound bars, no text, no letters, no gradients, no shadows, no mockup, no 3D, no watermark; original design; do not imitate any existing app logo or artwork; square image with clean solid pale mint background
```

### 04 — Living Archive

```text
Use case: logo-brand
Asset type: Android launcher icon proposal, 1024x1024 square raster
Primary request: Create an original LouiseLM capture-app mark called "Living Archive": a single angular flame/quill shape whose inner negative space subtly reveals the profile of Louise Michel; the quill tip curls around a small circular record dot, suggesting an idea spoken now and preserved locally.
Style/medium: bold Bauhaus-inspired vector-like symbol, flat geometric cut-paper shapes, highly abstract rather than illustrative
Composition/framing: one diagonal rising emblem centered in the square, unmistakable silhouette with few internal cuts, generous Android safe-zone margin, readable at 48px
Color palette: dark aubergine symbol, vivid marigold field, one small warm ivory cutout
Constraints: materially different from a literal portrait, scarf waveform, microphone, or privacy shield; no microphone object, no waveform bars, no text, no letters, no gradients, no shadow, no mockup, no 3D, no watermark; original design; do not imitate any existing logo or contemporary artwork; square image with solid marigold background
```

### 05 — Unblinking Witness

```text
Use case: logo-brand
Asset type: Android launcher icon proposal, 1024x1024 square raster
Primary request: Create an original LouiseLM capture-app mark called "Unblinking Witness": an extreme close crop of one resolute eye and a sweep of dark hair evoking Louise Michel, framed by four chunky open capture brackets; a small red recording dot sits in one empty corner.
Style/medium: stark editorial screen-print logo, flat vector-like blocks, minimal pop-art geometry
Composition/framing: square face fragment with asymmetrical eye as focal point, four bold corner brackets creating a capture frame, very few shapes, generous Android safe-zone margin, readable at 48px
Color palette: cobalt blue background, near-black portrait shapes, off-white eye/capture brackets, one vermilion dot
Constraints: materially different from full portraits, profile-with-microphone, scarf waveform, shield, and quill concepts; no microphone, no waveform, no text, no letters, no gradients, no shadow, no mockup, no 3D, no border, no watermark; original design; do not imitate any existing app logo or contemporary artwork; square image with solid cobalt field
```
