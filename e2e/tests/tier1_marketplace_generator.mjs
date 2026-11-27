/**
 * Tier 1 (Happy-Path) — Area 2: Marketplace Generator CLI & Determinism (ads-simulator)
 */

import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { defineTest } from "../registry.mjs";
import { readJsonFile, runBinaryOk, validateMarketplaceJson } from "../helpers.mjs";

const AREA = "marketplace-generator";

export const tests = [
  defineTest({
    id: "T1-A2-01",
    tier: 1,
    area: AREA,
    features: ["F6"],
    title: "Default seed (42) generates schema v1 marketplace with exact entity counts",
    async fn(ctx) {
      const outDir = path.join(ctx.tmpDir, "mkt-default");
      const res = await runBinaryOk("world-generator", ["--output-dir", outDir]);
      const expectedFile = path.join(outDir, "marketplace.json");
      assert.strictEqual(res.stdout.trim(), expectedFile, "stdout must report written marketplace.json path");
      assert.ok(fs.existsSync(expectedFile), "marketplace.json must exist");

      const mp = readJsonFile(expectedFile);
      validateMarketplaceJson(mp, { expectedSeed: 42 });
      assert.strictEqual(mp.categories[0].id, "category-001");
      assert.strictEqual(mp.categories[0].group, "Electronics");
      assert.strictEqual(mp.categories[0].name, "Phones");
    },
  }),

  defineTest({
    id: "T1-A2-02",
    tier: 1,
    area: AREA,
    features: ["F6"],
    title: "Referential integrity, globally unique 6000 item IDs, price & budget ranges",
    async fn(ctx) {
      const outDir = path.join(ctx.tmpDir, "mkt-integrity");
      await runBinaryOk("world-generator", ["--seed", "42", "--output-dir", outDir]);
      const mp = readJsonFile(path.join(outDir, "marketplace.json"));
      validateMarketplaceJson(mp, { expectedSeed: 42 });

      assert.strictEqual(mp.advertisers[0].id, "advertiser-01");
      assert.strictEqual(mp.advertisers[49].id, "advertiser-50");
      assert.strictEqual(mp.categories[99].id, "category-100");
    },
  }),

  defineTest({
    id: "T1-A2-03",
    tier: 1,
    area: AREA,
    features: ["F6"],
    title: "User personas contain exact schema keys and browsing events preserve 9/3 split",
    async fn(ctx) {
      const outDir = path.join(ctx.tmpDir, "mkt-users");
      await runBinaryOk("world-generator", ["--seed", "42", "--output-dir", outDir]);
      const mp = readJsonFile(path.join(outDir, "marketplace.json"));
      validateMarketplaceJson(mp, { expectedSeed: 42 });

      for (const user of mp.users) {
        assert.deepStrictEqual(
          Object.keys(user).sort(),
          ["id"],
          `User ${user.id} must only serialize id`,
        );
      }
    },
  }),

  defineTest({
    id: "T1-A2-04",
    tier: 1,
    area: AREA,
    features: ["F6"],
    title: "Identical seeds produce byte-for-byte deterministic marketplace.json output",
    async fn(ctx) {
      const dirA = path.join(ctx.tmpDir, "det-a");
      const dirB = path.join(ctx.tmpDir, "det-b");
      await runBinaryOk("world-generator", ["--seed", "42", "--output-dir", dirA]);
      await runBinaryOk("world-generator", ["--seed", "42", "--output-dir", dirB]);

      const bytesA = fs.readFileSync(path.join(dirA, "marketplace.json"));
      const bytesB = fs.readFileSync(path.join(dirB, "marketplace.json"));
      assert.strictEqual(Buffer.compare(bytesA, bytesB), 0, "marketplace.json must be byte-for-byte identical for same seed");
    },
  }),

  defineTest({
    id: "T1-A2-05",
    tier: 1,
    area: AREA,
    features: ["F6"],
    title: "Different seeds produce distinct realizations while preserving taxonomy",
    async fn(ctx) {
      const dirA = path.join(ctx.tmpDir, "seed-42");
      const dirB = path.join(ctx.tmpDir, "seed-43");
      await runBinaryOk("world-generator", ["--seed", "42", "--output-dir", dirA]);
      await runBinaryOk("world-generator", ["--seed", "43", "--output-dir", dirB]);

      const mpA = readJsonFile(path.join(dirA, "marketplace.json"));
      const mpB = readJsonFile(path.join(dirB, "marketplace.json"));
      validateMarketplaceJson(mpA, { expectedSeed: 42 });
      validateMarketplaceJson(mpB, { expectedSeed: 43 });

      assert.deepStrictEqual(mpA.categories, mpB.categories, "Taxonomy categories must be invariant across seeds");
      assert.notDeepStrictEqual(mpA.advertisers, mpB.advertisers, "Advertiser catalogs must vary across distinct seeds");
      assert.notDeepStrictEqual(mpA.browsing_events, mpB.browsing_events, "Browsing events must vary across distinct seeds");
    },
  }),

  defineTest({
    id: "T1-A2-06",
    tier: 1,
    area: AREA,
    features: ["F6"],
    title: "--help and -h print usage and exit 0 without creating files",
    async fn(ctx) {
      const emptyCwd = path.join(ctx.tmpDir, "help-cwd");
      fs.mkdirSync(emptyCwd, { recursive: true });
      for (const flag of ["--help", "-h"]) {
        const res = await runBinaryOk("world-generator", [flag], { cwd: emptyCwd });
        assert.ok(/usage:/i.test(res.stdout), `${flag} stdout must contain Usage:`);
        assert.ok(res.stdout.includes("--output-dir"), `${flag} stdout must document --output-dir`);
        assert.ok(res.stdout.includes("--seed"), `${flag} stdout must document --seed`);
        assert.strictEqual(res.stderr.trim(), "", `${flag} stderr must be empty`);
      }
      assert.deepStrictEqual(fs.readdirSync(emptyCwd), [], "--help must not create any files");
    },
  }),
];
