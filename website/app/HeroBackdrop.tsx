/** The hero's backdrop — atmospheric, and deliberately says nothing about the
    product. Fills the hero behind the wordmark, tagline and install line, and is
    masked and scrimmed in globals.css so it dissolves into the page below.

    Atmospheric is not the same as inert: the demo window shares the hero and
    frosts this image through its glass, so the picture is load-bearing on the
    bloom side. Swapping public/hero-backdrop.webp is still a drop-in, but a dark
    or flat replacement takes the window's glass with it and leaves it a solid
    card — see the .hero-backdrop and .roster-window rules in globals.css, and
    docs/04-website.md ("The hero"). */
export function HeroBackdrop() {
  return (
    <div className="hero-backdrop" aria-hidden="true">
      <img
        src="/hero-backdrop.webp"
        alt=""
        width={1536}
        height={1024}
        fetchPriority="high"
      />
    </div>
  );
}
