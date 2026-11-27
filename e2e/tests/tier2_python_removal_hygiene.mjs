/**
 * Tier 2 (Boundary / Edge / Negative) — Area 1: Legacy Python Removal & Repo Hygiene
 */

import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { defineTest } from "../registry.mjs";
import { listRepoDirs, listRepoFiles } from "../helpers.mjs";

const AREA = "python-removal-hygiene";

export const tests = [
  defineTest({
    id: "T2-A1-01",
    tier: 2,
    area: AREA,
    features: ["F1", "F2", "F4"],
    title: "Zero Python bytecode, cache directories, or Python packaging manifests exist",
    async fn(ctx) {
      const files = listRepoFiles(ctx.repoRoot, new Set([".git", ".agents", "target"]));
      const dirs = listRepoDirs(ctx.repoRoot, new Set([".git", ".agents", "target"]));

      const badFiles = files.filter(
        (f) =>
          /\.(pyc|pyo|pyd)$/i.test(f) ||
          (/(^|\/)(requirements\.txt|pyproject\.toml|setup\.py|setup\.cfg|Pipfile)$/i.test(f) &&
            f !== "ml/pytorch/requirements.txt"),
      );
      assert.deepStrictEqual(badFiles, [], `Unexpected Python artifacts found: ${badFiles.join(", ")}`);

      const badDirs = dirs.filter((d) =>
        /(^|\/)(__pycache__|\.pytest_cache|\.venv|venv)$/.test(d),
      );
      assert.deepStrictEqual(badDirs, [], `Unexpected Python cache/venv directories found: ${badDirs.join(", ")}`);
    },
  }),

  defineTest({
    id: "T2-A1-02",
    tier: 2,
    area: AREA,
    features: ["F3", "F18"],
    title: "Documentation files contain zero stale Python execution or unittest commands",
    async fn(ctx) {
      const docFiles = [
        "README.md",
        path.join("simulation", "README.md"),
        path.join("docs", "marketplace-data.md"),
      ];
      for (const rel of docFiles) {
        const full = path.join(ctx.repoRoot, rel);
        assert.ok(fs.existsSync(full), `Expected documentation file to exist: ${rel}`);
        const text = fs.readFileSync(full, "utf8");
        assert.ok(!/python3?\s+/i.test(text), `${rel} contains stale python command`);
        assert.ok(!/marketplace_data\.py/i.test(text), `${rel} contains stale marketplace_data.py reference`);
        assert.ok(!/\bunittest\b/i.test(text), `${rel} contains stale unittest reference`);
        assert.ok(!/\bpytest\b/i.test(text), `${rel} contains stale pytest reference`);
      }
    },
  }),

  defineTest({
    id: "T2-A1-03",
    tier: 2,
    area: AREA,
    features: ["F5"],
    title: "Member Cargo.toml manifests have no rogue [workspace] and enforce Edition 2024 / MSRV 1.88",
    async fn(ctx) {
      const rootToml = fs.readFileSync(path.join(ctx.repoRoot, "Cargo.toml"), "utf8");
      const rootHasWorkspacePkg2024 = /edition\s*=\s*"2024"/.test(rootToml);
      const rootHasWorkspaceMsrv = /rust-version\s*=\s*"1\.88"/.test(rootToml);
      assert.ok(rootHasWorkspacePkg2024, 'Root Cargo.toml must declare edition = "2024"');
      assert.ok(rootHasWorkspaceMsrv, 'Root Cargo.toml must declare rust-version = "1.88"');

      for (const member of [
        "world-generator",
        "ml",
        "ad-server",
        "simulation",
        "simulation-analyzer",
        "sim-agent",
        "dashboard",
      ]) {
        const manifestPath = path.join(ctx.repoRoot, member, "Cargo.toml");
        assert.ok(fs.existsSync(manifestPath), `Expected member manifest: ${member}/Cargo.toml`);
        const toml = fs.readFileSync(manifestPath, "utf8");
        assert.ok(!/\[workspace\]/.test(toml), `${member}/Cargo.toml must NOT declare a nested [workspace]`);
        assert.ok(
          /edition\s*=\s*"2024"/.test(toml) || /edition\.workspace\s*=\s*true/.test(toml),
          `${member}/Cargo.toml must use edition = "2024" (or inherit workspace edition)`,
        );
        assert.ok(
          /rust-version\s*=\s*"1\.88"/.test(toml) || /rust-version\.workspace\s*=\s*true/.test(toml),
          `${member}/Cargo.toml must use rust-version = "1.88" (or inherit workspace rust-version)`,
        );
      }
    },
  }),

  defineTest({
    id: "T2-A1-04",
    tier: 2,
    area: AREA,
    features: ["F3"],
    title: "docs/marketplace-data.md preserves Rust serde backward-compatibility contract note",
    async fn(ctx) {
      const docPath = path.join(ctx.repoRoot, "docs", "marketplace-data.md");
      const text = fs.readFileSync(docPath, "utf8");
      assert.ok(
        text.includes("preferred_category_ids"),
        "docs/marketplace-data.md must preserve preferred_category_ids user schema docs",
      );
      assert.ok(
        /older|additional|extra|backward|compat/i.test(text),
        "docs/marketplace-data.md must preserve backward-compatibility note for Rust deserialization",
      );
    },
  }),

  defineTest({
    id: "T2-A1-05",
    tier: 2,
    area: AREA,
    features: ["F2", "F5"],
    title: "world-generator/tests/ directory contains exclusively Rust (.rs) test files",
    async fn(ctx) {
      const testsDir = path.join(ctx.repoRoot, "world-generator", "tests");
      assert.ok(fs.existsSync(testsDir), "world-generator/tests/ directory must exist");
      const entries = fs.readdirSync(testsDir, { withFileTypes: true });
      const files = entries.filter((e) => e.isFile()).map((e) => e.name);
      assert.ok(files.includes("marketplace.rs"), "world-generator/tests/marketplace.rs must exist");
      const nonRs = files.filter((f) => !f.endsWith(".rs"));
      assert.deepStrictEqual(nonRs, [], `Found non-Rust files in world-generator/tests/: ${nonRs.join(", ")}`);
    },
  }),

  defineTest({
    id: "T2-A1-06",
    tier: 2,
    area: AREA,
    features: ["F5", "F18"],
    title: "README.md documents unified 7-crate workspace (world-generator, ad-server, simulation-analyzer, sim-agent, dashboard)",
    async fn(ctx) {
      const readme = fs.readFileSync(path.join(ctx.repoRoot, "README.md"), "utf8");
      for (const crateName of [
        "world-generator",
        "ad-server",
        "visit-generator",
        "simulation-analyzer",
        "sim-agent",
        "dashboard",
      ]) {
        assert.ok(readme.includes(crateName), `README.md must document ${crateName}`);
      }
      assert.ok(/cargo\s+(run|test|build)/i.test(readme), "README.md must document cargo commands");
      assert.ok(!/marketplace_data\.py/i.test(readme), "README.md must not reference legacy marketplace_data.py");
    },
  }),
];
