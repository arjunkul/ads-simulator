/**
 * Declarative, side-effect-free E2E test registry for ads-simulator.
 *
 * Strictly forbids any skip primitive: every registered test executes and must
 * either PASS or FAIL loudly.
 */

export const VALID_TIERS = Object.freeze([1, 2, 3, 4]);

export const FEATURE_AREAS = Object.freeze([
  "python-removal-hygiene",
  "marketplace-generator",
  "cpc-simulator",
  "sim-agent-plotting",
  "dashboard-catalog-api",
  "dashboard-runner-export",
  "dashboard-ui-interactivity",
]);

export const AREA_LABELS = Object.freeze({
  "python-removal-hygiene": "Legacy Python Removal & Repo Hygiene",
  "marketplace-generator": "Marketplace Generator CLI & Determinism",
  "cpc-simulator": "30-Day CPC Simulator CLI & Ledger Accounting",
  "sim-agent-plotting": "sim-agent 30-Day Execution & Budget Plotting",
  "dashboard-catalog-api": "Dashboard Feature Catalog API & Live Introspection",
  "dashboard-runner-export": "Dashboard Live Sim-Agent Runner & Static Export",
  "dashboard-ui-interactivity": "Dashboard Browser/UI Rendering & Interactivity",
});

export const TIER_LABELS = Object.freeze({
  1: "Tier 1: Isolated Happy-Path",
  2: "Tier 2: Boundary, Edge & Negative",
  3: "Tier 3: Cross-Feature Combinations",
  4: "Tier 4: Real-World Application Scenarios",
});

export const DEFAULT_TIMEOUTS_MS = Object.freeze({
  1: 30_000,
  2: 30_000,
  3: 60_000,
  4: 90_000,
});

const FEATURE_AREA_SET = new Set(FEATURE_AREAS);

/**
 * Define an immutable declarative E2E test descriptor.
 *
 * @param {{
 *   id: string,
 *   tier: 1 | 2 | 3 | 4,
 *   area: string,
 *   areas?: string[],
 *   features: string[],
 *   title: string,
 *   timeoutMs?: number,
 *   fn: (ctx: object) => Promise<void>,
 * }} spec
 */
export function defineTest(spec) {
  if (!spec || typeof spec !== "object") {
    throw new TypeError("defineTest: spec must be a non-null object");
  }
  if ("skip" in spec || "skipped" in spec || "only" in spec) {
    throw new Error(
      `defineTest(${spec.id ?? "unknown"}): skip/only primitives are forbidden in the E2E suite`,
    );
  }

  const { id, tier, area, areas, features, title, timeoutMs, fn } = spec;

  if (typeof id !== "string" || id.trim().length === 0) {
    throw new TypeError("defineTest: id must be a non-empty string");
  }
  if (!VALID_TIERS.includes(tier)) {
    throw new RangeError(`defineTest(${id}): tier must be one of ${VALID_TIERS.join(", ")}, got ${tier}`);
  }
  if (typeof title !== "string" || title.trim().length === 0) {
    throw new TypeError(`defineTest(${id}): title must be a non-empty string`);
  }
  if (typeof fn !== "function") {
    throw new TypeError(`defineTest(${id}): fn must be a function`);
  }
  if (!Array.isArray(features) || features.length === 0 || !features.every((f) => typeof f === "string" && /^F\d+$/.test(f))) {
    throw new TypeError(`defineTest(${id}): features must be a non-empty array of F<n> feature IDs`);
  }

  let resolvedArea;
  let resolvedAreas;

  if (tier === 1 || tier === 2) {
    if (!FEATURE_AREA_SET.has(area)) {
      throw new RangeError(`defineTest(${id}): Tier ${tier} area "${area}" must be one of ${FEATURE_AREAS.join(", ")}`);
    }
    resolvedArea = area;
    resolvedAreas = Array.isArray(areas) && areas.length > 0 ? [...new Set(areas)] : [area];
  } else if (tier === 3) {
    resolvedArea = area ?? "cross-feature";
    resolvedAreas = Array.isArray(areas) ? [...new Set(areas)] : [];
    if (resolvedAreas.length < 2) {
      throw new RangeError(`defineTest(${id}): Tier 3 test must specify at least 2 areas`);
    }
  } else {
    resolvedArea = area ?? "workflow";
    resolvedAreas = Array.isArray(areas) ? [...new Set(areas)] : [];
    if (resolvedAreas.length < 3) {
      throw new RangeError(`defineTest(${id}): Tier 4 scenario must specify at least 3 areas`);
    }
  }

  for (const slug of resolvedAreas) {
    if (!FEATURE_AREA_SET.has(slug)) {
      throw new RangeError(`defineTest(${id}): invalid area slug "${slug}" in areas`);
    }
  }

  const resolvedTimeout = timeoutMs ?? DEFAULT_TIMEOUTS_MS[tier];
  if (!Number.isInteger(resolvedTimeout) || resolvedTimeout <= 0) {
    throw new RangeError(`defineTest(${id}): timeoutMs must be a positive integer`);
  }

  return Object.freeze({
    id: id.trim(),
    tier,
    area: resolvedArea,
    areas: Object.freeze(resolvedAreas),
    features: Object.freeze([...features]),
    title: title.trim(),
    timeoutMs: resolvedTimeout,
    fn,
  });
}

