/**
 * Aggregates and validates all 16 Tier 1-4 E2E test modules.
 * Side-effect-free on import so `node e2e/runner.mjs --list` runs instantaneously.
 */

import { validateRegistry } from "../registry.mjs";

import { tests as t1Hygiene } from "./tier1_python_removal_hygiene.mjs";
import { tests as t1Marketplace } from "./tier1_marketplace_generator.mjs";
import { tests as t1Cpc } from "./tier1_cpc_simulator.mjs";
import { tests as t1SimAgent } from "./tier1_sim_agent_plotting.mjs";
import { tests as t1DashCatalog } from "./tier1_dashboard_catalog_api.mjs";
import { tests as t1DashRunner } from "./tier1_dashboard_runner_export.mjs";
import { tests as t1DashUi } from "./tier1_dashboard_ui_interactivity.mjs";

import { tests as t2Hygiene } from "./tier2_python_removal_hygiene.mjs";
import { tests as t2Marketplace } from "./tier2_marketplace_generator.mjs";
import { tests as t2Cpc } from "./tier2_cpc_simulator.mjs";
import { tests as t2SimAgent } from "./tier2_sim_agent_plotting.mjs";
import { tests as t2DashCatalog } from "./tier2_dashboard_catalog_api.mjs";
import { tests as t2DashRunner } from "./tier2_dashboard_runner_export.mjs";
import { tests as t2DashUi } from "./tier2_dashboard_ui_interactivity.mjs";

import { tests as t3Combinations } from "./tier3_combinations.mjs";
import { tests as t4Scenarios } from "./tier4_scenarios.mjs";

export const ALL_TESTS = Object.freeze(
  validateRegistry([
    ...t1Hygiene,
    ...t1Marketplace,
    ...t1Cpc,
    ...t1SimAgent,
    ...t1DashCatalog,
    ...t1DashRunner,
    ...t1DashUi,
    ...t2Hygiene,
    ...t2Marketplace,
    ...t2Cpc,
    ...t2SimAgent,
    ...t2DashCatalog,
    ...t2DashRunner,
    ...t2DashUi,
    ...t3Combinations,
    ...t4Scenarios,
  ]),
);
