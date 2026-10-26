import { generateKeyPairSync, randomBytes } from "node:crypto";
import { cloudflareTest, readD1Migrations } from "@cloudflare/vitest-pool-workers";
import { defineConfig } from "vitest/config";

// A throwaway Clerk signing key: the Worker verifies networklessly with the public half (CLERK_JWT_KEY),
// the tests mint session tokens with the private half.
const { publicKey, privateKey } = generateKeyPairSync("rsa", { modulusLength: 2048 });

export default defineConfig(async () => ({
  plugins: [
    cloudflareTest({
      wrangler: { configPath: "./wrangler.toml", environment: "local" },
      miniflare: {
        d1Databases: ["ANDROID_MIGRATION_DB"],
        // The pool's bundled workerd trails wrangler's; pin to the newest date it supports.
        compatibilityDate: "2026-08-22",
        bindings: {
          TEST_MIGRATIONS: await readD1Migrations("./migrations"),
          CLERK_JWT_KEY: publicKey.export({ type: "spki", format: "pem" }).toString(),
          TEST_CLERK_PRIVATE_JWK: JSON.stringify(privateKey.export({ format: "jwk" })),
          // The web-device tests mock Clerk's Backend API; the key only has to be present.
          CLERK_SECRET_KEY: "sk_test_unit",
          WEB_ORIGINS: "https://web.test,https://web2.test",
          // Every JSON answer is checked against serve/contract/cloud/contract.json (src/index.ts strictProblems).
          KITH_CONTRACT_STRICT: "1",
          CLERK_WEBHOOK_SECRET: `whsec_${randomBytes(24).toString("base64")}`,
        },
      },
    }),
  ],
  test: {
    setupFiles: ["./test/setup.ts"],
    testTimeout: 20_000,
  },
}));
