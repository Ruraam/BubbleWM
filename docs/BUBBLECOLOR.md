# BubbleColor

BubbleColor is BubbleWM's native wallpaper-to-theme engine. It lives in the
BubbleWM Rust crate and is deliberately independent of Smithay, BubblePanel,
BubbleSettings and BubbleOS. No separate daemon or BubbleOS service is
required for palette generation.

## Current implementation

- Bounded local image decoding (64 MiB input, 16,384 px maximum edge and
  256 MiB decoder allocation limit).
- Thumbnail sampling before analysis to keep CPU and memory costs predictable.
- Deterministic five-center k-means clustering in OKLab rather than RGB.
- Chroma-aware cluster ranking so a colorful subject is not automatically
  discarded in favor of a large neutral background.
- Material-style semantic roles, complementary color harmonies and
  Cyberpunk Glass / neon accent tokens.
- Dark and light modes, plus balanced, vivid and muted palette styles.
- Black/white foreground selection and lightness adjustment of accent tones
  to meet the configured WCAG text contrast target.
- Versioned, serializable output and diagnostic contrast ratios.
- No random seeds, network requests, GPU requirements or compositor calls.

## Rust API

The first iteration exposes two entry points:

- `generate_from_path(path, options)`: safely decodes a local wallpaper.
- `generate_from_pixels(pixels, options, source_file)`: analyzes caller
  supplied RGB samples and is useful for tests and future wallpaper providers.

The returned `ThemeDocument` contains a schema version, source name, options,
semantic colors and measured contrast diagnostics. Colors are represented as
RGB triples and can be formatted with `Rgb::to_hex()`.

## Ownership and integration boundaries

BubbleColor owns color analysis and semantic token generation only.

- BubbleWM owns applying tokens to compositor decorations and rendering.
- BubblePanel consumes the same versioned semantic tokens; it must not run
  its own wallpaper analysis.
- BubbleSettings will edit validated theme options and request preview/apply.
- Wallpaper selection and file watching belong in a future appearance layer,
  not in the compositor render loop.
- BubbleOS may package or configure BubbleWM, but BubbleColor must work without
  BubbleOS-specific paths, services or APIs.

Image decoding and palette generation must never run on the compositor event
loop. The eventual integration should generate a candidate off-thread, validate
it, then publish an immutable palette snapshot to the compositor.


## Asynchronous generation runtime

`bubblewm::theme_runtime::ThemeRuntime` provides a bounded worker-thread
boundary around `generate_from_path`:

- `start()` creates a dedicated `bubblecolor-worker` thread.
- `request(generation, path, options)` queues work without decoding on the caller.
- `try_take_update()` polls for a completed `ThemeDocument` without waiting.
- Every result carries its generation number so consumers can reject stale
  results after a newer wallpaper selection.
- The request and result queues are bounded; the worker never blocks waiting for
  the compositor to consume a result.
- `shutdown()` joins the worker; dropping the runtime also performs orderly
  shutdown.

This is the asynchronous service boundary, not yet live wallpaper watching or
visual application. The compositor must next own a runtime instance, poll it
from its normal event loop, and publish accepted snapshots to decoration and
panel adapters. No renderer state is accessed by the worker.

## Deliberately deferred

This first engine commit does not yet change titlebar rendering, panel colors,
blur, shaders, IPC or live wallpaper watching. Those integrations should follow
only after the palette API and tests are stable. Real backdrop blur remains a
BubbleFX/compositor capability; a theme token must never imply that blur is
available. When unsupported, consumers should use a translucent solid surface.

## Validation

Run:

```sh
cargo fmt --all -- --check
cargo test --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
```

Tests cover OKLab conversion, neutral-background subject selection, palette
personality differences, 4.5:1 and 7:1 text contrast, grayscale wallpapers,
invalid inputs and schema versioning.
