/**
 * One source of truth for navigation: the sidebar renders it per section and
 * the header's primary links point at the same routes. Slugs match
 * `src/content/docs/<slug>.md`.
 */
export interface NavItem {
  slug: string;
  title: string;
  section: "Start" | "Using" | "Build" | "Reference";
}

export const SECTIONS = ["Start", "Using", "Build", "Reference"] as const;

export const nav: NavItem[] = [
  { slug: "getting-started", title: "Getting started", section: "Start" },
  { slug: "install", title: "Install", section: "Start" },
  { slug: "configuration", title: "Configuration", section: "Start" },

  { slug: "cli", title: "CLI", section: "Using" },
  { slug: "sessions", title: "Sessions", section: "Using" },
  { slug: "tools", title: "Tools", section: "Using" },
  { slug: "editors", title: "Editors (ACP)", section: "Using" },

  { slug: "sdk-typescript", title: "TypeScript SDK", section: "Build" },
  { slug: "sdk-python", title: "Python SDK", section: "Build" },
  { slug: "architecture", title: "Architecture", section: "Build" },
  { slug: "releasing", title: "Package layout", section: "Build" },

  { slug: "repo-map", title: "Repository map", section: "Reference" },
];

export const primaryLinks = [
  { href: "/getting-started/", label: "Docs" },
  { href: "/sdk-typescript/", label: "SDKs" },
  { href: "/cli/", label: "CLI" },
];

export function itemsFor(section: NavItem["section"]): NavItem[] {
  return nav.filter((item) => item.section === section);
}
