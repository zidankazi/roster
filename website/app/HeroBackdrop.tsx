/** The hero's decorative backdrop — purely atmospheric, and deliberately says
    nothing about the product. Fills the hero behind the wordmark, tagline and
    install line, and is masked and scrimmed in globals.css so it dissolves
    into the page before the demo window. Swapping the image is a drop-in at
    public/hero-backdrop.webp; see docs/04-website.md ("The hero"). */
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
