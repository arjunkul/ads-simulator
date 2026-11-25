/**
 * Tier 1 (Happy-Path) — Area 1: Legacy Python Removal & Repo Hygiene
 */

import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { defineTest } from "../registry.mjs";
import { assertZeroPythonFiles, listRepoFiles, REPO_ROOT } from "../helpers.mjs";

const AREA = "python-removal-hygiene";

export const tests = [
  defineTest({
    id: "T1-A1-01",
    tier: 1,
    area: AREA,
    features: ["F1", "F2"],
    title: "Zero .py files exist anywhere in repository tree",
    async fn(ctx) {
      const files = assertZeroPythonFiles(ctx.repoRoot);
      assert.ok(files.length > 0, "Expected repository files to be discovered");
      ctx.log(`Scanned ${files.length} repository files: 0 .py files found`);
    },
  }),

  defineTest({
    id: "T1-A1-02",
    tier: 1,
    area: AREA,
    features: ["F1", "F2"],
    title: "Legacy Python scripts removed while Rust integration tests preserved",
    async fn(ctx) {
      assert.strictEqual(
        fs.existsSync(path.join(ctx.repoRoot, "marketplace_data.py")),
        false,
        "Legacy marketplace_data.py must be deleted",
      );
      assert.strictEqual(
        fs.existsSync(path.join(ctx.repoRoot, "tests", "test_marketplace_data.py")),
        false,
        "Legacy tests/test_marketplace_data.py must be deleted",
      );
      const rustTestPath = path.join(ctx.repoRoot, "tests", "marketplace.rs");
      assert.strictEqual(fs.existsSync(rustTestPath), true, "tests/marketplace.rs must be preserved");
      const content = fs.readFileSync(rustTestPath, "utf8");
      assert.ok(content.length > 500, "tests/marketplace.rs must be non-empty");
      assert.ok(content.includes("generate_marketplace"), "tests/marketplace.rs must test generate_marketplace");
    },
  }),

  defineTest({
    id: "T1-A1-03",
    tier: 1,
    area: AREA,
    features: ["F3"],
    title: "docs/marketplace-data.md cleaned of legacy Python CLI references",
    async fn(ctx) {
      const docPath = path.join(ctx.repoRoot, "docs", "marketplace-data.md");
      assert.ok(fs.existsSync(docPath), "docs/marketplace-data.md must exist");
      const text = fs.readFileSync(docPath, "utf8");
      assert.ok(!/python3?\s+marketplace_data\.py/i.test(text), "Found dead python3 marketplace_data.py command in docs/marketplace-data.md");
      assert.ok(!/marketplace_data\.py/i.test(text), "Found stale marketplace_data.py reference in docs/marketplace-data.md");
      assert.ok(!/original Python generator/i.test(text), "Found stale 'original Python generator' phrasing in docs/marketplace-data.md");
      assert.ok(text.includes("cargo run"), "docs/marketplace-data.md should document cargo run");
      assert.ok(text.includes("marketplace.json"), "docs/marketplace-data.md should document marketplace.json");
      assert.ok(text.includes("schema_version"), "docs/marketplace-data.md should document schema_version");
      assert.ok(text.includes("score_preference"), "docs/marketplace-data.md should document score_preference");
    },
  }),

  defineTest({
    id: "T1-A1-04",
    tier: 1,
    area: AREA,
    features: ["F4"],
    title: ".gitignore updated for pure-Rust workspace (/target/, /output/, no __pycache__/)",
    async fn(ctx) {
      const giPath = path.join(ctx.repoRoot, ".gitignore");
      assert.ok(fs.existsSync(giPath), ".gitignore must exist");
      const lines = fs
        .readFileSync(giPath, "utf8")
        .split(/\r?\n/)
        .map((l) => l.trim())
        .filter((l) => l.length > 0 && !l.startsWith("#"));
      assert.ok(
        !lines.some((l) => l.includes("__pycache__")),
        `.gitignore must not contain __pycache__ entries, got: ${lines.join(", ")}`,
      );
      assert.ok(
        lines.some((l) => l === "/target/" || l === "target/" || l === "/target"),
        ".gitignore must ignore /target/",
      );
      assert.ok(
        lines.some((l) => l === "/output/" || l === "output/" || l === "/output"),
        ".gitignore must ignore /output/",
      );
    },
  }),

  defineTest({
    id: "T1-A1-05",
    tier: 1,
    area: AREA,
    features: ["F5"],
    title: "Unified Cargo workspace at root with single Cargo.lock and all member crates",
    async fn(ctx) {
      const rootToml = fs.readFileSync(path.join(ctx.repoRoot, "Cargo.toml"), "utf8");
      assert.ok(/\[workspace\]/.test(rootToml), "Root Cargo.toml must declare [workspace]");
      for (const member of [
        "ad-server",
        "impression-generator",
        "simulation",
        "simulation-analyzer",
        "sim-agent",
        "dashboard",
      ]) {
        assert.ok(
          new RegExp(`["']${member}["']`).test(rootToml),
          `Root Cargo.toml workspace members must include "${member}"`,
        );
      }
      const rootLock = path.join(ctx.repoRoot, "Cargo.lock");
      assert.ok(fs.existsSync(rootLock) && fs.statSync(rootLock).size > 0, "Root Cargo.lock must exist and be non-empty");
      assert.strictEqual(
        fs.existsSync(path.join(ctx.repoRoot, "simulation", "Cargo.lock")),
        false,
        "Nested simulation/Cargo.lock must be removed after workspace unification",
      );
      assert.strictEqual(
        fs.existsSync(path.join(ctx.repoRoot, "simulation", ".gitignore")),
        false,
        "Nested simulation/.gitignore must be removed after workspace unification",
      );
    },
  }),

  defineTest({
    id: "T1-A1-06",
    tier: 1,
    area: AREA,
    features: ["F1", "F18"],
    title: "e2e/ directory is pure Node.js ESM with zero Python and zero npm dependencies",
    async fn(ctx) {
      const e2eDir = path.join(ctx.repoRoot ?? REPO_ROOT, "e2e");
      const files = listRepoFiles(e2eDir, new Set());
      assert.deepStrictEqual(
        files.filter((f) => f.toLowerCase().endsWith(".py")),
        [],
        "e2e/ must contain zero .py files",
      );
      assert.ok(!files.some((f) => f.includes("node_modules/")), "e2e/ must not contain node_modules");
      assert.ok(
        !files.some((f) => /(package-lock\.json|yarn\.lock|pnpm-lock\.yaml)$/.test(f)),
        "e2e/ must not have external npm lockfiles",
      );
      assert.ok(files.includes("runner.mjs"), "e2e/runner.mjs must exist");
      assert.ok(files.includes("helpers.mjs"), "e2e/helpers.mjs must exist");
      assert.ok(files.includes("registry.mjs"), "e2e/registry.mjs must exist");
    },
  }),
];
