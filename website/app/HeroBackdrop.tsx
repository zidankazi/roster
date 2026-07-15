/** Decorative photo band behind the hero — purely atmospheric, no meaning.
    Sits under the wordmark/tagline/install and dissolves into the cream page
    before the demo window, so the terminal card is never read against it. */
export function HeroBackdrop() {
  return (
    <div className="hero-backdrop" aria-hidden="true">
      <img
        src="/hero-cyclist.webp"
        alt=""
        width={3840}
        height={2560}
        fetchPriority="high"
      />
    </div>
  );
}
