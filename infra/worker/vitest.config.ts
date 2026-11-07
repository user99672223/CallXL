import { cloudflareTest } from "@cloudflare/vitest-plugin";
import { defineConfig } from "vitest/config";

export default defineConfig({
  plugins: [
    cloudflareTest({
      wrangler: { configPath: "./wrangler.toml" },
      miniflare: { bindings: { APP_SECRET: "test-app-secret", TOKEN_SIGNING_KEY: "test-signing-key" } },
    }),
  ],
});
