import type { MetadataRoute } from "next";
import { locales } from "@/lib/i18n/config";
import { SITE_URL } from "@/lib/page-meta";
import { WIKI_PAGES } from "@/lib/wiki";

// Public, indexable routes (locale-prefixed). /admin and /api are
// intentionally excluded; see app/robots.ts.
const PATHS = [
  "",
  "/install",
  "/docs",
  "/wiki",
  ...WIKI_PAGES.map((page) => `/wiki/${page.slug}`),
  "/faq",
  "/roadmap",
  "/feed",
  "/contribute",
];

export default function sitemap(): MetadataRoute.Sitemap {
  const lastModified = new Date();
  return PATHS.flatMap((path) =>
    locales.map((locale) => ({
      url: `${SITE_URL}/${locale}${path}`,
      lastModified,
      alternates: {
        languages: {
          en: `${SITE_URL}/en${path}`,
          zh: `${SITE_URL}/zh${path}`,
        },
      },
    })),
  );
}
