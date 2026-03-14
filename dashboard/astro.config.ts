import solid from "@astrojs/solid-js";
import tailwindcss from "@tailwindcss/vite";
import { defineConfig, envField } from "astro/config";

export default defineConfig({
  output: "static",

  integrations: [solid()],

  vite: {
    plugins: [tailwindcss()],
    resolve: {
      alias: {
        "@": "/src",
        "@components": "/src/components",
        "@lib": "/src/lib",
        "@hooks": "/src/hooks",
        "@stores": "/src/stores",
        "@utils": "/src/utils",
      },
    },
  },

  image: {
    domains: [],
  },

  prefetch: {
    prefetchAll: true,
    defaultStrategy: "hover",
  },

  env: {
    schema: {
      PUBLIC_SITE_URL: envField.string({
        context: "client",
        access: "public",
        default: "http://localhost:4321",
      }),
    },
  },
});
