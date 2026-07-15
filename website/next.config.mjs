/** @type {import('next').NextConfig} */
const nextConfig = {
  // The short install line the README and the site print. It proxies the
  // repo's own install.sh rather than serving a copy from public/, so the
  // script keeps exactly one source of truth and an edit to it goes live
  // without a site deploy — vercel.json's ignoreCommand skips commits that
  // miss website/, so a copy here would silently go stale.
  //
  // When the domain lands this rewrite is unchanged; only the host printed in
  // front of /install.sh moves. See docs/04-website.md.
  async rewrites() {
    return [
      {
        source: "/install.sh",
        destination:
          "https://raw.githubusercontent.com/zidankazi/roster/main/install.sh",
      },
    ];
  },
};

export default nextConfig;
