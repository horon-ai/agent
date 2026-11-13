import { defineCollection } from "astro:content";
import { glob } from "astro/loaders";
import { z } from "astro/zod";

const docs = defineCollection({
  loader: glob({ base: "./src/content/docs", pattern: "**/*.{md,mdx}" }),
  schema: z.object({
    title: z.string(),
    description: z.string(),
    /** Group shown in the sidebar; order inside a group follows `order`. */
    section: z.enum(["Start", "Using", "Build", "Reference"]),
    order: z.number(),
    /** Hide from the sidebar (still routable). */
    hidden: z.boolean().default(false),
  }),
});

export const collections = { docs };