/**
 * Validate invariants across the complete test registry.
 * Throws loudly on duplicate IDs, invalid tiers, or unknown areas.
 *
 * @param {readonly object[]} tests
 */
export function validateRegistry(tests) {
  if (!Array.isArray(tests)) {
    throw new TypeError("validateRegistry: tests must be an array");
  }
  const seenIds = new Set();
  for (const t of tests) {
    if (!t || typeof t !== "object") {
      throw new TypeError("validateRegistry: encountered non-object test entry");
    }
    if ("skip" in t || "skipped" in t) {
      throw new Error(`validateRegistry: test ${t.id} contains forbidden skip property`);
    }
    if (seenIds.has(t.id)) {
      throw new Error(`validateRegistry: duplicate test id "${t.id}"`);
    }
    seenIds.add(t.id);

    if (!VALID_TIERS.includes(t.tier)) {
      throw new Error(`validateRegistry: test ${t.id} has invalid tier ${t.tier}`);
    }
    if (t.tier <= 2 && !FEATURE_AREA_SET.has(t.area)) {
      throw new Error(`validateRegistry: test ${t.id} has unknown area "${t.area}"`);
    }
    for (const slug of t.areas ?? []) {
      if (!FEATURE_AREA_SET.has(slug)) {
        throw new Error(`validateRegistry: test ${t.id} references unknown area "${slug}"`);
      }
    }
    if (typeof t.fn !== "function") {
      throw new Error(`validateRegistry: test ${t.id} is missing fn`);
    }
  }
  return tests;
}

/**
 * Compute tier and per-area summary counts for the registry.
 *
 * @param {readonly object[]} tests
 */
export function summarizeRegistry(tests) {
  const byTier = { 1: 0, 2: 0, 3: 0, 4: 0 };
  const tier1ByArea = Object.fromEntries(FEATURE_AREAS.map((a) => [a, 0]));
  const tier2ByArea = Object.fromEntries(FEATURE_AREAS.map((a) => [a, 0]));
  const byArea = Object.fromEntries(FEATURE_AREAS.map((a) => [a, 0]));

  for (const t of tests) {
    byTier[t.tier] = (byTier[t.tier] ?? 0) + 1;
    if (t.tier === 1 && Object.hasOwn(tier1ByArea, t.area)) {
      tier1ByArea[t.area] += 1;
    } else if (t.tier === 2 && Object.hasOwn(tier2ByArea, t.area)) {
      tier2ByArea[t.area] += 1;
    }
    for (const slug of t.areas ?? []) {
      if (Object.hasOwn(byArea, slug)) {
        byArea[slug] += 1;
      }
    }
  }

  return {
    total: tests.length,
    byTier,
    tier1ByArea,
    tier2ByArea,
    byArea,
  };
}
