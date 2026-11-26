/**
 * Tier 2 (Boundary / Edge / Negative) — Area 2: Marketplace Generator CLI & Determinism
 */

import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { defineTest } from "../registry.mjs";
import { readJsonFile, runBinary, runBinaryOk, validateMarketplaceJson } from "../helpers.mjs";

const AREA = "marketplace-generator";

export const tests = [
  defineTest({
    id: "T2-A2-01",
    tier: 2,
    area: AREA,
    features: ["F6"],
    title: "Negative seeds (-42, -9) succeed deterministically and differ from positive seeds",
    async fn(ctx) {
      const dNegA = path.join(ctx.tmpDir, "neg42-a");
      const dNegB = path.join(ctx.tmpDir, "neg42-b");
      const dNeg9 = path.join(ctx.tmpDir, "neg9");
      const dPos = path.join(ctx.tmpDir, "pos42");

      await runBinaryOk("ads-simulator", ["--seed", "-42", "--output-dir", dNegA]);
      await runBinaryOk("ads-simulator", ["--seed", "-42", "--output-dir", dNegB]);
      await runBinaryOk("ads-simulator", ["--seed", "-9", "--output-dir", dNeg9]);
      await runBinaryOk("ads-simulator", ["--seed", "42", "--output-dir", dPos]);

      const bA = fs.readFileSync(path.join(dNegA, "marketplace.json"));
      const bB = fs.readFileSync(path.join(dNegB, "marketplace.json"));
      const bPos = fs.readFileSync(path.join(dPos, "marketplace.json"));
      const b9 = fs.readFileSync(path.join(dNeg9, "marketplace.json"));

      assert.strictEqual(Buffer.compare(bA, bB), 0, "seed=-42 must be deterministic");
      assert.notStrictEqual(Buffer.compare(bA, bPos), 0, "seed=-42 must differ from seed=+42");
      assert.notStrictEqual(Buffer.compare(bA, b9), 0, "seed=-42 must differ from seed=-9");

      validateMarketplaceJson(JSON.parse(bA.toString("utf8")), { expectedSeed: -42 });
      validateMarketplaceJson(JSON.parse(b9.toString("utf8")), { expectedSeed: -9 });
    },
  }),

  defineTest({
    id: "T2-A2-02",
    tier: 2,
    area: AREA,
    features: ["F6"],
    title: "Extreme 64-bit signed integer seeds (0, i64::MAX, i64::MIN) serialize accurately",
    async fn(ctx) {
      const cases = [
        { name: "zero", seedStr: "0", regex: /"seed"\s*:\s*0\b/ },
        { name: "max", seedStr: "9223372036854775807", regex: /"seed"\s*:\s*9223372036854775807\b/ },
        { name: "min", seedStr: "-9223372036854775808", regex: /"seed"\s*:\s*-9223372036854775808\b/ },
      ];
      const rawOutputs = [];
      for (const c of cases) {
        const dir = path.join(ctx.tmpDir, `seed-${c.name}`);
        await runBinaryOk("ads-simulator", ["--seed", c.seedStr, "--output-dir", dir]);
        const raw = fs.readFileSync(path.join(dir, "marketplace.json"), "utf8");
        assert.ok(c.regex.test(raw), `Raw JSON must preserve exact i64 seed ${c.seedStr}`);
        validateMarketplaceJson(JSON.parse(raw), { expectedSeed: null });
        rawOutputs.push(raw);
      }
      assert.notStrictEqual(rawOutputs[0], rawOutputs[1], "seed 0 and i64::MAX must differ");
      assert.notStrictEqual(rawOutputs[1], rawOutputs[2], "i64::MAX and i64::MIN must differ");
      assert.notStrictEqual(rawOutputs[0], rawOutputs[2], "seed 0 and i64::MIN must differ");
    },
  }),

  defineTest({
    id: "T2-A2-03",
    tier: 2,
    area: AREA,
    features: ["F6"],
    title: "Deeply nested non-existent --output-dir is auto-created with zero temp residue",
    async fn(ctx) {
      const deepDir = path.join(ctx.tmpDir, "deep", "nested", "marketplace", "out");
      await runBinaryOk("ads-simulator", ["--seed", "42", "--output-dir", deepDir]);
      assert.deepStrictEqual(
        fs.readdirSync(deepDir),
        ["marketplace.json"],
        "Directory must contain only marketplace.json with no leftover temp files",
      );
      validateMarketplaceJson(readJsonFile(path.join(deepDir, "marketplace.json")), { expectedSeed: 42 });
    },
  }),

  defineTest({
    id: "T2-A2-04",
    tier: 2,
    area: AREA,
    features: ["F6"],
    title: "Atomic overwrite on rerun replaces existing marketplace.json cleanly",
    async fn(ctx) {
      const dir = path.join(ctx.tmpDir, "overwrite");
      await runBinaryOk("ads-simulator", ["--seed", "42", "--output-dir", dir]);
      await runBinaryOk("ads-simulator", ["--seed", "77", "--output-dir", dir]);
      assert.deepStrictEqual(fs.readdirSync(dir), ["marketplace.json"]);
      validateMarketplaceJson(readJsonFile(path.join(dir, "marketplace.json")), { expectedSeed: 77 });
    },
  }),

  defineTest({
    id: "T2-A2-05",
    tier: 2,
    area: AREA,
    features: ["F6"],
    title: "Missing required --output-dir fails with non-zero exit and usage error",
    async fn(ctx) {
      for (const args of [[], ["--seed", "42"]]) {
        const res = await runBinary("ads-simulator", args);
        assert.notStrictEqual(res.code, 0, `Expected non-zero exit for args ${JSON.stringify(args)}`);
        assert.ok(res.stderr.trim().length > 0, "Expected non-empty stderr message");
        assert.ok(/--output-dir|usage/i.test(res.stderr), `stderr should mention --output-dir or usage, got: ${res.stderr}`);
      }
    },
  }),

  defineTest({
    id: "T2-A2-06",
    tier: 2,
    area: AREA,
    features: ["F6"],
    title: "Unknown flags and missing flag values are rejected cleanly",
    async fn(ctx) {
      const outDir = path.join(ctx.tmpDir, "bad-flags");
      const badInvocations = [
        ["--unknown-flag"],
        ["--output-dir"],
        ["--output-dir", outDir, "--seed"],
      ];
      for (const args of badInvocations) {
        const res = await runBinary("ads-simulator", args);
        assert.notStrictEqual(res.code, 0, `Expected non-zero exit for ${JSON.stringify(args)}`);
        assert.ok(res.stderr.trim().length > 0, `Expected stderr diagnostic for ${JSON.stringify(args)}`);
      }
      assert.strictEqual(fs.existsSync(path.join(outDir, "marketplace.json")), false, "Failed CLI must not write marketplace.json");
    },
  }),

  defineTest({
    id: "T2-A2-07",
    tier: 2,
    area: AREA,
    features: ["F6"],
    title: "Non-integer and out-of-range i64 --seed values are rejected",
    async fn(ctx) {
      const outDir = path.join(ctx.tmpDir, "bad-seeds");
      for (const badSeed of ["abc", "3.14", "9223372036854775808", "-9223372036854775809"]) {
        const res = await runBinary("ads-simulator", ["--output-dir", outDir, "--seed", badSeed]);
        assert.notStrictEqual(res.code, 0, `Expected non-zero exit for --seed ${badSeed}`);
        assert.ok(res.stderr.trim().length > 0, `Expected stderr diagnostic for --seed ${badSeed}`);
      }
      assert.strictEqual(fs.existsSync(path.join(outDir, "marketplace.json")), false);
    },
  }),
];
